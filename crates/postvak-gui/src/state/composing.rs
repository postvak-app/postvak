//! The composer in the reader pane: answering the shown message, sending through the outbox, and taking a send back.
use postvak_core::daemon::client::LazyClient;
use postvak_core::send::check::DEFAULT_SIZE_LIMIT;
use postvak_core::send::compose::{Prefilled, Queued, SendKind, SendRequest};
use postvak_core::send::reply::{ForwardAs, ReplyTo};

use super::*;
use crate::compose::Field;

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
    #[allow(dead_code, reason = "new mail has no window of its own yet")]
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

/// The daemon's refusal reaches the client as text only: the outbox's `CancelError::TooLate` says "too late".
fn cancel_error(text: &str) -> CancelError {
    if text.contains("too late to cancel") {
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

    fn composer_ref(&self, target: ComposeTarget) -> Option<&Composer> {
        match target {
            ComposeTarget::Pane => self.composer.as_ref(),
            ComposeTarget::Window(_) => None,
        }
    }

    fn composer_mut(&mut self, target: ComposeTarget) -> Option<&mut Composer> {
        match target {
            ComposeTarget::Pane => self.composer.as_mut(),
            ComposeTarget::Window(_) => None,
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
    }

    /// The composer's own actions: its fields, Send, Close and Discard.
    pub(super) fn compose(&mut self, action: UiAction) {
        match action {
            UiAction::ComposeSend(target) => return self.compose_send(target),
            UiAction::ComposeClose(target) | UiAction::ComposeDiscard(target) => {
                if target == ComposeTarget::Pane {
                    self.composer = None;
                }
                return;
            }
            _ => {}
        }
        let (UiAction::ComposeEdit(target, ..)
        | UiAction::ComposeShowCcBcc(target)
        | UiAction::ComposeAttach(target)
        | UiAction::ComposeRemoveFile(target, _)
        | UiAction::ComposeToggleForwardPart(target, _)
        | UiAction::ComposeForwardAsAttachment(target, _)) = action
        else {
            return;
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
            // shortcut: no file dialog yet, so Attach does nothing until the desktop's open dialog is wired in.
            _ => {}
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
        if let Err(error) = composer.check(&identity, DEFAULT_SIZE_LIMIT as u64) {
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
    }

    /// Whether `action` waits for "Discard this message?" about the pane's changed composer; an unchanged one closes.
    pub(super) fn holds_for_compose(&mut self, action: &UiAction) -> bool {
        let Some(composer) = &self.composer else {
            return false;
        };
        if !self.leaves_pane(action, composer) {
            return false;
        }
        if composer.leave_question().is_some() {
            self.leave_compose = Some(Box::new(action.clone()));
            return true;
        }
        self.composer = None;
        false
    }

    /// Whether `action` takes the reader pane from `composer`: another message, folder or view, or another composer.
    fn leaves_pane(&self, action: &UiAction, composer: &Composer) -> bool {
        match action {
            UiAction::SelectRow(index) => self.list.rows.get(*index).is_none_or(|row| {
                let original = composer.original.as_ref();
                self.accounts[row.account].name != composer.account
                    || original.is_none_or(|o| o.folder != row.folder || o.uid != row.uid)
            }),
            UiAction::SelectView(view) => *view != self.view,
            UiAction::ComposeClose(target) | UiAction::ComposeDiscard(target) => {
                *target == ComposeTarget::Pane
            }
            UiAction::MoveCursor(_)
            | UiAction::StepFolder(_)
            | UiAction::OpenMessage(_)
            | UiAction::Reply(_)
            | UiAction::Forward
            | UiAction::ReopenCompose(_) => true,
            _ => false,
        }
    }

    /// Puts the composer of sent gesture `id`, taken back from the outbox, in the reader pane.
    pub(super) fn reopen_compose(&mut self, id: u64) {
        let Some(taken) = self.taken.iter_mut().find(|taken| taken.id == id) else {
            return;
        };
        if let Some(mut composer) = taken.composer.take() {
            taken.state = UndoState::Undone;
            composer.sending = false;
            self.composer = Some(*composer);
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
            (Ok(prefilled), Some(identity)) => composer.fill(prefilled, &identity),
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
                let pane_empty = target == ComposeTarget::Pane && self.composer.is_none();
                match self.composer_mut(target).filter(|_| still_open) {
                    Some(open) => {
                        open.sending = false;
                        open.error = Some((Field::Send, why));
                    }
                    // Left meanwhile: what was typed comes back rather than going with the refusal.
                    None if pane_empty => {
                        composer.error = Some((Field::Send, why));
                        self.composer = Some(composer);
                    }
                    None => self.note_error(None, format!("not sent: {why}")),
                }
                return;
            }
        };
        if still_open && target == ComposeTarget::Pane {
            self.composer = None;
        }
        if undo_secs == 0 {
            self.toast = Some(Toast::new("Sent".into(), false));
            return;
        }
        let words = undo::sending_text(&queued.first_recipient, queued.more);
        let account = self.accounts.iter().position(|a| a.name == queued.account);
        let id = self.take(words.clone(), 0);
        if let Some(taken) = self.taken.iter_mut().find(|taken| taken.id == id) {
            taken.outbox = account.map(|account| (account, queued.id));
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

    /// Lets go of each sent composer whose message left `waiting` in its account's outbox, read on the file poll.
    pub(super) fn drop_sent_composers(&mut self, waiting: &[(usize, String)]) {
        for taken in &mut self.taken {
            if taken.state == UndoState::Open
                && let Some(outbox) = &taken.outbox
                && !waiting.contains(outbox)
            {
                taken.composer = None;
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
    use postvak_core::send::Envelope;
    use postvak_core::send::build::Draft;
    use postvak_core::send::compose::Body;
    use postvak_core::send::outbox::{Entry, Outbox, State};

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
        _wires: Wires,
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
        Replying {
            driver,
            _wires: wires,
            fx,
        }
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
    fn the_daemons_too_late_refusal_is_told_apart() {
        let too_late = postvak_core::send::outbox::CancelError::TooLate("0000abcd".into());
        assert_eq!(cancel_error(&too_late.to_string()), CancelError::TooLate);
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

        driver.apply(UiAction::LeaveComposeCancel);
        assert!(driver.app.composer.is_some());
        assert_eq!(driver.app.top_dialog(), None);

        driver.press("j");
        driver.apply(UiAction::LeaveComposeDiscard);
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

    #[test]
    fn the_composer_is_dropped_once_its_message_left_waiting() {
        let Replying { mut driver, fx, .. } = replying(&[1]);
        driver.app.queue_send = |paths, _, request| {
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
        };
        open(&mut driver, "r");
        send(&mut driver, "Thanks");
        driver.at(POLL);
        assert!(
            driver.app.taken.last().unwrap().composer.is_some(),
            "still waiting"
        );

        let (_, id) = driver.app.taken.last().unwrap().outbox.clone().unwrap();
        let outbox = Outbox::open(&fx.paths, "work").unwrap();
        outbox.set_state(&id, State::Sending).unwrap();
        driver.at(2.0 * POLL);
        assert!(driver.app.taken.last().unwrap().composer.is_none());
    }
}
