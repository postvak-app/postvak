use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

use zeroize::Zeroizing;

use crate::config::{AccountConfig, PasswordSource};

const SERVICE: &str = "postvak";

pub struct Secret(Zeroizing<String>);

impl Secret {
    pub fn new(value: String) -> Secret {
        Secret(Zeroizing::new(value))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CredentialError {
    #[error("no password stored for account '{0}'; run `postvak account add`")]
    NotFound(String),
    #[error("the keyring is locked or denied access")]
    Locked,
    #[error("no keyring is available on this system; use `password = {{ command = \"...\" }}`")]
    Unavailable,
    #[error("password command for account '{account}' exited with status {status}")]
    CommandFailed { account: String, status: i32 },
    #[error("password command for account '{0}' printed nothing")]
    CommandEmpty(String),
    #[error("password command could not be started: {0}")]
    CommandSpawn(String),
    #[error(
        "refusing to run the password command for account '{account}': {} {why}; fix it with `{fix}`",
        path.display()
    )]
    ConfigUntrusted {
        account: String,
        path: PathBuf,
        why: &'static str,
        fix: String,
    },
}

/// The account's password. A password command runs as it is given: callers check the file it came from first, with
/// `check_command_trusted`.
pub fn resolve(account: &AccountConfig) -> Result<Secret, CredentialError> {
    match &account.password {
        PasswordSource::Command { command } => run_command(&account.name, command),
        PasswordSource::Keyring { .. } => {
            let entry = keyring_entry(&account.name)?;
            let name = account.name.clone();
            // macOS asks again for Keychain access after each new unsigned binary, and the prompt may sit behind other windows.
            let password = hint_if_slow(
                Duration::from_secs(2),
                move || {
                    log::info!(
                        "{name}: waiting for the OS keyring; answer its password prompt if one is showing"
                    )
                },
                || entry.get_password(),
            );
            match password {
                Ok(password) => Ok(Secret::new(password)),
                Err(e) => Err(map_keyring_error(e, &account.name)),
            }
        }
    }
}

/// Refuses to go on with an account whose password command sits in a `config_file` another user could have written,
/// the way OpenSSH treats `~/.ssh/config`: the
/// file and the folder it sits in (after following symlinks, so a file kept in dotfiles is judged where it lives)
/// must belong to the current user or root, and not be writable by group or others. A folder with the sticky bit
/// (such as `/nix/store`) keeps others from replacing the file, so its write bits do not count. A missing file has
/// no command to distrust. A keyring account has no command, so it always passes.
pub fn check_command_trusted(
    account: &AccountConfig,
    config_file: &Path,
) -> Result<(), CredentialError> {
    if !matches!(account.password, PasswordSource::Command { .. }) {
        return Ok(());
    }
    // A file that cannot be resolved cannot be read either, so the config load reports it.
    let Ok(file) = fs::canonicalize(config_file) else {
        return Ok(());
    };
    check_trusted_as(rustix::process::geteuid().as_raw(), &account.name, &file)
}

fn check_trusted_as(uid: u32, account: &str, file: &Path) -> Result<(), CredentialError> {
    let untrusted = |path: &Path, why, fix: String| CredentialError::ConfigUntrusted {
        account: account.to_string(),
        path: path.to_path_buf(),
        why,
        fix,
    };
    let folder = file.parent();
    for path in std::iter::once(file).chain(folder) {
        let meta = fs::metadata(path).map_err(|_| {
            untrusted(
                path,
                "cannot be inspected",
                format!("chmod go-w {}", path.display()),
            )
        })?;
        let shown = shell_quote(path);
        if meta.uid() != uid && meta.uid() != 0 {
            return Err(untrusted(
                path,
                "belongs to another user",
                format!("chown $USER {shown}"),
            ));
        }
        let sticky = meta.is_dir() && meta.mode() & 0o1000 != 0;
        if meta.mode() & 0o022 != 0 && !sticky {
            return Err(untrusted(
                path,
                "can be written by other users",
                format!("chmod go-w {shown}"),
            ));
        }
    }
    Ok(())
}

fn shell_quote(path: &Path) -> String {
    let text = path.to_string_lossy();
    if text
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-+@:,".contains(c))
    {
        text.into_owned()
    } else {
        format!("'{}'", text.replace('\'', "'\\''"))
    }
}

