//! Replies and forwards of a stored message: who they go to, the subject, the threading headers, and the original
//! quoted below the user's text, as other mail clients place it.

use chrono::Datelike;
use mail_parser::MessageParser;
use serde::{Deserialize, Serialize};

use super::build::{Draft, FileAttachment, InlineImage, mailbox_text};
use super::quote_html::{self, Original};
use crate::config::Identity;

/// Who a reply goes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyTo {
    /// The sender (Reply-To, else From).
    Sender,
    /// The sender and everyone else the original went to.
    All,
    /// The mailing list the original came through (`List-Post`).
    List,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ForwardAs {
    /// The original's text below a header block, its attachments attached again.
    #[default]
    Inline,
    /// The original whole, as a `message/rfc822` part, so its signature still holds.
    Attachment,
}

/// Which of the original's attachments an inline forward takes along.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AttachmentFilter {
    #[default]
    All,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForwardOptions {
    pub as_: ForwardAs,
    /// Put in front of the subject unless it already starts with a forward prefix; may be empty.
    pub subject_prefix: String,
    /// Text above the user's.
    pub note: Option<String>,
    pub attachments: AttachmentFilter,
}

impl Default for ForwardOptions {
    fn default() -> Self {
        ForwardOptions {
            as_: ForwardAs::Inline,
            subject_prefix: "Fwd: ".into(),
            note: None,
            attachments: AttachmentFilter::All,
        }
    }
}

/// Who a forward goes to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Recipients {
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
}

/// The flag the original gets once the answer has been sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OriginalFlag {
    Answered,
    Forwarded,
}

