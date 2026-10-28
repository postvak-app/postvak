//! The window's background threads: one-off work whose answer arrives on a channel, and long-lived workers that take
//! jobs in turn. Every thread is named, and one the OS cannot start is logged rather than panicking: its channel then
//! reads as stopped.
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};

use crate::html::Wake;

/// Runs `f` on a thread called `name`; whether it started.
pub(crate) fn spawn(name: &str, f: impl FnOnce() + Send + 'static) -> bool {
    match std::thread::Builder::new().name(name.into()).spawn(f) {
        Ok(_) => true,
        Err(e) => {
            log::warn!("could not start the {name} thread: {e}");
            false
        }
    }
}

/// Runs `f` once on a thread called `name`; its answer arrives on the receiver, and `wake` runs once it has. When the
/// thread cannot start, the receiver reads as stopped.
pub(crate) fn oneshot<T: Send + 'static>(
    name: &str,
    wake: Wake,
    f: impl FnOnce() -> T + Send + 'static,
) -> Receiver<T> {
    let (done, answer) = mpsc::channel();
    spawn(name, move || {
        let _ = done.send(f());
        wake();
    });
    answer
}

/// A receiver whose sender went away without sending what was expected.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Stopped;

/// The next answer on `answer` without waiting: `None` while none is in, `Err(Stopped)` once the work ended without
/// one.
pub(crate) fn poll<T>(answer: &Receiver<T>) -> Option<Result<T, Stopped>> {
    match answer.try_recv() {
        Ok(value) => Some(Ok(value)),
        Err(TryRecvError::Empty) => None,
        Err(TryRecvError::Disconnected) => Some(Err(Stopped)),
    }
}

/// One thread that takes jobs in turn, sends each result back and wakes the window; it ends with the window.
pub(crate) struct Worker<J, R> {
    jobs: Sender<J>,
    done: Receiver<R>,
}

impl<J: Send + 'static, R: Send + 'static> Worker<J, R> {
    /// Starts the thread called `name`, which runs `work` on each job.
    pub fn start(name: &str, wake: Wake, mut work: impl FnMut(J) -> R + Send + 'static) -> Self {
        let (jobs, todo) = mpsc::channel::<J>();
        let (send, done) = mpsc::channel();
        spawn(name, move || {
            for job in todo {
                if send.send(work(job)).is_err() {
                    return;
                }
                wake();
            }
        });
        Worker { jobs, done }
    }

    /// Queues `job`; it is dropped when the thread is gone.
    pub fn send(&self, job: J) {
        let _ = self.jobs.send(job);
    }

    /// The results that are in.
    pub fn done(&self) -> Vec<R> {
        self.done.try_iter().collect()
    }
}

#[cfg(test)]
impl<J, R> Worker<J, R> {
    /// A worker with no thread: the test reads the jobs and answers in its place.
    pub fn fake() -> (Worker<J, R>, Receiver<J>, Sender<R>) {
        let (jobs, todo) = mpsc::channel();
        let (send, done) = mpsc::channel();
        (Worker { jobs, done }, todo, send)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use super::*;
    use crate::test_support::wait_until;

    fn counting() -> (Wake, Arc<AtomicUsize>) {
        let woken = Arc::new(AtomicUsize::new(0));
        let count = woken.clone();
        (
            Arc::new(move || {
                count.fetch_add(1, Ordering::SeqCst);
            }),
            woken,
        )
    }

    #[test]
    fn a_oneshot_sends_its_answer_then_wakes() {
        let (wake, woken) = counting();
        let answer = oneshot("test-oneshot", wake, || 42);
        assert_eq!(answer.recv_timeout(Duration::from_secs(5)), Ok(42));
        // The wake runs after the send.
        wait_until(|| woken.load(Ordering::SeqCst) == 1);
    }

    #[test]
    fn poll_tells_waiting_from_answered_from_stopped() {
        let (send, answer) = mpsc::channel();
        assert_eq!(poll(&answer), None);
        send.send(7).unwrap();
        assert_eq!(poll(&answer), Some(Ok(7)));
        drop(send);
        assert_eq!(poll::<i32>(&answer), Some(Err(Stopped)));
    }

    #[test]
    fn a_worker_answers_its_jobs_in_order() {
        let (wake, _) = counting();
        let worker = Worker::start("test-worker", wake, |n: u32| n * 2);
        for n in 1..=3 {
            worker.send(n);
        }
        let mut got = Vec::new();
        wait_until(|| {
            got.extend(worker.done());
            got.len() >= 3
        });
        assert_eq!(got, [2, 4, 6]);
    }
}
