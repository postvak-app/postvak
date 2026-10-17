/*! The sender's avatar without a toolkit: the colour behind their initials and, with `[ui] sender_images` on, their
mail domain's logo, asked for once per domain over HTTPS from public hosts and kept in the cache directory. Nothing keyed
by the address itself, such as Gravatar, is ever asked: it would tell a third party who mails you. */
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use super::html::{FetchJob, Fetcher, Wake};
use super::theme::TINTS;
use crate::paths::write_atomic;

/// Which avatar fill `address` gets: FNV-1a of the lowercased address, so it holds across runs and releases.
pub(crate) fn tint(address: &str) -> usize {
    let hash = address
        .to_lowercase()
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
        });
    usize::try_from(hash % TINTS as u64).unwrap_or_default()
}

/// The domain of a From field ("Name <a@example.com>" or a bare address), when it is a plain host name: labels of
/// letters, digits and inner hyphens, at least two. That leaves out IP literals and local names, and keeps the domain
/// safe as a file name.
pub(crate) fn domain(from: &str) -> Option<String> {
    let address = from
        .split_once('<')
        .and_then(|(_, rest)| rest.split_once('>'))
        .map_or(from, |(address, _)| address);
    let (_, host) = address.trim().rsplit_once('@')?;
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    let label = |label: &str| {
        !label.is_empty()
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    };
    (host.contains('.') && host.split('.').all(label)).then_some(host)
}

/// Where a domain publishes its logo: the touch icon, a PNG most sites have. The window draws PNG but not ICO, so the
/// favicon is not asked for. ponytail: one guess; read the home page's `<link rel="icon">` if too few senders have it.
fn logo_url(domain: &str) -> String {
    format!("https://{domain}/apple-touch-icon.png")
}

fn is_png(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x89PNG\r\n\x1a\n")
}

/// The sender logos: on disk under `dir`, one PNG per domain, and the ones asked for.
pub(crate) struct Logos {
    dir: PathBuf,
    /// Started for the first domain not on disk; tests put a fake here.
    pub(crate) fetcher: Option<Fetcher>,
    /// Per domain, once known: whether it has a logo. A domain without one is not asked again while the window is
    /// open; it is not written down, so a download that failed offline is tried again next time.
    known: HashMap<String, bool>,
    asked: HashSet<String>,
}

impl Logos {
    pub fn new(dir: PathBuf) -> Logos {
        Logos {
            dir,
            fetcher: None,
            known: HashMap::new(),
            asked: HashSet::new(),
        }
    }

    fn file(&self, domain: &str) -> PathBuf {
        self.dir.join(format!("{domain}.png"))
    }

    /// `domain`'s logo on disk, when it has one.
    pub fn logo(&self, domain: &str) -> Option<PathBuf> {
        self.known
            .get(domain)
            .copied()
            .unwrap_or_default()
            .then(|| self.file(domain))
    }

    /// Finds `domain`'s logo on disk, or asks for it, when nothing is known about it yet.
    pub fn want(&mut self, domain: &str, wake: &Wake) {
        if self.known.contains_key(domain) || self.asked.contains(domain) {
            return;
        }
        if self.file(domain).is_file() {
            self.known.insert(domain.to_string(), true);
            return;
        }
        self.asked.insert(domain.to_string());
        let fetcher = self
            .fetcher
            .get_or_insert_with(|| Fetcher::start(wake.clone()));
        fetcher.send(FetchJob {
            tag: 0,
            urls: vec![logo_url(domain)],
        });
    }

