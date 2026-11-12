//! The `forward` action: its table in rules.toml, the checks it gets when a rule compiles, and the decisions a forward
//! takes without side effects: the once-only key, the loop guard, the rate, and the draft it sends.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Matcher, TextMatch, mime_matches, mime_pattern, parse_size};
use crate::config::Identity;
use crate::send::build::{Draft, FileAttachment};
use crate::send::reply::{AttachmentFilter, ForwardAs, ForwardOptions, Recipients};

pub const DEFAULT_PREFIX: &str = "Fwd: ";
pub const DEFAULT_RATE: u32 = 20;
/// What a forward's action label, and so its action log row, starts with.
pub const LABEL_PREFIX: &str = "forward:";

/// Added to every rule forward with our address, kept from the original's; it is how a loop is seen.
pub const LOOP_HEADER: &str = "X-Postvak-Forwarded";

/// A message that carries this many loop headers has been forwarded on far enough.
const MAX_HOPS: usize = 3;

/// Sends the message on through the account's SMTP server. Only new mail, and only once you approved the rule in
/// Postvak: any change to the rule needs a new approval
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Forward {
    /// One address or a list
    #[serde(with = "addresses")]
    #[schemars(with = "super::OneOrMany<String>")]
    pub to: Vec<String>,
    /// One address or a list
    #[serde(default, with = "addresses", skip_serializing_if = "Vec::is_empty")]
    #[schemars(with = "Option<super::OneOrMany<String>>")]
    pub cc: Vec<String>,
    /// One address or a list
    #[serde(default, with = "addresses", skip_serializing_if = "Vec::is_empty")]
    #[schemars(with = "Option<super::OneOrMany<String>>")]
    pub bcc: Vec<String>,
    /// Your address to send as: the account's or one an alias matches; needs the rule's account. Default the address of
    /// yours the message was sent to, else the account's
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// Put before the subject unless it already starts with a forward prefix; may be empty; {subaddress} as in move.
    /// Default "Fwd: "
    #[serde(default = "default_prefix", skip_serializing_if = "is_default_prefix")]
    pub subject_prefix: String,
    /// Text above the forwarded message
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// inline (default): the text below a header block, attachments attached again; attachment: the original whole
    #[serde(rename = "as", default, skip_serializing_if = "is_default")]
    #[schemars(with = "ForwardAsSchema")]
    pub as_: ForwardAs,
    /// Which attachments go along with an inline forward: true (default) all, false none, or a table naming them
    #[serde(default, skip_serializing_if = "is_default")]
    pub attachments: Attachments,
    /// When the message is larger: a size such as 10MB, or { limit = "10MB", over = "strip" }. skip (default) does not
    /// forward it, strip forwards it without attachments. Default limit 25MB or the server's own
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_size: Option<MaxSize>,
    /// Forwards per hour, at most; more pauses the rule until you approve it again. Default 20
    #[serde(default = "default_rate", skip_serializing_if = "is_default_rate")]
    pub rate: u32,
}

// Mirrors `ForwardAs` for the schema, which lives in `send`.
#[derive(JsonSchema)]
#[serde(rename_all = "lowercase")]
#[expect(dead_code, reason = "only describes the field for the schema")]
enum ForwardAsSchema {
    Inline,
    Attachment,
}

/// true forwards every attachment, false none, a table those that match all its conditions
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Attachments {
    Every(bool),
    Matching(AttachmentMatch),
}

impl Default for Attachments {
    fn default() -> Self {
        Attachments::Every(true)
    }
}

/// Attachments that match every condition given; at least one
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AttachmentMatch {
    /// A MIME type such as application/pdf, or application/* for any subtype
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub type_: Option<String>,
    /// The file name
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<TextMatch>,
    /// At most this size, e.g. 500KB, 5MB
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_size: Option<String>,
}

