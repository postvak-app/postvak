use std::collections::HashMap;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::actions::{self, ActionError};
use crate::config::{AccountConfig, ConfigError, Identity};
use crate::credentials::{self, CredentialError};
use crate::mail_ops::imap::ImapOps;
use crate::mail_ops::{Envelope, IdleOutcome, MailError, MailOps, RemoteFolder, StatusInfo};
use crate::message::{clean, parse_headers, thread_id};
use crate::paths::Paths;
use crate::rules::apply::{ApplyError, apply, ensure_body_text, plan_for};
use crate::rules::engine::{Context, Mode, evaluate, folder_needs_body};
use crate::rules::{Action, CompiledRule, RulesError};
use crate::store::{Folder, Message, Store, StoreError};
use crate::time::{clock, now};
use crate::trash::{RestoreError, Trash};

pub(crate) mod index;

pub use index::IndexOutcome;

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error(transparent)]
    Action(#[from] ActionError),
    #[error(transparent)]
    Restore(#[from] RestoreError),
    #[error(transparent)]
    Mail(#[from] MailError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Apply(#[from] ApplyError),
    #[error(transparent)]
    Rules(#[from] RulesError),
    #[error(transparent)]
    Credentials(#[from] CredentialError),
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("stopped")]
    Stopped,
    #[error("crashed: {0}")]
    Crashed(String),
}

/// Names one command so the event that completes it can be matched to whoever sent it.
pub type RequestId = u64;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Event {
    Activity {
        account: String,
        activity: Activity,
    },
    ActionDone {
        account: String,
        folder: String,
        results: EventResults,
        request: RequestId,
    },
    BodyReady {
        account: String,
        folder: String,
        uid: u32,
        request: RequestId,
    },
    /// Attachment `index` of a message, fetched on its own for a reader that asked for it.
    AttachmentFetched {
        account: String,
        folder: String,
        uid: u32,
        index: usize,
        /// As the sender named it; `message::attachment_file_name` makes it safe.
        name: Option<String>,
        #[serde(with = "crate::message::base64_bytes")]
        contents: Vec<u8>,
        request: RequestId,
    },
    /// What a reader shows of a large message while the rest of it downloads; `BodyReady` follows, once the message is
    /// stored or, when `preview.whole` is false, at once.
    BodyPreview {
        account: String,
        folder: String,
        uid: u32,
        preview: Box<actions::Preview>,
    },
    CommandFailed {
        account: String,
        request: RequestId,
        message: String,
    },
    /// The sync thread logged in; `uidplus` says whether the server can expunge just the messages asked, `encrypted`
    /// whether the session runs over TLS.
    Connected {
        account: String,
        uidplus: bool,
        encrypted: bool,
    },
    Error {
        account: String,
        message: String,
    },
    /// The indexer stored the text of `count` more messages, so a search may find more now.
    Indexed {
        account: String,
        count: usize,
    },
    NewMail {
        account: String,
        folder: String,
        uid: u32,
        from: String,
        subject: String,
    },
    /// The stored messages a `SearchServer` found, as (folder, uid); `query` tells a window whether it still wants them.
    ServerSearchDone {
        account: String,
        request: RequestId,
        query: String,
        hits: Vec<(String, u32)>,
    },
    Restored {
        account: String,
        folder: String,
        request: RequestId,
    },
    RuleApplied {
        account: String,
        request: RequestId,
        evaluated: usize,
        actions: usize,
        errors: Vec<String>,
    },
    Synced {
        account: String,
        new_messages: usize,
        actions: usize,
        requests: Vec<RequestId>,
        /// The errors this pass also sent as `Error` events, and the rules file's while it is invalid.
        errors: Vec<String>,
    },
}

impl Event {
    /// The requests this event completes, so the daemon can reply to whoever sent them.
    pub fn request_ids(&self) -> Vec<RequestId> {
        match self {
            Event::ActionDone { request, .. }
            | Event::AttachmentFetched { request, .. }
            | Event::BodyReady { request, .. }
            | Event::CommandFailed { request, .. }
            | Event::Restored { request, .. }
            | Event::RuleApplied { request, .. }
            | Event::ServerSearchDone { request, .. } => vec![*request],
            Event::Synced { requests, .. } => requests.clone(),
            Event::Activity { .. }
            | Event::BodyPreview { .. }
            | Event::Connected { .. }
            | Event::Error { .. }
            | Event::Indexed { .. }
            | Event::NewMail { .. } => Vec::new(),
        }
    }
}

#[derive(Debug, Default)]
pub struct RulesRun {
    pub evaluated: usize,
    pub actions: usize,
    /// NewMail notifications and per-folder or per-message errors that did not stop the run.
    pub events: Vec<Event>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Activity {
    Connecting,
    ListingFolders,
    SyncingFolder {
        folder: String,
        index: usize,
        of: usize,
    },
    FetchingHeaders {
        folder: String,
        done: usize,
        total: usize,
    },
    FetchingBodies {
        folder: String,
        done: usize,
        total: usize,
    },
    RunningRules {
        folder: String,
    },
    RunningCommand {
        what: String,
    },
    /// Up to date, and indexing message bodies for search in the meantime: `done` of `total` messages.
    Indexing {
        done: u64,
        total: u64,
    },
    Idle {
        since: i64,
    },
    Offline {
        reason: String,
        retry_at: i64,
    },
    /// The server refused the login, so the account waits an hour, or for a sync request, before trying again: each
    /// retry is a failed login, and servers lock the account or ban the address after a run of them.
    LoginRejected {
        reason: String,
        retry_at: i64,
    },
    /// The account has no sync thread, so it never syncs until the daemon restarts or its config changes.
    NotRunning {
        reason: String,
    },
}

impl fmt::Display for Activity {
    /// The status line text the window and `postvak daemon status` show.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Activity::Connecting => "connecting…".into(),
            Activity::ListingFolders => "listing folders".into(),
            Activity::SyncingFolder { folder, index, of } => {
                format!("{} ({index}/{of})", clean(folder, false))
            }
            Activity::FetchingHeaders {
                folder,
                done,
                total,
            } => fetching(folder, "headers", *done, *total),
            Activity::FetchingBodies {
                folder,
                done,
                total,
            } => fetching(folder, "bodies", *done, *total),
            Activity::RunningRules { folder } => {
                format!("running rules on {}", clean(folder, false))
            }
            Activity::RunningCommand { what } => clean(what, false),
            Activity::Indexing { done, total } => format!(
                "up to date · indexing {} of {}",
                group_digits(*done, ','),
                group_digits(*total, ',')
            ),
            Activity::Idle { since } => format!("up to date · {}", clock(*since)),
            Activity::Offline { reason, retry_at } => {
                format!(
                    "offline ({}) · retry {}",
                    clean(reason, false),
                    clock(*retry_at)
                )
            }
            Activity::LoginRejected { reason, retry_at } => {
                format!(
                    "login rejected ({}) · retry {}",
                    clean(reason, false),
                    clock(*retry_at)
                )
            }
            Activity::NotRunning { reason } => format!("not running ({})", clean(reason, false)),
        };
        f.write_str(&text)
    }
}

/// "INBOX headers 1,200 / 5,000".
fn fetching(folder: &str, what: &str, done: usize, total: usize) -> String {
    format!(
        "{} {what} {} / {}",
        clean(folder, false),
        group_digits(done as u64, ','),
        group_digits(total as u64, ',')
    )
}

/// `n` with `separator` between groups of three digits.
pub fn group_digits(n: u64, separator: char) -> String {
    let digits = n.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            grouped.push(separator);
        }
        grouped.push(digit);
    }
    grouped
}

/// Work the daemon asks an account's sync thread to do on its connection, on a client's behalf.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Command {
    /// `by` is logged as the action's rule name.
    Apply {
        folder: String,
        uids: Vec<u32>,
        #[serde(with = "ActionDef")]
        action: Action,
        by: String,
    },
    ApplyRule {
        name: String,
    },
    FetchBody {
        folder: String,
        uid: u32,
        /// Sends a large message's `BodyPreview` before fetching the whole of it, for a reader.
        #[serde(default)]
        preview: bool,
    },
    /// Attachment `index` (1-based, as the reader lists it), from the stored message or fetched on its own.
    FetchAttachment {
        folder: String,
        uid: u32,
        index: usize,
    },
    Restore {
        file: PathBuf,
    },
    /// Searches the server for messages with every word of `query`, in `folder` or every synced folder.
    SearchServer {
        folder: Option<String>,
        query: String,
    },
    SyncNow,
}

