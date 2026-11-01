//! Indexing message bodies for search in the account's idle time. A slice takes the newest messages the store has
//! not indexed, fetches each one's structure and only its text parts, and stores the text and the attachment names,
//! never the raw message. It fetches the structures of a run of messages of one folder in one command, and keeps the
//! folder selected for the next slice. It stops after `SLICE_BUDGET`, so pushes and commands wait at most that long.

use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use crate::actions::{self, ActionError};
use crate::config::AccountConfig;
use crate::mail_ops::{MailOps, Structure, TextPart};
use crate::message;
use crate::store::{Message, Store};
use crate::sync::{Activity, Event, LostConnection, SyncError};
use crate::time::now;

/// How long one slice fetches before the loop checks INBOX and waits.
pub(crate) const SLICE_BUDGET: Duration = Duration::from_secs(5);
/// The IDLE between slices: long enough for a push or a command to arrive, short enough to keep indexing.
pub(crate) const INTER_SLICE_IDLE: Duration = Duration::from_secs(1);
/// A message without a readable structure is fetched whole up to this size, and indexed from its headers above it.
pub(crate) const FALLBACK_RAW_LIMIT: u32 = 2 * 1024 * 1024;
/// Rows read per slice; the budget usually ends a slice first.
const SLICE_CANDIDATES: u32 = 200;
/// Messages of one folder whose `BODYSTRUCTURE` is fetched in one command.
const STRUCTURE_BATCH: usize = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexOutcome {
    /// Some messages were tried; call again after a short wait.
    Progressed,
    /// Nothing waits for the indexer.
    Done,
}

/// The indexer's state for one connection: the index is checked once per session, and given up on when a rebuild
/// fails too. It also knows which folder its last slice left selected, so the next slice need not select it again.
#[derive(Default)]
pub(crate) struct Session {
    checked: bool,
    broken: bool,
    selected: Option<String>,
}

impl Session {
    /// The folder the last slice left selected on the connection, as long as nothing else selected another.
    pub(crate) fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    /// Call when anything but a slice may have selected a folder: a pass, or a command that used the connection.
    pub(crate) fn forget_selection(&mut self) {
        self.selected = None;
    }

    /// Runs one slice when the store has a backlog, after checking the search index once.
    pub(crate) fn slice(
        &mut self,
        ops: &mut dyn MailOps,
        store: &Store,
        account: &AccountConfig,
        events: &Sender<Event>,
    ) -> Result<IndexOutcome, SyncError> {
        if self.broken {
            return Ok(IndexOutcome::Done);
        }
        if !self.checked {
            self.checked = true;
            if let Err(e) = store.check_search_index() {
                log::warn!(
                    "{}: search index check failed ({e}); rebuilding",
                    account.name
                );
                if let Err(e) = store.rebuild_search_index() {
                    self.broken = true;
                    let _ = events.send(Event::Error {
                        account: account.name.clone(),
                        message: format!("search index broken: {e}"),
                    });
                    return Ok(IndexOutcome::Done);
                }
            }
        }
        let started = Instant::now();
        slice(ops, store, account, events, &mut self.selected, &mut || {
            started.elapsed()
        })
    }
}

