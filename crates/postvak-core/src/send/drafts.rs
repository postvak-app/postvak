//! Drafts on the server: which folder holds them, building one as its message would be sent, and reading one back,
//! Postvak's own and other clients'.

use anyhow::anyhow;
use mail_parser::{MessageParser, PartKind};

use super::build::{self, mailbox_text};
use super::compose::{self, SendKind, SendRequest};
use super::reply::{self, ForwardAs, ReplyTo};
use crate::config::{self, AccountConfig, Config, PasswordSource};
use crate::daemon::client::LazyClient;
use crate::mail_ops::{self, MailError, MailOps};
use crate::paths::Paths;
use crate::store::{Folder, Store};
use crate::sync::LostConnection;

/// The header naming a Postvak draft's kind: `<kind>; original="<message-id>"; parts="0,2"`.
pub const HEADER: &str = "X-Postvak-Draft";

const HTML_ONLY: &str = "This draft had only HTML; it opens as text.";

/// What a draft answers, if anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DraftKind {
    New,
    Reply(ReplyTo),
    Forward(ForwardAs),
}

impl DraftKind {
    const WORDS: [(&str, DraftKind); 6] = [
        ("forward", DraftKind::Forward(ForwardAs::Inline)),
        (
            "forward-attachment",
            DraftKind::Forward(ForwardAs::Attachment),
        ),
        ("new", DraftKind::New),
        ("reply", DraftKind::Reply(ReplyTo::Sender)),
        ("reply-all", DraftKind::Reply(ReplyTo::All)),
        ("reply-list", DraftKind::Reply(ReplyTo::List)),
    ];

    fn word(self) -> &'static str {
        Self::WORDS
            .iter()
            .find(|(_, kind)| *kind == self)
            .map_or("new", |(word, _)| word)
    }

    fn from_word(word: &str) -> Option<DraftKind> {
        Self::WORDS
            .iter()
            .find(|(known, _)| *known == word)
            .map(|(_, kind)| *kind)
    }

    fn of(kind: &SendKind) -> DraftKind {
        match kind {
            SendKind::New { .. } => DraftKind::New,
            SendKind::Reply { to, .. } => DraftKind::Reply(*to),
            SendKind::Forward { as_, .. } => DraftKind::Forward(*as_),
        }
    }

    /// The kind as [`compose::draft_for`] takes it; folder, uid and recipients play no part in the text it builds.
    fn send_kind(self) -> SendKind {
        match self {
            DraftKind::New => SendKind::New {
                to: Vec::new(),
                cc: Vec::new(),
                bcc: Vec::new(),
                subject: String::new(),
            },
            DraftKind::Reply(to) => SendKind::Reply {
                folder: String::new(),
                uid: 0,
                to,
            },
            DraftKind::Forward(as_) => SendKind::Forward {
                folder: String::new(),
                uid: 0,
                as_,
                to: Vec::new(),
                cc: Vec::new(),
                bcc: Vec::new(),
            },
        }
    }
}

/// A draft read back into the composer's fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenedDraft {
    pub kind: DraftKind,
    /// The raw message a Postvak reply or forward answers, its quote rebuilt when it is sent; None for a draft sent as
    /// written.
    pub original: Option<Vec<u8>>,
    /// The message a draft sent as written answers, by its In-Reply-To, when the store has it: only drawn below it.
    pub shown_original: Option<Vec<u8>>,
    /// The bare address.
    pub from: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub subject: String,
    /// The user's text: for a Postvak reply or forward, without the quote or forwarded block below it.
    pub text: String,
    /// The draft's own attachments as (file name, contents): not those a forward takes from its original.
    pub files: Vec<(String, Vec<u8>)>,
    /// An inline forward's attachments kept, by index into [`reply::forward_attachments`]; None for all.
    pub forward_parts: Option<Vec<usize>>,
    /// With its angle brackets.
    pub in_reply_to: Option<String>,
    /// With their angle brackets, oldest first.
    pub references: Vec<String>,
    /// A line for the user about how the draft opened.
    pub note: Option<&'static str>,
    /// The draft's own Message-ID, without angle brackets.
    pub message_id: String,
}

/// The message `request` describes, built as `compose` would send it, with a new Message-ID and [`HEADER`]; without the
/// checks a draft may fail (no recipient yet, too large), and not queued.
pub fn build(
    config: &Config,
    paths: &Paths,
    client: &LazyClient,
    request: &SendRequest,
) -> anyhow::Result<Vec<u8>> {
    let account = config::single(&config.accounts, request.account.as_deref())?;
    let identity = account.identity()?;
    let (mut draft, original) = compose::message_draft(
        paths,
        client,
        account,
        &identity,
        &config.sending,
        request,
        |_| {
            Err(anyhow!(
                "a draft takes its text as given, not from the editor"
            ))
        },
    )?;
    draft.postvak_draft = Some(marker_value(
        DraftKind::of(&request.kind),
        original.as_ref().map(|(raw, _)| raw.as_slice()),
        request.forward_parts.as_deref(),
    ));
    Ok(build::build(&draft, crate::time::now())?)
}

/// What [`HEADER`] says.
#[derive(Debug, PartialEq, Eq)]
struct Marker {
    kind: DraftKind,
    /// Without angle brackets.
    original: Option<String>,
    parts: Option<Vec<usize>>,
}

fn marker_value(kind: DraftKind, original: Option<&[u8]>, parts: Option<&[usize]>) -> String {
    let mut value = kind.word().to_string();
    let Some(raw) = original.filter(|_| kind != DraftKind::New) else {
        return value;
    };
    // A Message-ID that would break the quoting is left out, so the draft opens as another client's.
    if let Some(id) = crate::message::parse_headers(raw).message_id.filter(|id| {
        id.chars()
            .all(|c| c.is_ascii_graphic() && !matches!(c, '"' | ';'))
    }) {
        value.push_str(&format!("; original=\"<{id}>\""));
    }
    if let (DraftKind::Forward(ForwardAs::Inline), Some(parts)) = (kind, parts) {
        let total = reply::forward_attachments(raw).len();
        let kept: Vec<String> = (0..total)
            .filter(|index| parts.contains(index))
            .map(|index| index.to_string())
            .collect();
        if kept.len() < total {
            value.push_str(&format!("; parts=\"{}\"", kept.join(",")));
        }
    }
    value
}