/// `Action` skips its user-only variants in rules.toml, so the wire derives them separately.
#[derive(Serialize, Deserialize)]
#[serde(remote = "Action", rename_all = "snake_case")]
enum ActionDef {
    Archive,
    Delete,
    Flag,
    Junk,
    MarkRead,
    MarkUnread,
    Move(String),
    NotJunk,
    Notify,
    Silent,
    Stop,
    Tag(String),
    Trash,
    Unflag,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub request: RequestId,
    pub command: Command,
}

/// The pass a command needs after it; when several ask, the larger wins.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum NextPass {
    #[default]
    None,
    /// Syncs INBOX and runs the rules, e.g. on mail a move put in a folder it already synced.
    Inbox,
    Full,
}

#[derive(Debug, Default, PartialEq)]
pub struct CommandsRun {
    pub used_connection: bool,
    pub next: NextPass,
}

/// Called between steps of a pass with what is happening; may run queued commands on the connection.
/// Returns true when it used the connection, so the caller selects its folder again.
pub type Checkpoint<'a> = dyn FnMut(&mut dyn MailOps, Activity) -> Result<bool, SyncError> + 'a;

/// Envelopes are fetched this many messages at a time, each batch committed on its own.
const CHUNK: usize = 500;

#[cfg(any(test, feature = "testing"))]
thread_local! {
    /// Replaces `CHUNK` for syncs on this thread, so a test proves chunking with a dozen messages.
    pub static CHUNK_OVERRIDE: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

fn chunk_size() -> usize {
    #[cfg(any(test, feature = "testing"))]
    if let Some(size) = CHUNK_OVERRIDE.get() {
        return size;
    }
    CHUNK
}

pub fn sync_folder(
    ops: &mut dyn MailOps,
    store: &Store,
    folder: &RemoteFolder,
) -> Result<usize, SyncError> {
    sync_folder_with(ops, store, folder, &mut |_, _| Ok(false))
}

pub fn sync_folder_with(
    ops: &mut dyn MailOps,
    store: &Store,
    folder: &RemoteFolder,
    checkpoint: &mut Checkpoint<'_>,
) -> Result<usize, SyncError> {
    let info = ops.select(&folder.name)?;
    let stored = store.folder(&folder.name)?;
    // Newly tracked, a placeholder, or reset: everything listed now was already on the server.
    let (last_uid, starts_tracking) = match &stored {
        Some(f) if f.uidvalidity == info.uidvalidity => (f.last_uid, false),
        // uidvalidity 0 marks a placeholder created by a rule move into a folder not synced yet.
        Some(f) if f.uidvalidity != 0 => {
            log::info!(
                "{}: UIDVALIDITY changed {} -> {}, resyncing",
                folder.name,
                f.uidvalidity,
                info.uidvalidity
            );
            (0, true)
        }
        _ => (0, true),
    };

    let flag_fetch = if starts_tracking {
        FlagFetch::None
    } else {
        flag_fetch(
            store.flags_synced(&folder.name)?,
            info.highest_modseq,
            now(),
        )
    };
    let updates = match flag_fetch {
        FlagFetch::None => Vec::new(),
        FlagFetch::All => ops.fetch_flags(last_uid, None)?,
        FlagFetch::ChangedSince(modseq) => ops.fetch_flags(last_uid, Some(modseq))?,
    };
    // Every uid is below UIDNEXT, so one no higher than last_uid + 1 means nothing new.
    let uids: Vec<u32> = if info.uidnext.is_some_and(|next| next <= last_uid + 1) {
        Vec::new()
    } else {
        ops.search_uids(last_uid + 1)?
            .into_iter()
            .filter(|&uid| uid > last_uid)
            .collect()
    };
    // Without an expunge, the server holds exactly the stored messages plus the new ones.
    let present: Option<Vec<u32>> = if flag_fetch == FlagFetch::All {
        Some(updates.iter().map(|u| u.uid).collect())
    } else if !starts_tracking
        && info.exists as usize != store.message_count(&folder.name)? as usize + uids.len()
    {
        Some(ops.search_uids(1)?)
    } else {
        None
    };
    let row = |last_uid| Folder {
        name: folder.name.clone(),
        uidvalidity: info.uidvalidity,
        last_uid,
        special_use: folder.special_use.clone(),
        delimiter: folder.delimiter.clone(),
    };

    store.transaction(|| {
        if starts_tracking && stored.is_some() {
            store.reset_folder(&folder.name, info.uidvalidity)?;
        }
        store.upsert_folder(&row(last_uid))?;
        // Unknown until this sync completes, so a pass cut short never looks unchanged to the next.
        store.set_uid_next(&folder.name, 0)?;
        if starts_tracking {
            store.set_initial_uid_next(&folder.name, uids.last().map_or(0, |uid| uid + 1))?;
            store.set_rules_uid(&folder.name, 0)?;
            store.set_notified_uid(&folder.name, 0)?;
        }
        for u in &updates {
            store.update_flags(&folder.name, u.uid, &u.flags.join(" "))?;
        }
        if let Some(present) = &present {
            store.remove_missing(&folder.name, last_uid, present)?;
        }
        // New messages are fetched after the select, so their flags are at least this fresh.
        if starts_tracking || flag_fetch != FlagFetch::None {
            store.set_flags_synced(&folder.name, info.highest_modseq.unwrap_or(0), now())?;
        }
        Ok::<_, SyncError>(())
    })?;

    let total = uids.len();
    let mut done = 0;
    let mut new = 0;
    let mut complete = true;
    for chunk in uids.chunks(chunk_size()) {
        let progress = Activity::FetchingHeaders {
            folder: folder.name.clone(),
            done,
            total,
        };
        // A command ran on the connection and may have selected another folder.
        if checkpoint(ops, progress)? {
            let uidvalidity = ops.select(&folder.name)?.uidvalidity;
            if uidvalidity != info.uidvalidity {
                log::info!(
                    "{}: UIDVALIDITY changed {} -> {} during sync, resyncing next pass",
                    folder.name,
                    info.uidvalidity,
                    uidvalidity
                );
                complete = false;
                break;
            }
        }
        let (first, last) = (chunk[0], chunk[chunk.len() - 1]);
        let envelopes = ops.fetch_envelopes(first, last)?;
        store.transaction(|| {
            for env in envelopes {
                store.insert_message(&to_message(&folder.name, env))?;
                new += 1;
            }
            store.upsert_folder(&row(last))
        })?;
        done += chunk.len();
    }
    if complete {
        store.set_uid_next(&folder.name, info.uidnext.unwrap_or(0))?;
    }
    if total > 0 {
        checkpoint(
            ops,
            Activity::FetchingHeaders {
                folder: folder.name.clone(),
                done,
                total,
            },
        )?;
    }
    Ok(new)
}

/// Without CONDSTORE, flag changes made elsewhere show up within this long.
const FULL_FLAGS_EVERY_SECS: i64 = 30 * 60;

#[derive(Debug, PartialEq)]
enum FlagFetch {
    None,
    All,
    ChangedSince(u64),
}

/// Which flags a tracked folder needs; a full fetch of a large folder costs megabytes, which Gmail counts against its daily cap.
fn flag_fetch(
    (stored_modseq, synced_at): (u64, i64),
    highest_modseq: Option<u64>,
    now: i64,
) -> FlagFetch {
    match highest_modseq {
        Some(highest) if stored_modseq == 0 || highest < stored_modseq => FlagFetch::All,
        Some(highest) if highest == stored_modseq => FlagFetch::None,
        Some(_) => FlagFetch::ChangedSince(stored_modseq),
        None if now - synced_at >= FULL_FLAGS_EVERY_SECS => FlagFetch::All,
        None => FlagFetch::None,
    }
}

fn to_message(folder: &str, env: Envelope) -> Message {
    let parsed = parse_headers(&env.headers);
    Message {
        folder: folder.to_string(),
        uid: env.uid,
        thread_id: thread_id(&parsed, folder, env.uid),
        message_id: parsed.message_id,
        from_addr: parsed.from,
        to_addr: parsed.to,
        cc_addr: parsed.cc,
        delivered_to: parsed.delivered_to,
        in_reply_to: parsed.in_reply_to,
        refs: if parsed.references.is_empty() {
            None
        } else {
            Some(parsed.references.join(" "))
        },
        subject: parsed.subject,
        date: parsed.date,
        internaldate: env.internaldate,
        flags: env.flags.join(" "),
        size: env.size,
        headers: env.headers,
        body_text: None,
        attachment_names: None,
        indexed_at: None,
    }
}

/// One index slice with an unlimited budget, for tests that index a fake server's mail.
#[cfg(any(test, feature = "testing"))]
pub fn index_slice(
    ops: &mut dyn MailOps,
    store: &Store,
    account: &AccountConfig,
    events: &Sender<Event>,
) -> Result<IndexOutcome, SyncError> {
    index::slice(ops, store, account, events, &mut None, &mut || {
        Duration::ZERO
    })
}

#[cfg(any(test, feature = "testing"))]
pub fn sync_all(ops: &mut dyn MailOps, store: &Store) -> Result<(usize, Vec<String>), SyncError> {
    sync_all_with(ops, store, &mut |_, _| Ok(false))
}

/// Syncs every listed folder. A folder that fails is skipped and reported in the second list; the others still sync.
/// A lost connection or a stop ends the pass with that error instead.
pub fn sync_all_with(
    ops: &mut dyn MailOps,
    store: &Store,
    checkpoint: &mut Checkpoint<'_>,
) -> Result<(usize, Vec<String>), SyncError> {
    checkpoint(ops, Activity::ListingFolders)?;
    let selected_at_list = ops.selected().map(str::to_string);
    let mut statuses = HashMap::new();
    let mut folders = Vec::new();
    for (folder, status) in ops.list_folders_status()? {
        if let Some(status) = status
            && selected_at_list.as_deref() != Some(folder.name.as_str())
        {
            statuses.insert(folder.name.clone(), status);
        }
        folders.push(folder);
    }
    // They hold no messages, so nothing selects them; a move to one is refused by name.
    let (noselect, mut folders): (Vec<RemoteFolder>, Vec<RemoteFolder>) =
        folders.into_iter().partition(|f| f.noselect);
    let names: Vec<&str> = noselect.iter().map(|f| f.name.as_str()).collect();
    store.set_noselect_folders(&names)?;
    special_use_by_name(&mut folders);
    // Gmail's All Mail, Starred and Important hold copies of messages in other folders; syncing them doubles the traffic.
    folders.retain(|f| {
        !matches!(
            f.special_use.as_deref(),
            Some("All" | "Flagged" | "Important")
        )
    });
    // New mail shows up before the rest of a first sync, which can take minutes.
    folders.sort_by_key(|f| !f.name.eq_ignore_ascii_case("INBOX"));
    let of = folders.len();
    let mut new = 0;
    let mut errors = Vec::new();
    for (index, folder) in folders.iter().enumerate() {
        let syncing = Activity::SyncingFolder {
            folder: folder.name.clone(),
            index: index + 1,
            of,
        };
        checkpoint(ops, syncing)?;
        let synced = match unchanged(ops, store, folder, statuses.remove(&folder.name)) {
            Ok(true) => Ok(0),
            Ok(false) => sync_folder_with(ops, store, folder, checkpoint),
            Err(e) => Err(e),
        };
        match synced {
            Ok(found) => new += found,
            Err(e) if matches!(e, SyncError::Stopped) || e.is_lost_connection() => return Err(e),
            Err(e) => errors.push(format!("{}: {e}; skipped this pass", folder.name)),
        }
    }
    forget_unlisted_folders(store, &folders)?;
    // A folder whose first sync failed has no row yet, so only a pass without errors vouches for the list.
    if errors.is_empty() {
        store.set_folders_listed(now())?;
    }
    Ok((new, errors))
}

/// Whether `folder` is as its last complete sync left it, by `listed` (its status from LIST-STATUS) or else a STATUS,
/// so the pass can skip its SELECT. INBOX, which IDLE watches, always syncs, and so does the selected folder, since
/// RFC 3501 6.3.10 forbids STATUS as a new-mail check there. Without CONDSTORE the full flag fetch, when due, needs the
/// SELECT too. A server refusing the STATUS gets the SELECT instead.
fn unchanged(
    ops: &mut dyn MailOps,
    store: &Store,
    folder: &RemoteFolder,
    listed: Option<StatusInfo>,
) -> Result<bool, SyncError> {
    if folder.name.eq_ignore_ascii_case("INBOX") || ops.selected() == Some(folder.name.as_str()) {
        return Ok(false);
    }
    let Some(stored) = store.folder(&folder.name)? else {
        return Ok(false);
    };
    let uid_next = store.uid_next(&folder.name)?;
    // uidvalidity 0 is a rule move's placeholder, uid_next 0 a folder whose last sync did not complete.
    if stored.uidvalidity == 0 || uid_next == 0 {
        return Ok(false);
    }
    let status = match listed {
        Some(status) => status,
        None => match ops.status(&folder.name) {
            Ok(status) => status,
            Err(e) if e.is_lost_connection() => return Err(e.into()),
            Err(e) => {
                log::debug!("{}: STATUS failed, selecting it instead: {e}", folder.name);
                return Ok(false);
            }
        },
    };
    let same = status.uidvalidity == stored.uidvalidity
        && status.uidnext == uid_next
        && status.messages == store.message_count(&folder.name)?
        && flag_fetch(
            store.flags_synced(&folder.name)?,
            status.highest_modseq,
            now(),
        ) == FlagFetch::None;
    let listing_changed = stored.special_use != folder.special_use
        || (folder.delimiter.is_some() && stored.delimiter != folder.delimiter);
    if same && listing_changed {
        store.upsert_folder(&Folder {
            special_use: folder.special_use.clone(),
            delimiter: folder.delimiter.clone(),
            ..stored
        })?;
    }
    Ok(same)
}

/// Drops store folders this pass did not sync, with their messages; rule-move placeholders wait for the server to list them.
fn forget_unlisted_folders(store: &Store, synced: &[RemoteFolder]) -> Result<(), SyncError> {
    for stored in store.folders()? {
        if stored.uidvalidity != 0 && !synced.iter().any(|f| f.name == stored.name) {
            log::info!(
                "{}: no longer synced, removing it from the store",
                stored.name
            );
            store.remove_folder(&stored.name)?;
        }
    }
    Ok(())
}

/// Spec section 6: a role no folder is marked with goes to a top-level folder, or one directly under `INBOX` + delimiter,
/// named after it, ignoring case; the earlier name wins. Soverin marks its `Archive` folder `\\All`, so the Archive role
/// may take a `\\All` folder by name.
fn special_use_by_name(folders: &mut [RemoteFolder]) {
    const NAMES: [(&str, &[&str]); 5] = [
        ("Archive", &["Archive", "Archives"]),
        ("Drafts", &["Drafts"]),
        ("Junk", &["Junk", "Junk E-mail", "Spam"]),
        ("Sent", &["Sent", "Sent Items", "Sent Messages"]),
        ("Trash", &["Trash", "Deleted Items", "Bin"]),
    ];
    for (role, names) in NAMES {
        if folders
            .iter()
            .any(|f| f.special_use.as_deref() == Some(role))
        {
            continue;
        }
        let Some(index) = names.iter().find_map(|name| {
            folders.iter().position(|f| {
                let unmarked = f.special_use.is_none()
                    || (role == "Archive" && f.special_use.as_deref() == Some("All"));
                unmarked && top_level_name(f).eq_ignore_ascii_case(name)
            })
        }) else {
            continue;
        };
        folders[index].special_use = Some(role.to_string());
    }
}

/// The folder's name without a leading `INBOX` + delimiter; a nested folder's name keeps its delimiter, so it never
/// equals a role's name.
fn top_level_name(folder: &RemoteFolder) -> &str {
    crate::folder_name::inbox_prefix(&folder.name, folder.delimiter.as_deref())
        .unwrap_or(&folder.name)
}

pub fn load_rules_for(
    store: &Store,
    path: &Path,
    now: i64,
) -> Result<Vec<CompiledRule>, RulesError> {
    let file = crate::rules::load(path)?;
    let mut compiled = crate::rules::compile(&file)?;
    // A disabled rule, such as a pending proposal, starts its clock when it is enabled, not when it was written.
    let enabled: Vec<&str> = compiled
        .iter()
        .filter(|r| r.rule.enabled)
        .map(|r| r.rule.name.as_str())
        .collect();
    store.forget_rules_except(&enabled)?;
    for rule in compiled.iter_mut().filter(|r| r.rule.enabled) {
        rule.first_seen_at =
            store.rule_first_seen(&rule.rule.name, &rule.rule.definition()?, now)?;
    }
    Ok(compiled)
}

#[allow(clippy::too_many_arguments)]
pub fn run_rules(
    ops: &mut dyn MailOps,
    store: &Store,
    trash: &Trash,
    rules: &[CompiledRule],
    account: &AccountConfig,
    identity: &Identity,
    mode: Mode,
    now: i64,
) -> Result<RulesRun, SyncError> {
    run_rules_with(
        ops,
        store,
        trash,
        rules,
        account,
        identity,
        mode,
        now,
        &mut |_, _| Ok(false),
    )
}

#[allow(clippy::too_many_arguments)]
pub fn run_rules_with(
    ops: &mut dyn MailOps,
    store: &Store,
    trash: &Trash,
    rules: &[CompiledRule],
    account: &AccountConfig,
    identity: &Identity,
    mode: Mode,
    now: i64,
    checkpoint: &mut Checkpoint<'_>,
) -> Result<RulesRun, SyncError> {
    let ctx = Context {
        account: &account.name,
        identity,
        now,
        mode,
        notify_default: account.notify,
        keep_in_folder: false,
    };
    let mut folders: Vec<String> = rules
        .iter()
        .filter(|r| r.rule.enabled && r.applies_to_account(&account.name))
        .map(|r| {
            // The store's spelling, so a rule written in NFD runs in the folder the server lists in NFC.
            Ok(crate::folder_name::known(store, r.folder())?
                .map_or_else(|| r.folder().to_string(), |f| f.name))
        })
        .collect::<Result<_, StoreError>>()?;
    folders.push("INBOX".to_string());
    folders.sort();
    folders.dedup();

    let mut run = RulesRun::default();
    'folders: for folder in folders.iter().cloned() {
        // Before any select, so a command run here leaves nothing to select again.
        checkpoint(
            ops,
            Activity::RunningRules {
                folder: folder.clone(),
            },
        )?;
        let Some(stored) = store.folder(&folder)? else {
            continue;
        };
        // A rule move's placeholder has no server uids to act on until the next full sync adopts the folder.
        if stored.uidvalidity == 0 {
            continue;
        }
        let needs_body = folder_needs_body(rules, &account.name, &folder);
        let (rules_uid, notified_uid) = (store.rules_uid(&folder)?, store.notified_uid(&folder)?);
        let unseen = unseen_by_rules(store, &stored)?;
        let fresh = |uid: u32| mode == Mode::Normal && unseen(uid);
        // Rules look at every message, so a folder of 100k is read on each pass; without a body condition the indexed
        // bodies stay unread.
        let messages = if needs_body {
            store.messages_in_folder(&folder)?
        } else {
            store.messages_without_body(&folder, 0)?
        };
        let highest_uid = messages.last().map(|m| m.uid.min(stored.last_uid));
        // A rule's body condition does not download a message larger than this; such mail counts as body unknown.
        let wants_body = |m: &Message| {
            needs_body
                && m.body_text.is_none()
                && fresh(m.uid)
                && m.size.is_none_or(|size| size <= MAX_RULE_BODY)
        };
        let bodies_total = if needs_body {
            messages.iter().filter(|m| wants_body(m)).count()
        } else {
            0
        };
        let mut bodies_done = 0;
        // Set once a command used the connection; from then on each row is read again, as it may have moved or changed.
        let mut commands_ran = false;
        // Rules are evaluated from the store; the folder is selected only once a message needs the server.
        let mut selected = false;
        for mut msg in messages {
            let is_fresh = fresh(msg.uid);
            // Mail found by a first sync or resync already sat on the server; fetching it would download the whole folder.
            if wants_body(&msg) {
                let progress = Activity::FetchingBodies {
                    folder: folder.clone(),
                    done: bodies_done,
                    total: bodies_total,
                };
                if checkpoint(ops, progress)? {
                    commands_ran = true;
                    selected = false;
                }
            }
            if commands_ran {
                match store.message(&folder, msg.uid)? {
                    Some(current) => msg = current,
                    None => continue,
                }
            }
            if wants_body(&msg) {
                if !select_for_rules(ops, &stored, &mut selected, account, &mut run.events)? {
                    continue 'folders;
                }
                match ensure_body_text(&msg, ops, store) {
                    Ok(text) => msg.body_text = Some(text),
                    Err(e) if e.is_lost_connection() => return Err(e.into()),
                    Err(e) => log::warn!("{}/{}: body fetch failed: {e}", msg.folder, msg.uid),
                }
                bodies_done += 1;
            }
            let plan = plan_for(rules, &msg, &ctx, store)?;
            run.evaluated += 1;
            let notifies = is_fresh && msg.uid > notified_uid && plan.notify && folder == "INBOX";
            // A notification waits for the SELECT too: mail a resync finds never notifies.
            if (notifies || !plan.actions.is_empty())
                && !select_for_rules(ops, &stored, &mut selected, account, &mut run.events)?
            {
                continue 'folders;
            }
            if !plan.actions.is_empty() {
                match apply(&plan, &msg, ops, store, trash, now) {
                    Ok(executed) => run.actions += executed,
                    // The rest of the pass would fail the same way; end it so the engine reconnects.
                    Err(e) if e.is_lost_connection() => return Err(e.into()),
                    Err(e) => {
                        run.events.push(account_error(
                            account,
                            format!("{}/{}: {e}", msg.folder, msg.uid),
                        ));
                        continue;
                    }
                }
            }
            if notifies {
                run.events.push(new_mail(account, &msg));
            }
        }
        if mode == Mode::Normal
            && let Some(highest_uid) = highest_uid
        {
            if highest_uid > rules_uid {
                store.set_rules_uid(&folder, highest_uid)?;
            }
            if highest_uid > notified_uid {
                store.set_notified_uid(&folder, highest_uid)?;
            }
        }
    }
    if mode == Mode::Normal {
        // Folders no rule looks at count as seen up to their last uid, so a rule added for one later finds no old
        // mail fresh.
        store.mark_rules_seen_except(&folders)?;
    }
    Ok(run)
}

