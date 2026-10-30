//! Undo without a toolkit: the actions taken in this window, what undoing one sent, and the toast's words.
use postvak_core::actions::{UndoOutcome, UndoReport};
use postvak_core::rules::Action;

use super::numbers::counted;

/// How long a toast stays when nothing holds it, in seconds.
pub(crate) const TOAST_SECONDS: f64 = 8.0;

/// One gesture of this window, such as archiving the selection: an `Apply` per account and folder, undone together.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Taken {
    pub id: u64,
    /// The toast's text, such as "Archived 2 messages".
    pub done: String,
    /// The log's action ids, per account, as the `ActionDone`s brought them.
    pub actions: Vec<(usize, i64)>,
    /// `Apply`s whose `ActionDone` has not come yet.
    pub waiting: usize,
    pub state: UndoState,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum UndoState {
    /// Can still be undone.
    Open,
    /// Undo was asked before every `ActionDone` came; it is sent once they have.
    Asked,
    /// Undo was sent; `left` answers are to come.
    Undoing {
        left: usize,
        reports: Vec<UndoReport>,
    },
    Undone,
}

/// The message at the foot of the list pane after an action or an undo.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Toast {
    pub text: String,
    /// Offers Undo, with its key.
    pub undo: bool,
    pub hovered: bool,
    /// When it goes, on the window's clock; set by the first tick that shows it, and again when the pointer leaves it.
    pub until: Option<f64>,
}

impl Toast {
    pub fn new(text: String, undo: bool) -> Toast {
        Toast {
            text,
            undo,
            hovered: false,
            until: None,
        }
    }
}

/// What a gesture did, in the past tense, for the toast.
pub(crate) fn done_text(action: &Action, count: usize) -> String {
    let messages = counted(count as u64, "message", "messages");
    match action {
        Action::Archive => format!("Archived {messages}"),
        Action::Delete | Action::Trash => format!("Deleted {messages}"),
        Action::Flag => format!("Flagged {messages}"),
        Action::Junk => format!("Marked {messages} as junk"),
        Action::MarkRead => format!("Marked {messages} read"),
        Action::MarkUnread => format!("Marked {messages} unread"),
        Action::Move(folder) => format!("Moved {messages} to {}", super::folders::label(folder)),
        Action::NotJunk => format!("Marked {messages} as not junk"),
        Action::Tag(_) => format!("Tagged {messages}"),
        Action::Unflag => format!("Unflagged {messages}"),
        Action::After(_) | Action::Notify | Action::Silent | Action::Stop => {
            format!("Updated {messages}")
        }
    }
}

/// What an undo did, for the toast: what came back, then what was left alone and why.
pub(crate) fn undone_text(reports: &[UndoReport]) -> String {
    let rows = || reports.iter().flat_map(|report| &report.rows);
    let count = |outcome| rows().filter(|row| row.outcome == outcome).count() as u64;
    let moved_to: Vec<&str> = rows()
        .filter(|row| row.outcome == UndoOutcome::PutBack && is_move(&row.action))
        .map(|row| row.folder.as_str())
        .collect();
    let (put_back, restored) = (count(UndoOutcome::PutBack), count(UndoOutcome::Restored));
    let mut parts = Vec::new();
    match moved_to.first() {
        Some(folder)
            if moved_to.len() as u64 == put_back && moved_to.iter().all(|f| f == folder) =>
        {
            parts.push(format!(
                "Moved {} back to {}",
                counted(put_back, "message", "messages"),
                super::folders::label(folder)
            ))
        }
        _ if put_back > 0 => parts.push(format!(
            "Put back {}",
            counted(put_back, "message", "messages")
        )),
        _ => {}
    }
    if restored > 0 {
        parts.push(format!(
            "Restored {} from the backups",
            counted(restored, "message", "messages")
        ));
    }
    // After the first part the noun is understood, so the line fits the list pane.
    let noun = |n: u64, first: bool| {
        if first {
            counted(n, "message", "messages")
        } else {
            super::numbers::count(n)
        }
    };
    let changed = count(UndoOutcome::ChangedSince) + count(UndoOutcome::Ambiguous);
    if changed > 0 {
        parts.push(format!(
            "{} changed since, left alone",
            noun(changed, parts.is_empty())
        ));
    }
    let gone = count(UndoOutcome::BackupGone);
    if gone > 0 {
        parts.push(format!(
            "{} had no backup left",
            noun(gone, parts.is_empty())
        ));
    }
    let failed = count(UndoOutcome::Failed);
    if failed > 0 {
        parts.push(format!(
            "{} could not be put back",
            noun(failed, parts.is_empty())
        ));
    }
    if parts.is_empty() {
        return "Nothing to undo".into();
    }
    // The first part leads with a capital; the rest follow it in lower case.
    let mut text = parts.remove(0);
    for part in parts {
        text.push_str("; ");
        text.push_str(&lower_first(&part));
    }
    text
}

fn lower_first(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|first| first.to_lowercase().chain(chars).collect())
        .unwrap_or_default()
}

fn is_move(label: &str) -> bool {
    label.starts_with("move:") || matches!(label, "archive" | "junk" | "not_junk" | "trash")
}

#[cfg(test)]
mod tests {
    use super::*;
    use postvak_core::actions::UndoRow;

    fn report(rows: &[(&str, &str, UndoOutcome)]) -> UndoReport {
        UndoReport {
            action: 1,
            rows: rows
                .iter()
                .map(|&(folder, action, outcome)| UndoRow {
                    folder: folder.into(),
                    uid: 1,
                    subject: None,
                    action: action.into(),
                    outcome,
                    error: None,
                })
                .collect(),
        }
    }

    #[test]
    fn the_toast_says_what_the_action_did() {
        assert_eq!(done_text(&Action::Archive, 1), "Archived 1 message");
        assert_eq!(done_text(&Action::Trash, 3), "Deleted 3 messages");
        assert_eq!(
            done_text(&Action::Move("INBOX".into()), 2),
            "Moved 2 messages to Inbox"
        );
    }

    #[test]
    fn an_undo_says_where_the_mail_went_and_what_was_left_alone() {
        use UndoOutcome::*;
        let archived = report(&[("INBOX", "archive", PutBack), ("INBOX", "archive", PutBack)]);
        assert_eq!(undone_text(&[archived]), "Moved 2 messages back to Inbox");
        let partly = report(&[
            ("INBOX", "trash", PutBack),
            ("INBOX", "trash", ChangedSince),
        ]);
        assert_eq!(
            undone_text(&[partly]),
            "Moved 1 message back to Inbox; 1 changed since, left alone"
        );
        let none_back = report(&[("INBOX", "trash", ChangedSince)]);
        assert_eq!(
            undone_text(&[none_back]),
            "1 message changed since, left alone"
        );
        let mixed = report(&[("INBOX", "read", PutBack), ("Work", "delete", Restored)]);
        assert_eq!(
            undone_text(&[mixed]),
            "Put back 1 message; restored 1 message from the backups"
        );
        assert_eq!(
            undone_text(&[report(&[("INBOX", "stop", NothingToUndo)])]),
            "Nothing to undo"
        );
    }
}
