//! Resources for an HTML body: `cid:` parts of the message, `data:` URIs, and remote content the user chose to load,
//! all from memory. Nothing is fetched here.
use std::collections::BTreeSet;
use std::io::Cursor;
use std::sync::{Arc, Mutex, PoisonError};

use blitz_traits::net::{Bytes, NetHandler, NetProvider, Request};

use super::Fetched;
use postvak_core::message::InlinePart;

/// Decoded bytes one image may take, and all resources of one page together; a small compressed image can claim
/// gigabytes, and the page asks for each URL anew.
const MAX_IMAGE: u64 = 64 * 1024 * 1024;
const MAX_PAGE: u64 = 256 * 1024 * 1024;

/// Answers every request at once, from memory. A refused request gets empty bytes, so a stylesheet in `<head>` does not
/// hold up painting and an image simply fails to decode.
pub(crate) struct Inline {
    fetched: Fetched,
    parts: Vec<InlinePart>,
    refused: Arc<Mutex<BTreeSet<String>>>,
    spent: Mutex<u64>,
}

impl Inline {
    /// The provider, and the remote URLs it was asked for that are not in `fetched`.
    pub fn new(parts: Vec<InlinePart>, fetched: Fetched) -> (Inline, Arc<Mutex<BTreeSet<String>>>) {
        let refused = Arc::new(Mutex::new(BTreeSet::new()));
        let provider = Inline {
            fetched,
            parts,
            refused: refused.clone(),
            spent: Mutex::new(0),
        };
        (provider, refused)
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
            "http" | "https" => {
                let bytes = self.fetched.get(url.as_str()).cloned();
                if bytes.is_none() {
                    self.refused
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .insert(url.to_string());
                }
                bytes
            }
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
        let bytes = self
            .resolve(&request)
            .filter(|bytes| self.afford(bytes))
            .unwrap_or_default();
        handler.bytes(request.url.to_string(), Bytes::from(bytes));
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

    fn fetch(provider: &Inline, url: &str) -> Vec<u8> {
        let got = Arc::new(Mutex::new(None));
        let request = Request::get(Url::parse(url).unwrap());
        provider.fetch(0, request, Box::new(Capture(got.clone())));
        let answered = got.lock().unwrap().take();
        answered
            .expect("every request is answered at once")
            .to_vec()
    }

    fn provider() -> (Inline, Arc<Mutex<BTreeSet<String>>>) {
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
        let (inline, refused) = provider();
        assert_eq!(fetch(&inline, "cid:logo@example.com"), b"PNG");
        assert_eq!(fetch(&inline, "cid:logo%40example.com"), b"PNG");
        assert_eq!(fetch(&inline, "data:text/plain;base64,aGk="), b"hi");
        assert_eq!(fetch(&inline, "data:,a%20b"), b"a b");
        assert_eq!(fetch(&inline, "https://cdn.example.com/hero.png"), b"HERO");
        assert!(refused.lock().unwrap().is_empty());
    }

    #[test]
    fn everything_else_is_answered_empty_and_remote_is_noted() {
        let (inline, refused) = provider();
        assert!(fetch(&inline, "cid:unknown").is_empty());
        assert!(fetch(&inline, "cid:logo@example.com?1").is_empty());
        assert!(fetch(&inline, "cid:logo@example.com#1").is_empty());
        assert!(fetch(&inline, "file:///etc/passwd").is_empty());
        assert!(fetch(&inline, "postvak://mail/relative.png").is_empty());
        assert!(refused.lock().unwrap().is_empty());
        assert!(fetch(&inline, "https://tracker.example.com/p.gif").is_empty());
        assert_eq!(
            *refused.lock().unwrap(),
            BTreeSet::from(["https://tracker.example.com/p.gif".to_string()])
        );
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
        let (inline, _) = Inline::new(parts, Arc::new(BTreeMap::new()));
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
        let (inline, _) = Inline::new(parts, Arc::new(BTreeMap::new()));
        let served = (0..40)
            .filter(|_| !fetch(&inline, "cid:big").is_empty())
            .count();
        assert_eq!(served as u64, MAX_PAGE / decoded_size(&image));
    }
}