/// Selects the folder of `stored` unless `selected` says it still is, and checks it is the folder the store holds.
/// False, with an error event, when the folder's rules must stop: the SELECT failed, or the server's UIDVALIDITY is no
/// longer the stored one, so the stored uids would name unrelated server messages. A lost connection is an error.
fn select_for_rules(
    ops: &mut dyn MailOps,
    stored: &Folder,
    selected: &mut bool,
    account: &AccountConfig,
    events: &mut Vec<Event>,
) -> Result<bool, SyncError> {
    let folder = &stored.name;
    if *selected && ops.selected() == Some(folder.as_str()) {
        return Ok(true);
    }
    let message = match ops.select(folder) {
        Ok(info) => match actions::check_selected(stored, &info) {
            actions::Selected::Synced => {
                *selected = true;
                return Ok(true);
            }
            actions::Selected::Placeholder => {
                format!("{folder}: not synced yet, skipping its rules")
            }
            actions::Selected::Changed => {
                format!("{folder}: folder changed on server; resync needed, skipping its rules")
            }
        },
        Err(e) if e.is_lost_connection() => return Err(e.into()),
        Err(e) => format!("{folder}: {e}; skipping its rules"),
    };
    events.push(account_error(account, message));
    Ok(false)
}

/// The largest message, in bytes, a rule's body condition downloads by itself.
pub const MAX_RULE_BODY: u32 = 25 * 1024 * 1024;