/// A size such as 10MB, or a table with limit and over
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum MaxSize {
    Limit(String),
    Table(SizeTable),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SizeTable {
    /// A size such as 10MB; default 25MB or the server's own
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<String>,
    /// skip: not forwarded; strip: forwarded without its attachments
    #[serde(default)]
    pub over: OverSize,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum OverSize {
    #[default]
    Skip,
    Strip,
}

impl MaxSize {
    /// The limit as written, `None` for the default.
    pub fn limit_text(&self) -> Option<&str> {
        match self {
            MaxSize::Limit(text) => Some(text),
            MaxSize::Table(table) => table.limit.as_deref(),
        }
    }

    /// The limit in bytes, `None` for the default; checked when the rule compiled.
    pub fn limit(&self) -> Option<u64> {
        self.limit_text().and_then(parse_size)
    }

    pub fn over(&self) -> OverSize {
        match self {
            MaxSize::Limit(_) => OverSize::Skip,
            MaxSize::Table(table) => table.over,
        }
    }
}

impl Forward {
    pub fn recipients(&self) -> impl Iterator<Item = &String> {
        self.to.iter().chain(&self.cc).chain(&self.bcc)
    }

    /// `<first recipient>`, with ` (+N)` for the others.
    pub fn targets(&self) -> String {
        let mut all = self.recipients();
        let first = all.next().map(String::as_str).unwrap_or_default();
        match all.count() {
            0 => first.to_string(),
            others => format!("{first} (+{others})"),
        }
    }

    /// `forward:` and the targets: the action's label and its action log row.
    pub fn label(&self) -> String {
        format!("{LABEL_PREFIX}{}", self.targets())
    }

    /// Refused when the table cannot be sent as written; `account` is the rule's, which `from` needs.
    pub fn check(&self, account: Option<&str>) -> Result<(), String> {
        if self.to.is_empty() {
            return Err("actions.forward.to needs an address".into());
        }
        for address in self.recipients() {
            crate::send::check::valid_address(address.trim()).map_err(|_| {
                format!(
                    "actions.forward: '{}' is not an address such as books@example.org",
                    address.escape_default()
                )
            })?;
        }
        if let Some(from) = &self.from {
            if account.is_none() {
                return Err("actions.forward.from needs the rule's account: a rule for every account sends as each \
                            account's own address"
                    .into());
            }
            if from.contains('*') || crate::send::check::valid_address(from.trim()).is_err() {
                return Err(format!(
                    "actions.forward.from: '{}' is not an address such as me@example.org",
                    from.escape_default()
                ));
            }
        }
        if self.subject_prefix.chars().any(char::is_control) {
            return Err(
                "actions.forward.subject_prefix must not contain control characters".into(),
            );
        }
        if self.rate == 0 {
            return Err("actions.forward.rate must be at least 1".into());
        }
        if let Attachments::Matching(filter) = &self.attachments {
            if self.as_ == ForwardAs::Attachment {
                return Err("actions.forward.attachments cannot pick attachments with as = \"attachment\", which \
                            forwards the original whole"
                    .into());
            }
            filter.check()?;
        }
        if let Some(text) = self.max_size.as_ref().and_then(MaxSize::limit_text)
            && parse_size(text).is_none()
        {
            return Err(format!(
                "actions.forward.max_size: expected a size such as 500KB or 5MB, got '{text}'"
            ));
        }
        Ok(())
    }
}

impl AttachmentMatch {
    fn check(&self) -> Result<(), String> {
        if self.type_.is_none() && self.name.is_none() && self.max_size.is_none() {
            return Err("actions.forward.attachments needs type, name or max_size".into());
        }
        if let Some(text) = &self.type_
            && mime_pattern(text).is_none()
        {
            return Err(format!(
                "actions.forward.attachments.type: expected a MIME type such as application/pdf or image/*, got '{text}'"
            ));
        }
        if let Some(text) = &self.max_size
            && parse_size(text).is_none()
        {
            return Err(format!(
                "actions.forward.attachments.max_size: expected a size such as 500KB or 5MB, got '{text}'"
            ));
        }
        self.name_matcher().transpose().map(drop)
    }

    /// The `name` condition compiled; `None` without one.
    pub(crate) fn name_matcher(&self) -> Option<Result<Matcher, String>> {
        let name = self.name.as_ref()?;
        Some(
            Matcher::build(
                "",
                "actions.forward.attachments.name",
                &name.contains,
                &name.equals,
                &name.regex,
            )
            .map_err(|e| match e {
                super::RulesError::Invalid { reason, .. } => reason,
                other => other.to_string(),
            }),
        )
    }
}

/// What a forward rule keeps of a message it forwarded: `id:<Message-ID>`, or `uid:<folder>/<uidvalidity>/<uid>` for
/// one without, so the message is not forwarded again once it moved back or synced again.
pub fn once_key(msg: &crate::store::Message, uidvalidity: u32) -> String {
    match msg
        .message_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        Some(id) => format!("id:{id}"),
        None => format!("uid:{}/{uidvalidity}/{}", msg.folder, msg.uid),
    }
}

/// Why a forward rule leaves a message unforwarded while its other actions run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skip {
    /// A loop header names this account: it forwarded the message before.
    Loop,
    TooManyHops,
    /// `Auto-Submitted: auto-replied`: an out-of-office or a bounce, which forwarding could bounce back.
    AutoReply,
    /// Over `max_size` with `skip`.
    OverSize,
}

