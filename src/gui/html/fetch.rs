//! Downloads a message's remote content once the user asks: capped in count, size and time, public hosts only.
use std::collections::BTreeMap;
use std::io::Read;
use std::net::IpAddr;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use super::{FetchDone, FetchJob, MAX_REMOTE, Wake};

/// A larger response counts as failed.
const MAX_BYTES: u64 = 5 * 1024 * 1024;
const MAX_REDIRECTS: usize = 5;
const TIMEOUT: Duration = Duration::from_secs(15);
const WORKERS: usize = 6;

/// The download thread and its two channels, as `Renderer` has them.
pub(crate) struct Fetcher {
    done: Receiver<FetchDone>,
    jobs: Sender<FetchJob>,
}

impl Fetcher {
    pub fn start(wake: Wake) -> Fetcher {
        let (jobs, todo) = mpsc::channel::<FetchJob>();
        let (send, done) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("html-fetch".into())
            .spawn(move || {
                for job in todo {
                    let fetched = fetch_all(job.urls, get);
                    if send
                        .send(FetchDone {
                            tag: job.tag,
                            fetched,
                        })
                        .is_err()
                    {
                        return;
                    }
                    wake();
                }
            });
        if let Err(e) = spawned {
            log::warn!("could not start loading remote content: {e}");
        }
        Fetcher { done, jobs }
    }

    pub fn send(&self, job: FetchJob) {
        let _ = self.jobs.send(job);
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
        (Fetcher { done, jobs }, todo, send)
    }
}

/// Fetches `urls` on a few workers; each one ends up in the map, empty when `get` failed or the host is refused.
fn fetch_all(
    urls: Vec<String>,
    get: impl Fn(&str) -> Option<Vec<u8>> + Sync,
) -> BTreeMap<String, Vec<u8>> {
    let queue = Mutex::new(urls.into_iter().take(MAX_REMOTE).collect::<Vec<_>>());
    let fetched = Mutex::new(BTreeMap::new());
    std::thread::scope(|scope| {
        for _ in 0..WORKERS {
            scope.spawn(|| {
                while let Some(url) = queue.lock().unwrap_or_else(PoisonError::into_inner).pop() {
                    let bytes = public(&url).then(|| get(&url)).flatten();
                    fetched
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .insert(url, bytes.unwrap_or_default());
                }
            });
        }
    });
    fetched.into_inner().unwrap_or_else(PoisonError::into_inner)
}

/// The body of a GET, when it answers 2xx within the limits and every hop is a public host. Nothing about the URL is
/// logged: it can identify the reader.
fn get(url: &str) -> Option<Vec<u8>> {
    get_from(url, public)
}

/// `get` with the host check given, so tests can serve from this machine. Redirects are followed here, so each one is
/// checked.
fn get_from(url: &str, allowed: impl Fn(&str) -> bool) -> Option<Vec<u8>> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .max_redirects(0)
        .timeout_global(Some(TIMEOUT))
        .build()
        .into();
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

