//! Reading the opened message from its store on a thread of its own. The raw message and the MIME parse that finds its
//! attachments and HTML cost milliseconds per megabyte, which a frame cannot spend. The window waits a short budget for
//! the answer, so most mail still opens in the frame that chose it. Requests coalesce, so stepping through the list
//! fast reads only the message the cursor stopped on.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use postvak_core::message::{self, Attachment, HtmlBody};
use postvak_core::paths::Paths;
use postvak_core::store::{Message, Store};

use super::html::Wake;
use super::list::RowKey;

const THREAD: &str = "postvak-reader";

/// What one read found of a message.
#[derive(Debug, Default)]
pub(crate) struct Read {
    /// `None` when the store no longer holds the message.
    pub message: Option<Message>,
    pub attachments: Vec<Attachment>,
    pub html: Option<HtmlBody>,
    /// Whether the store holds the raw bytes, which the HTML view and the attachments need.
    pub has_raw: bool,
}

/// One message to read, numbered so an answer that a later request overtook is dropped.
pub(crate) struct Request {
    pub generation: u64,
    /// The account's name, which names its store.
    pub account: String,
    pub key: RowKey,
}

/// How the thread reads a message from an open store; tests swap it to hold a read back.
pub(crate) type ReadFn = fn(&Store, &RowKey) -> Read;

/// Reads `key` as stored now: the row, and from the raw bytes, when there are some, the attachments and the HTML.
pub(crate) fn read(store: &Store, key: &RowKey) -> Read {
    let raw = store.raw(&key.0, key.1).ok().flatten();
    let (attachments, html) = raw
        .as_deref()
        .map(message::attachments_and_html)
        .unwrap_or_default();
    Read {
        message: store.message(&key.0, key.1).ok().flatten(),
        attachments,
        html,
        has_raw: raw.is_some(),
    }
}

/// The read thread and its two channels. Dropping it ends the thread once it finishes its current read.
pub(crate) struct Reader {
    paths: Paths,
    read: ReadFn,
    wake: Wake,
    replies: Receiver<(u64, Read)>,
    requests: Sender<Request>,
}

impl Reader {
    /// Starts the thread; `wake` runs after each answer, so the window shows it.
    pub fn start(paths: Paths, read: ReadFn, wake: Wake) -> Reader {
        let (requests, jobs) = mpsc::channel();
        let (done, replies) = mpsc::channel();
        let (thread_paths, waker) = (paths.clone(), wake.clone());
        let spawned = std::thread::Builder::new()
            .name(THREAD.into())
            .spawn(move || run(&thread_paths, read, &jobs, &done, &waker));
        if let Err(e) = spawned {
            log::warn!("could not start the message reader: {e}");
        }
        Reader {
            paths,
            read,
            wake,
            replies,
            requests,
        }
    }

    /// Sends a request; when the thread has gone, starts a new one for it.
    pub fn send(&mut self, request: Request) {
        if let Err(mpsc::SendError(request)) = self.requests.send(request) {
            *self = Reader::start(self.paths.clone(), self.read, self.wake.clone());
            let _ = self.requests.send(request);
        }
    }

    /// Waits at most `budget` for the answer to `generation`, dropping older ones on the way.
    pub fn wait(&self, generation: u64, budget: Duration) -> Option<Read> {
        let deadline = Instant::now() + budget;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.replies.recv_timeout(left) {
                Ok((answered, read)) if answered == generation => return Some(read),
                Ok(_) => {}
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => return None,
            }
        }
    }

    /// The answers that came since the last call.
    pub fn replies(&self) -> Vec<(u64, Read)> {
        self.replies.try_iter().collect()
    }
}

