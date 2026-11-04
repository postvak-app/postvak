//! The composers: a reply or forward in the reader pane, new mail in windows of its own, sending through the outbox,
//! and taking a send back.
use postvak_core::daemon::client::LazyClient;
use postvak_core::send::check::DEFAULT_SIZE_LIMIT;
use postvak_core::send::compose::{Prefilled, Queued, SendKind, SendRequest};
use postvak_core::send::mailto::Mailto;
use postvak_core::send::reply::{ForwardAs, ReplyTo};

use super::*;
use crate::compose::{ComposeKind, Field};

pub(crate) type PrefillFn = fn(&Paths, &Config, &str, &SendKind) -> Result<Prefilled, String>;
pub(crate) type QueueFn = fn(&Paths, &Config, SendRequest) -> Result<Queued, String>;
pub(crate) type CancelFn = fn(&Paths, &str, &str) -> Result<(), CancelError>;

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
    answer: Receiver<Result<(Queued, u64), String>>,
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

/// Builds, checks and queues `request` as `postvak send` does; the window never opens an editor.
pub(crate) fn queue_send(
    paths: &Paths,
    config: &Config,
    request: SendRequest,
) -> Result<Queued, String> {
    let client = LazyClient::new(paths);
    let no_editor = |_: &str| Err(anyhow::anyhow!("the window has no editor"));
    postvak_core::send::compose::compose(config, paths, &client, request, no_editor)
        .map_err(|e| error_text(&e))
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

fn load_config(paths: &Paths) -> Result<Config, String> {
    Config::load(&paths.config_file()).map_err(|e| format!("could not read config.toml: {e}"))
}

impl App {
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

    /// Drops the composer at `target`; a window's question goes with it.
    fn drop_composer(&mut self, target: ComposeTarget) {
        match target {
            ComposeTarget::Pane => self.composer = None,
            ComposeTarget::Window(id) => {
                self.new_mail.retain(|c| c.id != id);
                self.leave_new_mail.retain(|&asking| asking != id);
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

    pub(super) fn new_mail_failed(&mut self, id: u64, why: &str) {
        self.drop_composer(ComposeTarget::Window(id));
        self.note_error(None, format!("could not open a new-mail window: {why}"));
    }

    /// Closes new-mail window `id`, asking first when it was changed; one being sent stays.
    fn close_new_mail(&mut self, id: u64) {
        let target = ComposeTarget::Window(id);
        let Some(composer) = self.composer_ref(target).filter(|c| !c.sending) else {
            return;
        };
        if composer.leave_question().is_none() {
            self.drop_composer(target);
        } else if !self.leave_new_mail.contains(&id) {
            self.leave_new_mail.push(id);
        }
    }

    /// Cancel on "Discard this message?": the composer stays.
    pub(super) fn leave_cancel(&mut self, target: ComposeTarget) {
        match target {
            ComposeTarget::Pane => self.leave_compose = None,
            ComposeTarget::Window(id) => self.leave_new_mail.retain(|&asking| asking != id),
        }
    }

    /// Discard on "Discard this message?": the composer goes, and what asked to leave the pane goes on.
    pub(super) fn leave_discard(&mut self, target: ComposeTarget) {
        match target {
            ComposeTarget::Pane => {
                if let Some(leave) = self.leave_compose.take() {
                    self.composer = None;
                    self.apply_action(*leave);
                }
            }
            ComposeTarget::Window(id) if self.leave_new_mail.contains(&id) => {
                self.drop_composer(target);
            }
            ComposeTarget::Window(_) => {}
        }
    }

    /// Opens a reply (`Some`) or a forward of the shown message in the reader pane, loading while it is filled.
    pub(super) fn answer(&mut self, reply_to: Option<ReplyTo>) {
        let Some(body) = &self.body else { return };
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
            UiAction::ComposeSend(target) => return self.compose_send(target),
            UiAction::ComposeAttach(target) => return self.compose_attach(target),
            // The pane's composer was asked about before this, in `holds_for_compose`.
            UiAction::ComposeClose(target) | UiAction::ComposeDiscard(target) => {
                match target {
                    ComposeTarget::Pane => self.composer = None,
                    ComposeTarget::Window(id) => self.close_new_mail(id),
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
            .filter(|composer| !composer.loading && !composer.sending)
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
        // Until the queue answers, the composer is what comes back on a refusal.
        if composer.sending {
            return true;
        }
        if composer.leave_question().is_some() {
            self.leave_compose = Some(Box::new(action.clone()));
            return true;
        }
        self.composer = None;
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
                .is_some_and(|reopened| reopened.kind != ComposeKind::New),
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
            if composer.kind == ComposeKind::New {
                self.new_mail.push(*composer);
            } else {
                self.composer = Some(*composer);
            }
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
        self.receive_attach();
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
    fn sent(&mut self, sending: Sending, result: Result<(Queued, u64), String>, now: f64) {
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
            Err(why) => {
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
        if still_open {
            self.drop_composer(target);
        }
        if undo_secs == 0 {
            self.toast = Some(Toast::new("Sent".into(), false));
            return;
        }
        let words = undo::sending_text(&queued.first_recipient, queued.more);
        // Without the account in the window there is nothing to take it back from, so nothing to undo.
        let Some(account) = self.accounts.iter().position(|a| a.name == queued.account) else {
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
                self.toast = Some(Toast::new("Not sent".into(), false));
                self.apply_action(UiAction::ReopenCompose(id));
            }
            Err(CancelError::TooLate) => {
                taken.state = UndoState::Undone;
                taken.composer = None;
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
                taken.composer = None;
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

    fn queued(_: &Paths, _: &Config, request: SendRequest) -> Result<Queued, String> {
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
        let fx = Fixture::new(&["work"]);
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
    fn leaving_a_changed_composer_asks_in_the_window() {
        let (_fx, open) = replying_in_the_window();
        open.window()
            .invoke_compose_edited(crate::ui::ComposeField::Text, "Hi".into());
        assert_eq!(
            open.with(|live| live.app.composer.as_ref().map(|c| c.text.clone())),
            Some("Hi".into())
        );
        open.press("j");
        assert_eq!(open.find("Discard this message?").len(), 1);
        open.click("Cancel");
        assert!(open.find("Discard this message?").is_empty());
        assert!(open.with(|live| live.app.composer.is_some()));
        open.press("j");
        let discard = open.find("Discard");
        assert_eq!(discard.len(), 2, "the composer's and the question's");
        discard[1].invoke_accessible_default_action();
        open.tick();
        assert!(open.with(|live| live.app.composer.is_none()));
        assert!(open.find("Your text").is_empty());
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
    fn the_keys_open_each_kind() {
        let Replying { mut driver, .. } = replying(&[1]);
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

    /// Queues as the daemon does: the entry is in the account's outbox, waiting.
    fn queued_in_outbox(paths: &Paths, _: &Config, request: SendRequest) -> Result<Queued, String> {
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
        windows[0].invoke_compose_edited(crate::ui::ComposeField::Subject, "First".into());
        windows[1].invoke_compose_edited(crate::ui::ComposeField::Subject, "Second".into());
        open.tick();
        assert_eq!(new_mail_subjects(&open), ["First", "Second"]);
        assert_eq!(windows[0].get_compose().subject, "First");
        assert_eq!(windows[1].get_compose().subject, "Second");
        assert!(open.with(|live| live.app.composer.is_none()));
    }

    #[test]
    fn closing_a_changed_new_mail_window_asks() {
        use slint::platform::{Key, WindowEvent};
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = fx.window();
        open.with(|live| live.app.apply(UiAction::NewMail(None)));
        open.compose_windows()[0].invoke_compose_close();
        open.tick();
        assert!(open.compose_windows().is_empty(), "an unchanged one closes");

        open.with(|live| live.app.apply(UiAction::NewMail(None)));
        let window = open.compose_windows()[0].clone_strong();
        window.invoke_compose_edited(crate::ui::ComposeField::Text, "Hi".into());
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
        window.invoke_leave_compose_discard();
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
}
