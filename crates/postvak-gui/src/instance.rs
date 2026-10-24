//! One window per user. The first `postvak gui` holds `gui.lock` and, once its window shows, listens on `gui.sock`; a
//! second one hands it its request and exits, so a start from the dock or the launcher brings back the window that is
//! already open instead of opening another.
use std::fs::File;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use postvak_core::paths::{self, Paths};

/// How long a second start waits for the first window to listen, which it does once it shows.
const WAIT_FOR_FIRST: Duration = Duration::from_secs(3);
const TRY_EVERY: Duration = Duration::from_millis(100);
/// How long either side waits on the other once connected.
const ANSWER: Duration = Duration::from_secs(2);

/// What a second start asks the first window, beyond coming to the front. One JSON line; a window ignores fields it
/// does not know, so a newer start can ask an older window.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub(crate) struct Request {}

pub(crate) enum Claim {
    /// This is the first window; held until the process ends.
    First(Instance),
    /// The first window took the request.
    Handed,
    /// No lock to take, or a first window that did not answer: this start goes on as if alone.
    Alone,
}

pub(crate) struct Instance {
    _lock: File,
    socket: PathBuf,
}

/// Takes the lock, or hands `request` to the window that holds it.
pub(crate) fn claim(paths: &Paths, request: &Request) -> Claim {
    match take_lock(paths) {
        Ok(Some(lock)) => Claim::First(Instance {
            _lock: lock,
            socket: paths.gui_socket(),
        }),
        Ok(None) => match hand_over(&paths.gui_socket(), request, WAIT_FOR_FIRST) {
            Ok(()) => Claim::Handed,
            Err(e) => {
                log::warn!(
                    "another Postvak window holds the lock but did not answer ({e}); opening this one too"
                );
                Claim::Alone
            }
        },
        Err(e) => {
            log::warn!("could not take {}: {e}", paths.gui_lock().display());
            Claim::Alone
        }
    }
}

/// The lock, or None when another process holds it.
fn take_lock(paths: &Paths) -> io::Result<Option<File>> {
    // The private state directory also guards the socket between `bind` and its chmod.
    paths::create_private_dir(&paths.state_dir)?;
    paths::try_lock_file(&paths.gui_lock())
}

/// Sends `request` and waits for the answer, trying to connect for up to `wait` while the first window starts.
fn hand_over(socket: &Path, request: &Request, wait: Duration) -> io::Result<()> {
    let mut waited = Duration::ZERO;
    let stream = loop {
        match UnixStream::connect(socket) {
            Ok(stream) => break stream,
            Err(e) if waited >= wait => return Err(e),
            Err(_) => {
                std::thread::sleep(TRY_EVERY);
                waited += TRY_EVERY;
            }
        }
    };
    stream.set_read_timeout(Some(ANSWER))?;
    stream.set_write_timeout(Some(ANSWER))?;
    let mut line = serde_json::to_string(request)?;
    line.push('\n');
    (&stream).write_all(line.as_bytes())?;
    let mut answer = String::new();
    BufReader::new(&stream).read_line(&mut answer)?;
    if answer.trim() == "ok" {
        Ok(())
    } else {
        Err(io::Error::other(format!("unexpected answer {answer:?}")))
    }
}

impl Instance {
    /// Answers later starts on a thread of its own; `on_request` runs there for each one.
    pub(crate) fn listen(&self, on_request: impl Fn(Request) + Send + 'static) -> io::Result<()> {
        // The lock is ours, so a socket file left here is from a window that died.
        let listener = paths::bind_private(&self.socket)?;
        std::thread::Builder::new()
            .name("gui-instance".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    match stream.and_then(|stream| answer(&stream)) {
                        Ok(request) => on_request(request),
                        Err(e) => log::debug!("a second start could not be answered: {e}"),
                    }
                }
            })?;
        Ok(())
    }
}

/// Reads one request and says it was taken.
fn answer(stream: &UnixStream) -> io::Result<Request> {
    stream.set_read_timeout(Some(ANSWER))?;
    stream.set_write_timeout(Some(ANSWER))?;
    let mut line = String::new();
    // A request is a few hundred bytes at most.
    BufReader::new(io::Read::take(stream, 64 * 1024)).read_line(&mut line)?;
    let request = serde_json::from_str(&line).map_err(io::Error::other)?;
    let mut stream = stream;
    stream.write_all(b"ok\n")?;
    Ok(request)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_start_hands_its_request_to_the_first() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let Claim::First(first) = claim(&paths, &Request::default()) else {
            panic!("the first start did not take the lock");
        };
        let (sent, received) = std::sync::mpsc::channel();
        first
            .listen(move |request| sent.send(request).unwrap())
            .unwrap();

        assert!(matches!(claim(&paths, &Request::default()), Claim::Handed));
        assert_eq!(received.recv_timeout(ANSWER).unwrap(), Request::default());
    }

    #[test]
    fn the_lock_frees_with_the_first_window() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let first = claim(&paths, &Request::default());
        assert!(matches!(first, Claim::First(_)));
        drop(first);
        assert!(matches!(
            claim(&paths, &Request::default()),
            Claim::First(_)
        ));
    }

    /// A first window that holds the lock but never listens, such as one stuck before it shows, does not keep a
    /// second start from opening.
    #[test]
    fn a_first_window_that_does_not_answer_leaves_this_one_alone() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let _first = claim(&paths, &Request::default());
        let error = hand_over(&paths.gui_socket(), &Request::default(), Duration::ZERO);
        assert!(error.is_err());
    }

    #[test]
    fn a_socket_left_by_a_window_that_died_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let Claim::First(first) = claim(&paths, &Request::default()) else {
            panic!("the first start did not take the lock");
        };
        std::fs::write(paths.gui_socket(), b"").unwrap();
        first.listen(|_| {}).unwrap();
        hand_over(&paths.gui_socket(), &Request::default(), ANSWER).unwrap();
    }

    /// A newer `postvak gui` may ask for more than this window knows; it still comes to the front.
    #[test]
    fn a_request_with_fields_this_window_does_not_know_is_taken() {
        let request: Request = serde_json::from_str(r#"{"open":{"uid":7}}"#).unwrap();
        assert_eq!(request, Request::default());
    }
}
