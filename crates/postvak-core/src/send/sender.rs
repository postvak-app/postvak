//! One sender thread per account, beside its IMAP sync thread, so a long sync, a broken IMAP login or IDLE never holds
//! back a message that is due or a cancel. Each thread sends what its outbox holds once it is due, retries temporary
//! failures for an hour, and hands what needs IMAP afterwards (the Sent copy, the flag on the original) to the daemon
//! on `finish`, again every few minutes until the IMAP side reports it done.

use std::collections::{HashMap, VecDeque};
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::outbox::{CancelError, Entry, Outbox, State};
use super::{SmtpError, SmtpOps, server_for};
use crate::config::{AccountConfig, Config};
use crate::credentials;
use crate::paths::Paths;
use crate::store::Store;

/// Opens a new SMTP client; the daemon's is [`super::smtp::Lettre`].
pub type SmtpConnector = Arc<dyn Fn() -> Box<dyn SmtpOps + Send> + Send + Sync>;
/// The current Unix time; tests move it on.
pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

/// The real SMTP client.
pub fn lettre_connector() -> SmtpConnector {
    Arc::new(|| Box::new(super::smtp::Lettre))
}

/// How long temporary failures are retried before a message fails.
pub const RETRY_FOR_SECS: i64 = 3600;
/// How often the IMAP side is asked again for a sent message's copy and flag.
const FINISH_EVERY_SECS: i64 = 300;
/// The longest a thread sleeps without looking at its outbox.
const LONGEST_SLEEP_SECS: i64 = 60;
/// Why a message for an account without a server failed.
pub const NOT_SET_UP: &str = "sending is not set up for this account; see `postvak guide sending`";

/// How many of an account's messages wait and how many failed.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxCounts {
    /// Waiting for their due time, being sent, or being retried after a temporary failure.
    pub waiting: u32,
    /// Failed for good, until retried or cancelled.
    pub failed: u32,
}

impl OutboxCounts {
    pub fn is_empty(&self) -> bool {
        self.waiting == 0 && self.failed == 0
    }
}

/// How many of `entries` wait and how many failed for good.
pub fn counts_of<'a>(entries: impl IntoIterator<Item = &'a Entry>) -> OutboxCounts {
    let mut count = OutboxCounts::default();
    for entry in entries {
        match entry.state {
            State::Waiting | State::Sending | State::Failed { retrying: true, .. } => {
                count.waiting += 1
            }
            State::Failed {
                retrying: false, ..
            } => count.failed += 1,
            State::Sent => {}
        }
    }
    count
}

/// What one account's threads share, kept across config reloads.
struct Shared {
    outbox: Outbox,
    /// Held for every change to the outbox, so a cancel and a send never both take the same message.
    lock: Mutex<()>,
    wake: Condvar,
    woken: Mutex<bool>,
    recovered: AtomicBool,
    /// The ids of the last messages that left the outbox because they were sent, newest last, so a caller waiting on
    /// one can tell that from a cancel.
    sent: Mutex<VecDeque<String>>,
}

/// How many sent ids [`Shared::sent`] remembers.
const SENT_REMEMBERED: usize = 256;

impl Shared {
    /// Removes sent message `id` from the outbox and remembers that it went. Called with the outbox lock held.
    fn remove_sent(&self, id: &str) -> io::Result<()> {
        self.outbox.remove(id)?;
        let mut sent = self.sent.lock().unwrap_or_else(|p| p.into_inner());
        if sent.len() == SENT_REMEMBERED {
            sent.pop_front();
        }
        sent.push_back(id.to_string());
        Ok(())
    }

    fn lock(&self) -> MutexGuard<'_, ()> {
        self.lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn wake(&self) {
        *self.woken.lock().unwrap_or_else(|p| p.into_inner()) = true;
        self.wake.notify_all();
    }

    /// Sleeps up to `secs`, or until woken.
    fn sleep(&self, secs: i64) {
        let woken = self.woken.lock().unwrap_or_else(|p| p.into_inner());
        let wait =
            Duration::from_secs(u64::try_from(secs.clamp(1, LONGEST_SLEEP_SECS)).unwrap_or(1));
        let (mut woken, _) = self
            .wake
            .wait_timeout_while(woken, wait, |woken| !*woken)
            .unwrap_or_else(|p| p.into_inner());
        *woken = false;
    }
}

