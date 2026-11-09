//! The composers: a reply or forward in the reader pane, new mail in windows of its own, sending through the outbox,
//! and taking a send back.
use postvak_core::daemon::client::LazyClient;
use postvak_core::message::header_value;
use postvak_core::send::build::bare_address;
use postvak_core::send::check::DEFAULT_SIZE_LIMIT;
use postvak_core::send::compose::{Prefilled, Queued, SendKind, SendRequest};
use postvak_core::send::drafts::{self, SavedDraft, unbracketed};
use postvak_core::send::mailto::Mailto;
use postvak_core::send::reply::{ForwardAs, ReplyTo};

use super::*;
use crate::compose::{
    COMPLETIONS, ComposeDraft, ComposeKind, Field, complete, files_dir, load_recoveries,
    merge_known, recovery_dir, remove_files, remove_recovery, remove_recovery_file,
    remove_unkept_files, save_recovery, sweep_files, typed_part,
};

pub(crate) type PrefillFn = fn(&Paths, &Config, &str, &SendKind) -> Result<Prefilled, String>;
pub(crate) type QueueFn = fn(&Paths, &Config, SendRequest) -> Result<Queued, QueueError>;

/// Why a message was not queued: its original is gone from the store (core's `OriginalGone`), or anything else, in
/// words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum QueueError {
    OriginalGone(String),
    Failed(String),
}

impl QueueError {
    fn into_text(self) -> String {
        match self {
            QueueError::OriginalGone(text) | QueueError::Failed(text) => text,
        }
    }
}

impl From<String> for QueueError {
    fn from(text: String) -> Self {
        QueueError::Failed(text)
    }
}

impl From<&str> for QueueError {
    fn from(text: &str) -> Self {
        QueueError::Failed(text.to_string())
    }
}
/// Saves `request` built as a draft in `account`'s Drafts, or with None only deletes, in place of the copies `replaces`.
pub(crate) type SaveDraftFn = fn(
    &Paths,
    &Config,
    &str,
    Option<(SendRequest, String)>,
    Vec<String>,
) -> Result<SavedDraft, String>;
/// Saves the message `raw` as it is in `account`'s Drafts, or with None only deletes, in place of the copies `replaces`.
pub(crate) type SaveRawFn =
    fn(&Paths, &str, Option<Vec<u8>>, Vec<String>) -> Result<SavedDraft, String>;
pub(crate) type CancelFn = fn(&Paths, &str, &str) -> Result<(), CancelError>;
pub(crate) type RetryFn = fn(&Paths, &str, &str) -> Result<(), String>;

/// The address field completions are for: the composer's place and id, and which of To, Cc and Bcc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CompletionFor {
    pub target: ComposeTarget,
    pub id: u64,
    pub field: ComposeField,
}

/// The addresses offered under a field, as `Name <address>`, and the highlighted one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Completion {
    pub to: CompletionFor,
    pub choices: Vec<String>,
    pub chosen: usize,
}

/// A completion query: its number, the accounts whose stores it reads, and the address typed so far.
type CompleteJob = (u64, Vec<String>, String);
pub(crate) type Completer = worker::Worker<CompleteJob, (u64, Vec<String>)>;

/// The thread that reads the stores for completions, keeping each store open between keystrokes; it skips a query a
/// later keystroke overtook.
fn completer(paths: Paths, wake: Wake) -> Completer {
    let mut stores: HashMap<String, Store> = HashMap::new();
    worker::Worker::start_newest("compose-complete", wake, move |job: CompleteJob| {
        let (number, accounts, prefix) = job;
        let mut found = Vec::new();
        for name in accounts {
            let store = match stores.entry(name) {
                std::collections::hash_map::Entry::Occupied(open) => open.into_mut(),
                std::collections::hash_map::Entry::Vacant(closed) => {
                    match Store::open_account(&paths, closed.key()) {
                        Ok(store) => closed.insert(store),
                        Err(e) => {
                            log::warn!(
                                "[{}] could not open the store to complete: {e}",
                                closed.key()
                            );
                            continue;
                        }
                    }
                }
            };
            match store.addresses(&prefix, COMPLETIONS) {
                Ok(known) => found.extend(known),
                Err(e) => log::warn!("address completion failed: {e}"),
            }
        }
        (number, merge_known(found))
    })
}

/// Sends failed outbox message `id` of `account` again.
pub(crate) fn retry_send(paths: &Paths, account: &str, id: &str) -> Result<(), String> {
    LazyClient::new(paths)
        .client()
        .and_then(|client| client.retry_send(account, id))
        .map_err(|e| error_text(&e))
}

/// Why a sent message could not be taken back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CancelError {
    /// It is being handed to the server, or already was.
    TooLate,
    Failed(String),
}

/// Where a composer is: the reader pane, or a new-mail window by its composer's id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ComposeTarget {
    Pane,
    Window(u64),
}

/// A message on its way to the outbox: the composer as it was sent, and the answer with the undo delay.
pub(crate) struct Sending {
    target: ComposeTarget,
    composer: Composer,
    answer: Receiver<Result<(Queued, u64), QueueError>>,
}

/// A composer on its way to Drafts: the composer as saved, the worker's answer, and what waits for it: another save
/// asked meanwhile, and what Save draft on the leave question goes on with.
pub(crate) struct ComposeSave {
    target: ComposeTarget,
    composer: Composer,
    answer: Receiver<Result<SavedDraft, String>>,
    again: bool,
    then: Option<UiAction>,
}

fn error_text(e: &anyhow::Error) -> String {
    clean(&format!("{e:#}"), false)
}

/// What a reply or forward opens with, through a daemon client of its own.
pub(crate) fn prefill(
    paths: &Paths,
    config: &Config,
    account: &str,
    kind: &SendKind,
) -> Result<Prefilled, String> {
    let client = LazyClient::new(paths);
    postvak_core::send::compose::prefill(config, paths, &client, Some(account), kind)
        .map_err(|e| error_text(&e))
}

/// Builds `request` as a draft with the Message-ID given, as `queue_send` would build the message, and has the daemon
/// save it; waits for the answer to this request alone, so no other command's refusal is taken for it.
pub(crate) fn save_draft(
    paths: &Paths,
    config: &Config,
    account: &str,
    request: Option<(SendRequest, String)>,
    replaces: Vec<String>,
) -> Result<SavedDraft, String> {
    let client = LazyClient::new(paths);
    let raw = request
        .map(|(request, message_id)| {
            drafts::build(config, paths, &client, &request)
                .map(|raw| drafts::with_message_id(&raw, &message_id))
        })
        .transpose()
        .map_err(|e| error_text(&e))?;
    ask_save(&client, account, raw, replaces)
}

/// Has the daemon save `raw` as it is, as `save_draft` does a built one.
pub(crate) fn save_raw_draft(
    paths: &Paths,
    account: &str,
    raw: Option<Vec<u8>>,
    replaces: Vec<String>,
) -> Result<SavedDraft, String> {
    ask_save(&LazyClient::new(paths), account, raw, replaces)
}

fn ask_save(
    client: &LazyClient,
    account: &str,
    raw: Option<Vec<u8>>,
    replaces: Vec<String>,
) -> Result<SavedDraft, String> {
    let command = Command::SaveDraft { raw, replaces };
    draft_answer(
        client
            .client()
            .and_then(|client| client.request(account, command)),
    )
}

/// The daemon's answer to a save, or why it failed; an unexpected answer is named by its kind alone, as it may hold mail.
fn draft_answer(answer: anyhow::Result<Event>) -> Result<SavedDraft, String> {
    match answer {
        Ok(Event::DraftSaved {
            message_id, left, ..
        }) => Ok(SavedDraft { message_id, left }),
        Ok(Event::CommandFailed { message, .. }) => Err(clean(&message, false)),
        Ok(other) => {
            let kind: String = format!("{other:?}")
                .chars()
                .take_while(char::is_ascii_alphanumeric)
                .collect();
            Err(format!("unexpected answer from the daemon: {kind}"))
        }
        Err(e) => Err(error_text(&e)),
    }
}

/// A draft being read on a worker: its Message-ID without brackets, and the composer it opens in.
pub(crate) type OpeningDraft = (String, Receiver<Result<Composer, String>>);

/// Draft `folder`/`uid` of `account` to open as composer `id`, with every account's addresses for From.
pub(crate) struct DraftToOpen {
    pub account: String,
    pub folder: String,
    pub uid: u32,
    pub id: u64,
    pub identities: Vec<(String, Identity)>,
}

/// Reads the stored draft into composer `draft.id`, its files written beside the composer's recovery file. The message
/// it answers is found by Message-ID in the account's store, its raw bytes from there or fetched through the daemon.
pub(crate) fn open_draft(paths: &Paths, draft: DraftToOpen) -> Result<Composer, String> {
    let DraftToOpen {
        account,
        folder,
        uid,
        id,
        identities,
    } = draft;
    let store = Store::open_account(paths, &account).map_err(|e| e.to_string())?;
    let raw = (store.raw(&folder, uid).map_err(|e| e.to_string())?)
        .ok_or("the draft is not downloaded yet")?;
    compose_from_raw(paths, &store, &account, &raw, id, &identities)
}

/// Reads draft `raw` of `account` into composer `id`, as `open_draft` does a stored one.
fn compose_from_raw(
    paths: &Paths,
    store: &Store,
    account: &str,
    raw: &[u8],
    id: u64,
    identities: &[(String, Identity)],
) -> Result<Composer, String> {
    let client = LazyClient::new(paths);
    let lookup = |id: &str| store.latest_by_message_id(id).ok().flatten();
    let find = |id: &str| match client.raw_message(account, store, &lookup(id)?) {
        Ok(raw) => Some(raw),
        Err(e) => {
            log::warn!("[{account}] could not read the message a draft answers: {e:#}");
            None
        }
    };
    let opened = postvak_core::send::drafts::read(raw, find).map_err(|e| error_text(&e))?;
    let answered = (opened.original.as_ref())
        .or(opened.shown_original.as_ref())
        .and_then(|raw| postvak_core::message::parse_headers(raw).message_id)
        .and_then(|id| lookup(&id))
        .map(|message| (message.folder, message.uid));
    let dir = recovery_dir(paths);
    Composer::from_draft(
        id,
        account,
        identities,
        opened,
        answered,
        &files_dir(&dir, id),
    )
    .map_err(|e| {
        remove_recovery(&dir, id);
        format!("could not keep the draft's files: {e}")
    })
}

/// A delete-only save that left a copy in Drafts did not delete it.
fn all_deleted(saved: SavedDraft) -> Result<(), String> {
    match saved.left.is_empty() {
        true => Ok(()),
        false => Err("a copy is still in Drafts".into()),
    }
}

/// Failed outbox message `outbox_id` of `account` to edit as composer `id`, with every account's addresses for From.
pub(crate) struct OutboxToEdit {
    pub account: String,
    pub outbox_id: String,
    pub id: u64,
    pub identities: Vec<(String, Identity)>,
}

/// A failed outbox message being turned into a draft on a worker, by account index and outbox id; the answer is the
/// composer the draft opens in (or why it did not), or why the message stayed in the outbox.
pub(crate) type EditingOutbox = (
    (usize, String),
    Receiver<Result<Result<Composer, String>, String>>,
);

/// Saves failed outbox message `edit.outbox_id` as a draft in place of its drafts, and only then takes it out of the
/// outbox; a refused take-out deletes the draft again, so the message is in exactly one of them. The draft opens from
/// the bytes saved.
pub(crate) fn edit_outbox(
    paths: &Paths,
    edit: OutboxToEdit,
    save: SaveRawFn,
    cancel: CancelFn,
) -> Result<Result<Composer, String>, String> {
    use postvak_core::send::outbox::{Outbox, State};
    let OutboxToEdit {
        account,
        outbox_id,
        id,
        identities,
    } = edit;
    let not_saved = |why: String| format!("Not saved as a draft: {why}");
    let outbox = Outbox::open(paths, &account).map_err(|e| not_saved(e.to_string()))?;
    let entry = (outbox.get(&outbox_id)).map_err(|e| not_saved(e.to_string()))?;
    if !matches!(
        entry.state,
        State::Failed {
            retrying: false,
            ..
        }
    ) {
        return Err(not_saved("it is not failed any more".into()));
    }
    let raw = (outbox.raw(&outbox_id)).map_err(|e| not_saved(e.to_string()))?;
    let message_id = postvak_core::send::build::message_id(&entry.envelope.from);
    let raw = drafts::with_message_id(&raw, &message_id);
    let saved = save(paths, &account, Some(raw.clone()), entry.draft).map_err(not_saved)?;
    if let Err(refused) = cancel(paths, &account, &outbox_id) {
        let mut why = match refused {
            CancelError::TooLate => "It is on its way already".to_string(),
            CancelError::Failed(why) => format!("Not taken out of the outbox: {why}"),
        };
        let copy = saved.message_id.into_iter().collect();
        if let Err(e) = save(paths, &account, None, copy).and_then(all_deleted) {
            log::warn!("[{account}] the draft of outbox message {outbox_id} was not deleted: {e}");
            why.push_str(", and the draft copy could not be removed; it is in Drafts");
        }
        return Err(why);
    }
    let opened = Store::open_account(paths, &account)
        .map_err(|e| e.to_string())
        .and_then(|store| compose_from_raw(paths, &store, &account, &raw, id, &identities))
        .map(|mut composer| {
            if let Some(draft) = &mut composer.draft {
                draft.left = saved.left;
            }
            composer
        });
    Ok(opened)
}

/// Builds, checks and queues `request` as `postvak send` does; the window never opens an editor.
pub(crate) fn queue_send(
    paths: &Paths,
    config: &Config,
    request: SendRequest,
) -> Result<Queued, QueueError> {
    let client = LazyClient::new(paths);
    let no_editor = |_: &str| Err(anyhow::anyhow!("the window has no editor"));
    postvak_core::send::compose::compose(config, paths, &client, request, no_editor)
        .map_err(|e| queue_error(&e))
}

fn queue_error(e: &anyhow::Error) -> QueueError {
    match e.downcast_ref::<postvak_core::send::compose::OriginalGone>() {
        Some(_) => QueueError::OriginalGone(error_text(e)),
        None => QueueError::Failed(error_text(e)),
    }
}

/// Takes outbox message `id` of `account` back before it goes.
pub(crate) fn cancel_send(paths: &Paths, account: &str, id: &str) -> Result<(), CancelError> {
    LazyClient::new(paths)
        .client()
        .and_then(|client| client.cancel_send(account, id))
        .map(drop)
        .map_err(|e| cancel_error(&format!("{e:#}")))
}

/// The daemon's refusal reaches the client as text only: the outbox's `TooLate` and `NotFound` both mean it went.
fn cancel_error(text: &str) -> CancelError {
    // Gone from the outbox means it went too: a sent entry is removed after a while.
    if text.contains("too late to cancel")
        || (text.starts_with("no message ") && text.ends_with(" in the outbox"))
    {
        CancelError::TooLate
    } else {
        CancelError::Failed(clean(text, false))
    }
}

/// Why a composer cannot go on: its account's addresses could not be read from config.toml.
const NO_IDENTITY: &str = "this account's addresses could not be read from config.toml";
const STOPPED: &str = "the window's worker stopped before it answered";

/// The line a composer reopened from its recovery file shows.
pub(crate) const RESTORED: &str = "Restored after Postvak closed";
/// The line of a composer restored from a file written while it was being sent.
pub(crate) const MAY_HAVE_GONE: &str =
    "This may have gone before Postvak closed: check the outbox or Sent";
/// What the first Send of such a composer asks; the next sends it.
pub(crate) const SEND_AGAIN: &str = "Send it again? Send once more to send it.";
/// The line of a reply whose original went while it was written, now new mail in a window of its own.
pub(crate) const GONE_AS_NEW: &str =
    "The message you answered is gone, so this goes as new mail, still answering it.";
/// Why a reply or forward draft did not open: the reader pane holds another composer.
pub(crate) const CLOSE_FIRST: &str = "Close the open message first to edit this draft.";
/// Seconds between writes of the changed composers' recovery files.
const RECOVERY_EVERY: f64 = 5.0;

fn load_config(paths: &Paths) -> Result<Config, String> {
    Config::load(&paths.config_file()).map_err(|e| format!("could not read config.toml: {e}"))
}

impl App {
    /// Writes the recovery file of each changed composer that changed since its last one, and takes away the file of
    /// one changed back; quitting calls it once more, without asking anything.
    pub(crate) fn save_recoveries(&mut self) {
        let open: Vec<Composer> = self
            .composer
            .iter()
            .chain(&self.new_mail)
            .cloned()
            .collect();
        for composer in open {
            self.keep_recovery(composer);
        }
    }

    /// Writes `composer`'s recovery file when it changed since the last one, or takes it away once it is unchanged.
    fn keep_recovery(&mut self, composer: Composer) {
        let dir = recovery_dir(&self.paths);
        let written = (self.recovery_written.iter()).position(|w| w.id == composer.id);
        match written {
            Some(index) if !composer.changed() && !composer.saving => {
                // The open composer still lists its draft files.
                remove_recovery_file(&dir, composer.id);
                self.recovery_written.remove(index);
            }
            Some(index) if self.recovery_written[index] == composer => {}
            // A save on its way has named a copy only this file remembers after a crash.
            _ if !composer.changed() && !composer.saving => {}
            _ => match save_recovery(&dir, &composer) {
                Ok(()) => {
                    self.recovery_written.retain(|w| w.id != composer.id);
                    self.recovery_written.push(composer);
                }
                Err(e) => log::warn!(
                    "could not keep the unsent message {}.json: {e}",
                    composer.id
                ),
            },
        }
    }

    pub(super) fn keep_recoveries(&mut self, now: f64) {
        if now >= self.recovery_due {
            self.recovery_due = now + RECOVERY_EVERY;
            self.save_recoveries();
        }
    }

    /// Reopens each composer a crash or quit left, saying so: a reply or forward in the reader pane over its original,
    /// new mail in a window; one whose original is gone from the store goes on as new mail.
    pub(super) fn restore_composers(&mut self) {
        let mut kept: u64 = 0;
        sweep_files(&recovery_dir(&self.paths));
        for mut composer in load_recoveries(&recovery_dir(&self.paths)) {
            self.next_compose = self.next_compose.max(composer.id + 1);
            if composer.original.is_some() && !self.original_stored(&composer) {
                composer.without_original();
            }
            if composer.original.is_some() && self.composer.is_some() {
                // shortcut: a second reply or forward waits on disk for a later start, as the pane holds one, and the
                // notice says so; open it once the pane is free if that turns out to happen.
                kept += 1;
                continue;
            }
            composer.may_have_gone |= std::mem::take(&mut composer.sending);
            composer.error = Some(match composer.may_have_gone {
                true => (Field::Send, MAY_HAVE_GONE.into()),
                false => (Field::Note, RESTORED.into()),
            });
            self.recovery_written.push(composer.clone());
            self.place_composer(composer);
        }
        if kept > 0 {
            let messages =
                crate::numbers::counted(kept, "more unsent message", "more unsent messages");
            self.notice(format!("{messages} kept for a later start"));
        }
    }

    /// Puts `composer` back: one over a message, a reply or forward or a draft answering it, in the reader pane over
    /// that message, which the reader opens; new mail, or one whose original is gone from the store, in a window as
    /// new mail.
    fn place_composer(&mut self, mut composer: Composer) {
        if composer.original.is_some() && !self.original_stored(&composer) {
            composer.without_original();
        }
        let target = match &composer.original {
            Some(original) => {
                self.open_message(postvak_core::notify::Open {
                    account: Some(composer.account.clone()),
                    folder: original.folder.clone(),
                    uid: original.uid,
                });
                self.composer = Some(composer);
                ComposeTarget::Pane
            }
            _ => {
                let target = ComposeTarget::Window(composer.id);
                self.new_mail.push(composer);
                target
            }
        };
        self.check_size(target);
    }

    /// Moves the pane's reply, whose original is gone, to a window as new mail that still answers it.
    fn answer_as_new_mail(&mut self) {
        let Some(mut composer) = self.composer.take() else {
            return;
        };
        composer.without_original();
        composer.sending = false;
        composer.error = Some((Field::Note, GONE_AS_NEW.into()));
        let target = ComposeTarget::Window(composer.id);
        self.new_mail.push(composer);
        self.check_size(target);
    }

    fn original_stored(&self, composer: &Composer) -> bool {
        let Some(original) = &composer.original else {
            return false;
        };
        let account = self.accounts.iter().find(|a| a.name == composer.account);
        let store = account.and_then(|a| a.store.as_ref().ok());
        store.is_some_and(|store| {
            matches!(store.message(&original.folder, original.uid), Ok(Some(_)))
        })
    }

    fn identity_of(&self, account: &str) -> Option<Identity> {
        let account = self.accounts.iter().find(|a| a.name == account)?;
        account.identity.clone()
    }

    /// The largest message `account`'s server takes, as its last connection said, else the usual limit.
    fn size_limit(&self, account: &str) -> u64 {
        let account = self.accounts.iter().find(|a| a.name == account);
        account
            .and_then(|a| a.store.as_ref().ok()?.smtp_state().ok()??.size)
            .unwrap_or(DEFAULT_SIZE_LIMIT as u64)
    }

    /// Reads the limit of the composer's account, which From can change, and checks the size against it.
    fn check_size(&mut self, target: ComposeTarget) {
        let Some(account) = self.composer_ref(target).map(|c| c.account.clone()) else {
            return;
        };
        let limit = self.size_limit(&account);
        if let Some(composer) = self.composer_mut(target) {
            composer.size_limit = limit;
            composer.check_size();
        }
    }

    pub(crate) fn composer_ref(&self, target: ComposeTarget) -> Option<&Composer> {
        match target {
            ComposeTarget::Pane => self.composer.as_ref(),
            ComposeTarget::Window(id) => self.new_mail.iter().find(|c| c.id == id),
        }
    }

    fn composer_mut(&mut self, target: ComposeTarget) -> Option<&mut Composer> {
        match target {
            ComposeTarget::Pane => self.composer.as_mut(),
            ComposeTarget::Window(id) => self.new_mail.iter_mut().find(|c| c.id == id),
        }
    }

    /// Drops the composer at `target` and its recovery file; a window's question goes with it.
    fn drop_composer(&mut self, target: ComposeTarget) {
        if let Some(id) = self.composer_ref(target).map(|c| c.id) {
            remove_recovery(&recovery_dir(&self.paths), id);
            self.recovery_written.retain(|w| w.id != id);
        }
        self.forget_composer(target);
    }

