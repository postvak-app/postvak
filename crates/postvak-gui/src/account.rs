//! The add-account form without a toolkit: what its fields mean, and the save, which runs on a thread since the OS
//! keyring may prompt and the first sync that tests the login takes a while. The first-run window (`lib.rs`) and the
//! mail window (`state.rs`) both use it.
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

/// What a save in flight reports, in order: `Saved` once config.toml holds the account, then whether its first sync
/// passed; or `Refused` when nothing was saved.
#[derive(Debug)]
pub(crate) enum Step {
    Refused(String),
    Saved(AccountConfig),
    Synced,
    /// The account stays saved, as with `postvak account add`; saving again replaces it.
    Failed(String),
}

impl Step {
    /// What the form says about a failed first sync of `name`.
    pub fn failed(name: &str, why: &str) -> String {
        let why = why.trim_end_matches('.');
        format!(
            "'{name}' is saved, but its first sync failed: {why}. Fix the fields and save again to replace it."
        )
    }
}

/// The steps of a save in flight.
pub(crate) type Saving = Receiver<Step>;

/// Tests a saved account's login by name; tests swap it so no daemon starts.
pub(crate) type Check = fn(&Paths, &str) -> Result<(), String>;

/// The mail window's form while it is open.
#[derive(Default)]
pub(crate) struct AccountForm {
    /// Why the last save was refused, shown under the fields.
    pub error: Option<String>,
    pub saving: Option<Saving>,
    /// The save in flight has written the account and now tests its login.
    pub testing: bool,
}

/// What the form says while a save runs.
pub(crate) fn progress(testing: bool) -> &'static str {
    match testing {
        true => "Testing the login…",
        false => "Saving…",
    }
}