struct AccountSender {
    config: AccountConfig,
    stop: Arc<AtomicBool>,
}

pub struct Senders {
    paths: Paths,
    smtp: SmtpConnector,
    finish: Sender<(String, String)>,
    clock: Clock,
    shared: HashMap<String, Arc<Shared>>,
    accounts: Vec<AccountSender>,
}

impl Senders {
    /// Starts a sender thread per account; `finish` takes (account, outbox id) for the IMAP side.
    pub fn start(
        config: &Config,
        paths: &Paths,
        smtp: SmtpConnector,
        finish: Sender<(String, String)>,
    ) -> Senders {
        Senders::start_with_clock(config, paths, smtp, finish, Arc::new(crate::time::now))
    }

    pub fn start_with_clock(
        config: &Config,
        paths: &Paths,
        smtp: SmtpConnector,
        finish: Sender<(String, String)>,
        clock: Clock,
    ) -> Senders {
        let mut senders = Senders {
            paths: paths.clone(),
            smtp,
            finish,
            clock,
            shared: HashMap::new(),
            accounts: Vec::new(),
        };
        senders.reload(config);
        senders
    }

    /// Restarts the threads of changed accounts, stops those of removed ones and starts new ones. A thread in the middle
    /// of a send finishes it before it stops.
    pub fn reload(&mut self, config: &Config) {
        let (kept, stale): (Vec<_>, Vec<_>) = std::mem::take(&mut self.accounts)
            .into_iter()
            .partition(|sender| config.accounts.contains(&sender.config));
        for sender in stale {
            sender.stop.store(true, Ordering::Release);
            if let Some(shared) = self.shared.get(&sender.config.name) {
                shared.wake();
            }
        }
        self.accounts = kept;
        for account in &config.accounts {
            if self.accounts.iter().any(|s| s.config.name == account.name) {
                continue;
            }
            if let Some(sender) = self.spawn(account) {
                self.accounts.push(sender);
            }
        }
    }

    fn spawn(&mut self, account: &AccountConfig) -> Option<AccountSender> {
        let shared = match self.shared.get(&account.name) {
            Some(shared) => shared.clone(),
            None => {
                let outbox = match Outbox::open(&self.paths, &account.name) {
                    Ok(outbox) => outbox,
                    Err(e) => {
                        log::error!("{}: the outbox cannot be opened: {e}", account.name);
                        return None;
                    }
                };
                let shared = Arc::new(Shared {
                    outbox,
                    lock: Mutex::new(()),
                    wake: Condvar::new(),
                    woken: Mutex::new(false),
                    recovered: AtomicBool::new(false),
                    sent: Mutex::new(VecDeque::new()),
                });
                self.shared.insert(account.name.clone(), shared.clone());
                shared
            }
        };
        let stop = Arc::new(AtomicBool::new(false));
        let worker = Worker {
            account: account.clone(),
            paths: self.paths.clone(),
            shared,
            stop: stop.clone(),
            smtp: self.smtp.clone(),
            finish: self.finish.clone(),
            clock: self.clock.clone(),
        };
        let spawned = std::thread::Builder::new()
            .name(format!("send-{}", account.name))
            .spawn(move || worker.run());
        if let Err(e) = spawned {
            log::error!("{}: could not start its sender thread: {e}", account.name);
            return None;
        }
        Some(AccountSender {
            config: account.clone(),
            stop,
        })
    }