impl Skip {
    pub fn log_text(&self) -> &'static str {
        match self {
            Skip::Loop => "forward skipped: this account forwarded it before",
            Skip::TooManyHops => "forward skipped: forwarded three times already",
            Skip::AutoReply => "forward skipped: an automatic reply",
            Skip::OverSize => "forward skipped: larger than max_size",
        }
    }
}

/// The addresses in the loop headers of a message's header block, in order.
fn hops(headers: &[u8]) -> Vec<String> {
    crate::message::header_fields(headers)
        .into_iter()
        .filter(|field| field.name.eq_ignore_ascii_case(LOOP_HEADER))
        .map(|field| field.value)
        .collect()
}

/// Whether a message with these headers must not be forwarded, and why.
pub fn loop_check(headers: &[u8], identity: &Identity) -> Option<Skip> {
    let hops = hops(headers);
    if hops.iter().any(|address| identity.is_me(address)) {
        return Some(Skip::Loop);
    }
    if hops.len() >= MAX_HOPS {
        return Some(Skip::TooManyHops);
    }
    let auto = crate::message::header_value(headers, "Auto-Submitted").unwrap_or_default();
    let kind = auto.split(';').next().unwrap_or_default().trim();
    kind.eq_ignore_ascii_case("auto-replied")
        .then_some(Skip::AutoReply)
}

/// `built`, a forward of a message with `original_headers`, with the original's loop headers and ours on top. A value
/// that is no address is dropped rather than copied into a header of ours.
pub fn with_loop_header(built: &[u8], original_headers: &[u8], our_address: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(built.len() + 128);
    let ours = our_address.to_string();
    for address in hops(original_headers).iter().chain([&ours]) {
        let address = address.trim();
        if crate::send::check::valid_address(address).is_ok() {
            out.extend_from_slice(format!("{LOOP_HEADER}: {address}\r\n").as_bytes());
        }
    }
    out.extend_from_slice(built);
    out
}

