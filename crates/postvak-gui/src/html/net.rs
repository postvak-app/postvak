//! Resources for an HTML body: `cid:` parts of the message, `data:` URIs, and remote content the user chose to load,
//! all from memory. Nothing is fetched here: a remote request waits until its bytes are downloaded and `deliver`ed,
//! so they go into the laid-out page without parsing it again.
use std::io::Cursor;
use std::sync::{Arc, Mutex, PoisonError};

use blitz_traits::net::{Bytes, NetHandler, NetProvider, Request};

use super::Fetched;
use postvak_core::message::InlinePart;

/// Decoded bytes one image may take, and all resources of one page together; a small compressed image can claim
/// gigabytes, and the page asks for each URL anew.
const MAX_IMAGE: u64 = 64 * 1024 * 1024;
const MAX_PAGE: u64 = 256 * 1024 * 1024;

/// Answers every local request at once, from memory; a refused one gets empty bytes, so an image simply fails to
/// decode. A remote request `fetched` lacks waits for `deliver` or `release`.
pub(crate) struct Inline {
    fetched: Fetched,
    parts: Vec<InlinePart>,
    remote: Mutex<Remote>,
    spent: Mutex<u64>,
}

/// What the page asked for from the web that `fetched` lacked.
#[derive(Default)]
struct Remote {
    /// Each URL once, in the order the page asked for them, since `take_noted` last took them.
    noted: Vec<String>,
    /// Every URL noted, so one is noted once.
    seen: std::collections::HashSet<String>,
    waiting: Vec<(String, Box<dyn NetHandler>)>,
}

impl Inline {
    pub fn new(parts: Vec<InlinePart>, fetched: Fetched) -> Arc<Inline> {
        Arc::new(Inline {
            fetched,
            parts,
            remote: Mutex::new(Remote::default()),
            spent: Mutex::new(0),
        })
    }

    /// The remote URLs asked for since the last call, in the order asked.
    pub fn take_noted(&self) -> Vec<String> {
        std::mem::take(&mut self.remote().noted)
    }

    /// Whether a request for `url` waits for its bytes.
    pub fn waits_for(&self, url: &str) -> bool {
        self.remote()
            .waiting
            .iter()
            .any(|(waiting, _)| waiting == url)
    }

    /// Answers the requests waiting for `url` with `bytes`, or with nothing when the page cannot afford them.
    pub fn deliver(&self, url: &str, bytes: &[u8]) {
        let answered = self.take_waiting(|waiting| waiting == url);
        for (url, handler) in answered {
            let bytes = if self.afford(bytes) { bytes } else { &[] };
            handler.bytes(url, Bytes::from(bytes.to_vec()));
        }
    }

    /// Answers the requests waiting for a URL `which` picks with nothing, as a refused one is answered.
    pub fn release(&self, which: impl Fn(&str) -> bool) {
        for (url, handler) in self.take_waiting(|url| which(url)) {
            handler.bytes(url, Bytes::new());
        }
    }

    /// Takes the waiting requests out, so answering one, which may ask for more (a stylesheet's `@import`), does not
    /// find the lock held.
    fn take_waiting(&self, which: impl Fn(&str) -> bool) -> Vec<(String, Box<dyn NetHandler>)> {
        let mut remote = self.remote();
        let (taken, kept) = std::mem::take(&mut remote.waiting)
            .into_iter()
            .partition(|(url, _)| which(url));
        remote.waiting = kept;
        taken
    }

    fn remote(&self) -> std::sync::MutexGuard<'_, Remote> {
        self.remote.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn resolve(&self, request: &Request) -> Option<Vec<u8>> {
        let url = &request.url;
        match url.scheme() {
            "cid" if url.query().is_none() && url.fragment().is_none() => {
                let cid = percent_encoding::percent_decode_str(url.path()).decode_utf8_lossy();
                self.parts
                    .iter()
                    .find(|part| part.cid == cid)
                    .map(|part| part.bytes.clone())
            }
            "data" => {
                let data = data_url::DataUrl::process(url.as_str()).ok()?;
                data.decode_to_vec().ok().map(|(bytes, _)| bytes)
            }
            "http" | "https" => self.fetched.get(url.as_str()).cloned(),
            _ => None,
        }
    }
}

impl Inline {
    /// Whether the page can still afford `bytes`, decoded; takes their cost from its budget when it can.
    fn afford(&self, bytes: &[u8]) -> bool {
        let cost = decoded_size(bytes);
        let mut spent = self.spent.lock().unwrap_or_else(PoisonError::into_inner);
        if cost > MAX_IMAGE || spent.saturating_add(cost) > MAX_PAGE {
            return false;
        }
        *spent += cost;
        true
    }
}

/// The RGBA size of an image, read from its header; other resources cost their length.
fn decoded_size(bytes: &[u8]) -> u64 {
    let dimensions = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()
        .and_then(|reader| reader.into_dimensions().ok());
    match dimensions {
        Some((width, height)) => u64::from(width) * u64::from(height) * 4,
        None => bytes.len() as u64,
    }
}