    /// The outbox of `account` while it is configured; a removed account's outbox stays on disk, untouched.
    fn shared(&self, account: &str) -> io::Result<&Arc<Shared>> {
        self.accounts
            .iter()
            .any(|sender| sender.config.name == account)
            .then(|| self.shared.get(account))
            .flatten()
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("no account named '{account}'"),
                )
            })
    }

    /// Each configured account's entries.
    fn each_outbox(&self) -> impl Iterator<Item = (&str, Vec<Entry>)> {
        self.accounts.iter().filter_map(|sender| {
            let name = sender.config.name.as_str();
            let shared = self.shared.get(name)?;
            let _lock = shared.lock();
            Some((name, shared.outbox.list().unwrap_or_default()))
        })
    }

    /// Puts a message in `account`'s outbox and wakes its thread.
    pub fn queue(&self, account: &str, keep: &[u8], entry: &Entry) -> io::Result<()> {
        let shared = self.shared(account)?;
        {
            let _lock = shared.lock();
            shared.outbox.add(keep, entry)?;
        }
        log::info!("{account}: queued {}", entry.id);
        shared.wake();
        Ok(())
    }

    /// Takes back a message not yet handed to the server.
    pub fn cancel(&self, account: &str, id: &str) -> Result<Vec<u8>, CancelError> {
        let shared = self
            .shared(account)
            .map_err(|_| CancelError::NotFound(id.to_string()))?;
        let raw = {
            let _lock = shared.lock();
            shared.outbox.cancel(id)?
        };
        log::info!("{account}: cancelled {id}");
        Ok(raw)
    }

    /// Sends a waiting or failed message now; a failed one starts its attempts over.
    pub fn retry(&self, account: &str, id: &str) -> io::Result<()> {
        let shared = self.shared(account)?;
        {
            let _lock = shared.lock();
            let mut entry = shared.outbox.get(id)?;
            match entry.state {
                State::Waiting => {}
                State::Failed { .. } => {
                    entry.state = State::Waiting;
                    entry.attempts = 0;
                    entry.first_try = None;
                }
                State::Sending | State::Sent => {
                    return Err(io::Error::other(format!(
                        "message {id} is already on its way"
                    )));
                }
            }
            shared.outbox.raw(id)?;
            entry.due = (self.clock)();
            shared.outbox.write(&entry)?;
        }
        log::info!("{account}: retrying {id}");
        shared.wake();
        Ok(())
    }

    /// The entry `id` of `account`.
    pub fn entry(&self, account: &str, id: &str) -> io::Result<Entry> {
        let shared = self.shared(account)?;
        let _lock = shared.lock();
        shared.outbox.get(id)
    }

    /// Whether message `id` of `account` left the outbox because it was sent, rather than cancelled; remembered for the
    /// last few hundred sent messages, the time a caller waits on one.
    pub fn was_sent(&self, account: &str, id: &str) -> bool {
        self.shared(account).is_ok_and(|shared| {
            shared
                .sent
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .iter()
                .any(|sent| sent == id)
        })
    }

    /// The message `id` of `account` as kept.
    pub fn raw(&self, account: &str, id: &str) -> io::Result<Vec<u8>> {
        self.shared(account)?.outbox.raw(id)
    }

    /// Every entry of every account, by account.
    pub fn entries(&self) -> Vec<(String, Entry)> {
        self.each_outbox()
            .flat_map(|(name, entries)| entries.into_iter().map(move |e| (name.to_string(), e)))
            .collect()
    }

    /// How many messages wait and failed, per account.
    pub fn counts(&self) -> Vec<(String, OutboxCounts)> {
        self.each_outbox()
            .map(|(name, entries)| (name.to_string(), counts_of(&entries)))
            .collect()
    }

    /// Whether any account has a message to send, being sent or retried, or sent with IMAP work left: the daemon does
    /// not exit for idleness while it has.
    pub fn busy(&self) -> bool {
        self.each_outbox()
            .flat_map(|(_, entries)| entries)
            .any(|entry| match entry.state {
                State::Waiting | State::Sending | State::Sent => true,
                State::Failed { retrying, .. } => retrying,
            })
    }

    /// The IMAP side's report for a sent message: removes it once its copy, its flag and its draft's delete are done.
    pub fn finished(
        &self,
        account: &str,
        id: &str,
        copied: bool,
        flagged: bool,
        draft_deleted: bool,
    ) {
        let Ok(shared) = self.shared(account) else {
            return;
        };
        let _lock = shared.lock();
        let Ok(mut entry) = shared.outbox.get(id) else {
            return;
        };
        entry.copied |= copied;
        entry.flagged |= flagged;
        entry.draft_deleted |= draft_deleted;
        let result = if entry.finished() {
            shared.remove_sent(id)
        } else {
            shared.outbox.write(&entry)
        };
        if let Err(e) = result {
            log::warn!("{account}: outbox {id}: {e}");
        }
    }

    /// Wakes `account`'s thread to look at its outbox now.
    pub fn wake(&self, account: &str) {
        if let Ok(shared) = self.shared(account) {
            shared.wake();
        }
    }

    /// Signals every thread to stop. A send in progress is not waited for: were the process to end during it, the
    /// message stays `sending` and is never sent again.
    pub fn stop(self) {
        for sender in &self.accounts {
            sender.stop.store(true, Ordering::Release);
        }
        for shared in self.shared.values() {
            shared.wake();
        }
    }
}

