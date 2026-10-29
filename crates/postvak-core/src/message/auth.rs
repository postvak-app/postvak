//! Sender domains and the `Authentication-Results` header (RFC 8601): the verdict a receiving server recorded on
//! DKIM, SPF and DMARC. Rules use it for `from_domain`, `to_domain` and `authenticated`.

use mail_parser::MessageParser;

/// The header a receiving server adds with its verdict.
const HEADER: &str = "Authentication-Results";

/// Second-level labels under which a country code hands out names, as in `example.co.uk`. A stand-in for the public
/// suffix list: close enough to keep `co.uk` from passing as one organisation, without a list to keep current.
const SECOND_LEVEL: &[&str] = &[
    "ac", "co", "com", "edu", "go", "gov", "gv", "lg", "ltd", "me", "mil", "ne", "net", "nic",
    "or", "org", "plc", "sch",
];

/// The domain of a bare address, lower-cased and without a trailing dot; None without an `@` or a domain.
pub fn address_domain(address: &str) -> Option<String> {
    let (_, domain) = address.rsplit_once('@')?;
    let domain = domain
        .trim()
        .trim_end_matches('>')
        .trim_end_matches('.')
        .to_lowercase();
    (!domain.is_empty()).then_some(domain)
}

/// Whether `domain` is `parent` or one of its subdomains: `mail.paypal.com` is in `paypal.com`, `notpaypal.com` is
/// not. Both lower-cased.
pub fn in_domain(domain: &str, parent: &str) -> bool {
    domain == parent
        || domain
            .strip_suffix(parent)
            .is_some_and(|rest| rest.ends_with('.'))
}

/// The domain an organisation registered, approximately: the last two labels, or three under a country code's
/// second level such as `co.uk`. `mx.soverin.net` is `soverin.net`.
pub fn org_domain(domain: &str) -> String {
    let labels: Vec<&str> = domain.trim_end_matches('.').split('.').collect();
    let n = labels.len();
    let keep = if n >= 3 && labels[n - 1].len() == 2 && SECOND_LEVEL.contains(&labels[n - 2]) {
        3
    } else {
        2
    };
    labels[n.saturating_sub(keep)..].join(".").to_lowercase()
}

/// One `Authentication-Results` header.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuthResults {
    /// The server that wrote it, lower-cased.
    pub authserv_id: String,
    pub results: Vec<MethodResult>,
}

/// One method's verdict, such as `dkim=pass header.d=example.com`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MethodResult {
    /// Lower-cased, without a version: `dkim`, `spf`, `dmarc`.
    pub method: String,
    /// Lower-cased: `pass`, `fail`, `none` and so on.
    pub result: String,
    /// `ptype.property` lower-cased, such as `header.d`, with the value as written, unquoted.
    pub properties: Vec<(String, String)>,
}