/// [`HEADER`]'s value read leniently: an unknown kind or a malformed parameter gives None.
fn marker(value: &str) -> Option<Marker> {
    let mut pieces = value.split(';');
    let kind = DraftKind::from_word(pieces.next()?.trim())?;
    let mut marker = Marker {
        kind,
        original: None,
        parts: None,
    };
    for piece in pieces {
        let (key, quoted) = piece.split_once('=')?;
        let value = quoted.trim().strip_prefix('"')?.strip_suffix('"')?;
        match key.trim() {
            "original" => {
                let id = value.trim().trim_start_matches('<').trim_end_matches('>');
                marker.original = Some(id.to_string()).filter(|id| !id.is_empty());
            }
            "parts" if value.trim().is_empty() => marker.parts = Some(Vec::new()),
            "parts" => {
                let parts = value.split(',').map(|index| index.trim().parse().ok());
                marker.parts = Some(parts.collect::<Option<_>>()?);
            }
            _ => {}
        }
    }
    Some(marker)
}

/// The text of a format=flowed body as written: soft line breaks joined, space-stuffing undone (RFC 3676).
fn unflowed(text: &str, delsp: bool) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out = String::with_capacity(text.len());
    for (index, line) in lines.iter().enumerate() {
        let line = line.strip_prefix(' ').unwrap_or(line);
        let last = index + 1 == lines.len();
        let soft = !last && line.ends_with(' ') && line != "-- ";
        match line.strip_suffix(' ') {
            Some(joined) if soft && delsp => out.push_str(joined),
            _ => out.push_str(line),
        }
        if !soft && !last {
            out.push('\n');
        }
    }
    out
}

/// The first body text of `msg`, line ends as `\n` but format=flowed left as sent: the text, whether it is flowed and
/// with `delsp=yes`, and whether it came from HTML.
fn body_of(raw: &[u8], msg: &mail_parser::Message<'_>) -> (String, Option<bool>, bool) {
    let part = msg.text_body().next();
    let html = part.is_some_and(|part| matches!(part.kind(), PartKind::Html));
    let attribute = |name: &str| {
        part.and_then(|part| part.content_type())
            .and_then(|ct| ct.attribute(name))
            .map(str::to_ascii_lowercase)
    };
    let flowed = (!html && attribute("format").as_deref() == Some("flowed"))
        .then(|| attribute("delsp").as_deref() == Some("yes"));
    let text = crate::message::body_text(raw).replace("\r\n", "\n");
    (text, flowed, html)
}

/// A file as (name, contents).
type NamedFile = (String, Vec<u8>);

/// A Postvak draft's own text and files: what is left once the quote or forward block Postvak builds for `original`
/// is taken off; None when the draft no longer ends with it, as after an edit in another client.
fn own_parts(
    marker: &Marker,
    original: &[u8],
    from: &str,
    sent: &str,
    flowed: Option<bool>,
    files: &[(String, String, Vec<u8>)],
) -> Option<(String, Vec<NamedFile>)> {
    // The text below the user's does not depend on whose identity builds it.
    let identity = AccountConfig::new("draft", "", from, PasswordSource::Keyring { keyring: true })
        .identity()
        .ok()?;
    let below = compose::draft_for(
        &identity,
        &marker.kind.send_kind(),
        original,
        "",
        marker.parts.as_deref(),
    )
    .ok()?
    .text;
    let below = if flowed.is_some() {
        build::flowed(&below)
    } else {
        below
    };
    let user = sent
        .trim_end_matches('\n')
        .strip_suffix(below.trim_end_matches('\n'))?;
    let user = match flowed {
        Some(delsp) => unflowed(user, delsp),
        None => user.to_string(),
    };
    let own = match marker.kind {
        DraftKind::Forward(ForwardAs::Inline) => {
            let forwarded: Vec<_> = reply::attached_files(original)
                .into_iter()
                .enumerate()
                .filter(|(index, _)| {
                    marker
                        .parts
                        .as_ref()
                        .is_none_or(|parts| parts.contains(index))
                })
                .map(|(_, file)| file)
                .collect();
            let (head, rest) = files.split_at_checked(forwarded.len())?;
            let unchanged = head
                .iter()
                .zip(&forwarded)
                .all(|((name, _, bytes), file)| *name == file.name && *bytes == file.bytes);
            if !unchanged {
                return None;
            }
            rest
        }
        DraftKind::Forward(ForwardAs::Attachment) => match files.split_last()? {
            ((_, content_type, bytes), rest)
                if content_type == "message/rfc822" && bytes.as_slice() == original =>
            {
                rest
            }
            _ => return None,
        },
        DraftKind::New | DraftKind::Reply(_) => files,
    };
    let own = own
        .iter()
        .map(|(name, _, bytes)| (name.clone(), bytes.clone()))
        .collect();
    Some((user, own))
}