/// The daemon's senders, as the sync threads queue their rules' forwards: refused once they stopped.
impl crate::rules::apply::ForwardQueue for Mutex<Option<Senders>> {
    fn queue(&self, account: &str, keep: Vec<u8>, entry: Entry) -> io::Result<()> {
        let senders = self
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match senders.as_ref() {
            Some(senders) => senders.queue(account, &keep, &entry),
            None => Err(io::Error::other("the sender threads have stopped")),
        }
    }
}

struct Worker {
    account: AccountConfig,
    paths: Paths,
    shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
    smtp: SmtpConnector,
    finish: Sender<(String, String)>,
    clock: Clock,
}

/// Why an attempt failed, as the outbox shows it, and whether trying again later may work.
struct Failure {
    reason: String,
    temporary: bool,
}

impl From<SmtpError> for Failure {
    fn from(error: SmtpError) -> Self {
        Failure {
            temporary: error.is_temporary(),
            reason: error.to_string(),
        }
    }
}

/// What a send needs from outside the outbox.
struct Ready {
    store: Option<Store>,
    server: super::SmtpServer,
    secret: credentials::Secret,
    /// The message without Bcc.
    transmit: Vec<u8>,
}

impl Worker {
    fn run(self) {
        if !self.shared.recovered.swap(true, Ordering::AcqRel) {
            let _lock = self.shared.lock();
            if let Err(e) = self.shared.outbox.recover() {
                log::error!("{}: the outbox cannot be recovered: {e}", self.account.name);
            }
        }
        let mut last_finish = i64::MIN;
        while !self.stop.load(Ordering::Acquire) {
            let now = (self.clock)();
            let mut entries = self.list();
            if now.saturating_sub(last_finish) >= FINISH_EVERY_SECS {
                last_finish = now;
                for entry in entries.iter().filter(|e| e.state == State::Sent) {
                    let _ = self
                        .finish
                        .send((self.account.name.clone(), entry.id.clone()));
                }
            }
            let mut tried = false;
            for entry in entries.iter().filter(|entry| due(entry, now)) {
                if self.stop.load(Ordering::Acquire) {
                    return;
                }
                self.send(&entry.id);
                tried = true;
            }
            if tried {
                entries = self.list();
            }
            let next = entries
                .iter()
                .filter(|e| {
                    matches!(
                        e.state,
                        State::Waiting | State::Failed { retrying: true, .. }
                    )
                })
                .map(|e| e.due)
                .min();
            let now = (self.clock)();
            match next {
                Some(due) if due <= now => {}
                Some(due) => self.shared.sleep(due - now),
                None => self.shared.sleep(LONGEST_SLEEP_SECS),
            }
        }
    }

    fn list(&self) -> Vec<Entry> {
        let _lock = self.shared.lock();
        self.shared.outbox.list().unwrap_or_default()
    }

    /// The entry `id` while it is still due, read under the lock.
    fn due_entry(&self, id: &str, now: i64) -> Option<Entry> {
        self.shared
            .outbox
            .get(id)
            .ok()
            .filter(|entry| due(entry, now))
    }

