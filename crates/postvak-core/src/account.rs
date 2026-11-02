//! Adding an account: the one writer of a new account into config.toml, for the CLI, MCP and the GUI alike; and
//! trusting an authserv-id for one already there.
use std::time::{Duration, Instant};

use anyhow::bail;

use crate::config::{self, AccountConfig, Config, ConfigError, PasswordSource, SmtpConfig};
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
/// The save merges into the file as it reads then, so what another front end saved since `config` was loaded stays,
/// and `config` becomes what the file holds. The daemon watches config.toml and starts the account's sync itself, so
/// a wrong login shows as that sync's error.
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
    *config = config::save_account(&paths.config_file(), &account)?;
    Ok(())
}

/// How long a front end waits for the daemon to run a new account; a restart waits for the old thread to end.
pub const START_TIMEOUT: Duration = Duration::from_secs(10);

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

/// Logs in as `account` through the daemon without saving anything, after checking it against `config`: the test a
/// front end runs before `add`. Without `secret`, the account's own password source answers, as for a password
/// command.
pub fn test_login(
    client: &Client,
    paths: &Paths,
    config: &Config,
    account: &AccountConfig,
    secret: Option<&Secret>,
) -> anyhow::Result<()> {
    check(config, account)?;
    let resolved;
    let secret = match secret {
        Some(secret) => secret,
        None => {
            credentials::check_command_trusted(account, &paths.config_file())?;
            resolved = credentials::resolve(account)?;
            &resolved
        }
    };
    client.test_login(account, secret)
}

/// A change to an account's SMTP server.
#[derive(Debug, Clone, PartialEq)]
pub enum SmtpChange {
    /// These settings, checked by logging in.
    Given(SmtpConfig),
    /// Whatever the guess finds now.
    Guess,
    /// The guess the daemon stored, written to config.toml.
    SaveGuess,
}

/// The SMTP server for an account not saved yet: `given` checked by logging in, else a guess; None when the guess
/// finds nothing, so the account is added without one. `secret` is the password just typed, None for a command.
pub fn smtp_for_new(
    client: &Client,
    account: &AccountConfig,
    given: Option<SmtpConfig>,
    secret: Option<&Secret>,
) -> anyhow::Result<Option<SmtpConfig>> {
    match given {
        Some(smtp) => {
            let candidate = AccountConfig {
                smtp: Some(smtp.clone()),
                ..account.clone()
            };
            client.test_smtp(&candidate, secret)?;
            Ok(Some(smtp))
        }
        None => Ok(client
            .guess_smtp(account, secret)?
            .map(|(server, _)| SmtpConfig::new(&server.host, server.tls))),
    }
}

/// Sets account `name`'s SMTP server in config.toml after checking it by logging in; returns what was saved. The only
/// path from a guess to config.toml.
pub fn set_smtp(
    config: &mut Config,
    paths: &Paths,
    client: &Client,
    name: &str,
    change: SmtpChange,
) -> anyhow::Result<SmtpConfig> {
    let account = config
        .account(name)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("no account named '{name}'"))?;
    let bare = AccountConfig {
        smtp: None,
        ..account.clone()
    };
    let smtp = match change {
        SmtpChange::Given(smtp) => {
            let candidate = AccountConfig {
                smtp: Some(smtp.clone()),
                ..account.clone()
            };
            check(config, &candidate)?;
            credentials::check_command_trusted(&candidate, &paths.config_file())?;
            client.test_smtp(&candidate, None)?;
            smtp
        }
        SmtpChange::Guess => match client.guess_smtp(&bare, None)? {
            Some((server, _)) => SmtpConfig::new(&server.host, server.tls),
            None => bail!(
                "no SMTP server answered for {}; set one with --smtp-host",
                account.host
            ),
        },
        SmtpChange::SaveGuess => {
            let state = crate::store::Store::open_account(paths, name)?.smtp_state()?;
            let Some(server) = crate::send::server_for(&bare, state.as_ref()) else {
                bail!("no stored SMTP guess for '{name}'; try --guess or --smtp-host");
            };
            let smtp = SmtpConfig::new(&server.host, server.tls);
            let candidate = AccountConfig {
                smtp: Some(smtp.clone()),
                ..account.clone()
            };
            client.test_smtp(&candidate, None)?;
            smtp
        }
    };
    // Keep what the table already said about the password and the Sent copy.
    let kept = account.smtp.clone();
    let smtp = SmtpConfig {
        username: smtp
            .username
            .or_else(|| kept.as_ref().and_then(|k| k.username.clone())),
        password: smtp
            .password
            .or_else(|| kept.as_ref().and_then(|k| k.password.clone())),
        save_sent: smtp
            .save_sent
            .or_else(|| kept.as_ref().and_then(|k| k.save_sent)),
        ..smtp
    };
    *config = config::update_account(&paths.config_file(), name, |a| a.smtp = Some(smtp.clone()))?;
    Ok(smtp)
}

