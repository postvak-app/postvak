//! Sender brand logos (BIMI): which DNS record a message may show the logo of, the logo's URL in that record, and a
//! week-long cache per record. Only mail that passed DMARC for its From domain, by the receiving server's own
//! `Authentication-Results`, gets a logo; anyone can put a brand's selector on spoofed mail. Nothing here logs: a
//! lookup tells whoever reads the log who mails you.
use std::io;
use std::path::{Path, PathBuf};

use mail_parser::MessageParser;
use serde::{Deserialize, Serialize};

use crate::paths::write_atomic;

/// How long a lookup's answer, a logo or none, is kept.
const WEEK: i64 = 7 * 24 * 60 * 60;
/// The most an SVG may weigh; the BIMI profile (SVG Tiny PS) caps logos at 32 KiB.
pub const MAX_SVG: usize = 32 * 1024;

/// The record a message's logo is published in: `<selector>._bimi.<domain>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Brand {
    pub domain: String,
    pub selector: String,
}

impl Brand {
    pub fn record_name(&self) -> String {
        format!("{}._bimi.{}", self.selector, self.domain)
    }
}

/// The brand record a message may show, when its one From address's domain passed DMARC by the topmost
/// `Authentication-Results`, the one the receiving server added last. A `BIMI-Selector` header picks the selector.
pub fn brand(raw_headers: &[u8]) -> Option<Brand> {
    let msg = MessageParser::default().parse_headers(raw_headers)?;
    let from: Vec<_> = msg.from()?.mailboxes().collect();
    let [from] = from.as_slice() else {
        return None;
    };
    let (_, domain) = from.address()?.rsplit_once('@')?;
    let domain = domain_name(domain)?;
    // `Headers::get` gives the last of a repeated header; the topmost is the first.
    let first = |name: &str| {
        let header = msg.headers().all(name).next()?;
        std::str::from_utf8(header.raw_value()).ok()
    };
    if !dmarc_passed(first("Authentication-Results")?, &domain) {
        return None;
    }
    let selector = match first("BIMI-Selector") {
        Some(header) => selector(header)?,
        None => "default".into(),
    };
    Some(Brand { domain, selector })
}

/// Whether an `Authentication-Results` value says DMARC passed for `domain`. A `header.from` that names another
/// domain is a pass for that one, not this.
fn dmarc_passed(results: &str, domain: &str) -> bool {
    let results = without_comments(results);
    results.split(';').skip(1).any(|method| {
        let mut words = method.split_whitespace();
        let Some((name, result)) = words.next().and_then(|word| word.split_once('=')) else {
            return false;
        };
        if !name.eq_ignore_ascii_case("dmarc") || !result.eq_ignore_ascii_case("pass") {
            return false;
        }
        words
            .filter_map(|word| word.split_once('='))
            .filter(|(property, _)| property.eq_ignore_ascii_case("header.from"))
            .all(|(_, from)| from.trim_end_matches('.').eq_ignore_ascii_case(domain))
    })
}

/// The text with its `(comments)` removed, nested ones too.
fn without_comments(text: &str) -> String {
    let mut depth = 0usize;
    text.chars()
        .filter(|c| match c {
            '(' => {
                depth += 1;
                false
            }
            ')' => {
                depth = depth.saturating_sub(1);
                false
            }
            _ => depth == 0,
        })
        .collect()
}

/// The `s=` of a `BIMI-Selector: v=BIMI1; s=name;` header, when it is a plain DNS label.
fn selector(header: &str) -> Option<String> {
    let tags = tags(header);
    if *tags.first()? != ("v", "BIMI1") {
        return None;
    }
    let (_, selector) = tags.iter().find(|(tag, _)| *tag == "s")?;
    label(selector).then(|| selector.to_ascii_lowercase())
}

/// `tag=value` pairs separated by `;`, trimmed; pairs without `=` are left out.
fn tags(text: &str) -> Vec<(&str, &str)> {
    text.split(';')
        .filter_map(|pair| pair.split_once('='))
        .map(|(tag, value)| (tag.trim(), value.trim()))
        .collect()
}

/// Letters, digits and inner hyphens, 1 to 63 of them.
fn label(text: &str) -> bool {
    (1..=63).contains(&text.len())
        && text.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        && !text.starts_with('-')
        && !text.ends_with('-')
}