    /// Sends message `id` if it is still due.
    fn send(&self, id: &str) {
        let name = &self.account.name;
        let now = (self.clock)();
        // The slow part first, outside the lock, so a cancel or a status never waits on it: the store, the password
        // (a keyring prompt or a command) and the message file.
        let ready = match self.prepare(id) {
            Ok(ready) => ready,
            Err(failure) => {
                let entry = {
                    let _lock = self.shared.lock();
                    self.due_entry(id, now)
                };
                if let Some(entry) = entry {
                    self.failed(entry, failure, now);
                }
                return;
            }
        };
        let marked = {
            let _lock = self.shared.lock();
            let Some(mut entry) = self.due_entry(id, now) else {
                return;
            };
            entry.state = State::Sending;
            entry.attempts += 1;
            entry.first_try.get_or_insert(now);
            // Synced before the server sees a byte: after a crash, this message is never sent again.
            match self.shared.outbox.write(&entry) {
                Ok(()) => Ok(entry),
                Err(e) => Err((
                    entry,
                    Failure {
                        reason: format!("the outbox cannot be written: {e}"),
                        temporary: true,
                    },
                )),
            }
        };
        let mut entry = match marked {
            Ok(entry) => entry,
            Err((entry, failure)) => return self.failed(entry, failure, now),
        };
        log::info!("{name}: sending {id}");
        let result = (self.smtp)().send(
            &ready.server,
            &ready.secret,
            self.account.ca_file.as_deref(),
            &entry.envelope,
            &ready.transmit,
        );
        let now = (self.clock)();
        let caps = match result {
            Ok(caps) => caps,
            Err(error) => return self.failed(entry, error.into(), now),
        };
        log::info!("{name}: sent {id}");
        if let Some(store) = &ready.store
            && let Err(e) = store.save_smtp_capabilities(caps.size, caps.eight_bit_mime)
        {
            log::warn!("{name}: the SMTP capabilities were not stored: {e}");
        }
        let _lock = self.shared.lock();
        entry.state = State::Sent;
        let written = if entry.finished() {
            self.shared.remove_sent(id)
        } else {
            self.shared.outbox.write(&entry)
        };
        if let Err(e) = written {
            log::error!("{name}: outbox {id} was sent but could not be updated: {e}");
        }
        if !entry.finished() {
            let _ = self.finish.send((name.clone(), id.to_string()));
        }
    }

    /// What sending `id` needs from outside the outbox; a failure that needs no server says why.
    fn prepare(&self, id: &str) -> Result<Ready, Failure> {
        let store = Store::open_account(&self.paths, &self.account.name).ok();
        let state = store
            .as_ref()
            .and_then(|store| store.smtp_state().ok().flatten());
        let server = server_for(&self.account, state.as_ref()).ok_or_else(|| Failure {
            reason: NOT_SET_UP.into(),
            temporary: false,
        })?;
        // A locked keyring or a password command that failed may work later.
        let secret = credentials::check_command_trusted(&self.account, &self.paths.config_file())
            .and_then(|()| credentials::resolve_smtp(&self.account))
            .map_err(|e| Failure {
                reason: e.to_string(),
                temporary: true,
            })?;
        let raw = self.shared.outbox.raw(id).map_err(|e| Failure {
            reason: format!("the message file cannot be read: {e}"),
            temporary: false,
        })?;
        Ok(Ready {
            store,
            server,
            secret,
            transmit: super::check::without_bcc(&raw),
        })
    }

    /// Records a failed attempt: tried again later while temporary and within the hour, else failed for good.
    fn failed(&self, mut entry: Entry, failure: Failure, now: i64) {
        let name = &self.account.name;
        let first = *entry.first_try.get_or_insert(now);
        let retry = failure.temporary && now.saturating_sub(first) < RETRY_FOR_SECS;
        // The reason may hold the server's words; it goes to the outbox, never the log.
        entry.state = State::Failed {
            reason: failure.reason,
            retrying: retry,
        };
        if retry {
            entry.due = now + backoff(entry.attempts);
            log::info!(
                "{name}: {} failed for now; trying again at {}",
                entry.id,
                entry.due
            );
        } else {
            log::warn!("{name}: {} failed", entry.id);
        }
        let _lock = self.shared.lock();
        if let Err(e) = self.shared.outbox.write(&entry) {
            log::error!("{name}: outbox {}: {e}", entry.id);
        }
    }
}

