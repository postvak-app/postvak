//! Downloads a message's remote content once the user asks: capped in count, size and time, public hosts only.
use std::collections::BTreeMap;
use std::io::Read;
#[cfg(test)]
use std::sync::mpsc::Sender;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{LazyLock, Mutex, PoisonError};
use std::time::{Duration, Instant};

use super::{FetchDone, FetchJob, MAX_REMOTE, Wake};
use crate::worker::spawn;
use postvak_core::net::{public, public_agent};

/// A larger response counts as failed.
const MAX_BYTES: u64 = 16 * 1024 * 1024;
const MAX_REDIRECTS: usize = 5;
const TIMEOUT: Duration = Duration::from_secs(15);
/// A host that does not answer the connect in this long counts as failed, so a dead tracker gives up early.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Downloads that finish within this long of the last batch wait for the next one, so the page is not laid out
/// again for every image.
const BATCH: Duration = Duration::from_millis(500);
const WORKERS: usize = 6;

/// Downloads each job on a thread of its own, so a later round (the images a stylesheet names) need not wait for a
/// slow earlier one. Each job answers in batches, the last one marked.
pub(crate) struct Fetcher {
    start: Box<dyn Fn(FetchJob) + Send>,
    done: Receiver<FetchDone>,
}

impl Fetcher {
    pub fn start(wake: Wake) -> Fetcher {
        let (send, done) = mpsc::channel();
        let start = move |job: FetchJob| {
            let tag = job.tag;
            let (thread_send, thread_wake) = (send.clone(), wake.clone());
            let started = spawn("html-fetch", move || {
                fetch_all(job.urls, get, |fetched, last| {
                    let _ = thread_send.send(FetchDone { tag, fetched, last });
                    thread_wake();
                });
            });
            if !started {
                let _ = send.send(FetchDone {
                    tag,
                    fetched: BTreeMap::new(),
                    last: true,
                });
                wake();
            }
        };
        Fetcher {
            start: Box::new(start),
            done,
        }
    }

    pub fn send(&self, job: FetchJob) {
        (self.start)(job);
    }

    pub fn done(&self) -> Vec<FetchDone> {
        self.done.try_iter().collect()
    }
}

#[cfg(test)]
impl Fetcher {
    /// A fetcher with no thread: the test reads the jobs and answers in its place.
    pub fn fake() -> (Fetcher, Receiver<FetchJob>, Sender<FetchDone>) {
        let (jobs, todo) = mpsc::channel();
        let (send, done) = mpsc::channel();
        let start = move |job| {
            let _ = jobs.send(job);
        };
        let fetcher = Fetcher {
            start: Box::new(start),
            done,
        };
        (fetcher, todo, send)
    }
}

/// Fetches `urls` on a few workers and hands `deliver` what finished, at most once per `BATCH`, then the rest with
/// `last` set. Each URL ends up in one batch, empty when `get` failed or the host is refused.
fn fetch_all(
    urls: Vec<String>,
    get: impl Fn(&str) -> Option<Vec<u8>> + Sync,
    mut deliver: impl FnMut(BTreeMap<String, Vec<u8>>, bool),
) {
    let queue = Mutex::new(urls.into_iter().take(MAX_REMOTE).collect::<Vec<_>>());
    let (send, results) = mpsc::channel();
    std::thread::scope(|scope| {
        for _ in 0..WORKERS {
            let (send, queue, get) = (send.clone(), &queue, &get);
            scope.spawn(move || {
                loop {
                    // A `while let` would hold the lock for the whole download, one at a time.
                    let next = queue.lock().unwrap_or_else(PoisonError::into_inner).pop();
                    let Some(url) = next else { return };
                    let bytes = public(&url).then(|| get(&url)).flatten();
                    if send.send((url, bytes.unwrap_or_default())).is_err() {
                        return;
                    }
                }
            });
        }
        drop(send);
        let mut batch = BTreeMap::new();
        let mut flushed = Instant::now();
        loop {
            let next = if batch.is_empty() {
                results.recv().map_err(|_| RecvTimeoutError::Disconnected)
            } else {
                results.recv_timeout(BATCH.saturating_sub(flushed.elapsed()))
            };
            match next {
                Ok((url, bytes)) => {
                    batch.insert(url, bytes);
                }
                Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => {}
            }
            if !batch.is_empty() && flushed.elapsed() >= BATCH {
                deliver(std::mem::take(&mut batch), false);
                flushed = Instant::now();
            }
        }
        deliver(batch, true);
    });
}

