//! Building a message from what the user wrote: plain text as `format=flowed`, the HTML of a quote when there is one,
//! attachments, and the headers a reply or forward needs.

use std::time::{Duration, SystemTime};

use lettre::message::header::{ContentTransferEncoding, ContentType, HeaderName, HeaderValue};
use lettre::message::{Attachment, Mailbox, MultiPart, SinglePart};

/// The width plain text is wrapped at.
const WIDTH: usize = 72;

/// A message to build. Bcc is kept in what [`build`] returns; the checks remove it for transmission.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Draft {
    pub from: String,
    pub from_name: Option<String>,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub subject: String,
    /// The plain text, line ends as `\n`.
    pub text: String,
    /// An HTML alternative: only for a reply or forward of an HTML original.
    pub html: Option<HtmlPart>,
    pub attachments: Vec<FileAttachment>,
    /// The Message-ID answered, with its angle brackets.
    pub in_reply_to: Option<String>,
    /// The thread's Message-IDs, oldest first, with their angle brackets.
    pub references: Vec<String>,
    /// A whole message enclosed as a `message/rfc822` part, byte for byte.
    pub enclosed: Option<Vec<u8>>,
    /// Written as the `X-Postvak-Draft` header: what kind of draft this is, for opening it again.
    pub postvak_draft: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileAttachment {
    pub name: String,
    pub content_type: String,
    pub bytes: Vec<u8>,
}

/// The HTML alternative and the images its `cid:` URLs name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HtmlPart {
    pub html: String,
    pub inline: Vec<InlineImage>,
}

/// A part the HTML names as `cid:…`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineImage {
    /// The Content-ID without its angle brackets.
    pub cid: String,
    pub content_type: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BuildError {
    #[error("'{0}' is not an email address")]
    Address(String),
    #[error("{0}")]
    Message(String),
}

/// The MIME type for a file name, by its extension; `application/octet-stream` for one not in the table.
pub fn content_type_for(name: &str) -> &'static str {
    let extension = name
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "txt" | "text" | "log" => "text/plain",
        "md" => "text/markdown",
        "csv" => "text/csv",
        "html" | "htm" => "text/html",
        "ics" => "text/calendar",
        "vcf" => "text/vcard",
        "patch" | "diff" => "text/x-diff",
        "pdf" => "application/pdf",
        "json" => "application/json",
        "xml" => "application/xml",
        "zip" => "application/zip",
        "gz" => "application/gzip",
        "tar" => "application/x-tar",
        "7z" => "application/x-7z-compressed",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "odt" => "application/vnd.oasis.opendocument.text",
        "ods" => "application/vnd.oasis.opendocument.spreadsheet",
        "odp" => "application/vnd.oasis.opendocument.presentation",
        "eml" => "message/rfc822",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "heic" => "image/heic",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" => "audio/ogg",
        "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        _ => "application/octet-stream",
    }
}

/// `text` as `format=flowed` (RFC 3676): the user's lines wrapped at 72 with a space before each soft break, quoted
/// lines (`>`) and the signature separator left whole, trailing spaces trimmed from the rest, and a line that would
/// start with a space or `From ` stuffed with a space.
pub fn flowed(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + text.len() / 30);
    let text = text.replace("\r\n", "\n");
    for line in text.split('\n') {
        if line == "-- " {
            out.push_str(line);
        } else if line.starts_with('>') {
            out.push_str(line.trim_end_matches(' '));
        } else {
            for (i, piece) in wrap(line.trim_end_matches(' ')).iter().enumerate() {
                if i > 0 {
                    out.push('\n');
                }
                if piece.starts_with(' ') || piece.starts_with("From ") {
                    out.push(' ');
                }
                out.push_str(piece);
            }
        }
        out.push('\n');
    }
    // `split` gave one line more than the text had line ends.
    out.pop();
    out
}