/// Whether `entry` should be tried at `now`.
fn due(entry: &Entry, now: i64) -> bool {
    matches!(
        entry.state,
        State::Waiting | State::Failed { retrying: true, .. }
    ) && entry.due <= now
}

/// How long to wait after `attempts` tries: a minute, two, five, then ten.
fn backoff(attempts: u32) -> i64 {
    match attempts {
        0 | 1 => 60,
        2 => 120,
        3 => 300,
        _ => 600,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicI64;
    use std::sync::mpsc;
    use std::time::Instant;

    use super::*;
    use crate::config::{PasswordSource, SmtpConfig, SmtpTls};
    use crate::send::Envelope;
    use crate::send::outbox::{MAY_HAVE_BEEN_SENT, Original};
    use crate::send::reply::OriginalFlag;
    use crate::send::smtp::SharedSmtp;

    struct Harness {
        _dir: tempfile::TempDir,
        smtp: SharedSmtp,
        clock: Arc<AtomicI64>,
        finish: mpsc::Receiver<(String, String)>,
        senders: Option<Senders>,
    }

    fn account(smtp: bool) -> AccountConfig {
        let mut account = AccountConfig::new(
            "work",
            "imap.example.com",
            "me@example.com",
            PasswordSource::Command {
                command: "printf secret".into(),
            },
        );
        if smtp {
            account.smtp = Some(SmtpConfig::new("smtp.example.com", SmtpTls::Implicit));
        }
        account
    }

    impl Harness {
        fn new(smtp: bool) -> Harness {
            Harness::with(smtp, |_| {})
        }

        /// A harness whose outbox `before` fills before the threads start.
        fn with(smtp: bool, before: impl FnOnce(&Outbox)) -> Harness {
            let dir = tempfile::tempdir().unwrap();
            let paths = Paths::under(dir.path());
            paths.ensure_account("work").unwrap();
            before(&Outbox::open(&paths, "work").unwrap());
            let fake = SharedSmtp::default();
            let clock = Arc::new(AtomicI64::new(1_000_000));
            let (tx, finish) = mpsc::channel();
            let config = Config {
                accounts: vec![account(smtp)],
                ..Default::default()
            };
            let connector: SmtpConnector = {
                let fake = fake.clone();
                Arc::new(move || Box::new(fake.clone()))
            };
            let time = clock.clone();
            let senders = Senders::start_with_clock(
                &config,
                &paths,
                connector,
                tx,
                Arc::new(move || time.load(Ordering::SeqCst)),
            );
            Harness {
                _dir: dir,
                smtp: fake,
                clock,
                finish,
                senders: Some(senders),
            }
        }

        fn senders(&self) -> &Senders {
            self.senders.as_ref().unwrap()
        }

        fn now(&self) -> i64 {
            self.clock.load(Ordering::SeqCst)
        }

        fn advance(&self, secs: i64) {
            self.clock.fetch_add(secs, Ordering::SeqCst);
            self.senders().wake("work");
        }

        fn sent(&self) -> usize {
            self.smtp.with(|s| s.sent.len())
        }

        fn queue(&self, due: i64, original: bool) -> Entry {
            let entry = new_entry(due, original);
            self.senders().queue("work", MESSAGE, &entry).unwrap();
            entry
        }

        fn state(&self, id: &str) -> Option<State> {
            self.senders().entry("work", id).ok().map(|e| e.state)
        }
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            if let Some(senders) = self.senders.take() {
                senders.stop();
            }
        }
    }

    const MESSAGE: &[u8] =
        b"From: me@example.com\r\nTo: a@x.org\r\nBcc: hidden@y.org\r\nSubject: s\r\n\r\nbody\r\n";

    fn new_entry(due: i64, original: bool) -> Entry {
        Entry::new(
            Envelope {
                from: "me@example.com".into(),
                to: vec!["a@x.org".into(), "hidden@y.org".into()],
            },
            due,
            false,
            original.then(|| Original {
                message_id: "<m@x.org>".into(),
                flag: OriginalFlag::Answered,
            }),
        )
    }

    fn until(what: &str, mut done: impl FnMut() -> bool) {
        let start = Instant::now();
        while !done() {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "timed out waiting for {what}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn a_message_waits_out_its_delay() {
        let h = Harness::new(true);
        let entry = h.queue(h.now() + 10, false);
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(h.sent(), 0);
        assert_eq!(h.state(&entry.id), Some(State::Waiting));
        assert!(h.senders().busy());
        h.advance(10);
        until("the send", || h.sent() == 1);
        let (server, envelope, raw) = h.smtp.with(|s| s.sent[0].clone());
        assert_eq!(server.host, "smtp.example.com");
        assert_eq!(server.username, "me@example.com");
        assert_eq!(envelope.to, ["a@x.org", "hidden@y.org"]);
        assert!(!String::from_utf8(raw).unwrap().contains("hidden"));
        // Nothing left to do on the IMAP side: it is gone.
        until("the removal", || h.state(&entry.id).is_none());
        assert!(!h.senders().busy());
    }

    #[test]
    fn a_sent_message_is_told_apart_from_a_cancelled_one() {
        let h = Harness::new(true);
        let sent = h.queue(h.now(), false);
        until("the send", || h.state(&sent.id).is_none());
        assert!(h.senders().was_sent("work", &sent.id));
        let cancelled = h.queue(h.now() + 10, false);
        h.senders().cancel("work", &cancelled.id).unwrap();
        assert!(!h.senders().was_sent("work", &cancelled.id));
    }

    #[test]
    fn cancel_before_due_sends_nothing() {
        let h = Harness::new(true);
        let entry = h.queue(h.now() + 10, false);
        assert_eq!(h.senders().cancel("work", &entry.id).unwrap(), MESSAGE);
        h.advance(20);
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(h.sent(), 0);
        assert!(h.senders().entries().is_empty());
    }

    #[test]
    fn retry_sends_a_waiting_message_before_its_due_time() {
        let h = Harness::new(true);
        let entry = h.queue(h.now() + 10, false);
        h.senders().retry("work", &entry.id).unwrap();
        until("the send", || h.sent() == 1);
    }

    #[test]
    fn restart_after_a_crash_in_sending_does_not_resend() {
        let entry = new_entry(0, false);
        let h = Harness::with(true, |outbox| {
            outbox.add(MESSAGE, &entry).unwrap();
            outbox.set_state(&entry.id, State::Sending).unwrap();
        });
        until("the recovery", || h.senders().counts()[0].1.failed == 1);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(h.sent(), 0);
        assert_eq!(
            h.state(&entry.id),
            Some(State::Failed {
                reason: MAY_HAVE_BEEN_SENT.into(),
                retrying: false
            })
        );
        assert!(!h.senders().busy());
        // Retried by hand, it goes.
        h.senders().retry("work", &entry.id).unwrap();
        until("the retried send", || h.sent() == 1);
    }

    #[test]
    fn due_messages_go_at_start() {
        let entry = new_entry(5, false);
        let h = Harness::with(true, |outbox| outbox.add(MESSAGE, &entry).unwrap());
        until("the send", || h.sent() == 1);
    }

    #[test]
    fn a_temporary_failure_is_retried_then_fails_after_an_hour() {
        let h = Harness::new(true);
        h.smtp.with(|s| {
            s.script
                .extend((0..100).map(|_| Err(SmtpError::Temporary("451 later".into()))));
        });
        let entry = h.queue(h.now(), false);
        until("the first try", || {
            matches!(
                h.state(&entry.id),
                Some(State::Failed { retrying: true, .. })
            )
        });
        assert_eq!(
            h.senders().counts()[0].1,
            OutboxCounts {
                waiting: 1,
                failed: 0
            }
        );
        assert!(h.senders().busy());
        let tries = || h.smtp.with(|s| s.script.len());
        let before = tries();
        h.advance(60);
        until("the second try", || tries() < before);
        h.advance(RETRY_FOR_SECS);
        until("the failure", || {
            matches!(
                h.state(&entry.id),
                Some(State::Failed {
                    retrying: false,
                    ..
                })
            )
        });
        match h.state(&entry.id) {
            Some(State::Failed { reason, .. }) => assert!(reason.contains("451 later"), "{reason}"),
            other => panic!("{other:?}"),
        }
        assert_eq!(
            h.senders().counts()[0].1,
            OutboxCounts {
                waiting: 0,
                failed: 1
            }
        );
        assert!(!h.senders().busy());
        assert_eq!(h.sent(), 0);
    }

    #[test]
    fn a_permanent_failure_fails_at_once_with_the_reason() {
        let h = Harness::new(true);
        h.smtp.with(|s| {
            s.script
                .push_back(Err(SmtpError::Permanent("550 no such user".into())))
        });
        let entry = h.queue(h.now(), false);
        until("the failure", || {
            matches!(h.state(&entry.id), Some(State::Failed { .. }))
        });
        match h.state(&entry.id) {
            Some(State::Failed { reason, retrying }) => {
                assert!(reason.contains("550 no such user"));
                assert!(!retrying);
            }
            other => panic!("{other:?}"),
        }
        // A failed one can be cancelled, returning it.
        assert_eq!(h.senders().cancel("work", &entry.id).unwrap(), MESSAGE);
    }

    #[test]
    fn busy_while_waiting_and_until_finished() {
        let h = Harness::new(true);
        let entry = h.queue(h.now(), true);
        assert!(h.senders().busy());
        let (account, id) = h.finish.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!((account.as_str(), id.as_str()), ("work", entry.id.as_str()));
        assert_eq!(h.state(&entry.id), Some(State::Sent));
        assert!(h.senders().busy());
        assert!(matches!(
            h.senders().cancel("work", &entry.id),
            Err(CancelError::TooLate(_))
        ));
        h.senders().finished("work", &entry.id, true, false, false);
        assert!(h.senders().busy());
        h.senders().finished("work", &entry.id, false, true, false);
        assert_eq!(h.state(&entry.id), None);
        assert!(!h.senders().busy());
    }

    fn queue_draft(h: &Harness, due: i64) -> Entry {
        let entry = Entry {
            draft: vec!["<d1@x>".into()],
            ..new_entry(due, false)
        };
        h.senders().queue("work", MESSAGE, &entry).unwrap();
        entry
    }

    #[test]
    fn a_sent_draft_waits_for_its_delete() {
        let h = Harness::new(true);
        let entry = queue_draft(&h, h.now());
        let (_, id) = h.finish.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(id, entry.id);
        h.senders().finished("work", &entry.id, true, true, false);
        assert_eq!(h.state(&entry.id), Some(State::Sent));
        h.senders().finished("work", &entry.id, false, false, true);
        assert_eq!(h.state(&entry.id), None);
    }

    #[test]
    fn a_cancelled_or_failed_message_leaves_its_draft() {
        let h = Harness::new(true);
        let cancelled = queue_draft(&h, h.now() + 10);
        h.senders().cancel("work", &cancelled.id).unwrap();
        h.smtp.with(|s| {
            s.script
                .push_back(Err(SmtpError::Permanent("550 no such user".into())))
        });
        let failed = queue_draft(&h, h.now());
        until("the failure", || {
            matches!(h.state(&failed.id), Some(State::Failed { .. }))
        });
        h.advance(FINISH_EVERY_SECS);
        std::thread::sleep(Duration::from_millis(200));
        assert!(
            h.finish.try_recv().is_err(),
            "nothing is handed to the IMAP side"
        );
    }

    #[test]
    fn an_account_without_a_server_fails_with_sending_not_set_up() {
        let h = Harness::new(false);
        let entry = h.queue(h.now(), false);
        until("the failure", || {
            matches!(h.state(&entry.id), Some(State::Failed { .. }))
        });
        match h.state(&entry.id) {
            Some(State::Failed {
                reason,
                retrying: false,
            }) => {
                assert!(reason.contains("not set up"), "{reason}")
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(h.sent(), 0);
    }
}
