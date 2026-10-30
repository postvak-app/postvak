//! The Preferences dialog without a toolkit: the choices its settings offer, in words, and what each change writes to
//! config.toml through `config`.
use std::path::Path;

use postvak_core::config::{self, Config, ConfigError, HtmlColours};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Section {
    #[default]
    General,
    Accounts,
}

/// A change made in Preferences; an account's settings name the account.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Setting {
    CheckUpdates(bool),
    HtmlColours(HtmlColours),
    SenderImages(bool),
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
                    .any(|known| known.eq_ignore_ascii_case(alias))
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

    mod window {
        use postvak_core::config::Config;
        use slint::ComponentHandle;

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
    }
}