    fn forget_composer(&mut self, target: ComposeTarget) {
        match target {
            ComposeTarget::Pane => self.composer = None,
            ComposeTarget::Window(id) => {
                self.new_mail.retain(|c| c.id != id);
                self.leave_new_mail.retain(|&(asking, _)| asking != id);
            }
        }
    }

    /// Opens new mail in a window of its own, on the account of the folder in view, else the first.
    pub(super) fn new_mail(&mut self, mailto: Option<Mailto>) {
        let Some(account) = self.accounts.get(self.view_account().unwrap_or(0)) else {
            return;
        };
        let account = account.name.clone();
        let identities: Vec<(String, Identity)> = (self.accounts.iter())
            .filter_map(|a| Some((a.name.clone(), a.identity.clone()?)))
            .collect();
        let id = self.next_compose;
        self.next_compose += 1;
        self.new_mail
            .push(Composer::new_mail(id, &account, &identities, mailto));
        self.check_size(ComposeTarget::Window(id));
    }

    /// Keeps the composer of a window that could not open in its recovery file, for the next start.
    pub(super) fn new_mail_failed(&mut self, id: u64, why: &str) {
        let target = ComposeTarget::Window(id);
        if let Some(composer) = self.composer_ref(target)
            && let Err(e) = save_recovery(&recovery_dir(&self.paths), composer)
        {
            log::warn!("could not keep the unsent message {id}.json: {e}");
        }
        self.forget_composer(target);
        self.note_error(None, format!("could not open a new-mail window: {why}"));
    }

    /// Closes new-mail window `id`, or discards it when `discarding`, asking first when it was changed; one being sent
    /// or saved stays.
    fn close_new_mail(&mut self, id: u64, discarding: bool) {
        let target = ComposeTarget::Window(id);
        let Some(composer) = self
            .composer_ref(target)
            .filter(|c| !c.sending && !c.saving)
        else {
            return;
        };
        if composer.leave_question(discarding).is_none() {
            self.drop_composer(target);
        } else if !self.leave_new_mail.iter().any(|(asking, _)| *asking == id) {
            self.leave_new_mail.push((id, discarding));
        }
    }

    /// What the composer at `target` asks before it goes, while it asks, and whether Save draft is an answer.
    pub(crate) fn leave_question(&self, target: ComposeTarget) -> Option<(&'static str, bool)> {
        let composer = self.composer_ref(target)?;
        let discarding = match target {
            ComposeTarget::Pane => {
                matches!(self.leave_compose.as_deref()?, UiAction::ComposeDiscard(_))
            }
            ComposeTarget::Window(id) => {
                self.leave_new_mail
                    .iter()
                    .find(|(asking, _)| *asking == id)?
                    .1
            }
        };
        let question = composer.leave_question(discarding)?;
        Some((question, !discarding))
    }

    /// Cancel on "Discard this message?": the composer stays.
    pub(super) fn leave_cancel(&mut self, target: ComposeTarget) {
        match target {
            ComposeTarget::Pane => self.leave_compose = None,
            ComposeTarget::Window(id) => self.leave_new_mail.retain(|&(asking, _)| asking != id),
        }
    }

    /// Discard on "Discard this message?": the composer goes, and what asked to leave the pane goes on. Discard itself
    /// deletes the composer's draft too; leaving another way keeps it.
    pub(super) fn leave_discard(&mut self, target: ComposeTarget) {
        // Its save's answer names the copy a discard deletes.
        if self.composer_ref(target).is_some_and(|c| c.saving) {
            return;
        }
        match target {
            ComposeTarget::Pane => {
                if let Some(leave) = self.leave_compose.take() {
                    if matches!(*leave, UiAction::ComposeDiscard(_)) {
                        self.delete_draft(target);
                    }
                    self.drop_composer(ComposeTarget::Pane);
                    self.apply_action(*leave);
                }
            }
            ComposeTarget::Window(id) => {
                let asking = self.leave_new_mail.iter().find(|(asking, _)| *asking == id);
                let Some(&(_, discarding)) = asking else {
                    return;
                };
                if discarding {
                    self.delete_draft(target);
                }
                self.drop_composer(target);
            }
        }
    }

    /// Save draft on the leave question: once saved, what asked to leave goes on; a window closes.
    pub(super) fn leave_save(&mut self, target: ComposeTarget) {
        if !self.composer_ref(target).is_some_and(Composer::can_save)
            || self.refuse_without_drafts(target)
        {
            return;
        }
        let then = match target {
            ComposeTarget::Pane => self.leave_compose.take().map(|leave| *leave),
            ComposeTarget::Window(id) => {
                let asking = self.leave_new_mail.iter().any(|(asking, _)| *asking == id);
                self.leave_cancel(target);
                asking.then_some(UiAction::ComposeClose(target))
            }
        };
        if then.is_some() {
            self.compose_save_draft(target, then);
        }
    }

    /// Says so on the composer at `target` when its account has no Drafts folder, before any copy is named.
    fn refuse_without_drafts(&mut self, target: ComposeTarget) -> bool {
        let Some(account) = self.composer_ref(target).map(|c| c.account.clone()) else {
            return false;
        };
        let index = self.accounts.iter().position(|a| a.name == account);
        let has_drafts = index.is_some_and(|index| {
            (self.accounts[index].folders.iter()).any(|f| self.is_drafts(index, &f.name))
        });
        if !has_drafts && let Some(composer) = self.composer_mut(target) {
            let why = postvak_core::send::drafts::SaveDraftError::NoDraftsFolder.to_string();
            composer.error = Some((Field::Send, why));
        }
        !has_drafts
    }

    /// Deletes every copy in Drafts of the composer at `target`, on a worker; a failure is noted.
    fn delete_draft(&mut self, target: ComposeTarget) {
        let Some(composer) = self.composer_ref(target) else {
            return;
        };
        let Some(draft) = &composer.draft else {
            return;
        };
        let replaces = draft.ids();
        let account = composer.account.clone();
        let (paths, save) = (self.paths.clone(), self.save_draft);
        let answer = worker::oneshot("compose-draft-delete", self.wake.clone(), move || {
            save(&paths, &load_config(&paths)?, &account, None, replaces).and_then(all_deleted)
        });
        self.compose_deleting.push(answer);
    }

    /// Builds and saves the composer's draft on a worker, in place of the copy saved before; a save asked for while one
    /// is on its way runs once that one is answered. `then` is applied once it is saved.
    fn compose_save_draft(&mut self, target: ComposeTarget, then: Option<UiAction>) {
        // A save under the leave question answers it, so no Discard waits on a save it cannot see.
        self.leave_cancel(target);
        if self.refuse_without_drafts(target) {
            return;
        }
        let Some(composer) = self.composer_mut(target).filter(|c| c.can_save()) else {
            return;
        };
        if composer.saving {
            let id = composer.id;
            let save = (self.compose_saves.iter_mut())
                .find(|save| save.target == target && save.composer.id == id);
            if let Some(save) = save {
                save.again = true;
                save.then = then.or(save.then.take());
            }
            return;
        }
        composer.saving = true;
        let saved = composer.clone();
        let request = composer.request();
        let replaces = (composer.draft.as_ref())
            .map(ComposeDraft::ids)
            .unwrap_or_default();
        // Known before the APPEND, so a save that fails, times out or dies with the window leaves no copy unnamed.
        let message_id = postvak_core::send::build::message_id(&bare_address(&composer.from));
        composer.may_hold(message_id.clone());
        let account = composer.account.clone();
        let pending = composer.clone();
        self.keep_recovery(pending);
        let (paths, save) = (self.paths.clone(), self.save_draft);
        let answer = worker::oneshot("compose-draft", self.wake.clone(), move || {
            save(
                &paths,
                &load_config(&paths)?,
                &account,
                Some((request, message_id)),
                replaces,
            )
        });
        self.compose_saves.push(ComposeSave {
            target,
            composer: saved,
            answer,
            again: false,
            then,
        });
    }

    /// Takes each answered save, and notes each delete that failed.
    fn receive_saves(&mut self) {
        let mut index = 0;
        while index < self.compose_saves.len() {
            match worker::poll(&self.compose_saves[index].answer) {
                Some(result) => {
                    let save = self.compose_saves.remove(index);
                    match result.unwrap_or_else(|Stopped| Err(STOPPED.into())) {
                        Ok(saved) => self.draft_saved(save, saved),
                        Err(why) => self.save_failed(save.target, save.composer.id, why),
                    }
                }
                None => index += 1,
            }
        }
        let mut index = 0;
        while index < self.compose_deleting.len() {
            match worker::poll(&self.compose_deleting[index]) {
                Some(result) => {
                    self.compose_deleting.remove(index);
                    if let Err(why) = result.unwrap_or_else(|Stopped| Err(STOPPED.into())) {
                        self.note_error(None, format!("could not delete the draft: {why}"));
                    }
                }
                None => index += 1,
            }
        }
    }

    fn save_failed(&mut self, target: ComposeTarget, id: u64, why: String) {
        if let Some(composer) = self.composer_mut(target).filter(|c| c.id == id) {
            composer.saving = false;
            composer.error = Some((Field::Send, clean(&why, false)));
        }
    }

    /// The composer that asked has its draft in Drafts now, and its recovery file says so; the next save, or what Save
    /// draft on the leave question goes on with, follows.
    fn draft_saved(&mut self, save: ComposeSave, saved: SavedDraft) {
        let ComposeSave {
            target,
            composer: asked,
            again,
            then,
            ..
        } = save;
        let Some(message_id) = saved.message_id else {
            let why = "the draft was saved without a Message-ID".to_string();
            return self.save_failed(target, asked.id, why);
        };
        let Some(composer) = self.composer_mut(target).filter(|c| c.id == asked.id) else {
            return;
        };
        let draft = ComposeDraft {
            message_id,
            left: saved.left,
        };
        composer.saved_as(&asked, draft);
        // A restored composer's warning stays until it is sent, as its first Send still asks.
        if !composer.may_have_gone && composer.error.as_ref().is_some_and(|e| e.0 == Field::Send) {
            composer.error = None;
        }
        let composer = composer.clone();
        self.keep_recovery(composer);
        self.toast = Some(Toast::new("Draft saved".into(), false));
        if again {
            self.compose_save_draft(target, then);
        } else if let Some(then) = then {
            self.apply_action(then);
        }
    }

    /// Opens the shown draft in a composer, read on a worker; one open already comes to the front. A draft not
    /// downloaded yet opens once it is.
    pub(super) fn edit_compose_draft(&mut self) {
        let Some(body) = &self.body else { return };
        if !self.is_drafts(body.account, &body.key.0) {
            return;
        }
        let message_id = (body.message.as_ref())
            .and_then(|m| m.message_id.as_deref())
            .map(|id| unbracketed(id).to_string())
            .unwrap_or_default();
        let holds = |c: &Composer| {
            !message_id.is_empty()
                && (c.draft.as_ref())
                    .is_some_and(|d| d.ids().iter().any(|id| unbracketed(id) == message_id))
        };
        if self.composer.as_ref().is_some_and(holds) {
            return;
        }
        if let Some(open) = self.new_mail.iter().find(|c| holds(c)) {
            self.raise_compose = Some(open.id);
            return;
        }
        if (self.compose_opening.as_ref()).is_some_and(|(opening, _)| *opening == message_id) {
            return;
        }
        let store = self.accounts[body.account].store.as_ref().ok();
        if !store.is_some_and(|s| matches!(s.has_raw(&body.key.0, body.key.1), Ok(true))) {
            if self.shown_on_demand() {
                self.download_whole(UiAction::EditComposeDraft);
            } else if let Some(body) = &mut self.body {
                body.after_download = Some(UiAction::EditComposeDraft);
            }
            return;
        }
        let draft = DraftToOpen {
            account: self.accounts[body.account].name.clone(),
            folder: body.key.0.clone(),
            uid: body.key.1,
            id: self.next_compose,
            identities: (self.accounts.iter())
                .filter_map(|a| Some((a.name.clone(), a.identity.clone()?)))
                .collect(),
        };
        self.next_compose += 1;
        let paths = self.paths.clone();
        let answer = worker::oneshot("compose-draft-open", self.wake.clone(), move || {
            open_draft(&paths, draft)
        });
        self.compose_opening = Some((message_id, answer));
    }

    /// Opens the draft read on the worker: over the message it answers in the reader pane, else in a window.
    fn receive_opened_draft(&mut self) {
        let Some((_, answer)) = &self.compose_opening else {
            return;
        };
        let Some(result) = worker::poll(answer) else {
            return;
        };
        self.compose_opening = None;
        self.place_opened_draft(result.unwrap_or_else(|Stopped| Err(STOPPED.into())));
    }

    /// Places a draft read on a worker: over the message it answers in the reader pane, else in a window.
    fn place_opened_draft(&mut self, opened: Result<Composer, String>) {
        let mut composer = match opened {
            Ok(composer) => composer,
            Err(why) => return self.note_error(None, format!("could not open the draft: {why}")),
        };
        // The pane holds one composer: one started while the draft was read keeps it.
        if self.composer.is_some() && composer.original.is_some() {
            if composer.kind != ComposeKind::New {
                remove_recovery(&recovery_dir(&self.paths), composer.id);
                return self.note_error(None, CLOSE_FIRST.into());
            }
            composer.without_original();
        }
        self.place_composer(composer);
    }

    /// Opens a reply (`Some`) or a forward of the shown message in the reader pane, loading while it is filled.
    pub(super) fn answer(&mut self, reply_to: Option<ReplyTo>) {
        let Some(body) = &self.body else { return };
        let headers = body.message.as_ref().map(|m| m.headers.as_slice());
        let has_list = headers.is_some_and(|h| header_value(h, "List-Post").is_some());
        if reply_to == Some(ReplyTo::List) && !has_list {
            return;
        }
        let account = self.accounts[body.account].name.clone();
        let (folder, uid) = body.key.clone();
        let id = self.next_compose;
        self.next_compose += 1;
        let (composer, kind) = match reply_to {
            Some(to) => (
                Composer::reply(id, &account, &folder, uid, to),
                SendKind::Reply { folder, uid, to },
            ),
            None => (
                Composer::forward(id, &account, &folder, uid),
                SendKind::Forward {
                    folder,
                    uid,
                    as_: ForwardAs::Inline,
                    to: Vec::new(),
                    cc: Vec::new(),
                    bcc: Vec::new(),
                },
            ),
        };
        let (paths, prefill) = (self.paths.clone(), self.prefill);
        let answer = worker::oneshot("compose-prefill", self.wake.clone(), move || {
            prefill(&paths, &load_config(&paths)?, &account, &kind)
        });
        self.compose_prefill = Some((id, answer));
        self.composer = Some(composer);
        self.check_size(ComposeTarget::Pane);
    }

    /// The composer's own actions: its fields, Send, Close and Discard.
    pub(super) fn compose(&mut self, action: UiAction) {
        match action {
            UiAction::ComposeCompletionStep(target, by) => return self.step_completion(target, by),
            UiAction::ComposeCompletionPick(target, index) => {
                return self.pick_completion(target, index);
            }
            UiAction::ComposeCompletionClose(target) => {
                if self.completion_target() == Some(target) {
                    self.end_completion();
                }
                return;
            }
            UiAction::ComposeSend(target) => {
                self.end_completion();
                return self.compose_send(target);
            }
            UiAction::ComposeAttach(target) => return self.compose_attach(target),
            UiAction::ComposeSaveDraft(target) => {
                self.end_completion();
                return self.compose_save_draft(target, None);
            }
            // The pane's composer was asked about before this, in `holds_for_compose`.
            UiAction::ComposeClose(target) | UiAction::ComposeDiscard(target) => {
                let discarding = matches!(action, UiAction::ComposeDiscard(_));
                match target {
                    ComposeTarget::Pane => self.drop_composer(target),
                    ComposeTarget::Window(id) => self.close_new_mail(id, discarding),
                }
                return;
            }
            _ => {}
        }
        let (UiAction::ComposeEdit(target, ..)
        | UiAction::ComposeShowCcBcc(target)
        | UiAction::ComposeRemoveFile(target, _)
        | UiAction::ComposeToggleForwardPart(target, _)
        | UiAction::ComposeForwardAsAttachment(target, _)) = action
        else {
            return;
        };
        // Only these change what is attached or the account whose limit counts.
        let resized =
            !matches!(action, UiAction::ComposeEdit(_, field, _) if field != ComposeField::From);
        let edited = match &action {
            UiAction::ComposeEdit(_, field, _) => Some(*field),
            _ => None,
        };
        let Some(composer) = self.composer_mut(target) else {
            return;
        };
        match action {
            UiAction::ComposeEdit(_, field, value) => composer.set(field, value),
            UiAction::ComposeShowCcBcc(_) => composer.show_cc_bcc = true,
            UiAction::ComposeRemoveFile(_, index) if index < composer.files.len() => {
                composer.files.remove(index);
            }
            UiAction::ComposeToggleForwardPart(_, index) => {
                if let Some(part) = composer.forward_parts.get_mut(index) {
                    part.2 = !part.2;
                }
            }
            UiAction::ComposeForwardAsAttachment(_, on) => composer.forward_as_attachment = on,
            _ => {}
        }
        if resized {
            self.check_size(target);
        }
        if let Some(field) = edited {
            self.ask_completion(target, field);
        }
    }

    /// Asks every account's store for the address being typed in `field`; any field but an address ends completion.
    fn ask_completion(&mut self, target: ComposeTarget, field: ComposeField) {
        let Some(composer) = self.composer_ref(target) else {
            return;
        };
        let text = match field {
            ComposeField::To => &composer.to,
            ComposeField::Cc => &composer.cc,
            ComposeField::Bcc => &composer.bcc,
            _ => return self.end_completion(),
        };
        let prefix = typed_part(text).1.to_string();
        let to = CompletionFor {
            target,
            id: composer.id,
            field,
        };
        if prefix.is_empty() {
            return self.end_completion();
        }
        self.next_completion += 1;
        let number = self.next_completion;
        let accounts = self.accounts.iter().map(|a| a.name.clone()).collect();
        let completer = (self.completer)
            .get_or_insert_with(|| completer(self.paths.clone(), self.wake.clone()));
        completer.send((number, accounts, prefix));
        self.completing = Some((number, to));
    }

    fn completion_target(&self) -> Option<ComposeTarget> {
        let shown = self.completion.as_ref().map(|c| c.to.target);
        shown.or(self.completing.map(|(_, to)| to.target))
    }

    /// The completions go, and an answer still on its way is dropped.
    fn end_completion(&mut self) {
        self.completion = None;
        self.completing = None;
    }

    /// Takes the answer to the newest query only; one that a later keystroke overtook is dropped.
    fn receive_completion(&mut self) {
        let Some(completer) = &self.completer else {
            return;
        };
        for (number, choices) in completer.done() {
            let Some((_, to)) = self.completing.filter(|(asked, _)| *asked == number) else {
                continue;
            };
            self.completing = None;
            let open = (self.composer_ref(to.target)).is_some_and(|composer| composer.id == to.id);
            self.completion = (open && !choices.is_empty()).then_some(Completion {
                to,
                choices,
                chosen: 0,
            });
        }
    }

    fn step_completion(&mut self, target: ComposeTarget, by: isize) {
        if let Some(completion) = self.completion.as_mut().filter(|c| c.to.target == target) {
            let last = completion.choices.len().saturating_sub(1);
            completion.chosen = completion.chosen.saturating_add_signed(by).min(last);
        }
    }

    /// Puts completion `index` in place of the address being typed, with a comma after it for the next.
    fn pick_completion(&mut self, target: ComposeTarget, index: usize) {
        let Some(completion) = self.completion.take_if(|c| c.to.target == target) else {
            return;
        };
        self.completing = None;
        let Some(choice) = completion.choices.get(index) else {
            return;
        };
        let to = completion.to;
        let Some(composer) = self.composer_mut(target).filter(|c| c.id == to.id) else {
            return;
        };
        let text = match to.field {
            ComposeField::Cc => &composer.cc,
            ComposeField::Bcc => &composer.bcc,
            _ => &composer.to,
        };
        let completed = complete(text, choice);
        composer.set(to.field, completed);
    }

    /// Asks the daemon on a worker to send failed outbox message `id` of `account` again.
    pub(super) fn retry_outbox(&mut self, account: usize, id: String) {
        let Some(name) = self.accounts.get(account).map(|a| a.name.clone()) else {
            return;
        };
        self.outbox_refused.remove(&(account, id.clone()));
        self.retried.push((account, id.clone()));
        let (paths, retry) = (self.paths.clone(), self.retry_send);
        let answer = worker::oneshot("compose-retry", self.wake.clone(), move || {
            retry(&paths, &name, &id)
        });
        self.retrying.push(answer);
    }

    /// Sends the message the toast counts down for at once, rather than when its undo delay ends.
    pub(super) fn send_now(&mut self) {
        let Some(words) = self
            .toast
            .as_ref()
            .and_then(|toast| toast.count_down.clone())
        else {
            return;
        };
        let Some((account, id)) = self.taken.last().and_then(|taken| taken.outbox.clone()) else {
            return;
        };
        self.retry_outbox(account, id);
        self.toast = Some(Toast::new(words, false));
    }

    /// Turns failed outbox message `id` of `account` into a draft on a worker, and opens it once saved.
    pub(super) fn edit_outbox(&mut self, account: usize, id: String) {
        let Some(name) = self.accounts.get(account).map(|a| a.name.clone()) else {
            return;
        };
        let key = (account, id.clone());
        if self
            .outbox_editing
            .iter()
            .any(|(editing, _)| *editing == key)
        {
            return;
        }
        self.outbox_refused.remove(&key);
        let edit = OutboxToEdit {
            account: name,
            outbox_id: id,
            id: self.next_compose,
            identities: (self.accounts.iter())
                .filter_map(|a| Some((a.name.clone(), a.identity.clone()?)))
                .collect(),
        };
        self.next_compose += 1;
        let (paths, save, cancel) = (self.paths.clone(), self.save_raw_draft, self.cancel_send);
        let answer = worker::oneshot("compose-outbox-edit", self.wake.clone(), move || {
            edit_outbox(&paths, edit, save, cancel)
        });
        self.outbox_editing.push((key, answer));
    }

    /// An edited outbox message opens as its draft; one left in the outbox says why in the outbox dialog, until its state
    /// changes.
    fn receive_outbox_edits(&mut self) {
        let mut index = 0;
        while index < self.outbox_editing.len() {
            let Some(result) = worker::poll(&self.outbox_editing[index].1) else {
                index += 1;
                continue;
            };
            let (key, _) = self.outbox_editing.remove(index);
            self.read_outboxes();
            match result.unwrap_or_else(|Stopped| Err(STOPPED.into())) {
                Ok(opened) => self.place_opened_draft(opened),
                Err(why) => {
                    let shown = self
                        .outbox
                        .iter()
                        .find(|(a, e)| (*a, &e.id) == (key.0, &key.1));
                    if let Some((_, entry)) = shown {
                        let state = entry.state.clone();
                        self.outbox_refused.insert(key, (state, clean(&why, false)));
                    }
                }
            }
        }
    }