/// A lowercase host name of at least two labels, without a trailing dot; IP literals and anything else are None.
fn domain_name(text: &str) -> Option<String> {
    let text = text.trim_end_matches('.');
    let labels: Vec<&str> = text.split('.').collect();
    let valid = text.len() <= 253
        && labels.len() >= 2
        && labels.iter().all(|part| label(part))
        && labels
            .last()
            .is_some_and(|tld| tld.bytes().any(|b| b.is_ascii_alphabetic()));
    valid.then(|| text.to_ascii_lowercase())
}

/// The logo URL of the one BIMI record among a name's TXT records: an `https` URL to a public host. None when there is
/// no such record, more than one, or it declines a logo with an empty `l=`.
pub fn location(records: &[String]) -> Option<String> {
    let bimi: Vec<_> = records
        .iter()
        .map(|record| tags(record))
        .filter(|tags| tags.first() == Some(&("v", "BIMI1")))
        .collect();
    let [tags] = bimi.as_slice() else {
        return None;
    };
    let (_, location) = tags.iter().find(|(tag, _)| *tag == "l")?;
    // An early draft allowed a comma-separated list; the first is the one to use.
    let url = url::Url::parse(location.split(',').next()?.trim()).ok()?;
    (url.scheme() == "https" && crate::net::public(url.as_str())).then(|| url.to_string())
}

