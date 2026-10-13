//! The status bar without a toolkit: a line per account from its error or latest activity, its progress, the mark
//! before it, the sync button's hint and the version. The window draws the bar, the history window and the theme
//! switch.
use crate::sync::Activity;

use super::state::Account;

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
}