/* Whether a uid is fresh: not yet seen by the rules, not already on the server when the folder was first tracked, and
synced. A row a move stored above last_uid waits for sync, so unsynced mail below it is not marked seen too early. */
fn unseen_by_rules(
    store: &Store,
    folder: &Folder,
) -> Result<impl Fn(u32) -> bool + use<>, StoreError> {
    let rules_uid = store.rules_uid(&folder.name)?;
    let initial_uid_next = store.initial_uid_next(&folder.name)?;
    let last_uid = folder.last_uid;
    Ok(move |uid| uid > rules_uid && uid >= initial_uid_next && uid <= last_uid)
}

fn new_mail(account: &AccountConfig, msg: &Message) -> Event {
    Event::NewMail {
        account: account.name.clone(),
        folder: msg.folder.clone(),
        uid: msg.uid,
        from: msg.from_addr.clone().unwrap_or_default(),
        subject: msg.subject.clone().unwrap_or_default(),
    }
}

fn account_error(account: &AccountConfig, message: String) -> Event {
    log::warn!("{}: {message}", account.name);
    Event::Error {
        account: account.name.clone(),
        message,
    }
}

/// Runs every queued command in arrival order. A failed command is reported and the next one runs; a lost
/// connection ends the drain with the error, so the session reconnects. `SyncNow` requests go onto
/// `carried.sync_requests` for the next full pass to answer.
#[allow(clippy::too_many_arguments)]
fn run_commands(
    ops: &mut dyn MailOps,
    store: &Store,
    trash: &Trash,
    account: &AccountConfig,
    identity: &Identity,
    rules_path: &Path,
    commands: &Receiver<Job>,
    carried: &mut Carried,
    events: &Sender<Event>,
) -> Result<CommandsRun, SyncError> {
    let mut run = CommandsRun::default();
    while let Ok(Job { request, command }) = commands.try_recv() {
        if command == Command::SyncNow {
            run.next = NextPass::Full;
            carried.sync_requests.push(request);
            continue;
        }
        carried.running = Some(request);
        let ran = run_command(
            ops, store, trash, account, identity, rules_path, request, command, events,
        );
        carried.running = None;
        let ran = ran?;
        run.used_connection |= ran.used_connection;
        run.next = run.next.max(ran.next);
    }
    Ok(run)
}

