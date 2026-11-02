//! The checks every message passes before it is queued, built by Postvak or given whole (`--raw`): a message that
//! fails one is refused with the reason, and nothing is queued.

use mail_parser::MessageParser;

use super::Capabilities;
use crate::config::Identity;

/// The size limit when the server named none: 25 MiB.
pub const DEFAULT_SIZE_LIMIT: usize = 25 * 1024 * 1024;

/// The longest line RFC 5322 allows, without its CRLF.
const MAX_LINE: usize = 998;

/// Who to send to besides the headers, as sendmail's arguments say it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Recipients {
    /// Addresses given as arguments.
    pub extra: Vec<String>,
    /// Whether To, Cc and Bcc are recipients too (`-t`, or no addresses given).
    pub from_headers: bool,
    /// `-f`: the envelope sender, which must equal From.
    pub envelope_from: Option<String>,
}

/// A message that passed: what to send, to whom, and what to keep.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checked {
    /// The From address.
    pub from: String,
    /// Every recipient, once each.
    pub recipients: Vec<String>,
    /// The message as it is transmitted: CRLF line ends, Bcc removed.
    pub transmit: Vec<u8>,
    /// The message as given, with CRLF line ends and Bcc kept: for the outbox and the Sent copy.
    pub keep: Vec<u8>,
    /// Whether the body holds 8-bit bytes.
    pub eight_bit: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CheckError {
    #[error("From {0} is neither the account's address nor one of its aliases")]
    NotOurs(String),
    #[error("From must be a complete address, not an alias pattern with *")]
    WildcardFrom,
    #[error("the message has no recipients")]
    NoRecipients,
    #[error("'{0}' is not an email address")]
    BadAddress(String),
    #[error("the message has no {0} header")]
    MissingHeader(&'static str),
    #[error("the message has a carriage return without a line feed")]
    BareCr,
    #[error("line {0} is longer than 998 bytes")]
    LongLine(usize),
    #[error("header {0} holds non-ASCII text; encode it as RFC 2047 encoded words")]
    NonAsciiHeader(String),
    #[error(
        "the body holds 8-bit text without a declared charset and `Content-Transfer-Encoding: 8bit`"
    )]
    EightBitWithoutCharset,
    #[error("the body holds 8-bit text and the server does not take it (no 8BITMIME)")]
    EightBitNotOffered,
    #[error("the message is {size} bytes and the server takes at most {limit}")]
    TooLarge { size: usize, limit: usize },
    #[error("the message could not be parsed")]
    Unparsable,
    #[error("-f names a different address than From")]
    FromMismatch,
    #[error("From must hold exactly one address")]
    SeveralFrom,
}

/// Checks `raw` for sending as `identity`, against what the server offered last time (`caps`, None when unknown).
pub fn check(
    raw: &[u8],
    args: &Recipients,
    identity: &Identity,
    caps: Option<&Capabilities>,
) -> Result<Checked, CheckError> {
    if raw
        .iter()
        .enumerate()
        .any(|(i, b)| *b == b'\r' && raw.get(i + 1) != Some(&b'\n'))
    {
        return Err(CheckError::BareCr);
    }
    let keep = crlf(raw);
    let header_end = header_end(&keep);
    for (number, line) in keep.split(|b| *b == b'\n').enumerate() {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.len() > MAX_LINE {
            return Err(CheckError::LongLine(number + 1));
        }
    }
    let headers = &keep[..header_end];
    if let Some(line) = headers.split(|b| *b == b'\n').find(|line| !line.is_ascii()) {
        let name = String::from_utf8_lossy(line)
            .split(':')
            .next()
            .unwrap_or_default()
            .trim()
            .to_string();
        return Err(CheckError::NonAsciiHeader(name));
    }
    let (from, recipients, eight_bit) = inspect(&keep, header_end, args, identity, caps)?;
    let transmit = without_bcc(&keep, header_end);
    let limit = caps
        .and_then(|caps| caps.size)
        .and_then(|size| usize::try_from(size).ok())
        .unwrap_or(DEFAULT_SIZE_LIMIT);
    if transmit.len() > limit {
        return Err(CheckError::TooLarge {
            size: transmit.len(),
            limit,
        });
    }
    Ok(Checked {
        from,
        recipients,
        transmit,
        keep,
        eight_bit,
    })
}