/// One unquoted line in pieces of at most `WIDTH` characters, each but the last ending in the space it was broken at.
/// A word longer than the width is left whole.
fn wrap(line: &str) -> Vec<String> {
    let mut pieces = Vec::new();
    let mut current = String::new();
    let mut width = 0;
    for (i, word) in line.split(' ').enumerate() {
        let len = word.chars().count();
        if i > 0 {
            // Room for the space and the word, and the space a later break would leave at the end.
            if width > 0 && width + 1 + len + 1 > WIDTH {
                current.push(' ');
                pieces.push(std::mem::take(&mut current));
                width = 0;
                current.push_str(word);
                width += len;
                continue;
            }
            current.push(' ');
            width += 1;
        }
        current.push_str(word);
        width += len;
    }
    pieces.push(current);
    pieces
}

/// A new Message-ID on `from`'s domain.
pub fn message_id(from: &str) -> String {
    let domain = from.rsplit_once('@').map_or("postvak.invalid", |(_, d)| d);
    let (a, b) = (crate::sync::random_u64(), crate::sync::random_u64());
    format!("<{a:016x}{b:016x}@{domain}>")
}

fn mailbox(name: Option<&str>, address: &str) -> Result<Mailbox, BuildError> {
    let address = address
        .trim()
        .parse()
        .map_err(|_| BuildError::Address(address.to_string()))?;
    Ok(Mailbox::new(name.and_then(header_name), address))
}

/// A display name fit for a header: control characters, which lettre cannot write, become spaces.
fn header_name(name: &str) -> Option<String> {
    let name: String = name
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    Some(name.trim().to_string()).filter(|name| !name.is_empty())
}

/// A recipient as written: `linus@x.org` or `Linus Example <linus@x.org>`.
fn recipient(text: &str) -> Result<Mailbox, BuildError> {
    text.trim()
        .parse()
        .map_err(|_| BuildError::Address(text.to_string()))
}

/// A recipient with its name, as [`Draft`] holds one: `Linus Example <linus@x.org>`, quoted where the name needs it.
pub fn mailbox_text(name: Option<&str>, address: &str) -> String {
    match (
        name.and_then(header_name),
        address.trim().parse::<lettre::Address>(),
    ) {
        (Some(name), Ok(parsed)) => Mailbox::new(Some(name), parsed).to_string(),
        _ => address.trim().to_string(),
    }
}

/// The bare address of a recipient as [`Draft`] holds one.
pub fn bare_address(text: &str) -> String {
    match text.trim().parse::<Mailbox>() {
        Ok(mailbox) => mailbox.email.to_string(),
        Err(_) => text.trim().to_string(),
    }
}

/// The message as it will be kept: Date at `now`, a new Message-ID, MIME-Version, and Bcc still in it.
pub fn build(draft: &Draft, now: i64) -> Result<Vec<u8>, BuildError> {
    let mut builder = lettre::Message::builder()
        .from(mailbox(draft.from_name.as_deref(), &draft.from)?)
        .date(SystemTime::UNIX_EPOCH + Duration::from_secs(u64::try_from(now).unwrap_or(0)))
        .message_id(Some(message_id(&draft.from)))
        .subject(draft.subject.clone())
        .keep_bcc();
    for to in &draft.to {
        builder = builder.to(recipient(to)?);
    }
    for cc in &draft.cc {
        builder = builder.cc(recipient(cc)?);
    }
    for bcc in &draft.bcc {
        builder = builder.bcc(recipient(bcc)?);
    }
    if let Some(id) = &draft.in_reply_to {
        builder = builder.in_reply_to(id.clone());
    }
    if !draft.references.is_empty() {
        builder = builder.references(draft.references.join(" "));
    }
    if let Some(value) = &draft.postvak_draft {
        builder = builder.raw_header(HeaderValue::new(
            HeaderName::new_from_ascii_str("X-Postvak-Draft"),
            value.clone(),
        ));
    }
    if draft.to.is_empty() && draft.cc.is_empty() && draft.bcc.is_empty() {
        // lettre wants an envelope recipient even for a message only formatted, as a draft without one yet is.
        let from: lettre::Address =
            (draft.from.trim().parse()).map_err(|_| BuildError::Address(draft.from.clone()))?;
        let envelope = lettre::address::Envelope::new(Some(from.clone()), vec![from])
            .map_err(|e| BuildError::Message(e.to_string()))?;
        builder = builder.envelope(envelope);
    }

    let text = text_part(&draft.text)?;
    let attached = !draft.attachments.is_empty() || draft.enclosed.is_some();
    let message = match (&draft.html, attached) {
        (None, false) => builder.singlepart(text),
        (html, _) => {
            let body = match html {
                Some(html) => Body::Multi(
                    MultiPart::alternative()
                        .singlepart(text)
                        .multipart(html_part(html)?),
                ),
                None => Body::Single(text),
            };
            if attached {
                let mut mixed = match body {
                    Body::Multi(part) => MultiPart::mixed().multipart(part),
                    Body::Single(part) => MultiPart::mixed().singlepart(part),
                };
                for file in &draft.attachments {
                    mixed = mixed.singlepart(
                        Attachment::new(file.name.clone())
                            .body(file.bytes.clone(), content_type(&file.content_type)?),
                    );
                }
                if let Some(raw) = &draft.enclosed {
                    mixed = mixed.singlepart(enclosed_part(raw)?);
                }
                builder.multipart(mixed)
            } else {
                match body {
                    Body::Multi(part) => builder.multipart(part),
                    Body::Single(part) => builder.singlepart(part),
                }
            }
        }
    }
    .map_err(|e| BuildError::Message(e.to_string()))?;
    Ok(message.formatted())
}

