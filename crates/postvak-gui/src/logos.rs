//! Sender logos for the reader, with `[ui] sender_images = true`: which logo each brand has, looked up and fetched on
//! a thread of its own. The window draws the SVG; a logo it cannot draw shows the initials.
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use hickory_resolver::Resolver;
use hickory_resolver::proto::rr::RData;
use postvak_core::bimi::{self, Brand, MAX_SVG, Net};
use postvak_core::config::Identity;

use super::html::{self, Wake};
use crate::worker::Worker;

/// Lookups in flight at once, so a brand whose DNS never answers holds back no other.
const LOOKUPS: usize = 4;
/// Each DNS query gives up after this.
const DNS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// What the worker sends back: the brand and its SVG, if it has one.
type Found = (Brand, Option<String>);

pub(crate) struct Logos {
    enabled: bool,
    cache_dir: PathBuf,
    /// Swapped by tests before the first lookup starts the worker.
    pub(crate) net: Arc<dyn Net + Send + Sync>,
    /// `None` while a lookup is pending, and for a brand without a logo.
    known: HashMap<Brand, Option<Arc<str>>>,
    worker: Option<Worker<Brand, Found>>,
}

impl Logos {
    pub fn new(enabled: bool, cache_dir: PathBuf) -> Logos {
        Logos {
            enabled,
            cache_dir,
            net: Arc::new(Internet),
            known: HashMap::new(),
            worker: None,
        }
    }

    /// The brand a message's headers allow, while sender images are on, as the account's trusted servers say: none for
    /// an account that trusts no server.
    pub fn brand(&self, headers: &[u8], identity: Option<&Identity>) -> Option<Brand> {
        let identity = identity.filter(|_| self.enabled)?;
        bimi::brand(headers, |id| identity.trusts_authserv_id(id))
    }

    /// Asks the worker for `brand`'s logo the first time it comes up, unless the cache has it; the window wakes when it
    /// is in.
    pub fn want(&mut self, brand: &Brand, wake: &Wake) {
        if self.known.contains_key(brand) {
            return;
        }
        if let Some(svg) = bimi::cached(&self.cache_dir, brand, postvak_core::time::now()) {
            self.known.insert(brand.clone(), svg.map(Arc::from));
            return;
        }
        self.worker
            .get_or_insert_with(|| spawn(self.cache_dir.clone(), self.net.clone(), wake.clone()))
            .send(brand.clone());
        self.known.insert(brand.clone(), None);
    }

    /// Takes the worker's answers.
    pub fn receive(&mut self) {
        let Some(worker) = &self.worker else {
            return;
        };
        for (brand, svg) in worker.done() {
            self.known.insert(brand, svg.map(Arc::from));
        }
    }

    /// The SVG `brand` shows, once it is in.
    pub fn svg(&self, brand: &Brand) -> Option<&Arc<str>> {
        self.known.get(brand)?.as_ref()
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    #[cfg(test)]
    pub fn started(&self) -> bool {
        self.worker.is_some()
    }
}

/// `LOOKUPS` threads that look brands up; they end with the window.
fn spawn(cache_dir: PathBuf, net: Arc<dyn Net + Send + Sync>, wake: Wake) -> Worker<Brand, Found> {
    Worker::start_many("sender-logos", LOOKUPS, wake, move |brand: Brand| {
        // A failed lookup shows the initials for this session. Nothing is logged: a log line would name the sender's
        // domain.
        let svg = bimi::logo(&cache_dir, &brand, postvak_core::time::now(), net.as_ref())
            .ok()
            .flatten();
        (brand, svg)
    })
}

/// DNS through the system's resolvers, and HTTPS through the downloader HTML mail uses, which checks every hop's host.
pub(crate) struct Internet;

impl Net for Internet {
    fn txt(&self, name: &str) -> anyhow::Result<Vec<String>> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        runtime.block_on(async {
            let mut builder = Resolver::builder_tokio()?;
            builder.options_mut().timeout = DNS_TIMEOUT;
            builder.options_mut().attempts = 2;
            let resolver = builder.build()?;
            // Fully qualified, so the system's search domains are never tried with it.
            match resolver.txt_lookup(format!("{name}.")).await {
                Ok(lookup) => Ok(lookup
                    .answers()
                    .iter()
                    .filter_map(|record| match &record.data {
                        RData::TXT(txt) => Some(
                            txt.txt_data
                                .iter()
                                .map(|part| String::from_utf8_lossy(part))
                                .collect::<String>(),
                        ),
                        _ => None,
                    })
                    .collect()),
                Err(e) if e.is_no_records_found() || e.is_nx_domain() => Ok(Vec::new()),
                Err(e) => Err(e.into()),
            }
        })
    }