impl MethodResult {
    pub fn property(&self, name: &str) -> Option<&str> {
        self.properties
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    fn passed(&self, method: &str) -> bool {
        self.method == method && self.result == "pass"
    }
}

#[derive(Debug, PartialEq)]
enum Token {
    Word(String),
    Eq,
    Semi,
}

/// Words, `=` and `;`, with comments and whitespace dropped and quoted strings unquoted.
fn tokens(value: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut word = String::new();
    let mut chars = value.chars();
    let flush = |word: &mut String, tokens: &mut Vec<Token>| {
        if !word.is_empty() {
            tokens.push(Token::Word(std::mem::take(word)));
        }
    };
    while let Some(c) = chars.next() {
        match c {
            '(' => {
                flush(&mut word, &mut tokens);
                let mut depth = 1usize;
                while depth > 0 {
                    match chars.next() {
                        Some('\\') => {
                            chars.next();
                        }
                        Some('(') => depth += 1,
                        Some(')') => depth -= 1,
                        Some(_) => {}
                        None => break,
                    }
                }
            }
            '"' => {
                flush(&mut word, &mut tokens);
                let mut quoted = String::new();
                loop {
                    match chars.next() {
                        Some('\\') => quoted.extend(chars.next()),
                        Some('"') | None => break,
                        Some(c) => quoted.push(c),
                    }
                }
                tokens.push(Token::Word(quoted));
            }
            ';' => {
                flush(&mut word, &mut tokens);
                tokens.push(Token::Semi);
            }
            '=' => {
                flush(&mut word, &mut tokens);
                tokens.push(Token::Eq);
            }
            c if c.is_whitespace() => flush(&mut word, &mut tokens),
            c => word.push(c),
        }
    }
    flush(&mut word, &mut tokens);
    tokens
}

/// One `method=result` and its `ptype.property=value` pairs; None when the first three tokens are not that.
fn result_info(tokens: &[Token]) -> Option<MethodResult> {
    let [
        Token::Word(method),
        Token::Eq,
        Token::Word(result),
        rest @ ..,
    ] = tokens
    else {
        return None;
    };
    let method = method.split('/').next().unwrap_or_default().to_lowercase();
    let mut properties = Vec::new();
    let mut rest = rest;
    while !rest.is_empty() {
        match rest {
            [Token::Word(key), Token::Eq, Token::Word(value), tail @ ..] => {
                properties.push((key.to_lowercase(), value.clone()));
                rest = tail;
            }
            [_, tail @ ..] => rest = tail,
            [] => break,
        }
    }
    Some(MethodResult {
        method,
        result: result.to_lowercase(),
        properties,
    })
}

/// The value of one `Authentication-Results` header; None without an authserv-id. Results that do not parse are left
/// out, so a header reading `none` has no results.
pub fn parse(value: &str) -> Option<AuthResults> {
    let tokens = tokens(value);
    let mut groups = tokens.split(|t| *t == Token::Semi);
    let authserv_id = match groups.next()?.first()? {
        Token::Word(id) => id.to_lowercase(),
        _ => return None,
    };
    Some(AuthResults {
        authserv_id,
        results: groups.filter_map(result_info).collect(),
    })
}

/// Every `Authentication-Results` header in the block that parses, topmost first: the one the last server to receive
/// the message added.
pub fn all(raw_headers: &[u8]) -> Vec<AuthResults> {
    let Some(msg) = MessageParser::default().parse_headers(raw_headers) else {
        return Vec::new();
    };
    msg.headers()
        .iter()
        .filter(|h| h.raw_name().trim().eq_ignore_ascii_case(HEADER))
        .filter_map(|h| parse(&String::from_utf8_lossy(h.raw_value())))
        .collect()
}

/// Whether a DKIM signer's domain is aligned with the From domain: the same, or a parent or subdomain of it within
/// the same organisation, so `com` never counts as a parent.
fn aligned(signer: &str, from: &str) -> bool {
    (in_domain(signer, from) || in_domain(from, signer)) && org_domain(signer) == org_domain(from)
}

impl AuthResults {
    /// Whether this header says mail from `from_domain` (lower-cased) is authenticated. A DMARC pass or fail for that
    /// domain decides, since it applies the domain's own alignment policy, which may be stricter than ours; without
    /// one, DKIM passing with a signing domain aligned with it does.
    pub fn authenticates(&self, from_domain: &str) -> bool {
        let dmarc = self.results.iter().find(|r| {
            r.method == "dmarc"
                && matches!(r.result.as_str(), "pass" | "fail")
                && r.property("header.from").is_none_or(|domain| {
                    domain
                        .trim_end_matches('.')
                        .eq_ignore_ascii_case(from_domain)
                })
        });
        if let Some(dmarc) = dmarc {
            return dmarc.result == "pass";
        }
        self.results.iter().any(|r| {
            let signer = r
                .property("header.d")
                .map(str::to_string)
                .or_else(|| r.property("header.i").and_then(address_domain));
            r.passed("dkim")
                && signer.is_some_and(|signer| {
                    aligned(&signer.trim_end_matches('.').to_lowercase(), from_domain)
                })
        })
    }
}

/// Whether the message with these headers and this From field is authenticated, as the topmost
/// `Authentication-Results` header whose authserv-id `trusted` accepts says. That is safe when the trusted ids are
/// servers of the provider that write the header on every incoming message: theirs sits above any the sender wrote,
/// and RFC 8601 has them remove copies that claim their id. A From with no address, or more than one, is not
/// authenticated; neither is mail without such a header.
pub fn authenticated(raw_headers: &[u8], from: &str, trusted: impl Fn(&str) -> bool) -> bool {
    let addresses = crate::message::bare_addresses(from);
    let [address] = addresses.as_slice() else {
        return false;
    };
    let Some(domain) = address_domain(address) else {
        return false;
    };
    all(raw_headers)
        .into_iter()
        .find(|header| trusted(&header.authserv_id))
        .is_some_and(|header| header.authenticates(&domain))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_domains_ignore_case_and_need_an_at() {
        assert_eq!(
            address_domain("A@Mail.Example.COM.").as_deref(),
            Some("mail.example.com")
        );
        assert_eq!(address_domain("\"a@b\"@c.org").as_deref(), Some("c.org"));
        assert_eq!(address_domain("nobody"), None);
        assert_eq!(address_domain("x@"), None);
    }

    #[test]
    fn a_domain_holds_its_subdomains_but_not_lookalikes() {
        assert!(in_domain("microsoft.com", "microsoft.com"));
        assert!(in_domain(
            "accountprotection.microsoft.com",
            "microsoft.com"
        ));
        assert!(!in_domain("notmicrosoft.com", "microsoft.com"));
        assert!(!in_domain("microsoft.com.evil", "microsoft.com"));
        assert!(!in_domain("gui.com", "ui.com"));
    }

    #[test]
    fn org_domains_keep_country_second_levels() {
        assert_eq!(org_domain("mx.soverin.net"), "soverin.net");
        assert_eq!(org_domain("imap.soverin.net"), "soverin.net");
        assert_eq!(org_domain("mail.example.co.uk"), "example.co.uk");
        assert_eq!(org_domain("example.de"), "example.de");
        assert_eq!(org_domain("localhost"), "localhost");
    }

    #[test]
    fn parses_a_typical_header() {
        let header = parse(
            "mx.soverin.net;\r\n\tdkim=pass (2048-bit key; unprotected) header.d=crates.io header.i=@crates.io \
             header.b=\"AbC=\";\r\n spf=pass smtp.mailfrom=bounces.crates.io;\r\n dmarc=pass (p=none dis=none) \
             header.from=crates.io",
        )
        .unwrap();
        assert_eq!(header.authserv_id, "mx.soverin.net");
        let methods: Vec<(&str, &str)> = header
            .results
            .iter()
            .map(|r| (r.method.as_str(), r.result.as_str()))
            .collect();
        assert_eq!(
            methods,
            [("dkim", "pass"), ("spf", "pass"), ("dmarc", "pass")]
        );
        assert_eq!(header.results[0].property("header.d"), Some("crates.io"));
        assert_eq!(header.results[0].property("header.b"), Some("AbC="));
        assert_eq!(header.results[2].property("header.from"), Some("crates.io"));
    }

    #[test]
    fn parses_versions_spaces_around_equals_and_no_result() {
        let header = parse("\"Mx.Example.org\" 1; DKIM/1 = Pass header.d = example.org").unwrap();
        assert_eq!(header.authserv_id, "mx.example.org");
        assert!(header.results[0].passed("dkim"));
        assert_eq!(header.results[0].property("header.d"), Some("example.org"));
        assert!(parse("mx.example.org; none").unwrap().results.is_empty());
        assert_eq!(parse(""), None);
        assert_eq!(parse("; dkim=pass"), None);
        assert_eq!(parse("(only a comment"), None);
    }

    #[test]
    fn dmarc_pass_or_aligned_dkim_authenticates() {
        let header = |value: &str| parse(value).unwrap();
        assert!(header("mx.a.net; dmarc=pass header.from=paypal.com").authenticates("paypal.com"));
        assert!(header("mx.a.net; dmarc=pass").authenticates("paypal.com"));
        assert!(
            !header("mx.a.net; dmarc=pass header.from=evil.example").authenticates("paypal.com")
        );
        assert!(!header("mx.a.net; dmarc=fail header.from=paypal.com").authenticates("paypal.com"));
        assert!(!header("mx.a.net; spf=pass smtp.mailfrom=paypal.com").authenticates("paypal.com"));
        // DKIM: the signer is the From domain, a parent or a subdomain of it.
        assert!(header("mx.a.net; dkim=pass header.d=paypal.com").authenticates("paypal.com"));
        assert!(header("mx.a.net; dkim=pass header.d=paypal.com").authenticates("mail.paypal.com"));
        assert!(header("mx.a.net; dkim=pass header.d=e.paypal.com").authenticates("paypal.com"));
        assert!(header("mx.a.net; dkim=pass header.i=@paypal.com").authenticates("paypal.com"));
        assert!(!header("mx.a.net; dkim=pass header.d=sendgrid.net").authenticates("paypal.com"));
        assert!(!header("mx.a.net; dkim=pass header.d=com").authenticates("paypal.com"));
        assert!(!header("mx.a.net; dkim=pass header.d=co.uk").authenticates("bank.co.uk"));
        assert!(!header("mx.a.net; dkim=pass").authenticates("paypal.com"));
        assert!(!header("mx.a.net; dkim=fail header.d=paypal.com").authenticates("paypal.com"));
        // An unaligned pass does not stop an aligned one in the same header.
        assert!(
            header("mx.a.net; dkim=pass header.d=sendgrid.net; dkim=pass header.d=paypal.com")
                .authenticates("paypal.com")
        );
        // A regression: DMARC's verdict for the From domain decides over our relaxed alignment, since the domain may
        // publish strict alignment (adkim=s), under which a subdomain's signature does not count.
        assert!(
            !header(
                "mx.a.net; dkim=pass header.d=news.bank.example; dmarc=fail header.from=bank.example"
            )
            .authenticates("bank.example")
        );
        assert!(
            !header("mx.a.net; dmarc=fail; dkim=pass header.d=bank.example")
                .authenticates("bank.example")
        );
        // A DMARC verdict for another domain, or one that is no verdict, leaves it to DKIM.
        assert!(
            header(
                "mx.a.net; dmarc=fail header.from=other.example; dkim=pass header.d=bank.example"
            )
            .authenticates("bank.example")
        );
        assert!(
            header(
                "mx.a.net; dmarc=none header.from=bank.example; dkim=pass header.d=bank.example"
            )
            .authenticates("bank.example")
        );
    }

    fn trusted(id: &str) -> bool {
        id == "mx.soverin.net"
    }

    #[test]
    fn only_the_topmost_trusted_header_counts() {
        let raw = b"Authentication-Results: mx.soverin.net; dmarc=fail header.from=paypal.com\r\n\
            Authentication-Results: mx.soverin.net; dmarc=pass header.from=paypal.com\r\n\
            From: PayPal <service@paypal.com>\r\n\r\n";
        assert!(!authenticated(raw, "PayPal <service@paypal.com>", trusted));
        let raw = b"Authentication-Results: relay.example; dmarc=fail\r\n\
            Authentication-Results: mx.soverin.net; dmarc=pass header.from=paypal.com\r\n\r\n";
        assert!(authenticated(raw, "service@paypal.com", trusted));
        let raw = b"Authentication-Results: mx.soverin.net; dmarc=pass header.from=paypal.com\r\n\
            Authentication-Results: relay.example; dmarc=fail\r\n\r\n";
        assert!(authenticated(raw, "service@paypal.com", trusted));
    }

    #[test]
    fn a_forged_header_from_another_server_does_not_count() {
        let raw =
            b"Authentication-Results: mx.soverin.net.evil; dmarc=pass header.from=paypal.com\r\n\
            Authentication-Results: mx.google.com; dmarc=pass header.from=paypal.com\r\n\r\n";
        assert!(!authenticated(raw, "service@paypal.com", trusted));
        assert!(!authenticated(
            b"Subject: hi\r\n\r\n",
            "service@paypal.com",
            trusted
        ));
    }

    #[test]
    fn the_from_field_needs_exactly_one_address() {
        let raw = b"Authentication-Results: mx.soverin.net; dmarc=pass\r\n\r\n";
        let check = |from: &str| authenticated(raw, from, trusted);
        assert!(check("\"apple.com\" <a@paypal.com>"));
        assert!(!check("a@paypal.com, b@paypal.com"));
        assert!(!check(""));
        assert!(!check("undisclosed"));
    }
}
