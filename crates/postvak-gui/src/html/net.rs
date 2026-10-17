//! Resources for an HTML body: `cid:` parts of the message, `data:` URIs, and remote content the user chose to load,
//! all from memory. Nothing is fetched here.
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, PoisonError};

use blitz_traits::net::{Bytes, NetHandler, NetProvider, Request};

use super::Fetched;
use postvak_core::message::InlinePart;

/// Answers every request at once, from memory. A refused request gets empty bytes, so a stylesheet in `<head>` does not
/// hold up painting and an image simply fails to decode.
pub(crate) struct Inline {
    fetched: Fetched,
    parts: Vec<InlinePart>,
    refused: Arc<Mutex<BTreeSet<String>>>,
}

impl Inline {
    /// The provider, and the remote URLs it was asked for that are not in `fetched`.
    pub fn new(parts: Vec<InlinePart>, fetched: Fetched) -> (Inline, Arc<Mutex<BTreeSet<String>>>) {
        let refused = Arc::new(Mutex::new(BTreeSet::new()));
        let provider = Inline {
            fetched,
            parts,
            refused: refused.clone(),
        };
        (provider, refused)
    }

    fn resolve(&self, request: &Request) -> Option<Vec<u8>> {
        let url = &request.url;
        match url.scheme() {
            "cid" => {
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

impl NetProvider for Inline {
    fn fetch(&self, _doc_id: usize, request: Request, handler: Box<dyn NetHandler>) {
        let bytes = self.resolve(&request).unwrap_or_default();
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
        assert!(fetch(&inline, "file:///etc/passwd").is_empty());
        assert!(fetch(&inline, "postvak://mail/relative.png").is_empty());
        assert!(refused.lock().unwrap().is_empty());
        assert!(fetch(&inline, "https://tracker.example.com/p.gif").is_empty());
        assert_eq!(
            *refused.lock().unwrap(),
            BTreeSet::from(["https://tracker.example.com/p.gif".to_string()])
        );
    }
}