    /// A retry the daemon took shows at once in the outbox; one it refused says why.
    fn receive_retries(&mut self) {
        let mut index = 0;
        while index < self.retrying.len() {
            match worker::poll(&self.retrying[index]) {
                Some(result) => {
                    self.retrying.remove(index);
                    match result.unwrap_or_else(|Stopped| Err(STOPPED.into())) {
                        Ok(()) => self.read_outboxes(),
                        Err(why) => self.note_error(None, format!("could not retry: {why}")),
                    }
                }
                None => index += 1,
            }
        }
    }

    /// Opens the desktop's open dialog for the composer at `target`; one at a time, and not while it is sent.
    fn compose_attach(&mut self, target: ComposeTarget) {
        if self.compose_attaching.is_some() {
            return;
        }
        let Some(id) = self
            .composer_ref(target)
            .filter(|c| !c.sending)
            .map(|c| c.id)
        else {
            return;
        };
        let answer = (self.ask_open_paths)(self.wake.clone());
        self.compose_attaching = Some((target, id, answer));
    }

    /// Adds the files the dialog chose to the composer that asked, if it is still open; a cancelled dialog adds none,
    /// and a file that could not be read is left out and named.
    fn receive_attach(&mut self) {
        let Some((target, id, answer)) = &self.compose_attaching else {
            return;
        };
        let Some(result) = worker::poll(answer) else {
            return;
        };
        let (target, id) = (*target, *id);
        self.compose_attaching = None;
        let Some(composer) = self.composer_mut(target).filter(|c| c.id == id) else {
            return;
        };
        let chosen = match result.unwrap_or_else(|Stopped| Err(anyhow::anyhow!(STOPPED))) {
            Ok(chosen) => chosen,
            Err(e) => {
                let why = format!("Could not open the file dialog: {}", error_text(&e));
                composer.error = Some((Field::Attachments, why));
                return;
            }
        };
        let mut unreadable = None;
        for (path, size) in chosen {
            match size {
                Some(size) => composer.files.push((path, size)),
                None => unreadable = unreadable.or(Some(path)),
            }
        }
        self.check_size(target);
        if let (Some(path), Some(composer)) = (unreadable, self.composer_mut(target)) {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            composer.error = Some((Field::Attachments, format!("{name} can't be read.")));
        }
    }

    /// Checks the composer and queues its message on a worker; ignored while it loads or a send is in flight.
    fn compose_send(&mut self, target: ComposeTarget) {
        let identity = self
            .composer_ref(target)
            .and_then(|composer| self.identity_of(&composer.account));
        let Some(composer) = self
            .composer_mut(target)
            .filter(|composer| !composer.loading && !composer.sending && !composer.saving)
        else {
            return;
        };
        let Some(identity) = identity else {
            composer.error = Some((Field::From, NO_IDENTITY.into()));
            return;
        };
        if let Err(error) = composer.check(&identity, composer.size_limit) {
            composer.error = Some(error);
            return;
        }
        if std::mem::take(&mut composer.may_have_gone) {
            composer.error = Some((Field::Send, SEND_AGAIN.into()));
            return;
        }
        composer.error = None;
        composer.sending = true;
        let sent = composer.clone();
        let request = composer.request();
        let (paths, queue) = (self.paths.clone(), self.queue_send);
        let answer = worker::oneshot("compose-send", self.wake.clone(), move || {
            let config = load_config(&paths)?;
            queue(&paths, &config, request).map(|queued| (queued, config.sending.undo_secs))
        });
        self.compose_sending.push(Sending {
            target,
            composer: sent,
            answer,
        });
        // A "Discard this message?" asked before Send no longer holds: the message is on its way.
        self.leave_cancel(target);
    }

    /// Whether `action` waits for "Discard this message?" about the pane's changed composer; an unchanged one closes.
    pub(super) fn holds_for_compose(&mut self, action: &UiAction) -> bool {
        let Some(composer) = &self.composer else {
            return false;
        };
        if !self.leaves_pane(action, composer) {
            return false;
        }
        // Until the queue or Drafts answers, the composer is what comes back on a refusal.
        if composer.sending || composer.saving {
            return true;
        }
        let discarding = matches!(action, UiAction::ComposeDiscard(_));
        if composer.leave_question(discarding).is_some() {
            self.leave_compose = Some(Box::new(action.clone()));
            return true;
        }
        self.drop_composer(ComposeTarget::Pane);
        false
    }

    /// Whether `action` takes the reader pane from `composer`: it changes the message shown below away from the
    /// original (the cursor, the list's rows, or the original itself), or opens another composer in its place.
    fn leaves_pane(&self, action: &UiAction, composer: &Composer) -> bool {
        let is_original = |account: usize, folder: &str, uid: u32| {
            self.accounts[account].name == composer.account
                && composer
                    .original
                    .as_ref()
                    .is_some_and(|o| o.folder == folder && o.uid == uid)
        };
        let row_is_original = |index: usize| {
            (self.list.rows.get(index))
                .is_some_and(|row| is_original(row.account, &row.folder, row.uid))
        };
        let hides_original = |action: &Action| {
            Optimistic::of(action) == Some(Optimistic::Hidden)
                && (self.targets().iter()).any(|(account, folder, uids)| {
                    uids.iter().any(|&uid| is_original(*account, folder, uid))
                })
        };
        match action {
            UiAction::SelectRow(index)
            | UiAction::ExtendSelection(index)
            | UiAction::TargetRow(index) => !row_is_original(*index),
            UiAction::SelectView(view) => *view != self.view,
            UiAction::SetFilter(filter) => *filter != self.filter,
            UiAction::Act(action) => hides_original(action),
            UiAction::MoveTo(folder) => hides_original(&Action::Move(folder.clone())),
            UiAction::DropOn(_) => hides_original(&Action::Move(String::new())),
            UiAction::ComposeClose(target) | UiAction::ComposeDiscard(target) => {
                *target == ComposeTarget::Pane
            }
            UiAction::MoveCursor(_)
            | UiAction::StepFolder(_)
            | UiAction::OpenMessage(_)
            | UiAction::SearchFor(_)
            | UiAction::SearchAccepted
            | UiAction::StartSearch
            | UiAction::Reply(_)
            | UiAction::Forward => true,
            UiAction::ReopenCompose(id) => (self.taken.iter().find(|taken| taken.id == *id))
                .and_then(|taken| taken.composer.as_ref())
                .is_some_and(|reopened| reopened.original.is_some()),
            _ => false,
        }
    }

    /// Puts the composer of sent gesture `id`, taken back from the outbox, where it was: a reply or forward in the
    /// reader pane, new mail in a new window.
    pub(super) fn reopen_compose(&mut self, id: u64) {
        let Some(taken) = self.taken.iter_mut().find(|taken| taken.id == id) else {
            return;
        };
        if let Some(mut composer) = taken.composer.take() {
            taken.state = UndoState::Undone;
            composer.sending = false;
            self.place_composer(*composer);
        }
    }

    /// Takes sent message `outbox_id` of `account` back on a worker, for gesture `taken`.
    pub(super) fn cancel_outbox(&mut self, taken: u64, account: usize, outbox_id: String) {
        let (paths, cancel) = (self.paths.clone(), self.cancel_send);
        let name = self.accounts[account].name.clone();
        let answer = worker::oneshot("compose-cancel", self.wake.clone(), move || {
            cancel(&paths, &name, &outbox_id)
        });
        self.compose_cancelling.push((taken, answer));
    }

    pub(super) fn receive_compose(&mut self, now: f64) {
        self.receive_prefill();
        self.receive_opened_draft();
        self.receive_attach();
        self.receive_completion();
        self.receive_retries();
        self.receive_outbox_edits();
        self.receive_saves();
        let mut index = 0;
        while index < self.compose_sending.len() {
            match worker::poll(&self.compose_sending[index].answer) {
                Some(result) => {
                    let sending = self.compose_sending.remove(index);
                    self.sent(
                        sending,
                        result.unwrap_or_else(|Stopped| Err(STOPPED.into())),
                        now,
                    );
                }
                None => index += 1,
            }
        }
        let mut index = 0;
        while index < self.compose_cancelling.len() {
            match worker::poll(&self.compose_cancelling[index].1) {
                Some(result) => {
                    let (taken, _) = self.compose_cancelling.remove(index);
                    let stopped = || CancelError::Failed(STOPPED.into());
                    self.cancelled(taken, result.unwrap_or_else(|Stopped| Err(stopped())));
                }
                None => index += 1,
            }
        }
    }

    fn receive_prefill(&mut self) {
        let Some((id, answer)) = &self.compose_prefill else {
            return;
        };
        let Some(result) = worker::poll(answer) else {
            return;
        };
        let id = *id;
        self.compose_prefill = None;
        let identity = self
            .composer
            .as_ref()
            .and_then(|composer| self.identity_of(&composer.account));
        let Some(composer) = self.composer.as_mut().filter(|composer| composer.id == id) else {
            return;
        };
        // A failed prefill keeps the composer loading, so Send stays off; the error says why.
        match (
            result.unwrap_or_else(|Stopped| Err(STOPPED.into())),
            identity,
        ) {
            (Ok(prefilled), Some(identity)) => {
                composer.fill(prefilled, &identity);
                composer.check_size();
            }
            (Ok(_), None) => composer.error = Some((Field::Send, NO_IDENTITY.into())),
            (Err(why), _) => composer.error = Some((Field::Send, why)),
        }
    }

    /// The daemon's answer to a send: a queued message closes its composer and shows the toast, which Undo takes
    /// back; a refused one stays open with why.
    fn sent(&mut self, sending: Sending, result: Result<(Queued, u64), QueueError>, now: f64) {
        let Sending {
            target,
            mut composer,
            ..
        } = sending;
        composer.sending = false;
        let still_open = self
            .composer_ref(target)
            .is_some_and(|open| open.id == composer.id);
        let (queued, undo_secs) = match result {
            Ok(answer) => answer,
            Err(QueueError::OriginalGone(_)) if still_open && target == ComposeTarget::Pane => {
                self.answer_as_new_mail();
                return;
            }
            Err(why) => {
                let why = why.into_text();
                match self.composer_mut(target).filter(|_| still_open) {
                    Some(open) => {
                        open.sending = false;
                        open.error = Some((Field::Send, why));
                    }
                    None => self.note_error(None, format!("not sent: {why}")),
                }
                return;
            }
        };
        let dir = recovery_dir(&self.paths);
        if still_open {
            // Its draft files stay while Undo may reopen it; the gesture removes them when it lets the composer go.
            remove_recovery_file(&dir, composer.id);
            self.recovery_written.retain(|w| w.id != composer.id);
            self.forget_composer(target);
        }
        if undo_secs == 0 {
            remove_files(&dir, composer.id);
            self.toast = Some(Toast::new("Sent".into(), false));
            return;
        }
        let words = undo::sending_text(&queued.first_recipient, queued.more);
        // Without the account in the window there is nothing to take it back from, so nothing to undo.
        let Some(account) = self.accounts.iter().position(|a| a.name == queued.account) else {
            remove_files(&dir, composer.id);
            self.toast = Some(Toast::new(words, false));
            return;
        };
        let id = self.take(words.clone(), 0);
        if let Some(taken) = self.taken.iter_mut().find(|taken| taken.id == id) {
            taken.outbox = Some((account, queued.id));
            taken.composer = Some(Box::new(composer));
        }
        self.toast = Some(Toast::sending(words, undo_secs, now));
    }

    /// The daemon's answer to taking sent gesture `id` back: in time it reopens the composer, too late it says so.
    fn cancelled(&mut self, id: u64, result: Result<(), CancelError>) {
        let Some(taken) = self.taken.iter_mut().find(|taken| taken.id == id) else {
            return;
        };
        match result {
            Ok(()) => {
                // Open, with nothing left to cancel: `z` reopens the composer again if leaving the pane was cancelled.
                taken.outbox = None;
                taken.state = if taken.composer.is_some() {
                    UndoState::Open
                } else {
                    UndoState::Undone
                };
                // Kept on disk too, as it may wait in `taken` behind another composer.
                if let Some(composer) = &taken.composer
                    && let Err(e) = save_recovery(&recovery_dir(&self.paths), composer)
                {
                    log::warn!(
                        "could not keep the unsent message {}.json: {e}",
                        composer.id
                    );
                }
                self.toast = Some(Toast::new("Not sent".into(), false));
                self.apply_action(UiAction::ReopenCompose(id));
            }
            Err(CancelError::TooLate) => {
                taken.state = UndoState::Undone;
                if let Some(composer) = taken.composer.take() {
                    remove_unkept_files(&recovery_dir(&self.paths), composer.id);
                }
                self.toast = Some(Toast::new("Already on its way".into(), false));
            }
            Err(CancelError::Failed(why)) => {
                taken.state = UndoState::Open;
                self.toast = Some(Toast::new(format!("Could not take it back: {why}"), false));
            }
        }
    }

    /// Closes each sent gesture whose message is being sent, was sent or is gone from its account's outbox, read on
    /// the file poll; `kept` are the entries still waiting or failed, which Undo can take back.
    pub(super) fn drop_sent_composers(&mut self, kept: &[(usize, String)]) {
        for taken in &mut self.taken {
            if taken.state == UndoState::Open
                && let Some(outbox) = &taken.outbox
                && !kept.contains(outbox)
            {
                taken.outbox = None;
                if let Some(composer) = taken.composer.take() {
                    remove_unkept_files(&recovery_dir(&self.paths), composer.id);
                }
                taken.state = UndoState::Undone;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::compose::ComposeKind;
    use crate::test_support::{Driver, Fixture, Wires, message};
    use i_slint_backend_testing::ElementHandle;
    use postvak_core::send::Envelope;
    use postvak_core::send::build::Draft;
    use postvak_core::send::compose::Body;
    use postvak_core::send::outbox::{Entry, Outbox, State};
    use slint::ComponentHandle;

    const PANE: ComposeTarget = ComposeTarget::Pane;

    fn filled(_: &Paths, _: &Config, account: &str, kind: &SendKind) -> Result<Prefilled, String> {
        let subject = match kind {
            SendKind::Forward { .. } => "Fwd: subject 1",
            _ => "Re: subject 1",
        };
        Ok(Prefilled {
            account: account.into(),
            draft: Draft {
                from: "work@example.com".into(),
                to: vec!["Sender 1 <sender1@example.com>".into()],
                subject: subject.into(),
                ..Draft::default()
            },
            original: Some(b"Message-ID: <1@example.com>\r\n\r\nBody of 1".to_vec()),
            attachments: Vec::new(),
        })
    }

    fn queued(_: &Paths, _: &Config, request: SendRequest) -> Result<Queued, QueueError> {
        Ok(Queued {
            account: request.account.unwrap_or_default(),
            id: "0000abcd".into(),
            due: 0,
            first_recipient: "Sender 1".into(),
            more: 0,
        })
    }

    struct Replying {
        driver: Driver,
        wires: Wires,
        fx: Fixture,
    }

    /// A window over `uids` in work's INBOX, with the daemon's sending faked.
    fn replying(uids: &[u32]) -> Replying {
        replying_with(uids, None)
    }

    fn replying_with(uids: &[u32], folder: Option<&str>) -> Replying {
        let fx = Fixture::new(&["work"]);
        if let Some(folder) = folder {
            fx.folder("work", folder, Some(folder));
        }
        for &uid in uids {
            fx.add("work", message("INBOX", uid, &format!("subject {uid}")));
        }
        let (mut driver, wires) = fx.driver();
        driver.app.prefill = filled;
        driver.app.queue_send = queued;
        driver.app.cancel_send = |_, _, _| Ok(());
        Replying { driver, wires, fx }
    }

    fn filled_in(app: &App) -> bool {
        app.composer.as_ref().is_some_and(|c| !c.loading)
    }

    /// Presses `key` and waits for the composer it opens to fill.
    fn open(driver: &mut Driver, key: &str) {
        driver.press(key);
        driver.tick_until(filled_in);
    }

    /// Types into the pane's composer and sends it; waits until the daemon's answer closed it.
    fn send(driver: &mut Driver, text: &str) {
        driver.apply(UiAction::ComposeEdit(PANE, ComposeField::Text, text.into()));
        driver.apply(UiAction::ComposeSend(PANE));
        driver.tick_until(|app| app.composer.is_none() && app.compose_sending.is_empty());
    }

    fn toast(driver: &Driver) -> Option<(&str, bool)> {
        let toast = driver.app.toast.as_ref()?;
        Some((toast.text.as_str(), toast.undo))
    }

    fn undone(app: &App) -> bool {
        app.compose_cancelling.is_empty()
            && app
                .toast
                .as_ref()
                .is_some_and(|toast| toast.text != "Undoing…")
    }

    #[test]
    fn the_reply_composer_sits_above_the_original() {
        let fx = Fixture::new(&["work"]);
        fx.add("work", message("INBOX", 1, "subject 1"));
        let (open, _wires) = fx.window();
        open.with(|live| live.app.prefill = filled);
        open.press("r");
        open.tick_until(|open| open.with(|live| filled_in(&live.app)));
        let subject = open.get("Subject").accessible_value().unwrap_or_default();
        assert!(subject.starts_with("Re: "), "{subject}");
        assert_eq!(open.window().get_body_text(), "Body of 1");
        let (_, composer) = open.span("Your text");
        let (original, _) = open.span("Message text");
        assert!(composer < original, "{composer} vs {original}");
    }

    /// The window over message 1 with its reply open and filled.
    fn replying_in_the_window() -> (Fixture, crate::test_support::Open) {
        let fx = Fixture::new(&["work"]);
        fx.add("work", message("INBOX", 1, "subject 1"));
        fx.add("work", message("INBOX", 2, "subject 2"));
        let (open, _wires) = fx.window();
        open.with(|live| {
            live.app.prefill = filled;
            live.app.queue_send = queued;
        });
        open.press("r");
        open.tick_until(|open| open.with(|live| filled_in(&live.app)));
        (fx, open)
    }

    #[test]
    fn a_reply_takes_the_keyboard_to_its_text() {
        let (_fx, open) = replying_in_the_window();
        open.press("Q");
        assert!(open.with(|live| {
            live.app
                .composer
                .as_ref()
                .is_some_and(|composer| composer.text.contains('Q'))
        }));
    }

    #[test]
    fn leaving_a_changed_composer_asks_in_the_window() {
        let (_fx, open) = replying_in_the_window();
        open.window()
            .global::<crate::ui::ComposeState>()
            .invoke_edited(crate::ui::ComposeField::Text, "Hi".into());
        assert_eq!(
            open.with(|live| live.app.composer.as_ref().map(|c| c.text.clone())),
            Some("Hi".into())
        );
        // The reply has the keyboard, so Esc is how the keys leave it.
        open.press("escape");
        assert_eq!(open.find("Discard this message?").len(), 1);
        open.click("Cancel");
        assert!(open.find("Discard this message?").is_empty());
        assert!(open.with(|live| live.app.composer.is_some()));
        open.press("escape");
        let discard = open.find("Discard");
        assert_eq!(discard.len(), 2, "the composer's and the question's");
        discard[1].invoke_accessible_default_action();
        open.tick();
        assert!(open.with(|live| live.app.composer.is_none()));
        assert!(open.find("Your text").is_empty());
        let cursor = open.with(|live| live.app.list.cursor);
        open.press("j");
        assert_ne!(
            open.with(|live| live.app.list.cursor),
            cursor,
            "the list has the keyboard again"
        );
    }

    #[test]
    fn the_fields_wait_while_loading_and_sending() {
        let (_fx, open) = replying_in_the_window();
        let read_only = |open: &crate::test_support::Open| {
            ["Subject", "To", "Your text"].map(|label| open.get(label).accessible_read_only())
        };
        assert_eq!(read_only(&open), [Some(false); 3]);
        for set in [
            |c: &mut Composer| c.loading = true,
            |c: &mut Composer| c.sending = true,
        ] {
            open.with(|live| live.app.composer.as_mut().map(set));
            open.tick();
            assert_eq!(read_only(&open), [Some(true); 3]);
            open.with(|live| {
                live.app
                    .composer
                    .as_mut()
                    .map(|c| (c.loading, c.sending) = (false, false))
            });
            open.tick();
        }
    }

    #[test]
    fn ctrl_enter_sends_from_the_composer() {
        use slint::platform::{Key, WindowEvent};
        let (_fx, open) = replying_in_the_window();
        open.tap("Your text");
        open.get("Your text").set_accessible_value("Hi");
        let window = open.window();
        let key = |text: slint::SharedString, pressed: bool| {
            let event = if pressed {
                WindowEvent::KeyPressed { text }
            } else {
                WindowEvent::KeyReleased { text }
            };
            window.window().dispatch_event(event);
        };
        key(Key::Control.into(), true);
        key(Key::Return.into(), true);
        key(Key::Return.into(), false);
        key(Key::Control.into(), false);
        open.tick_until(|open| open.with(|live| live.app.composer.is_none()));
    }

    #[test]
    fn escape_closes_the_composer_and_asks_when_changed() {
        let Replying { mut driver, .. } = replying(&[1, 2]);
        driver.press("x");
        open(&mut driver, "r");
        driver.press("escape");
        assert!(
            driver.app.composer.is_none(),
            "an unchanged composer closes"
        );
        assert!(!driver.app.list.marked.is_empty(), "the selection stays");

        open(&mut driver, "r");
        driver.apply(UiAction::ComposeEdit(PANE, ComposeField::Text, "Hi".into()));
        driver.press("escape");
        assert_eq!(driver.app.top_dialog(), Some(Dialog::LeaveCompose));
        driver.press("escape");
        assert_eq!(
            driver.app.top_dialog(),
            None,
            "Esc answers the question with Cancel"
        );
        assert!(driver.app.composer.is_some());
        driver.press("escape");
        driver.apply(UiAction::LeaveComposeDiscard(PANE));
        assert!(driver.app.composer.is_none());
        assert!(!driver.app.list.marked.is_empty(), "the selection stays");
    }

    #[test]
    fn a_gone_original_is_told_apart_through_context() {
        let gone = postvak_core::send::compose::OriginalGone {
            folder: "INBOX".into(),
            uid: 1,
            account: "work".into(),
        };
        let error = anyhow::Error::new(gone).context("sending");
        assert!(matches!(queue_error(&error), QueueError::OriginalGone(_)));
        let other = anyhow::anyhow!("no message INBOX/1 in account 'work'");
        assert!(
            matches!(queue_error(&other), QueueError::Failed(_)),
            "the words alone do not count"
        );
    }

    #[test]
    fn the_daemons_too_late_refusal_is_told_apart() {
        use postvak_core::send::outbox::CancelError as Outbox;
        for gone in [
            Outbox::TooLate("0000abcd".into()),
            Outbox::NotFound("0000abcd".into()),
        ] {
            assert_eq!(cancel_error(&gone.to_string()), CancelError::TooLate);
        }
        assert_eq!(
            cancel_error("the daemon is stopping"),
            CancelError::Failed("the daemon is stopping".into())
        );
    }

    #[test]
    fn reply_opens_the_composer_over_the_original_and_fills_it() {
        let Replying { mut driver, .. } = replying(&[1]);
        driver.app.apply(UiAction::Reply(ReplyTo::Sender));
        let composer = driver.app.composer.as_ref().unwrap();
        assert!(composer.loading);
        assert_eq!(
            composer
                .original
                .as_ref()
                .map(|o| (o.folder.as_str(), o.uid)),
            Some(("INBOX", 1))
        );
        driver.app.apply(UiAction::ComposeSend(PANE));
        assert!(
            !driver.app.composer.as_ref().unwrap().sending,
            "Send waits for the prefill"
        );
        assert!(driver.app.compose_sending.is_empty());

        driver.tick_until(filled_in);
        let composer = driver.app.composer.as_ref().unwrap();
        assert_eq!(composer.kind, ComposeKind::Reply(ReplyTo::Sender));
        assert_eq!(composer.subject, "Re: subject 1");
        assert_eq!(composer.to, "Sender 1 <sender1@example.com>");
        assert_eq!(composer.from, "work@example.com");
        assert_eq!(
            driver.app.body.as_ref().map(|b| b.key.1),
            Some(1),
            "the original stays shown below"
        );
    }

    #[test]
    fn send_queues_the_request_and_shows_the_toast() {
        static SENT: Mutex<Vec<SendRequest>> = Mutex::new(Vec::new());
        let Replying { mut driver, .. } = replying(&[1]);
        driver.app.queue_send = |paths, config, request| {
            SENT.lock().unwrap().push(request.clone());
            queued(paths, config, request)
        };
        open(&mut driver, "r");
        send(&mut driver, "Thanks");

        let sent = SENT.lock().unwrap().clone();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].body, Body::Given("Thanks".into()));
        assert!(matches!(
            sent[0].kind,
            SendKind::Reply {
                uid: 1,
                to: ReplyTo::Sender,
                ..
            }
        ));
        assert_eq!(toast(&driver), Some(("Sending to Sender 1 in 10 s", true)));
        let taken = driver.app.taken.last().unwrap();
        assert_eq!(taken.outbox, Some((0, "0000abcd".to_string())));
        driver.at(3.5);
        assert_eq!(toast(&driver), Some(("Sending to Sender 1 in 7 s", true)));
        driver.at(10.0);
        assert_eq!(toast(&driver), None, "the toast goes when the message does");
    }

    #[test]
    fn send_now_sends_the_waiting_message_at_once() {
        static SENT_NOW: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());
        let Replying { mut driver, .. } = replying(&[1]);
        driver.app.retry_send = |_, account, id| {
            SENT_NOW.lock().unwrap().push((account.into(), id.into()));
            Ok(())
        };
        open(&mut driver, "r");
        send(&mut driver, "Thanks");
        driver.apply(UiAction::SendNow);
        assert_eq!(toast(&driver), Some(("Sending to Sender 1", false)));
        driver.tick_until(|app| app.retrying.is_empty());
        assert_eq!(
            SENT_NOW.lock().unwrap().clone(),
            [("work".to_string(), "0000abcd".to_string())]
        );
        driver.apply(UiAction::SendNow);
        driver.tick_until(|app| app.retrying.is_empty());
        assert_eq!(
            SENT_NOW.lock().unwrap().len(),
            1,
            "only while the toast counts down"
        );
    }