/// The body of a GET, when it answers 2xx within the limits and every hop is a public host. Nothing about the URL is
/// logged: it can identify the reader.
pub(crate) fn get(url: &str) -> Option<Vec<u8>> {
    // One agent for the whole process, so its pool keeps connections and TLS sessions alive between images.
    static AGENT: LazyLock<ureq::Agent> = LazyLock::new(|| public_agent(config()));
    get_from(url, public, &AGENT)
}

fn config() -> ureq::config::ConfigBuilder<ureq::typestate::AgentScope> {
    ureq::Agent::config_builder()
        .max_redirects(0)
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_global(Some(TIMEOUT))
}

/// `get` with the host check and agent given, so tests can serve from this machine. Redirects are followed here, so
/// each one is checked.
fn get_from(url: &str, allowed: impl Fn(&str) -> bool, agent: &ureq::Agent) -> Option<Vec<u8>> {
    let mut url = url.to_string();
    for _ in 0..=MAX_REDIRECTS {
        if !allowed(&url) {
            return None;
        }
        let response = agent.get(&url).call().ok()?;
        if response.status().is_redirection() {
            let location = response.headers().get("location")?.to_str().ok()?;
            url = blitz_traits::net::Url::parse(&url)
                .ok()?
                .join(location)
                .ok()?
                .to_string();
            continue;
        }
        if !response.status().is_success() {
            return None;
        }
        let mut bytes = Vec::new();
        response
            .into_body()
            .into_reader()
            .take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        return (bytes.len() as u64 <= MAX_BYTES).then_some(bytes);
    }
    None
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// Every batch `fetch_all` delivers, with whether it was marked last.
    fn batches(
        urls: Vec<String>,
        get: impl Fn(&str) -> Option<Vec<u8>> + Sync,
    ) -> Vec<(BTreeMap<String, Vec<u8>>, bool)> {
        let mut delivered = Vec::new();
        fetch_all(urls, get, |fetched, last| delivered.push((fetched, last)));
        delivered
    }

    fn merged(delivered: Vec<(BTreeMap<String, Vec<u8>>, bool)>) -> BTreeMap<String, Vec<u8>> {
        delivered
            .into_iter()
            .flat_map(|(fetched, _)| fetched)
            .collect()
    }

    #[test]
    fn every_url_gets_an_answer_and_refused_hosts_are_never_asked() {
        let asked = AtomicUsize::new(0);
        let urls = vec![
            "https://cdn.example.com/a.png".to_string(),
            "https://cdn.example.com/missing.png".to_string(),
            "http://192.168.1.1/reboot".to_string(),
            "http://localhost:8080/x".to_string(),
        ];
        let delivered = batches(urls, |url| {
            asked.fetch_add(1, Ordering::Relaxed);
            url.ends_with("a.png").then(|| b"PNG".to_vec())
        });
        assert_eq!(asked.load(Ordering::Relaxed), 2);
        assert!(delivered.last().unwrap().1, "the last batch is marked");
        let fetched = merged(delivered);
        assert_eq!(fetched.len(), 4);
        assert_eq!(fetched["https://cdn.example.com/a.png"], b"PNG");
        assert!(fetched["https://cdn.example.com/missing.png"].is_empty());
        assert!(fetched["http://192.168.1.1/reboot"].is_empty());
    }

    #[test]
    fn at_most_max_urls_are_fetched() {
        let urls = (0..MAX_REMOTE + 5)
            .map(|i| format!("https://example.com/{i}.png"))
            .collect();
        assert_eq!(
            merged(batches(urls, |_| Some(Vec::new()))).len(),
            MAX_REMOTE
        );
    }

    #[test]
    fn downloads_run_side_by_side() {
        let (running, most) = (AtomicUsize::new(0), AtomicUsize::new(0));
        let urls = (0..WORKERS)
            .map(|i| format!("https://example.com/{i}.png"))
            .collect();
        batches(urls, |_| {
            let now = running.fetch_add(1, Ordering::SeqCst) + 1;
            most.fetch_max(now, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(50));
            running.fetch_sub(1, Ordering::SeqCst);
            Some(Vec::new())
        });
        assert!(most.load(Ordering::SeqCst) > 1, "one download at a time");
    }

    #[test]
    fn a_slow_url_does_not_hold_back_the_ones_already_in() {
        let released = std::sync::atomic::AtomicBool::new(false);
        let slow = "https://tracker.example.com/pixel.gif";
        let fast = "https://cdn.example.com/a.png";
        let mut delivered = Vec::new();
        fetch_all(
            vec![slow.to_string(), fast.to_string()],
            |url| {
                let deadline = Instant::now() + Duration::from_secs(5);
                while url == slow && !released.load(Ordering::Relaxed) && Instant::now() < deadline
                {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Some(url.as_bytes().to_vec())
            },
            |fetched, last| {
                if fetched.contains_key(fast) {
                    released.store(true, Ordering::Relaxed);
                }
                delivered.push((fetched.into_keys().collect::<Vec<_>>(), last));
            },
        );
        assert_eq!(
            delivered,
            [
                (vec![fast.to_string()], false),
                (vec![slow.to_string()], true)
            ]
        );
    }

    /// A server on a free local port that answers one request with `response`.
    fn serve_once(response: Vec<u8>) -> String {
        serve_seen(response).0
    }

    /// `serve_once`, which also hands back the request it was sent.
    fn serve_seen(response: Vec<u8>) -> (String, Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (send, seen) = mpsc::channel();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 4096];
            let read = stream.read(&mut request).unwrap_or(0);
            let _ = send.send(String::from_utf8_lossy(&request[..read]).into_owned());
            let _ = stream.write_all(&response);
        });
        (format!("http://127.0.0.1:{port}/image.png"), seen)
    }

    fn response(status: &str, body: &[u8]) -> Vec<u8> {
        let head = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        [head.as_bytes(), body].concat()
    }

    fn gzipped(body: &[u8]) -> Vec<u8> {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        encoder.write_all(body).unwrap();
        let body = encoder.finish().unwrap();
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        [head.as_bytes(), &body].concat()
    }

    fn plain() -> ureq::Agent {
        config().build().into()
    }

    fn local(url: &str) -> bool {
        url.starts_with("http://127.0.0.1:")
    }

    #[test]
    fn get_returns_a_success_body_and_nothing_for_an_error_or_an_oversized_body() {
        let ok = serve_once(response("200 OK", b"PNG"));
        assert_eq!(get_from(&ok, local, &plain()), Some(b"PNG".to_vec()));
        let missing = serve_once(response("404 Not Found", b"gone"));
        assert_eq!(get_from(&missing, local, &plain()), None);
        let huge = serve_once(response("200 OK", &vec![0; MAX_BYTES as usize + 1]));
        assert_eq!(get_from(&huge, local, &plain()), None);
    }

    #[test]
    fn a_gzipped_body_is_asked_for_and_arrives_decoded() {
        let css = b"body { color: red }";
        let (url, seen) = serve_seen(gzipped(css));
        assert_eq!(get_from(&url, local, &plain()), Some(css.to_vec()));
        let request = seen.recv().unwrap().to_ascii_lowercase();
        assert!(request.contains("accept-encoding: gzip"), "{request}");
    }

    #[test]
    fn the_size_limit_counts_decoded_bytes_so_a_small_gzip_bomb_fails() {
        let bomb = gzipped(&vec![0; MAX_BYTES as usize + 1]);
        assert!(bomb.len() < 64 * 1024, "{} bytes on the wire", bomb.len());
        assert_eq!(get_from(&serve_once(bomb), local, &plain()), None);
    }

    #[test]
    fn redirects_are_followed_only_to_allowed_hosts() {
        let target = serve_once(response("200 OK", b"PNG"));
        let hop = format!("HTTP/1.1 302 Found\r\nLocation: {target}\r\nContent-Length: 0\r\n\r\n");
        assert_eq!(
            get_from(&serve_once(hop.into_bytes()), local, &plain()),
            Some(b"PNG".to_vec())
        );
        let inward =
            "HTTP/1.1 302 Found\r\nLocation: http://192.168.0.1/admin\r\nContent-Length: 0\r\n\r\n";
        assert_eq!(
            get_from(&serve_once(inward.as_bytes().to_vec()), local, &plain()),
            None
        );
        let ok = serve_once(response("200 OK", b"PNG"));
        assert_eq!(get(&ok), None, "this machine is not a public host");
    }
}