/// Reads until the app hangs up, each time only the newest request, over a connection of its own: SQLite's
/// write-ahead log lets it read while the daemon writes.
fn run(
    paths: &Paths,
    read: ReadFn,
    jobs: &Receiver<Request>,
    done: &Sender<(u64, Read)>,
    wake: &Wake,
) {
    while let Ok(first) = jobs.recv() {
        let request = jobs.try_iter().last().unwrap_or(first);
        let found = match Store::open_account(paths, &request.account) {
            Ok(store) => read(&store, &request.key),
            Err(e) => {
                log::warn!(
                    "[{}] could not open the store to read a message: {e}",
                    request.account
                );
                Read::default()
            }
        };
        if done.send((request.generation, found)).is_err() {
            return;
        }
        wake();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;
    use crate::body;
    use crate::state::App;
    use crate::test_support::{Fixture, message};

    fn shown(app: &App) -> Option<u32> {
        app.body
            .as_ref()
            .and_then(|b| b.message.as_ref())
            .map(|m| m.uid)
    }

    fn inbox(fx: &Fixture, uids: &[u32]) {
        for &uid in uids {
            let mut m = message("INBOX", uid, &format!("subject {uid}"));
            m.body_text = Some(format!("text {uid}"));
            fx.add("work", m);
        }
    }

    #[test]
    fn a_read_that_outlasts_the_budget_shows_loading_then_the_message() {
        static RELEASE: AtomicBool = AtomicBool::new(false);
        fn held(store: &Store, key: &RowKey) -> Read {
            while !RELEASE.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(1));
            }
            read(store, key)
        }
        let fx = Fixture::new(&["work"]);
        inbox(&fx, &[1, 2]);
        let (mut driver, _wires) = fx.driver();
        assert_eq!(shown(&driver.app), Some(2));
        driver.app.body_budget = Duration::ZERO;
        driver.app.read_body = held;
        driver.app.reader = None;
        driver.press("j");
        assert_eq!(
            shown(&driver.app),
            None,
            "the frame does not wait for the read"
        );
        assert_eq!(body::pane(&driver.app).empty, Some("Loading…"));
        RELEASE.store(true, Ordering::SeqCst);
        driver.tick_until(|app| shown(app).is_some());
        assert_eq!(shown(&driver.app), Some(1));
        let text = driver.app.body.as_ref().and_then(|b| b.text.clone());
        assert_eq!(text.as_deref(), Some("text 1"));
    }

    #[test]
    fn stepping_through_the_list_reads_only_where_the_cursor_stopped() {
        static RELEASE: AtomicBool = AtomicBool::new(false);
        static READS: Mutex<Vec<u32>> = Mutex::new(Vec::new());
        fn held(store: &Store, key: &RowKey) -> Read {
            READS.lock().unwrap().push(key.1);
            while !RELEASE.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(1));
            }
            read(store, key)
        }
        let fx = Fixture::new(&["work"]);
        inbox(&fx, &[1, 2, 3, 4]);
        let (mut driver, _wires) = fx.driver();
        driver.app.body_budget = Duration::ZERO;
        driver.app.read_body = held;
        driver.app.reader = None;
        driver.press("j");
        driver.tick_until(|_| READS.lock().unwrap().len() == 1);
        driver.press("j j");
        RELEASE.store(true, Ordering::SeqCst);
        driver.tick_until(|app| shown(app).is_some());
        assert_eq!(shown(&driver.app), Some(1));
        assert_eq!(*READS.lock().unwrap(), [3, 1], "message 2 was passed over");
    }

    #[test]
    fn a_message_gone_from_the_store_says_so_after_the_read() {
        let fx = Fixture::new(&["work"]);
        inbox(&fx, &[1]);
        let (mut driver, wires) = fx.driver();
        assert_eq!(shown(&driver.app), Some(1));
        fx.store("work").remove_message("INBOX", 1).unwrap();
        let ready = postvak_core::sync::Event::BodyReady {
            account: "work".into(),
            folder: "INBOX".into(),
            uid: 1,
            request: 0,
        };
        driver.receive(&wires, [ready]);
        assert_eq!(
            body::pane(&driver.app).empty,
            Some("This message is no longer in the local store.")
        );
    }

    #[cfg(unix)]
    #[test]
    fn reading_for_an_account_with_no_directory_yet_makes_it_private() {
        use std::os::unix::fs::PermissionsExt;
        let fx = Fixture::new(&["work"]);
        let mut reader = Reader::start(fx.paths.clone(), read, std::sync::Arc::new(|| {}));
        reader.send(Request {
            generation: 1,
            account: "new".into(),
            key: ("INBOX".into(), 1),
        });
        assert!(reader.wait(1, Duration::from_secs(5)).is_some());
        let meta = std::fs::metadata(fx.paths.account_dir("new")).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o700);
    }
}