/// From, the recipients and whether the body is 8-bit, from the parsed message.
fn inspect(
    keep: &[u8],
    header_end: usize,
    args: &Recipients,
    identity: &Identity,
    caps: Option<&Capabilities>,
) -> Result<(String, Vec<String>, bool), CheckError> {
    let msg = MessageParser::default()
        .parse(keep)
        .ok_or(CheckError::Unparsable)?;
    for (header, name) in [
        ("From", "From"),
        ("Date", "Date"),
        ("Message-ID", "Message-ID"),
    ] {
        if msg.headers().get(header).is_none() {
            return Err(CheckError::MissingHeader(name));
        }
    }
    let from_list = msg.from().ok_or(CheckError::MissingHeader("From"))?;
    let mut froms = from_list.mailboxes();
    let from = froms
        .next()
        .and_then(|m| m.address())
        .map(str::to_string)
        .ok_or(CheckError::MissingHeader("From"))?;
    if froms.next().is_some() {
        return Err(CheckError::SeveralFrom);
    }
    check_from(&from, identity)?;
    if let Some(envelope_from) = &args.envelope_from
        && !envelope_from.trim().eq_ignore_ascii_case(&from)
    {
        return Err(CheckError::FromMismatch);
    }

    let mut recipients: Vec<String> = Vec::new();
    let mut add = |address: &str| -> Result<(), CheckError> {
        let address = address.trim();
        valid_address(address)?;
        if !recipients.iter().any(|r| r.eq_ignore_ascii_case(address)) {
            recipients.push(address.to_string());
        }
        Ok(())
    };
    for address in &args.extra {
        add(address)?;
    }
    if args.from_headers {
        for list in [msg.to(), msg.cc(), msg.bcc()].into_iter().flatten() {
            for mailbox in list.mailboxes() {
                match mailbox.address() {
                    Some(address) => add(address)?,
                    None => {
                        return Err(CheckError::BadAddress(
                            mailbox.name().unwrap_or_default().to_string(),
                        ));
                    }
                }
            }
        }
    }
    if recipients.is_empty() {
        return Err(CheckError::NoRecipients);
    }

    let eight_bit = !keep[header_end..].is_ascii();
    if eight_bit {
        let declared = msg.parts().all(|part| {
            part.is_multipart()
                || part.raw_body().is_ascii()
                || (part
                    .content_transfer_encoding()
                    .is_some_and(|cte| cte.trim().eq_ignore_ascii_case("8bit"))
                    && (!part.is_text()
                        || part
                            .content_type()
                            .is_some_and(|ct| ct.attribute("charset").is_some())))
        });
        if !declared {
            return Err(CheckError::EightBitWithoutCharset);
        }
        if caps.is_some_and(|caps| !caps.eight_bit_mime) {
            return Err(CheckError::EightBitNotOffered);
        }
    }

    Ok((from, recipients, eight_bit))
}

/// Whether `from` may be sent from as `identity`: a complete address, the account's or one an alias matches.
pub fn check_from(from: &str, identity: &Identity) -> Result<(), CheckError> {
    if from.contains('*') {
        return Err(CheckError::WildcardFrom);
    }
    valid_address(from)?;
    if !identity.is_me(from) {
        return Err(CheckError::NotOurs(from.to_string()));
    }
    Ok(())
}

/// Whether `address` is an addr-spec (`local@domain`) SMTP can send to.
pub fn valid_address(address: &str) -> Result<(), CheckError> {
    address
        .parse::<lettre::Address>()
        .map(drop)
        .map_err(|_| CheckError::BadAddress(address.to_string()))
}

/// `raw` with every line ending in CRLF; a lone CR was refused before.
fn crlf(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len() + raw.len() / 40);
    for (i, b) in raw.iter().enumerate() {
        if *b == b'\n' && (i == 0 || raw[i - 1] != b'\r') {
            out.push(b'\r');
        }
        out.push(*b);
    }
    out
}

/// Where the header block ends in a CRLF message: after its last header line's CRLF, before the blank line; the whole
/// message when there is no blank line.
fn header_end(message: &[u8]) -> usize {
    if message.starts_with(b"\r\n") {
        return 0;
    }
    message
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| i + 2)
        .unwrap_or(message.len())
}