impl OriginalFlag {
    pub fn flag(&self) -> &'static str {
        match self {
            OriginalFlag::Answered => "\\Answered",
            OriginalFlag::Forwarded => "$Forwarded",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReplyError {
    #[error("the original message could not be read")]
    Unparsable,
    #[error("the message came through no mailing list (it has no List-Post header)")]
    NoListPost,
    #[error("there is nobody to reply to: the original names no other address")]
    NoRecipients,
}

/// Prefixes that mark a subject as a reply or forward already, in the languages mail clients write them.
const REPLY_PREFIXES: &[&str] = &[
    "re:", "aw:", "sv:", "antw:", "fwd:", "fw:", "wg:", "tr:", "doorst:",
];
const FORWARD_PREFIXES: &[&str] = &["fwd:", "fw:", "wg:", "tr:", "doorst:"];

/// `subject` with `prefix` in front, unless it already starts with one that means the same: for `Re: ` any reply or
/// forward prefix, for `Fwd: ` any forward prefix, for another prefix that prefix itself.
pub fn prefixed(subject: &str, prefix: &str) -> String {
    let subject = subject.trim();
    let lower = subject.to_lowercase();
    let wanted = prefix.trim().to_lowercase();
    let known: &[&str] = match wanted.as_str() {
        "" => return subject.to_string(),
        "re:" => REPLY_PREFIXES,
        "fwd:" => FORWARD_PREFIXES,
        _ => &[],
    };
    let has = known.iter().any(|p| lower.starts_with(p)) || lower.starts_with(&wanted);
    if has {
        subject.to_string()
    } else {
        format!("{}{subject}", prefix)
    }
}

/// Our address the original was sent to: the first in To, Cc or Delivered-To the identity owns (never a wildcard
/// pattern), else the account's address.
pub fn our_address(original_headers: &[u8], identity: &Identity) -> String {
    let Some(msg) = MessageParser::default().parse_headers(original_headers) else {
        return identity.address().to_string();
    };
    let listed = [msg.to(), msg.cc()].into_iter().flatten().flat_map(|list| {
        list.mailboxes()
            .filter_map(|m| m.address())
            .map(str::to_string)
            .collect::<Vec<_>>()
    });
    let delivered = crate::message::header_value(original_headers, "Delivered-To");
    listed
        .chain(delivered)
        .map(|address| {
            address
                .trim()
                .trim_start_matches('<')
                .trim_end_matches('>')
                .to_string()
        })
        .find(|address| !address.contains('*') && identity.is_me(address))
        .unwrap_or_else(|| identity.address().to_string())
}

/// What a reply or forward needs from the original.
struct Read {
    from: Vec<(Option<String>, String)>,
    reply_to: Vec<(Option<String>, String)>,
    to: Vec<(Option<String>, String)>,
    cc: Vec<(Option<String>, String)>,
    list_post: Option<String>,
    subject: String,
    message_id: Option<String>,
    references: Vec<String>,
    date: Option<mail_parser::DateTime>,
    date_text: Option<String>,
    from_text: String,
    to_text: String,
    cc_text: String,
    text: String,
    html: Option<Original>,
    attachments: Vec<FileAttachment>,
}

/// What a reply or forward needs from `raw`; its attachments decoded only when `attachments` asks for them.
fn read(raw: &[u8], attachments: bool) -> Result<Read, ReplyError> {
    let msg = MessageParser::default()
        .parse(raw)
        .ok_or(ReplyError::Unparsable)?;
    let mailboxes = |list: Option<mail_parser::AddressList<'_>>| -> Vec<(Option<String>, String)> {
        list.map(|list| {
            list.mailboxes()
                .filter_map(|m| Some((m.name().map(str::to_string), m.address()?.to_string())))
                .collect()
        })
        .unwrap_or_default()
    };
    let list_post = msg.list_post().and_then(|list| {
        list.mailboxes()
            .filter_map(|m| m.address())
            .find_map(|url| {
                let rest = url.trim().strip_prefix("mailto:").or_else(|| {
                    url.trim()
                        .get(..7)
                        .filter(|scheme| scheme.eq_ignore_ascii_case("mailto:"))
                        .map(|_| &url.trim()[7..])
                })?;
                let address = rest.split('?').next().unwrap_or_default().trim();
                (!address.is_empty()).then(|| address.to_string())
            })
    });
    let headers = crate::message::parse_headers(raw);
    let (html, inline, attachments) = parts(&msg, attachments);
    let html = html.map(|html| Original { html, inline });
    Ok(Read {
        from: mailboxes(msg.from()),
        reply_to: mailboxes(msg.reply_to()),
        to: mailboxes(msg.to()),
        cc: mailboxes(msg.cc()),
        list_post,
        subject: msg.subject().unwrap_or_default().to_string(),
        message_id: msg.message_id().map(|id| format!("<{id}>")),
        references: headers
            .references
            .iter()
            .map(|id| format!("<{id}>"))
            .collect(),
        date: msg.date(),
        date_text: crate::message::header_text(raw, "Date"),
        from_text: headers.from.unwrap_or_default(),
        to_text: headers.to.unwrap_or_default(),
        cc_text: headers.cc.unwrap_or_default(),
        text: crate::message::body_text(raw),
        html,
        attachments,
    })
}

/// The original's HTML part as the reader shows it, the images it names by `cid:`, and, when `attachments`, its
/// other attachments, in the reader's order.
fn parts(
    msg: &mail_parser::Message<'_>,
    attachments: bool,
) -> (Option<String>, Vec<InlineImage>, Vec<FileAttachment>) {
    let html = crate::message::html_text(msg).map(|html| html.into_owned());
    let named = html
        .as_deref()
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let (_, leaves) = crate::message::sorted_parts(msg);
    let mut inline = Vec::new();
    let mut files = Vec::new();
    for part in leaves {
        let cid = part
            .content_id()
            .map(|cid| {
                cid.trim()
                    .trim_start_matches('<')
                    .trim_end_matches('>')
                    .to_ascii_lowercase()
            })
            .filter(|cid| !cid.is_empty() && named.contains(&format!("cid:{cid}")));
        if cid.is_none() && !attachments {
            continue;
        }
        let content_type = part
            .content_type()
            .map(|ct| {
                format!("{}/{}", ct.ctype(), ct.subtype().unwrap_or("octet-stream"))
                    .to_ascii_lowercase()
            })
            .unwrap_or_else(|| "application/octet-stream".into());
        let bytes = part.decoded().into_owned();
        match cid {
            Some(_) => inline.push(InlineImage {
                // As the part names itself: the HTML may spell it in another case.
                cid: part
                    .content_id()
                    .unwrap_or_default()
                    .trim()
                    .trim_start_matches('<')
                    .trim_end_matches('>')
                    .to_string(),
                content_type,
                bytes,
            }),
            None => files.push(FileAttachment {
                name: crate::message::attachment_file_name(part.attachment_name(), files.len() + 1),
                content_type,
                bytes,
            }),
        }
    }
    (html, inline, files)
}

/// `On Mon, 2 Nov 2026 10:00, Linus Example wrote:`, in English whatever the locale: other clients do not parse it.
fn attribution(read: &Read) -> String {
    let who = read
        .from
        .first()
        .map(|(name, address)| {
            name.clone()
                .filter(|n| !n.trim().is_empty())
                .unwrap_or_else(|| address.clone())
        })
        .unwrap_or_else(|| "someone".into());
    match read.date.as_ref().and_then(english_date) {
        Some(date) => format!("On {date}, {who} wrote:"),
        None => format!("{who} wrote:"),
    }
}

/// `Mon, 2 Nov 2026 10:00` in the original's own time zone.
fn english_date(date: &mail_parser::DateTime) -> Option<String> {
    let day = chrono::NaiveDate::from_ymd_opt(
        i32::from(date.year),
        u32::from(date.month),
        u32::from(date.day),
    )?;
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let month = MONTHS.get(usize::from(date.month).checked_sub(1)?)?;
    Some(format!(
        "{}, {} {month} {} {:02}:{:02}",
        day.weekday(),
        date.day,
        date.year,
        date.hour,
        date.minute
    ))
}

/// The original's text quoted: `> ` before each line, `>` before one already quoted.
fn quote(text: &str) -> String {
    let text = text.replace("\r\n", "\n");
    text.trim_end()
        .split('\n')
        .map(|line| {
            let line = line.trim_end();
            if line.is_empty() {
                ">".to_string()
            } else if line.starts_with('>') {
                format!(">{line}")
            } else {
                format!("> {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The user's text, a blank line, then what follows it. An empty text leaves an empty first line to write on.
fn above(body: &str, rest: &str) -> String {
    format!("{}\n\n{rest}\n", body.trim_end_matches(['\n', '\r']))
}

/// Adds `who` to `list` unless it is one of ours or already there (in `list` or `seen`).
fn add_unique(
    list: &mut Vec<String>,
    seen: &mut Vec<String>,
    identity: &Identity,
    (name, address): &(Option<String>, String),
) {
    let lower = address.to_ascii_lowercase();
    if identity.is_me(address) || seen.contains(&lower) {
        return;
    }
    seen.push(lower);
    list.push(mailbox_text(name.as_deref(), address));
}

/// A reply to `original`, with `body` above the quote.
pub fn reply(
    original: &[u8],
    to: ReplyTo,
    identity: &Identity,
    body: &str,
) -> Result<Draft, ReplyError> {
    let read = read(original, false)?;
    let from = our_address(original, identity);
    let from_ours =
        !read.from.is_empty() && read.from.iter().all(|(_, address)| identity.is_me(address));
    let mut seen = Vec::new();
    let mut to_list = Vec::new();
    let mut cc_list = Vec::new();
    match to {
        ReplyTo::List => {
            let list = read.list_post.clone().ok_or(ReplyError::NoListPost)?;
            to_list.push(list);
        }
        ReplyTo::Sender | ReplyTo::All => {
            // A reply to our own message (one in Sent) goes where it went, as other clients do.
            let sender = if from_ours {
                &read.to
            } else if !read.reply_to.is_empty() {
                &read.reply_to
            } else {
                &read.from
            };
            for who in sender {
                add_unique(&mut to_list, &mut seen, identity, who);
            }
            if to == ReplyTo::All {
                for who in &read.to {
                    add_unique(&mut to_list, &mut seen, identity, who);
                }
                for who in &read.cc {
                    add_unique(&mut cc_list, &mut seen, identity, who);
                }
            }
        }
    }
    if to_list.is_empty() {
        if cc_list.is_empty() {
            return Err(ReplyError::NoRecipients);
        }
        to_list = std::mem::take(&mut cc_list);
    }
    let attribution = attribution(&read);
    let text = above(body, &format!("{attribution}\n{}", quote(&read.text)));
    let html = read
        .html
        .as_ref()
        .and_then(|original| quote_html::reply_html(body, &attribution, original));
    let mut references = read.references.clone();
    references.extend(read.message_id.clone());
    Ok(Draft {
        from_name: identity.display_name_for(&from).map(str::to_string),
        from,
        to: to_list,
        cc: cc_list,
        subject: prefixed(&read.subject, "Re: "),
        text,
        html,
        in_reply_to: read.message_id.clone(),
        references,
        ..Default::default()
    })
}

/// A forward of `original` to `recipients`, with `body` above it.
pub fn forward(
    original: &[u8],
    recipients: Recipients,
    identity: &Identity,
    body: &str,
    options: &ForwardOptions,
) -> Result<Draft, ReplyError> {
    let reattach = options.as_ == ForwardAs::Inline && options.attachments == AttachmentFilter::All;
    let read = read(original, reattach)?;
    let from = our_address(original, identity);
    let body = match &options.note {
        Some(note) if body.trim().is_empty() => note.clone(),
        Some(note) => format!("{note}\n\n{body}"),
        None => body.to_string(),
    };
    let mut draft = Draft {
        from_name: identity.display_name_for(&from).map(str::to_string),
        from,
        to: recipients.to,
        cc: recipients.cc,
        bcc: recipients.bcc,
        subject: prefixed(&read.subject, &options.subject_prefix),
        ..Default::default()
    };
    match options.as_ {
        ForwardAs::Attachment => {
            draft.text = body.trim_end_matches(['\n', '\r']).to_string() + "\n";
            draft.enclosed = Some(original.to_vec());
        }
        ForwardAs::Inline => {
            let mut block: Vec<(&str, String)> = vec![("From", read.from_text.clone())];
            block.extend(read.date_text.clone().map(|date| ("Date", date)));
            block.push(("Subject", read.subject.clone()));
            block.push(("To", read.to_text.clone()));
            if !read.cc_text.is_empty() {
                block.push(("Cc", read.cc_text.clone()));
            }
            let mut header = String::from("---------- Forwarded message ----------");
            for (name, value) in &block {
                header.push_str(&format!("\n{name}: {value}"));
            }
            draft.text = above(
                &body,
                &format!("{header}\n\n{}", read.text.replace("\r\n", "\n").trim_end()),
            );
            draft.html = read
                .html
                .as_ref()
                .and_then(|original| quote_html::forward_html(&body, &block, original));
            if options.attachments == AttachmentFilter::All {
                draft.attachments = read.attachments.clone();
            }
            draft.references = read.references.clone();
            draft.references.extend(read.message_id.clone());
        }
    }
    Ok(draft)
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
        account.aliases = vec!["alias@example.org".into(), "*@shop.example.com".into()];
        account.display_name = Some("Pieter".into());
        account.identity().unwrap()
    }

    fn original(headers: &str, body: &str) -> Vec<u8> {
        format!(
            "From: Linus Example <linus@x.org>\r\nTo: me@example.com\r\nSubject: Plans\r\nDate: Mon, 2 Nov 2026 10:00:00 +0100\r\nMessage-ID: <m1@x.org>\r\n{headers}\r\n{body}"
        )
        .into_bytes()
    }

    fn addresses(list: &[String]) -> Vec<String> {
        list.iter()
            .map(|m| super::super::build::bare_address(m))
            .collect()
    }

    #[test]
    fn reply_goes_to_reply_to_else_from() {
        let draft = reply(&original("", "hi\r\n"), ReplyTo::Sender, &identity(), "ok").unwrap();
        assert_eq!(draft.to, ["Linus Example <linus@x.org>"]);
        assert!(draft.cc.is_empty());
        let with = original("Reply-To: list@x.org\r\n", "hi\r\n");
        let draft = reply(&with, ReplyTo::Sender, &identity(), "ok").unwrap();
        assert_eq!(addresses(&draft.to), ["list@x.org"]);
        assert_eq!(draft.from, "me@example.com");
        assert_eq!(draft.from_name.as_deref(), Some("Pieter"));
    }

    #[test]
    fn reply_to_our_own_message_goes_to_its_to() {
        let sent = "From: Me <me@example.com>\r\nTo: Linus <linus@x.org>, other@x.org\r\nSubject: s\r\nMessage-ID: <s@example.com>\r\n\r\nbody\r\n";
        let draft = reply(sent.as_bytes(), ReplyTo::Sender, &identity(), "again").unwrap();
        assert_eq!(addresses(&draft.to), ["linus@x.org", "other@x.org"]);
        assert_eq!(draft.from, "me@example.com");
    }

    #[test]
    fn reply_all_keeps_to_in_to_and_cc_in_cc() {
        let raw = original("Cc: c1@x.org, C2 <c2@x.org>\r\n", "hi").to_vec();
        let raw = String::from_utf8(raw)
            .unwrap()
            .replace("To: me@example.com", "To: me@example.com, t1@x.org");
        let draft = reply(raw.as_bytes(), ReplyTo::All, &identity(), "").unwrap();
        assert_eq!(addresses(&draft.to), ["linus@x.org", "t1@x.org"]);
        assert_eq!(addresses(&draft.cc), ["c1@x.org", "c2@x.org"]);
    }

    #[test]
    fn reply_all_drops_every_address_of_ours_and_duplicates() {
        let raw = String::from_utf8(original(
            "Cc: Alias@Example.org, third@x.org, LINUS@x.org\r\n",
            "hi",
        ))
        .unwrap();
        let draft = reply(raw.as_bytes(), ReplyTo::All, &identity(), "").unwrap();
        assert_eq!(addresses(&draft.to), ["linus@x.org"]);
        assert_eq!(addresses(&draft.cc), ["third@x.org"]);
        // Sent to the alias: the reply is from it.
        assert_eq!(draft.from, "me@example.com");
        let to_alias = raw.replace("To: me@example.com", "To: alias@example.org");
        assert_eq!(
            reply(to_alias.as_bytes(), ReplyTo::All, &identity(), "")
                .unwrap()
                .from,
            "alias@example.org"
        );
    }

    #[test]
    fn reply_list_uses_list_post_and_is_refused_without_it() {
        let raw = original("List-Post: <mailto:dev@lists.x?subject=hi>\r\n", "hi");
        let draft = reply(&raw, ReplyTo::List, &identity(), "").unwrap();
        assert_eq!(draft.to, ["dev@lists.x"]);
        assert!(draft.cc.is_empty());
        assert_eq!(
            reply(&original("", "hi"), ReplyTo::List, &identity(), ""),
            Err(ReplyError::NoListPost)
        );
    }

    #[test]
    fn prefixed_does_not_stack() {
        for kept in ["Re: x", "AW: x", "antw: x", "FW: x", "SV: x", "Doorst: x"] {
            assert_eq!(prefixed(kept, "Re: "), kept);
        }
        assert_eq!(prefixed("x", "Re: "), "Re: x");
        for kept in ["FW: x", "Doorst: x", "WG: x", "fwd: x", "TR: x"] {
            assert_eq!(prefixed(kept, "Fwd: "), kept);
        }
        assert_eq!(prefixed("Re: x", "Fwd: "), "Fwd: Re: x");
        assert_eq!(prefixed("x", ""), "x");
        assert_eq!(prefixed("[ext] x", "[ext] "), "[ext] x");
        assert_eq!(prefixed("x", "[ext] "), "[ext] x");
    }

    #[test]
    fn reply_threads() {
        let raw = original("References: <a@x.org> <b@x.org>\r\n", "hi");
        let draft = reply(&raw, ReplyTo::Sender, &identity(), "").unwrap();
        assert_eq!(draft.in_reply_to.as_deref(), Some("<m1@x.org>"));
        assert_eq!(draft.references, ["<a@x.org>", "<b@x.org>", "<m1@x.org>"]);
        assert_eq!(draft.subject, "Re: Plans");
    }

    #[test]
    fn quote_prefixes_each_line_under_the_attribution() {
        let draft = reply(
            &original("", "line\r\n\r\n> old\r\n"),
            ReplyTo::Sender,
            &identity(),
            "Sure.\n",
        )
        .unwrap();
        assert_eq!(
            draft.text,
            "Sure.\n\nOn Mon, 2 Nov 2026 10:00, Linus Example wrote:\n> line\n>\n>> old\n"
        );
        assert!(draft.html.is_none());
        let empty = reply(&original("", "line"), ReplyTo::Sender, &identity(), "").unwrap();
        assert!(empty.text.starts_with("\n\nOn Mon"), "{:?}", empty.text);
    }

    #[test]
    fn quote_of_an_html_only_original_uses_its_text() {
        let raw = original(
            "MIME-Version: 1.0\r\nContent-Type: text/html; charset=utf-8\r\n",
            "<p>Hello <b>there</b></p>",
        );
        let draft = reply(&raw, ReplyTo::Sender, &identity(), "").unwrap();
        assert!(draft.text.contains("> Hello there"), "{:?}", draft.text);
        let html = draft.html.unwrap();
        assert!(
            html.html.contains(&format!(
                "{}<p>Hello <b>there</b></p>",
                quote_html::QUOTE_OPEN
            )),
            "{}",
            html.html
        );
    }

    #[test]
    fn a_plain_original_gets_no_html() {
        let draft = reply(&original("", "plain"), ReplyTo::Sender, &identity(), "x").unwrap();
        assert!(draft.html.is_none());
        let fwd = forward(
            &original("", "plain"),
            Recipients {
                to: vec!["a@b.org".into()],
                ..Default::default()
            },
            &identity(),
            "x",
            &ForwardOptions::default(),
        )
        .unwrap();
        assert!(fwd.html.is_none());
    }

    #[test]
    fn from_is_the_address_the_original_was_sent_to() {
        let raw = String::from_utf8(original("Delivered-To: alias@example.org\r\n", "hi"))
            .unwrap()
            .replace("To: me@example.com", "To: list@lists.x");
        assert_eq!(
            reply(raw.as_bytes(), ReplyTo::Sender, &identity(), "")
                .unwrap()
                .from,
            "alias@example.org"
        );
        let shop = raw.replace(
            "Delivered-To: alias@example.org",
            "Delivered-To: orders@shop.example.com",
        );
        let draft = reply(shop.as_bytes(), ReplyTo::Sender, &identity(), "").unwrap();
        assert_eq!(draft.from, "orders@shop.example.com");
        assert_eq!(draft.from_name.as_deref(), Some("Pieter"));
        let nobody = raw.replace("Delivered-To: alias@example.org\r\n", "");
        assert_eq!(
            reply(nobody.as_bytes(), ReplyTo::Sender, &identity(), "")
                .unwrap()
                .from,
            "me@example.com"
        );
    }

    const MIXED: &str =
        "MIME-Version: 1.0\r\nCc: c@x.org\r\nContent-Type: multipart/mixed; boundary=b\r\n";
    const MIXED_BODY: &str = "--b\r\nContent-Type: text/plain\r\n\r\nthe text\r\n--b\r\nContent-Type: application/pdf\r\nContent-Disposition: attachment; filename=a.pdf\r\nContent-Transfer-Encoding: base64\r\n\r\nJVBERg==\r\n--b--\r\n";

    #[test]
    fn inline_forward_has_the_header_block_and_reattaches() {
        let raw = original(MIXED, MIXED_BODY);
        let to = Recipients {
            to: vec!["boss@y.org".into()],
            bcc: vec!["me2@y.org".into()],
            ..Default::default()
        };
        let draft = forward(&raw, to, &identity(), "FYI", &ForwardOptions::default()).unwrap();
        assert_eq!(draft.subject, "Fwd: Plans");
        assert_eq!(draft.to, ["boss@y.org"]);
        assert_eq!(draft.bcc, ["me2@y.org"]);
        assert!(draft.text.starts_with("FYI\n\n---------- Forwarded message ----------\nFrom: Linus Example <linus@x.org>\nDate: Mon, 2 Nov 2026 10:00:00 +0100\nSubject: Plans\nTo: me@example.com\nCc: c@x.org\n\nthe text"), "{:?}", draft.text);
        assert_eq!(draft.attachments.len(), 1);
        assert_eq!(draft.attachments[0].name, "a.pdf");
        assert_eq!(draft.attachments[0].content_type, "application/pdf");
        assert_eq!(draft.attachments[0].bytes, b"%PDF");
        assert_eq!(draft.in_reply_to, None);
        assert_eq!(draft.references, ["<m1@x.org>"]);
        let none = ForwardOptions {
            attachments: AttachmentFilter::None,
            ..Default::default()
        };
        assert!(
            forward(&raw, Recipients::default(), &identity(), "", &none)
                .unwrap()
                .attachments
                .is_empty()
        );
    }

    #[test]
    fn attachment_forward_encloses_the_raw_original_unchanged() {
        let raw = original(MIXED, MIXED_BODY);
        let options = ForwardOptions {
            as_: ForwardAs::Attachment,
            ..Default::default()
        };
        let draft = forward(
            &raw,
            Recipients::default(),
            &identity(),
            "see attached",
            &options,
        )
        .unwrap();
        assert_eq!(draft.enclosed.as_deref(), Some(raw.as_slice()));
        assert_eq!(draft.text, "see attached\n");
        assert!(draft.attachments.is_empty());
    }

    #[test]
    fn attachment_forward_has_no_in_reply_to_or_references() {
        let raw = original("References: <a@x.org>\r\n", "hi");
        let options = ForwardOptions {
            as_: ForwardAs::Attachment,
            ..Default::default()
        };
        let draft = forward(&raw, Recipients::default(), &identity(), "", &options).unwrap();
        assert_eq!(draft.in_reply_to, None);
        assert!(draft.references.is_empty());
    }

    #[test]
    fn an_html_forward_keeps_its_cid_images_out_of_the_attachments() {
        let body = "--b\r\nContent-Type: multipart/related; boundary=r\r\n\r\n--r\r\nContent-Type: text/html\r\n\r\n<p>hi<img src=\"cid:logo@x\"></p>\r\n--r\r\nContent-Type: image/png\r\nContent-ID: <logo@x>\r\nContent-Transfer-Encoding: base64\r\n\r\niVBORw==\r\n--r--\r\n--b\r\nContent-Type: application/pdf\r\nContent-Disposition: attachment; filename=a.pdf\r\n\r\nx\r\n--b--\r\n";
        let raw = original(MIXED, body);
        let draft = forward(
            &raw,
            Recipients::default(),
            &identity(),
            "",
            &ForwardOptions::default(),
        )
        .unwrap();
        let html = draft.html.unwrap();
        assert_eq!(html.inline.len(), 1);
        assert_eq!(html.inline[0].cid, "logo@x");
        assert_eq!(html.inline[0].content_type, "image/png");
        assert_eq!(draft.attachments.len(), 1);
        assert_eq!(OriginalFlag::Forwarded.flag(), "$Forwarded");
        assert_eq!(OriginalFlag::Answered.flag(), "\\Answered");
    }
}
