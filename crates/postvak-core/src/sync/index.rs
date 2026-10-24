//! Indexing message bodies for search in the account's idle time. A slice takes the newest messages the store has
//! not indexed, fetches each one's structure and only its text parts, and stores the text and the attachment names,
//! never the raw message. It stops after `SLICE_BUDGET`, so pushes and commands wait at most that long.

use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use crate::actions::{self, ActionError};
use crate::config::AccountConfig;
use crate::mail_ops::{MailOps, Structure, TextPart};
use crate::message;
use crate::store::{Message, Store};
use crate::sync::{Activity, Event, SyncError, connection_lost};
use crate::time::now;

/// How long one slice fetches before the loop checks INBOX and waits.
pub(crate) const SLICE_BUDGET: Duration = Duration::from_secs(5);
/// The IDLE between slices: long enough for a push or a command to arrive, short enough to keep indexing.
pub(crate) const INTER_SLICE_IDLE: Duration = Duration::from_secs(1);
/// A message without a readable structure is fetched whole up to this size, and indexed from its headers above it.
pub(crate) const FALLBACK_RAW_LIMIT: u32 = 2 * 1024 * 1024;
/// Rows read per slice; the budget usually ends a slice first.
const SLICE_CANDIDATES: u32 = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexOutcome {
    /// Some messages were tried; call again after a short wait.
    Progressed,
    /// Nothing waits for the indexer.
    Done,
}

/// The indexer's state for one connection: the index is checked once per session, and given up on when a rebuild
/// fails too.
#[derive(Default)]
pub(crate) struct Session {
    checked: bool,
    broken: bool,
}

impl Session {
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
        slice(ops, store, account, events, &mut || started.elapsed())
    }
}

/// Indexes the newest unindexed messages for at most `SLICE_BUDGET` of `elapsed`, always at least one. A message
/// whose fetch fails is marked tried and retried a day later; only a lost connection ends the slice early.
pub(crate) fn slice(
    ops: &mut dyn MailOps,
    store: &Store,
    account: &AccountConfig,
    events: &Sender<Event>,
    elapsed: &mut dyn FnMut() -> Duration,
) -> Result<IndexOutcome, SyncError> {
    let at = now();
    let (remaining, total) = store.backlog_count(at)?;
    if remaining == 0 {
        return Ok(IndexOutcome::Done);
    }
    let _ = events.send(Event::Activity {
        account: account.name.clone(),
        activity: Activity::Indexing {
            done: total - remaining,
            total,
        },
    });
    let mut selected: Option<String> = None;
    let mut unselectable: Option<String> = None;
    let mut done = 0;
    for msg in store.backlog(at, SLICE_CANDIDATES)? {
        if done > 0 && elapsed() >= SLICE_BUDGET {
            break;
        }
        let selectable = if unselectable.as_deref() == Some(msg.folder.as_str()) {
            false
        } else if selected.as_deref() == Some(msg.folder.as_str()) {
            true
        } else {
            match actions::select_synced(ops, store, &msg.folder) {
                Ok(()) => {
                    selected = Some(msg.folder.clone());
                    true
                }
                Err(e) if connection_lost(&e) => return Err(e.into()),
                Err(e) => {
                    log::warn!("{}: indexing skipped: {e}", msg.folder);
                    unselectable = Some(msg.folder.clone());
                    false
                }
            }
        };
        let indexed = if selectable {
            index_one(ops, &msg)
        } else {
            Err(ActionError::UnknownFolder(msg.folder.clone()))
        };
        match indexed {
            Ok((text, names)) => {
                store.set_indexed(&msg.folder, msg.uid, Some(&text), &names, at)?
            }
            Err(e) if connection_lost(&e) => return Err(e.into()),
            Err(e) => {
                log::warn!("{}/{}: indexing failed: {e}", msg.folder, msg.uid);
                store.set_indexed(&msg.folder, msg.uid, None, &[], at)?;
            }
        }
        done += 1;
    }
    let _ = events.send(Event::Indexed {
        account: account.name.clone(),
        count: done,
    });
    Ok(IndexOutcome::Progressed)
}

/// The message's text and attachment names: from its text parts, or from the whole message when the server gives
/// no structure and the message is small enough; headers only otherwise.
fn index_one(ops: &mut dyn MailOps, msg: &Message) -> Result<(String, Vec<String>), ActionError> {
    let Some(structure) = ops.fetch_structure(msg.uid)? else {
        if msg.size.is_some_and(|size| size > FALLBACK_RAW_LIMIT) {
            return Ok((String::new(), Vec::new()));
        }
        return Ok(match ops.fetch_raw(msg.uid)? {
            Some(raw) => (
                message::body_text(&raw),
                message::attachments(&raw)
                    .into_iter()
                    .filter_map(|a| a.name)
                    .collect(),
            ),
            None => (String::new(), Vec::new()),
        });
    };
    let mut parts = Vec::new();
    for part in wanted_parts(&structure) {
        if let Some(bytes) = ops.fetch_part(msg.uid, &part.section)? {
            parts.push((part.clone(), bytes));
        }
    }
    Ok((message::text_from_parts(&parts), structure.attachment_names))
}

/// The parts worth fetching: every text part outside a `multipart/alternative`, and of each alternative its first
/// plain part, else its first HTML part, so the HTML is not downloaded when the plain text exists.
fn wanted_parts(structure: &Structure) -> Vec<&TextPart> {
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