    /// Keeps the logos that arrived. Nothing about the domain is logged: it says who mails you.
    pub fn receive(&mut self) {
        let Some(fetcher) = &self.fetcher else { return };
        for done in fetcher.done() {
            for (url, bytes) in done.fetched {
                let Some(domain) = self.asked.iter().find(|d| logo_url(d) == url).cloned() else {
                    continue;
                };
                self.asked.remove(&domain);
                let kept = is_png(&bytes)
                    && std::fs::create_dir_all(&self.dir)
                        .and_then(|()| write_atomic(&self.file(&domain), &bytes))
                        .inspect_err(|e| log::debug!("could not keep a sender logo: {e}"))
                        .is_ok();
                self.known.insert(domain, kept);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::gui::html::FetchDone;

    #[test]
    fn the_tint_is_stable_ignores_case_and_spreads() {
        assert_eq!(tint("ada@example.com"), tint("Ada@Example.com"));
        let tints: HashSet<usize> = ["ada", "alan", "grace", "linus", "mum", "shop", "news"]
            .iter()
            .map(|name| tint(&format!("{name}@example.com")))
            .collect();
        assert!(tints.len() > 2, "{tints:?}");
        assert!(tints.iter().all(|&t| t < TINTS));
        // Pinned: a change would recolour everyone's avatars.
        assert_eq!(tint("linus@example.com"), 1);
    }

    #[test]
    fn only_a_plain_domain_gets_a_logo() {
        assert_eq!(
            domain("Linus <linus@Mail.Example.com>").as_deref(),
            Some("mail.example.com")
        );
        assert_eq!(domain("shop@example.com.").as_deref(), Some("example.com"));
        for refused in [
            "nobody",
            "a@localhost",
            "a@[127.0.0.1]",
            "a@../../etc",
            "a@exa mple.com",
            "a@-x.com",
            "a@x..com",
        ] {
            assert_eq!(domain(refused), None, "{refused}");
        }
    }

    fn fake() -> (
        Logos,
        std::sync::mpsc::Receiver<FetchJob>,
        std::sync::mpsc::Sender<FetchDone>,
        tempfile::TempDir,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let (fetcher, jobs, done) = Fetcher::fake();
        let mut logos = Logos::new(dir.path().join("senders"));
        logos.fetcher = Some(fetcher);
        (logos, jobs, done, dir)
    }

    fn answer(done: &std::sync::mpsc::Sender<FetchDone>, url: &str, bytes: &[u8]) {
        let fetched = BTreeMap::from([(url.to_string(), bytes.to_vec())]);
        done.send(FetchDone { tag: 0, fetched }).unwrap();
    }

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\nrest";

    #[test]
    fn a_logo_is_asked_for_once_by_domain_and_kept_on_disk() {
        let (mut logos, jobs, done, dir) = fake();
        let wake: Wake = std::sync::Arc::new(|| {});
        logos.want("example.com", &wake);
        logos.want("example.com", &wake);
        let urls: Vec<Vec<String>> = jobs.try_iter().map(|job| job.urls).collect();
        assert_eq!(urls, [["https://example.com/apple-touch-icon.png"]]);
        assert_eq!(logos.logo("example.com"), None, "not here yet");

        answer(&done, "https://example.com/apple-touch-icon.png", PNG);
        logos.receive();
        let file = logos.logo("example.com").unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), PNG);

        let mut later = Logos::new(dir.path().join("senders"));
        later.want("example.com", &wake);
        assert_eq!(
            later.logo("example.com"),
            Some(file),
            "read from disk, not asked"
        );
        assert!(later.fetcher.is_none());
    }

    #[test]
    fn a_domain_without_a_png_keeps_its_initials_and_is_not_asked_again() {
        let (mut logos, jobs, done, dir) = fake();
        let wake: Wake = std::sync::Arc::new(|| {});
        logos.want("example.org", &wake);
        answer(
            &done,
            "https://example.org/apple-touch-icon.png",
            b"<html>not found</html>",
        );
        logos.receive();
        logos.want("example.org", &wake);
        assert_eq!(jobs.try_iter().count(), 1);
        assert_eq!(logos.logo("example.org"), None);
        assert!(!dir.path().join("senders/example.org.png").exists());
    }
}
