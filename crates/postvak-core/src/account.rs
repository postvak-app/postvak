//! Adding an account: the one writer of a new account into config.toml, for the CLI, MCP and the GUI alike.
use std::time::{Duration, Instant};

use anyhow::bail;

use crate::config::{AccountConfig, Config, ConfigError, PasswordSource};
use crate::credentials::{self, CredentialError, Secret};
use crate::daemon::Client;
use crate::paths::Paths;

/// How often `await_started` asks the daemon again.
const POLL: Duration = Duration::from_millis(100);

#[derive(Debug, thiserror::Error)]
pub enum AddError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Credential(#[from] CredentialError),
    #[error("account '{0}' keeps its password in the keyring, so a password is required")]
    MissingSecret(String),
}

fn with_account(config: &Config, account: &AccountConfig) -> Config {
    let mut candidate = config.clone();
    match candidate
        .accounts
        .iter_mut()
        .find(|a| a.name == account.name)
    {
        Some(existing) => *existing = account.clone(),
        None => candidate.accounts.push(account.clone()),
    }
    candidate
}

/// Validates `config` with `account` replacing any account of the same name; touches neither disk nor `config`.
pub fn check(config: &Config, account: &AccountConfig) -> Result<(), ConfigError> {
    with_account(config, account).validate()
}

/// Validates the account, stores a keyring password, then saves config.toml; an account of the same name is replaced.
/// The daemon watches config.toml and starts the account's sync itself, so a wrong login shows as that sync's error.
pub fn add(
    config: &mut Config,
    paths: &Paths,
    account: AccountConfig,
    secret: Option<Secret>,
) -> Result<(), AddError> {
    check(config, &account)?;
    if matches!(account.password, PasswordSource::Keyring { .. }) {
        let secret = secret.ok_or_else(|| AddError::MissingSecret(account.name.clone()))?;
        credentials::store(&account.name, &secret)?;
    }
    let candidate = with_account(config, &account);
    candidate.save(&paths.config_file())?;
    *config = candidate;
    Ok(())
}