impl NetProvider for Inline {
    fn fetch(&self, _doc_id: usize, request: Request, handler: Box<dyn NetHandler>) {
        let url = request.url.to_string();
        let remote = matches!(request.url.scheme(), "http" | "https");
        let bytes = self.resolve(&request);
        if remote && bytes.is_none() {
            let mut waiting = self.remote();
            if waiting.seen.insert(url.clone()) {
                waiting.noted.push(url.clone());
            }
            waiting.waiting.push((url, handler));
            return;
        }
        let bytes = bytes.filter(|bytes| self.afford(bytes)).unwrap_or_default();
        handler.bytes(url, Bytes::from(bytes));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use blitz_traits::net::Url;

    use std::collections::BTreeMap;

    use super::*;

    struct Capture(Arc<Mutex<Option<Bytes>>>);

    impl NetHandler for Capture {
        fn bytes(self: Box<Self>, _resolved_url: String, bytes: Bytes) {
            *self.0.lock().unwrap() = Some(bytes);
        }
    }

    /// What `url` is answered with; None while it waits, with the slot its answer will land in.
    fn ask(provider: &Inline, url: &str) -> Arc<Mutex<Option<Bytes>>> {
        let got = Arc::new(Mutex::new(None));
        let request = Request::get(Url::parse(url).unwrap());
        provider.fetch(0, request, Box::new(Capture(got.clone())));
        got
    }

    fn fetch(provider: &Inline, url: &str) -> Vec<u8> {
        let answered = ask(provider, url).lock().unwrap().take();
        answered.expect("answered at once").to_vec()
    }

    fn provider() -> Arc<Inline> {
        let fetched = BTreeMap::from([(
            "https://cdn.example.com/hero.png".to_string(),
            b"HERO".to_vec(),
        )]);
        let parts = vec![InlinePart {
            cid: "logo@example.com".into(),
            bytes: b"PNG".to_vec(),
        }];
        Inline::new(parts, Arc::new(fetched))
    }

    #[test]
    fn cid_and_data_resolve_from_memory() {
        let inline = provider();
        assert_eq!(fetch(&inline, "cid:logo@example.com"), b"PNG");
        assert_eq!(fetch(&inline, "cid:logo%40example.com"), b"PNG");
        assert_eq!(fetch(&inline, "data:text/plain;base64,aGk="), b"hi");
        assert_eq!(fetch(&inline, "data:,a%20b"), b"a b");
        assert_eq!(fetch(&inline, "https://cdn.example.com/hero.png"), b"HERO");
        assert!(inline.take_noted().is_empty());
    }

    #[test]
    fn everything_else_local_is_answered_empty() {
        let inline = provider();
        assert!(fetch(&inline, "cid:unknown").is_empty());
        assert!(fetch(&inline, "cid:logo@example.com?1").is_empty());
        assert!(fetch(&inline, "cid:logo@example.com#1").is_empty());
        assert!(fetch(&inline, "file:///etc/passwd").is_empty());
        assert!(fetch(&inline, "postvak://mail/relative.png").is_empty());
        assert!(inline.take_noted().is_empty());
    }

    #[test]
    fn a_remote_request_waits_for_its_bytes_and_is_noted_once_in_order() {
        let inline = provider();
        let (pixel, hero) = (
            "https://tracker.example.com/p.gif",
            "https://cdn.example.com/b.png",
        );
        let first = ask(&inline, pixel);
        let again = ask(&inline, pixel);
        let other = ask(&inline, hero);
        assert!(first.lock().unwrap().is_none(), "waits");
        assert_eq!(inline.take_noted(), [pixel, hero]);
        assert!(inline.take_noted().is_empty(), "taken");
        assert!(inline.waits_for(pixel));
        inline.deliver(pixel, b"GIF");
        assert_eq!(first.lock().unwrap().as_deref(), Some(&b"GIF"[..]));
        assert_eq!(again.lock().unwrap().as_deref(), Some(&b"GIF"[..]));
        assert!(!inline.waits_for(pixel));
        assert!(other.lock().unwrap().is_none(), "another URL still waits");
        inline.release(|url| url == hero);
        assert_eq!(other.lock().unwrap().as_deref(), Some(&b""[..]));
        assert!(!inline.waits_for(hero));
    }

    #[test]
    fn a_delivered_image_counts_against_the_page_budget() {
        let inline = Inline::new(Vec::new(), Arc::default());
        let url = "https://cdn.example.com/huge.png";
        let got = ask(&inline, url);
        inline.deliver(url, &png_header(5_000));
        assert_eq!(got.lock().unwrap().as_deref(), Some(&b""[..]));
    }

    /// A PNG claiming `side` x `side` RGBA pixels, with an empty IDAT and no pixel data.
    fn png_header(side: u32) -> Vec<u8> {
        let mut out = Vec::new();
        let mut encoder = png::Encoder::new(&mut out, side, side);
        encoder.set_color(png::ColorType::Rgba);
        let mut writer = encoder.write_header().unwrap();
        // The decoder reports the size only once the first IDAT starts.
        writer.write_chunk(png::chunk::IDAT, &[]).unwrap();
        drop(writer);
        out
    }

    #[test]
    fn an_image_too_large_decoded_is_answered_empty() {
        let bomb = png_header(5_000);
        let parts = vec![InlinePart {
            cid: "bomb".into(),
            bytes: bomb.clone(),
        }];
        let inline = Inline::new(parts, Arc::new(BTreeMap::new()));
        assert_eq!(decoded_size(&bomb), 100_000_000);
        assert!(fetch(&inline, "cid:bomb").is_empty());
    }

    #[test]
    fn the_page_budget_ends_repeated_requests_for_one_image() {
        let image = png_header(2_000);
        let parts = vec![InlinePart {
            cid: "big".into(),
            bytes: image.clone(),
        }];
        let inline = Inline::new(parts, Arc::new(BTreeMap::new()));
        let served = (0..40)
            .filter(|_| !fetch(&inline, "cid:big").is_empty())
            .count();
        assert_eq!(served as u64, MAX_PAGE / decoded_size(&image));
    }
}