/// The fields of the draft `raw`; `find_original` looks a Message-ID (without angle brackets) up in the account's store.
/// A draft Postvak wrote opens as its kind, with the user's text apart from the original; any other, or one whose
/// original is gone or whose quote was edited, opens whole as new mail keeping its threading, to be sent as written.
pub fn read(
    raw: &[u8],
    find_original: impl Fn(&str) -> Option<Vec<u8>>,
) -> anyhow::Result<OpenedDraft> {
    let msg = MessageParser::default()
        .parse(raw)
        .ok_or_else(|| anyhow!("the draft could not be read"))?;
    let headers = crate::message::parse_headers(raw);
    let mailboxes = |list: Option<mail_parser::AddressList<'_>>| -> Vec<String> {
        list.map(|list| {
            list.mailboxes()
                .filter_map(|m| Some(mailbox_text(m.name(), m.address()?)))
                .collect()
        })
        .unwrap_or_default()
    };
    let from = msg
        .from()
        .and_then(|list| {
            list.mailboxes()
                .find_map(|m| m.address().map(str::to_string))
        })
        .unwrap_or_default();
    let files: Vec<(String, String, Vec<u8>)> = reply::attached_files(raw)
        .into_iter()
        .map(|file| (file.name, file.content_type, file.bytes))
        .collect();
    let (text, flowed, html) = body_of(raw, &msg);
    let bracketed = |id: &String| format!("<{id}>");
    let mut opened = OpenedDraft {
        kind: DraftKind::New,
        original: None,
        shown_original: None,
        to: mailboxes(msg.to()),
        cc: mailboxes(msg.cc()),
        bcc: mailboxes(msg.bcc()),
        subject: msg.subject().unwrap_or_default().to_string(),
        text: String::new(),
        files: Vec::new(),
        forward_parts: None,
        in_reply_to: headers.in_reply_to.as_ref().map(bracketed),
        references: headers.references.iter().map(bracketed).collect(),
        note: None,
        message_id: headers.message_id.clone().unwrap_or_default(),
        from,
    };
    let marker = crate::message::header_text(raw, HEADER).and_then(|value| marker(&value));
    if let Some(marker) = marker.filter(|_| !html) {
        let original = match marker.kind {
            DraftKind::New => Some(Vec::new()),
            _ => marker.original.as_deref().and_then(&find_original),
        };
        if let Some(original) = original
            && let Some((text, files)) =
                own_parts(&marker, &original, &opened.from, &text, flowed, &files)
        {
            opened.kind = marker.kind;
            match marker.kind {
                // New mail Postvak wrote may still answer a message, as a foreign reply saved again does.
                DraftKind::New => {
                    opened.shown_original = headers.in_reply_to.as_deref().and_then(&find_original)
                }
                _ => opened.original = Some(original),
            }
            opened.text = text;
            opened.files = files;
            opened.forward_parts = marker.parts;
            return Ok(opened);
        }
    }
    opened.shown_original = headers.in_reply_to.as_deref().and_then(&find_original);
    opened.text = match flowed {
        Some(delsp) => unflowed(&text, delsp),
        None => text,
    };
    opened.note = html.then_some(HTML_ONLY);
    opened.files = files
        .into_iter()
        .map(|(name, _, bytes)| (name, bytes))
        .collect();
    Ok(opened)
}

/// The folder drafts are kept in: the one marked `\Drafts`, else one named `Drafts`.
pub fn drafts_folder(folders: &[Folder]) -> Option<String> {
    folders
        .iter()
        .find(|folder| folder.special_use.as_deref() == Some("Drafts"))
        .or_else(|| {
            folders
                .iter()
                .find(|folder| crate::folder_name::same(&folder.name, "Drafts"))
        })
        .map(|folder| folder.name.clone())
}

