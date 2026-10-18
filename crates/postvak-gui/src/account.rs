//! The add-account form without a toolkit: what its fields mean, and the save, which runs on a thread since the OS
//! keyring may prompt. The first-run window (`lib.rs`) and the mail window (`state.rs`) both use it.
use std::fmt;
use std::sync::mpsc::{self, Receiver};

use postvak_core::config::{AccountConfig, Config, PasswordSource};
use postvak_core::credentials::Secret;
use postvak_core::message::clean;
use postvak_core::paths::Paths;

use super::html::Wake;

/// Why a save gave no answer: its thread ended without one.
pub(crate) const STOPPED: &str = "the save stopped before it finished";

/// The form's text as typed; `Fields::account` says what it means.
#[derive(Clone, Default, PartialEq)]
pub(crate) struct Fields {
    pub name: String,
    pub host: String,
    /// Empty means 993.
    pub port: String,
    pub username: String,
    pub password: String,
    /// Empty means the username.
    pub address: String,
}

impl fmt::Debug for Fields {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Fields")
            .field("name", &self.name)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .field("address", &self.address)
            .finish()
    }
}

impl Fields {
    /// The account the fields describe, with its password in the keyring, and the password to store.
    pub fn account(&self) -> Result<(AccountConfig, Secret), String> {
        let required = [
            ("account name", &self.name),
            ("IMAP host", &self.host),
            ("username", &self.username),
            ("password", &self.password),
        ];
        if let Some((label, _)) = required.iter().find(|(_, value)| value.trim().is_empty()) {
            return Err(format!("the {label} is required"));
        }
        let port = match self.port.trim() {
            "" => 993,
            text => text
                .parse()
                .map_err(|_| format!("the port must be a number, not '{}'", clean(text, false)))?,
        };
        let address = Some(self.address.trim())
            .filter(|address| !address.is_empty())
            .map(str::to_string);
        let account = AccountConfig {
            port,
            address,
            ..AccountConfig::new(
                self.name.trim(),
                self.host.trim(),
                self.username.trim(),
                PasswordSource::Keyring { keyring: true },
            )
        };
        Ok((account, Secret::new(self.password.clone())))
    }
}

/// The answer of a save in flight: the saved account, or why it was refused.
pub(crate) type Saving = Receiver<Result<AccountConfig, String>>;

/// The mail window's form while it is open.
#[derive(Default)]
pub(crate) struct AccountForm {
    /// Why the last save was refused, shown under the fields.
    pub error: Option<String>,
    pub saving: Option<Saving>,
}

/// Checks the fields, then saves on a thread, since the OS keyring may prompt; `wake` runs once it answered. The
/// daemon notices config.toml and starts the account itself, so a wrong password shows as its first sync's error.
pub(crate) fn save(paths: Paths, fields: &Fields, wake: Wake) -> Result<Saving, String> {
    let (account, secret) = fields.account()?;
    let (done, answer) = mpsc::channel();
    std::thread::spawn(move || {
        let saved = Config::load(&paths.config_file())
            .map_err(|e| e.to_string())
            .and_then(|mut config| {
                postvak_core::account::add(&mut config, &paths, account.clone(), Some(secret))
                    .map_err(|e| e.to_string())
            })
            .map(|()| account)
            .map_err(|e| clean(&e, false));
        let _ = done.send(saved);
        wake();
    });
    Ok(answer)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use postvak_core::config::Config;
    use postvak_core::paths::Paths;

    use super::*;

    pub(crate) fn filled(name: &str) -> Fields {
        Fields {
            name: name.into(),
            host: "imap.example.com".into(),
            port: String::new(),
            username: "me@example.com".into(),
            password: "hunter2".into(),
            address: String::new(),
        }
    }

    fn home() -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        (dir, paths)
    }

    #[test]
    fn debug_never_shows_the_password() {
        let text = format!("{:?}", filled("work"));
        assert!(!text.contains("hunter2"), "{text}");
        assert!(text.contains("work"));
    }

    #[test]
    fn the_fields_make_a_keyring_account_with_the_default_port() {
        let mut fields = filled("work");
        let (account, secret) = fields.account().unwrap();
        assert_eq!(account.name, "work");
        assert_eq!(account.port, 993);
        assert_eq!(account.address, None);
        assert_eq!(account.password, PasswordSource::Keyring { keyring: true });
        assert_eq!(secret.expose(), "hunter2");
        fields.port = " 143 ".into();
        fields.address = "me@example.org".into();
        let (account, _) = fields.account().unwrap();
        assert_eq!(account.port, 143);
        assert_eq!(account.address.as_deref(), Some("me@example.org"));
    }

    #[test]
    fn a_missing_field_or_a_bad_port_is_refused_by_name() {
        let mut fields = filled("work");
        fields.port = "abc".into();
        assert_eq!(
            fields.account().err().unwrap(),
            "the port must be a number, not 'abc'"
        );
        let mut fields = filled("work");
        fields.password = String::new();
        assert_eq!(fields.account().err().unwrap(), "the password is required");
    }

    #[test]
    fn save_adds_the_account_to_config_toml_and_answers_with_it() {
        let (_dir, paths) = home();
        let answer = save(paths.clone(), &filled("work"), Arc::new(|| {})).unwrap();
        let saved = answer
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
        assert_eq!(saved.name, "work");
        let config = Config::load(&paths.config_file()).unwrap();
        assert_eq!(config.accounts, [saved]);
    }

    #[test]
    fn a_name_config_toml_refuses_comes_back_as_the_answer() {
        let (_dir, paths) = home();
        let answer = save(paths.clone(), &filled("my work"), Arc::new(|| {})).unwrap();
        let refused = answer.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(
            refused.unwrap_err(),
            "config.toml: account name 'my work' may only contain letters, digits, '-' and '_'"
        );
        assert!(
            Config::load(&paths.config_file())
                .unwrap()
                .accounts
                .is_empty()
        );
    }
}
