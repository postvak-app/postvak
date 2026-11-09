//! The Preferences dialog without a toolkit: the choices its settings offer, in words, and what each change writes to
//! config.toml through `config`.
use std::path::Path;

use postvak_core::config::{
    self, AccountConfig, Config, ConfigError, HtmlColours, Signature, SmtpTls,
};
use postvak_core::send;
use postvak_core::store::SmtpState;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Section {
    #[default]
    General,
    Accounts,
    Sending,
}

/// A change made in Preferences; an account's settings name the account.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Setting {
    CheckUpdates(bool),
    HtmlColours(HtmlColours),
    SenderImages(bool),
    /// The signature for an address, replacing its own (case ignored); an empty text removes it.
    Signature {
        address: String,
        text: String,
        on_replies: bool,
    },
    UndoSecs(u64),
    AddAlias(String, String),
    IndexBodies(String, bool),
    KeepDeleted(String, u64),
    Notify(String, bool),
    RemoveAlias(String, usize),
    RemoveAuthservId(String, usize),
    SyncEvery(String, u64),
}

/// Writes `setting` into config.toml as the file reads now; returns the config it then holds.
pub(crate) fn save(path: &Path, setting: &Setting) -> Result<Config, ConfigError> {
    let account = |name: &str, change: &dyn Fn(&mut config::AccountConfig)| {
        config::update_account(path, name, change)
    };
    match setting {
        Setting::CheckUpdates(on) => config::update_ui(path, |ui| ui.check_updates = *on),
        Setting::HtmlColours(colours) => config::update_ui(path, |ui| ui.html_colours = *colours),
        Setting::SenderImages(on) => config::update_ui(path, |ui| ui.sender_images = *on),
        Setting::Signature {
            address,
            text,
            on_replies,
        } => config::update_sending(path, |sending| {
            let known = sending
                .signatures
                .iter()
                .position(|s| s.address.eq_ignore_ascii_case(address));
            let signature = (!text.trim().is_empty()).then(|| Signature {
                address: address.clone(),
                text: text.clone(),
                on_replies: *on_replies,
            });
            match (known, signature) {
                (Some(index), Some(signature)) => sending.signatures[index] = signature,
                (Some(index), None) => {
                    sending.signatures.remove(index);
                }
                (None, Some(signature)) => sending.signatures.push(signature),
                (None, None) => {}
            }
        }),
        Setting::UndoSecs(secs) => {
            config::update_sending(path, |sending| sending.undo_secs = *secs)
        }
        Setting::AddAlias(name, alias) => {
            let alias = alias.trim();
            if !alias.contains('@') {
                return Err(ConfigError::Invalid(format!(
                    "'{alias}' is not an address; an alias is one, with * for any part"
                )));
            }
            account(name, &|a| {
                if !a
                    .aliases
                    .iter()
                    .any(|known| known.pattern().eq_ignore_ascii_case(alias))
                {
                    a.aliases.push(alias.into());
                }
            })
        }
        Setting::IndexBodies(name, on) => account(name, &|a| a.index_bodies = *on),
        Setting::KeepDeleted(name, days) => account(name, &|a| a.trash_retention_days = *days),
        Setting::Notify(name, on) => account(name, &|a| a.notify = *on),
        Setting::RemoveAlias(name, index) => account(name, &|a| {
            if *index < a.aliases.len() {
                a.aliases.remove(*index);
            }
        }),
        Setting::RemoveAuthservId(name, index) => account(name, &|a| {
            if let Some(ids) = a.trusted_authserv_ids.as_mut()
                && *index < ids.len()
            {
                ids.remove(*index);
            }
        }),
        Setting::SyncEvery(name, secs) => account(name, &|a| a.sync_interval_secs = *secs),
    }
}

/// How often an account may sync, in seconds.
const SYNC_EVERY: [u64; 7] = [60, 120, 300, 600, 900, 1800, 3600];
/// How long deleted mail may be kept, in days.
const KEEP_DELETED: [u64; 6] = [7, 14, 30, 60, 90, 365];