/// Adds `id` to the trusted_authserv_ids of account `name` as config.toml reads now, unless it is listed already;
/// returns the config the file then holds. The user's yes to what [`rules::authserv_offers`](crate::rules::authserv_offers)
/// offers.
pub fn trust_authserv_id(paths: &Paths, name: &str, id: &str) -> Result<Config, ConfigError> {
    let id = id.trim_end_matches('.').to_lowercase();
    config::update_account(&paths.config_file(), name, |account| {
        let ids = account.trusted_authserv_ids.get_or_insert_with(Vec::new);
        if !ids
            .iter()
            .any(|known| known.trim_end_matches('.').eq_ignore_ascii_case(&id))
        {
            ids.push(id);
        }
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::config::{self, AccountConfig, Config, PasswordSource};
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
    fn trusting_an_authserv_id_adds_it_once() {
        let (_dir, paths) = home();
        let mut config = Config::default();
        add(&mut config, &paths, command_account("work"), None).unwrap();
        trust_authserv_id(&paths, "work", "MX.Google.com.").unwrap();
        let config = trust_authserv_id(&paths, "work", "mx.google.com").unwrap();
        assert_eq!(
            config.accounts[0].trusted_authserv_ids.as_deref(),
            Some(&["mx.google.com".to_string()][..])
        );
        assert_eq!(
            config.accounts,
            Config::load(&paths.config_file()).unwrap().accounts
        );
        assert!(trust_authserv_id(&paths, "home", "mx.google.com").is_err());
        assert!(
            trust_authserv_id(&paths, "work", "not a name").is_err(),
            "the id is validated like one written by hand"
        );
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
    fn add_from_a_stale_config_keeps_what_another_front_end_saved() {
        let (_dir, paths) = home();
        let mut window = Config::load(&paths.config_file()).unwrap();
        let mut cli = Config::load(&paths.config_file()).unwrap();
        add(&mut cli, &paths, command_account("work"), None).unwrap();
        config::save_theme(&paths.config_file(), config::Theme::Dark).unwrap();
        add(&mut window, &paths, command_account("home"), None).unwrap();
        let saved = Config::load(&paths.config_file()).unwrap();
        let names: Vec<&str> = saved.accounts.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["work", "home"]);
        assert_eq!(saved.ui.theme, config::Theme::Dark);
        assert_eq!(
            window.accounts, saved.accounts,
            "the caller's config is refreshed"
        );
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
    fn test_login_refuses_a_password_command_from_a_config_others_can_write() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::under(home.path());
        write_config(&paths);
        let daemon = TestDaemon::serve(home, paths.clone(), options(recording_connector(), None));
        let client = Client::connect_or_start(&daemon.paths).unwrap();
        let config = Config::load(&paths.config_file()).unwrap();
        let account = command_account("cmd");
        std::fs::set_permissions(paths.config_file(), std::fs::Permissions::from_mode(0o666))
            .unwrap();
        let err = test_login(&client, &paths, &config, &account, None).unwrap_err();
        assert!(
            format!("{err:#}").contains("refusing to run the password command"),
            "{err:#}"
        );
        std::fs::set_permissions(paths.config_file(), std::fs::Permissions::from_mode(0o600))
            .unwrap();
        test_login(&client, &paths, &config, &account, None).unwrap();
    }

    #[test]
    fn test_login_saves_nothing_and_says_why_a_login_fails() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::under(home.path());
        write_config(&paths);
        let before = std::fs::read_to_string(paths.config_file()).unwrap();
        let daemon = TestDaemon::serve(home, paths.clone(), options(recording_connector(), None));
        let client = Client::connect_or_start(&daemon.paths).unwrap();
        let config = Config::load(&paths.config_file()).unwrap();
        let account = keyring_account("tested");
        let good = Secret::new("hunter2".into());
        test_login(&client, &paths, &config, &account, Some(&good)).unwrap();
        let wrong = Secret::new("wrong".into());
        let err = test_login(&client, &paths, &config, &account, Some(&wrong)).unwrap_err();
        assert_eq!(
            format!("{err:#}"),
            "imap.example.com: login failed: [AUTHENTICATIONFAILED] Authentication failed."
        );
        assert_eq!(
            std::fs::read_to_string(paths.config_file()).unwrap(),
            before
        );
        assert!(
            credentials::resolve(&account).is_err(),
            "no password stored"
        );
        assert!(
            !client
                .status()
                .unwrap()
                .accounts
                .iter()
                .any(|a| a.name == "tested"),
            "the daemon runs no account for a test"
        );
    }

    #[test]
    fn test_login_resolves_a_password_command_and_refuses_an_invalid_account() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::under(home.path());
        write_config(&paths);
        let daemon = TestDaemon::serve(home, paths.clone(), options(recording_connector(), None));
        let client = Client::connect_or_start(&daemon.paths).unwrap();
        let config = Config::load(&paths.config_file()).unwrap();
        test_login(&client, &paths, &config, &command_account("cmd"), None).unwrap();
        let mut wrong = command_account("cmd");
        wrong.password = PasswordSource::Command {
            command: "printf wrong".into(),
        };
        assert!(test_login(&client, &paths, &config, &wrong, None).is_err());
        let err =
            test_login(&client, &paths, &config, &command_account("my cmd"), None).unwrap_err();
        assert!(
            err.to_string().contains("may only contain letters, digits"),
            "{err}"
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

    fn smtp_daemon(smtp: &crate::send::smtp::SharedSmtp) -> TestDaemon {
        TestDaemon::start_with(crate::daemon::Options {
            smtp: crate::daemon::test_support::recording_smtp(smtp.clone()),
            ..options(recording_connector(), None)
        })
    }

    /// Waits until the daemon guessed for its own two accounts, so a test's script is not taken by them.
    fn after_own_guesses(smtp: &crate::send::smtp::SharedSmtp) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while smtp.with(|s| s.logins.len()) < 2 {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn add_saves_the_guessed_smtp_server() {
        let smtp = crate::send::smtp::SharedSmtp::default();
        let daemon = smtp_daemon(&smtp);
        after_own_guesses(&smtp);
        smtp.with(|s| {
            s.script
                .push_back(Err(crate::send::SmtpError::Connect("refused".into())))
        });
        let client = Client::connect(&daemon.paths).unwrap();
        let mut account = command_account("new");
        let found = smtp_for_new(&client, &account, None, None).unwrap();
        assert_eq!(
            found,
            Some(crate::config::SmtpConfig::new(
                "smtp.example.com",
                crate::config::SmtpTls::Starttls
            ))
        );
        account.smtp = found;
        let mut config = Config::load(&daemon.paths.config_file()).unwrap();
        add(&mut config, &daemon.paths, account.clone(), None).unwrap();
        let saved = Config::load(&daemon.paths.config_file()).unwrap();
        assert_eq!(saved.account("new").unwrap().smtp, account.smtp);
    }

    #[test]
    fn add_without_a_working_guess_saves_no_smtp() {
        let smtp = crate::send::smtp::SharedSmtp::default();
        let daemon = smtp_daemon(&smtp);
        after_own_guesses(&smtp);
        smtp.with(|s| {
            s.script
                .extend((0..4).map(|_| Err(crate::send::SmtpError::Auth("no".into()))))
        });
        let client = Client::connect(&daemon.paths).unwrap();
        assert_eq!(
            smtp_for_new(&client, &command_account("new"), None, None).unwrap(),
            None
        );
        // Given settings that do not log in are refused.
        smtp.with(|s| {
            s.script
                .push_back(Err(crate::send::SmtpError::Auth("no".into())))
        });
        let given =
            crate::config::SmtpConfig::new("smtp.example.com", crate::config::SmtpTls::Implicit);
        assert!(smtp_for_new(&client, &command_account("new"), Some(given), None).is_err());
    }
}