/// Runs `work`, calling `hint` once it has taken longer than `delay`.
fn hint_if_slow<T>(
    delay: Duration,
    hint: impl FnOnce() + Send + 'static,
    work: impl FnOnce() -> T,
) -> T {
    let (done, waiting) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || {
        if waiting.recv_timeout(delay) == Err(std::sync::mpsc::RecvTimeoutError::Timeout) {
            hint();
        }
    });
    let result = work();
    drop(done);
    result
}

pub fn store(account_name: &str, secret: &Secret) -> Result<(), CredentialError> {
    let entry = keyring_entry(account_name)?;
    entry
        .set_password(secret.expose())
        .map_err(|e| map_keyring_error(e, account_name))
}

fn run_command(account_name: &str, command: &str) -> Result<Secret, CredentialError> {
    let output = Command::new("sh")
        .arg("-c")
        .arg(command)
        .stderr(std::process::Stdio::inherit())
        .output()
        .map_err(|e| CredentialError::CommandSpawn(e.to_string()))?;
    if !output.status.success() {
        return Err(CredentialError::CommandFailed {
            account: account_name.to_string(),
            status: output.status.code().unwrap_or(-1),
        });
    }
    let stdout = Zeroizing::new(output.stdout);
    let text = Zeroizing::new(String::from_utf8_lossy(&stdout).into_owned());
    let trimmed = text.trim_end_matches(['\n', '\r']);
    if trimmed.is_empty() {
        return Err(CredentialError::CommandEmpty(account_name.to_string()));
    }
    Ok(Secret::new(trimmed.to_string()))
}

fn keyring_entry(account_name: &str) -> Result<keyring_core::Entry, CredentialError> {
    static STORE: OnceLock<Result<(), String>> = OnceLock::new();
    let init = STORE.get_or_init(|| platform_store().map(keyring_core::set_default_store));
    if let Err(detail) = init {
        log::debug!("keyring store init failed: {detail}");
        return Err(CredentialError::Unavailable);
    }
    keyring_core::Entry::new(SERVICE, account_name).map_err(|e| map_keyring_error(e, account_name))
}

#[cfg(all(target_os = "macos", not(any(test, feature = "testing"))))]
fn platform_store() -> Result<std::sync::Arc<keyring_core::CredentialStore>, String> {
    apple_native_keyring_store::keychain::Store::new()
        .map(|s| s as std::sync::Arc<keyring_core::CredentialStore>)
        .map_err(|e| e.to_string())
}

#[cfg(all(target_os = "linux", not(any(test, feature = "testing"))))]
fn platform_store() -> Result<std::sync::Arc<keyring_core::CredentialStore>, String> {
    zbus_secret_service_keyring_store::Store::new()
        .map(|s| s as std::sync::Arc<keyring_core::CredentialStore>)
        .map_err(|e| e.to_string())
}

#[cfg(all(
    not(any(target_os = "macos", target_os = "linux")),
    not(any(test, feature = "testing"))
))]
fn platform_store() -> Result<std::sync::Arc<keyring_core::CredentialStore>, String> {
    Err("no keyring store for this platform".to_string())
}

/// Tests never touch the OS keyring, the front ends' tests included (`testing`): an in-memory store for the process.
#[cfg(any(test, feature = "testing"))]
fn platform_store() -> Result<std::sync::Arc<keyring_core::CredentialStore>, String> {
    keyring_core::mock::Store::new()
        .map(|s| s as std::sync::Arc<keyring_core::CredentialStore>)
        .map_err(|e| e.to_string())
}