/// `message` without its Bcc header and that header's continuation lines; nothing else changes.
pub fn without_bcc(message: &[u8], header_end: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(message.len());
    let mut dropping = false;
    let headers = &message[..header_end];
    for line in headers.split_inclusive(|b| *b == b'\n') {
        let continuation = line.first().is_some_and(|b| *b == b' ' || *b == b'\t');
        if !continuation {
            dropping = line.len() >= 4 && line[..4].eq_ignore_ascii_case(b"bcc:");
        }
        if !dropping {
            out.extend_from_slice(line);
        }
    }
    out.extend_from_slice(&message[header_end..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AccountConfig, PasswordSource};

    fn identity() -> Identity {
        let mut account = AccountConfig::new(
            "work",
            "imap.example.com",
            "me@example.com",
            PasswordSource::Keyring { keyring: true },
        );
        account.aliases = vec!["*@shop.example.com".into(), "alias@example.org".into()];
        account.identity().unwrap()
    }

    const HEADERS: &str = "From: Me <me@example.com>\r\nTo: a@x.org\r\nDate: Mon, 2 Nov 2026 10:00:00 +0000\r\nMessage-ID: <1@example.com>\r\nSubject: hi\r\n";

    fn message(extra_headers: &str, body: &str) -> Vec<u8> {
        format!("{HEADERS}{extra_headers}\r\n{body}").into_bytes()
    }

    fn headers_only() -> Recipients {
        Recipients {
            from_headers: true,
            ..Default::default()
        }
    }

    fn run(raw: &[u8]) -> Result<Checked, CheckError> {
        check(raw, &headers_only(), &identity(), None)
    }

    #[test]
    fn a_plain_message_passes() {
        let checked = run(&message("", "hello\r\n")).unwrap();
        assert_eq!(checked.from, "me@example.com");
        assert_eq!(checked.recipients, ["a@x.org"]);
        assert!(!checked.eight_bit);
        assert_eq!(checked.transmit, checked.keep);
    }

    #[test]
    fn each_refusal() {
        let id = identity();
        let refused = |raw: Vec<u8>, args: &Recipients, caps: Option<&Capabilities>| {
            check(&raw, args, &id, caps).unwrap_err()
        };
        let h = headers_only();
        assert_eq!(
            refused(b"\r\n".to_vec(), &h, None),
            CheckError::MissingHeader("From")
        );
        assert_eq!(
            refused(
                HEADERS
                    .replace("me@example.com", "you@example.com")
                    .into_bytes(),
                &h,
                None
            ),
            CheckError::NotOurs("you@example.com".into())
        );
        assert_eq!(
            refused(
                HEADERS.replace("To: a@x.org\r\n", "").into_bytes(),
                &h,
                None
            ),
            CheckError::NoRecipients
        );
        assert_eq!(
            refused(
                HEADERS
                    .replace("To: a@x.org", "To: not-an-address@")
                    .into_bytes(),
                &h,
                None
            ),
            CheckError::BadAddress("not-an-address@".into())
        );
        for header in ["Date", "Message-ID"] {
            let raw: String = HEADERS
                .split_inclusive("\r\n")
                .filter(|line| !line.starts_with(&format!("{header}:")))
                .collect();
            assert_eq!(
                refused(raw.into_bytes(), &h, None),
                CheckError::MissingHeader(if header == "Date" {
                    "Date"
                } else {
                    "Message-ID"
                })
            );
        }
        assert_eq!(refused(message("", "a\rb"), &h, None), CheckError::BareCr);
        assert_eq!(
            refused(message("", &"x".repeat(999)), &h, None),
            CheckError::LongLine(7)
        );
        assert_eq!(
            refused(message("X-Name: Zoë\r\n", "x"), &h, None),
            CheckError::NonAsciiHeader("X-Name".into())
        );
        assert_eq!(
            refused(message("", "Zoë"), &h, None),
            CheckError::EightBitWithoutCharset
        );
        let eight = message(
            "MIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n",
            "Zoë",
        );
        let no_8bitmime = Capabilities::default();
        assert_eq!(
            refused(eight.clone(), &h, Some(&no_8bitmime)),
            CheckError::EightBitNotOffered
        );
        let offered = Capabilities {
            eight_bit_mime: true,
            size: Some(100),
        };
        assert!(check(&eight, &h, &id, None).unwrap().eight_bit);
        assert!(matches!(
            refused(eight.clone(), &h, Some(&offered)),
            CheckError::TooLarge { limit: 100, .. }
        ));
        let mismatch = Recipients {
            envelope_from: Some("other@example.com".into()),
            ..headers_only()
        };
        assert_eq!(
            refused(message("", "x"), &mismatch, None),
            CheckError::FromMismatch
        );
        assert_eq!(
            refused(
                HEADERS
                    .replace(
                        "From: Me <me@example.com>",
                        "From: me@example.com, alias@example.org"
                    )
                    .into_bytes(),
                &h,
                None
            ),
            CheckError::SeveralFrom
        );
        assert_eq!(
            refused(
                HEADERS
                    .replace("From: Me <me@example.com>", "To: a@x.org")
                    .into_bytes(),
                &h,
                None
            ),
            CheckError::MissingHeader("From")
        );
    }

    #[test]
    fn a_wildcard_alias_accepts_a_complete_address_only() {
        let shop = HEADERS.replace("me@example.com", "shop@shop.example.com");
        assert_eq!(run(shop.as_bytes()).unwrap().from, "shop@shop.example.com");
        let star = HEADERS.replace("me@example.com", "*@shop.example.com");
        assert_eq!(run(star.as_bytes()).unwrap_err(), CheckError::WildcardFrom);
        assert_eq!(
            check_from("*@shop.example.com", &identity()),
            Err(CheckError::WildcardFrom)
        );
        assert_eq!(check_from("Alias@Example.org", &identity()), Ok(()));
    }

    #[test]
    fn bcc_is_removed_from_what_is_transmitted_and_kept_in_the_copy() {
        let raw = message(
            "Bcc: hidden@y.org,\r\n  other@y.org\r\nX-After: kept\r\n",
            "body\r\n",
        );
        let checked = run(&raw).unwrap();
        assert_eq!(
            checked.recipients,
            ["a@x.org", "hidden@y.org", "other@y.org"]
        );
        let transmit = String::from_utf8(checked.transmit).unwrap();
        assert!(!transmit.contains("hidden"), "{transmit}");
        assert!(!transmit.to_lowercase().contains("bcc"), "{transmit}");
        assert!(
            transmit.contains("X-After: kept\r\n\r\nbody\r\n"),
            "{transmit}"
        );
        assert_eq!(checked.keep, raw);
        // A Bcc in the body is the body's.
        let body = run(&message("", "Bcc: stays\r\n")).unwrap();
        assert!(
            String::from_utf8(body.transmit)
                .unwrap()
                .ends_with("Bcc: stays\r\n")
        );
    }

    #[test]
    fn size_falls_back_to_25_mib() {
        let body = "x".repeat(76) + "\r\n";
        let lines = DEFAULT_SIZE_LIMIT / body.len() + 1;
        let raw = message("", &body.repeat(lines));
        assert!(matches!(
            run(&raw).unwrap_err(),
            CheckError::TooLarge {
                limit: 26_214_400,
                ..
            }
        ));
    }

    #[test]
    fn lf_line_ends_are_accepted_and_sent_as_crlf() {
        let raw = message("", "one\r\ntwo\r\n")
            .iter()
            .copied()
            .filter(|b| *b != b'\r')
            .collect::<Vec<_>>();
        let checked = run(&raw).unwrap();
        assert_eq!(checked.keep, message("", "one\r\ntwo\r\n"));
        assert_eq!(checked.transmit, checked.keep);
    }

    #[test]
    fn recipients_from_args_and_headers_are_deduplicated_case_insensitively() {
        let raw = message("Cc: B@x.org, a@X.org\r\n", "x");
        let args = Recipients {
            extra: vec!["b@x.org".into(), "c@x.org".into()],
            from_headers: true,
            envelope_from: Some("ME@example.com".into()),
        };
        let checked = check(&raw, &args, &identity(), None).unwrap();
        assert_eq!(checked.recipients, ["b@x.org", "c@x.org", "a@x.org"]);
        // Without -t, only the arguments.
        let args = Recipients {
            extra: vec!["c@x.org".into()],
            ..Default::default()
        };
        assert_eq!(
            check(&raw, &args, &identity(), None).unwrap().recipients,
            ["c@x.org"]
        );
        let bad = Recipients {
            extra: vec!["nope".into()],
            ..Default::default()
        };
        assert_eq!(
            check(&raw, &bad, &identity(), None).unwrap_err(),
            CheckError::BadAddress("nope".into())
        );
    }
}
