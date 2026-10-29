//! The small window shown in place of the mail window when that cannot start: a launch from a desktop entry or
//! Finder has no terminal to show stderr in.
use std::path::Path;

use postvak_core::message::clean;

/// Where the docs explain `postvak account add` and config.toml.
pub(crate) const ACCOUNTS_DOCS: &str = "https://postvak.pataar.nl/docs/accounts.html";

/// config.toml names no account, so the window has nothing to show.
#[derive(Debug, thiserror::Error)]
#[error("no accounts configured; run `postvak account add`")]
pub(crate) struct NoAccounts;

/// What went wrong and what to do about it, as the window states it.
#[derive(Debug, PartialEq)]
pub(crate) struct Problem {
    pub summary: String,
    pub fix: String,
    /// The error itself, when the summary does not say it all.
    pub detail: Option<String>,
    /// Docs to open from the window, as (label, url).
    pub link: Option<(&'static str, &'static str)>,
}

impl Problem {
    /// `log` is the daemon log, when the paths were found.
    pub fn of(error: &anyhow::Error, log: Option<&Path>) -> Problem {
        if error.is::<NoAccounts>() {
            return Problem {
                summary: "Postvak has no mail account set up yet.".into(),
                fix: "Run `postvak account add` in a terminal, then open Postvak again.".into(),
                detail: None,
                link: Some(("Setting up an account", ACCOUNTS_DOCS)),
            };
        }
        let fix = match log {
            Some(log) => format!("The background sync logs to {}", log.display()),
            None => "Run `postvak gui` in a terminal to see more.".into(),
        };
        Problem {
            summary: "Postvak could not start.".into(),
            fix,
            detail: Some(clean(&format!("{error:#}"), false)),
            link: None,
        }
    }

    /// Points at the crash folder too, which holds reports.
    pub fn with_crashes(mut self, dir: &Path) -> Problem {
        self.fix = format!("{}; crash reports are in {}", self.fix, dir.display());
        self
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn no_accounts_points_to_account_add_and_the_docs() {
        let problem = Problem::of(&anyhow::Error::new(NoAccounts), Some(Path::new("/l")));
        assert_eq!(problem.summary, "Postvak has no mail account set up yet.");
        assert!(problem.fix.contains("`postvak account add`"));
        assert_eq!(problem.link, Some(("Setting up an account", ACCOUNTS_DOCS)));
        assert_eq!(problem.detail, None);
    }

    #[test]
    fn no_accounts_is_found_under_context() {
        let error = anyhow::Error::new(NoAccounts).context("opening the window");
        let problem = Problem::of(&error, None);
        assert_eq!(problem.summary, "Postvak has no mail account set up yet.");
    }

    #[test]
    fn other_errors_show_their_text_and_the_daemon_log() {
        let error = anyhow::anyhow!("socket refused").context("could not start the daemon");
        let problem = Problem::of(&error, Some(Path::new("/state/daemon.log")));
        assert_eq!(problem.summary, "Postvak could not start.");
        assert_eq!(problem.fix, "The background sync logs to /state/daemon.log");
        assert_eq!(
            problem.detail.as_deref(),
            Some("could not start the daemon: socket refused")
        );
    }

    #[test]
    fn crash_reports_are_pointed_at() {
        let problem = Problem::of(
            &anyhow::anyhow!("boom"),
            Some(Path::new("/state/daemon.log")),
        )
        .with_crashes(Path::new("/state/crashes"));
        assert_eq!(
            problem.fix,
            "The background sync logs to /state/daemon.log; crash reports are in /state/crashes"
        );
    }

    #[test]
    fn without_paths_the_fix_is_to_run_it_in_a_terminal() {
        let problem = Problem::of(&anyhow::anyhow!("no home directory found"), None);
        assert_eq!(problem.fix, "Run `postvak gui` in a terminal to see more.");
        assert_eq!(problem.detail.as_deref(), Some("no home directory found"));
        assert_eq!(problem.link, None);
    }
}