/// Syncs `folder` now so front ends see a change at once, and the rules pass after it finds the mail; a failure is
/// left to the next full pass.
fn sync_now(ops: &mut dyn MailOps, store: &Store, folder: &str) {
    let stored = store.folder(folder).ok().flatten();
    let remote = RemoteFolder {
        name: folder.to_string(),
        special_use: stored.as_ref().and_then(|f| f.special_use.clone()),
        delimiter: stored.and_then(|f| f.delimiter),
        noselect: false,
    };
    if let Err(e) = sync_folder(ops, store, &remote) {
        log::warn!("{folder}: sync after a command failed: {e}");
    }
}

/// Runs one command other than `SyncNow` and sends its answer; `Err` only for a lost connection.
#[allow(clippy::too_many_arguments)]
fn run_command(
    ops: &mut dyn MailOps,
    store: &Store,
    trash: &Trash,
    account: &AccountConfig,
    identity: &Identity,
    rules_path: &Path,
    request: RequestId,
    command: Command,
    events: &Sender<Event>,
) -> Result<CommandsRun, SyncError> {
    crate::crash::note(&command);
    let name = || account.name.clone();
    let failed = |request, message: String| {
        log::warn!("{}: {message}", account.name);
        Event::CommandFailed {
            account: account.name.clone(),
            request,
            message,
        }
    };
    let mut run = CommandsRun::default();
    // Prefixed to a failure's message, so it names the message it is about.
    let mut context = String::new();
    let answer: Result<Event, SyncError> = match command {
        // run_commands holds SyncNow for the next full pass, so none arrives here; answer it rather than panic if one does.
        Command::SyncNow => Ok(failed(
            request,
            "sync now is answered by the next full pass".to_string(),
        )),
        Command::Apply {
            folder,
            uids,
            action,
            by,
        } => {
            run.used_connection = true;
            let _ = events.send(Event::Activity {
                account: name(),
                activity: Activity::RunningCommand {
                    what: describe(&action, uids.len()),
                },
            });
            let outcome = actions::run(ops, store, trash, &folder, &uids, &action, &by, now());
            let (results, lost) = match outcome {
                Ok(results) => split_lost(results),
                Err(e) => (
                    uids.iter().map(|&uid| (uid, Err(e.to_string()))).collect(),
                    e.is_lost_connection().then_some(e),
                ),
            };
            /* A move only drops the source row, so the target is synced before `ActionDone` makes the GUI reload. The
            rules pass after it runs the target folder's rules. */
            if results.iter().any(|(_, result)| result.is_ok()) {
                let target = crate::rules::apply::move_target(store, &folder, &action);
                if let Ok(Some(target)) = target {
                    sync_now(ops, store, &target);
                }
                if matches!(
                    action,
                    Action::Archive
                        | Action::Delete
                        | Action::Junk
                        | Action::Move(_)
                        | Action::NotJunk
                        | Action::Trash
                ) {
                    run.next = NextPass::Inbox;
                }
            }
            let _ = events.send(Event::ActionDone {
                account: name(),
                folder,
                results,
                request,
            });
            return match lost {
                Some(e) => Err(e.into()),
                None => Ok(run),
            };
        }
        Command::ApplyRule { name: rule_name } => {
            let rule = match crate::rules::enabled_rule(rules_path, &rule_name) {
                Ok(rule) => rule,
                Err(message) => {
                    let _ = events.send(failed(request, message.to_string()));
                    return Ok(run);
                }
            };
            run.used_connection = true;
            let rules = std::slice::from_ref(&rule);
            run_rules(
                ops,
                store,
                trash,
                rules,
                account,
                identity,
                Mode::ApplyExisting,
                now(),
            )
            .map(|rules_run| Event::RuleApplied {
                account: name(),
                request,
                evaluated: rules_run.evaluated,
                actions: rules_run.actions,
                errors: rules_run
                    .events
                    .into_iter()
                    .filter_map(|event| match event {
                        Event::Error { message, .. } => Some(message),
                        _ => None,
                    })
                    .collect(),
            })
        }
        Command::FetchBody {
            folder,
            uid,
            preview,
        } => {
            run.used_connection = true;
            context = format!("{folder}/{uid}: ");
            let mut show = |preview| {
                let _ = events.send(Event::BodyPreview {
                    account: name(),
                    folder: folder.clone(),
                    uid,
                    preview: Box::new(preview),
                });
            };
            let show: Option<&mut dyn FnMut(actions::Preview)> = preview.then_some(&mut show);
            actions::fetch_body(ops, store, &folder, uid, show)
                .map(|()| Event::BodyReady {
                    account: name(),
                    folder,
                    uid,
                    request,
                })
                .map_err(SyncError::from)
        }
        Command::FetchAttachment { folder, uid, index } => {
            run.used_connection = true;
            context = format!("{folder}/{uid}: ");
            let account = name();
            actions::fetch_attachment(ops, store, &folder, uid, index)
                .map(|(name, contents)| Event::AttachmentFetched {
                    account,
                    folder,
                    uid,
                    index,
                    name,
                    contents,
                    request,
                })
                .map_err(SyncError::from)
        }
        Command::SearchServer { folder, query } => {
            run.used_connection = true;
            search_server_in(ops, store, folder.as_deref(), &query)
                .map(|hits| Event::ServerSearchDone {
                    account: name(),
                    request,
                    query,
                    hits,
                })
                .map_err(SyncError::from)
        }
        Command::Restore { file } => {
            run.used_connection = true;
            trash
                .restore(ops, &file)
                .map(|folder| {
                    sync_now(ops, store, &folder);
                    run.next = NextPass::Inbox;
                    Event::Restored {
                        account: name(),
                        folder,
                        request,
                    }
                })
                .map_err(SyncError::from)
        }
    };
    match answer {
        Ok(event) => {
            let _ = events.send(event);
            Ok(run)
        }
        Err(e) => {
            let _ = events.send(failed(request, format!("{context}{e}")));
            if e.is_lost_connection() {
                Err(e)
            } else {
                Ok(run)
            }
        }
    }
}