/// The sync intervals to choose from, with `current` among them, as (seconds, words).
pub(crate) fn sync_choices(current: u64) -> Vec<(u64, String)> {
    choices(&SYNC_EVERY, current, |secs| match secs {
        60 => "1 minute".into(),
        3600 => "1 hour".into(),
        secs if secs % 60 == 0 => format!("{} minutes", secs / 60),
        secs => format!("{secs} seconds"),
    })
}

/// How long deleted mail may be kept, with `current` among them, as (days, words).
pub(crate) fn keep_choices(current: u64) -> Vec<(u64, String)> {
    choices(&KEEP_DELETED, current, |days| match days {
        1 => "1 day".into(),
        365 => "1 year".into(),
        days => format!("{days} days"),
    })
}

/// The undo delays to choose from, as (seconds, words); config.toml holds no other.
pub(crate) fn undo_choices() -> Vec<(u64, String)> {
    config::UNDO_SECS
        .iter()
        .map(|&secs| match secs {
            0 => (secs, "Off".into()),
            secs => (secs, format!("{secs} seconds")),
        })
        .collect()
}

/// One signature per address of every account, the account's own first and then its aliases, as config.toml signs
/// each now; an address without one has an empty text.
pub(crate) fn signature_rows(config: &Config) -> Vec<Signature> {
    let mut rows: Vec<Signature> = Vec::new();
    let addresses = config.accounts.iter().flat_map(|account| {
        std::iter::once(account.address()).chain(account.aliases.iter().map(|a| a.pattern()))
    });
    for address in addresses {
        if rows
            .iter()
            .any(|row| row.address.eq_ignore_ascii_case(address))
        {
            continue;
        }
        let saved = config
            .sending
            .signatures
            .iter()
            .find(|s| s.address.eq_ignore_ascii_case(address));
        rows.push(Signature {
            address: address.to_string(),
            text: saved.map(|s| s.text.clone()).unwrap_or_default(),
            on_replies: saved.is_none_or(|s| s.on_replies),
        });
    }
    rows
}

/// Where `account` sends through, in words, and whether the guess may be saved to config.toml.
pub(crate) fn smtp_line(account: &AccountConfig, state: Option<&SmtpState>) -> (String, bool) {
    let Some(server) = send::server_for(account, state) else {
        let why = if state.is_some() {
            "Not set up: no server answered"
        } else {
            "Not set up yet"
        };
        return (why.into(), false);
    };
    let tls = match server.tls {
        SmtpTls::Implicit => "TLS",
        SmtpTls::Starttls => "STARTTLS",
    };
    let guessed = account.smtp.is_none();
    let source = if guessed {
        "guessed"
    } else {
        "from config.toml"
    };
    (
        format!("{}:{} ({tls}), {source}", server.host, server.port),
        guessed,
    )
}

/// `presets` and a `current` value config.toml may hold besides them, in order.
fn choices(presets: &[u64], current: u64, words: fn(u64) -> String) -> Vec<(u64, String)> {
    let mut values = presets.to_vec();
    if !values.contains(&current) {
        values.push(current);
        values.sort_unstable();
    }
    values.into_iter().map(|v| (v, words(v))).collect()
}

#[cfg(test)]
mod tests {
    use postvak_core::config::{Signature, SmtpConfig, SmtpTls};
    use postvak_core::store::SmtpState;

    use super::*;

    const CONFIG: &str = "# mine\n[[accounts]]\nname = \"work\"\nhost = \"imap.example.com\"\nusername = \"me@example.com\"\npassword = { keyring = true }\ntrusted_authserv_ids = [\"mx.example.com\", \"mx2.example.com\"]\n";

