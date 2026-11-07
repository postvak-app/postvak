//! Downloads a message's remote content once the user asks: capped in count, size and time, public hosts only, and
//! kept in memory for a while, so opening the message again, or other mail with the same images, does not download
//! them again.
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::Read;
#[cfg(test)]
use std::sync::mpsc::Sender;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Condvar, LazyLock, Mutex, PoisonError};
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
/// again for every image. The first download, and a stylesheet, go at once: the top of the page shows sooner, and
/// the images a stylesheet names are asked for sooner.
const BATCH: Duration = Duration::from_millis(500);
/// Downloads at once, so a few dead trackers waiting out `CONNECT_TIMEOUT` do not hold up the rest.
const WORKERS: usize = 12;
/// Downloads from one host at once, as browsers allow; HTTP/1.1 asks one thing at a time per connection.
const PER_HOST: usize = 6;
/// Downloaded bytes kept in memory, all together, and how long each is kept: a mail's images rarely change, but
/// some (a countdown, the weather) do.
const CACHE_BYTES: u64 = 64 * 1024 * 1024;
const CACHE_AGE: Duration = Duration::from_secs(60 * 60);

static CACHE: LazyLock<Mutex<Cache>> = LazyLock::new(|| Mutex::new(Cache::default()));

/// Downloads each job on a thread of its own, so a later round (the images a stylesheet names) need not wait for a
/// slow earlier one. Each job answers in batches, the last one marked; what was downloaded lately comes first, from
/// memory.
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
                let deliver = |fetched, last| {
                    let _ = thread_send.send(FetchDone { tag, fetched, last });
                    thread_wake();
                };
                fetch_cached(job.urls, &CACHE, get, deliver);
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

/// Hands `deliver` what `cache` holds of `urls` at once, then `fetch_all`s the rest into it; the last batch is
/// marked.
fn fetch_cached(
    mut urls: Vec<String>,
    cache: &Mutex<Cache>,
    get: impl Fn(&str) -> Option<Vec<u8>> + Sync,
    mut deliver: impl FnMut(BTreeMap<String, Vec<u8>>, bool),
) {
    let now = Instant::now();
    let mut known = BTreeMap::new();
    {
        let mut cache = cache.lock().unwrap_or_else(PoisonError::into_inner);
        urls.retain(|url| match cache.get(url, now) {
            Some(bytes) => {
                known.insert(url.clone(), bytes);
                false
            }
            None => true,
        });
    }
    if !known.is_empty() || urls.is_empty() {
        deliver(known, urls.is_empty());
    }
    if !urls.is_empty() {
        let keep = |url: &str| {
            let bytes = get(url)?;
            cache
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .put(url, &bytes, Instant::now());
            Some(bytes)
        };
        fetch_all(urls, keep, deliver);
    }
}

/// The URLs waiting, each with its host, in the order the page asked for them, and the downloads running per host.
struct Queue {
    waiting: VecDeque<(String, String)>,
    running: HashMap<String, usize>,
}

impl Queue {
    fn new(urls: impl IntoIterator<Item = String>) -> Queue {
        Queue {
            waiting: urls
                .into_iter()
                .map(|url| {
                    let host = host(&url);
                    (url, host)
                })
                .collect(),
            running: HashMap::new(),
        }
    }

    /// The first waiting URL whose host has room, counted as running; None once nothing waits, and Some(None) when
    /// every waiting host is full.
    fn next(&mut self) -> Option<Option<(String, String)>> {
        if self.waiting.is_empty() {
            return None;
        }
        let free = self
            .waiting
            .iter()
            .position(|(_, host)| self.running.get(host).copied().unwrap_or(0) < PER_HOST);
        Some(free.and_then(|at| {
            let (url, host) = self.waiting.remove(at)?;
            *self.running.entry(host.clone()).or_default() += 1;
            Some((url, host))
        }))
    }

    fn finished(&mut self, host: &str) {
        if let Some(running) = self.running.get_mut(host) {
            *running = running.saturating_sub(1);
        }
    }
}

/// The host a URL connects to, which `PER_HOST` counts by; empty for one that does not parse, which never connects.
fn host(url: &str) -> String {
    blitz_traits::net::Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
        .unwrap_or_default()
}