/// Searches the named folder on the server, failing if it cannot, or every synced folder, skipping any that fail. Only a
/// lost connection stops the walk.
fn search_server_in(
    ops: &mut dyn MailOps,
    store: &Store,
    folder: Option<&str>,
    query: &str,
) -> Result<Vec<(String, u32)>, ActionError> {
    let words = actions::search_words(query);
    let folders = match folder {
        Some(folder) => vec![folder.to_string()],
        None if words.is_empty() => Vec::new(),
        None => store
            .folders()?
            .into_iter()
            .filter(|f| f.uidvalidity != 0)
            .map(|f| f.name)
            .collect(),
    };
    let mut hits = Vec::new();
    for name in folders {
        match actions::search_server(ops, store, &name, &words) {
            Ok(uids) => hits.extend(uids.into_iter().map(|uid| (name.clone(), uid))),
            Err(e) if folder.is_some() || e.is_lost_connection() => return Err(e),
            Err(e) => log::warn!("{name}: server search skipped: {e}"),
        }
    }
    Ok(hits)
}

/// Whether an error means the connection is gone, so a pass ends at once and the engine reconnects.
pub(crate) trait LostConnection {
    fn is_lost_connection(&self) -> bool;
}

impl LostConnection for MailError {
    fn is_lost_connection(&self) -> bool {
        matches!(self, MailError::Io(_) | MailError::Connect(_))
    }
}

impl LostConnection for ApplyError {
    fn is_lost_connection(&self) -> bool {
        matches!(self, ApplyError::Mail(e) if e.is_lost_connection())
    }
}

impl LostConnection for ActionError {
    fn is_lost_connection(&self) -> bool {
        match self {
            ActionError::Mail(e) => e.is_lost_connection(),
            ActionError::Apply(e) => e.is_lost_connection(),
            _ => false,
        }
    }
}

impl LostConnection for RestoreError {
    fn is_lost_connection(&self) -> bool {
        matches!(self, RestoreError::Mail(e) if e.is_lost_connection())
    }
}

impl LostConnection for SyncError {
    fn is_lost_connection(&self) -> bool {
        match self {
            SyncError::Action(e) => e.is_lost_connection(),
            SyncError::Restore(e) => e.is_lost_connection(),
            SyncError::Mail(e) => e.is_lost_connection(),
            SyncError::Apply(e) => e.is_lost_connection(),
            _ => false,
        }
    }
}

pub type EventResults = Vec<(u32, Result<usize, String>)>;

/// Stringifies per-uid results for the event, keeping the first connection loss so it can end the drain.
fn split_lost(results: Vec<actions::UidResult>) -> (EventResults, Option<ActionError>) {
    let mut lost = None;
    let strings = results
        .into_iter()
        .map(|(uid, result)| match result {
            Ok(count) => (uid, Ok(count)),
            Err(e) => {
                let message = e.to_string();
                if lost.is_none() && e.is_lost_connection() {
                    lost = Some(e);
                }
                (uid, Err(message))
            }
        })
        .collect();
    (strings, lost)
}

fn describe(action: &Action, count: usize) -> String {
    let verb = match action {
        Action::Archive => "archiving",
        Action::Delete | Action::Trash => "deleting",
        Action::Flag => "flagging",
        Action::Junk => "marking junk",
        Action::NotJunk => "marking not junk",
        Action::MarkRead => "marking read",
        Action::MarkUnread => "marking unread",
        Action::Move(_) => "moving",
        Action::Notify | Action::Silent | Action::Stop => "updating",
        Action::Tag(_) => "tagging",
        Action::Unflag => "unflagging",
    };
    let plural = if count == 1 { "" } else { "s" };
    format!("{verb} {count} message{plural}")
}

/// Resolves the account's password and logs in.
pub fn connect(account: &AccountConfig) -> Result<ImapOps, SyncError> {
    let secret = credentials::resolve(account)?;
    Ok(ImapOps::connect(account, &secret)?)
}

/// Syncs INBOX alone, draining queued commands first like a full pass does before each folder.
fn sync_inbox_with(
    ops: &mut dyn MailOps,
    store: &Store,
    checkpoint: &mut Checkpoint<'_>,
) -> Result<usize, SyncError> {
    let inbox = RemoteFolder {
        name: "INBOX".into(),
        special_use: None,
        delimiter: None,
        noselect: false,
    };
    let syncing = Activity::SyncingFolder {
        folder: inbox.name.clone(),
        index: 1,
        of: 1,
    };
    checkpoint(ops, syncing)?;
    sync_folder_with(ops, store, &inbox, checkpoint)
}

/// What an account's thread carries from one session to the next.
#[derive(Default)]
struct Carried {
    /// `SyncNow` requests the next full pass answers.
    sync_requests: Vec<RequestId>,
    /// The command running now, so one that panics still gets its answer.
    running: Option<RequestId>,
    rules: LoadedRules,
}

#[derive(Default)]
struct LoadedRules {
    /// The last rules that loaded; None until some have, and while the file is invalid with no rule to fall back on.
    last_good: Option<Vec<CompiledRule>>,
    /// The error of the last load, so each bad edit is reported once rather than every pass.
    error: Option<String>,
}

impl LoadedRules {
    /// Loads the rules file again; when it is invalid keeps the last good rules and reports a new error once.
    fn reload(
        &mut self,
        store: &Store,
        path: &Path,
        account: &AccountConfig,
        events: &Sender<Event>,
    ) {
        match load_rules_for(store, path, now()) {
            Ok(rules) => {
                self.last_good = Some(rules);
                self.error = None;
            }
            Err(e) => {
                self.last_good = self.last_good.take().filter(|rules| !rules.is_empty());
                let kept = if self.last_good.is_some() {
                    "keeping the previous rules"
                } else {
                    "running no rules until it is fixed"
                };
                let error = format!("{e}; {kept}");
                if self.error.as_ref() != Some(&error) {
                    let _ = events.send(account_error(account, error.clone()));
                }
                self.error = Some(error);
            }
        }
    }

    /// Runs the last good rules. With none it only notifies, leaving the mail unseen so the rules catch up on it.
    fn run(
        &self,
        ops: &mut dyn MailOps,
        store: &Store,
        trash: &Trash,
        account: &AccountConfig,
        identity: &Identity,
        checkpoint: &mut Checkpoint<'_>,
    ) -> Result<RulesRun, SyncError> {
        let Some(rules) = &self.last_good else {
            return Ok(RulesRun {
                events: notify_unruled(store, account, identity)?,
                ..RulesRun::default()
            });
        };
        run_rules_with(
            ops,
            store,
            trash,
            rules,
            account,
            identity,
            Mode::Normal,
            now(),
            checkpoint,
        )
    }
}

/// NewMail for fresh INBOX mail no notification covered yet, as the account's notify setting says. Moves only the
/// notified mark, so the rules still catch up on that mail.
fn notify_unruled(
    store: &Store,
    account: &AccountConfig,
    identity: &Identity,
) -> Result<Vec<Event>, SyncError> {
    let Some(inbox) = store.folder("INBOX")? else {
        return Ok(Vec::new());
    };
    let unseen = unseen_by_rules(store, &inbox)?;
    let notified_uid = store.notified_uid("INBOX")?;
    let ctx = Context {
        account: &account.name,
        identity,
        now: now(),
        mode: Mode::Normal,
        notify_default: account.notify,
        keep_in_folder: false,
    };
    // Only mail above `notified_uid` can notify, and the highest of it is all the rest needs.
    let messages = store.messages_without_body("INBOX", notified_uid)?;
    let events = messages
        .iter()
        .filter(|msg| unseen(msg.uid) && msg.uid > notified_uid && evaluate(&[], msg, &ctx).notify)
        .map(|msg| new_mail(account, msg))
        .collect();
    if let Some(highest_uid) = messages
        .last()
        .map(|msg| msg.uid.min(inbox.last_uid))
        .filter(|&uid| uid > notified_uid)
    {
        store.set_notified_uid("INBOX", highest_uid)?;
    }
    Ok(events)
}