/// Why a draft was not saved or deleted.
#[derive(Debug, thiserror::Error)]
pub enum SaveDraftError {
    #[error("This account has no Drafts folder")]
    NoDraftsFolder,
    #[error(transparent)]
    Mail(#[from] MailError),
    #[error(transparent)]
    Store(#[from] crate::store::StoreError),
}

/// What a save did: the new copy's Message-ID, and the replaced copies it could not delete, by Message-ID; each
/// `<id>`, as `replaces` takes them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SavedDraft {
    pub message_id: Option<String>,
    pub left: Vec<String>,
}

/// APPENDs `raw` to Drafts, then deletes every copy whose Message-ID is in `replaces`; without `raw`, only deletes.
/// The old copies go only once the new one is on the server, and one already gone is not an error. Once the APPEND
/// succeeded the save stands, whatever the deletes do: a copy left behind is in `left`, for the next save to replace.
pub fn save(
    ops: &mut dyn MailOps,
    store: &Store,
    account: &str,
    raw: Option<&[u8]>,
    replaces: &[String],
) -> Result<SavedDraft, SaveDraftError> {
    let folder = drafts_folder(&store.folders()?).ok_or(SaveDraftError::NoDraftsFolder)?;
    let replaces: Vec<String> = replaces.iter().filter_map(|id| bracketed(id)).collect();
    let Some(raw) = raw else {
        let (left, lost) = delete_each(ops, account, &folder, &replaces);
        return match lost {
            Some(e) => Err(e.into()),
            None => Ok(SavedDraft {
                message_id: None,
                left,
            }),
        };
    };
    ops.append(&folder, raw, &["\\Draft", "\\Seen"])?;
    let message_id = crate::message::parse_headers(raw)
        .message_id
        .and_then(|id| bracketed(&id));
    log::info!(
        "{account}: saved draft {} in {folder}",
        message_id.as_deref().unwrap_or("without a Message-ID")
    );
    let (left, _) = delete_each(ops, account, &folder, &replaces);
    Ok(SavedDraft { message_id, left })
}

/// Deletes each copy in `ids` from `folder`, going on past a failed one: the copies left, and the error that lost the
/// connection, after which none more was tried.
fn delete_each(
    ops: &mut dyn MailOps,
    account: &str,
    folder: &str,
    ids: &[String],
) -> (Vec<String>, Option<MailError>) {
    let mut left = Vec::new();
    for (index, id) in ids.iter().enumerate() {
        if let Err(e) = delete_drafts(ops, account, folder, std::slice::from_ref(id)) {
            log::warn!("{account}: draft {id} was not deleted from {folder}: {e}");
            left.push(id.clone());
            if e.is_lost_connection() {
                left.extend(ids[index + 1..].iter().cloned());
                return (left, Some(e));
            }
        }
    }
    (left, None)
}

/// `raw` with its Message-ID header, folded lines included, replaced by `id`, or with `id` added first when it has none.
pub fn with_message_id(raw: &[u8], id: &str) -> Vec<u8> {
    let newline: &[u8] = if raw.windows(2).any(|pair| pair == b"\r\n") {
        b"\r\n"
    } else {
        b"\n"
    };
    let header = [format!("Message-ID: {id}").as_bytes(), newline].concat();
    let mut out = Vec::with_capacity(raw.len() + header.len());
    let (mut replaced, mut skipping) = (false, false);
    let mut rest = raw;
    while let Some(end) = rest.iter().position(|&b| b == b'\n') {
        let (line, after) = rest.split_at(end + 1);
        if line.iter().all(|b| matches!(b, b'\r' | b'\n')) {
            break;
        }
        let folded = matches!(line.first(), Some(b' ' | b'\t'));
        if !(folded && skipping) {
            skipping =
                !folded && line.len() >= 11 && line[..11].eq_ignore_ascii_case(b"message-id:");
            if !skipping {
                out.extend_from_slice(line);
            } else if !replaced {
                out.extend_from_slice(&header);
                replaced = true;
            }
        }
        rest = after;
    }
    if !replaced {
        out.splice(0..0, header);
    }
    out.extend_from_slice(rest);
    out
}

/// `<id>`, or None for a blank one. `SEARCH HEADER` matches a substring, so only the brackets keep `<d1@x>` from
/// also matching `<xd1@x>`.
pub fn bracketed(id: &str) -> Option<String> {
    let id = unbracketed(id);
    (!id.is_empty()).then(|| format!("<{id}>"))
}

/// `id` without its angle brackets, as the store keeps Message-IDs.
pub fn unbracketed(id: &str) -> &str {
    id.trim().trim_start_matches('<').trim_end_matches('>')
}

/// Expunges every message in `folder` whose Message-ID is in `ids`, each `<id>`.
fn delete_drafts(
    ops: &mut dyn MailOps,
    account: &str,
    folder: &str,
    ids: &[String],
) -> Result<(), MailError> {
    if ids.is_empty() {
        return Ok(());
    }
    ops.select(folder)?;
    for id in ids {
        let uids = ops.search_header("Message-ID", id)?;
        if !uids.is_empty() {
            mail_ops::delete_messages(ops, &uids)?;
            log::info!("{account}: deleted draft {id} from {folder}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AccountConfig, PasswordSource};
    use crate::send::compose::Body;
    use crate::send::reply::tests::{MIXED, TWO, original};

    struct Fixture {
        _dir: tempfile::TempDir,
        paths: Paths,
        config: Config,
        client: LazyClient,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let mut account = AccountConfig::new(
            "work",
            "imap.example.com",
            "me@example.com",
            PasswordSource::Keyring { keyring: true },
        );
        account.display_name = Some("Pieter".into());
        let config = Config {
            accounts: vec![account],
            ..Default::default()
        };
        let client = LazyClient::new(&paths);
        Fixture {
            _dir: dir,
            paths,
            config,
            client,
        }
    }

    fn request(kind: SendKind, body: &str, raw: Option<&[u8]>) -> SendRequest {
        SendRequest {
            account: Some("work".into()),
            kind,
            body: Body::Given(body.into()),
            from: None,
            attachments: Vec::new(),
            now: false,
            save_sent: None,
            original: raw.map(<[u8]>::to_vec),
            headers: None,
            forward_parts: None,
            draft: Vec::new(),
            signature: false,
        }
    }

    fn reply_kind(to: ReplyTo) -> SendKind {
        SendKind::Reply {
            folder: "INBOX".into(),
            uid: 7,
            to,
        }
    }

    fn forward_kind(as_: ForwardAs) -> SendKind {
        SendKind::Forward {
            folder: "INBOX".into(),
            uid: 7,
            as_,
            to: vec!["boss@y.org".into()],
            cc: vec![],
            bcc: vec![],
        }
    }

    fn built(f: &Fixture, request: &SendRequest) -> Vec<u8> {
        build(&f.config, &f.paths, &f.client, request).unwrap()
    }

    /// Looks up only `raw`, by its Message-ID.
    fn finds(raw: &[u8]) -> impl Fn(&str) -> Option<Vec<u8>> + '_ {
        move |id| {
            (crate::message::parse_headers(raw).message_id.as_deref() == Some(id))
                .then(|| raw.to_vec())
        }
    }

    fn finds_nothing(_: &str) -> Option<Vec<u8>> {
        None
    }

    #[test]
    fn a_reply_draft_reads_back_its_text_and_kind() {
        let f = fixture();
        let raw = original(
            "Cc: c@x.org\r\nList-Post: <mailto:dev@lists.x>\r\n",
            "Shall we?\r\n",
        );
        for to in [ReplyTo::Sender, ReplyTo::All, ReplyTo::List] {
            let draft = built(
                &f,
                &request(
                    reply_kind(to),
                    "Sure.\n\nSee you then, and a line long enough to be wrapped at seventy-two characters.\n",
                    Some(&raw),
                ),
            );
            let marker = crate::message::header_text(&draft, HEADER).unwrap();
            assert!(marker.contains("original=\"<m1@x.org>\""), "{marker}");
            let opened = read(&draft, finds(&raw)).unwrap();
            assert_eq!(opened.kind, DraftKind::Reply(to));
            assert_eq!(
                opened.text,
                "Sure.\n\nSee you then, and a line long enough to be wrapped at seventy-two characters."
            );
            assert_eq!(opened.original.as_deref(), Some(raw.as_slice()));
            assert_eq!(opened.subject, "Re: Plans");
            assert_eq!(opened.from, "me@example.com");
            assert_eq!(opened.in_reply_to.as_deref(), Some("<m1@x.org>"));
            assert_eq!(opened.references, ["<m1@x.org>"]);
            assert!(opened.files.is_empty());
            assert_eq!(opened.note, None);
            assert_eq!(opened.forward_parts, None);
        }
        let all = read(
            &built(&f, &request(reply_kind(ReplyTo::All), "x", Some(&raw))),
            finds(&raw),
        )
        .unwrap();
        assert_eq!(all.to, ["Linus Example <linus@x.org>"]);
        assert_eq!(all.cc, ["c@x.org"]);
    }

    #[test]
    fn a_reply_to_an_html_original_reads_back_without_its_images() {
        let f = fixture();
        let body = "--b\r\nContent-Type: multipart/related; boundary=r\r\n\r\n--r\r\nContent-Type: text/html\r\n\r\n<p>hi<img src=\"cid:logo@x\"></p>\r\n--r\r\nContent-Type: image/png\r\nContent-ID: <logo@x>\r\nContent-Transfer-Encoding: base64\r\n\r\niVBORw==\r\n--r--\r\n--b--\r\n";
        let raw = original(MIXED, body);
        let draft = built(
            &f,
            &request(reply_kind(ReplyTo::Sender), "Zoë says hi.\n", Some(&raw)),
        );
        assert!(String::from_utf8_lossy(&draft).contains("multipart/alternative"));
        let opened = read(&draft, finds(&raw)).unwrap();
        assert_eq!(opened.kind, DraftKind::Reply(ReplyTo::Sender));
        assert_eq!(opened.text, "Zoë says hi.");
        assert!(opened.files.is_empty(), "{:?}", opened.files);
    }

    #[test]
    fn a_forward_draft_reads_back_the_parts_kept() {
        let f = fixture();
        let raw = original(MIXED, TWO);
        let file = f.paths.state_dir.join("notes.txt");
        std::fs::create_dir_all(&f.paths.state_dir).unwrap();
        std::fs::write(&file, b"my notes").unwrap();
        let mut kept_one = request(forward_kind(ForwardAs::Inline), "FYI\n", Some(&raw));
        kept_one.forward_parts = Some(vec![1]);
        kept_one.attachments = vec![file];
        let draft = built(&f, &kept_one);
        let marker = crate::message::header_text(&draft, HEADER).unwrap();
        assert_eq!(marker, "forward; original=\"<m1@x.org>\"; parts=\"1\"");
        let opened = read(&draft, finds(&raw)).unwrap();
        assert_eq!(opened.kind, DraftKind::Forward(ForwardAs::Inline));
        assert_eq!(opened.text, "FYI");
        assert_eq!(opened.forward_parts, Some(vec![1]));
        assert_eq!(
            opened.files,
            [("notes.txt".to_string(), b"my notes".to_vec())]
        );
        assert_eq!(opened.to, ["boss@y.org"]);
        assert_eq!(opened.subject, "Fwd: Plans");

        let mut kept_all = request(forward_kind(ForwardAs::Inline), "", Some(&raw));
        kept_all.forward_parts = Some(vec![0, 1]);
        let draft = built(&f, &kept_all);
        assert_eq!(
            crate::message::header_text(&draft, HEADER).as_deref(),
            Some("forward; original=\"<m1@x.org>\"")
        );
        let opened = read(&draft, finds(&raw)).unwrap();
        assert_eq!((opened.text.as_str(), opened.forward_parts), ("", None));
        assert!(opened.files.is_empty());
        let mut none = request(forward_kind(ForwardAs::Inline), "", Some(&raw));
        none.forward_parts = Some(vec![]);
        let opened = read(&built(&f, &none), finds(&raw)).unwrap();
        assert_eq!(opened.forward_parts, Some(vec![]));
    }

    #[test]
    fn a_forward_as_attachment_draft_reads_back() {
        let f = fixture();
        let raw = original(MIXED, TWO);
        let draft = built(
            &f,
            &request(
                forward_kind(ForwardAs::Attachment),
                "see attached\n",
                Some(&raw),
            ),
        );
        assert_eq!(
            crate::message::header_text(&draft, HEADER).as_deref(),
            Some("forward-attachment; original=\"<m1@x.org>\"")
        );
        let opened = read(&draft, finds(&raw)).unwrap();
        assert_eq!(opened.kind, DraftKind::Forward(ForwardAs::Attachment));
        assert_eq!(opened.text, "see attached");
        assert!(opened.files.is_empty(), "{:?}", opened.files);
        assert_eq!(opened.original.as_deref(), Some(raw.as_slice()));
    }

    #[test]
    fn a_draft_edited_in_another_client_opens_whole() {
        let f = fixture();
        let raw = original("", "Shall we?\r\n");
        let draft = built(
            &f,
            &request(reply_kind(ReplyTo::All), "Sure.\n", Some(&raw)),
        );
        let edited = String::from_utf8(draft)
            .unwrap()
            .replace("> Shall we?", "> Shall we? Or not.");
        let opened = read(edited.as_bytes(), finds(&raw)).unwrap();
        assert_eq!(opened.kind, DraftKind::New);
        assert_eq!(opened.in_reply_to.as_deref(), Some("<m1@x.org>"));
        assert_eq!(opened.references, ["<m1@x.org>"]);
        assert!(
            opened.text.starts_with("Sure.\n\nOn Mon, 2 Nov 2026"),
            "{:?}",
            opened.text
        );
        assert!(
            opened.text.contains("> Shall we? Or not."),
            "{:?}",
            opened.text
        );
        assert_eq!(opened.original, None);
        assert_eq!(opened.shown_original.as_deref(), Some(raw.as_slice()));
        assert_eq!(opened.forward_parts, None);
    }

    #[test]
    fn a_draft_whose_original_is_gone_opens_whole_keeping_the_thread() {
        let f = fixture();
        let raw = original("References: <a@x.org>\r\n", "Shall we?\r\n");
        let draft = built(
            &f,
            &request(reply_kind(ReplyTo::Sender), "Sure.\n", Some(&raw)),
        );
        let opened = read(&draft, finds_nothing).unwrap();
        assert_eq!(opened.kind, DraftKind::New);
        assert_eq!((opened.original, opened.shown_original), (None, None));
        assert!(opened.text.starts_with("Sure.\n\nOn "), "{:?}", opened.text);
        assert!(opened.text.contains("> Shall we?"), "{:?}", opened.text);
        assert_eq!(opened.in_reply_to.as_deref(), Some("<m1@x.org>"));
        assert_eq!(opened.references, ["<a@x.org>", "<m1@x.org>"]);
        assert_eq!(opened.subject, "Re: Plans");
    }

    #[test]
    fn a_foreign_draft_opens_with_its_text_and_attachments() {
        let raw = original("", "Shall we?\r\n");
        let foreign = b"From: Pieter <me@example.com>\r\nTo: Linus <linus@x.org>, b@x.org\r\nBcc: hidden@x.org\r\nSubject: Re: Plans\r\nMessage-ID: <d1@example.com>\r\nIn-Reply-To: <m1@x.org>\r\nReferences: <m1@x.org>\r\nX-Postvak-Draft: reply; original=\"<m1@x.org\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=b\r\n\r\n--b\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nHello\r\nthere \r\n--b\r\nContent-Type: application/pdf\r\nContent-Disposition: attachment; filename=a.pdf\r\nContent-Transfer-Encoding: base64\r\n\r\nJVBERg==\r\n--b--\r\n";
        let opened = read(foreign, finds(&raw)).unwrap();
        assert_eq!(opened.kind, DraftKind::New);
        assert_eq!(opened.original, None);
        assert_eq!(opened.shown_original.as_deref(), Some(raw.as_slice()));
        assert_eq!(opened.in_reply_to.as_deref(), Some("<m1@x.org>"));
        assert_eq!(opened.references, ["<m1@x.org>"]);
        assert_eq!(opened.text.trim_end(), "Hello\nthere");
        assert_eq!(opened.files, [("a.pdf".to_string(), b"%PDF".to_vec())]);
        assert_eq!(opened.to, ["Linus <linus@x.org>", "b@x.org"]);
        assert_eq!(opened.bcc, ["hidden@x.org"]);
        assert_eq!(opened.from, "me@example.com");
        assert_eq!(opened.message_id, "d1@example.com");
        assert_eq!(opened.note, None);
        let unthreaded = read(foreign, finds_nothing).unwrap();
        assert_eq!(unthreaded.kind, DraftKind::New);
        assert_eq!(unthreaded.shown_original, None);
        assert_eq!(unthreaded.in_reply_to.as_deref(), Some("<m1@x.org>"));
    }

    #[test]
    fn an_html_only_draft_opens_as_text_with_the_note() {
        let html = b"From: me@example.com\r\nTo: a@x.org\r\nSubject: s\r\nMIME-Version: 1.0\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<p>Hello <b>there</b></p>\r\n";
        let opened = read(html, finds_nothing).unwrap();
        assert_eq!(opened.kind, DraftKind::New);
        assert!(opened.text.contains("Hello there"), "{:?}", opened.text);
        assert!(!opened.text.contains('<'), "{:?}", opened.text);
        assert_eq!(
            opened.note,
            Some("This draft had only HTML; it opens as text.")
        );
    }

    #[test]
    fn each_build_has_a_new_message_id() {
        let f = fixture();
        let new = request(
            SendKind::New {
                to: vec!["a@x.org".into()],
                cc: vec![],
                bcc: vec![],
                subject: "s".into(),
            },
            "hi",
            None,
        );
        let (one, two) = (built(&f, &new), built(&f, &new));
        let id = |raw: &[u8]| read(raw, finds_nothing).unwrap().message_id;
        assert_ne!(id(&one), id(&two));
        assert_eq!(
            crate::message::parse_headers(&one).message_id,
            Some(id(&one))
        );
        assert_eq!(
            crate::message::header_text(&one, HEADER).as_deref(),
            Some("new")
        );
        assert_eq!(read(&one, finds_nothing).unwrap().text, "hi");
    }

    #[test]
    fn a_draft_without_recipients_builds() {
        let f = fixture();
        let nobody = request(
            SendKind::New {
                to: vec![],
                cc: vec![],
                bcc: vec![],
                subject: "Later".into(),
            },
            "",
            None,
        );
        let opened = read(&built(&f, &nobody), finds_nothing).unwrap();
        assert_eq!(opened.kind, DraftKind::New);
        assert!(opened.to.is_empty() && opened.cc.is_empty() && opened.bcc.is_empty());
        assert_eq!(opened.subject, "Later");
    }

    #[test]
    fn a_foreign_reply_draft_sent_again_quotes_nothing_twice() {
        let f = fixture();
        let raw = original("", "Shall we?\r\n");
        let draft = built(
            &f,
            &request(reply_kind(ReplyTo::Sender), "Sure.\n", Some(&raw)),
        );
        let edited = String::from_utf8(draft)
            .unwrap()
            .replace("> Shall we?", "> Shall we? Or not.");
        let opened = read(edited.as_bytes(), finds(&raw)).unwrap();
        assert_eq!(opened.kind, DraftKind::New);
        let again = SendRequest {
            headers: Some(compose::HeaderEdits {
                from: opened.from.clone(),
                to: opened.to.clone(),
                cc: opened.cc.clone(),
                bcc: opened.bcc.clone(),
                subject: opened.subject.clone(),
                in_reply_to: opened.in_reply_to.clone(),
                references: opened.references.clone(),
            }),
            ..request(
                SendKind::New {
                    to: opened.to.clone(),
                    cc: opened.cc.clone(),
                    bcc: opened.bcc.clone(),
                    subject: opened.subject.clone(),
                },
                &opened.text,
                opened.original.as_deref(),
            )
        };
        let rebuilt = built(&f, &again);
        let text = crate::message::body_text(&rebuilt);
        assert_eq!(text.matches("> Shall we? Or not.").count(), 1, "{text}");
        assert_eq!(text.matches("> Shall we?").count(), 1, "{text}");
        assert_eq!(text.matches(" wrote:").count(), 1, "{text}");
        assert!(text.starts_with("Sure."), "{text}");
        assert_eq!(
            crate::message::header_text(&rebuilt, "In-Reply-To").as_deref(),
            Some("<m1@x.org>")
        );
    }

    #[test]
    fn a_saved_foreign_reply_still_shows_its_original() {
        let f = fixture();
        let raw = original("", "Shall we?\r\n");
        let foreign = b"From: me@example.com\r\nTo: linus@x.org\r\nSubject: Re: Plans\r\nMessage-ID: <d1@example.com>\r\nIn-Reply-To: <m1@x.org>\r\nReferences: <m1@x.org>\r\n\r\nSure.\r\n\r\n> Shall we?\r\n";
        let opened = read(foreign, finds(&raw)).unwrap();
        let saved = built(
            &f,
            &SendRequest {
                headers: Some(compose::HeaderEdits {
                    from: opened.from.clone(),
                    to: opened.to.clone(),
                    cc: vec![],
                    bcc: vec![],
                    subject: opened.subject.clone(),
                    in_reply_to: opened.in_reply_to.clone(),
                    references: opened.references.clone(),
                }),
                ..request(
                    SendKind::New {
                        to: opened.to.clone(),
                        cc: vec![],
                        bcc: vec![],
                        subject: opened.subject.clone(),
                    },
                    &opened.text,
                    None,
                )
            },
        );
        assert_eq!(
            crate::message::header_text(&saved, HEADER).as_deref(),
            Some("new")
        );
        let reopened = read(&saved, finds(&raw)).unwrap();
        assert_eq!(reopened.kind, DraftKind::New);
        assert_eq!(reopened.original, None);
        assert_eq!(reopened.shown_original.as_deref(), Some(raw.as_slice()));
        assert_eq!(reopened.in_reply_to.as_deref(), Some("<m1@x.org>"));
    }

    #[test]
    fn a_forward_draft_whose_forwarded_files_changed_opens_whole() {
        let f = fixture();
        let raw = original(MIXED, TWO);
        let inline = built(
            &f,
            &request(forward_kind(ForwardAs::Inline), "FYI\n", Some(&raw)),
        );
        let inline = String::from_utf8(inline).unwrap();
        assert_eq!(inline.matches("1,2,3").count(), 1, "{inline}");
        let changed = inline.replace("1,2,3", "1,2,4");
        let opened = read(changed.as_bytes(), finds(&raw)).unwrap();
        assert_eq!(opened.kind, DraftKind::New);
        let names: Vec<_> = opened.files.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["a.pdf", "b.csv"]);
        assert_eq!(opened.files[1].1, b"1,2,4");

        let enclosed = built(
            &f,
            &request(forward_kind(ForwardAs::Attachment), "", Some(&raw)),
        );
        let enclosed = String::from_utf8(enclosed).unwrap();
        assert!(enclosed.contains("1,2,3"), "{enclosed}");
        let changed = enclosed.replace("1,2,3", "1,2,4");
        let opened = read(changed.as_bytes(), finds(&raw)).unwrap();
        assert_eq!(opened.kind, DraftKind::New);
        assert_eq!(opened.files.len(), 1);
    }

    #[test]
    fn a_malformed_marker_reads_as_foreign() {
        let raw = original("", "Shall we?\r\n");
        for marker in [
            "bogus; original=\"<m1@x.org>\"",
            "reply; original=<m1@x.org>",
            "forward; parts=\"x\"",
            "",
        ] {
            let draft = format!(
                "From: me@example.com\r\nSubject: Re: Plans\r\nIn-Reply-To: <m1@x.org>\r\nX-Postvak-Draft: {marker}\r\n\r\nSure.\r\n"
            );
            let opened = read(draft.as_bytes(), finds(&raw)).unwrap();
            assert_eq!(opened.kind, DraftKind::New, "{marker}");
            assert!(opened.shown_original.is_some(), "{marker}");
            assert_eq!(opened.text.trim_end(), "Sure.", "{marker}");
        }
    }

    /// The message without its Date, Message-ID and `X-Postvak-Draft`, and with its MIME boundaries named alike.
    fn comparable(raw: &[u8]) -> String {
        let raw = String::from_utf8(raw.to_vec()).unwrap();
        let mut boundaries: Vec<String> = raw
            .split("boundary=\"")
            .skip(1)
            .filter_map(|rest| rest.split('"').next().map(str::to_string))
            .collect();
        boundaries.sort_by_key(|b| std::cmp::Reverse(b.len()));
        let kept = raw
            .split("\r\n")
            .filter(|line| {
                !["Date: ", "Message-ID: ", "X-Postvak-Draft: "]
                    .iter()
                    .any(|name| line.starts_with(name))
            })
            .collect::<Vec<_>>()
            .join("\r\n");
        boundaries.iter().enumerate().fold(kept, |text, (i, b)| {
            text.replace(b, &format!("BOUNDARY{i}"))
        })
    }

    #[test]
    fn the_draft_is_the_message_compose_would_send() {
        use crate::send::compose::tests::{home, request};
        let h = home();
        for kind in [
            SendKind::New {
                to: vec!["a@x.org".into()],
                cc: vec![],
                bcc: vec![],
                subject: "Lunch".into(),
            },
            SendKind::Reply {
                folder: "INBOX".into(),
                uid: 7,
                to: ReplyTo::Sender,
            },
            SendKind::Forward {
                folder: "INBOX".into(),
                uid: 7,
                as_: ForwardAs::Inline,
                to: vec!["b@x.org".into()],
                cc: vec![],
                bcc: vec![],
            },
            SendKind::Forward {
                folder: "INBOX".into(),
                uid: 7,
                as_: ForwardAs::Attachment,
                to: vec!["b@x.org".into()],
                cc: vec![],
                bcc: vec![],
            },
        ] {
            let draft = build(
                &h.config,
                &h.daemon.paths,
                &h.client,
                &request(kind.clone()),
            )
            .unwrap();
            let queued = compose::compose(
                &h.config,
                &h.daemon.paths,
                &h.client,
                request(kind.clone()),
                |_| anyhow::bail!("no editor"),
            )
            .unwrap();
            let path = h
                .daemon
                .paths
                .outbox_dir("work")
                .join(format!("{}.eml", queued.id));
            let sent = std::fs::read(path).unwrap();
            assert_eq!(comparable(&draft), comparable(&sent), "{kind:?}");
        }
    }

    fn folder(name: &str, special_use: Option<&str>) -> Folder {
        Folder {
            name: name.into(),
            uidvalidity: 1,
            last_uid: 0,
            special_use: special_use.map(str::to_string),
            delimiter: None,
        }
    }

    #[test]
    fn with_message_id_replaces_the_header_and_keeps_the_rest() {
        let raw =
            b"From: a@x\r\nmessage-id:\r\n <old@x>\r\nSubject: s\r\n\r\nMessage-ID: <body@x>\r\n";
        assert_eq!(
            with_message_id(raw, "<new@x>"),
            b"From: a@x\r\nMessage-ID: <new@x>\r\nSubject: s\r\n\r\nMessage-ID: <body@x>\r\n"
        );
        assert_eq!(
            with_message_id(b"Subject: s\n\nbody\n", "<new@x>"),
            b"Message-ID: <new@x>\nSubject: s\n\nbody\n"
        );
    }

    #[test]
    fn drafts_folder_prefers_special_use_then_the_name() {
        let folders = [folder("Drafts", None), folder("Concepten", Some("Drafts"))];
        assert_eq!(drafts_folder(&folders).as_deref(), Some("Concepten"));
        assert_eq!(drafts_folder(&folders[..1]).as_deref(), Some("Drafts"));
        assert_eq!(drafts_folder(&[folder("drafts", None)]), None);
        assert_eq!(drafts_folder(&[]), None);
    }

    mod saving {
        use super::super::*;
        use crate::mail_ops::{MailError, RecordingOps};
        use crate::store::Store;

        const OLD: &str = "Message-ID: <d1@x>\r\nX-Postvak-Draft: new\r\n\r\n";

        fn draft(id: &str) -> Vec<u8> {
            format!("Message-ID: <{id}>\r\nX-Postvak-Draft: new\r\nSubject: s\r\n\r\nbody\r\n")
                .into_bytes()
        }

        fn with_drafts() -> (RecordingOps, Store) {
            let store = Store::open_in_memory().unwrap();
            store
                .upsert_folder(&Folder {
                    name: "Drafts".into(),
                    uidvalidity: 1,
                    last_uid: 0,
                    special_use: Some("Drafts".into()),
                    delimiter: None,
                })
                .unwrap();
            let mut ops = RecordingOps::default().with_folder("Drafts", Some("Drafts"));
            ops.add_mail("Drafts", 1, 0, OLD, Some(OLD));
            (ops, store)
        }

        fn ids(ops: &RecordingOps) -> Vec<Option<String>> {
            ops.mail["Drafts"]
                .iter()
                .map(|e| crate::message::parse_headers(&e.headers).message_id)
                .collect()
        }

        #[test]
        fn saving_appends_with_the_draft_flags() {
            let (mut ops, store) = with_drafts();
            let saved = save(&mut ops, &store, "work", Some(&draft("d2@x")), &[]).unwrap();
            assert_eq!(saved.message_id.as_deref(), Some("<d2@x>"));
            assert!(saved.left.is_empty());
            let appended = ops.mail["Drafts"].last().unwrap();
            assert_eq!(appended.flags, ["\\Draft", "\\Seen"]);
            assert_eq!(ops.mail["Drafts"].len(), 2);
        }

        #[test]
        fn saving_again_deletes_the_copy_it_replaces_after_the_append() {
            let (mut ops, store) = with_drafts();
            save(
                &mut ops,
                &store,
                "work",
                Some(&draft("d2@x")),
                &["<d1@x>".into()],
            )
            .unwrap();
            assert_eq!(ids(&ops), [Some("d2@x".to_string())]);
            let append = ops.calls.iter().position(|c| c.starts_with("append"));
            let delete = ops.calls.iter().position(|c| c.starts_with("add_flags"));
            assert!(append < delete && delete.is_some(), "{:?}", ops.calls);
        }

        #[test]
        fn a_failed_append_keeps_the_old_copy() {
            let (mut ops, store) = with_drafts();
            ops.fail_next = Some(MailError::Protocol("NO over quota".into()));
            let error = save(
                &mut ops,
                &store,
                "work",
                Some(&draft("d2@x")),
                &["<d1@x>".into()],
            )
            .unwrap_err();
            assert!(error.to_string().contains("over quota"), "{error}");
            assert_eq!(ids(&ops), [Some("d1@x".to_string())]);
            assert!(!ops.calls.iter().any(|c| c.starts_with("search_header")));
        }

        #[test]
        fn no_drafts_folder_is_refused_with_the_words() {
            let store = Store::open_in_memory().unwrap();
            let mut ops = RecordingOps::default();
            let error = save(&mut ops, &store, "work", Some(&draft("d2@x")), &[]).unwrap_err();
            assert_eq!(error.to_string(), "This account has no Drafts folder");
            assert!(ops.calls.is_empty(), "{:?}", ops.calls);
        }

        #[test]
        fn deleting_only_removes_the_copy() {
            let (mut ops, store) = with_drafts();
            let saved = save(&mut ops, &store, "work", None, &["<d1@x>".into()]).unwrap();
            assert_eq!(saved, SavedDraft::default());
            assert!(ops.mail["Drafts"].is_empty());
            assert!(!ops.calls.iter().any(|c| c.starts_with("append")));
        }

        #[test]
        fn deleting_a_draft_that_is_gone_is_fine() {
            let (mut ops, store) = with_drafts();
            let saved = save(&mut ops, &store, "work", None, &["<gone@x>".into()]).unwrap();
            assert_eq!(saved, SavedDraft::default());
            assert_eq!(ids(&ops), [Some("d1@x".to_string())]);
            assert!(!ops.calls.iter().any(|c| c.starts_with("add_flags")));
        }

        #[test]
        fn a_save_deletes_every_copy_it_replaces() {
            let (mut ops, store) = with_drafts();
            let older = "Message-ID: <d0@x>\r\n\r\n";
            ops.add_mail("Drafts", 2, 0, older, Some(older));
            let replaces = ["<d1@x>".to_string(), "<d0@x>".to_string()];
            let saved = save(&mut ops, &store, "work", Some(&draft("d2@x")), &replaces).unwrap();
            assert!(saved.left.is_empty(), "{saved:?}");
            assert_eq!(ids(&ops), [Some("d2@x".to_string())]);
        }

        #[test]
        fn a_failed_delete_after_the_append_reports_what_is_left() {
            for failure in [
                MailError::Protocol("NO expunge refused".into()),
                MailError::Io("reset".into()),
            ] {
                let (mut ops, store) = with_drafts();
                ops.fail_next_expunge = Some(failure);
                let saved = save(
                    &mut ops,
                    &store,
                    "work",
                    Some(&draft("d2@x")),
                    &["<d1@x>".into()],
                )
                .unwrap();
                assert_eq!(saved.message_id.as_deref(), Some("<d2@x>"));
                assert_eq!(saved.left, ["<d1@x>"]);
            }
        }

        #[test]
        fn a_replaced_id_never_deletes_a_draft_whose_id_merely_contains_it() {
            for replaced in ["d1@x", "<d1@x>"] {
                let (mut ops, store) = with_drafts();
                let longer = "Message-ID: <xd1@x>\r\n\r\n";
                ops.add_mail("Drafts", 2, 0, longer, Some(longer));
                let saved = save(&mut ops, &store, "work", None, &[replaced.into()]).unwrap();
                assert_eq!(saved, SavedDraft::default());
                assert_eq!(ids(&ops), [Some("xd1@x".to_string())], "{replaced}");
            }
        }

        #[test]
        fn a_failed_delete_only_tries_every_copy_and_reports_what_is_left() {
            let (mut ops, store) = with_drafts();
            let older = "Message-ID: <d0@x>\r\n\r\n";
            ops.add_mail("Drafts", 2, 0, older, Some(older));
            ops.fail_next_expunge = Some(MailError::Protocol("NO expunge refused".into()));
            let replaces = ["<d1@x>".to_string(), "<d0@x>".to_string()];
            let saved = save(&mut ops, &store, "work", None, &replaces).unwrap();
            assert_eq!(saved.left, ["<d1@x>"]);
            assert_eq!(ids(&ops), [Some("d1@x".to_string())]);
        }

        #[test]
        fn a_lost_connection_while_only_deleting_is_an_error() {
            let (mut ops, store) = with_drafts();
            ops.fail_next_expunge = Some(MailError::Io("reset".into()));
            assert!(save(&mut ops, &store, "work", None, &["<d1@x>".into()]).is_err());
        }
    }
}
