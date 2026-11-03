//! The `forward` action: its table in rules.toml and the checks it gets when a rule compiles.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Matcher, TextMatch, mime_pattern, parse_size};
use crate::send::reply::ForwardAs;

pub const DEFAULT_PREFIX: &str = "Fwd: ";
pub const DEFAULT_RATE: u32 = 20;

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
#[allow(dead_code)]
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
    /// The limit in bytes, `None` for the default; checked when the rule compiled.
    pub fn limit(&self) -> Option<u64> {
        match self {
            MaxSize::Limit(text) => parse_size(text),
            MaxSize::Table(table) => table.limit.as_deref().and_then(parse_size),
        }
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

    /// `forward:<first recipient>`, with ` (+N)` for the others: the action's label and its action log row.
    pub fn label(&self) -> String {
        let mut all = self.recipients();
        let first = all.next().map(String::as_str).unwrap_or_default();
        match all.count() {
            0 => format!("forward:{first}"),
            others => format!("forward:{first} (+{others})"),
        }
    }

    /// Refused when the table cannot be sent as written; `account` is the rule's.
    pub(super) fn check(&self, account: Option<&str>) -> Result<(), String> {
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
        if let Some(max) = &self.max_size {
            let text = match max {
                MaxSize::Limit(text) => Some(text),
                MaxSize::Table(table) => table.limit.as_ref(),
            };
            if let Some(text) = text
                && parse_size(text).is_none()
            {
                return Err(format!(
                    "actions.forward.max_size: expected a size such as 500KB or 5MB, got '{text}'"
                ));
            }
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