/// An `http`/`https` URL whose host is a name, or an address outside loopback and private networks, so a mail cannot
/// make the reader's machine call its router or a local service. A name that resolves to such an address still passes.
fn public(url: &str) -> bool {
    let Ok(url) = blitz_traits::net::Url::parse(url) else {
        return false;
    };
    if !matches!(url.scheme(), "http" | "https") {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    match host.trim_start_matches('[').trim_end_matches(']').parse() {
        Ok(ip) => routable(ip),
        Err(_) => {
            let name = host.trim_end_matches('.').to_ascii_lowercase();
            name != "localhost" && !name.ends_with(".localhost") && !name.ends_with(".local")
        }
    }
}

fn routable(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            !(ip.is_loopback()
                || ip.is_private()
                || ip.is_link_local()
                || ip.is_unspecified()
                || ip.is_broadcast()
                || ip.octets()[0] == 100 && ip.octets()[1] & 0xc0 == 64)
        }
        IpAddr::V6(ip) => {
            let first = ip.segments()[0];
            !(ip.is_loopback()
                || ip.is_unspecified()
                || first & 0xfe00 == 0xfc00
                || first & 0xffc0 == 0xfe80
                || ip
                    .to_ipv4_mapped()
                    .is_some_and(|v4| !routable(IpAddr::V4(v4))))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[test]
    fn every_url_gets_an_answer_and_refused_hosts_are_never_asked() {
        let asked = AtomicUsize::new(0);
        let urls = vec![
            "https://cdn.example.com/a.png".to_string(),
            "https://cdn.example.com/missing.png".to_string(),
            "http://192.168.1.1/reboot".to_string(),
            "http://localhost:8080/x".to_string(),
        ];
        let fetched = fetch_all(urls, |url| {
            asked.fetch_add(1, Ordering::Relaxed);
            url.ends_with("a.png").then(|| b"PNG".to_vec())
        });
        assert_eq!(asked.load(Ordering::Relaxed), 2);
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
        assert_eq!(fetch_all(urls, |_| Some(Vec::new())).len(), MAX_REMOTE);
    }

    #[test]
    fn only_public_web_hosts_are_fetched() {
        for allowed in [
            "https://example.com/a.png",
            "http://93.184.216.34/a.png",
            "https://[2606:2800:220:1::]/a.png",
        ] {
            assert!(public(allowed), "{allowed}");
        }
        for refused in [
            "file:///etc/passwd",
            "ftp://example.com/a",
            "http://localhost/a",
            "http://printer.local/a",
            "http://127.0.0.1/a",
            "http://10.0.0.1/a",
            "http://172.16.0.1/a",
            "http://192.168.0.1/a",
            "http://169.254.169.254/latest/meta-data",
            "http://100.64.0.1/a",
            "http://0.0.0.0/a",
            "http://[::1]/a",
            "http://[fd00::1]/a",
            "http://[fe80::1]/a",
            "http://[::ffff:192.168.0.1]/a",
            "not a url",
        ] {
            assert!(!public(refused), "{refused}");
        }
    }

    /// A server on a free local port that answers one request with `response`.
    fn serve_once(response: Vec<u8>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 1024];
            let _ = stream.read(&mut request);
            let _ = stream.write_all(&response);
        });
        format!("http://127.0.0.1:{port}/image.png")
    }

    fn response(status: &str, body: &[u8]) -> Vec<u8> {
        let head = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        [head.as_bytes(), body].concat()
    }

    fn local(url: &str) -> bool {
        url.starts_with("http://127.0.0.1:")
    }

    #[test]
    fn get_returns_a_success_body_and_nothing_for_an_error_or_an_oversized_body() {
        let ok = serve_once(response("200 OK", b"PNG"));
        assert_eq!(get_from(&ok, local), Some(b"PNG".to_vec()));
        let missing = serve_once(response("404 Not Found", b"gone"));
        assert_eq!(get_from(&missing, local), None);
        let huge = serve_once(response("200 OK", &vec![0; MAX_BYTES as usize + 1]));
        assert_eq!(get_from(&huge, local), None);
    }

    #[test]
    fn redirects_are_followed_only_to_allowed_hosts() {
        let target = serve_once(response("200 OK", b"PNG"));
        let hop = format!("HTTP/1.1 302 Found\r\nLocation: {target}\r\nContent-Length: 0\r\n\r\n");
        assert_eq!(
            get_from(&serve_once(hop.into_bytes()), local),
            Some(b"PNG".to_vec())
        );
        let inward =
            "HTTP/1.1 302 Found\r\nLocation: http://192.168.0.1/admin\r\nContent-Length: 0\r\n\r\n";
        assert_eq!(
            get_from(&serve_once(inward.as_bytes().to_vec()), local),
            None
        );
        let ok = serve_once(response("200 OK", b"PNG"));
        assert_eq!(get(&ok), None, "this machine is not a public host");
    }
}