/// Whether a download is a stylesheet or font rather than an image: what it asks for in turn should start at once.
fn urgent(bytes: &[u8]) -> bool {
    !bytes.is_empty() && image::guess_format(bytes).is_err()
}

/// Fetches `urls` on a few workers, in their order and at most `PER_HOST` from one host at once, and hands
/// `deliver` what finished: the first download and any stylesheet at once, the rest at most once per `BATCH`, then
/// the rest with `last` set. Each URL ends up in one batch, empty when `get` failed or the host is refused.
fn fetch_all(
    urls: Vec<String>,
    get: impl Fn(&str) -> Option<Vec<u8>> + Sync,
    mut deliver: impl FnMut(BTreeMap<String, Vec<u8>>, bool),
) {
    let queue = Queue::new(urls.into_iter().take(MAX_REMOTE));
    // Once every answer is in, what is left goes as the last batch.
    let mut outstanding = queue.waiting.len();
    let queue = Mutex::new(queue);
    let room = Condvar::new();
    let (send, results) = mpsc::channel();
    std::thread::scope(|scope| {
        for _ in 0..WORKERS {
            let (send, queue, room, get) = (send.clone(), &queue, &room, &get);
            scope.spawn(move || {
                loop {
                    // The lock is held only to pick a URL, never for the download.
                    let mut waiting = queue.lock().unwrap_or_else(PoisonError::into_inner);
                    let (url, host) = loop {
                        match waiting.next() {
                            None => return,
                            Some(Some(next)) => break next,
                            Some(None) => {
                                waiting =
                                    room.wait(waiting).unwrap_or_else(PoisonError::into_inner);
                            }
                        }
                    };
                    drop(waiting);
                    let bytes = public(&url).then(|| get(&url)).flatten();
                    queue
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .finished(&host);
                    room.notify_all();
                    if send.send((url, bytes.unwrap_or_default())).is_err() {
                        return;
                    }
                }
            });
        }
        drop(send);
        let mut batch = BTreeMap::new();
        // None until the first delivery, which goes at once once it holds a download: a refused or failed URL,
        // which answers at once and empty, does not take the place of the first image.
        let mut flushed: Option<Instant> = None;
        let mut now_please = false;
        loop {
            let next = match flushed {
                Some(at) if !batch.is_empty() => {
                    results.recv_timeout(BATCH.saturating_sub(at.elapsed()))
                }
                _ => results.recv().map_err(|_| RecvTimeoutError::Disconnected),
            };
            match next {
                Ok((url, bytes)) => {
                    outstanding -= 1;
                    now_please |= urgent(&bytes);
                    batch.insert(url, bytes);
                }
                Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => {}
            }
            let due = match flushed {
                Some(at) => at.elapsed() >= BATCH,
                None => batch.values().any(|bytes| !bytes.is_empty()),
            };
            if outstanding == 0 {
                break;
            }
            if !batch.is_empty() && (due || now_please) {
                deliver(std::mem::take(&mut batch), false);
                flushed = Some(Instant::now());
                now_please = false;
            }
        }
        deliver(batch, true);
    });
}

/// Downloaded bodies by URL, in memory only: at most `CACHE_BYTES` together, the least lately used going first,
/// and each for `CACHE_AGE`. A failed download is not kept, so it is tried again.
#[derive(Default)]
struct Cache {
    entries: HashMap<String, Cached>,
    size: u64,
    /// Counts uses, so the least lately used entry has the lowest `used`.
    clock: u64,
}

struct Cached {
    bytes: Vec<u8>,
    at: Instant,
    used: u64,
}

impl Cache {
    fn get(&mut self, url: &str, now: Instant) -> Option<Vec<u8>> {
        let fresh = self
            .entries
            .get(url)
            .is_some_and(|entry| now.saturating_duration_since(entry.at) < CACHE_AGE);
        if !fresh {
            self.remove(url);
            return None;
        }
        self.clock += 1;
        let entry = self.entries.get_mut(url)?;
        entry.used = self.clock;
        Some(entry.bytes.clone())
    }