    #[test]
    fn without_an_undo_delay_the_toast_says_sent() {
        let Replying { mut driver, fx, .. } = replying(&[1]);
        fx.append_config("\n[sending]\nundo_secs = 0\n");
        open(&mut driver, "r");
        send(&mut driver, "Thanks");
        assert_eq!(toast(&driver), Some(("Sent", false)));
    }

    #[test]
    fn undo_reopens_the_composer_as_it_was() {
        let Replying { mut driver, .. } = replying(&[1]);
        open(&mut driver, "r");
        driver.apply(UiAction::ComposeEdit(
            PANE,
            ComposeField::Cc,
            "cc@example.org".into(),
        ));
        let before = driver.app.composer.clone().unwrap();
        send(&mut driver, "Thanks");

        driver.apply(UiAction::Undo);
        driver.tick_until(|app| app.composer.is_some());
        let mut reopened = driver.app.composer.clone().unwrap();
        assert_eq!(reopened.text, "Thanks");
        assert_eq!(reopened.cc, "cc@example.org");
        assert!(!reopened.sending);
        reopened.text = before.text.clone();
        assert_eq!(reopened, before);
    }

    #[test]
    fn undo_reopens_a_reply_over_its_original() {
        let Replying { mut driver, .. } = replying(&[1, 2]);
        driver.press("down");
        open(&mut driver, "r");
        let original = driver.app.body.as_ref().map(|body| body.key.clone());
        send(&mut driver, "Thanks");
        driver.press("up");
        driver.tick_until(|app| app.body.as_ref().map(|b| &b.key) != original.as_ref());

        driver.apply(UiAction::Undo);
        driver.tick_until(|app| app.composer.is_some());
        driver.tick_until(|app| app.body.as_ref().map(|b| &b.key) == original.as_ref());
    }

    #[test]
    fn undo_of_a_reply_whose_original_went_reopens_it_as_new_mail() {
        let Replying { mut driver, fx, .. } = replying(&[1]);
        open(&mut driver, "r");
        send(&mut driver, "Thanks");
        let store = postvak_core::store::Store::open_account(&fx.paths, "work").unwrap();
        store.remove_message("INBOX", 1).unwrap();

        driver.apply(UiAction::Undo);
        driver.tick_until(|app| !app.new_mail.is_empty());
        assert!(driver.app.composer.is_none());
        assert_eq!(driver.app.new_mail[0].kind, ComposeKind::New);
        assert_eq!(driver.app.new_mail[0].text, "Thanks");
    }