/// What one account's passes share: its store, trash and identity.
struct AccountSync<'a> {
    account: &'a AccountConfig,
    store: Store,
    trash: Trash,
    identity: Identity,
    rules_path: PathBuf,
}

impl<'a> AccountSync<'a> {
    fn open(account: &'a AccountConfig, paths: &Paths) -> Result<AccountSync<'a>, SyncError> {
        Ok(AccountSync {
            store: Store::open_account(paths, &account.name)?,
            account,
            trash: Trash::new(paths.trash_dir(&account.name)),
            identity: account.identity()?,
            rules_path: paths.rules_file(),
        })
    }

    /// Reloads the rules, syncs every folder (`full`) or only INBOX, runs the rules and sends the events. A full pass
    /// answers the `SyncNow` requests in `carried` when it starts; ones drained at its checkpoints are kept there for
    /// the next. Queued commands run at the checkpoints; returns the pass they want next.
    fn pass(
        &mut self,
        ops: &mut dyn MailOps,
        full: bool,
        carried: &mut Carried,
        events: &Sender<Event>,
        commands: &Receiver<Job>,
        shutdown: &AtomicBool,
    ) -> Result<NextPass, SyncError> {
        carried
            .rules
            .reload(&self.store, &self.rules_path, self.account, events);
        // The checkpoint holds `carried` for the whole pass, so the rules stay outside it until the pass ends.
        let rules = std::mem::take(&mut carried.rules);
        let result =
            self.sync_and_run_rules(ops, full, &rules, carried, events, commands, shutdown);
        carried.rules = rules;
        result
    }

    #[allow(clippy::too_many_arguments)]
    fn sync_and_run_rules(
        &self,
        ops: &mut dyn MailOps,
        full: bool,
        rules: &LoadedRules,
        carried: &mut Carried,
        events: &Sender<Event>,
        commands: &Receiver<Job>,
        shutdown: &AtomicBool,
    ) -> Result<NextPass, SyncError> {
        let answering = if full { carried.sync_requests.len() } else { 0 };
        let (account, store, trash) = (self.account, &self.store, &self.trash);
        let (identity, rules_path) = (&self.identity, self.rules_path.as_path());
        let activity = |activity| Event::Activity {
            account: account.name.clone(),
            activity,
        };
        let mut next = NextPass::None;
        // After a command hits a lost connection nothing else may touch it this pass.
        let mut lost = None;
        let mut checkpoint = |ops: &mut dyn MailOps, step: Activity| {
            if lost.is_some() {
                return Err(SyncError::Stopped);
            }
            let _ = events.send(activity(step));
            if shutdown.load(Ordering::Acquire) {
                return Err(SyncError::Stopped);
            }
            match run_commands(
                ops, store, trash, account, identity, rules_path, commands, carried, events,
            ) {
                Ok(run) => {
                    next = next.max(run.next);
                    Ok(run.used_connection)
                }
                Err(e) => {
                    lost = Some(e);
                    Err(SyncError::Stopped)
                }
            }
        };
        let synced = if full {
            sync_all_with(ops, store, &mut checkpoint)
        } else {
            sync_inbox_with(ops, store, &mut checkpoint).map(|new| (new, Vec::new()))
        };
        let ran = synced.and_then(|(new, errors)| {
            for message in &errors {
                let _ = events.send(account_error(account, message.clone()));
            }
            let run = rules.run(ops, store, trash, account, identity, &mut checkpoint)?;
            Ok((new, errors, run))
        });
        if let Some(e) = lost {
            return Err(e);
        }
        let (new, mut errors, run) = ran?;
        errors.extend(rules.error.clone());
        for event in run.events {
            if let Event::Error { message, .. } = &event {
                errors.push(message.clone());
            }
            let _ = events.send(event);
        }
        let _ = events.send(Event::Synced {
            account: account.name.clone(),
            new_messages: new,
            actions: run.actions,
            requests: carried.sync_requests.drain(..answering).collect(),
            errors,
        });
        Ok(next)
    }
}

#[cfg(any(test, feature = "testing"))]
pub fn run_once(
    account: &AccountConfig,
    paths: &Paths,
    events: &Sender<Event>,
) -> Result<(), SyncError> {
    let mut ops = connect(account)?;
    let (_, no_commands) = std::sync::mpsc::channel();
    AccountSync::open(account, paths)?.pass(
        &mut ops,
        true,
        &mut Carried::default(),
        events,
        &no_commands,
        &AtomicBool::new(false),
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn run_loop(
    account: AccountConfig,
    paths: Paths,
    events: Sender<Event>,
    shutdown: Arc<AtomicBool>,
    commands: Receiver<Job>,
    wake: Arc<AtomicBool>,
    connect: impl FnMut() -> Result<Box<dyn MailOps>, SyncError>,
) {
    let name = account.name.clone();
    let (stop, woken) = (shutdown.clone(), wake.clone());
    run_loop_paced(
        account,
        paths,
        events,
        shutdown,
        commands,
        wake,
        connect,
        HEALTHY_AFTER,
        |delay| jittered(delay, random_u64()),
        |delay| {
            log::warn!("{name}: reconnecting in {}s", delay.as_secs());
            // Stop or a new command cuts this wait short; clearing the flag keeps later waits at full length.
            let until = std::time::Instant::now() + delay;
            while std::time::Instant::now() < until
                && !stop.load(Ordering::Acquire)
                && !woken.swap(false, Ordering::AcqRel)
            {
                std::thread::sleep(Duration::from_millis(100));
            }
        },
    );
}

/// How long a connection must stay up before the back-off starts over: a server that drops every IDLE keeps
/// doubling it instead of getting a full pass every minute.
const HEALTHY_AFTER: Duration = Duration::from_secs(300);

/// `delay` moved by up to a fifth either way, `r` picking where, so accounts on one server that lost the network
/// together do not retry in lockstep.
fn jittered(delay: Duration, r: u64) -> Duration {
    let span = (delay.as_millis() / 5) as u64;
    let offset = if span == 0 { 0 } else { r % (2 * span + 1) };
    delay - Duration::from_millis(span) + Duration::from_millis(offset)
}

fn random_u64() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish()
}

/// The loop body, with connection and back-off sleeping injected so tests can drive it with the fake; the delays
/// are exact, and a connection counts as healthy once it has stayed up `HEALTHY_AFTER`.
#[allow(clippy::too_many_arguments)]
pub fn run_loop_with(
    account: AccountConfig,
    paths: Paths,
    events: Sender<Event>,
    shutdown: Arc<AtomicBool>,
    commands: Receiver<Job>,
    wake: Arc<AtomicBool>,
    connect: impl FnMut() -> Result<Box<dyn MailOps>, SyncError>,
    sleep: impl FnMut(Duration),
) {
    run_loop_paced(
        account,
        paths,
        events,
        shutdown,
        commands,
        wake,
        connect,
        HEALTHY_AFTER,
        |delay| delay,
        sleep,
    );
}

/// `run_loop_with` with the uptime that counts as healthy and the jitter on each back-off delay as parameters.
#[allow(clippy::too_many_arguments)]
fn run_loop_paced(
    account: AccountConfig,
    paths: Paths,
    events: Sender<Event>,
    shutdown: Arc<AtomicBool>,
    commands: Receiver<Job>,
    wake: Arc<AtomicBool>,
    mut connect: impl FnMut() -> Result<Box<dyn MailOps>, SyncError>,
    healthy_after: Duration,
    mut jitter: impl FnMut(Duration) -> Duration,
    mut sleep: impl FnMut(Duration),
) {
    let mut backoff = Duration::from_secs(5);
    let mut carried = Carried::default();
    while !shutdown.load(Ordering::Acquire) {
        let mut connected_at = None;
        // A bug in one session must still answer its requests and leave the account retrying, not silently dead.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_session(
                &account,
                &paths,
                &events,
                &shutdown,
                &commands,
                &wake,
                &mut carried,
                &mut connect,
                &mut connected_at,
            )
        }))
        .unwrap_or_else(|panic| Err(SyncError::Crashed(panic_message(&*panic))));
        if connected_at.is_some_and(|at: std::time::Instant| at.elapsed() >= healthy_after) {
            backoff = Duration::from_secs(5);
        }
        match result {
            Ok(()) => break,
            Err(_) if shutdown.load(Ordering::Acquire) => break,
            Err(e) => {
                let _ = events.send(Event::Error {
                    account: account.name.clone(),
                    message: e.to_string(),
                });
                let rejected = matches!(e, SyncError::Mail(MailError::Auth(_)));
                let delay = if rejected {
                    LOGIN_RETRY
                } else {
                    jitter(backoff)
                };
                let (reason, retry_at) = (e.to_string(), now() + delay.as_secs() as i64);
                let (message, activity) = if rejected {
                    let message = rejected_message(&account.name, &reason, retry_at);
                    (message, Activity::LoginRejected { reason, retry_at })
                } else {
                    let message = offline_message(&account.name, &reason, retry_at);
                    (message, Activity::Offline { reason, retry_at })
                };
                let _ = events.send(Event::Activity {
                    account: account.name.clone(),
                    activity,
                });
                fail_requests(&account, &events, &commands, &mut carried, &message);
                sleep(delay);
                backoff = if rejected {
                    Duration::from_secs(5)
                } else {
                    (backoff * 2).min(Duration::from_secs(300))
                };
            }
        }
    }
    let stopped = format!("{} stopped", account.name);
    fail_requests(&account, &events, &commands, &mut carried, &stopped);
}