/// Indexes the newest unindexed messages for at most `SLICE_BUDGET` of `elapsed`, always at least one. A message
/// whose fetch fails is marked tried and retried a day later; only a lost connection ends the slice early.
/// `selected` is the folder selected on the connection, if known; it is updated to the one this slice leaves.
pub(crate) fn slice(
    ops: &mut dyn MailOps,
    store: &Store,
    account: &AccountConfig,
    events: &Sender<Event>,
    selected: &mut Option<String>,
    elapsed: &mut dyn FnMut() -> Duration,
) -> Result<IndexOutcome, SyncError> {
    let at = now();
    // Checked every idle turn, also once all is indexed; the total counts every row, so only when there is work.
    if store.backlog_remaining(at)? == 0 {
        return Ok(IndexOutcome::Done);
    }
    let (remaining, total) = store.backlog_count(at)?;
    let _ = events.send(Event::Activity {
        account: account.name.clone(),
        activity: Activity::Indexing {
            done: total - remaining,
            total,
        },
    });
    let mut unselectable: Option<String> = None;
    let mut done = 0;
    let candidates = store.backlog(at, SLICE_CANDIDATES)?;
    let mut rest = candidates.as_slice();
    while let Some(first) = rest.first() {
        if done > 0 && elapsed() >= SLICE_BUDGET {
            break;
        }
        let run = rest
            .iter()
            .take(STRUCTURE_BATCH)
            .take_while(|m| m.folder == first.folder)
            .count();
        let (batch, tail) = rest.split_at(run);
        rest = tail;
        let folder = first.folder.as_str();
        let selectable = if unselectable.as_deref() == Some(folder) {
            false
        } else if selected.as_deref() == Some(folder) {
            true
        } else {
            *selected = None;
            match actions::select_synced(ops, store, folder) {
                Ok(()) => {
                    *selected = Some(folder.to_string());
                    true
                }
                Err(e) if e.is_lost_connection() => return Err(e.into()),
                Err(e) => {
                    log::warn!("{folder}: indexing skipped: {e}");
                    unselectable = Some(folder.to_string());
                    false
                }
            }
        };
        let uids: Vec<u32> = batch.iter().map(|m| m.uid).collect();
        let mut structures = if selectable {
            match ops.fetch_structures(&uids) {
                Ok(found) => Some(found),
                Err(e) if e.is_lost_connection() => return Err(ActionError::from(e).into()),
                // One message the server chokes on must not fail its neighbours: ask for each on its own.
                Err(e) => {
                    log::warn!(
                        "{folder}: structures of {} messages failed: {e}",
                        uids.len()
                    );
                    None
                }
            }
        } else {
            None
        };
        for (n, msg) in batch.iter().enumerate() {
            if n > 0 && elapsed() >= SLICE_BUDGET {
                return finish(events, account, done);
            }
            let indexed = if selectable {
                let structure = match structures.as_mut() {
                    Some(found) => Ok(found.remove(&msg.uid)),
                    None => ops
                        .fetch_structures(&[msg.uid])
                        .map(|mut found| found.remove(&msg.uid))
                        .map_err(ActionError::from),
                };
                structure.and_then(|structure| index_one(ops, msg, structure))
            } else {
                Err(ActionError::UnknownFolder(msg.folder.clone()))
            };
            match indexed {
                Ok((text, structure)) => store.set_indexed(
                    &msg.folder,
                    msg.uid,
                    text.as_deref(),
                    structure.as_ref(),
                    at,
                )?,
                Err(e) if e.is_lost_connection() => return Err(e.into()),
                Err(e) => {
                    log::warn!("{}/{}: indexing failed: {e}", msg.folder, msg.uid);
                    store.set_indexed(&msg.folder, msg.uid, None, None, at)?;
                }
            }
            done += 1;
        }
    }
    finish(events, account, done)
}

fn finish(
    events: &Sender<Event>,
    account: &AccountConfig,
    done: usize,
) -> Result<IndexOutcome, SyncError> {
    let _ = events.send(Event::Indexed {
        account: account.name.clone(),
        count: done,
    });
    Ok(IndexOutcome::Progressed)
}

/// The message's text and structure: from its text parts, or from the whole message when the server gives no
/// structure and the message is small enough; headers only otherwise. A message indexed before part types were stored
/// keeps its text and gets its structure alone; None for either leaves the stored one as it is.
fn index_one(
    ops: &mut dyn MailOps,
    msg: &Message,
    structure: Option<Structure>,
) -> Result<(Option<String>, Option<Structure>), ActionError> {
    if msg.indexed_at.is_some() && msg.body_text.is_some() {
        return Ok((None, structure));
    }
    let Some(structure) = structure else {
        if msg.size.is_some_and(|size| size > FALLBACK_RAW_LIMIT) {
            return Ok((Some(String::new()), None));
        }
        return Ok(match ops.fetch_raw(msg.uid)? {
            Some(raw) => (Some(message::body_text(&raw)), message::structure(&raw)),
            None => (Some(String::new()), None),
        });
    };
    let mut parts = Vec::new();
    for part in wanted_parts(&structure) {
        if let Some(bytes) = ops.fetch_part(msg.uid, &part.section)? {
            parts.push((part.clone(), bytes));
        }
    }
    Ok((Some(message::text_from_parts(&parts)), Some(structure)))
}

/// The parts worth fetching: every text part outside a `multipart/alternative`, and of each alternative its first
/// plain part, else its first HTML part, so the HTML is not downloaded when the plain text exists.
pub(crate) fn wanted_parts(structure: &Structure) -> Vec<&TextPart> {
    let mut wanted: Vec<&TextPart> = Vec::new();
    for part in &structure.text_parts {
        let Some(group) = part.alternative else {
            wanted.push(part);
            continue;
        };
        match wanted.iter().position(|p| p.alternative == Some(group)) {
            Some(at) if wanted[at].html && !part.html => wanted[at] = part,
            Some(_) => {}
            None => wanted.push(part),
        }
    }
    wanted
}