fn map_keyring_error(e: keyring_core::Error, account_name: &str) -> CredentialError {
    log::debug!("keyring error for '{account_name}': {e}");
    match e {
        keyring_core::Error::NoEntry => CredentialError::NotFound(account_name.to_string()),
        keyring_core::Error::NoStorageAccess(_) => CredentialError::Locked,
        _ => CredentialError::Unavailable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(command: &str) -> AccountConfig {
        let password = PasswordSource::Command {
            command: command.into(),
        };
        AccountConfig::new("t", "h", "u@example.com", password)
    }

    #[test]
    fn hint_if_slow_fires_only_for_slow_work() {
        use std::sync::mpsc;
        use std::time::Duration;

        let (tx, rx) = mpsc::channel();
        let slow = hint_if_slow(
            Duration::from_millis(20),
            move || tx.send("slow").unwrap(),
            || {
                std::thread::sleep(Duration::from_millis(200));
                1
            },
        );
        assert_eq!(slow, 1);
        assert_eq!(rx.recv_timeout(Duration::from_secs(1)), Ok("slow"));

        let (tx, rx) = mpsc::channel();
        assert_eq!(
            hint_if_slow(
                Duration::from_millis(500),
                move || tx.send("fast").unwrap(),
                || 2
            ),
            2
        );
        assert!(rx.recv_timeout(Duration::from_millis(800)).is_err());
    }

    #[test]
    fn command_output_is_trimmed() {
        let secret = resolve(&account("printf 'hunter2\\n'")).unwrap();
        assert_eq!(secret.expose(), "hunter2");
    }

    #[test]
    fn empty_command_output_is_error() {
        assert!(matches!(
            resolve(&account("true")),
            Err(CredentialError::CommandEmpty(_))
        ));
    }

    #[test]
    fn failing_command_reports_status() {
        assert!(matches!(
            resolve(&account("exit 3")),
            Err(CredentialError::CommandFailed { status: 3, .. })
        ));
    }

    fn config_in(dir: &Path, file_mode: u32, dir_mode: u32) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let file = dir.join("config.toml");
        fs::write(&file, "").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(file_mode)).unwrap();
        fs::set_permissions(dir, fs::Permissions::from_mode(dir_mode)).unwrap();
        file
    }

    fn reason(result: Result<(), CredentialError>) -> String {
        match result {
            Err(e @ CredentialError::ConfigUntrusted { .. }) => e.to_string(),
            other => panic!("expected the config to be refused, got {other:?}"),
        }
    }

    #[test]
    fn a_config_only_its_owner_can_write_may_run_its_command() {
        let dir = tempfile::tempdir().unwrap();
        let file = config_in(dir.path(), 0o600, 0o755);
        assert!(check_command_trusted(&account("true"), &file).is_ok());
        let file = config_in(dir.path(), 0o644, 0o700);
        assert!(check_command_trusted(&account("true"), &file).is_ok());
    }

    #[test]
    fn a_config_others_can_write_is_refused_with_the_fix() {
        let dir = tempfile::tempdir().unwrap();
        for mode in [0o620, 0o602, 0o666] {
            let file = config_in(dir.path(), mode, 0o700);
            let message = reason(check_command_trusted(&account("true"), &file));
            let path = fs::canonicalize(&file).unwrap();
            assert!(
                message.contains("can be written by other users"),
                "{message}"
            );
            assert!(
                message.ends_with(&format!("`chmod go-w {}`", path.display())),
                "{message}"
            );
        }
    }

    #[test]
    fn a_config_in_a_folder_others_can_write_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let file = config_in(dir.path(), 0o600, 0o770);
        let message = reason(check_command_trusted(&account("true"), &file));
        let folder = fs::canonicalize(dir.path()).unwrap();
        assert!(message.contains(&folder.display().to_string()), "{message}");
        assert!(
            message.contains("can be written by other users"),
            "{message}"
        );
    }

    #[test]
    fn a_sticky_folder_keeps_others_from_replacing_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = config_in(dir.path(), 0o600, 0o1777);
        assert!(check_command_trusted(&account("true"), &file).is_ok());
    }

    #[test]
    fn a_symlinked_config_is_judged_where_it_lives() {
        let dotfiles = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let target = config_in(dotfiles.path(), 0o666, 0o700);
        let link = home.path().join("config.toml");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        reason(check_command_trusted(&account("true"), &link));
    }

    #[test]
    fn a_config_of_another_user_is_refused() {
        let me = rustix::process::geteuid().as_raw();
        if me == 0 {
            return; // root owns the files it makes, and root-owned files pass
        }
        let dir = tempfile::tempdir().unwrap();
        let file = config_in(dir.path(), 0o600, 0o700);
        let message = reason(check_trusted_as(me + 1, "t", &file));
        assert!(message.contains("belongs to another user"), "{message}");
    }

    #[test]
    fn a_missing_config_and_a_keyring_account_pass() {
        let dir = tempfile::tempdir().unwrap();
        let loose = config_in(dir.path(), 0o666, 0o777);
        let keyring = AccountConfig::new(
            "k",
            "h",
            "u@example.com",
            PasswordSource::Keyring { keyring: true },
        );
        assert!(check_command_trusted(&keyring, &loose).is_ok());
        assert!(check_command_trusted(&account("true"), &dir.path().join("missing.toml")).is_ok());
    }

    #[test]
    fn keyring_round_trips_in_tests() {
        let account = AccountConfig::new(
            "roundtrip",
            "h",
            "u@example.com",
            PasswordSource::Keyring { keyring: true },
        );
        store("roundtrip", &Secret::new("hunter2".into())).unwrap();
        assert_eq!(resolve(&account).unwrap().expose(), "hunter2");
    }
}