fn panic_message(panic: &(dyn std::any::Any + Send)) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|text| text.to_string())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".into())
}

/// How long an account whose login the server rejected waits before trying again on its own.
pub const LOGIN_RETRY: Duration = Duration::from_secs(3600);

/// Why an account whose login was rejected cannot run a command: `<account>: login rejected (<reason>); fix the
/// password, then sync, or it retries at <HH:MM>`, local time.
pub fn rejected_message(account: &str, reason: &str, retry_at: i64) -> String {
    format!(
        "{account}: login rejected ({reason}); fix the password, then sync, or it retries at {}",
        clock(retry_at)
    )
}

/// Why an offline account cannot run a command: `<account> is offline (<reason>); retrying at <HH:MM>`, local time.
pub fn offline_message(account: &str, reason: &str, retry_at: i64) -> String {
    format!(
        "{account} is offline ({reason}); retrying at {}",
        clock(retry_at)
    )
}

/// Fails the running command, the held `SyncNow` requests and every queued job, so each request still gets its one
/// answer.
fn fail_requests(
    account: &AccountConfig,
    events: &Sender<Event>,
    commands: &Receiver<Job>,
    carried: &mut Carried,
    message: &str,
) {
    let held = carried
        .running
        .take()
        .into_iter()
        .chain(carried.sync_requests.drain(..));
    let queued = std::iter::from_fn(|| commands.try_recv().ok().map(|job| job.request));
    for request in held.chain(queued) {
        let _ = events.send(Event::CommandFailed {
            account: account.name.clone(),
            request,
            message: message.to_string(),
        });
    }
}

#[allow(clippy::too_many_arguments)]
fn run_session(
    account: &AccountConfig,
    paths: &Paths,
    events: &Sender<Event>,
    shutdown: &AtomicBool,
    commands: &Receiver<Job>,
    wake: &AtomicBool,
    carried: &mut Carried,
    connect: &mut impl FnMut() -> Result<Box<dyn MailOps>, SyncError>,
    connected_at: &mut Option<std::time::Instant>,
) -> Result<(), SyncError> {
    let _ = events.send(Event::Activity {
        account: account.name.clone(),
        activity: Activity::Connecting,
    });
    let mut ops = connect()?;
    *connected_at = Some(std::time::Instant::now());
    let _ = events.send(Event::Connected {
        account: account.name.clone(),
        uidplus: ops.has_uidplus(),
        encrypted: ops.is_encrypted(),
    });
    let mut state = AccountSync::open(account, paths)?;
    let interval = Duration::from_secs(account.sync_interval_secs.max(10));
    let mut last_purge = 0i64;
    let mut full = true;
    let mut pass = true;
    let mut next = NextPass::None;
    // When the next full pass is due; the long IDLE's timeout is that pass, the short ones between index slices check.
    let mut next_full = now();
    let mut indexer = index::Session::default();

    while !shutdown.load(Ordering::Acquire) {
        if pass {
            next = state.pass(ops.as_mut(), full, carried, events, commands, shutdown)?;
            indexer.forget_selection();
            if full {
                next_full = now() + interval.as_secs() as i64;
            }
            if now() - last_purge > 3600 {
                match state.trash.purge(account.trash_retention_days, now()) {
                    Ok(0) => {}
                    Ok(removed) => log::info!("{}: purged {removed} trash files", account.name),
                    Err(e) => log::warn!("{}: trash purge failed: {e}", account.name),
                }
                last_purge = now();
            }
        }
        let drained = run_commands(
            ops.as_mut(),
            &state.store,
            &state.trash,
            account,
            &state.identity,
            &state.rules_path,
            commands,
            carried,
            events,
        )?;
        if drained.used_connection {
            indexer.forget_selection();
        }
        let wanted = drained.next.max(std::mem::take(&mut next));
        if wanted != NextPass::None {
            (full, pass) = (wanted == NextPass::Full, true);
            continue;
        }
        if account.index_bodies
            && indexer.slice(ops.as_mut(), &state.store, account, events)?
                == IndexOutcome::Progressed
        {
            // Mail that arrived while the backlog folder was selected, then a short wait so pushes and commands get
            // their turn before the next slice; the full pass still runs on its interval. STATUS leaves that folder
            // selected for the next slice, and is not allowed on the selected folder itself, so INBOX as backlog
            // folder is selected again, as it always was.
            let in_inbox = indexer.selected().is_none_or(|folder| folder == "INBOX");
            let count = state.store.message_count("INBOX")?;
            let arrived = if in_inbox {
                ops.select("INBOX")?.exists != count
            } else {
                ops.status("INBOX")?.messages != count
            };
            if arrived {
                indexer.forget_selection();
                (full, pass) = (false, true);
                continue;
            }
            (full, pass) = match ops.idle(index::INTER_SLICE_IDLE, wake)? {
                // The wait is in the backlog folder; INBOX is the next check's to find.
                IdleOutcome::NewMail if in_inbox => (false, true),
                IdleOutcome::NewMail | IdleOutcome::Timeout => {
                    let due = now() >= next_full;
                    (due, due)
                }
                IdleOutcome::Interrupted if shutdown.load(Ordering::Acquire) => return Ok(()),
                IdleOutcome::Interrupted => {
                    wake.swap(false, Ordering::AcqRel);
                    (full, false)
                }
            };
            continue;
        }
        let _ = events.send(Event::Activity {
            account: account.name.clone(),
            activity: Activity::Idle { since: now() },
        });
        ops.select("INBOX")?;
        indexer.forget_selection();
        let outcome = ops.idle(interval, wake)?;
        (full, pass) = match outcome {
            IdleOutcome::NewMail => (false, true),
            IdleOutcome::Timeout => (true, true),
            IdleOutcome::Interrupted if shutdown.load(Ordering::Acquire) => return Ok(()),
            // Woken for queued commands: run them, then wait again.
            IdleOutcome::Interrupted => {
                wake.swap(false, Ordering::AcqRel);
                (full, false)
            }
        };
    }
    Ok(())
}

#[cfg(test)]
mod tests;
