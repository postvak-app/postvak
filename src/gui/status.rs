//! The status bar without a toolkit: a line per account from its error or latest activity, its progress, the mark
//! before it, the sync button's hint and the version. The window draws the bar, the history window and the theme
//! switch.
use crate::sync::Activity;

use super::state::{Account, App};

/// A line of the status bar: the window's error, or an account's state.
#[derive(Debug, PartialEq)]
pub(crate) struct Line {
    pub text: String,
    pub dot: Option<Dot>,
    /// Drawn in the error colour.
    pub failed: bool,
    pub progress: Option<f32>,
}

/// The window's error first, then a line per account.
pub(crate) fn lines(app: &App) -> Vec<Line> {
    let error = app.error.iter().map(|error| Line {
        text: error.clone(),
        dot: None,
        failed: true,
        progress: None,
    });
    let accounts = app.accounts.iter().map(|account| Line {
        text: account_line(account),
        dot: dot(account),
        failed: failed(account),
        progress: account.activity.as_ref().and_then(progress),
    });
    error.chain(accounts).collect()
}

/// The tray icon's hover text: the unread mail in every INBOX, as All inboxes counts it.
pub(crate) fn tray_hint(app: &App) -> String {
    match super::folders::inbox_unread(&app.accounts) {
        0 => "Postvak".into(),
        unread => format!(
            "Postvak · {} unread",
            super::numbers::count(u64::from(unread))
        ),
    }
}

/// The history window's lines, newest first.
pub(crate) fn history(app: &App) -> Vec<String> {
    let lines = app.history.iter().rev();
    lines
        .map(|line| format!("{}  {}", crate::time::clock(line.at), line.text))
        .collect()
}

// Fixed in tests so a release bump does not change every GUI snapshot.
pub(crate) const VERSION: &str = if cfg!(test) {
    "0.0.0"
} else {
    env!("CARGO_PKG_VERSION")
};

/// The account's status line: its error or its latest activity; plus queued commands.
pub(crate) fn account_line(account: &Account) -> String {
    let state = match (&account.error, &account.activity) {
        (Some(error), _) => error.clone(),
        (None, Some(activity)) => activity.to_string(),
        (None, None) => "starting…".into(),
    };
    let mut line = format!("{}: {state}", account.name);
    if account.queued > 0 {
        line.push_str(&format!(" · {} queued", account.queued));
    }
    line
}

/// How far a header or body fetch has come, 0 to 1, while one runs.
pub(crate) fn progress(activity: &Activity) -> Option<f32> {
    match activity {
        Activity::FetchingBodies { done, total, .. }
        | Activity::FetchingHeaders { done, total, .. }
            if *total > 0 =>
        {
            Some(*done as f32 / *total as f32)
        }
        _ => None,
    }
}

/// An error, or a sync thread that is not running: the line shows in the error colour.
pub(crate) fn failed(account: &Account) -> bool {
    account.error.is_some() || matches!(account.activity, Some(Activity::NotRunning { .. }))
}

/// The mark before an account's line.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Dot {
    /// A ring in the error colour.
    Failed,
    /// A filled brass dot.
    UpToDate,
    /// A brass ring.
    Offline,
}

/// A ring in the error colour for an error, a brass ring while offline, a brass dot once up to date, none while it
/// syncs.
pub(crate) fn dot(account: &Account) -> Option<Dot> {
    if failed(account) {
        return Some(Dot::Failed);
    }
    match account.activity {
        Some(Activity::Idle { .. }) => Some(Dot::UpToDate),
        Some(Activity::Offline { .. }) => Some(Dot::Offline),
        _ => None,
    }
}

/// The sync button's hover text, with the platform's shortcut.
pub(crate) fn sync_key() -> &'static str {
    if cfg!(target_os = "macos") {
        "Sync now · Cmd+R"
    } else {
        "Sync now · Ctrl+R"
    }
}