    fn get(&self, url: &str) -> anyhow::Result<Vec<u8>> {
        let body =
            html::get(url).ok_or_else(|| anyhow::anyhow!("the logo could not be fetched"))?;
        // The downloader allows megabytes for remote images; a logo gets the BIMI limit. Too large reads as no logo.
        Ok(if body.len() > MAX_SVG {
            Vec::new()
        } else {
            body
        })
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::fixture::message;
    use crate::test_support::{Fixture, branded, cache_github_logo, trusting, wait_until};

    #[test]
    fn a_brand_is_only_read_while_sender_images_are_on() {
        let headers = branded("INBOX", 1, "Hi").headers;
        let identity = trusting("mx.example.com");
        let dir = tempfile::tempdir().unwrap();
        assert!(
            Logos::new(false, dir.path().into())
                .brand(&headers, Some(&identity))
                .is_none()
        );
        let on = Logos::new(true, dir.path().into());
        assert_eq!(
            on.brand(&headers, Some(&identity)).unwrap().record_name(),
            "default._bimi.github.com"
        );
        assert!(
            on.brand(&message("INBOX", 1, "Hi").headers, Some(&identity))
                .is_none()
        );
    }

    #[test]
    fn a_brand_needs_a_server_the_account_trusts() {
        let headers = branded("INBOX", 1, "Hi").headers;
        let dir = tempfile::tempdir().unwrap();
        let on = Logos::new(true, dir.path().into());
        assert!(on.brand(&headers, None).is_none());
        assert!(
            on.brand(&headers, Some(&trusting("mx.example.org")))
                .is_none()
        );
    }

    #[test]
    fn a_brand_is_asked_for_once_and_its_cached_logo_comes_in() {
        let fx = Fixture::new(&["work"]);
        cache_github_logo(&fx.paths);
        let mut logos = Logos::new(true, fx.paths.cache_dir.clone());
        logos.net = Arc::new(crate::test_support::Offline);
        let brand = logos
            .brand(
                &branded("INBOX", 1, "Hi").headers,
                Some(&trusting("mx.example.com")),
            )
            .unwrap();
        let wake: Wake = Arc::new(|| {});
        logos.want(&brand, &wake);
        logos.want(&brand, &wake);
        wait_until(|| {
            logos.receive();
            logos.svg(&brand).is_some()
        });
        assert!(logos.svg(&brand).is_some_and(|svg| svg.contains("<svg")));
    }

    #[test]
    fn a_cached_logo_is_in_at_once_without_a_lookup() {
        let fx = Fixture::new(&["work"]);
        cache_github_logo(&fx.paths);
        let mut logos = Logos::new(true, fx.paths.cache_dir.clone());
        let brand = logos
            .brand(
                &branded("INBOX", 1, "Hi").headers,
                Some(&trusting("mx.example.com")),
            )
            .unwrap();
        logos.want(&brand, &(Arc::new(|| {}) as Wake));
        assert!(logos.svg(&brand).is_some_and(|svg| svg.contains("<svg")));
        assert!(!logos.started(), "nothing to look up");
    }

    /// DNS that answers every name with no record, except `stuck`, which waits until `released` is set.
    struct Stuck {
        stuck: String,
        released: Arc<std::sync::atomic::AtomicBool>,
    }

    impl Net for Stuck {
        fn txt(&self, name: &str) -> anyhow::Result<Vec<String>> {
            while name == self.stuck && !self.released.load(std::sync::atomic::Ordering::Acquire) {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Ok(Vec::new())
        }

        fn get(&self, _url: &str) -> anyhow::Result<Vec<u8>> {
            Ok(Vec::new())
        }
    }

    #[test]
    fn a_stuck_lookup_does_not_hold_back_the_next_brand() {
        let fx = Fixture::new(&["work"]);
        let mut logos = Logos::new(true, fx.paths.cache_dir.clone());
        let slow = Brand {
            domain: "slow.example".into(),
            selector: "default".into(),
        };
        let fast = Brand {
            domain: "fast.example".into(),
            selector: "default".into(),
        };
        let released = Arc::new(std::sync::atomic::AtomicBool::new(false));
        logos.net = Arc::new(Stuck {
            stuck: slow.record_name(),
            released: released.clone(),
        });
        let wake: Wake = Arc::new(|| {});
        logos.want(&slow, &wake);
        logos.want(&fast, &wake);
        let Some(worker) = &logos.worker else {
            panic!("want starts the worker");
        };
        let mut found = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while found.is_empty() && std::time::Instant::now() < deadline {
            found.extend(worker.done());
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        released.store(true, std::sync::atomic::Ordering::Release);
        assert_eq!(found, [(fast, None)], "the fast brand came in first");
    }

    #[test]
    fn a_failed_lookup_has_no_logo_and_caches_nothing() {
        let fx = Fixture::new(&["work"]);
        let mut logos = Logos::new(true, fx.paths.cache_dir.clone());
        logos.net = Arc::new(crate::test_support::Offline);
        let brand = logos
            .brand(
                &branded("INBOX", 1, "Hi").headers,
                Some(&trusting("mx.example.com")),
            )
            .unwrap();
        let wake: Wake = Arc::new(|| {});
        logos.want(&brand, &wake);
        let Some(worker) = &logos.worker else {
            panic!("want starts the worker");
        };
        let mut found = Vec::new();
        wait_until(|| {
            found.extend(worker.done());
            !found.is_empty()
        });
        assert_eq!(found, [(brand.clone(), None)]);
        assert_eq!(logos.svg(&brand), None);
        assert!(!fx.paths.cache_dir.join("bimi").exists());
    }
}