enum Body {
    Single(SinglePart),
    Multi(MultiPart),
}

fn content_type(text: &str) -> Result<ContentType, BuildError> {
    ContentType::parse(text).map_err(|e| BuildError::Message(format!("content type {text}: {e}")))
}

fn text_part(text: &str) -> Result<SinglePart, BuildError> {
    let flowed = flowed(text);
    let body = lettre::message::Body::new_with_encoding(flowed, ContentTransferEncoding::SevenBit)
        .or_else(|bytes| {
            lettre::message::Body::new_with_encoding(
                bytes,
                ContentTransferEncoding::QuotedPrintable,
            )
        })
        .map_err(|_| BuildError::Message("the text could not be encoded".into()))?;
    Ok(SinglePart::builder()
        .header(content_type("text/plain; charset=utf-8; format=flowed")?)
        .body(body))
}

fn html_part(html: &HtmlPart) -> Result<MultiPart, BuildError> {
    let part = SinglePart::html(html.html.clone());
    if html.inline.is_empty() {
        // `alternative` takes a MultiPart; a related part of one keeps the shape simple for every client.
        return Ok(MultiPart::related().singlepart(part));
    }
    let mut related = MultiPart::related().singlepart(part);
    for image in &html.inline {
        related = related.singlepart(
            Attachment::new_inline(image.cid.clone())
                .body(image.bytes.clone(), content_type(&image.content_type)?),
        );
    }
    Ok(related)
}