/// The draft that forwards `original` as `forward` says, placeholders filled; or why it is skipped. `default_limit` is
/// the size limit when the rule names none.
pub fn draft(
    forward: &Forward,
    original: &[u8],
    identity: &Identity,
    default_limit: u64,
) -> Result<Result<Draft, Skip>, String> {
    let limit = forward
        .max_size
        .as_ref()
        .and_then(MaxSize::limit)
        .unwrap_or(default_limit);
    let over = u64::try_from(original.len()).unwrap_or(u64::MAX) > limit;
    let strip = over && forward.max_size.as_ref().map(MaxSize::over) == Some(OverSize::Strip);
    if over && !strip {
        return Ok(Err(Skip::OverSize));
    }
    // A stripped forward goes inline: the original whole would carry its attachments along.
    let options = ForwardOptions {
        as_: if strip {
            ForwardAs::Inline
        } else {
            forward.as_
        },
        subject_prefix: forward.subject_prefix.clone(),
        note: forward.note.clone(),
        attachments: AttachmentFilter::All,
    };
    let recipients = Recipients {
        to: forward.to.clone(),
        cc: forward.cc.clone(),
        bcc: forward.bcc.clone(),
    };
    let mut draft = crate::send::reply::forward(original, recipients, identity, "", &options)
        .map_err(|e| e.to_string())?;
    if let Some(from) = &forward.from {
        draft.from = from.trim().to_string();
        draft.from_name = identity.display_name_for(&draft.from).map(str::to_string);
    }
    if options.as_ == ForwardAs::Inline {
        let name = match &forward.attachments {
            Attachments::Matching(filter) => filter.name_matcher().transpose()?,
            Attachments::Every(_) => None,
        };
        let (kept, left): (Vec<_>, Vec<_>) = std::mem::take(&mut draft.attachments)
            .into_iter()
            .partition(|attachment| !strip && forward.attachments.takes(attachment, name.as_ref()));
        draft.attachments = kept;
        not_forwarded(&mut draft, &left);
    }
    Ok(Ok(draft))
}

impl Attachments {
    fn takes(&self, attachment: &FileAttachment, name: Option<&Matcher>) -> bool {
        match self {
            Attachments::Every(every) => *every,
            Attachments::Matching(filter) => {
                let type_ok =
                    filter
                        .type_
                        .as_deref()
                        .and_then(mime_pattern)
                        .is_none_or(|pattern| {
                            mime_matches(&pattern, &attachment.content_type.to_ascii_lowercase())
                        });
                let size_ok = filter
                    .max_size
                    .as_deref()
                    .and_then(parse_size)
                    .is_none_or(|max| {
                        u64::try_from(attachment.bytes.len()).unwrap_or(u64::MAX) <= max
                    });
                type_ok && size_ok && name.is_none_or(|matcher| matcher.is_match(&attachment.name))
            }
        }
    }
}

/// Names the attachments left out below the forwarded message: `Not forwarded: big.zip (40 MB)`.
fn not_forwarded(draft: &mut Draft, left: &[FileAttachment]) {
    if left.is_empty() {
        return;
    }
    let lines: Vec<String> = left
        .iter()
        .map(|attachment| {
            format!(
                "Not forwarded: {} ({})",
                attachment.name,
                size_text(attachment.bytes.len())
            )
        })
        .collect();
    draft.text = format!("{}\n\n{}\n", draft.text.trim_end(), lines.join("\n"));
    if let Some(html) = &mut draft.html {
        let close = "</body></html>";
        let paragraph: String = lines
            .iter()
            .map(|line| format!("<p>{}</p>", crate::send::quote_html::escape(line)))
            .collect();
        match html.html.strip_suffix(close) {
            Some(open) => html.html = format!("{open}{paragraph}{close}"),
            None => html.html.push_str(&paragraph),
        }
    }
}

/// `40 MB`, `512 KB` or `300 B`, in powers of 1024 as rules.toml counts them, rounded down.
fn size_text(bytes: usize) -> String {
    match bytes {
        0..1024 => format!("{bytes} B"),
        1024..1_048_576 => format!("{} KB", bytes >> 10),
        _ => format!("{} MB", bytes >> 20),
    }
}

fn default_prefix() -> String {
    DEFAULT_PREFIX.into()
}

fn is_default_prefix(prefix: &str) -> bool {
    prefix == DEFAULT_PREFIX
}

fn default_rate() -> u32 {
    DEFAULT_RATE
}

fn is_default_rate(rate: &u32) -> bool {
    *rate == DEFAULT_RATE
}

fn is_default<T: Default + PartialEq>(value: &T) -> bool {
    *value == T::default()
}

/// One address or a list in rules.toml, a list in memory; one address is written back bare.
mod addresses {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use super::super::OneOrMany;