    #[test]
    fn a_value_besides_the_presets_is_offered_in_order() {
        let words =
            |choices: Vec<(u64, String)>| choices.into_iter().map(|(_, w)| w).collect::<Vec<_>>();
        assert_eq!(
            words(sync_choices(90)),
            [
                "1 minute",
                "90 seconds",
                "2 minutes",
                "5 minutes",
                "10 minutes",
                "15 minutes",
                "30 minutes",
                "1 hour"
            ]
        );
        assert_eq!(sync_choices(120).len(), SYNC_EVERY.len());
        assert_eq!(words(keep_choices(1))[..2], ["1 day", "7 days"]);
    }

    #[test]
    fn each_setting_writes_config_toml_and_keeps_its_comments() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, CONFIG).unwrap();
        let work = || "work".to_string();
        for setting in [
            Setting::CheckUpdates(true),
            Setting::HtmlColours(HtmlColours::Original),
            Setting::SenderImages(true),
            Setting::Notify(work(), false),
            Setting::IndexBodies(work(), false),
            Setting::SyncEvery(work(), 600),
            Setting::KeepDeleted(work(), 90),
            Setting::AddAlias(work(), " me@example.org ".into()),
            Setting::AddAlias(work(), "ME@example.org".into()),
            Setting::AddAlias(work(), "*@shop.example.com".into()),
            Setting::RemoveAlias(work(), 0),
            Setting::RemoveAuthservId(work(), 0),
        ] {
            save(&path, &setting).unwrap();
        }
        let cfg = Config::load(&path).unwrap();
        assert!(cfg.ui.check_updates && cfg.ui.sender_images);
        assert_eq!(cfg.ui.html_colours, HtmlColours::Original);
        let account = &cfg.accounts[0];
        assert!(!account.notify && !account.index_bodies);
        assert_eq!(
            (account.sync_interval_secs, account.trash_retention_days),
            (600, 90)
        );
        assert_eq!(account.aliases, ["*@shop.example.com"]);
        assert_eq!(
            account.trusted_authserv_ids.as_deref(),
            Some(&["mx2.example.com".to_string()][..])
        );
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .starts_with("# mine\n")
        );
    }

    #[test]
    fn an_alias_that_is_no_address_is_refused_and_nothing_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, CONFIG).unwrap();
        let refused = save(
            &path,
            &Setting::AddAlias("work".into(), "example.org".into()),
        );
        assert!(
            matches!(refused, Err(ConfigError::Invalid(_))),
            "{refused:?}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), CONFIG);
    }

    #[test]
    fn undo_choices_are_the_spec_s_five() {
        assert_eq!(
            undo_choices(),
            [
                (0, "Off".to_string()),
                (5, "5 seconds".to_string()),
                (10, "10 seconds".to_string()),
                (20, "20 seconds".to_string()),
                (30, "30 seconds".to_string()),
            ]
        );
    }

    #[test]
    fn smtp_line_says_where_the_server_comes_from() {
        let mut account = Config::parse(CONFIG).unwrap().accounts.remove(0);
        assert_eq!(
            smtp_line(&account, None),
            ("Not set up yet".to_string(), false)
        );
        let mut state = SmtpState {
            host: None,
            port: None,
            tls: None,
            guessed_for_host: account.host.clone(),
            guessed_for_user: account.username.clone(),
            tried_at: 0,
            size: None,
            eight_bit_mime: false,
            sent_copy_problem: None,
        };
        assert_eq!(
            smtp_line(&account, Some(&state)),
            ("Not set up: no server answered".to_string(), false)
        );
        state = SmtpState {
            host: Some("smtp.x.org".into()),
            port: Some(587),
            tls: Some(SmtpTls::Starttls),
            guessed_for_host: account.host.clone(),
            guessed_for_user: account.username.clone(),
            tried_at: 0,
            size: None,
            eight_bit_mime: false,
            sent_copy_problem: None,
        };
        assert_eq!(
            smtp_line(&account, Some(&state)),
            ("smtp.x.org:587 (STARTTLS), guessed".to_string(), true)
        );
        account.smtp = Some(SmtpConfig::new("smtp.x.org", SmtpTls::Implicit));
        assert_eq!(
            smtp_line(&account, Some(&state)),
            ("smtp.x.org:465 (TLS), from config.toml".to_string(), false)
        );
    }

    #[test]
    fn a_signature_setting_adds_replaces_and_removes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, CONFIG).unwrap();
        let signature = |address: &str, text: &str, on_replies| Setting::Signature {
            address: address.into(),
            text: text.into(),
            on_replies,
        };
        save(&path, &signature("me@example.com", "Me\nmore", true)).unwrap();
        save(&path, &signature("*@shop.example.com", "Shop", true)).unwrap();
        let config = save(&path, &signature("ME@example.com", "Just me", false)).unwrap();
        assert_eq!(
            config.sending.signatures,
            [
                Signature {
                    address: "ME@example.com".into(),
                    text: "Just me".into(),
                    on_replies: false,
                },
                Signature {
                    address: "*@shop.example.com".into(),
                    text: "Shop".into(),
                    on_replies: true,
                },
            ]
        );
        let config = save(&path, &signature("me@example.com", "", true)).unwrap();
        assert_eq!(config.sending.signatures.len(), 1);
        let config = save(&path, &Setting::UndoSecs(30)).unwrap();
        assert_eq!(config.sending.undo_secs, 30);
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .starts_with("# mine\n")
        );
    }

    mod window {
        use postvak_core::config::{self, Config, SmtpConfig, SmtpTls};
        use slint::{ComponentHandle, Model};

        use crate::state::{Dialog, UiAction, UpdateCheck};
        use crate::test_support::Fixture;
        use crate::ui::{MenuAction, Preferences};

        use super::super::{Section, Setting};

        #[test]
        fn preferences_opens_from_the_menu_and_saves_each_switch_at_once() {
            let fx = Fixture::new(&["work"]);
            let (open, _wires) = fx.window();
            open.window().invoke_menu_item(MenuAction::Preferences);
            assert_eq!(open.find("Preferences").len(), 1);
            open.click("Check for updates");
            assert!(
                Config::load(&fx.paths.config_file())
                    .unwrap()
                    .ui
                    .check_updates
            );

            open.click("Accounts");
            assert_eq!(open.find("work@example.com").len(), 1);
            open.click("Notifications");
            let config = std::fs::read_to_string(fx.paths.config_file()).unwrap();
            assert!(config.starts_with("# test accounts\n"), "{config}");
            assert!(!Config::load(&fx.paths.config_file()).unwrap().accounts[0].notify);

            open.press("escape");
            assert!(open.find("Preferences").is_empty());
        }

        #[test]
        fn aliases_and_authserv_ids_are_added_and_removed_in_the_window() {
            let fx = Fixture::new(&["work"]);
            fx.append_config("trusted_authserv_ids = [\"mx.example.com\"]\n");
            let (open, _wires) = fx.window();
            open.window().invoke_menu_item(MenuAction::Preferences);
            open.click("Accounts");
            let window = open.window();
            let prefs = window.global::<Preferences>();
            prefs.invoke_add_alias("me@example.org".into());
            open.tick();
            open.click("Remove alias me@example.org");
            open.click("Remove authserv-id mx.example.com");
            let account = &Config::load(&fx.paths.config_file()).unwrap().accounts[0];
            assert!(account.aliases.is_empty());
            assert_eq!(account.trusted_authserv_ids.as_deref(), Some(&[][..]));

            prefs.invoke_add_alias("example.org".into());
            open.tick();
            assert!(open.with(|live| live.app.preferences.as_ref().unwrap().error.is_some()));
        }

        #[test]
        fn a_saved_change_shows_in_the_window_and_is_not_taken_for_an_outside_edit() {
            let fx = Fixture::new(&["work"]);
            let (mut driver, _wires) = fx.driver();
            driver.apply(UiAction::TogglePreferences);
            driver.apply(UiAction::PreferencesSection(Section::Accounts));
            driver.apply(UiAction::SetPreference(Setting::CheckUpdates(true)));
            assert!(!matches!(driver.app.update, UpdateCheck::Done(_)));
            driver.apply(UiAction::SetPreference(Setting::KeepDeleted(
                "work".into(),
                7,
            )));
            assert_eq!(driver.app.retention["work"], 7);
            driver.apply(UiAction::SetPreference(Setting::AddAlias(
                "work".into(),
                "*@example.org".into(),
            )));
            let identity = driver.app.accounts[0].identity.as_ref().unwrap();
            assert!(identity.is_me("shop@example.org"));
            driver.tick();
            assert!(!driver.app.config_changed);
            driver.apply(UiAction::Close(Dialog::Preferences));
            assert!(driver.app.preferences.is_none());
        }

        #[test]
        fn the_sending_page_saves_undo_and_a_signature() {
            let fx = Fixture::new(&["work"]);
            let (open, _wires) = fx.window();
            open.window().invoke_menu_item(MenuAction::Preferences);
            open.click("Sending");
            open.window().global::<Preferences>().invoke_undo_chosen(4);
            open.tick();
            open.get("Signature for work@example.com")
                .set_accessible_value("Work\nwork@example.com");
            open.tick();
            open.click("Save signature for work@example.com");
            let config = Config::load(&fx.paths.config_file()).unwrap();
            assert_eq!(config.sending.undo_secs, 30);
            let signature = &config.sending.signatures[0];
            assert_eq!(
                (signature.address.as_str(), signature.text.as_str()),
                ("work@example.com", "Work\nwork@example.com")
            );
            open.with(|live| assert_eq!(live.app.sending, config.sending));
        }

        #[test]
        fn a_failed_guess_says_why_and_writes_nothing() {
            let fx = Fixture::new(&["work"]);
            let before = std::fs::read(fx.paths.config_file()).unwrap();
            let (open, _wires) = fx.window();
            open.with(|live| live.app.set_smtp = |_, _, _| Err("no SMTP server answered".into()));
            open.window().invoke_menu_item(MenuAction::Preferences);
            open.click("Sending");
            open.click("Guess again");
            open.tick_until(|open| open.with(|live| live.app.setting_smtp.is_none()));
            let window = open.window();
            let shown = window.global::<Preferences>();
            assert_eq!(shown.get_error(), "");
            let server = shown.get_smtp().row_data(0).unwrap();
            assert_eq!(server.error, "Not saved: no SMTP server answered");
            assert_eq!(std::fs::read(fx.paths.config_file()).unwrap(), before);
            open.with(|live| {
                live.app
                    .apply(UiAction::PreferencesSection(Section::Accounts));
                assert_eq!(live.app.preferences.as_ref().unwrap().smtp_error, None);
            });
        }

        #[test]
        fn a_saved_guess_shows_the_new_config_and_ends_the_change() {
            let fx = Fixture::new(&["work"]);
            let (mut driver, _wires) = fx.driver();
            driver.app.set_smtp = |paths, name, _| {
                config::update_account(&paths.config_file(), name, |a| {
                    a.smtp = Some(SmtpConfig::new("smtp.example.com", SmtpTls::Implicit));
                })
                .map_err(|e| e.to_string())
            };
            driver.apply(UiAction::TogglePreferences);
            let sending = driver.app.sending.clone();
            driver.apply(UiAction::SmtpGuessAgain("work".into()));
            driver.tick_until(|app| app.setting_smtp.is_none());
            let prefs = driver.app.preferences.as_ref().unwrap();
            let account = &prefs.config.accounts[0];
            assert_eq!(
                super::super::smtp_line(account, prefs.smtp.get("work")).0,
                "smtp.example.com:465 (TLS), from config.toml"
            );
            assert_eq!((&prefs.error, &prefs.smtp_error), (&None, &None));
            assert_eq!(driver.app.sending, sending);
            assert!(!driver.app.config_changed);
        }
    }
}