/// A whole message as a `message/rfc822` part, untouched: 7bit or 8bit as its bytes allow, base64 only for one no
/// mail transport could carry as it is.
fn enclosed_part(raw: &[u8]) -> Result<SinglePart, BuildError> {
    let body =
        lettre::message::Body::new_with_encoding(raw.to_vec(), ContentTransferEncoding::SevenBit)
            .or_else(|bytes| {
                lettre::message::Body::new_with_encoding(bytes, ContentTransferEncoding::EightBit)
            })
            .or_else(|bytes| {
                lettre::message::Body::new_with_encoding(bytes, ContentTransferEncoding::Base64)
            })
            .map_err(|_| BuildError::Message("the message could not be enclosed".into()))?;
    Ok(SinglePart::builder()
        .header(content_type("message/rfc822")?)
        .body(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_control_character_in_a_name_becomes_a_space() {
        assert_eq!(
            mailbox_text(Some("Linus\r Example"), "linus@x.org"),
            "Linus  Example <linus@x.org>"
        );
        assert_eq!(mailbox_text(Some("\r\n"), "linus@x.org"), "linus@x.org");
    }
    use mail_parser::MessageParser;

    fn draft() -> Draft {
        Draft {
            from: "me@example.com".into(),
            to: vec!["linus@x.org".into()],
            subject: "Hello".into(),
            text: "Hi Linus,\n\nsee you.\n".into(),
            ..Default::default()
        }
    }

    fn header(raw: &[u8], name: &str) -> String {
        crate::message::header_value(raw, name).unwrap_or_default()
    }

    #[test]
    fn plain_message_has_the_required_headers() {
        let raw = build(
            &Draft {
                from_name: Some("Pieter Willekens".into()),
                cc: vec!["cc@x.org".into()],
                bcc: vec!["hidden@y.org".into()],
                ..draft()
            },
            1_793_620_800,
        )
        .unwrap();
        let msg = MessageParser::default().parse(&raw).unwrap();
        let from = msg.from().unwrap().first().unwrap();
        assert_eq!(from.name(), Some("Pieter Willekens"));
        assert_eq!(from.address(), Some("me@example.com"));
        assert_eq!(
            msg.to().unwrap().first().unwrap().address(),
            Some("linus@x.org")
        );
        assert_eq!(
            msg.cc().unwrap().first().unwrap().address(),
            Some("cc@x.org")
        );
        assert_eq!(
            msg.bcc().unwrap().first().unwrap().address(),
            Some("hidden@y.org")
        );
        assert_eq!(msg.date().unwrap().to_timestamp(), 1_793_620_800);
        assert!(
            msg.message_id().unwrap().ends_with("@example.com"),
            "{:?}",
            msg.message_id()
        );
        assert_eq!(header(&raw, "MIME-Version"), "1.0");
        let ct = header(&raw, "Content-Type")
            .to_ascii_lowercase()
            .replace(' ', "");
        assert_eq!(ct, "text/plain;charset=utf-8;format=flowed");
        assert_eq!(
            msg.body_text(0).unwrap().trim_end(),
            "Hi Linus,\r\n\r\nsee you."
        );
        // No name: the bare address.
        let raw = build(&draft(), 0).unwrap();
        assert_eq!(header(&raw, "From"), "me@example.com");
        assert_ne!(message_id("me@example.com"), message_id("me@example.com"));
    }

    #[test]
    fn long_lines_are_flowed_at_72_and_quotes_are_not() {
        let long = "word ".repeat(40).trim_end().to_string();
        let quote = format!("> {}", "quoted ".repeat(30).trim_end());
        let out = flowed(&format!("{long}\n{quote}\n-- \nsig"));
        let lines: Vec<&str> = out.split('\n').collect();
        let soft = &lines[..lines.len() - 4];
        assert!(soft.len() > 1);
        for line in soft {
            assert!(line.chars().count() <= 72, "{line:?}");
            assert!(line.ends_with(' '), "{line:?}");
        }
        assert!(!lines[lines.len() - 4].ends_with(' '));
        assert_eq!(lines[lines.len() - 3], quote);
        assert_eq!(lines[lines.len() - 2], "-- ");
        // Unflowed, the words are all there in order.
        let joined: String = lines[..lines.len() - 3].concat();
        assert_eq!(joined, long);
    }

    #[test]
    fn flowed_stuffs_and_trims() {
        assert_eq!(
            flowed("From here\n indented\ntrailing  "),
            " From here\n  indented\ntrailing"
        );
        assert_eq!(flowed("a\n\nb\n"), "a\n\nb\n");
        let word = "x".repeat(100);
        assert_eq!(flowed(&format!("a {word} b")), format!("a \n{word} \nb"));
    }

    #[test]
    fn non_ascii_subject_is_rfc2047_encoded_and_reads_back() {
        let raw = build(
            &Draft {
                subject: "Zoë's café ☕".into(),
                text: "Grüße\n".into(),
                ..draft()
            },
            0,
        )
        .unwrap();
        let headers = &raw[..raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap()];
        assert!(headers.is_ascii());
        let msg = MessageParser::default().parse(&raw).unwrap();
        assert_eq!(msg.subject(), Some("Zoë's café ☕"));
        assert_eq!(msg.body_text(0).unwrap().trim_end(), "Grüße");
        assert!(raw.is_ascii());
    }

    #[test]
    fn attachments_make_multipart_mixed() {
        let raw = build(
            &Draft {
                attachments: vec![FileAttachment {
                    name: "report.pdf".into(),
                    content_type: content_type_for("report.pdf").into(),
                    bytes: b"%PDF-1.7 \x00\xff".to_vec(),
                }],
                ..draft()
            },
            0,
        )
        .unwrap();
        assert!(header(&raw, "Content-Type").starts_with("multipart/mixed"));
        let attachments = crate::message::attachments(&raw);
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].name.as_deref(), Some("report.pdf"));
        assert_eq!(attachments[0].content_type, "application/pdf");
        assert_eq!(
            crate::message::attachment(&raw, 1).unwrap().1,
            b"%PDF-1.7 \x00\xff"
        );
        assert_eq!(content_type_for("x.UNKNOWN"), "application/octet-stream");
        assert_eq!(content_type_for("noextension"), "application/octet-stream");
        assert_eq!(content_type_for("photo.JPG"), "image/jpeg");
    }

    #[test]
    fn html_with_inline_makes_alternative_with_related() {
        let raw = build(
            &Draft {
                html: Some(HtmlPart {
                    html: "<p>hi</p><img src=\"cid:logo@x\">".into(),
                    inline: vec![InlineImage {
                        cid: "logo@x".into(),
                        content_type: "image/png".into(),
                        bytes: vec![0x89, b'P', b'N', b'G'],
                    }],
                }),
                ..draft()
            },
            0,
        )
        .unwrap();
        assert!(header(&raw, "Content-Type").starts_with("multipart/alternative"));
        let (_, html) = crate::message::attachments_and_html(&raw);
        let html = html.unwrap();
        assert!(html.html.contains("cid:logo@x"));
        assert_eq!(html.inline.len(), 1);
        assert_eq!(html.inline[0].cid, "logo@x");
        assert_eq!(html.inline[0].bytes, [0x89, b'P', b'N', b'G']);
        let text = String::from_utf8_lossy(&raw);
        assert!(text.contains("multipart/related"), "{text}");
        assert_eq!(
            crate::message::body_text(&raw).trim_end(),
            "Hi Linus,\r\n\r\nsee you."
        );
    }

    #[test]
    fn enclosed_message_is_a_message_rfc822_part_byte_for_byte() {
        let original =
            b"From: a@x.org\r\nDKIM-Signature: v=1; b=abc\r\nSubject: s\r\n\r\nbody  \r\n".to_vec();
        let raw = build(
            &Draft {
                enclosed: Some(original.clone()),
                ..draft()
            },
            0,
        )
        .unwrap();
        assert!(header(&raw, "Content-Type").starts_with("multipart/mixed"));
        let at = raw
            .windows(original.len())
            .position(|w| w == original.as_slice());
        assert!(at.is_some(), "{}", String::from_utf8_lossy(&raw));
        let text = String::from_utf8_lossy(&raw);
        assert!(text.contains("Content-Type: message/rfc822"), "{text}");
    }

    #[test]
    fn recipients_keep_their_names() {
        let raw = build(
            &Draft {
                to: vec![mailbox_text(Some("Linus, Example"), "linus@x.org")],
                ..draft()
            },
            0,
        )
        .unwrap();
        let msg = MessageParser::default().parse(&raw).unwrap();
        let to = msg.to().unwrap().first().unwrap();
        assert_eq!(
            (to.name(), to.address()),
            (Some("Linus, Example"), Some("linus@x.org"))
        );
        assert_eq!(
            bare_address("\"Linus, Example\" <linus@x.org>"),
            "linus@x.org"
        );
        assert_eq!(bare_address("linus@x.org"), "linus@x.org");
        assert!(
            build(
                &Draft {
                    to: vec!["nope".into()],
                    ..draft()
                },
                0
            )
            .is_err()
        );
    }

    #[test]
    fn reply_headers_are_written() {
        let raw = build(
            &Draft {
                in_reply_to: Some("<b@x.org>".into()),
                references: vec!["<a@x.org>".into(), "<b@x.org>".into()],
                ..draft()
            },
            0,
        )
        .unwrap();
        let parsed = crate::message::parse_headers(&raw);
        assert_eq!(parsed.in_reply_to.as_deref(), Some("b@x.org"));
        assert_eq!(parsed.references, ["a@x.org", "b@x.org"]);
    }
}