    pub fn serialize<S: Serializer>(list: &[String], serializer: S) -> Result<S::Ok, S::Error> {
        match list {
            [one] => one.serialize(serializer),
            _ => list.serialize(serializer),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<String>, D::Error> {
        OneOrMany::deserialize(deserializer).map(|value| value.as_slice().to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Action, Actions, compile, parse};
    use super::*;

    fn rule(account: &str, forward: &str) -> String {
        format!(
            "[[rules]]\nname = \"inv\"\n{account}match.from = {{ contains = \"billing@\" }}\nactions.forward = {forward}\n"
        )
    }

    fn forward_of(text: &str) -> Forward {
        let file = parse(text).unwrap();
        compile(&file).unwrap();
        match &file.rules[0].actions[..] {
            [Action::Forward(forward), ..] => (**forward).clone(),
            other => panic!("no forward first in {other:?}"),
        }
    }

    fn refusal(text: &str) -> String {
        match parse(text) {
            Ok(file) => compile(&file).unwrap_err().to_string(),
            Err(e) => e.to_string(),
        }
    }

    fn identity() -> Identity {
        let mut account = crate::config::AccountConfig::new(
            "shared",
            "imap.example.com",
            "team@example.com",
            crate::config::PasswordSource::Keyring { keyring: true },
        );
        account.aliases = vec![crate::config::AliasEntry::Pattern(
            "sales@example.com".into(),
        )];
        account.identity().unwrap()
    }

    fn headers(extra: &str) -> Vec<u8> {
        format!("From: a@x.org\r\nTo: team@example.com\r\nSubject: Invoice\r\n{extra}\r\n")
            .into_bytes()
    }

    #[test]
    fn a_header_with_our_address_is_a_loop() {
        let id = identity();
        assert_eq!(
            loop_check(&headers("X-Postvak-Forwarded: Sales@Example.com\r\n"), &id),
            Some(Skip::Loop)
        );
        assert_eq!(
            loop_check(&headers("x-postvak-forwarded: team@example.com\r\n"), &id),
            Some(Skip::Loop)
        );
        assert_eq!(loop_check(&headers(""), &id), None);
    }

    #[test]
    fn a_header_with_another_address_is_not() {
        // The chain shared -> own -> bookkeeper: the own account forwards what the shared one passed on.
        let own = crate::config::AccountConfig::new(
            "own",
            "imap.example.com",
            "me@example.org",
            crate::config::PasswordSource::Keyring { keyring: true },
        )
        .identity()
        .unwrap();
        let arrived = headers("X-Postvak-Forwarded: team@example.com\r\n");
        assert_eq!(loop_check(&arrived, &own), None);
        let built = b"From: me@example.org\r\nSubject: Fwd: Invoice\r\n\r\nbody\r\n";
        let sent = with_loop_header(built, &arrived, "me@example.org");
        assert_eq!(hops(&sent), ["team@example.com", "me@example.org"]);
        assert!(sent.ends_with(built));
        assert_eq!(
            loop_check(&sent, &own),
            Some(Skip::Loop),
            "back at the own account, the chain stops"
        );
    }

    #[test]
    fn a_loop_header_that_is_no_address_is_not_copied() {
        let arrived = headers("X-Postvak-Forwarded: <script>\r\n");
        let sent = with_loop_header(b"\r\n", &arrived, "me@example.org");
        assert_eq!(hops(&sent), ["me@example.org"]);
    }

    #[test]
    fn three_headers_are_too_many_hops() {
        let three = "X-Postvak-Forwarded: a@x.org\r\nX-Postvak-Forwarded: b@x.org\r\nX-Postvak-Forwarded: c@x.org\r\n";
        assert_eq!(
            loop_check(&headers(three), &identity()),
            Some(Skip::TooManyHops)
        );
        let two = "X-Postvak-Forwarded: a@x.org\r\nX-Postvak-Forwarded: b@x.org\r\n";
        assert_eq!(loop_check(&headers(two), &identity()), None);
    }

    #[test]
    fn auto_replied_is_skipped() {
        assert_eq!(
            loop_check(
                &headers("Auto-Submitted: Auto-Replied; owner-email=x\r\n"),
                &identity()
            ),
            Some(Skip::AutoReply)
        );
    }

    #[test]
    fn auto_generated_newsletters_are_not_skipped() {
        assert_eq!(
            loop_check(&headers("Auto-Submitted: auto-generated\r\n"), &identity()),
            None
        );
    }

    fn with_attachments() -> Vec<u8> {
        let part = |name: &str, ty: &str, bytes: usize| {
            format!(
                "--b\r\nContent-Type: {ty}; name=\"{name}\"\r\nContent-Disposition: attachment; filename=\"{name}\"\r\n\r\n{}\r\n",
                "x".repeat(bytes)
            )
        };
        format!(
            "From: Shop <billing@shop.example>\r\nTo: team@example.com\r\nSubject: Invoice 7\r\nMessage-ID: <i7@shop.example>\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"b\"\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\nYour invoice.\r\n{}{}--b--\r\n",
            part("invoice.pdf", "application/pdf", 100),
            part("big.zip", "application/zip", 3000),
        )
        .into_bytes()
    }

    fn table(text: &str) -> Forward {
        forward_of(&rule("account = \"shared\"\n", text))
    }

    fn names(draft: &Draft) -> Vec<&str> {
        draft.attachments.iter().map(|a| a.name.as_str()).collect()
    }

    #[test]
    fn the_draft_keeps_the_attachments_the_filter_takes_and_names_the_rest() {
        let id = identity();
        let raw = with_attachments();
        let forward = table(
            "{ to = \"books@example.org\", note = \"For the records\", attachments = { type = \"application/*\", max_size = \"1KB\" } }",
        );
        let draft = draft(&forward, &raw, &id, u64::MAX).unwrap().unwrap();
        assert_eq!(names(&draft), ["invoice.pdf"]);
        assert!(draft.text.starts_with("For the records"), "{}", draft.text);
        assert!(
            draft
                .text
                .trim_end()
                .ends_with("Not forwarded: big.zip (2 KB)"),
            "{}",
            draft.text
        );
        assert_eq!(draft.subject, "Fwd: Invoice 7");
        assert_eq!(draft.to, ["books@example.org"]);
        assert_eq!(
            draft.from, "team@example.com",
            "the address the message was sent to"
        );
        let by_name =
            table("{ to = \"b@x.org\", attachments = { name = { contains = \".ZIP\" } } }");
        assert_eq!(names(&draft_of(&by_name, &raw)), ["big.zip"]);
        let none = table("{ to = \"b@x.org\", attachments = false }");
        let draft = draft_of(&none, &raw);
        assert!(draft.attachments.is_empty());
        assert!(
            draft
                .text
                .contains("Not forwarded: invoice.pdf (100 B)\nNot forwarded: big.zip (2 KB)"),
            "{}",
            draft.text
        );
        let all =
            table("{ to = \"b@x.org\", from = \"Sales@example.com\", subject_prefix = \"\" }");
        let draft = draft_of(&all, &raw);
        assert_eq!(names(&draft), ["invoice.pdf", "big.zip"]);
        assert!(!draft.text.contains("Not forwarded"));
        assert_eq!(
            (draft.from.as_str(), draft.subject.as_str()),
            ("Sales@example.com", "Invoice 7")
        );
    }

    fn draft_of(forward: &Forward, raw: &[u8]) -> Draft {
        draft(forward, raw, &identity(), u64::MAX).unwrap().unwrap()
    }

    #[test]
    fn over_max_size_skips_or_strips() {
        let id = identity();
        let raw = with_attachments();
        let skip = table("{ to = \"b@x.org\", max_size = \"1KB\" }");
        assert_eq!(
            draft(&skip, &raw, &id, u64::MAX).unwrap(),
            Err(Skip::OverSize)
        );
        let default = table("{ to = \"b@x.org\" }");
        assert_eq!(
            draft(&default, &raw, &id, 1024).unwrap(),
            Err(Skip::OverSize),
            "the default limit holds too"
        );
        let strip = table(
            "{ to = \"b@x.org\", as = \"attachment\", max_size = { limit = \"1KB\", over = \"strip\" } }",
        );
        let draft = draft(&strip, &raw, &id, u64::MAX).unwrap().unwrap();
        assert!(draft.attachments.is_empty() && draft.enclosed.is_none());
        assert!(
            draft.text.contains("Not forwarded: invoice.pdf (100 B)"),
            "{}",
            draft.text
        );
        let whole = table("{ to = \"b@x.org\", as = \"attachment\" }");
        assert!(draft_of(&whole, &raw).enclosed.is_some());
    }

    #[test]
    fn size_text_rounds_down_in_powers_of_1024() {
        assert_eq!(size_text(1023), "1023 B");
        assert_eq!(size_text(40 << 20), "40 MB");
        assert_eq!(size_text(1536), "1 KB");
    }

    #[test]
    fn once_key_uses_message_id_else_folder_uidvalidity_uid() {
        let mut msg = crate::store::Message {
            folder: "INBOX".into(),
            uid: 7,
            message_id: Some("<a@x.org>".into()),
            ..Default::default()
        };
        assert_eq!(once_key(&msg, 42), "id:<a@x.org>");
        msg.message_id = Some("  ".into());
        assert_eq!(once_key(&msg, 42), "uid:INBOX/42/7");
        msg.message_id = None;
        assert_eq!(once_key(&msg, 42), "uid:INBOX/42/7");
    }

    #[test]
    fn forward_parses_with_defaults() {
        let forward = forward_of(&rule("", "{ to = \"a@x.org\" }"));
        assert_eq!(forward.to, ["a@x.org"]);
        assert_eq!(forward.subject_prefix, "Fwd: ");
        assert_eq!(forward.as_, ForwardAs::Inline);
        assert_eq!(forward.attachments, Attachments::Every(true));
        assert_eq!(forward.rate, 20);
        assert_eq!(forward.max_size, None);
    }

    #[test]
    fn forward_takes_lists_filters_and_size() {
        let invoices = "[[rules]]\nname = \"invoices\"\nmatch.from = { contains = \"billing@\" }\nmatch.has_part = \"application/pdf\"\nactions.archive = true\nactions.forward = { to = [\"books@example.org\", \"b@x.org\"], cc = \"c@x.org\", note = \"Invoice for the records\", attachments = { type = \"application/pdf\" } }\n";
        let forward = forward_of(invoices);
        assert_eq!(forward.to, ["books@example.org", "b@x.org"]);
        assert_eq!(forward.cc, ["c@x.org"]);
        assert_eq!(forward.note.as_deref(), Some("Invoice for the records"));
        assert_eq!(
            forward.attachments,
            Attachments::Matching(AttachmentMatch {
                type_: Some("application/pdf".into()),
                ..Default::default()
            })
        );
        let sized = forward_of(&rule(
            "",
            "{ to = \"a@x.org\", max_size = { limit = \"5MB\", over = \"strip\" } }",
        ));
        let max = sized.max_size.unwrap();
        assert_eq!(
            max.limit(),
            Some(5 << 20),
            "units are powers of 1024, as in larger_than"
        );
        assert_eq!(max.over(), OverSize::Strip);
        let bare = forward_of(&rule(
            "",
            "{ to = \"a@x.org\", max_size = \"1MB\", as = \"attachment\", attachments = false }",
        ));
        assert_eq!(
            bare.max_size.as_ref().map(|m| (m.limit(), m.over())),
            Some((Some(1 << 20), OverSize::Skip))
        );
        assert_eq!(bare.as_, ForwardAs::Attachment);
    }

    #[test]
    fn forward_refusals() {
        let cases = [
            (rule("", "{ to = [] }"), "needs an address"),
            (
                rule("", "{ to = \"not an address\" }"),
                "'not an address' is not an address",
            ),
            (
                rule("", "{ to = \"a@x.org\", bcc = \"nope\" }"),
                "'nope' is not an address",
            ),
            (
                rule(
                    "",
                    "{ to = \"a@x.org\", as = \"attachment\", attachments = { type = \"application/pdf\" } }",
                ),
                "cannot pick attachments",
            ),
            (
                rule("", "{ to = \"a@x.org\", from = \"me@x.org\" }"),
                "needs the rule's account",
            ),
            (
                rule(
                    "account = \"work\"\n",
                    "{ to = \"a@x.org\", from = \"*@x.org\" }",
                ),
                "is not an address",
            ),
            (
                rule(
                    "",
                    "{ to = \"a@x.org\", subject_prefix = \"Fwd\\r\\nBcc: x@y\" }",
                ),
                "control characters",
            ),
            (rule("", "{ to = \"a@x.org\", rate = 0 }"), "at least 1"),
            (
                rule("", "{ to = \"a@x.org\", max_size = \"huge\" }"),
                "max_size: expected a size",
            ),
            (
                rule("", "{ to = \"a@x.org\", attachments = {} }"),
                "needs type, name or max_size",
            ),
            (
                rule("", "{ to = \"a@x.org\", attachments = { type = \"pdf\" } }"),
                "expected a MIME type",
            ),
            (
                rule(
                    "",
                    "{ to = \"a@x.org\", attachments = { name = { contains = \"\" } } }",
                ),
                "must not be empty",
            ),
            (
                rule("", "{ to = \"a@x.org\", reply_to = \"b@x.org\" }"),
                "reply_to",
            ),
        ];
        for (text, wanted) in cases {
            let error = refusal(&text);
            assert!(error.contains(wanted), "{text}: {error}");
        }
        compile(
            &parse(&rule(
                "account = \"work\"\n",
                "{ to = \"a@x.org\", from = \"me@x.org\" }",
            ))
            .unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn forward_round_trips_through_from_list() {
        let file = parse(&rule("account = \"work\"\n", "{ to = [\"a@x.org\", \"b@x.org\"], from = \"me@x.org\", subject_prefix = \"\", rate = 5, max_size = { over = \"strip\" }, attachments = { name = { contains = \".pdf\" }, max_size = \"5MB\" } }")).unwrap();
        let list = &file.rules[0].actions;
        let table = Actions::from_list(list).unwrap();
        assert_eq!(&table.clone().into_list(), list);
        let doubled = [list.clone(), list.clone()].concat();
        assert_eq!(
            Actions::from_list(&doubled).unwrap_err(),
            "actions.forward is given twice"
        );
        let written = toml::to_string(&file).unwrap();
        assert_eq!(
            parse(&written).unwrap().rules[0].actions,
            *list,
            "{written}"
        );
        let single = toml::to_string(&parse(&rule("", "{ to = [\"a@x.org\"] }")).unwrap()).unwrap();
        assert!(single.contains("to = \"a@x.org\""), "{single}");
    }

    #[test]
    fn the_label_names_the_first_recipient_and_counts_the_others() {
        let one = forward_of(&rule("", "{ to = \"a@x.org\" }"));
        assert_eq!(Action::Forward(Box::new(one)).label(), "forward:a@x.org");
        let three = forward_of(&rule(
            "",
            "{ to = \"a@x.org\", cc = \"b@x.org\", bcc = \"c@x.org\" }",
        ));
        assert_eq!(three.label(), "forward:a@x.org (+2)");
    }

    #[test]
    fn a_changed_forward_changes_the_definition() {
        let definition = |forward: &str| {
            compile(&parse(&rule("", forward)).unwrap()).unwrap()[0]
                .definition
                .clone()
        };
        let base = definition("{ to = \"a@x.org\" }");
        assert_ne!(base, definition("{ to = [\"a@x.org\", \"b@x.org\"] }"));
        assert_ne!(base, definition("{ to = \"a@x.org\", rate = 21 }"));
        assert_ne!(base, definition("{ to = \"a@x.org\", note = \"x\" }"));
        assert_eq!(
            base,
            definition("{ to = [\"a@x.org\"], rate = 20 }"),
            "defaults written out are the same rule"
        );
    }
}