/// What a lookup needs from the network, so tests can answer it.
pub trait Net {
    /// The TXT records at `name`, each record's strings joined; empty when the name or its TXT records do not exist.
    fn txt(&self, name: &str) -> anyhow::Result<Vec<String>>;
    /// The body of a successful GET of `url`, whose host every hop must pass `net::public`.
    fn get(&self, url: &str) -> anyhow::Result<Vec<u8>>;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Cached {
    checked_at: i64,
    svg: Option<String>,
}

/// Where the answer for `brand` is kept: the record name is a safe file name, since `Brand` holds only labels.
fn cache_file(cache_dir: &Path, brand: &Brand) -> PathBuf {
    cache_dir
        .join("bimi")
        .join(format!("{}.json", brand.record_name()))
}

/// The SVG logo `brand` publishes, from the cache when it answered within the week. No record, no logo in it, or a
/// logo that is too large or not SVG text is cached as None; a failed lookup or fetch is an error and not cached, so
/// the next start tries again.
pub fn logo(
    cache_dir: &Path,
    brand: &Brand,
    now: i64,
    net: &dyn Net,
) -> anyhow::Result<Option<String>> {
    let path = cache_file(cache_dir, brand);
    if let Some(cached) = load(&path)
        && now >= cached.checked_at
        && now - cached.checked_at < WEEK
    {
        return Ok(cached.svg);
    }
    let svg = match location(&net.txt(&brand.record_name())?) {
        Some(url) => String::from_utf8(net.get(&url)?)
            .ok()
            .filter(|svg| svg.len() <= MAX_SVG && svg.contains("<svg")),
        None => None,
    };
    let fresh = Cached {
        checked_at: now,
        svg,
    };
    // A cache that cannot be written only means asking again next time.
    let _ = save(&path, &fresh);
    Ok(fresh.svg)
}

/// Writes `svg` as the answer for `brand` checked at `now`, as a lookup would; for test fixtures.
#[cfg(any(test, feature = "testing"))]
pub fn cache_logo(cache_dir: &Path, brand: &Brand, now: i64, svg: Option<&str>) -> io::Result<()> {
    let cached = Cached {
        checked_at: now,
        svg: svg.map(str::to_string),
    };
    save(&cache_file(cache_dir, brand), &cached)
}

fn load(path: &Path) -> Option<Cached> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn save(path: &Path, cached: &Cached) -> io::Result<()> {
    write_atomic(path, serde_json::to_string(cached)?.as_bytes())
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    const NOW: i64 = 1_790_000_000;
    const LOGO: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10"><rect width="10" height="10" fill="#ff0000"/></svg>"##;
    const RECORD: &str = "v=BIMI1; l=https://cdn.example.com/logo.svg";

    fn headers(lines: &[&str]) -> Vec<u8> {
        let mut text = lines.join("\r\n");
        text.push_str("\r\n\r\n");
        text.into_bytes()
    }

    fn github(results: &str) -> Vec<u8> {
        headers(&[
            &format!("Authentication-Results: {results}"),
            "From: GitHub <noreply@github.com>",
            "Subject: hi",
        ])
    }

    fn brand_of(domain: &str, selector: &str) -> Brand {
        Brand {
            domain: domain.into(),
            selector: selector.into(),
        }
    }

    #[test]
    fn a_dmarc_pass_for_the_from_domain_gives_its_default_record() {
        let pass = "mx.example.net; dkim=pass header.d=github.com; spf=pass smtp.mailfrom=github.com; dmarc=pass (p=REJECT sp=REJECT dis=NONE) header.from=github.com";
        let brand = brand(&github(pass)).unwrap();
        assert_eq!(brand, brand_of("github.com", "default"));
        assert_eq!(brand.record_name(), "default._bimi.github.com");
    }

    #[test]
    fn no_logo_without_a_dmarc_pass_for_that_domain() {
        for results in [
            "mx.example.net; dmarc=fail header.from=github.com",
            "mx.example.net; dmarc=none header.from=github.com",
            "mx.example.net; dmarc=pass header.from=evil.example",
            "mx.example.net; dkim=pass header.d=github.com",
            "mx.example.net (dmarc=pass header.from=github.com); spf=pass",
            "dmarc=pass header.from=github.com",
            "mx.example.net; none",
        ] {
            assert_eq!(brand(&github(results)), None, "{results}");
        }
        assert_eq!(
            brand(&headers(&["From: GitHub <noreply@github.com>"])),
            None
        );
    }

    #[test]
    fn only_the_topmost_results_count() {
        let forged_below = headers(&[
            "Authentication-Results: mx.example.net; dmarc=fail header.from=github.com",
            "Authentication-Results: mx.example.net; dmarc=pass header.from=github.com",
            "From: noreply@github.com",
        ]);
        assert_eq!(brand(&forged_below), None);
    }

    #[test]
    fn a_pass_without_header_from_is_still_a_pass() {
        assert!(dmarc_passed("mx.example.net; DMARC=Pass", "github.com"));
        assert!(dmarc_passed(
            "mx.example.net; dmarc=pass header.from=GitHub.com.",
            "github.com"
        ));
    }

    #[test]
    fn several_from_addresses_or_a_bad_domain_get_no_logo() {
        let pass = "Authentication-Results: mx.example.net; dmarc=pass";
        for from in [
            "From: a@github.com, b@github.com",
            "From: someone@[192.0.2.1]",
            "From: someone@localhost",
            "From: someone@exa_mple.com",
            "From: no address",
        ] {
            assert_eq!(brand(&headers(&[pass, from])), None, "{from}");
        }
    }

    #[test]
    fn a_bimi_selector_header_picks_the_record() {
        let pass = "Authentication-Results: mx.example.net; dmarc=pass header.from=example.com";
        let from = "From: Shop <news@Example.COM>";
        let with = |selector: &str| brand(&headers(&[pass, from, selector]));
        assert_eq!(
            with("BIMI-Selector: v=BIMI1; s=Spring;"),
            Some(brand_of("example.com", "spring"))
        );
        assert_eq!(with("BIMI-Selector: v=BIMI1; s=../etc;"), None);
        assert_eq!(with("BIMI-Selector: v=BIMI2; s=spring;"), None);
        assert_eq!(with("BIMI-Selector: s=spring; v=BIMI1"), None);
    }

    #[test]
    fn domain_names_are_lowercase_labels() {
        assert_eq!(
            domain_name("Mail.Example.COM.").as_deref(),
            Some("mail.example.com")
        );
        for bad in ["example.com/x", "192.0.2.1", "-a.example.com", "com", ""] {
            assert_eq!(domain_name(bad), None, "{bad}");
        }
    }

    #[test]
    fn location_takes_the_one_bimi_record_on_a_public_https_host() {
        let records = |list: &[&str]| list.iter().map(|r| r.to_string()).collect::<Vec<_>>();
        assert_eq!(
            location(&records(&[
                "google-site-verification=abc",
                "v=BIMI1; l=https://cdn.example.com/logo.svg; a=https://cdn.example.com/vmc.pem",
            ]))
            .as_deref(),
            Some("https://cdn.example.com/logo.svg")
        );
        for refused in [
            "v=BIMI1; l=;",
            "v=BIMI1; l=http://cdn.example.com/a.svg",
            "v=BIMI1; l=https://127.0.0.1/a.svg",
            "v=BIMI1; l=https://192.168.1.1/a.svg",
            "v=BIMI1; l=https://localhost/a.svg",
            "v=BIMI1; l=not a url",
        ] {
            assert_eq!(location(&records(&[refused])), None, "{refused}");
        }
        assert_eq!(
            location(&records(&[
                "v=BIMI1; l=https://a.example/a.svg",
                "v=BIMI1; l=https://b.example/b.svg"
            ])),
            None,
            "two records are none"
        );
        assert_eq!(location(&[]), None);
    }

    /// Answers from memory and records the calls.
    struct Fake {
        txt: Result<Vec<String>, String>,
        body: Vec<u8>,
        calls: RefCell<Vec<String>>,
    }

    impl Fake {
        fn new(txt: &[&str], body: &str) -> Fake {
            Fake {
                txt: Ok(txt.iter().map(|t| t.to_string()).collect()),
                body: body.as_bytes().to_vec(),
                calls: RefCell::new(Vec::new()),
            }
        }
    }

    impl Net for Fake {
        fn txt(&self, name: &str) -> anyhow::Result<Vec<String>> {
            self.calls.borrow_mut().push(format!("txt {name}"));
            self.txt.clone().map_err(anyhow::Error::msg)
        }

        fn get(&self, url: &str) -> anyhow::Result<Vec<u8>> {
            self.calls.borrow_mut().push(format!("get {url}"));
            Ok(self.body.clone())
        }
    }

    #[test]
    fn a_logo_is_fetched_once_and_kept_for_a_week() {
        let dir = tempfile::tempdir().unwrap();
        let brand = brand_of("example.com", "default");
        let net = Fake::new(&[RECORD], LOGO);
        let ask = |now| logo(dir.path(), &brand, now, &net).unwrap();
        assert_eq!(ask(NOW).as_deref(), Some(LOGO));
        assert_eq!(
            *net.calls.borrow(),
            [
                "txt default._bimi.example.com",
                "get https://cdn.example.com/logo.svg"
            ]
        );
        assert_eq!(ask(NOW + WEEK - 1).as_deref(), Some(LOGO));
        assert_eq!(net.calls.borrow().len(), 2, "answered from the cache");
        assert_eq!(ask(NOW + WEEK).as_deref(), Some(LOGO));
        assert_eq!(net.calls.borrow().len(), 4, "asked again after a week");
        assert_eq!(ask(NOW).as_deref(), Some(LOGO));
        assert_eq!(
            net.calls.borrow().len(),
            6,
            "a clock that went back asks again"
        );
    }

    #[test]
    fn no_record_and_bad_logos_are_cached_as_none() {
        let dir = tempfile::tempdir().unwrap();
        let too_large = format!("{LOGO}<!--{}-->", "x".repeat(MAX_SVG));
        let cases = [
            ("none", Fake::new(&[], LOGO)),
            ("declined", Fake::new(&["v=BIMI1; l="], LOGO)),
            ("not-svg", Fake::new(&[RECORD], "<html>")),
            ("too-large", Fake::new(&[RECORD], &too_large)),
        ];
        for (selector, net) in cases {
            let brand = brand_of("example.com", selector);
            assert_eq!(
                logo(dir.path(), &brand, NOW, &net).unwrap(),
                None,
                "{selector}"
            );
            let calls = net.calls.borrow().len();
            assert_eq!(logo(dir.path(), &brand, NOW + 60, &net).unwrap(), None);
            assert_eq!(net.calls.borrow().len(), calls, "{selector} cached");
        }
    }

    #[test]
    fn a_failed_lookup_is_an_error_and_not_cached() {
        let dir = tempfile::tempdir().unwrap();
        let brand = brand_of("example.com", "default");
        let offline = Fake {
            txt: Err("offline".into()),
            ..Fake::new(&[], "")
        };
        assert!(logo(dir.path(), &brand, NOW, &offline).is_err());
        assert!(!cache_file(dir.path(), &brand).exists());
    }

    #[test]
    fn a_cached_logo_is_answered_without_the_network() {
        let dir = tempfile::tempdir().unwrap();
        let brand = brand_of("github.com", "default");
        cache_logo(dir.path(), &brand, NOW, Some(LOGO)).unwrap();
        let offline = Fake {
            txt: Err("offline".into()),
            ..Fake::new(&[], "")
        };
        assert_eq!(
            logo(dir.path(), &brand, NOW + 60, &offline)
                .unwrap()
                .as_deref(),
            Some(LOGO)
        );
    }
}