    fn put(&mut self, url: &str, bytes: &[u8], now: Instant) {
        self.remove(url);
        let cost = bytes.len() as u64;
        if bytes.is_empty() || cost > CACHE_BYTES {
            return;
        }
        while self.size + cost > CACHE_BYTES {
            let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.used)
                .map(|(url, _)| url.clone())
            else {
                break;
            };
            self.remove(&oldest);
        }
        self.clock += 1;
        self.size += cost;
        self.entries.insert(
            url.to_string(),
            Cached {
                bytes: bytes.to_vec(),
                at: now,
                used: self.clock,
            },
        );
    }

    fn remove(&mut self, url: &str) {
        if let Some(entry) = self.entries.remove(url) {
            self.size -= entry.bytes.len() as u64;
        }
    }
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
        // Every worker may keep its connection to a host for the next image, rather than connecting again.
        .max_idle_connections_per_host(PER_HOST)
        .max_idle_connections(2 * WORKERS)
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

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";

    fn queue(urls: &[&str]) -> Queue {
        Queue::new(urls.iter().map(|url| url.to_string()))
    }

    #[test]
    fn urls_are_taken_in_the_page_order_skipping_a_full_host() {
        let busy: Vec<String> = (0..PER_HOST)
            .map(|i| format!("https://cdn.example.com/{i}.png"))
            .collect();
        let mut urls: Vec<&str> = busy.iter().map(String::as_str).collect();
        urls.extend([
            "https://cdn.example.com/later.png",
            "https://other.example.com/a.png",
        ]);
        let mut queue = queue(&urls);
        let taken: Vec<String> = (0..PER_HOST)
            .map(|_| queue.next().flatten().unwrap().0)
            .collect();
        assert_eq!(taken, busy, "in order");
        assert_eq!(
            queue.next().flatten().unwrap().0,
            "https://other.example.com/a.png",
            "cdn.example.com is full"
        );
        assert!(matches!(queue.next(), Some(None)), "waits for room");
        queue.finished("cdn.example.com");
        assert_eq!(
            queue.next().flatten().unwrap().0,
            "https://cdn.example.com/later.png"
        );
        assert!(queue.next().is_none());
    }

    /// The most downloads `fetch_all` ran at once, from one host and from all.
    fn most_at_once(urls: Vec<String>) -> (usize, usize) {
        let running = Mutex::new(HashMap::<String, usize>::new());
        let (most_host, most_all) = (AtomicUsize::new(0), AtomicUsize::new(0));
        batches(urls, |url| {
            {
                let mut running = running.lock().unwrap();
                *running.entry(host(url)).or_default() += 1;
                most_host.fetch_max(*running.values().max().unwrap(), Ordering::SeqCst);
                most_all.fetch_max(running.values().sum(), Ordering::SeqCst);
            }
            std::thread::sleep(Duration::from_millis(30));
            *running.lock().unwrap().get_mut(&host(url)).unwrap() -= 1;
            Some(Vec::new())
        });
        (most_host.into_inner(), most_all.into_inner())
    }

    #[test]
    fn one_host_gets_at_most_per_host_downloads_and_others_run_beside_it() {
        let one: Vec<String> = (0..3 * PER_HOST)
            .map(|i| format!("https://cdn.example.com/{i}.png"))
            .collect();
        let (host, _) = most_at_once(one.clone());
        assert!(host <= PER_HOST && host > 1, "{host} at once");
        let mut two = one;
        two.extend((0..3 * PER_HOST).map(|i| format!("https://img.example.net/{i}.png")));
        let (_, all) = most_at_once(two);
        assert!(all > PER_HOST, "{all} at once");
    }

    #[test]
    fn the_first_download_and_a_stylesheet_are_handed_over_at_once() {
        let (top, css, later) = (
            "https://cdn.example.com/a.png",
            "https://cdn.example.com/style.css",
            "https://cdn.example.com/b.png",
        );
        let delivered = batches(vec![top.into(), css.into(), later.into()], |url| {
            let (wait, bytes) = match url {
                u if u == top => (0, PNG.to_vec()),
                u if u == css => (60, b"body { background: url(x.png) }".to_vec()),
                _ => (250, PNG.to_vec()),
            };
            std::thread::sleep(Duration::from_millis(wait));
            Some(bytes)
        });
        let keys: Vec<(Vec<String>, bool)> = delivered
            .into_iter()
            .map(|(fetched, last)| (fetched.into_keys().collect(), last))
            .collect();
        assert_eq!(
            keys,
            [
                (vec![top.to_string()], false),
                (vec![css.to_string()], false),
                (vec![later.to_string()], true),
            ]
        );
    }

    #[test]
    fn images_finishing_together_wait_for_one_batch() {
        let urls: Vec<String> = (0..4)
            .map(|i| format!("https://cdn.example.com/{i}.png"))
            .collect();
        let delivered = batches(urls, |url| {
            if !url.ends_with("0.png") {
                std::thread::sleep(Duration::from_millis(50));
            }
            Some(PNG.to_vec())
        });
        let sizes: Vec<usize> = delivered.iter().map(|(fetched, _)| fetched.len()).collect();
        assert_eq!(sizes, [1, 3], "the first at once, the rest in one layout");
    }

    #[test]
    fn a_failure_answering_first_does_not_take_the_first_images_place() {
        let (dead, top, later) = (
            "https://cdn.example.com/missing.png",
            "https://cdn.example.com/a.png",
            "https://cdn.example.com/b.png",
        );
        let delivered = batches(vec![dead.into(), top.into(), later.into()], |url| {
            let wait = match url {
                u if u == dead => 0,
                u if u == top => 60,
                _ => 250,
            };
            std::thread::sleep(Duration::from_millis(wait));
            (url != dead).then(|| PNG.to_vec())
        });
        let keys: Vec<Vec<String>> = delivered
            .into_iter()
            .map(|(fetched, _)| fetched.into_keys().collect())
            .collect();
        assert_eq!(
            keys,
            [
                vec![top.to_string(), dead.to_string()],
                vec![later.to_string()]
            ],
            "the image at once, the failure with it"
        );
    }

    #[test]
    fn what_the_cache_holds_comes_at_once_and_only_the_rest_is_downloaded() {
        let cache = Mutex::new(Cache::default());
        let (known, new) = (
            "https://cdn.example.com/logo.png",
            "https://cdn.example.com/new.png",
        );
        cache.lock().unwrap().put(known, b"LOGO", Instant::now());
        let asked = Mutex::new(Vec::new());
        let mut delivered = Vec::new();
        fetch_cached(
            vec![known.into(), new.into()],
            &cache,
            |url| {
                asked.lock().unwrap().push(url.to_string());
                Some(b"NEW".to_vec())
            },
            |fetched, last| delivered.push((fetched, last)),
        );
        assert_eq!(*asked.lock().unwrap(), [new]);
        assert_eq!(delivered[0].0[known], b"LOGO");
        assert!(!delivered[0].1);
        assert_eq!(merged(delivered.clone())[new], b"NEW");
        assert!(delivered.last().unwrap().1);

        let mut all_known = Vec::new();
        fetch_cached(
            vec![known.into()],
            &cache,
            |_| panic!("downloaded"),
            |f, l| {
                all_known.push((f, l));
            },
        );
        assert_eq!(all_known.len(), 1);
        assert!(all_known[0].1, "one batch, marked last");
        assert!(
            cache.lock().unwrap().get(new, Instant::now()).is_some(),
            "kept"
        );
    }

    #[test]
    fn the_cache_forgets_the_least_lately_used_first_and_anything_old() {
        let now = Instant::now();
        let mut cache = Cache::default();
        let half = vec![1; (CACHE_BYTES / 2) as usize];
        cache.put("a", &half, now);
        cache.put("b", &half, now);
        assert!(cache.get("a", now).is_some());
        cache.put("c", &half, now);
        assert!(cache.get("b", now).is_none(), "b was used least lately");
        assert!(cache.get("a", now).is_some());
        assert!(cache.get("c", now).is_some());
        assert_eq!(cache.size, CACHE_BYTES);

        cache.put("too big", &vec![0; CACHE_BYTES as usize + 1], now);
        assert!(cache.get("too big", now).is_none());
        assert!(cache.get("a", now).is_some(), "nothing made room for it");

        cache.put("failed", &[], now);
        assert!(cache.get("failed", now).is_none());

        assert!(cache.get("a", now + CACHE_AGE).is_none());
        assert_eq!(cache.size, CACHE_BYTES / 2);
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