    #[test]
    fn z_undoes_a_send_too() {
        static CANCELLED: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());
        let Replying { mut driver, .. } = replying(&[1]);
        driver.app.cancel_send = |_, account, id| {
            CANCELLED.lock().unwrap().push((account.into(), id.into()));
            Ok(())
        };
        open(&mut driver, "r");
        send(&mut driver, "Thanks");
        driver.press("z");
        driver.tick_until(|app| app.composer.is_some());
        assert_eq!(
            CANCELLED.lock().unwrap().clone(),
            [("work".to_string(), "0000abcd".to_string())]
        );
        assert_eq!(driver.app.composer.as_ref().unwrap().text, "Thanks");
    }

    #[test]
    fn undo_too_late_says_so_and_reopens_nothing() {
        let Replying { mut driver, .. } = replying(&[1]);
        driver.app.cancel_send = |_, _, _| Err(CancelError::TooLate);
        open(&mut driver, "r");
        send(&mut driver, "Thanks");
        driver.press("z");
        driver.tick_until(undone);
        assert_eq!(toast(&driver), Some(("Already on its way", false)));
        assert!(driver.app.composer.is_none());
        assert!(driver.app.taken.last().unwrap().composer.is_none());
    }

    #[test]
    fn a_failed_queue_keeps_the_composer_with_the_error() {
        let Replying { mut driver, .. } = replying(&[1]);
        driver.app.queue_send = |_, _, _| Err("could not reach the daemon".into());
        open(&mut driver, "r");
        driver.apply(UiAction::ComposeEdit(
            PANE,
            ComposeField::Text,
            "Thanks".into(),
        ));
        driver.apply(UiAction::ComposeSend(PANE));
        driver.tick_until(|app| app.compose_sending.is_empty());
        let composer = driver.app.composer.as_ref().unwrap();
        assert_eq!(composer.text, "Thanks");
        assert!(!composer.sending);
        assert_eq!(
            composer.error,
            Some((Field::Send, "could not reach the daemon".into()))
        );
        assert!(driver.app.taken.is_empty(), "nothing to undo");
    }

    #[test]
    fn a_second_send_while_one_is_in_flight_is_ignored() {
        static QUEUED: AtomicUsize = AtomicUsize::new(0);
        let Replying { mut driver, .. } = replying(&[1]);
        driver.app.queue_send = |paths, config, request| {
            QUEUED.fetch_add(1, Ordering::SeqCst);
            queued(paths, config, request)
        };
        open(&mut driver, "r");
        driver.app.apply(UiAction::ComposeSend(PANE));
        assert!(driver.app.composer.as_ref().unwrap().sending);
        driver.app.apply(UiAction::ComposeSend(PANE));
        driver.tick_until(|app| app.composer.is_none() && app.compose_sending.is_empty());
        assert_eq!(QUEUED.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_send_is_checked_before_it_is_queued() {
        let Replying { mut driver, .. } = replying(&[1]);
        open(&mut driver, "r");
        driver.apply(UiAction::ComposeEdit(PANE, ComposeField::To, String::new()));
        driver.apply(UiAction::ComposeSend(PANE));
        let composer = driver.app.composer.as_ref().unwrap();
        assert_eq!(composer.error.as_ref().map(|e| e.0), Some(Field::To));
        assert!(!composer.sending);
        assert!(driver.app.compose_sending.is_empty());
    }

    #[test]
    fn leaving_a_changed_composer_asks_and_discard_goes_on() {
        let Replying { mut driver, .. } = replying(&[1, 2]);
        open(&mut driver, "r");
        driver.apply(UiAction::ComposeEdit(
            PANE,
            ComposeField::Text,
            "Thanks".into(),
        ));
        let cursor = driver.app.list.cursor;
        driver.press("j");
        assert_eq!(driver.app.top_dialog(), Some(Dialog::LeaveCompose));
        assert_eq!(driver.app.list.cursor, cursor, "held until answered");

        driver.apply(UiAction::LeaveComposeCancel(PANE));
        assert!(driver.app.composer.is_some());
        assert_eq!(driver.app.top_dialog(), None);

        driver.press("j");
        driver.apply(UiAction::LeaveComposeDiscard(PANE));
        assert!(driver.app.composer.is_none());
        assert_eq!(driver.app.list.cursor, cursor + 1);
        assert_eq!(driver.app.top_dialog(), None);
    }

    #[test]
    fn an_unchanged_composer_closes_when_leaving() {
        let Replying { mut driver, .. } = replying(&[1, 2]);
        open(&mut driver, "r");
        let cursor = driver.app.list.cursor;
        driver.press("j");
        assert!(driver.app.composer.is_none());
        assert_eq!(driver.app.top_dialog(), None);
        assert_eq!(driver.app.list.cursor, cursor + 1);
    }

    #[test]
    fn an_unexpected_answer_to_a_save_names_only_its_kind() {
        let event = Event::Error {
            account: "work".into(),
            message: "Subject: lunch".into(),
        };
        assert_eq!(
            draft_answer(Ok(event)),
            Err("unexpected answer from the daemon: Error".to_string())
        );
    }

    #[test]
    fn enter_on_the_list_outside_drafts_keeps_the_reply() {
        let Replying { mut driver, .. } = replying(&[1, 2]);
        open(&mut driver, "r");
        driver.app.focus = Focus::List;
        driver.press("enter");
        assert!(driver.app.composer.is_some(), "an unchanged one too");
        driver.apply(UiAction::ComposeEdit(PANE, ComposeField::Text, "Hi".into()));
        driver.press("enter");
        assert!(driver.app.composer.is_some());
        assert_eq!(driver.app.top_dialog(), None);
    }

    #[test]
    fn the_keys_open_each_kind() {
        let fx = Fixture::new(&["work"]);
        let mut list = message("INBOX", 1, "subject 1");
        list.headers =
            b"Subject: subject 1\r\nList-Post: <mailto:list@example.org>\r\n\r\n".to_vec();
        fx.add("work", list);
        let (mut driver, _wires) = fx.driver();
        driver.app.prefill = filled;
        for (key, kind) in [
            ("r", ComposeKind::Reply(ReplyTo::Sender)),
            ("R", ComposeKind::Reply(ReplyTo::All)),
            ("l", ComposeKind::Reply(ReplyTo::List)),
            ("f", ComposeKind::Forward),
        ] {
            open(&mut driver, key);
            assert_eq!(
                driver.app.composer.as_ref().map(|c| c.kind),
                Some(kind),
                "{key}"
            );
        }
    }

    #[test]
    fn l_opens_nothing_without_a_list_to_reply_to() {
        let Replying { mut driver, .. } = replying(&[1]);
        driver.tick_until(|app| app.body.is_some());
        driver.press("l");
        assert!(driver.app.composer.is_none());
        assert!(driver.app.compose_prefill.is_none());
    }

    /// Queues as the daemon does: the entry is in the account's outbox, waiting.
    fn queued_in_outbox(
        paths: &Paths,
        _: &Config,
        request: SendRequest,
    ) -> Result<Queued, QueueError> {
        let envelope = Envelope {
            from: "work@example.com".into(),
            to: vec!["sender1@example.com".into()],
        };
        let entry = Entry::new(envelope, 0, false, None);
        let outbox = Outbox::open(paths, "work").map_err(|e| e.to_string())?;
        outbox.add(b"raw", &entry).map_err(|e| e.to_string())?;
        Ok(Queued {
            id: entry.id,
            ..queued(paths, &Config::default(), request)?
        })
    }

    /// Sets the state of the newest sent message in work's outbox, and lets the file poll read it.
    fn outbox_state(driver: &mut Driver, fx: &Fixture, state: Option<State>) {
        let (_, id) = driver.app.taken.last().unwrap().outbox.clone().unwrap();
        let outbox = Outbox::open(&fx.paths, "work").unwrap();
        match state {
            Some(state) => outbox.set_state(&id, state).unwrap(),
            None => outbox.remove(&id).unwrap(),
        }
        let at = driver.now + POLL;
        driver.at(at);
    }

    #[test]
    fn the_composer_is_dropped_once_its_message_left_waiting() {
        let Replying { mut driver, fx, .. } = replying(&[1]);
        driver.app.queue_send = queued_in_outbox;
        open(&mut driver, "r");
        send(&mut driver, "Thanks");
        driver.at(POLL);
        assert!(
            driver.app.taken.last().unwrap().composer.is_some(),
            "still waiting"
        );
        outbox_state(&mut driver, &fx, Some(State::Sending));
        assert!(driver.app.taken.last().unwrap().composer.is_none());
    }

    #[test]
    fn undo_after_a_failure_reopens_the_composer() {
        let Replying { mut driver, fx, .. } = replying(&[1]);
        driver.app.queue_send = queued_in_outbox;
        open(&mut driver, "r");
        send(&mut driver, "Thanks");
        let failed = State::Failed {
            reason: "550 no such user".into(),
            retrying: false,
        };
        outbox_state(&mut driver, &fx, Some(failed));
        assert!(driver.app.taken.last().unwrap().composer.is_some());
        driver.press("z");
        driver.tick_until(|app| app.composer.is_some());
        assert_eq!(driver.app.composer.as_ref().unwrap().text, "Thanks");
    }

    #[test]
    fn undo_after_the_message_went_moves_on_to_the_previous_gesture() {
        static CANCELS: AtomicUsize = AtomicUsize::new(0);
        for gone in [Some(State::Sending), Some(State::Sent), None] {
            let Replying { mut driver, fx, .. } = replying(&[1]);
            driver.app.queue_send = queued_in_outbox;
            driver.app.cancel_send = |_, _, _| {
                CANCELS.fetch_add(1, Ordering::SeqCst);
                Ok(())
            };
            driver.press("u");
            let marked = driver.app.taken.last().unwrap().id;
            open(&mut driver, "r");
            send(&mut driver, "Thanks");
            outbox_state(&mut driver, &fx, gone.clone());
            driver.press("z");
            let previous = driver.app.taken.iter().find(|t| t.id == marked).unwrap();
            assert_ne!(previous.state, UndoState::Open, "z reached it: {gone:?}");
            assert!(driver.app.compose_cancelling.is_empty(), "{gone:?}");
        }
        assert_eq!(CANCELS.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn searching_with_a_changed_composer_asks() {
        let Replying { mut driver, .. } = replying(&[1]);
        open(&mut driver, "r");
        driver.apply(UiAction::ComposeEdit(
            PANE,
            ComposeField::Text,
            "Thanks".into(),
        ));
        for leave in [
            UiAction::StartSearch,
            UiAction::SearchFor("lunch".into()),
            UiAction::SearchAccepted,
            UiAction::SetFilter(Filter {
                unread: true,
                ..Filter::default()
            }),
        ] {
            driver.apply(leave.clone());
            assert_eq!(
                driver.app.top_dialog(),
                Some(Dialog::LeaveCompose),
                "{leave:?}"
            );
            assert_eq!(driver.app.search, None, "{leave:?}");
            driver.apply(UiAction::LeaveComposeCancel(PANE));
        }
        assert!(driver.app.composer.is_some());
    }

    #[test]
    fn archiving_the_original_with_a_changed_composer_asks() {
        let Replying {
            mut driver, wires, ..
        } = replying(&[1]);
        open(&mut driver, "r");
        driver.apply(UiAction::ComposeEdit(
            PANE,
            ComposeField::Text,
            "Thanks".into(),
        ));
        driver.press("u");
        assert_eq!(
            driver.app.top_dialog(),
            None,
            "marking it unread keeps it shown"
        );
        wires.sent();
        driver.press("e");
        assert_eq!(driver.app.top_dialog(), Some(Dialog::LeaveCompose));
        assert!(wires.sent().is_empty(), "nothing archived until answered");
        driver.apply(UiAction::LeaveComposeDiscard(PANE));
        assert!(driver.app.composer.is_none());
        assert_eq!(wires.sent().len(), 1, "then archived");
    }

    #[test]
    fn a_composer_being_sent_cannot_be_left() {
        let Replying { mut driver, .. } = replying(&[1, 2]);
        open(&mut driver, "r");
        driver.apply(UiAction::ComposeEdit(
            PANE,
            ComposeField::Text,
            "Thanks".into(),
        ));
        let cursor = driver.app.list.cursor;
        driver.app.apply(UiAction::ComposeSend(PANE));
        driver.app.apply(UiAction::MoveCursor(1));
        driver.app.apply(UiAction::ComposeDiscard(PANE));
        assert_eq!(driver.app.list.cursor, cursor);
        assert_eq!(driver.app.top_dialog(), None);
        assert!(driver.app.composer.as_ref().unwrap().sending);
        driver.tick_until(|app| app.composer.is_none() && app.compose_sending.is_empty());
        assert!(driver.app.taken.last().unwrap().composer.is_some());
    }

    #[test]
    fn a_send_for_an_account_not_in_the_window_keeps_nothing_to_undo() {
        let Replying { mut driver, .. } = replying(&[1]);
        driver.app.queue_send = |paths, config, request| {
            Ok(Queued {
                account: "elsewhere".into(),
                ..queued(paths, config, request)?
            })
        };
        open(&mut driver, "r");
        send(&mut driver, "Thanks");
        assert_eq!(toast(&driver), Some(("Sending to Sender 1", false)));
        assert!(driver.app.taken.is_empty());
    }

    fn new_mail_subjects(open: &crate::test_support::Open) -> Vec<String> {
        open.with(|live| {
            live.app
                .new_mail
                .iter()
                .map(|c| c.subject.clone())
                .collect()
        })
    }

    #[test]
    fn ctrl_n_opens_a_new_mail_window_on_the_viewed_account() {
        let fx = Fixture::new(&["home", "work"]);
        fx.add("work", message("INBOX", 1, "subject 1"));
        let (open, _wires) = fx.window();
        open.with(|live| {
            live.app.select_view(View::Folder {
                account: 1,
                folder: "INBOX".into(),
            })
        });
        assert!(open.window().invoke_key("n".into(), true, false));
        open.tick();
        let windows = open.compose_windows();
        assert_eq!(windows.len(), 1);
        let from = ElementHandle::find_by_accessible_label(&windows[0], "From")
            .next()
            .and_then(|e| e.accessible_value());
        assert_eq!(from.as_deref(), Some("work@example.com"));
        assert_eq!(
            open.with(|live| live.app.new_mail[0].kind),
            ComposeKind::New
        );

        open.with(|live| live.app.select_view(View::Rules));
        open.window().invoke_key("n".into(), true, false);
        open.tick();
        assert_eq!(
            open.with(|live| live
                .app
                .new_mail
                .iter()
                .map(|c| c.account.clone())
                .collect::<Vec<_>>()),
            ["work", "home"],
            "outside a folder, the first account"
        );
        assert_eq!(open.compose_windows().len(), 2);
    }

    #[test]
    fn two_new_mail_windows_keep_their_own_fields() {
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = fx.window();
        open.with(|live| {
            live.app.apply(UiAction::NewMail(None));
            live.app.apply(UiAction::NewMail(None));
        });
        let windows = open.compose_windows();
        assert_eq!(windows.len(), 2);
        windows[0]
            .global::<crate::ui::ComposeState>()
            .invoke_edited(crate::ui::ComposeField::Subject, "First".into());
        windows[1]
            .global::<crate::ui::ComposeState>()
            .invoke_edited(crate::ui::ComposeField::Subject, "Second".into());
        open.tick();
        assert_eq!(new_mail_subjects(&open), ["First", "Second"]);
        assert_eq!(
            windows[0]
                .global::<crate::ui::ComposeState>()
                .get_data()
                .subject,
            "First"
        );
        assert_eq!(
            windows[1]
                .global::<crate::ui::ComposeState>()
                .get_data()
                .subject,
            "Second"
        );
        assert!(open.with(|live| live.app.composer.is_none()));
    }

    #[test]
    fn closing_a_changed_new_mail_window_asks() {
        use slint::platform::{Key, WindowEvent};
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = fx.window();
        open.with(|live| live.app.apply(UiAction::NewMail(None)));
        open.compose_windows()[0]
            .global::<crate::ui::ComposeState>()
            .invoke_close();
        open.tick();
        assert!(open.compose_windows().is_empty(), "an unchanged one closes");

        open.with(|live| live.app.apply(UiAction::NewMail(None)));
        let window = open.compose_windows()[0].clone_strong();
        window
            .global::<crate::ui::ComposeState>()
            .invoke_edited(crate::ui::ComposeField::Text, "Hi".into());
        window.window().dispatch_event(WindowEvent::CloseRequested);
        open.tick();
        let asks = |window: &crate::ui::ComposeWindow| {
            ElementHandle::find_by_accessible_label(window, "Discard this message?").count()
        };
        assert_eq!(asks(&window), 1, "asked in that window");
        assert!(
            open.find("Discard this message?").is_empty(),
            "not in the mail window"
        );
        assert_eq!(open.compose_windows().len(), 1);

        let escape = |window: &crate::ui::ComposeWindow| {
            let text: slint::SharedString = Key::Escape.into();
            window
                .window()
                .dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
            window
                .window()
                .dispatch_event(WindowEvent::KeyReleased { text });
            open.tick();
        };
        escape(&window);
        assert_eq!(asks(&window), 0, "Esc answers Cancel");
        assert_eq!(new_mail_subjects(&open).len(), 1);
        escape(&window);
        assert_eq!(asks(&window), 1, "Esc closes, which asks");
        window
            .global::<crate::ui::ComposeState>()
            .invoke_leave_discard();
        open.tick();
        assert!(open.compose_windows().is_empty());
        assert!(new_mail_subjects(&open).is_empty());
    }

    #[test]
    fn a_mailto_link_opens_new_mail_with_its_fields() {
        let Replying { mut driver, .. } = replying(&[1]);
        driver.apply(UiAction::OpenUrl(
            "mailto:ann@example.org?cc=bob@example.org&subject=Lunch&body=Noon%3F".into(),
        ));
        assert!(crate::test_support::launched().is_empty(), "no other app");
        assert!(driver.app.composer.is_none());
        let composer = &driver.app.new_mail[0];
        assert_eq!(composer.kind, ComposeKind::New);
        assert_eq!(composer.account, "work");
        assert_eq!(
            (composer.to.as_str(), composer.cc.as_str()),
            ("ann@example.org", "bob@example.org")
        );
        assert_eq!(
            (composer.subject.as_str(), composer.text.as_str()),
            ("Lunch", "Noon?")
        );
        assert!(composer.show_cc_bcc);
    }

    #[test]
    fn undo_of_new_mail_reopens_it_in_a_window() {
        let Replying { mut driver, .. } = replying(&[1]);
        open(&mut driver, "r");
        driver.apply(UiAction::ComposeEdit(
            PANE,
            ComposeField::Text,
            "Draft reply".into(),
        ));
        driver.apply(UiAction::NewMail(None));
        let window = ComposeTarget::Window(driver.app.new_mail[0].id);
        driver.apply(UiAction::ComposeEdit(
            window,
            ComposeField::To,
            "ann@example.org".into(),
        ));
        driver.apply(UiAction::ComposeEdit(
            window,
            ComposeField::Text,
            "Hello".into(),
        ));
        let before = driver.app.new_mail[0].clone();
        driver.apply(UiAction::ComposeSend(window));
        driver.tick_until(|app| app.new_mail.is_empty() && app.compose_sending.is_empty());
        assert_eq!(toast(&driver), Some(("Sending to Sender 1 in 10 s", true)));

        driver.press("z");
        driver.tick_until(|app| !app.new_mail.is_empty());
        assert_eq!(driver.app.new_mail, [before]);
        assert_eq!(
            driver.app.top_dialog(),
            None,
            "the reply in the pane is not asked about"
        );
        assert_eq!(
            driver.app.composer.as_ref().map(|c| c.text.as_str()),
            Some("Draft reply")
        );
    }

    #[test]
    fn a_new_mail_window_that_cannot_open_says_why_once() {
        static TRIED: AtomicUsize = AtomicUsize::new(0);
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = fx.window();
        open.with(|live| {
            live.open_compose = |_| {
                TRIED.fetch_add(1, Ordering::SeqCst);
                Err(slint::PlatformError::Other("no display".into()))
            };
            live.app.apply(UiAction::NewMail(None));
        });
        open.tick();
        open.tick();
        assert_eq!(TRIED.load(Ordering::SeqCst), 1);
        assert!(open.compose_windows().is_empty());
        assert!(open.with(|live| live.app.new_mail.is_empty()));
        assert_eq!(
            open.with(|live| live.app.error.clone()).as_deref(),
            Some("could not open a new-mail window: no display")
        );
    }

    /// Presses `key` in `window`, with Ctrl (Cmd on macOS) when `command`.
    fn press_in(window: &crate::ui::ComposeWindow, key: slint::SharedString, command: bool) {
        use slint::platform::{Key, WindowEvent};
        let control: slint::SharedString = Key::Control.into();
        let window = window.window();
        if command {
            window.dispatch_event(WindowEvent::KeyPressed {
                text: control.clone(),
            });
        }
        window.dispatch_event(WindowEvent::KeyPressed { text: key.clone() });
        window.dispatch_event(WindowEvent::KeyReleased { text: key });
        if command {
            window.dispatch_event(WindowEvent::KeyReleased { text: control });
        }
    }

    /// Clicks the middle of the element labelled `label` in `window`.
    fn click_in(window: &crate::ui::ComposeWindow, label: &str) {
        use slint::platform::{PointerEventButton, WindowEvent};
        let element = ElementHandle::find_by_accessible_label(window, label)
            .next()
            .unwrap();
        let (at, size) = (element.absolute_position(), element.size());
        let position =
            slint::LogicalPosition::new(at.x + size.width / 2.0, at.y + size.height / 2.0);
        let button = PointerEventButton::Left;
        let window = window.window();
        window.dispatch_event(WindowEvent::PointerMoved { position });
        window.dispatch_event(WindowEvent::PointerPressed { position, button });
        window.dispatch_event(WindowEvent::PointerReleased { position, button });
    }

    #[test]
    fn ctrl_n_in_a_new_mail_window_opens_another() {
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = fx.window();
        open.with(|live| live.app.apply(UiAction::NewMail(None)));
        let window = open.compose_windows()[0].clone_strong();
        click_in(&window, "Your text");
        press_in(&window, "n".into(), true);
        open.tick();
        assert_eq!(open.compose_windows().len(), 2);
        assert_eq!(new_mail_subjects(&open).len(), 2);
        assert_eq!(
            open.with(|live| live.app.new_mail[0].text.clone()),
            "",
            "Ctrl+N typed nothing"
        );
    }

    #[test]
    fn escape_in_the_text_of_a_new_mail_window_closes_it() {
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = fx.window();
        open.with(|live| live.app.apply(UiAction::NewMail(None)));
        let window = open.compose_windows()[0].clone_strong();
        click_in(&window, "Your text");
        press_in(&window, "H".into(), false);
        open.tick();
        assert_eq!(
            open.with(|live| live.app.new_mail[0].text.clone()),
            "H",
            "the text has the keyboard"
        );
        press_in(&window, slint::platform::Key::Escape.into(), false);
        open.tick();
        let asks =
            ElementHandle::find_by_accessible_label(&window, "Discard this message?").count();
        assert_eq!(asks, 1, "Esc reached the window, which asks");
    }

    /// A message of `folder` to `to`, from `from`, at `at`.
    fn mail(folder: &str, uid: u32, from: &str, to: &str, at: i64) -> postvak_core::store::Message {
        let mut mail = message(folder, uid, "hello");
        mail.from_addr = Some(from.into());
        mail.to_addr = Some(to.into());
        mail.date = Some(at);
        mail.internaldate = at;
        mail
    }

    /// Two accounts: work wrote to Ann long ago and to Andy lately, home heard from Anna in between.
    fn people() -> Fixture {
        let fx = Fixture::new(&["home", "work"]);
        fx.folder("work", "Sent", Some("Sent"));
        let me = "Me <work@example.com>";
        fx.add(
            "work",
            mail("Sent", 1, me, "Ann Old <ann@example.org>", 1_000),
        );
        fx.add(
            "work",
            mail("Sent", 2, me, "Andy <andy@example.com>", 3_000),
        );
        let anna = "Anna New <anna@example.net>";
        fx.add("home", mail("INBOX", 1, anna, "home@example.com", 2_000));
        fx
    }

    fn completing(app: &App) -> bool {
        app.completion.is_some()
    }

    fn choices(app: &App) -> Vec<&str> {
        let completion = app.completion.as_ref().unwrap();
        completion.choices.iter().map(String::as_str).collect()
    }

    #[test]
    fn changing_from_takes_the_new_accounts_size_limit() {
        let fx = Fixture::new(&["home", "work"]);
        let limits = [("home", 20_000_000), ("work", 10_000_000)];
        for (account, limit) in limits {
            let store = fx.store(account);
            store.save_smtp_capabilities(Some(limit), false).unwrap();
        }
        let (mut driver, _wires) = fx.driver();
        driver.apply(UiAction::NewMail(None));
        let window = ComposeTarget::Window(driver.app.new_mail[0].id);
        assert_eq!(driver.app.new_mail[0].size_limit, 20_000_000);
        let dir = tempfile::tempdir().unwrap();
        driver.app.new_mail[0]
            .files
            .push((dir.path().join("big.iso"), 15_000_000));
        let work = "work\twork@example.com".to_string();
        driver.apply(UiAction::ComposeEdit(window, ComposeField::From, work));
        let composer = &driver.app.new_mail[0];
        assert_eq!(composer.account, "work");
        assert_eq!(composer.size_limit, 10_000_000);
        assert_eq!(
            composer.error.as_ref().map(|e| e.0),
            Some(Field::Attachments),
            "{:?}",
            composer.error
        );
    }

    #[test]
    fn completion_lists_people_written_to_first_across_accounts() {
        let fx = people();
        let (mut driver, _wires) = fx.driver();
        driver.apply(UiAction::NewMail(None));
        let window = ComposeTarget::Window(driver.app.new_mail[0].id);
        driver.apply(UiAction::ComposeEdit(window, ComposeField::To, "an".into()));
        driver.tick_until(completing);
        assert_eq!(
            choices(&driver.app),
            [
                "Andy <andy@example.com>",
                "Ann Old <ann@example.org>",
                "Anna New <anna@example.net>"
            ]
        );
        driver.apply(UiAction::ComposeEdit(
            window,
            ComposeField::To,
            "ann".into(),
        ));
        driver.tick_until(|app| {
            app.completion
                .as_ref()
                .is_some_and(|c| c.choices.len() == 2)
        });
        driver.apply(UiAction::ComposeEdit(
            window,
            ComposeField::To,
            "zed".into(),
        ));
        driver.tick_until(|app| !completing(app) && app.completing.is_none());
    }

    #[test]
    fn picking_a_completion_replaces_the_typed_part() {
        let fx = people();
        let (mut driver, _wires) = fx.driver();
        driver.apply(UiAction::NewMail(None));
        let window = ComposeTarget::Window(driver.app.new_mail[0].id);
        let typed = "bob@example.org, an";
        driver.apply(UiAction::ComposeEdit(
            window,
            ComposeField::To,
            typed.into(),
        ));
        driver.tick_until(completing);
        driver.apply(UiAction::ComposeCompletionStep(window, 1));
        driver.apply(UiAction::ComposeCompletionStep(window, 5));
        assert_eq!(driver.app.completion.as_ref().map(|c| c.chosen), Some(2));
        driver.apply(UiAction::ComposeCompletionStep(window, -1));
        let chosen = driver.app.completion.as_ref().unwrap().chosen;
        driver.apply(UiAction::ComposeCompletionPick(window, chosen));
        assert_eq!(
            driver.app.new_mail[0].to,
            "bob@example.org, Ann Old <ann@example.org>, "
        );
        assert!(!completing(&driver.app));
        driver.tick();
        assert!(!completing(&driver.app), "nothing typed after the comma");
    }

    #[test]
    fn a_completion_closes_when_its_field_is_left() {
        let fx = people();
        let (mut driver, _wires) = fx.driver();
        driver.apply(UiAction::NewMail(None));
        let window = ComposeTarget::Window(driver.app.new_mail[0].id);
        driver.apply(UiAction::ComposeEdit(window, ComposeField::To, "an".into()));
        driver.tick_until(completing);
        driver.apply(UiAction::ComposeCompletionClose(window));
        assert!(!completing(&driver.app));
        driver.apply(UiAction::ComposeEdit(
            window,
            ComposeField::To,
            "and".into(),
        ));
        driver.apply(UiAction::ComposeCompletionClose(window));
        // Long enough for the worker's answer, which the close made stale.
        std::thread::sleep(std::time::Duration::from_millis(200));
        driver.tick();
        assert!(
            !completing(&driver.app),
            "an answer after it closed is dropped"
        );
    }

    #[test]
    fn completion_shows_in_the_composer_under_its_field() {
        let fx = people();
        let (open, _wires) = fx.window();
        open.with(|live| live.app.apply(UiAction::NewMail(None)));
        let window = open.compose_windows()[0].clone_strong();
        let type_an = || {
            click_in(&window, "To");
            window
                .global::<crate::ui::ComposeState>()
                .invoke_edited(crate::ui::ComposeField::To, "an".into());
            open.tick_until(|open| open.with(|live| completing(&live.app)));
            open.tick();
        };
        type_an();
        let shown =
            ElementHandle::find_by_accessible_label(&window, "Andy <andy@example.com>").count();
        assert_eq!(shown, 1);
        press_in(&window, slint::platform::Key::Escape.into(), false);
        open.tick();
        assert!(
            open.with(|live| !completing(&live.app)),
            "Esc closes the list"
        );
        assert_eq!(open.compose_windows().len(), 1, "and not the window");

        type_an();
        press_in(&window, slint::platform::Key::DownArrow.into(), false);
        open.tick();
        press_in(&window, slint::platform::Key::Return.into(), false);
        open.tick();
        assert_eq!(
            open.with(|live| live.app.new_mail[0].to.clone()),
            "Ann Old <ann@example.org>, "
        );
        let field = ElementHandle::find_by_accessible_label(&window, "To").next();
        assert_eq!(
            field.and_then(|f| f.accessible_value()).as_deref(),
            Some("Ann Old <ann@example.org>, "),
            "the field shows it"
        );
    }

    /// Queues a reply to message 1 of work's INBOX as the daemon does: waiting, with its original's Message-ID.
    fn reply_waiting(
        paths: &Paths,
        _: &Config,
        request: SendRequest,
    ) -> Result<Queued, QueueError> {
        use postvak_core::send::outbox::Original;
        use postvak_core::send::reply::OriginalFlag;
        let envelope = Envelope {
            from: "work@example.com".into(),
            to: vec!["sender1@example.com".into()],
        };
        let original = Original {
            message_id: "<1@example.com>".into(),
            flag: OriginalFlag::Answered,
        };
        let entry = Entry::new(envelope, 0, false, Some(original));
        let outbox = Outbox::open(paths, "work").map_err(|e| e.to_string())?;
        outbox.add(b"raw", &entry).map_err(|e| e.to_string())?;
        Ok(Queued {
            id: entry.id,
            ..queued(paths, &Config::default(), request)?
        })
    }

    fn pending(driver: &Driver) -> Vec<(u32, bool)> {
        let rows = driver.app.list.rows.iter();
        rows.map(|row| (row.uid, row.pending)).collect()
    }

    #[test]
    fn a_reply_waiting_marks_its_original() {
        let Replying { mut driver, .. } = replying(&[1, 2]);
        driver.app.queue_send = reply_waiting;
        driver.press("j");
        assert_eq!(driver.app.list.rows[driver.app.list.cursor].uid, 1);
        open(&mut driver, "r");
        send(&mut driver, "Thanks");
        assert_eq!(pending(&driver), [(2, false), (1, false)]);
        let at = driver.now + POLL;
        driver.at(at);
        assert_eq!(pending(&driver), [(2, false), (1, true)]);
    }

    #[test]
    fn the_mark_matches_the_message_id_as_sync_and_send_read_it() {
        use postvak_core::message::parse_headers;
        use postvak_core::send::outbox::Original;
        use postvak_core::send::reply::OriginalFlag;
        let headers = b"Message-ID: <Q1.x@Example.org>\r\nSubject: question\r\n\r\n".to_vec();
        let fx = Fixture::new(&["work"]);
        let mut stored = message("INBOX", 1, "question");
        // What sync stores, and what `send::compose` puts in the outbox for a reply to it.
        stored.message_id = parse_headers(&headers).message_id;
        stored.headers = headers.clone();
        fx.add("work", stored);
        fx.add("work", message("INBOX", 2, "other"));
        let original = Original {
            message_id: parse_headers(&headers).message_id.unwrap(),
            flag: OriginalFlag::Answered,
        };
        let envelope = Envelope {
            from: "work@example.com".into(),
            to: vec!["sender1@example.com".into()],
        };
        let entry = Entry::new(envelope, 0, false, Some(original));
        Outbox::open(&fx.paths, "work")
            .unwrap()
            .add(b"raw", &entry)
            .unwrap();
        let (mut driver, _wires) = fx.driver();
        driver.at(POLL);
        assert_eq!(pending(&driver), [(2, false), (1, true)]);
    }

    #[test]
    fn the_mark_goes_once_sent() {
        let Replying { mut driver, fx, .. } = replying(&[1, 2]);
        driver.app.queue_send = reply_waiting;
        driver.press("j");
        open(&mut driver, "r");
        send(&mut driver, "Thanks");
        let (_, id) = driver.app.taken.last().unwrap().outbox.clone().unwrap();
        outbox_state(&mut driver, &fx, Some(State::Sending));
        assert_eq!(
            pending(&driver),
            [(2, false), (1, true)],
            "still on its way"
        );
        let outbox = Outbox::open(&fx.paths, "work").unwrap();
        outbox.set_state(&id, State::Sent).unwrap();
        let at = driver.now + POLL;
        driver.at(at);
        assert_eq!(pending(&driver), [(2, false), (1, false)]);
    }

    /// Work's outbox with one message waiting and one that failed, read by the file poll.
    fn two_in_the_outbox() -> (Fixture, Driver, Wires, String) {
        let fx = Fixture::new(&["work"]);
        let outbox = Outbox::open(&fx.paths, "work").unwrap();
        let envelope = |to: &[&str]| Envelope {
            from: "work@example.com".into(),
            to: to.iter().map(|to| to.to_string()).collect(),
        };
        let waiting = Entry::new(envelope(&["a@example.org"]), 1_790_000_000, false, None);
        outbox.add(b"raw", &waiting).unwrap();
        let to = envelope(&["b@example.org", "c@example.org"]);
        let mut failed = Entry::new(to, 0, false, None);
        failed.state = State::Failed {
            reason: "550 no such user".into(),
            retrying: false,
        };
        outbox.add(b"raw", &failed).unwrap();
        let (mut driver, wires) = fx.driver();
        driver.at(POLL);
        (fx, driver, wires, failed.id)
    }

    #[test]
    fn the_outbox_dialog_lists_waiting_and_failed() {
        let (_fx, mut driver, _wires, failed) = two_in_the_outbox();
        assert!(!driver.app.dialog_open(Dialog::Outbox));
        driver.apply(UiAction::OpenOutbox);
        assert_eq!(driver.app.top_dialog(), Some(Dialog::Outbox));
        let mut rows = crate::status::outbox_rows(&driver.app);
        rows.sort_by_key(|row| row.retry.is_some());
        let at = postvak_core::time::clock(1_790_000_000);
        assert_eq!(
            rows,
            [
                crate::status::OutboxRow {
                    title: "work · 1 recipient".into(),
                    state: format!("Goes at {at}"),
                    failed: false,
                    retry: None,
                },
                crate::status::OutboxRow {
                    title: "work · 2 recipients".into(),
                    state: "Failed: 550 no such user".into(),
                    failed: true,
                    retry: Some((0, failed)),
                },
            ]
        );
        driver.press("escape");
        assert!(!driver.app.dialog_open(Dialog::Outbox));
    }

    #[test]
    fn the_status_bars_outbox_line_opens_the_outbox() {
        let fx = Fixture::new(&["work"]);
        let outbox = Outbox::open(&fx.paths, "work").unwrap();
        let envelope = Envelope {
            from: "work@example.com".into(),
            to: vec!["a@example.org".into()],
        };
        let mut failed = Entry::new(envelope, 0, false, None);
        failed.state = State::Failed {
            reason: "550 no such user".into(),
            retrying: false,
        };
        outbox.add(b"raw", &failed).unwrap();
        let (open, _wires) = fx.window();
        open.advance(POLL);
        open.tick();
        open.click("1 message failed");
        assert!(open.with(|live| live.app.dialog_open(Dialog::Outbox)));
        assert!(open.with(|live| !live.app.history_open));
        assert_eq!(open.find("Retry").len(), 1);
    }

    #[test]
    fn retry_sends_a_failed_one_again() {
        static RETRIED: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());
        // Held until the first check, so the stub's answer cannot re-read the outbox before it.
        static UNANSWERED: Mutex<()> = Mutex::new(());
        let unanswered = UNANSWERED.lock().unwrap();
        let (_fx, mut driver, _wires, failed) = two_in_the_outbox();
        driver.app.retry_send = |_, account, id| {
            let _answered = UNANSWERED.lock().unwrap();
            RETRIED.lock().unwrap().push((account.into(), id.into()));
            Ok(())
        };
        driver.apply(UiAction::OpenOutbox);
        driver.apply(UiAction::RetrySend(0, failed.clone()));
        let offered = |app: &App| {
            let rows = crate::status::outbox_rows(app);
            rows.iter().any(|row| row.retry.is_some())
        };
        assert!(!offered(&driver.app), "Retry goes once clicked");
        drop(unanswered);
        driver.tick_until(|app| app.retrying.is_empty());
        assert!(
            offered(&driver.app),
            "the outbox, read again, still has it failed"
        );
        assert_eq!(
            RETRIED.lock().unwrap().clone(),
            [("work".to_string(), failed)]
        );
        assert_eq!(driver.app.error, None);

        driver.app.retry_send = |_, _, _| Err("the daemon is stopping".into());
        driver.apply(UiAction::RetrySend(0, "0000abcd".into()));
        driver.tick_until(|app| app.retrying.is_empty());
        assert_eq!(
            driver.app.error.as_deref(),
            Some("could not retry: the daemon is stopping")
        );
    }

    mod outbox_edit {
        use std::path::PathBuf;

        use postvak_core::send::drafts;

        use super::*;

        const FAILED: &[u8] = b"From: work@example.com\r\nTo: ann@x.org\r\nBcc: boss@x.org\r\nSubject: Lunch\r\nMessage-ID: <o1@example.com>\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nNoon?\r\n";

        /// What the fakes were asked, in order, by whose home: `save <replaces>` or `cancel <id>`.
        static ASKED: Mutex<Vec<(PathBuf, String)>> = Mutex::new(Vec::new());

        fn asked(fx: &Fixture) -> Vec<String> {
            let asked = ASKED.lock().unwrap();
            let home = fx.paths.config_file();
            (asked.iter())
                .filter(|(of, _)| *of == home)
                .map(|(_, what)| what.clone())
                .collect()
        }

        /// Work with a Drafts folder and FAILED failed in its outbox, a copy `<d0@example.com>` in Drafts.
        fn failed_in_the_outbox() -> (Fixture, Driver, Wires, String) {
            let fx = Fixture::new(&["work"]);
            fx.folder("work", "Drafts", Some("Drafts"));
            let envelope = Envelope {
                from: "work@example.com".into(),
                to: vec!["ann@x.org".into(), "boss@x.org".into()],
            };
            let mut failed = Entry::new(envelope, 0, false, None);
            failed.state = State::Failed {
                reason: "550 no such user".into(),
                retrying: false,
            };
            failed.draft = vec!["<d0@example.com>".into()];
            Outbox::open(&fx.paths, "work")
                .unwrap()
                .add(FAILED, &failed)
                .unwrap();
            let (mut driver, wires) = fx.driver();
            driver.at(POLL);
            driver.app.save_raw_draft = saved_as_draft;
            driver.app.cancel_send = |paths, account, id| {
                ASKED
                    .lock()
                    .unwrap()
                    .push((paths.config_file(), format!("cancel {id}")));
                Outbox::open(paths, account).unwrap().cancel(id).unwrap();
                Ok(())
            };
            driver.apply(UiAction::OpenOutbox);
            (fx, driver, wires, failed.id)
        }

        /// The fake save: records a delete, or a save of FAILED under a new Message-ID as `save <replaces> as <id>`.
        fn saved_as_draft(
            paths: &Paths,
            account: &str,
            raw: Option<Vec<u8>>,
            replaces: Vec<String>,
        ) -> Result<SavedDraft, String> {
            assert_eq!(account, "work");
            let replaces = replaces.join(",");
            let (what, message_id) = match raw {
                Some(raw) => {
                    let id = postvak_core::message::parse_headers(&raw)
                        .message_id
                        .unwrap();
                    let id = format!("<{id}>");
                    assert_ne!(id, "<o1@example.com>", "a new Message-ID");
                    assert!(id.ends_with("@example.com>"), "{id}");
                    assert_eq!(raw, drafts::with_message_id(FAILED, &id));
                    (format!("save {replaces} as {id}"), Some(id))
                }
                None => (format!("delete {replaces}"), None),
            };
            ASKED.lock().unwrap().push((paths.config_file(), what));
            let left = message_id
                .iter()
                .map(|_| "<d0@example.com>".into())
                .collect();
            Ok(SavedDraft { message_id, left })
        }

        /// The Message-ID the fake saved FAILED as.
        fn saved_id(fx: &Fixture) -> String {
            let asked = asked(fx);
            let save = asked.iter().find_map(|what| what.split_once(" as "));
            save.unwrap().1.to_string()
        }

        fn edit(driver: &mut Driver, id: &str) {
            driver.apply(UiAction::EditOutbox(0, id.into()));
            assert!(
                crate::status::outbox_rows(&driver.app)[0].retry.is_none(),
                "Retry and Edit go while it is saved"
            );
            driver.tick_until(|app| app.outbox_editing.is_empty());
        }

        #[test]
        fn edit_turns_a_failed_message_into_a_draft_and_opens_it() {
            let (fx, mut driver, _wires, id) = failed_in_the_outbox();
            edit(&mut driver, &id);
            let saved = saved_id(&fx);
            assert_eq!(
                asked(&fx),
                [
                    format!("save <d0@example.com> as {saved}"),
                    format!("cancel {id}")
                ]
            );
            let window = &driver.app.new_mail[0];
            assert_eq!(
                (window.to.as_str(), window.subject.as_str()),
                ("ann@x.org", "Lunch")
            );
            assert_eq!(window.bcc, "boss@x.org");
            assert!(window.text.contains("Noon?"));
            let draft = window.draft.as_ref().unwrap();
            assert_eq!(draft.message_id, saved, "it opens from the bytes saved");
            assert_eq!(draft.left, ["<d0@example.com>"]);
            assert!(
                Outbox::open(&fx.paths, "work")
                    .unwrap()
                    .list()
                    .unwrap()
                    .is_empty()
            );
            assert!(crate::status::outbox_rows(&driver.app).is_empty());
            assert_eq!(driver.app.error, None);
        }

        #[test]
        fn a_refused_draft_save_leaves_the_failed_message_in_the_outbox() {
            let (fx, mut driver, _wires, id) = failed_in_the_outbox();
            driver.app.save_raw_draft =
                |_, _, _, _| Err("this account has no Drafts folder".into());
            edit(&mut driver, &id);
            assert!(asked(&fx).is_empty(), "nothing is cancelled");
            assert!(driver.app.new_mail.is_empty() && driver.app.composer.is_none());
            assert_eq!(
                Outbox::open(&fx.paths, "work")
                    .unwrap()
                    .list()
                    .unwrap()
                    .len(),
                1
            );
            let rows = crate::status::outbox_rows(&driver.app);
            assert_eq!(
                rows[0].state,
                "Failed: 550 no such user · Not saved as a draft: this account has no Drafts folder"
            );
            assert_eq!(rows[0].retry, Some((0, id.clone())));
            driver.app.retry_send = |_, _, _| Ok(());
            driver.apply(UiAction::RetrySend(0, id));
            let rows = crate::status::outbox_rows(&driver.app);
            assert_eq!(
                rows[0].state, "Failed: 550 no such user",
                "Retry clears why"
            );
        }

        /// The fake save: deletes only, and refuses that.
        fn refusing_deletes(
            paths: &Paths,
            account: &str,
            raw: Option<Vec<u8>>,
            replaces: Vec<String>,
        ) -> Result<SavedDraft, String> {
            match raw {
                Some(_) => saved_as_draft(paths, account, raw, replaces),
                None => Err("the connection dropped".into()),
            }
        }

        #[test]
        fn a_too_late_cancel_removes_the_saved_draft_and_opens_nothing() {
            let (fx, mut driver, _wires, id) = failed_in_the_outbox();
            driver.app.cancel_send = |paths, account, id| {
                let sending = State::Sending;
                Outbox::open(paths, account)
                    .unwrap()
                    .set_state(id, sending)
                    .unwrap();
                Err(CancelError::TooLate)
            };
            edit(&mut driver, &id);
            let saved = saved_id(&fx);
            assert_eq!(
                asked(&fx),
                [
                    format!("save <d0@example.com> as {saved}"),
                    format!("delete {saved}")
                ]
            );
            assert!(driver.app.new_mail.is_empty() && driver.app.composer.is_none());
            let rows = crate::status::outbox_rows(&driver.app);
            assert_eq!(rows[0].state, "Sending… · It is on its way already");
            assert_eq!(rows[0].retry, None);
        }

        #[test]
        fn a_refused_cancel_removes_the_saved_draft_and_keeps_the_entry() {
            let (fx, mut driver, _wires, id) = failed_in_the_outbox();
            driver.app.cancel_send =
                |_, _, _| Err(CancelError::Failed("the daemon is stopping".into()));
            edit(&mut driver, &id);
            let saved = saved_id(&fx);
            assert_eq!(
                asked(&fx),
                [
                    format!("save <d0@example.com> as {saved}"),
                    format!("delete {saved}")
                ]
            );
            assert!(driver.app.new_mail.is_empty() && driver.app.composer.is_none());
            let rows = crate::status::outbox_rows(&driver.app);
            assert_eq!(
                rows[0].state,
                "Failed: 550 no such user · Not taken out of the outbox: the daemon is stopping"
            );
            assert_eq!(rows[0].retry, Some((0, id.clone())));
            let outbox = Outbox::open(&fx.paths, "work").unwrap();
            outbox.set_state(&id, State::Waiting).unwrap();
            let at = driver.now + POLL;
            driver.at(at);
            let rows = crate::status::outbox_rows(&driver.app);
            assert!(
                rows[0].state.starts_with("Goes at "),
                "a new state clears why"
            );
            assert!(!rows[0].state.contains(" · "));
        }

        #[test]
        fn a_draft_copy_that_cannot_be_removed_is_named() {
            let (_fx, mut driver, _wires, id) = failed_in_the_outbox();
            driver.app.save_raw_draft = refusing_deletes;
            driver.app.cancel_send =
                |_, _, _| Err(CancelError::Failed("the daemon is stopping".into()));
            edit(&mut driver, &id);
            assert_eq!(
                crate::status::outbox_rows(&driver.app)[0].state,
                "Failed: 550 no such user · Not taken out of the outbox: the daemon is stopping, \
                 and the draft copy could not be removed; it is in Drafts"
            );
        }
    }

    mod drafts {
        use std::path::PathBuf;
        use std::sync::mpsc::{Sender, channel};

        use super::*;
        use crate::compose::{ComposeDraft, load_recoveries, recovery_dir};
        use crate::test_support::wait_until;

        /// One save asked of the faked daemon: by whose home, what it saves (None: only deletes), and the answer it waits for.
        struct Call {
            home: PathBuf,
            account: String,
            text: Option<String>,
            message_id: Option<String>,
            replaces: Vec<String>,
            answer: Sender<Result<SavedDraft, String>>,
        }

        static CALLS: Mutex<Vec<Call>> = Mutex::new(Vec::new());

        /// Waits on its worker until the test answers.
        fn held(
            paths: &Paths,
            _: &Config,
            account: &str,
            request: Option<(SendRequest, String)>,
            replaces: Vec<String>,
        ) -> Result<SavedDraft, String> {
            let (answer, answered) = channel();
            let (text, message_id) = request
                .map(|(request, message_id)| match request.body {
                    Body::Given(text) => (text, message_id),
                    _ => (String::new(), message_id),
                })
                .unzip();
            CALLS.lock().unwrap().push(Call {
                home: paths.state_dir.clone(),
                account: account.into(),
                text,
                message_id,
                replaces,
                answer,
            });
            answered.recv().unwrap_or(Err("the test ended".into()))
        }

        fn take_call(fx: &Fixture) -> Option<Call> {
            let mut calls = CALLS.lock().unwrap();
            let index = calls.iter().position(|c| c.home == fx.paths.state_dir)?;
            Some(calls.remove(index))
        }

        /// Waits for the window's next save.
        fn asked(driver: &mut Driver, fx: &Fixture) -> Call {
            let mut call = None;
            wait_until(|| {
                driver.tick();
                call = take_call(fx);
                call.is_some()
            });
            let call = call.unwrap();
            assert_eq!(call.account, "work");
            call
        }

        fn saved(id: &str, left: &[&str]) -> Result<SavedDraft, String> {
            Ok(SavedDraft {
                message_id: Some(id.into()),
                left: left.iter().map(|id| id.to_string()).collect(),
            })
        }

        /// Answers `call` and waits until the window took the answer.
        fn answer(driver: &mut Driver, call: Call, result: Result<SavedDraft, String>) {
            call.answer.send(result).unwrap();
            driver
                .tick_until(|app| app.compose_saves.is_empty() && app.compose_deleting.is_empty());
        }

        fn drafting(uids: &[u32]) -> Replying {
            let mut replying = replying_with(uids, Some("Drafts"));
            replying.driver.app.save_draft = held;
            replying
        }

        fn write(driver: &mut Driver, target: ComposeTarget, text: &str) {
            driver.apply(UiAction::ComposeEdit(
                target,
                ComposeField::Text,
                text.into(),
            ));
        }

        fn composer(driver: &Driver) -> &Composer {
            driver.app.composer.as_ref().unwrap()
        }

        /// Types `text` into the composer at `target`, saves it as `id` and waits for the answer.
        fn save(
            driver: &mut Driver,
            fx: &Fixture,
            target: ComposeTarget,
            text: &str,
            id: &str,
            left: &[&str],
        ) {
            write(driver, target, text);
            driver.apply(UiAction::ComposeSaveDraft(target));
            let call = asked(driver, fx);
            answer(driver, call, saved(id, left));
        }

        fn new_mail(driver: &mut Driver) -> ComposeTarget {
            driver.apply(UiAction::NewMail(None));
            let target = ComposeTarget::Window(driver.app.new_mail.last().unwrap().id);
            driver.apply(UiAction::ComposeEdit(
                target,
                ComposeField::Subject,
                "Plans".into(),
            ));
            target
        }

        #[test]
        fn save_draft_sends_the_built_draft_and_marks_it_saved() {
            let Replying { mut driver, fx, .. } = drafting(&[1]);
            open(&mut driver, "r");
            write(&mut driver, PANE, "Hi");
            driver.apply(UiAction::ComposeSaveDraft(PANE));
            let call = asked(&mut driver, &fx);
            assert_eq!(call.text.as_deref(), Some("Hi"));
            assert!(call.replaces.is_empty());
            assert!(composer(&driver).saving);

            answer(&mut driver, call, saved("<d1@draft>", &[]));
            let composer = composer(&driver);
            let draft = ComposeDraft {
                message_id: "<d1@draft>".into(),
                left: Vec::new(),
            };
            assert_eq!(composer.draft, Some(draft));
            assert!(!composer.saving);
            assert!(!composer.changed(), "saved, so leaving asks nothing");
            assert_eq!(toast(&driver), Some(("Draft saved", false)));
        }

        #[test]
        fn without_a_drafts_folder_save_says_so_and_names_no_copy() {
            let Replying { mut driver, fx, .. } = replying(&[1]);
            driver.app.save_draft = held;
            open(&mut driver, "r");
            write(&mut driver, PANE, "Hi");
            driver.apply(UiAction::ComposeSaveDraft(PANE));
            driver.tick();
            assert!(take_call(&fx).is_none());
            let composer = composer(&driver);
            assert_eq!(composer.draft, None, "Discard asks nothing about a draft");
            let error = composer.error.as_ref().map(|e| e.1.as_str());
            assert_eq!(error, Some("This account has no Drafts folder"));
        }

        #[test]
        fn an_unchanged_composer_saving_keeps_its_file_until_saved() {
            let Replying { mut driver, fx, .. } = drafting(&[1]);
            open(&mut driver, "r");
            driver.apply(UiAction::ComposeSaveDraft(PANE));
            let call = asked(&mut driver, &fx);
            let kept = load_recoveries(&recovery_dir(&fx.paths));
            let pending = kept.first().and_then(|c| c.draft.clone());
            assert!(
                pending.is_some(),
                "a crash now still knows the copy's Message-ID"
            );

            answer(&mut driver, call, saved("<d1@draft>", &[]));
            assert!(load_recoveries(&recovery_dir(&fx.paths)).is_empty());
        }

        #[test]
        fn a_second_save_replaces_the_first_copy() {
            let Replying { mut driver, fx, .. } = drafting(&[1]);
            open(&mut driver, "r");
            save(&mut driver, &fx, PANE, "Hi", "<d1@draft>", &["<old@draft>"]);
            write(&mut driver, PANE, "Hi again");
            driver.apply(UiAction::ComposeSaveDraft(PANE));
            let call = asked(&mut driver, &fx);
            assert_eq!(call.text.as_deref(), Some("Hi again"));
            assert_eq!(call.replaces, ["<d1@draft>", "<old@draft>"]);
            answer(&mut driver, call, saved("<d2@draft>", &[]));
        }

        #[test]
        fn a_save_while_one_is_in_flight_waits_for_it() {
            let Replying { mut driver, fx, .. } = drafting(&[1]);
            open(&mut driver, "r");
            write(&mut driver, PANE, "Hi");
            driver.apply(UiAction::ComposeSaveDraft(PANE));
            let first = asked(&mut driver, &fx);
            write(&mut driver, PANE, "Hi there");
            driver.apply(UiAction::ComposeSaveDraft(PANE));
            driver.tick();
            assert!(take_call(&fx).is_none(), "one save at a time");

            first.answer.send(saved("<d1@draft>", &[])).unwrap();
            let second = asked(&mut driver, &fx);
            assert_eq!(second.text.as_deref(), Some("Hi there"));
            assert_eq!(second.replaces, ["<d1@draft>"]);
            let kept = load_recoveries(&recovery_dir(&fx.paths));
            let draft = kept
                .first()
                .and_then(|c| c.draft.clone())
                .map(|d| d.message_id);
            assert_eq!(
                draft.as_deref(),
                Some("<d1@draft>"),
                "typed during the save, its file names the saved draft at once"
            );
            answer(&mut driver, second, saved("<d2@draft>", &[]));
        }

        #[test]
        fn a_refused_save_frees_the_composer() {
            let Replying { mut driver, fx, .. } = drafting(&[1]);
            open(&mut driver, "r");
            write(&mut driver, PANE, "Hi");
            driver.apply(UiAction::ComposeSaveDraft(PANE));
            let call = asked(&mut driver, &fx);
            let pending = call.message_id.clone().unwrap();
            answer(
                &mut driver,
                call,
                Err("This account has no Drafts folder".into()),
            );
            let composer = composer(&driver);
            assert_eq!(composer.text, "Hi");
            let draft = ComposeDraft {
                message_id: pending,
                left: Vec::new(),
            };
            assert_eq!(composer.draft, Some(draft), "it may be on the server");
            assert!(!composer.saving);
            assert!(composer.changed());
            let error = Some((Field::Send, "This account has no Drafts folder".to_string()));
            assert_eq!(composer.error, error);
            driver.at(6.0);
            assert_eq!(
                load_recoveries(&recovery_dir(&fx.paths)).len(),
                1,
                "its file stays"
            );

            driver.apply(UiAction::ComposeClose(PANE));
            assert_eq!(
                driver.app.top_dialog(),
                Some(Dialog::LeaveCompose),
                "Close asks again"
            );
            driver.apply(UiAction::LeaveComposeCancel(PANE));
            driver.apply(UiAction::ComposeSend(PANE));
            driver.tick_until(|app| app.composer.is_none());
        }

        #[test]
        fn an_unrelated_refusal_during_a_save_is_not_the_saves() {
            let Replying {
                mut driver,
                wires,
                fx,
            } = drafting(&[1]);
            open(&mut driver, "r");
            write(&mut driver, PANE, "Hi");
            driver.apply(UiAction::ComposeSaveDraft(PANE));
            let call = asked(&mut driver, &fx);
            let refused = Event::CommandFailed {
                account: "work".into(),
                request: 0,
                message: "work is offline".into(),
            };
            driver.receive(&wires, [refused]);
            assert!(composer(&driver).saving);
            assert_eq!(composer(&driver).error, None);

            answer(&mut driver, call, saved("<d1@draft>", &[]));
            assert!(composer(&driver).draft.is_some());
        }

        #[test]
        fn a_save_clears_its_earlier_error_but_not_the_may_have_gone_note() {
            let Replying { mut driver, fx, .. } = drafting(&[1]);
            open(&mut driver, "r");
            write(&mut driver, PANE, "Hi");
            driver.apply(UiAction::ComposeSaveDraft(PANE));
            let call = asked(&mut driver, &fx);
            answer(&mut driver, call, Err("work is offline".into()));
            save(&mut driver, &fx, PANE, "Hi", "<d1@draft>", &[]);
            assert_eq!(composer(&driver).error, None);

            let note = (Field::Send, MAY_HAVE_GONE.to_string());
            if let Some(composer) = driver.app.composer.as_mut() {
                composer.may_have_gone = true;
                composer.error = Some(note.clone());
            }
            save(&mut driver, &fx, PANE, "Hi again", "<d2@draft>", &[]);
            assert_eq!(composer(&driver).error, Some(note));
            assert!(composer(&driver).may_have_gone);
        }

        #[test]
        fn the_leave_question_offers_save_draft_and_goes_on_after_it() {
            let Replying { mut driver, fx, .. } = drafting(&[1, 2]);
            open(&mut driver, "r");
            write(&mut driver, PANE, "Hi");
            let cursor = driver.app.list.cursor;
            driver.press("j");
            assert_eq!(
                driver.app.leave_question(PANE),
                Some(("Discard this message?", true))
            );
            driver.apply(UiAction::LeaveComposeSave(PANE));
            assert_eq!(driver.app.top_dialog(), None);
            assert_eq!(driver.app.list.cursor, cursor, "held until it is saved");
            let call = asked(&mut driver, &fx);
            answer(&mut driver, call, saved("<d1@draft>", &[]));
            assert!(driver.app.composer.is_none());
            assert_eq!(driver.app.list.cursor, cursor + 1);

            open(&mut driver, "r");
            write(&mut driver, PANE, "Hi");
            driver.press("k");
            driver.apply(UiAction::LeaveComposeSave(PANE));
            let call = asked(&mut driver, &fx);
            answer(
                &mut driver,
                call,
                Err("This account has no Drafts folder".into()),
            );
            assert!(driver.app.composer.is_some(), "a failed save stays open");
            assert_eq!(driver.app.list.cursor, cursor + 1);
        }

        #[test]
        fn a_new_mail_windows_leave_question_saves_then_closes_it() {
            let Replying { mut driver, fx, .. } = drafting(&[1]);
            let window = new_mail(&mut driver);
            driver.apply(UiAction::ComposeClose(window));
            assert_eq!(
                driver.app.leave_question(window),
                Some(("Discard this message?", true))
            );
            driver.apply(UiAction::LeaveComposeSave(window));
            assert_eq!(driver.app.leave_question(window), None);
            assert_eq!(driver.app.new_mail.len(), 1, "open until it is saved");
            let call = asked(&mut driver, &fx);
            answer(&mut driver, call, saved("<d1@draft>", &[]));
            assert!(driver.app.new_mail.is_empty());
        }

        #[test]
        fn sending_a_saved_draft_names_it_and_undo_keeps_it() {
            static SENT: Mutex<Vec<SendRequest>> = Mutex::new(Vec::new());
            let Replying { mut driver, fx, .. } = drafting(&[1]);
            driver.app.queue_send = |paths, config, request| {
                SENT.lock().unwrap().push(request.clone());
                queued(paths, config, request)
            };
            open(&mut driver, "r");
            save(&mut driver, &fx, PANE, "Hi", "<d1@draft>", &["<old@draft>"]);
            send(&mut driver, "Hi there");
            let sent = SENT.lock().unwrap().clone();
            assert_eq!(sent[0].draft, ["<d1@draft>", "<old@draft>"]);

            driver.apply(UiAction::Undo);
            driver.tick_until(|app| app.composer.is_some());
            let draft = ComposeDraft {
                message_id: "<d1@draft>".into(),
                left: vec!["<old@draft>".into()],
            };
            assert_eq!(composer(&driver).draft, Some(draft));
        }

        #[test]
        fn discarding_a_saved_draft_asks_and_deletes_it() {
            let Replying { mut driver, fx, .. } = drafting(&[1]);
            open(&mut driver, "r");
            save(&mut driver, &fx, PANE, "Hi", "<d1@draft>", &[]);
            driver.apply(UiAction::ComposeDiscard(PANE));
            assert_eq!(
                driver.app.leave_question(PANE),
                Some(("Delete this draft?", false))
            );
            driver.apply(UiAction::LeaveComposeDiscard(PANE));
            assert!(driver.app.composer.is_none());
            let call = asked(&mut driver, &fx);
            assert_eq!(
                (call.text.as_deref(), call.replaces.as_slice()),
                (None, &["<d1@draft>".to_string()][..])
            );
            answer(&mut driver, call, Err("work is offline".into()));
            assert_eq!(
                driver.app.error.as_deref(),
                Some("could not delete the draft: work is offline")
            );

            let window = new_mail(&mut driver);
            save(&mut driver, &fx, window, "Hi", "<d2@draft>", &[]);
            driver.apply(UiAction::ComposeDiscard(window));
            assert_eq!(
                driver.app.leave_question(window),
                Some(("Delete this draft?", false))
            );
            driver.apply(UiAction::LeaveComposeDiscard(window));
            assert!(driver.app.new_mail.is_empty());
            let call = asked(&mut driver, &fx);
            assert_eq!(
                (call.text.as_deref(), call.replaces.as_slice()),
                (None, &["<d2@draft>".to_string()][..])
            );
            answer(&mut driver, call, Ok(SavedDraft::default()));
        }

        #[test]
        fn each_save_names_a_new_message_id_on_the_from_domain() {
            let Replying { mut driver, fx, .. } = drafting(&[1]);
            open(&mut driver, "r");
            write(&mut driver, PANE, "Hi");
            driver.apply(UiAction::ComposeSaveDraft(PANE));
            let first = asked(&mut driver, &fx);
            let id = first.message_id.clone().unwrap();
            assert!(id.starts_with('<') && id.ends_with("@example.com>"), "{id}");
            answer(&mut driver, first, saved(&id, &[]));
            driver.apply(UiAction::ComposeSaveDraft(PANE));
            let second = asked(&mut driver, &fx);
            assert_ne!(second.message_id, Some(id));
            answer(&mut driver, second, saved("<d2@draft>", &[]));
        }

        #[test]
        fn a_failed_save_leaves_its_copy_for_the_next_save_to_replace() {
            let Replying { mut driver, fx, .. } = drafting(&[1]);
            open(&mut driver, "r");
            save(&mut driver, &fx, PANE, "Hi", "<d1@draft>", &[]);
            write(&mut driver, PANE, "Hi again");
            driver.apply(UiAction::ComposeSaveDraft(PANE));
            let call = asked(&mut driver, &fx);
            assert_eq!(call.replaces, ["<d1@draft>"]);
            let pending = call.message_id.clone().unwrap();
            answer(&mut driver, call, Err("the daemon did not answer".into()));
            driver.apply(UiAction::ComposeSaveDraft(PANE));
            let call = asked(&mut driver, &fx);
            assert_eq!(call.replaces, ["<d1@draft>".to_string(), pending]);
            answer(&mut driver, call, saved("<d3@draft>", &[]));
        }

        #[test]
        fn a_save_in_flight_names_its_copy_in_the_recovery_file() {
            let Replying { mut driver, fx, .. } = drafting(&[1]);
            open(&mut driver, "r");
            save(&mut driver, &fx, PANE, "Hi", "<d1@draft>", &[]);
            write(&mut driver, PANE, "Hi again");
            driver.apply(UiAction::ComposeSaveDraft(PANE));
            let call = asked(&mut driver, &fx);
            let pending = call.message_id.clone().unwrap();
            drop(driver);
            drop(call);

            let (restarted, _wires) = fx.driver();
            let pane = restarted.app.composer.as_ref().unwrap();
            assert_eq!(pane.text, "Hi again");
            assert_eq!(
                pane.draft.as_ref().map(ComposeDraft::ids),
                Some(vec!["<d1@draft>".to_string(), pending])
            );
        }

        #[test]
        fn a_save_closes_the_leave_question_and_discard_waits_for_it() {
            let Replying { mut driver, fx, .. } = drafting(&[1, 2]);
            open(&mut driver, "r");
            write(&mut driver, PANE, "Hi");
            driver.press("j");
            assert!(driver.app.leave_question(PANE).is_some());
            driver.apply(UiAction::ComposeSaveDraft(PANE));
            assert_eq!(driver.app.leave_question(PANE), None);
            driver.apply(UiAction::LeaveComposeDiscard(PANE));
            assert!(driver.app.composer.is_some(), "kept until the save answers");
            let call = asked(&mut driver, &fx);
            answer(&mut driver, call, saved("<d1@draft>", &[]));

            let window = new_mail(&mut driver);
            driver.apply(UiAction::ComposeDiscard(window));
            assert!(driver.app.leave_question(window).is_some());
            driver.apply(UiAction::ComposeSaveDraft(window));
            assert_eq!(driver.app.leave_question(window), None);
            driver.apply(UiAction::LeaveComposeDiscard(window));
            assert_eq!(driver.app.new_mail.len(), 1, "kept until the save answers");
            let call = asked(&mut driver, &fx);
            answer(&mut driver, call, saved("<d2@draft>", &[]));
        }

        #[test]
        fn save_draft_on_the_leave_question_keeps_it_when_no_save_starts() {
            let Replying { mut driver, fx, .. } = drafting(&[1, 2]);
            open(&mut driver, "r");
            write(&mut driver, PANE, "Hi");
            driver.press("j");
            driver.app.composer.as_mut().unwrap().loading = true;
            driver.apply(UiAction::LeaveComposeSave(PANE));
            assert!(driver.app.leave_question(PANE).is_some());
            driver.tick();
            assert!(take_call(&fx).is_none());
        }

        #[test]
        fn a_copy_a_discard_left_in_drafts_is_noted() {
            let Replying { mut driver, fx, .. } = drafting(&[1]);
            open(&mut driver, "r");
            save(&mut driver, &fx, PANE, "Hi", "<d1@draft>", &[]);
            driver.apply(UiAction::ComposeDiscard(PANE));
            driver.apply(UiAction::LeaveComposeDiscard(PANE));
            let call = asked(&mut driver, &fx);
            let left = SavedDraft {
                message_id: None,
                left: vec!["<d1@draft>".into()],
            };
            answer(&mut driver, call, Ok(left));
            assert_eq!(
                driver.app.error.as_deref(),
                Some("could not delete the draft: a copy is still in Drafts")
            );
        }

        #[test]
        fn close_leaves_a_saved_draft() {
            let Replying { mut driver, fx, .. } = drafting(&[1]);
            open(&mut driver, "r");
            save(&mut driver, &fx, PANE, "Hi", "<d1@draft>", &[]);
            driver.apply(UiAction::ComposeClose(PANE));
            assert!(driver.app.composer.is_none());
            assert_eq!(driver.app.top_dialog(), None);
            driver.tick();
            assert!(take_call(&fx).is_none());
        }

        #[test]
        fn ctrl_s_saves() {
            use slint::platform::{Key, WindowEvent};
            let fx = Fixture::new(&["work"]);
            fx.add("work", message("INBOX", 1, "subject 1"));
            fx.folder("work", "Drafts", Some("Drafts"));
            let (open, _wires) = fx.window();
            open.with(|live| {
                live.app.prefill = filled;
                live.app.save_draft = held;
            });
            open.press("r");
            open.tick_until(|open| open.with(|live| filled_in(&live.app)));
            assert_eq!(open.find("Save draft").len(), 1);
            open.tap("Your text");
            open.get("Your text").set_accessible_value("Hi");
            let window = open.window();
            let control: slint::SharedString = Key::Control.into();
            for event in [
                WindowEvent::KeyPressed {
                    text: control.clone(),
                },
                WindowEvent::KeyPressed { text: "s".into() },
                WindowEvent::KeyReleased { text: "s".into() },
                WindowEvent::KeyReleased { text: control },
            ] {
                window.window().dispatch_event(event);
            }
            let mut call = None;
            wait_until(|| {
                open.tick();
                call = take_call(&fx);
                call.is_some()
            });
            let call = call.unwrap();
            assert_eq!(call.text.as_deref(), Some("Hi"));
            call.answer.send(saved("<d1@draft>", &[])).unwrap();
            open.tick_until(|open| open.with(|live| live.app.compose_saves.is_empty()));
        }
    }

    mod attach {
        use std::cell::RefCell;
        use std::path::PathBuf;

        use super::*;

        thread_local! {
            /// What the faked file dialog answers on this test's thread, where the window runs.
            static CHOSEN: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) };
            /// The faked dialog still open, whose answer the test sends when it wants.
            static OPEN: RefCell<Option<std::sync::mpsc::Sender<crate::open_files::Chosen>>> =
                const { RefCell::new(None) };
        }

        fn chosen(_: crate::html::Wake) -> Receiver<crate::open_files::Chosen> {
            let (done, answer) = std::sync::mpsc::channel();
            let paths = CHOSEN.with(|c| c.borrow().clone());
            let _ = done.send(Ok(crate::open_files::sized(paths)));
            answer
        }

        fn left_open(_: crate::html::Wake) -> Receiver<crate::open_files::Chosen> {
            let (done, answer) = std::sync::mpsc::channel();
            OPEN.with(|open| *open.borrow_mut() = Some(done));
            answer
        }

        /// A file of `size` bytes in `dir`.
        fn file(dir: &tempfile::TempDir, name: &str, size: u64) -> PathBuf {
            let path = dir.path().join(name);
            let file = std::fs::File::create(&path).unwrap();
            file.set_len(size).unwrap();
            path
        }

        /// Opens a reply, answers its file dialog with `files` and waits for the answer to be taken in.
        fn attach(driver: &mut Driver, files: Vec<PathBuf>) {
            open(driver, "r");
            CHOSEN.with(|c| *c.borrow_mut() = files);
            driver.app.ask_open_paths = chosen;
            driver.apply(UiAction::ComposeAttach(PANE));
            driver.tick_until(|app| app.compose_attaching.is_none());
        }

        #[test]
        fn chosen_files_are_attached_and_counted() {
            let dir = tempfile::tempdir().unwrap();
            let (a, b) = (file(&dir, "a.pdf", 1000), file(&dir, "b.txt", 3000));
            let Replying { mut driver, .. } = replying(&[1]);
            attach(&mut driver, vec![a.clone(), b.clone()]);
            let composer = driver.app.composer.as_ref().unwrap();
            assert_eq!(composer.files, [(a, 1000), (b, 3000)]);
            assert_eq!(composer.size_line(), "3 KB of 25.0 MB");
            assert_eq!(composer.error, None);
        }

        #[test]
        fn too_much_shows_at_once() {
            let dir = tempfile::tempdir().unwrap();
            let big = file(&dir, "big.iso", 30_000_000);
            let Replying { mut driver, fx, .. } = replying(&[1]);
            fx.store("work")
                .save_smtp_capabilities(Some(20_000_000), false)
                .unwrap();
            attach(&mut driver, vec![big]);
            let composer = driver.app.composer.as_ref().unwrap();
            assert_eq!(composer.size_line(), "28.6 MB of 19.1 MB");
            assert_eq!(
                composer.error,
                Some((
                    Field::Attachments,
                    "28.6 MB attached; the limit is 19.1 MB.".into()
                ))
            );
            driver.apply(UiAction::ComposeRemoveFile(PANE, 0));
            let composer = driver.app.composer.as_ref().unwrap();
            assert_eq!(composer.error, None, "removing it takes the error away");
            assert_eq!(composer.size_line(), "");
        }

        #[test]
        fn a_file_that_cannot_be_read_is_left_out_and_named() {
            let dir = tempfile::tempdir().unwrap();
            let a = file(&dir, "a.pdf", 1000);
            let Replying { mut driver, .. } = replying(&[1]);
            attach(&mut driver, vec![a.clone(), dir.path().join("gone.txt")]);
            let composer = driver.app.composer.as_ref().unwrap();
            assert_eq!(composer.files, [(a, 1000)]);
            assert_eq!(
                composer.error,
                Some((Field::Attachments, "gone.txt can't be read.".into()))
            );
        }

        #[test]
        fn files_chosen_for_a_closed_composer_attach_nowhere() {
            let dir = tempfile::tempdir().unwrap();
            let a = file(&dir, "a.pdf", 1000);
            let Replying { mut driver, .. } = replying(&[1]);
            open(&mut driver, "r");
            driver.app.ask_open_paths = left_open;
            driver.apply(UiAction::ComposeAttach(PANE));
            driver.apply(UiAction::ComposeClose(PANE));
            open(&mut driver, "r");
            let sender = OPEN.with(|open| open.borrow_mut().take()).unwrap();
            sender.send(Ok(crate::open_files::sized(vec![a]))).unwrap();
            driver.tick_until(|app| app.compose_attaching.is_none());
            let composer = driver.app.composer.as_ref().unwrap();
            assert!(composer.files.is_empty(), "the new reply got nothing");
            assert_eq!(composer.error, None);
        }

        #[test]
        fn a_cancelled_dialog_attaches_nothing() {
            let Replying { mut driver, .. } = replying(&[1]);
            attach(&mut driver, Vec::new());
            let composer = driver.app.composer.as_ref().unwrap();
            assert!(composer.files.is_empty());
            assert_eq!(composer.error, None);
        }

        #[test]
        fn a_dialog_that_cannot_open_says_why() {
            let mut replying = replying(&[1]);
            open(&mut replying.driver, "r");
            replying.driver.app.ask_open_paths = |_| {
                let (done, answer) = std::sync::mpsc::channel();
                let _ = done.send(Err(anyhow::anyhow!("no portal")));
                answer
            };
            replying.driver.apply(UiAction::ComposeAttach(PANE));
            let driver = &mut replying.driver;
            driver.tick_until(|app| app.compose_attaching.is_none());
            assert_eq!(
                driver.app.composer.as_ref().unwrap().error,
                Some((
                    Field::Attachments,
                    "Could not open the file dialog: no portal".into()
                ))
            );
        }
    }

    mod recovery {
        use super::*;
        use crate::compose::{load_recoveries, recovery_dir};

        fn left(fx: &Fixture) -> Vec<Composer> {
            load_recoveries(&recovery_dir(&fx.paths))
        }

        fn write(driver: &mut Driver, target: ComposeTarget, field: ComposeField, text: &str) {
            driver.apply(UiAction::ComposeEdit(target, field, text.into()));
        }

        /// New mail typed into, in a window, by its target.
        fn new_mail_typed(driver: &mut Driver, subject: &str) -> ComposeTarget {
            driver.apply(UiAction::NewMail(None));
            let target = ComposeTarget::Window(driver.app.new_mail.last().unwrap().id);
            write(driver, target, ComposeField::Subject, subject);
            target
        }

        #[test]
        fn a_changed_composer_is_written_every_few_seconds() {
            let Replying { mut driver, fx, .. } = replying(&[1]);
            new_mail_typed(&mut driver, "Plans");
            driver.at(1.0);
            assert!(left(&fx).is_empty(), "not before 5 s");
            driver.at(6.0);
            assert_eq!(left(&fx)[0].subject, "Plans");
            driver.apply(UiAction::NewMail(None));
            driver.at(12.0);
            assert_eq!(left(&fx).len(), 1, "an unchanged one has nothing to keep");
        }

        #[test]
        fn a_queued_or_discarded_composer_leaves_no_file() {
            let Replying { mut driver, fx, .. } = replying(&[1]);
            let window = new_mail_typed(&mut driver, "Plans");
            driver.at(6.0);
            assert_eq!(left(&fx).len(), 1);
            driver.apply(UiAction::ComposeClose(window));
            driver.apply(UiAction::LeaveComposeDiscard(window));
            assert!(driver.app.new_mail.is_empty());
            assert!(left(&fx).is_empty(), "discarded");

            open(&mut driver, "r");
            write(&mut driver, PANE, ComposeField::Text, "Thanks");
            driver.at(12.0);
            assert_eq!(left(&fx).len(), 1);
            driver.apply(UiAction::ComposeSend(PANE));
            driver.tick_until(|app| app.composer.is_none());
            assert!(left(&fx).is_empty(), "queued");
        }

        #[test]
        fn start_reopens_left_composers_with_the_line() {
            let Replying { mut driver, fx, .. } = replying(&[1, 2]);
            driver.press("down");
            open(&mut driver, "r");
            let original = driver.app.body.as_ref().map(|body| body.key.clone());
            write(&mut driver, PANE, ComposeField::Text, "Half a reply");
            new_mail_typed(&mut driver, "Plans");
            driver.at(6.0);
            drop(driver);

            let (mut restarted, _wires) = fx.driver();
            let pane = restarted.app.composer.clone().unwrap();
            assert_eq!(pane.text, "Half a reply");
            assert_eq!(pane.kind, ComposeKind::Reply(ReplyTo::Sender));
            assert_eq!(pane.error, Some((Field::Note, RESTORED.into())));
            assert_eq!(
                pane.original.as_ref().map(|o| (o.folder.clone(), o.uid)),
                original
            );
            restarted.tick_until(|app| app.body.is_some());
            assert_eq!(
                restarted.app.body.as_ref().map(|body| body.key.clone()),
                original,
                "the original shows below it"
            );
            let [window] = restarted.app.new_mail.as_slice() else {
                panic!("{:?}", restarted.app.new_mail);
            };
            assert_eq!(window.subject, "Plans");
            assert_eq!(window.error, Some((Field::Note, RESTORED.into())));
            assert!(restarted.app.next_compose > pane.id.max(window.id));
        }

        #[test]
        fn a_reply_whose_original_is_gone_reopens_as_new_mail() {
            let Replying { mut driver, fx, .. } = replying(&[1]);
            open(&mut driver, "r");
            write(&mut driver, PANE, ComposeField::Text, "Half a reply");
            driver.app.composer.as_mut().unwrap().in_reply_to = Some("<1@example.com>".into());
            driver.at(6.0);
            drop(driver);
            let store = postvak_core::store::Store::open_account(&fx.paths, "work").unwrap();
            store.remove_message("INBOX", 1).unwrap();

            let (restarted, _wires) = fx.driver();
            assert!(restarted.app.composer.is_none());
            let [window] = restarted.app.new_mail.as_slice() else {
                panic!("{:?}", restarted.app.new_mail);
            };
            assert_eq!(window.kind, ComposeKind::New);
            assert_eq!(window.text, "Half a reply");
            assert_eq!(window.to, "Sender 1 <sender1@example.com>");
            assert_eq!(window.in_reply_to.as_deref(), Some("<1@example.com>"));
        }

        #[test]
        fn a_composer_saved_while_sending_restores_with_the_warning() {
            let Replying { mut driver, fx, .. } = replying(&[1]);
            open(&mut driver, "r");
            write(&mut driver, PANE, ComposeField::Text, "Thanks");
            driver.app.composer.as_mut().unwrap().sending = true;
            driver.app.save_recoveries();
            drop(driver);

            let (mut restarted, _wires) = fx.driver();
            restarted.app.queue_send = queued;
            let pane = restarted.app.composer.clone().unwrap();
            assert!(!pane.sending);
            assert_eq!(pane.error, Some((Field::Send, MAY_HAVE_GONE.into())));
            restarted.apply(UiAction::ComposeSend(PANE));
            let pane = restarted.app.composer.as_ref().unwrap();
            assert_eq!(pane.error, Some((Field::Send, SEND_AGAIN.into())));
            assert!(restarted.app.compose_sending.is_empty(), "asked first");
            restarted.apply(UiAction::ComposeSend(PANE));
            restarted.tick_until(|app| app.composer.is_none() && app.compose_sending.is_empty());
            assert!(left(&fx).is_empty(), "sent once asked");
        }

        #[test]
        fn a_second_left_reply_waits_with_a_notice() {
            let Replying { mut driver, fx, .. } = replying(&[1]);
            open(&mut driver, "r");
            write(&mut driver, PANE, ComposeField::Text, "First");
            let mut second = driver.app.composer.clone().unwrap();
            second.id += 100;
            crate::compose::save_recovery(&recovery_dir(&fx.paths), &second).unwrap();
            driver.app.save_recoveries();
            drop(driver);

            let (restarted, _wires) = fx.driver();
            assert!(restarted.app.composer.is_some());
            assert_eq!(
                restarted.app.notice.as_deref(),
                Some("1 more unsent message kept for a later start")
            );
            assert_eq!(left(&fx).len(), 2, "the second stays on disk");
        }

        #[test]
        fn a_reply_whose_original_went_meanwhile_goes_as_new_mail() {
            let Replying { mut driver, .. } = replying(&[1]);
            driver.app.queue_send = |_, _, _| {
                let gone = postvak_core::send::compose::OriginalGone {
                    folder: "INBOX".into(),
                    uid: 1,
                    account: "work".into(),
                };
                Err(QueueError::OriginalGone(gone.to_string()))
            };
            open(&mut driver, "r");
            write(&mut driver, PANE, ComposeField::Text, "Thanks");
            driver.app.composer.as_mut().unwrap().in_reply_to = Some("<1@example.com>".into());
            driver.apply(UiAction::ComposeSend(PANE));
            driver.tick_until(|app| app.compose_sending.is_empty());
            assert!(driver.app.composer.is_none());
            let [window] = driver.app.new_mail.as_slice() else {
                panic!("{:?}", driver.app.new_mail);
            };
            assert_eq!(window.kind, ComposeKind::New);
            assert_eq!(window.text, "Thanks");
            assert_eq!(window.in_reply_to.as_deref(), Some("<1@example.com>"));
            assert_eq!(window.error, Some((Field::Note, GONE_AS_NEW.into())));
            assert!(!window.sending);
        }

        #[test]
        fn a_taken_back_reply_held_by_another_composer_keeps_its_file() {
            let Replying { mut driver, fx, .. } = replying(&[1, 2]);
            open(&mut driver, "r");
            send(&mut driver, "Thanks");
            assert!(left(&fx).is_empty());
            driver.press("down");
            open(&mut driver, "r");
            write(&mut driver, PANE, ComposeField::Text, "Another");

            driver.apply(UiAction::Undo);
            driver.tick_until(|app| app.leave_compose.is_some());
            driver.apply(UiAction::LeaveComposeCancel(PANE));
            let texts: Vec<_> = left(&fx).into_iter().map(|c| c.text).collect();
            assert_eq!(texts, ["Thanks"], "kept on disk while it waits");
        }

        #[test]
        fn quitting_writes_the_files_without_asking() {
            let Replying { mut driver, fx, .. } = replying(&[1]);
            open(&mut driver, "r");
            write(&mut driver, PANE, ComposeField::Text, "Half a reply");
            new_mail_typed(&mut driver, "Plans");
            assert!(left(&fx).is_empty());
            driver.app.save_recoveries();
            let texts: Vec<_> = left(&fx).into_iter().map(|c| (c.text, c.subject)).collect();
            assert_eq!(texts.len(), 2, "{texts:?}");
            assert_eq!(driver.app.top_dialog(), None);
            assert!(driver.app.leave_compose.is_none() && driver.app.leave_new_mail.is_empty());
        }

        #[test]
        fn a_new_mail_window_that_fails_to_open_keeps_its_file() {
            let Replying { mut driver, fx, .. } = replying(&[1]);
            let ComposeTarget::Window(id) = new_mail_typed(&mut driver, "Plans") else {
                panic!("not a window");
            };
            driver.apply(UiAction::NewMailFailed(id, "no display".into()));
            assert!(driver.app.new_mail.is_empty());
            assert_eq!(left(&fx)[0].subject, "Plans");
        }
    }

    /// Opening a draft from Drafts in a composer, through the real store and `drafts::read`.
    mod opening_drafts {
        use super::*;
        use crate::compose::{files_dir, load_recoveries, recovery_dir};
        use postvak_core::send::drafts;

        const ORIGINAL: &[u8] = b"From: Linus Example <linus@x.org>\r\nTo: work@example.com\r\nSubject: Plans\r\nDate: Mon, 2 Nov 2026 10:00:00 +0000\r\nMessage-ID: <m1@x.org>\r\n\r\nShall we?\r\n";

        /// Stores `raw` as `folder`/`uid` of work, downloaded.
        fn stored(fx: &Fixture, folder: &str, uid: u32, raw: &[u8]) {
            listed(fx, folder, uid, raw);
            fx.store("work").set_raw(folder, uid, raw, "text").unwrap();
        }

        /// Stores the headers of `raw` as `folder`/`uid` of work, its body not downloaded.
        fn listed(fx: &Fixture, folder: &str, uid: u32, raw: &[u8]) {
            let parsed = postvak_core::message::parse_headers(raw);
            let mut m = message(folder, uid, parsed.subject.as_deref().unwrap_or_default());
            m.message_id = parsed.message_id;
            m.from_addr = parsed.from;
            m.headers = raw.to_vec();
            m.body_text = None;
            fx.add("work", m);
        }

        fn request(kind: SendKind, text: &str, original: Option<&[u8]>) -> SendRequest {
            SendRequest {
                account: Some("work".into()),
                kind,
                body: Body::Given(text.into()),
                from: None,
                attachments: Vec::new(),
                now: false,
                save_sent: None,
                original: original.map(<[u8]>::to_vec),
                headers: None,
                forward_parts: None,
                draft: Vec::new(),
                signature: false,
            }
        }

        /// A draft Postvak saved for `request`, as it is on the server.
        fn built(fx: &Fixture, request: &SendRequest) -> Vec<u8> {
            let config = Config::load(&fx.paths.config_file()).unwrap();
            drafts::build(&config, &fx.paths, &LazyClient::new(&fx.paths), request).unwrap()
        }

        fn new_mail_draft(fx: &Fixture) -> Vec<u8> {
            let kind = SendKind::New {
                to: vec!["ann@x.org".into()],
                cc: Vec::new(),
                bcc: Vec::new(),
                subject: "Lunch".into(),
            };
            built(fx, &request(kind, "Noon?", None))
        }

        /// work with ORIGINAL in INBOX and a Drafts folder holding `draft` as uid 5; the window shows Drafts.
        fn in_drafts(draft: impl FnOnce(&Fixture) -> Vec<u8>) -> (Driver, Fixture) {
            in_drafts_as(draft, true)
        }

        /// As `in_drafts`, with the draft's raw bytes not downloaded yet unless `downloaded`.
        fn in_drafts_as(
            draft: impl FnOnce(&Fixture) -> Vec<u8>,
            downloaded: bool,
        ) -> (Driver, Fixture) {
            let fx = Fixture::new(&["work"]);
            fx.folder("work", "Drafts", Some("Drafts"));
            stored(&fx, "INBOX", 1, ORIGINAL);
            let raw = draft(&fx);
            listed(&fx, "Drafts", 5, &raw);
            if downloaded {
                fx.store("work").set_raw("Drafts", 5, &raw, "text").unwrap();
            }
            let (mut driver, _wires) = fx.driver();
            driver.apply(UiAction::SelectView(View::Folder {
                account: 0,
                folder: "Drafts".into(),
            }));
            assert!(driver.app.body_follows_cursor());
            (driver, fx)
        }

        fn opened(app: &App) -> bool {
            app.compose_opening.is_none() && (app.composer.is_some() || !app.new_mail.is_empty())
        }

        fn edit(driver: &mut Driver) {
            driver.apply(UiAction::EditComposeDraft);
            driver.tick_until(opened);
        }

        #[test]
        fn editing_a_reply_draft_opens_it_over_its_original() {
            let reply = SendKind::Reply {
                folder: "INBOX".into(),
                uid: 1,
                to: ReplyTo::Sender,
            };
            let (mut driver, _fx) =
                in_drafts(|fx| built(fx, &request(reply, "Sure.", Some(ORIGINAL))));
            edit(&mut driver);
            let composer = driver.app.composer.as_ref().unwrap();
            assert_eq!(composer.kind, ComposeKind::Reply(ReplyTo::Sender));
            assert_eq!(composer.text, "Sure.");
            assert_eq!(composer.subject, "Re: Plans");
            let original = composer.original.as_ref().unwrap();
            assert_eq!((original.folder.as_str(), original.uid), ("INBOX", 1));
            assert_eq!(original.raw, ORIGINAL);
            assert!(composer.draft.as_ref().unwrap().message_id.starts_with('<'));
            assert!(driver.app.new_mail.is_empty());
            assert_eq!(
                driver.app.body.as_ref().map(|b| b.key.clone()),
                Some(("INBOX".into(), 1))
            );
            assert!(!composer.changed());
        }

        #[test]
        fn editing_a_new_mail_draft_opens_a_window() {
            let (mut driver, _fx) = in_drafts(new_mail_draft);
            edit(&mut driver);
            assert!(driver.app.composer.is_none());
            let window = &driver.app.new_mail[0];
            assert_eq!(window.kind, ComposeKind::New);
            assert_eq!(
                (window.to.as_str(), window.subject.as_str()),
                ("ann@x.org", "Lunch")
            );
            assert_eq!(window.text, "Noon?");
            assert!(window.draft.is_some());
        }

        #[test]
        fn a_foreign_reply_draft_opens_as_new_mail_over_the_message_it_answers() {
            let (mut driver, fx) = in_drafts(|_| {
                b"From: work@example.com\r\nTo: linus@x.org\r\nSubject: Re: Plans\r\nMessage-ID: <f1@other>\r\nIn-Reply-To: <m1@x.org>\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"b\"\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\nYes\r\n> Shall we?\r\n--b\r\nContent-Type: text/plain\r\nContent-Disposition: attachment; filename=\"../map.txt\"\r\n\r\nnorth\r\n--b--\r\n".to_vec()
            });
            edit(&mut driver);
            let composer = driver.app.composer.as_ref().unwrap();
            assert_eq!(composer.kind, ComposeKind::New);
            assert_eq!(composer.text, "Yes\n> Shall we?");
            assert_eq!(composer.in_reply_to.as_deref(), Some("<m1@x.org>"));
            let original = composer.original.as_ref().unwrap();
            assert_eq!((original.folder.as_str(), original.uid), ("INBOX", 1));
            assert!(original.raw.is_empty(), "shown, never quoted");
            let dir = files_dir(&recovery_dir(&fx.paths), composer.id);
            assert_eq!(composer.files, [(dir.join("0").join("map.txt"), 5)]);
            assert_eq!(composer.request().original, None);
        }

        /// A draft from another client, new mail with a file.
        fn foreign_with_a_file(_: &Fixture) -> Vec<u8> {
            b"From: work@example.com\r\nTo: linus@x.org\r\nSubject: Map\r\nMessage-ID: <f2@other>\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"b\"\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\nHere\r\n--b\r\nContent-Type: text/plain\r\nContent-Disposition: attachment; filename=\"map.txt\"\r\n\r\nnorth\r\n--b--\r\n".to_vec()
        }

        fn queued_with_files(
            paths: &Paths,
            config: &Config,
            request: SendRequest,
        ) -> Result<Queued, QueueError> {
            match request.attachments.iter().all(|path| path.exists()) {
                true => queued(paths, config, request),
                false => Err("a file is gone".into()),
            }
        }

        fn send_window(driver: &mut Driver) {
            let target = ComposeTarget::Window(driver.app.new_mail[0].id);
            driver.apply(UiAction::ComposeSend(target));
            driver.tick_until(|app| app.new_mail.is_empty() && app.compose_sending.is_empty());
        }

        #[test]
        fn undoing_the_send_of_an_opened_draft_keeps_its_files() {
            let (mut driver, _fx) = in_drafts(foreign_with_a_file);
            driver.app.queue_send = queued_with_files;
            driver.app.cancel_send = |_, _, _| Ok(());
            edit(&mut driver);
            let file = driver.app.new_mail[0].files[0].0.clone();
            send_window(&mut driver);
            assert!(file.exists(), "kept while Undo may reopen it");
            driver.apply(UiAction::Undo);
            driver.tick_until(|app| !app.new_mail.is_empty());
            assert_eq!(driver.app.new_mail[0].files[0].0, file);
            send_window(&mut driver);
            assert!(
                driver.app.taken.iter().any(|t| t.composer.is_some()),
                "sent again with its file"
            );
            driver.app.drop_sent_composers(&[]);
            assert!(!file.exists(), "gone once the gesture lets it go");
        }

        #[test]
        fn an_opened_draft_changed_back_keeps_its_files() {
            let (mut driver, fx) = in_drafts(foreign_with_a_file);
            edit(&mut driver);
            let target = ComposeTarget::Window(driver.app.new_mail[0].id);
            let file = driver.app.new_mail[0].files[0].0.clone();
            driver.apply(UiAction::ComposeEdit(
                target,
                ComposeField::Text,
                "Here, and more".into(),
            ));
            driver.at(6.0);
            assert_eq!(load_recoveries(&recovery_dir(&fx.paths)).len(), 1);
            driver.apply(UiAction::ComposeEdit(
                target,
                ComposeField::Text,
                "Here".into(),
            ));
            driver.at(12.0);
            assert!(
                load_recoveries(&recovery_dir(&fx.paths)).is_empty(),
                "unchanged again"
            );
            assert!(file.exists(), "the open composer still lists it");
        }

        #[test]
        fn an_evicted_gesture_leaves_files_a_recovery_file_names() {
            let (mut driver, fx) = in_drafts(foreign_with_a_file);
            edit(&mut driver);
            let composer = driver.app.new_mail.remove(0);
            let file = composer.files[0].0.clone();
            save_recovery(&recovery_dir(&fx.paths), &composer).unwrap();
            let id = driver.app.take("Sending".into(), 0);
            if let Some(taken) = driver.app.taken.iter_mut().find(|t| t.id == id) {
                taken.composer = Some(Box::new(composer));
            }
            for _ in 0..=MAX_TAKEN {
                driver.app.take("Archived".into(), 0);
            }
            assert!(driver.app.taken.iter().all(|t| t.id != id), "evicted");
            assert!(
                file.exists(),
                "kept for the next start, as its recovery file names it"
            );
        }

        #[test]
        fn a_reply_draft_waits_for_a_free_pane() {
            let reply = SendKind::Reply {
                folder: "INBOX".into(),
                uid: 1,
                to: ReplyTo::Sender,
            };
            let (mut driver, fx) =
                in_drafts(|fx| built(fx, &request(reply, "Sure.", Some(ORIGINAL))));
            driver.app.apply(UiAction::EditComposeDraft);
            let id = driver.app.next_compose - 1;
            driver.app.composer = Some(Composer::reply(99, "work", "INBOX", 1, ReplyTo::Sender));
            driver.tick_until(|app| app.compose_opening.is_none());
            assert_eq!(driver.app.composer.as_ref().map(|c| c.id), Some(99));
            assert!(driver.app.new_mail.is_empty());
            assert_eq!(driver.app.error.as_deref(), Some(CLOSE_FIRST));
            assert!(!recovery_dir(&fx.paths).join(id.to_string()).exists());
        }

        #[test]
        fn a_draft_open_under_an_older_copy_comes_forward() {
            let (mut driver, _fx) = in_drafts(new_mail_draft);
            edit(&mut driver);
            let shown = driver
                .app
                .body
                .as_ref()
                .and_then(|b| b.message.as_ref()?.message_id.clone())
                .unwrap();
            let window = &mut driver.app.new_mail[0];
            window.draft = Some(ComposeDraft {
                message_id: "<newer@x>".into(),
                left: vec![format!("<{shown}>")],
            });
            let id = window.id;
            driver.apply(UiAction::EditComposeDraft);
            assert!(driver.app.compose_opening.is_none());
            assert_eq!(driver.app.raise_compose, Some(id));
        }

        #[test]
        fn enter_on_a_draft_row_edits_it() {
            let (mut driver, _fx) = in_drafts(new_mail_draft);
            driver.app.focus = Focus::List;
            driver.press("enter");
            driver.tick_until(opened);
            assert_eq!(driver.app.new_mail.len(), 1);
        }

        #[test]
        fn enter_outside_drafts_opens_nothing() {
            let (mut driver, _fx) = in_drafts(new_mail_draft);
            driver.apply(UiAction::SelectView(View::Folder {
                account: 0,
                folder: "INBOX".into(),
            }));
            driver.press("enter");
            driver.tick();
            assert!(driver.app.compose_opening.is_none());
            assert!(driver.app.new_mail.is_empty() && driver.app.composer.is_none());
        }

        #[test]
        fn opening_a_draft_twice_shows_the_open_composer() {
            let (mut driver, _fx) = in_drafts(new_mail_draft);
            edit(&mut driver);
            let id = driver.app.new_mail[0].id;
            driver.apply(UiAction::EditComposeDraft);
            driver.tick();
            assert!(driver.app.compose_opening.is_none());
            assert_eq!(driver.app.new_mail.len(), 1);
            assert_eq!(driver.app.raise_compose, Some(id));
        }

        #[test]
        fn a_draft_not_downloaded_opens_once_it_is() {
            let mut raw = Vec::new();
            let (mut driver, fx) = in_drafts_as(
                |fx| {
                    raw = new_mail_draft(fx);
                    raw.clone()
                },
                false,
            );
            assert!(
                driver
                    .app
                    .body
                    .as_ref()
                    .is_some_and(|b| b.message.is_some())
            );
            driver.apply(UiAction::EditComposeDraft);
            assert!(driver.app.compose_opening.is_none() && driver.app.new_mail.is_empty());
            fx.store("work").set_raw("Drafts", 5, &raw, "text").unwrap();
            driver.app.reload_body(0, ("Drafts".into(), 5), 0.0);
            driver.tick_until(opened);
            assert_eq!(driver.app.new_mail.len(), 1);
        }
    }

    /// The same message through the composer and through `postvak send`, against an in-process daemon.
    mod equal_to_the_cli {
        use super::*;
        use postvak_core::daemon::test_support::{TestDaemon, options, recording_connector};
        use postvak_core::send::signature::sign;
        use postvak_core::store::{Message, Store};
        use std::time::{Duration, Instant};

        const ORIGINAL: &[u8] = b"From: Linus Example <linus@x.org>\r\nTo: me@example.com\r\nCc: Ann <ann@x.org>\r\nList-Post: <mailto:list@x.org>\r\nSubject: Plans\r\nDate: Mon, 2 Nov 2026 10:00:00 +0000\r\nMessage-ID: <m1@x.org>\r\n\r\nShall we?\r\n";
        const TEXT: &str = "Yes!\n";
        const SIGNATURE: &str = "Pieter\nPostvak";

        /// A daemon whose work account can send, holding ORIGINAL as INBOX/7.
        fn daemon() -> (TestDaemon, Config) {
            let daemon = TestDaemon::start_with(options(recording_connector(), None));
            let deadline = Instant::now() + Duration::from_secs(5);
            while Store::open_account(&daemon.paths, "work")
                .unwrap()
                .smtp_state()
                .unwrap()
                .is_none()
            {
                assert!(Instant::now() < deadline, "the SMTP guess was never stored");
                std::thread::sleep(Duration::from_millis(20));
            }
            daemon.wait_idle("work");
            let store = Store::open_account(&daemon.paths, "work").unwrap();
            let parsed = postvak_core::message::parse_headers(ORIGINAL);
            store
                .insert_message(&Message {
                    folder: "INBOX".into(),
                    uid: 7,
                    message_id: parsed.message_id.clone(),
                    from_addr: parsed.from.clone(),
                    subject: parsed.subject.clone(),
                    thread_id: "m1@x.org".into(),
                    internaldate: 1,
                    headers: ORIGINAL.to_vec(),
                    ..Default::default()
                })
                .unwrap();
            store.set_raw("INBOX", 7, ORIGINAL, "Shall we?").unwrap();
            let mut config = Config::load(&daemon.paths.config_file()).unwrap();
            config
                .sending
                .signatures
                .push(postvak_core::config::Signature {
                    address: "me@example.com".into(),
                    text: SIGNATURE.into(),
                    on_replies: true,
                });
            (daemon, config)
        }

        /// The queued message without its Date and Message-ID, and with its MIME boundaries named alike.
        fn queued_bytes(paths: &Paths, queued: &Queued) -> String {
            let path = paths.outbox_dir("work").join(format!("{}.eml", queued.id));
            let raw = String::from_utf8(std::fs::read(path).unwrap()).unwrap();
            let mut boundaries: Vec<String> = raw
                .split("boundary=\"")
                .skip(1)
                .filter_map(|rest| rest.split('"').next().map(str::to_string))
                .collect();
            boundaries.sort_by_key(|b| std::cmp::Reverse(b.len()));
            let kept = raw
                .split("\r\n")
                .filter(|line| !line.starts_with("Date: ") && !line.starts_with("Message-ID: "))
                .collect::<Vec<_>>()
                .join("\r\n");
            boundaries.iter().enumerate().fold(kept, |text, (i, b)| {
                text.replace(b, &format!("BOUNDARY{i}"))
            })
        }

        fn cli(kind: SendKind) -> SendRequest {
            SendRequest {
                account: Some("work".into()),
                kind,
                body: Body::Given(TEXT.into()),
                from: None,
                attachments: Vec::new(),
                now: false,
                save_sent: None,
                original: None,
                headers: None,
                forward_parts: None,
                draft: Vec::new(),
                signature: true,
            }
        }

        /// The composer opened on `composer`'s kind, the user's text typed at the top and `to` into To, then sent.
        fn through_the_composer(
            paths: &Paths,
            config: &Config,
            mut composer: Composer,
            kind: &SendKind,
            to: Option<&str>,
        ) -> Queued {
            let identity = config.accounts[0].identity().unwrap();
            composer.fill(prefill(paths, config, "work", kind).unwrap(), &identity);
            let typed = format!("{}{}", sign(TEXT, SIGNATURE), composer.text);
            composer.set(ComposeField::Text, typed);
            if let Some(to) = to {
                composer.set(ComposeField::To, to.into());
            }
            composer.forward_as_attachment = matches!(
                kind,
                SendKind::Forward {
                    as_: ForwardAs::Attachment,
                    ..
                }
            );
            queue_send(paths, config, composer.request()).unwrap()
        }

        fn assert_equal(kind: SendKind, composer: Composer, to: Option<&str>) {
            let (daemon, config) = daemon();
            let paths = &daemon.paths;
            let by_cli = queue_send(paths, &config, cli(kind.clone())).unwrap();
            let by_gui = through_the_composer(paths, &config, composer, &kind, to);
            assert!(queued_bytes(paths, &by_cli).contains("Pieter"), "{kind:?}");
            assert_eq!(
                queued_bytes(paths, &by_gui),
                queued_bytes(paths, &by_cli),
                "{kind:?}"
            );
        }

        fn reply(to: ReplyTo) {
            let kind = SendKind::Reply {
                folder: "INBOX".into(),
                uid: 7,
                to,
            };
            assert_equal(kind, Composer::reply(1, "work", "INBOX", 7, to), None);
        }

        fn forward(as_: ForwardAs) {
            let kind = SendKind::Forward {
                folder: "INBOX".into(),
                uid: 7,
                as_,
                to: vec!["bob@x.org".into()],
                cc: vec![],
                bcc: vec![],
            };
            let composer = Composer::forward(1, "work", "INBOX", 7);
            assert_equal(kind, composer, Some("bob@x.org"));
        }

        #[test]
        fn new_mail() {
            let (daemon, config) = daemon();
            let paths = &daemon.paths;
            let kind = SendKind::New {
                to: vec!["bob@x.org".into()],
                cc: vec![],
                bcc: vec![],
                subject: "Lunch".into(),
            };
            let by_cli = queue_send(paths, &config, cli(kind)).unwrap();
            let accounts: Vec<_> = (config.accounts.iter())
                .map(|account| (account.name.clone(), account.identity().unwrap()))
                .collect();
            let mut composer = Composer::new_mail(1, "work", &accounts, None);
            composer.set(ComposeField::To, "bob@x.org".into());
            composer.set(ComposeField::Subject, "Lunch".into());
            composer.set(ComposeField::Text, sign(TEXT, SIGNATURE));
            let by_gui = queue_send(paths, &config, composer.request()).unwrap();
            assert!(queued_bytes(paths, &by_cli).contains("Pieter"));
            assert_eq!(queued_bytes(paths, &by_gui), queued_bytes(paths, &by_cli));
        }

        #[test]
        fn a_reply() {
            reply(ReplyTo::Sender);
        }

        #[test]
        fn a_reply_to_all() {
            reply(ReplyTo::All);
        }

        #[test]
        fn a_reply_to_the_list() {
            reply(ReplyTo::List);
        }

        #[test]
        fn an_inline_forward() {
            forward(ForwardAs::Inline);
        }

        #[test]
        fn a_forward_as_attachment() {
            forward(ForwardAs::Attachment);
        }
    }
}