/// Waits until the daemon lists `name` as running. Each status request makes the daemon apply a changed config.toml
/// first, and an account restarting on a changed config is not listed until its new thread runs, so a command sent
/// after this reaches the thread of the config just saved.
pub fn await_started(client: &Client, name: &str, timeout: Duration) -> anyhow::Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        if client.status()?.accounts.iter().any(|a| a.name == name) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!(
                "the daemon did not pick up account '{name}' within {} s",
                timeout.as_secs_f32()
            );
        }
        std::thread::sleep(POLL);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::config::{AccountConfig, Config, PasswordSource};
    use crate::credentials::{self, Secret};
    use crate::daemon::Client;
    use crate::daemon::test_support::{TestDaemon, options, recording_connector, write_config};
    use crate::paths::Paths;

    fn home() -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        (dir, paths)
    }

    fn keyring_account(name: &str) -> AccountConfig {
        AccountConfig::new(
            name,
            "imap.example.com",
            "me@example.com",
            PasswordSource::Keyring { keyring: true },
        )
    }

    fn command_account(name: &str) -> AccountConfig {
        AccountConfig::new(
            name,
            "imap.example.com",
            "me@example.com",
            PasswordSource::Command {
                command: "printf x".into(),
            },
        )
    }

    #[test]
    fn add_saves_a_command_account_without_a_secret() {
        let (_dir, paths) = home();
        let mut config = Config::default();
        add(&mut config, &paths, command_account("work"), None).unwrap();
        let saved = Config::load(&paths.config_file()).unwrap();
        assert_eq!(saved.accounts.len(), 1);
        assert_eq!(saved.accounts[0].name, "work");
        assert_eq!(config.accounts, saved.accounts);
    }

    #[test]
    fn add_stores_the_keyring_secret_then_saves() {
        let (_dir, paths) = home();
        let mut config = Config::default();
        let account = keyring_account("home");
        add(
            &mut config,
            &paths,
            account.clone(),
            Some(Secret::new("hunter2".into())),
        )
        .unwrap();
        assert_eq!(credentials::resolve(&account).unwrap().expose(), "hunter2");
        assert!(paths.config_file().exists());
    }

    #[test]
    fn add_refuses_a_keyring_account_without_a_secret_and_writes_nothing() {
        let (_dir, paths) = home();
        let mut config = Config::default();
        let err = add(&mut config, &paths, keyring_account("nosecret"), None).unwrap_err();
        assert!(
            matches!(err, AddError::MissingSecret(ref name) if name == "nosecret"),
            "{err}"
        );
        assert!(!paths.config_file().exists());
        assert!(config.accounts.is_empty());
    }

    #[test]
    fn add_replaces_an_account_of_the_same_name() {
        let (_dir, paths) = home();
        let mut config = Config::default();
        add(&mut config, &paths, command_account("work"), None).unwrap();
        add(&mut config, &paths, command_account("home"), None).unwrap();
        let mut changed = command_account("work");
        changed.port = 1993;
        add(&mut config, &paths, changed, None).unwrap();
        let saved = Config::load(&paths.config_file()).unwrap();
        assert_eq!(config.accounts, saved.accounts);
        let names: Vec<&str> = config.accounts.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["work", "home"], "replaced in place");
        assert_eq!(config.accounts[0].port, 1993);
    }

    #[test]
    fn add_leaves_config_untouched_on_a_validation_error() {
        let (_dir, paths) = home();
        let mut config = Config::default();
        add(&mut config, &paths, command_account("work"), None).unwrap();
        let before = std::fs::read_to_string(paths.config_file()).unwrap();
        let err = add(&mut config, &paths, command_account("my work"), None).unwrap_err();
        assert!(
            err.to_string().contains("may only contain letters, digits"),
            "{err}"
        );
        assert_eq!(
            std::fs::read_to_string(paths.config_file()).unwrap(),
            before
        );
        assert_eq!(config.accounts.len(), 1);
    }

    #[test]
    fn check_refuses_an_invalid_account_and_leaves_config_alone() {
        let config = Config::default();
        let err = check(&config, &command_account("my work")).unwrap_err();
        assert!(
            err.to_string().contains("may only contain letters, digits"),
            "{err}"
        );
        assert!(check(&config, &command_account("work")).is_ok());
        assert!(config.accounts.is_empty());
    }

    /// Port 1 cannot connect; any other port can.
    fn port_one_fails() -> crate::engine::Connector {
        std::sync::Arc::new(|account: &AccountConfig| {
            if account.port == 1 {
                return Err(crate::sync::SyncError::Io(std::io::Error::other(
                    "connection refused",
                )));
            }
            recording_connector()(account)
        })
    }

    #[test]
    fn adding_an_account_again_syncs_with_its_new_config() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::under(home.path());
        write_config(&paths);
        let daemon = TestDaemon::serve(home, paths.clone(), options(port_one_fails(), None));
        let client = Client::connect_or_start(&daemon.paths).unwrap();
        let mut config = Config::load(&paths.config_file()).unwrap();
        let mut account = command_account("late");
        account.port = 1;
        add(&mut config, &paths, account.clone(), None).unwrap();
        await_started(&client, "late", Duration::from_secs(10)).unwrap();
        let first = client.request("late", crate::sync::Command::SyncNow);
        assert!(first.is_err(), "{first:?}");

        account.port = 2;
        add(&mut config, &paths, account, None).unwrap();
        await_started(&client, "late", Duration::from_secs(10)).unwrap();
        let second = client.request("late", crate::sync::Command::SyncNow);
        assert!(
            matches!(second, Ok(crate::sync::Event::Synced { .. })),
            "{second:?}"
        );
    }

    #[test]
    fn await_started_waits_for_the_daemon_to_pick_up_the_account() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::under(home.path());
        write_config(&paths);
        let daemon = TestDaemon::serve(home, paths.clone(), options(recording_connector(), None));
        let client = Client::connect_or_start(&daemon.paths).unwrap();
        let mut config = Config::load(&paths.config_file()).unwrap();
        add(&mut config, &paths, command_account("late"), None).unwrap();
        await_started(&client, "late", Duration::from_secs(10)).unwrap();
        assert!(
            client
                .status()
                .unwrap()
                .accounts
                .iter()
                .any(|a| a.name == "late")
        );
        let err = await_started(&client, "never", Duration::from_millis(300)).unwrap_err();
        assert!(err.to_string().contains("never"), "{err}");
    }
}
