//! Undo: puts back what one logged action did, where that can be done exactly, and leaves the rest alone.
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::{ActionError, run, select_synced};
use crate::config::AccountConfig;
use crate::folder_name;
use crate::mail_ops::MailOps;
use crate::paths::Paths;
use crate::rules::Action;
use crate::store::{LogEntry, Store, StoreError, has_flag, with_flag};
use crate::trash::Trash;

/// What undo did, or would do, with one logged row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UndoOutcome {
    /// Moved back to its folder, or its flag set back.
    PutBack,
    /// Brought back from its backup: a new message, without its flags and with today's arrival date.
    Restored,
    /// No longer where the action left it, so left alone.
    ChangedSince,
    /// Cannot be told apart from another message, or has no Message-ID to find it by, so left alone.
    Ambiguous,
    /// Its backup was purged.
    BackupGone,
    /// The row changed no mail, such as `notify` or `stop`.
    NothingToUndo,
    /// The server or the store refused; `error` says why.
    Failed,
}

impl UndoOutcome {
    /// What happened to the message, as the CLI and the window say it.
    pub fn text(self) -> &'static str {
        match self {
            UndoOutcome::PutBack => "put back",
            UndoOutcome::Restored => "restored from its backup, as a new message without its flags",
            UndoOutcome::ChangedSince => "changed since, left alone",
            UndoOutcome::Ambiguous => "cannot be told apart from another message, left alone",
            UndoOutcome::BackupGone => "its backup was purged",
            UndoOutcome::NothingToUndo => "nothing to undo",
            UndoOutcome::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UndoRow {
    /// The folder the message is put back in.
    pub folder: String,
    /// Its uid when the action ran.
    pub uid: u32,
    pub subject: Option<String>,
    /// The logged action this row reverses.
    pub action: String,
    pub outcome: UndoOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UndoReport {
    pub action: i64,
    pub rows: Vec<UndoRow>,
}

impl UndoReport {
    pub fn count(&self, outcome: UndoOutcome) -> usize {
        self.rows
            .iter()
            .filter(|row| row.outcome == outcome)
            .count()
    }

    /// The folders mail went back to, each once.
    pub fn folders_put_back(&self) -> Vec<String> {
        let mut folders: Vec<String> = self
            .rows
            .iter()
            .filter(|row| matches!(row.outcome, UndoOutcome::PutBack | UndoOutcome::Restored))
            .map(|row| row.folder.clone())
            .collect();
        folders.sort();
        folders.dedup();
        folders
    }
}

/// The newest direct action not undone yet in the stores of `accounts`: its account and id. A rule's action is never it.
pub fn newest_direct_action(
    paths: &Paths,
    accounts: &[&AccountConfig],
) -> Result<Option<(String, i64)>, StoreError> {
    let mut newest: Option<(i64, String, i64)> = None;
    for account in accounts {
        let store = Store::open_account(paths, &account.name)?;
        if let Some((action, at)) = store.newest_direct_action()?
            && newest
                .as_ref()
                .is_none_or(|(newest_at, ..)| at > *newest_at)
        {
            newest = Some((at, account.name.clone(), action));
        }
    }
    Ok(newest.map(|(_, account, action)| (account, action)))
}

/// How one row is reversed.
#[derive(Debug, Clone, PartialEq)]
enum Step {
    /// Move `uid` in `from`, where the action took the message, back to `to`.
    Move {
        from: String,
        uid: u32,
        to: String,
    },
    /// Set (`set`) or clear `flag` on `uid` in `folder`.
    Flag {
        folder: String,
        uid: u32,
        flag: String,
        set: bool,
    },
    Restore(PathBuf),
    Skip(UndoOutcome),
}

/// The flag a logged label set or cleared, and the state it left: `read` left `\Seen` set.
fn logged_flag(label: &str) -> Option<(String, bool)> {
    let flag = |name: &str, state| Some((name.to_string(), state));
    match label {
        "read" => flag("\\Seen", true),
        "read:false" => flag("\\Seen", false),
        "flagged" => flag("\\Flagged", true),
        "flagged:false" => flag("\\Flagged", false),
        _ => label
            .strip_prefix("tag:")
            .and_then(|keyword| flag(keyword, true)),
    }
}

fn is_move(label: &str) -> bool {
    label.starts_with("move:") || matches!(label, "archive" | "junk" | "not_junk" | "trash")
}

/// A message's folder and uid where it is now, or why it cannot be found there.
type Found = Result<(String, u32), UndoOutcome>;
/// Where this action's moves took each message, by the folder and uid it had.
type Moved = HashMap<(String, u32), Found>;

/// Where a moved message is now: found in the folder it went to by Message-ID and internaldate, or a reason to skip it.
fn moved_copy(store: &Store, row: &LogEntry) -> Result<Found, StoreError> {
    let (Some(target), Some(message_id), Some(internaldate)) =
        (&row.target, &row.message_id, row.internaldate)
    else {
        return Ok(Err(UndoOutcome::Ambiguous));
    };
    let Some(target) = folder_name::known(store, target)? else {
        return Ok(Err(UndoOutcome::ChangedSince));
    };
    Ok(
        match store
            .messages_by_id(&target.name, message_id, internaldate)?
            .as_slice()
        {
            [] => Err(UndoOutcome::ChangedSince),
            [found] => Ok((target.name, found.uid)),
            _ => Err(UndoOutcome::Ambiguous),
        },
    )
}

/// How to reverse `row`. `moved` holds where this action's moves took each message, so a flag set before the move is
/// cleared where the message is now.
fn step(store: &Store, row: &LogEntry, moved: &Moved) -> Result<Step, StoreError> {
    let label = row.action.as_str();
    if label == "delete" {
        let backup = row.trash_file.as_deref().map(PathBuf::from);
        return Ok(match backup.filter(|file| file.is_file()) {
            Some(file) => Step::Restore(file),
            None => Step::Skip(UndoOutcome::BackupGone),
        });
    }
    if is_move(label) {
        return Ok(match moved.get(&(row.folder.clone(), row.uid)) {
            Some(Ok((from, uid))) => Step::Move {
                from: from.clone(),
                uid: *uid,
                to: row.folder.clone(),
            },
            Some(Err(skip)) => Step::Skip(*skip),
            None => Step::Skip(UndoOutcome::Ambiguous),
        });
    }
    let Some((flag, left)) = logged_flag(label) else {
        return Ok(Step::Skip(UndoOutcome::NothingToUndo));
    };
    let (folder, uid) = match moved.get(&(row.folder.clone(), row.uid)) {
        Some(Ok(now_at)) => now_at.clone(),
        Some(Err(skip)) => return Ok(Step::Skip(*skip)),
        None => (row.folder.clone(), row.uid),
    };
    let current = store
        .message(&folder, uid)?
        .filter(|msg| msg.message_id == row.message_id);
    Ok(match current {
        Some(msg) if has_flag(&msg.flags, &flag) == left => Step::Flag {
            folder,
            uid,
            flag,
            set: !left,
        },
        _ => Step::Skip(UndoOutcome::ChangedSince),
    })
}

/// Each row of `action` with how to reverse it; refuses an action not in the log, or one undone already.
fn plan(store: &Store, action: i64) -> Result<Vec<(LogEntry, Step)>, ActionError> {
    let rows = store.action_rows(action)?;
    if rows.is_empty() {
        return Err(ActionError::NoSuchAction(action));
    }
    if store.is_undone(action)? {
        return Err(ActionError::AlreadyUndone(action));
    }
    let mut moved = HashMap::new();
    for row in rows.iter().filter(|row| is_move(&row.action)) {
        moved.insert((row.folder.clone(), row.uid), moved_copy(store, row)?);
    }
    rows.into_iter()
        .map(|row| {
            let step = step(store, &row, &moved)?;
            Ok((row, step))
        })
        .collect()
}

fn report_row(row: &LogEntry, outcome: UndoOutcome, error: Option<String>) -> UndoRow {
    UndoRow {
        folder: row.folder.clone(),
        uid: row.uid,
        subject: row.subject.clone(),
        action: row.action.clone(),
        outcome,
        error,
    }
}

/// What `undo` would do with `action`, from the store alone.
pub fn preview_undo(store: &Store, action: i64) -> Result<UndoReport, ActionError> {
    let rows = plan(store, action)?
        .iter()
        .map(|(row, step)| {
            let outcome = match step {
                Step::Move { .. } | Step::Flag { .. } => UndoOutcome::PutBack,
                Step::Restore(_) => UndoOutcome::Restored,
                Step::Skip(skip) => *skip,
            };
            report_row(row, outcome, None)
        })
        .collect();
    Ok(UndoReport { action, rows })
}

/// Reverses `action` row by row, as `by`: flags first, while the messages are still where the action left them, then
/// moves back and restores. A row that cannot be put back exactly is skipped and says why. The rows written are marked as
/// this undo's, so the action is not offered again.
pub fn undo(
    ops: &mut dyn MailOps,
    store: &Store,
    trash: &Trash,
    action: i64,
    by: &str,
    now: i64,
) -> Result<UndoReport, ActionError> {
    let planned = plan(store, action)?;
    let logged_before = store.last_log_id()?;
    let mut outcomes: Vec<(UndoOutcome, Option<String>)> = planned
        .iter()
        .map(|(_, step)| match step {
            Step::Skip(skip) => (*skip, None),
            _ => (UndoOutcome::Failed, None),
        })
        .collect();
    let undo_row = |row: &LogEntry, folder: &str, uid: u32| LogEntry {
        at: now,
        rule_name: by.to_string(),
        folder: folder.to_string(),
        uid,
        message_id: row.message_id.clone(),
        subject: row.subject.clone(),
        action: format!("undo:{}", row.action),
        ..LogEntry::default()
    };

    let mut flags: BTreeMap<(&str, &str, bool), Vec<usize>> = BTreeMap::new();
    let mut moves: BTreeMap<(&str, &str), Vec<usize>> = BTreeMap::new();
    for (index, (_, step)) in planned.iter().enumerate() {
        match step {
            Step::Flag {
                folder, flag, set, ..
            } => flags.entry((folder, flag, *set)).or_default().push(index),
            Step::Move { from, to, .. } => moves.entry((from, to)).or_default().push(index),
            Step::Restore(_) | Step::Skip(_) => {}
        }
    }
    let uid_at = |index: usize| match planned[index].1 {
        Step::Flag { uid, .. } | Step::Move { uid, .. } => uid,
        _ => 0,
    };

    for ((folder, flag, set), indexes) in flags {
        let uids: Vec<u32> = indexes.iter().map(|&index| uid_at(index)).collect();
        let sent = select_synced(ops, store, folder).and_then(|()| {
            let sent = if set {
                ops.add_flags_to(&uids, &[flag])
            } else {
                ops.remove_flags_from(&uids, &[flag])
            };
            sent.map_err(ActionError::from)
        });
        for (&index, &uid) in indexes.iter().zip(&uids) {
            outcomes[index] = match &sent {
                Ok(()) => {
                    let row = &planned[index].0;
                    let stored = store.message(folder, uid).and_then(|msg| match msg {
                        Some(msg) => {
                            store.update_flags(folder, uid, &with_flag(&msg.flags, flag, set))
                        }
                        None => Ok(()),
                    });
                    match stored.and_then(|()| store.log_action(&undo_row(row, folder, uid))) {
                        Ok(()) => (UndoOutcome::PutBack, None),
                        Err(e) => (UndoOutcome::Failed, Some(e.to_string())),
                    }
                }
                Err(e) => (UndoOutcome::Failed, Some(e.to_string())),
            };
        }
    }

    for ((from, to), indexes) in moves {
        let uids: Vec<u32> = indexes.iter().map(|&index| uid_at(index)).collect();
        let moved = run(
            ops,
            store,
            trash,
            from,
            &uids,
            &Action::Move(to.to_string()),
            by,
            now,
        );
        for (&index, &uid) in indexes.iter().zip(&uids) {
            outcomes[index] = match &moved {
                Ok(results) => match results.iter().find(|(done, _)| *done == uid) {
                    Some((_, Ok(_))) => (UndoOutcome::PutBack, None),
                    Some((_, Err(e))) => (UndoOutcome::Failed, Some(e.to_string())),
                    None => (UndoOutcome::Failed, None),
                },
                Err(e) => (UndoOutcome::Failed, Some(e.to_string())),
            };
        }
    }

    for (index, (row, step)) in planned.iter().enumerate() {
        let Step::Restore(file) = step else { continue };
        let restored = trash
            .restore(ops, store, file)
            .map_err(|e| e.to_string())
            .and_then(|folder| {
                store
                    .log_action(&undo_row(row, &folder, row.uid))
                    .map_err(|e| e.to_string())
            });
        outcomes[index] = match restored {
            Ok(()) => (UndoOutcome::Restored, None),
            Err(e) => (UndoOutcome::Failed, Some(e)),
        };
    }

    store.mark_undo(logged_before, action)?;
    let rows = planned
        .iter()
        .zip(outcomes)
        .map(|((row, _), (outcome, error))| report_row(row, outcome, error))
        .collect();
    Ok(UndoReport { action, rows })
}