/// The newer release's link text.
pub(crate) fn update_text(version: &str) -> String {
    format!("↑ v{version} available")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gui::state::Account;

    fn account() -> Account {
        Account {
            activity: None,
            error: None,
            folders: Vec::new(),
            name: "work".into(),
            pending: Default::default(),
            queued: 0,
            store: Err("no store".into()),
        }
    }

    #[test]
    fn an_up_to_date_account_gets_a_brass_dot_and_an_error_wins() {
        let mut account = account();
        assert!(!failed(&account));
        assert_eq!(dot(&account), None);
        account.activity = Some(Activity::Idle {
            since: 1_790_000_000,
        });
        assert_eq!(dot(&account), Some(Dot::UpToDate));
        account.error = Some("boom".into());
        assert!(failed(&account));
        assert_eq!(dot(&account), Some(Dot::Failed));
        assert_eq!(account_line(&account), "work: boom");
    }

    #[test]
    fn fetching_headers_shows_the_counts_and_progress() {
        let mut account = account();
        let activity = Activity::FetchingHeaders {
            folder: "INBOX".into(),
            done: 12_500,
            total: 48_213,
        };
        assert_eq!(progress(&activity), Some(12_500.0 / 48_213.0));
        account.activity = Some(activity);
        account.queued = 2;
        assert_eq!(
            account_line(&account),
            "work: INBOX headers 12,500 / 48,213 · 2 queued"
        );
    }

    #[test]
    fn an_account_that_is_not_running_has_failed() {
        let mut account = account();
        account.activity = Some(Activity::NotRunning {
            reason: "could not start its sync thread".into(),
        });
        assert!(failed(&account));
        assert_eq!(dot(&account), Some(Dot::Failed));
    }

    use crate::gui::state::{UiAction, UpdateCheck};
    use crate::gui::test_support::{Fixture, Open, Wires, launched, message};
    use crate::sync::Event;
    use crate::time::clock;

    fn send(open: &Open, wires: &Wires, events: impl IntoIterator<Item = Event>) {
        for event in events {
            wires.events.send(event).unwrap();
        }
        open.tick();
    }

    fn activity(activity: Activity) -> Event {
        Event::Activity {
            account: "work".into(),
            activity,
        }
    }

    fn error(message: &str) -> Event {
        Event::Error {
            account: "work".into(),
            message: message.into(),
        }
    }

    #[test]
    fn the_status_bar_shows_each_accounts_line() {
        let fx = Fixture::new(&["work"]);
        let (open, wires) = fx.window();
        let fetching = Activity::FetchingHeaders {
            folder: "INBOX".into(),
            done: 12_500,
            total: 48_213,
        };
        send(&open, &wires, [activity(fetching)]);
        assert_eq!(open.find("work: INBOX headers 12,500 / 48,213").len(), 1);
        let offline = Activity::Offline {
            reason: "timeout".into(),
            retry_at: 1_790_000_300,
        };
        send(&open, &wires, [activity(offline)]);
        let line = format!("work: offline (timeout) · retry {}", clock(1_790_000_300));
        assert_eq!(open.find(&line).len(), 1);
        let stopped = Activity::NotRunning {
            reason: "could not start its sync thread".into(),
        };
        send(&open, &wires, [activity(stopped)]);
        assert_eq!(
            open.find("work: not running (could not start its sync thread)")
                .len(),
            1
        );
        assert!(!open.with(|live| live.app.accounts[0].busy()));
    }

    #[test]
    fn an_error_shows_on_the_account_line_and_in_the_history() {
        let fx = Fixture::new(&["work"]);
        let (open, wires) = fx.window();
        send(&open, &wires, [error("login failed\u{1b}[2J")]);
        open.click("work: login failed[2J");
        assert!(open.with(|live| live.app.history_open));
        let history = open.window().get_history();
        let lines: Vec<String> = slint::Model::iter(&history).map(String::from).collect();
        assert!(
            lines
                .iter()
                .any(|line| line.ends_with("  work: login failed[2J")),
            "{lines:?}"
        );
        open.press("escape");
        assert!(!open.with(|live| live.app.history_open));
    }

    #[test]
    fn an_empty_history_says_so() {
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = fx.window();
        open.with(|live| live.app.apply(UiAction::ToggleHistory, 0.0));
        assert_eq!(open.find("Nothing has happened yet.").len(), 1);
        open.click("Close");
        assert!(!open.with(|live| live.app.history_open));
    }

    #[test]
    fn synced_reloads_the_shown_folder_and_its_unread_count() {
        let fx = Fixture::new(&["work"]);
        let (open, wires) = fx.window();
        let mut unread = message("INBOX", 9, "late arrival");
        unread.flags = String::new();
        fx.add("work", unread);
        assert!(open.with(|live| live.app.list.rows.is_empty()));
        let synced = Event::Synced {
            account: "work".into(),
            new_messages: 1,
            actions: 0,
            requests: vec![],
            errors: vec![],
        };
        send(&open, &wires, [synced]);
        assert_eq!(open.with(|live| live.app.list.rows.len()), 1);
        assert_eq!(open.find("Inbox (1)").len(), 1);
    }

    #[test]
    fn a_rule_applied_or_bodies_fetched_reloads_the_shown_folder() {
        let completions = [
            Event::BodiesFetched {
                account: "work".into(),
                request: 1,
                fetched: 1,
            },
            Event::RuleApplied {
                account: "work".into(),
                request: 2,
                evaluated: 1,
                actions: 1,
                errors: vec![],
            },
        ];
        for (uid, completion) in (1..).zip(completions) {
            let fx = Fixture::new(&["work"]);
            let (mut driver, wires) = fx.driver();
            fx.add("work", message("INBOX", uid, "written by the daemon"));
            assert!(driver.app.list.rows.is_empty());
            wires.events.send(completion).unwrap();
            driver.tick();
            assert_eq!(driver.app.list.rows.len(), 1);
        }
    }

    #[test]
    fn choosing_dark_applies_it_and_writes_it_keeping_comments() {
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = fx.window();
        open.click("Dark");
        let window = open.window();
        let theme = slint::ComponentHandle::global::<crate::gui::ui::Theme>(&window);
        assert_eq!(
            theme.get_preference(),
            crate::gui::ui::ThemePreference::Dark
        );
        let config = std::fs::read_to_string(fx.paths.config_file()).unwrap();
        assert!(
            config.starts_with("# test accounts\n") && config.contains("theme = \"dark\""),
            "{config}"
        );
    }

    #[test]
    fn progress_ticks_stay_out_of_the_history() {
        let fx = Fixture::new(&["work"]);
        let (mut driver, wires) = fx.driver();
        let fetching = |done| {
            activity(Activity::FetchingHeaders {
                folder: "INBOX".into(),
                done,
                total: 2_000,
            })
        };
        let idle = || {
            activity(Activity::Idle {
                since: 1_790_000_000,
            })
        };
        let events = [
            fetching(500),
            fetching(1_000),
            fetching(1_500),
            idle(),
            idle(),
            error("boom"),
        ];
        for event in events {
            wires.events.send(event).unwrap();
        }
        driver.tick();
        let texts: Vec<&str> = driver
            .app
            .history
            .iter()
            .map(|line| line.text.as_str())
            .collect();
        let up_to_date = format!("work: up to date · {}", clock(1_790_000_000));
        assert_eq!(texts, [up_to_date.as_str(), "work: boom"]);
    }

    #[test]
    fn offline_keeps_the_error_on_the_line() {
        let fx = Fixture::new(&["work"]);
        let (open, wires) = fx.window();
        let offline = Activity::Offline {
            reason: "login failed".into(),
            retry_at: 1_790_000_300,
        };
        send(&open, &wires, [error("login failed"), activity(offline)]);
        assert_eq!(open.find("work: login failed").len(), 1);
    }

    #[test]
    fn theme_buttons_read_system_light_dark_from_left_to_right() {
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = fx.window();
        let left = |label| open.get(label).absolute_position().x;
        assert!(left("System") < left("Light") && left("Light") < left("Dark"));
    }

    #[test]
    fn a_newer_release_links_to_its_notes() {
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = fx.window();
        open.with(|live| live.app.update = UpdateCheck::Done(Some("9.9.9".into())));
        open.click("↑ v9.9.9 available");
        assert_eq!(
            launched(),
            ["https://github.com/postvak-app/postvak/releases/tag/v9.9.9"]
        );
    }

    #[test]
    fn no_newer_release_shows_only_the_version() {
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = fx.window();
        open.with(|live| live.app.update = UpdateCheck::Done(None));
        assert!(open.window().get_update().is_empty());
        assert_eq!(open.find(&format!("v{VERSION}")).len(), 1);
    }

    #[test]
    fn the_tray_counts_the_unread_mail_in_every_inbox() {
        let fx = Fixture::new(&["home", "work"]);
        fx.folder("work", "Archive", Some("Archive"));
        let (driver, _wires) = fx.driver();
        assert_eq!(tray_hint(&driver.app), "Postvak");
        for (account, folder, uid) in [
            ("home", "INBOX", 1),
            ("work", "INBOX", 2),
            ("work", "Archive", 3),
        ] {
            let mut unread = message(folder, uid, "hello");
            unread.flags = String::new();
            fx.add(account, unread);
        }
        let (driver, _wires) = fx.driver();
        assert_eq!(tray_hint(&driver.app), "Postvak · 2 unread");
    }

    #[test]
    fn the_status_bar_sync_button_syncs_every_account() {
        let fx = Fixture::new(&["home", "work"]);
        let (open, wires) = fx.window();
        open.click("Sync now");
        let sent: Vec<String> = wires
            .sent()
            .into_iter()
            .map(|(account, _)| account)
            .collect();
        assert_eq!(sent, ["home", "work"]);
    }
}