/// Checks the fields, then on a thread saves and runs `check`, the first sync that tests the login; `wake` runs once
/// it answered. The daemon notices config.toml and starts the account itself.
pub(crate) fn save(
    paths: Paths,
    fields: &Fields,
    wake: Wake,
    check: Check,
) -> Result<Saving, String> {
    let (account, secret) = fields.account()?;
    let (done, answer) = mpsc::channel();
    std::thread::spawn(move || {
        let saved = Config::load(&paths.config_file())
            .map_err(|e| e.to_string())
            .and_then(|mut config| {
                postvak_core::account::add(&mut config, &paths, account.clone(), Some(secret))
                    .map_err(|e| e.to_string())
            })
            .map_err(|e| clean(&e, false));
        if let Err(why) = saved {
            let _ = done.send(Step::Refused(why));
            return wake();
        }
        let name = account.name.clone();
        let _ = done.send(Step::Saved(account));
        wake();
        let _ = done.send(match check(&paths, &name) {
            Ok(()) => Step::Synced,
            Err(why) => Step::Failed(Step::failed(&name, &why)),
        });
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
    use std::time::Instant;

    use crate::state::View;
    use crate::test_support::{Fixture, Open};
    use crate::ui::MenuAction;
    use postvak_core::sync::Event;

    fn fill_window(open: &Open, name: &str, host: &str) {
        let values = [
            ("Account name", name),
            ("IMAP host", host),
            ("Username", "me@example.com"),
            ("Password", "hunter2"),
        ];
        for (label, value) in values {
            open.get(label).set_accessible_value(value);
        }
    }

    /// Clicks Save account and ticks until the save thread answered, for at most 5 s of real time.
    fn save_in(open: &Open) {
        open.click("Save account");
        let saving = || {
            open.with(|live| {
                live.app
                    .account_form
                    .as_ref()
                    .is_some_and(|form| form.saving.is_some())
            })
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while saving() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
            open.tick();
        }
        assert!(!saving(), "the save did not finish");
    }

    fn names(open: &Open) -> Vec<String> {
        open.with(|live| live.app.accounts.iter().map(|a| a.name.clone()).collect())
    }

    #[test]
    fn add_account_saves_it_and_its_sync_shows_on_its_line() {
        let fx = Fixture::new(&["work"]);
        let (open, wires) = fx.window();
        open.window().invoke_menu_item(MenuAction::AddAccount);
        assert_eq!(open.find("Add account").len(), 1);
        fill_window(&open, "home", "imap.example.com");
        save_in(&open);
        assert!(open.find("Add account").is_empty());
        assert_eq!(names(&open), ["work", "home"]);
        assert_eq!(
            open.with(|live| live.app.view.clone()),
            View::Folder {
                account: 1,
                folder: "INBOX".into()
            }
        );
        let config = Config::load(&fx.paths.config_file()).unwrap();
        assert!(config.accounts.iter().any(|a| a.name == "home"));
        // The daemon's first sync of the new account reaches its line.
        let refused = Event::Error {
            account: "home".into(),
            message: "login failed".into(),
        };
        wires.events.send(refused).unwrap();
        open.tick();
        assert_eq!(open.find("home: login failed").len(), 1);
        open.advance(5.0);
        assert!(
            open.labels_containing("reopen the window").is_empty(),
            "the window's own save is no outside edit"
        );
    }

    #[test]
    fn a_failed_first_sync_keeps_the_form_open_with_the_account_listed() {
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = fx.window();
        open.with(|live| live.app.check_login = |_, _| Err("login failed".into()));
        open.window().invoke_menu_item(MenuAction::AddAccount);
        fill_window(&open, "home", "imap.example.com");
        save_in(&open);
        let why = "'home' is saved, but its first sync failed: login failed. Fix the fields and save again to replace it.";
        assert_eq!(open.find(why).len(), 1);
        assert_eq!(names(&open), ["work", "home"], "saved, as with the CLI");
        open.press("escape");
        assert!(open.find("Add account").is_empty());
        assert_eq!(names(&open), ["work", "home"]);
    }

    #[test]
    fn escape_during_the_first_sync_closes_the_form_and_keeps_the_account() {
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = fx.window();
        open.with(|live| {
            live.app.check_login = |_, _| {
                std::thread::sleep(Duration::from_secs(60));
                Ok(())
            }
        });
        open.window().invoke_menu_item(MenuAction::AddAccount);
        fill_window(&open, "home", "imap.example.com");
        open.click("Save account");
        let deadline = Instant::now() + Duration::from_secs(5);
        while open.find("Testing the login…").is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
            open.tick();
        }
        assert_eq!(open.find("Testing the login…").len(), 1);
        assert_eq!(names(&open), ["work", "home"]);
        open.press("escape");
        assert!(open.find("Add account").is_empty());
    }

    #[test]
    fn saving_an_existing_name_replaces_the_account() {
        let fx = Fixture::new(&["work"]);
        let (open, wires) = fx.window();
        let refused = Event::Error {
            account: "work".into(),
            message: "login failed".into(),
        };
        wires.events.send(refused).unwrap();
        open.tick();
        open.window().invoke_menu_item(MenuAction::AddAccount);
        fill_window(&open, "work", "imap.example.org");
        save_in(&open);
        assert_eq!(names(&open), ["work"]);
        assert!(
            open.labels_containing("login failed").is_empty(),
            "the old account's error goes with it"
        );
        let config = Config::load(&fx.paths.config_file()).unwrap();
        assert_eq!(config.accounts.len(), 1);
        assert_eq!(config.accounts[0].host, "imap.example.org");
    }

    #[test]
    fn a_refused_save_keeps_the_form_open_and_says_why() {
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = fx.window();
        open.window().invoke_menu_item(MenuAction::AddAccount);
        fill_window(&open, "my home", "imap.example.com");
        save_in(&open);
        let why =
            "config.toml: account name 'my home' may only contain letters, digits, '-' and '_'";
        assert_eq!(open.find(why).len(), 1);
        assert_eq!(names(&open), ["work"]);
    }

    #[test]
    fn escape_closes_the_form_and_letters_typed_stay_text() {
        let fx = Fixture::new(&["work"]);
        fx.add("work", crate::test_support::message("INBOX", 1, "hello"));
        let (open, wires) = fx.window();
        open.window().invoke_menu_item(MenuAction::AddAccount);
        open.press("e");
        assert!(
            wires.sent().is_empty(),
            "e is text in the form, not archive"
        );
        open.press("escape");
        assert!(open.find("Add account").is_empty());
    }

    fn two_messages() -> Fixture {
        let fx = Fixture::new(&["work"]);
        fx.add("work", crate::test_support::message("INBOX", 1, "hello"));
        fx.add("work", crate::test_support::message("INBOX", 2, "again"));
        fx
    }

    fn cursor(open: &Open) -> usize {
        open.with(|live| live.app.list.cursor)
    }

    #[test]
    fn the_keys_work_again_after_escape_closes_the_form() {
        let fx = two_messages();
        let (open, _wires) = fx.window();
        open.window().invoke_menu_item(MenuAction::AddAccount);
        open.press("escape");
        open.press("j");
        assert_eq!(cursor(&open), 1);
    }

    #[test]
    fn the_keys_work_again_after_a_save_closes_the_form() {
        let fx = two_messages();
        let (open, _wires) = fx.window();
        open.window().invoke_menu_item(MenuAction::AddAccount);
        // Draws the form, so its first field takes the keyboard before the save.
        open.tick();
        fill_window(&open, "work", "imap.example.org");
        save_in(&open);
        open.press("j");
        assert_eq!(cursor(&open), 1);
    }

    fn filled(name: &str) -> Fields {
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

    fn step(answer: &Saving) -> Step {
        answer.recv_timeout(Duration::from_secs(5)).unwrap()
    }

    #[test]
    fn save_adds_the_account_to_config_toml_then_tests_it() {
        let (_dir, paths) = home();
        let answer = save(
            paths.clone(),
            &filled("work"),
            Arc::new(|| {}),
            |paths, name| {
                // The check runs once config.toml holds the account, as the daemon starts it from there.
                let config = Config::load(&paths.config_file()).map_err(|e| e.to_string())?;
                match config.accounts.iter().any(|a| a.name == name) {
                    true => Ok(()),
                    false => Err("not saved yet".into()),
                }
            },
        )
        .unwrap();
        let Step::Saved(saved) = step(&answer) else {
            panic!("the account is saved first");
        };
        assert_eq!(saved.name, "work");
        assert!(matches!(step(&answer), Step::Synced));
        let config = Config::load(&paths.config_file()).unwrap();
        assert_eq!(config.accounts, [saved]);
    }

    #[test]
    fn a_failed_check_names_the_saved_account_and_why() {
        let (_dir, paths) = home();
        let answer = save(paths.clone(), &filled("work"), Arc::new(|| {}), |_, _| {
            Err("login failed.".into())
        })
        .unwrap();
        assert!(matches!(step(&answer), Step::Saved(_)));
        let Step::Failed(why) = step(&answer) else {
            panic!("the check failed");
        };
        assert_eq!(
            why,
            "'work' is saved, but its first sync failed: login failed. Fix the fields and save again to replace it."
        );
        assert_eq!(
            Config::load(&paths.config_file()).unwrap().accounts.len(),
            1
        );
    }

    #[test]
    fn a_name_config_toml_refuses_comes_back_as_the_answer() {
        let (_dir, paths) = home();
        let answer = save(
            paths.clone(),
            &filled("my work"),
            Arc::new(|| {}),
            |_, _| panic!("nothing saved, so nothing to test"),
        )
        .unwrap();
        let Step::Refused(why) = step(&answer) else {
            panic!("refused");
        };
        assert_eq!(
            why,
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
