pub mod apply;
pub mod coverage;
pub mod edit;

pub use edit::{list_toml, rule_toml};
pub mod engine;
pub mod hits;
pub mod lists;
mod warnings;

pub use lists::{Domain, ListProposal, ListRef, Lists, NamedList};

pub use warnings::{
    AccountFolders, authenticated_warnings, authserv_offers, check_warnings, open_folder_warnings,
    preview_warnings, store_warnings, warnings, watched_folder_warnings,
};

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum RulesError {
    #[error("rules.toml: {0}")]
    Parse(String),
    #[error("rule '{rule}': {reason}")]
    Invalid { rule: String, reason: String },
    #[error("list '{list}': {reason}")]
    List { list: String, reason: String },
    #[error("no rule named '{0}'")]
    NotFound(String),
    /// A batch of proposals refused as a whole; each failing rule is named by its index.
    #[error("nothing proposed: {0}")]
    Batch(String),
    #[error("rule '{0}' is disabled; approve or enable it first")]
    Disabled(String),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Store(#[from] crate::store::StoreError),
    #[error(transparent)]
    Config(#[from] crate::config::ConfigError),
}

impl From<toml_edit::TomlError> for RulesError {
    fn from(e: toml_edit::TomlError) -> Self {
        RulesError::Parse(e.to_string())
    }
}

pub(crate) fn invalid(rule: &str, reason: impl Into<String>) -> RulesError {
    RulesError::Invalid {
        rule: rule.to_string(),
        reason: reason.into(),
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RuleFile {
    /// Named values that conditions refer to with { list = "name" }, so several rules share one copy
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub lists: Lists,
    /// New lists and changes to lists that an agent proposed; each changes nothing until a human approves it
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub list_proposals: Vec<ListProposal>,
    #[serde(default)]
    pub rules: Vec<Rule>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    /// Unique name; renaming a rule restarts its clock
    pub name: String,
    /// Only for this account; default all accounts
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    /// The folder the rule watches; default INBOX
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    /// Disabled rules are skipped; proposals start disabled
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Who proposed the rule; set by `rules propose` and removed when a human approves it
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposed_by: Option<String>,
    /// The rule this proposal changes once approved; set by `rules propose --edit`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaces: Option<String>,
    /// Conditions that must all hold; at least one
    #[serde(rename = "match")]
    pub matches: Match,
    /// What to do; at least one
    #[serde(with = "action_table")]
    #[schemars(with = "Actions")]
    pub actions: Vec<Action>,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Match {
    /// The From header
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<TextMatch>,
    /// A From display name, unquoted, never the address; a From without one never matches
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_name: Option<TextMatch>,
    /// The To header
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<TextMatch>,
    /// The Cc header
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cc: Option<TextMatch>,
    /// The subject
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<TextMatch>,
    /// The plain-text body; HTML mail is converted
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<TextMatch>,
    /// Headers by name, such as List-Id, each with its own text condition; every header named must match. Of a repeated
    /// header only the last copy counts, the one nearest the body
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<BTreeMap<String, TextMatch>>,
    /// The From address's domain or any subdomain of it, never the display name; a list matches any of them
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_domain: Option<OneOrMany<Domain>>,
    /// The domain, or any subdomain, of an address in the To header, not Cc; a list matches any of them
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_domain: Option<OneOrMany<Domain>>,
    /// An Authentication-Results header from a server in the account's trusted_authserv_ids says DMARC passed for the From domain, or without a DMARC verdict, DKIM passed for a domain aligned with it; never true without that list
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authenticated: Option<bool>,
    /// Arrived at least this long ago: 30min, 1h, 2d, 3w, 6mo or 1y; a bare m is refused as ambiguous
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub older_than: Option<String>,
    /// Arrived less than this long ago, the opposite of older_than: holds mail back while it is new
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub younger_than: Option<String>,
    /// Arrived on one of these local weekdays: mon-fri, sat,sun or fri-mon, each day by its first three letters or in
    /// full
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arrived_days: Option<String>,
    /// Arrived between these local times, such as 09:00-18:00, the end not included; 22:00-07:00 wraps past midnight.
    /// Read apart from arrived_days: with fri, mail from Friday 03:00 matches and from Saturday 03:00 does not
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arrived_hours: Option<String>,
    /// Larger than this size, e.g. 500KB, 5MB; a bare number is bytes and 1KB is 1024 bytes
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub larger_than: Option<String>,
    /// Read (true) or unread (false)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read: Option<bool>,
    /// Flagged (true) or not (false)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flagged: Option<bool>,
    /// Your address or an alias is among the recipients: To, Cc or Delivered-To
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_me: Option<bool>,
    /// An address among the recipients (To, Cc or Delivered-To) is this alias; * matches any run of characters, and
    /// nothing else is special: it is no regex
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    /// Carries this IMAP keyword, which Thunderbird shows as a tag (e.g. $label1); case-insensitive
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    /// Holds when no entry holds; an entry holds when all its conditions do
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub none: Option<Vec<Match>>,
    /// Holds when at least one entry holds; an entry holds when all its conditions do
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub any: Option<Vec<Match>>,
}

/// One value, or a list of them
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged, expecting = "one value or a list of them")]
pub enum OneOrMany<T> {
    One(T),
    Many(Vec<T>),
}

impl<T> OneOrMany<T> {
    pub fn as_slice(&self) -> &[T] {
        match self {
            OneOrMany::One(one) => std::slice::from_ref(one),
            OneOrMany::Many(many) => many,
        }
    }
}

/// Exactly one of contains, equals, regex, list
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TextMatch {
    /// Case-insensitive substring; a list matches any of them
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contains: Option<OneOrMany<String>>,
    /// The whole value, case-insensitive; on from, to and cc also any single address; a list matches any of them
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub equals: Option<OneOrMany<String>>,
    /// Rust regex syntax, case-insensitive like the others; (?-i) makes it case-sensitive
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regex: Option<String>,
    /// The name of a text list in [lists], whose condition this is
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub list: Option<String>,
}

/// What a rule does, each at most once and in any order: flags and tags are set before a move, and a delete wins over
/// the rest
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Actions {
    /// true moves the message to the server's Archive folder
    #[serde(default, skip_serializing_if = "Option::is_none")]
    archive: Option<bool>,
    /// true backs the message up as .eml locally, then removes it from the server; later rules don't run
    #[serde(default, skip_serializing_if = "Option::is_none")]
    delete: Option<bool>,
    /// true flags the message, false unflags it
    #[serde(default, skip_serializing_if = "Option::is_none")]
    flagged: Option<bool>,
    /// true tags the message $Junk, which trains server spam filters, and moves it to the server's Junk folder; other
    /// actions but delete don't run
    #[serde(default, skip_serializing_if = "Option::is_none")]
    junk: Option<bool>,
    /// Moves the message to this folder, creating it if needed; {subaddress} is the +tag of your address the message
    /// was sent to, and without a tag of letters, digits, - and _ the message is not moved
    #[serde(default, rename = "move", skip_serializing_if = "Option::is_none")]
    move_to: Option<String>,
    /// true notifies even when the message was moved, false never notifies; the first matching rule that says wins
    #[serde(default, skip_serializing_if = "Option::is_none")]
    notify: Option<bool>,
    /// true marks the message read, false unread
    #[serde(default, skip_serializing_if = "Option::is_none")]
    read: Option<bool>,
    /// true: later rules don't run for this message
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stop: Option<bool>,
    /// Adds this IMAP keyword, or each of a list; Thunderbird shows them as tags (e.g. $label1)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tag: Option<OneOrMany<String>>,
    // Last, out of order: serde reads a list into a struct by position, so the old `actions = ["delete"]` stays refused.
    /// Waits before the other actions run: a span such as 15min or 3d after the message arrived, "read" until it is
    /// read, or a list of them, whichever comes first. Until then notify and stop already apply
    #[serde(default, skip_serializing_if = "Option::is_none")]
    after: Option<OneOrMany<String>>,
}

impl Actions {
    fn into_list(self) -> Vec<Action> {
        let when = |set: Option<bool>, action: Action| (set == Some(true)).then_some(action);
        let either =
            |set: Option<bool>, yes: Action, no: Action| set.map(|set| if set { yes } else { no });
        [
            when(self.delete, Action::Delete),
            when(self.archive, Action::Archive),
            when(self.junk, Action::Junk),
            self.move_to.map(Action::Move),
            either(self.read, Action::MarkRead, Action::MarkUnread),
            either(self.flagged, Action::Flag, Action::Unflag),
        ]
        .into_iter()
        .flatten()
        .chain(
            self.tag
                .iter()
                .flat_map(OneOrMany::as_slice)
                .map(|keyword| Action::Tag(keyword.clone())),
        )
        .chain(either(self.notify, Action::Notify, Action::Silent))
        .chain(when(self.stop, Action::Stop))
        .chain(
            self.after
                .iter()
                .flat_map(OneOrMany::as_slice)
                .map(|when| Action::After(when.clone())),
        )
        .collect()
    }

    /// The table that holds `list`; refused when the list says one thing twice or holds an action rules cannot take.
    pub fn from_list(list: &[Action]) -> Result<Actions, String> {
        let mut table = Actions::default();
        let mut tags = Vec::new();
        let mut after = Vec::new();
        for action in list {
            let (key, slot, value) = match action {
                Action::Archive => ("archive", &mut table.archive, true),
                Action::Delete => ("delete", &mut table.delete, true),
                Action::Flag => ("flagged", &mut table.flagged, true),
                Action::Junk => ("junk", &mut table.junk, true),
                Action::Unflag => ("flagged", &mut table.flagged, false),
                Action::MarkRead => ("read", &mut table.read, true),
                Action::MarkUnread => ("read", &mut table.read, false),
                Action::Notify => ("notify", &mut table.notify, true),
                Action::Silent => ("notify", &mut table.notify, false),
                Action::Stop => ("stop", &mut table.stop, true),
                Action::Move(folder) => {
                    if table.move_to.replace(folder.clone()).is_some() {
                        return Err("actions.move is given twice".into());
                    }
                    continue;
                }
                Action::Tag(keyword) => {
                    tags.push(keyword.clone());
                    continue;
                }
                Action::After(when) => {
                    after.push(when.clone());
                    continue;
                }
                Action::Trash | Action::NotJunk => {
                    return Err(format!("{} is not a rule action", action.label()));
                }
            };
            if slot.replace(value).is_some() {
                return Err(format!("actions.{key} is given twice"));
            }
        }
        if table.archive == Some(true) && table.move_to.is_some() {
            return Err("actions cannot both move and archive".into());
        }
        if table.junk == Some(true) && (table.archive == Some(true) || table.move_to.is_some()) {
            return Err("actions cannot both junk and move or archive".into());
        }
        table.tag = one_or_many(tags);
        table.after = one_or_many(after);
        Ok(table)
    }
}

fn one_or_many(mut values: Vec<String>) -> Option<OneOrMany<String>> {
    match values.len() {
        0 => None,
        1 => values.pop().map(OneOrMany::One),
        _ => Some(OneOrMany::Many(values)),
    }
}

/// `actions` is a table in rules.toml and a list in memory, the form the engine and the window's editor work with.
mod action_table {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use super::{Action, Actions};

    pub fn serialize<S: Serializer>(list: &[Action], serializer: S) -> Result<S::Ok, S::Error> {
        Actions::from_list(list)
            .map_err(serde::ser::Error::custom)?
            .serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<Action>, D::Error> {
        Actions::deserialize(deserializer).map(Actions::into_list)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Back up as .eml locally, then remove from the server; stops later rules
    Delete,
    /// Set \Seen
    MarkRead,
    /// Set \Flagged
    Flag,
    /// Move to the server's Archive folder
    Archive,
    /// Notify even when the message was moved
    Notify,
    /// Never notify
    Silent,
    /// Later rules don't run for this message
    Stop,
    MarkUnread,
    Unflag,
    /// A user delete: to the Trash folder, or deleted with a backup when there is none or the message is already in it.
    Trash,
    /// A user report: tagged $Junk, which trains server spam filters, and moved to the Junk folder.
    Junk,
    /// Undoes a wrong `Junk`: tagged $NotJunk and moved to INBOX.
    NotJunk,
    /// Move to this folder, creating it if needed
    Move(String),
    /// Add this IMAP keyword, which Thunderbird shows as a tag (e.g. $label1)
    Tag(String),
    /// The rule's other actions wait until this span after arrival, such as 15min, or until the message is read
    /// (`read`); of several, the first that comes
    After(String),
}

impl Action {
    /// Whether `after` delays this action: everything but `after` itself and `notify` and `stop`, which hold at once.
    pub fn waits_with_after(&self) -> bool {
        !matches!(
            self,
            Action::After(_) | Action::Notify | Action::Silent | Action::Stop
        )
    }

    /// The label of the command a user gave for this action: `delete` sends `Trash` outside the Trash folder.
    pub fn command_label(&self) -> String {
        match self {
            Action::Trash => Action::Delete.label(),
            _ => self.label(),
        }
    }

    /// The action in the words of a rule's `actions` table: a bare key is `key = true`, `key:value` any other value.
    /// Actions that only users take, such as `trash`, have names of their own.
    pub fn label(&self) -> String {
        match self {
            Action::Delete => "delete".into(),
            Action::MarkRead => "read".into(),
            Action::Flag => "flagged".into(),
            Action::Archive => "archive".into(),
            Action::Notify => "notify".into(),
            Action::Silent => "notify:false".into(),
            Action::Stop => "stop".into(),
            Action::MarkUnread => "read:false".into(),
            Action::Unflag => "flagged:false".into(),
            Action::Trash => "trash".into(),
            Action::Junk => "junk".into(),
            Action::NotJunk => "not_junk".into(),
            Action::Move(folder) => format!("move:{folder}"),
            Action::Tag(keyword) => format!("tag:{keyword}"),
            Action::After(when) => format!("after:{when}"),
        }
    }
}

/// Whether `name` is who took a direct action rather than a rule: direct actions are logged under `cli`, `gui` or `mcp`,
/// alone or followed by `:` and who asked.
pub fn is_actor(name: &str) -> bool {
    let who = name.split_once(':').map_or(name, |(who, _)| who);
    matches!(who, "cli" | "gui" | "mcp")
}

fn default_true() -> bool {
    true
}

/// JSON Schema of rules.toml; a proposal for `rules propose` is one entry of `rules`.
pub fn schema() -> String {
    let schema = schemars::schema_for!(RuleFile);
    // Schema's own Serialize puts `$schema` and `title` first, unlike `{:#}` on its value. Serializing an
    // in-memory schema to a String can't fail: no I/O, and every map key is a string.
    #[allow(clippy::expect_used)]
    let text = serde_json::to_string_pretty(&schema).expect("a schema serializes");
    text + "\n"
}

pub fn parse(text: &str) -> Result<RuleFile, RulesError> {
    toml::from_str(text).map_err(|e| RulesError::Parse(format!("{e}{}", old_syntax_hint(text))))
}

/// How to rewrite a file that still lists actions, says `seen` or names a header with `name`.
fn old_syntax_hint(text: &str) -> &'static str {
    let old = text.lines().any(|line| {
        let line: String = line
            .split('#')
            .next()
            .unwrap_or_default()
            .split_whitespace()
            .collect();
        line.starts_with("actions=[")
            || line.starts_with("seen=")
            || line.starts_with("match.seen=")
            || line.contains("{seen=")
            || line.contains("header={name=")
            || line.contains("header=[{name=")
    });
    if old {
        "\nThis looks like the old rules syntax: `actions` is a table (`actions.move = \"Lists\"`, \
         `actions.read = true`, `actions.notify = false`), `seen` is `read`, and a header is keyed by its name \
         (`match.header.List-Id = { contains = \"x\" }`). `postvak guide rules` has the details."
    } else {
        ""
    }
}

pub fn load(path: &Path) -> Result<RuleFile, RulesError> {
    parse(&crate::paths::read_or_empty(path)?)
}

#[derive(Debug, Clone)]
pub(crate) enum Matcher {
    Contains(Vec<String>),
    Equals(Vec<String>),
    Regex(regex::Regex),
}

impl Matcher {
    pub(crate) fn is_match(&self, text: &str) -> bool {
        match self {
            Matcher::Contains(needles) => {
                let text = text.to_lowercase();
                needles.iter().any(|needle| text.contains(needle))
            }
            Matcher::Equals(wanted) => {
                let text = text.trim().to_lowercase();
                wanted.contains(&text)
            }
            Matcher::Regex(re) => re.is_match(text),
        }
    }

    /// For from, to and cc: `equals` also accepts any single bare address, so a display name or a second recipient
    /// does not stop the match.
    pub(crate) fn is_address_match(&self, field: &str) -> bool {
        match self {
            Matcher::Equals(wanted) => {
                self.is_match(field)
                    || crate::message::bare_addresses(field)
                        .iter()
                        .any(|address| wanted.contains(&address.to_lowercase()))
            }
            _ => self.is_match(field),
        }
    }

    fn build(
        rule: &str,
        path: &str,
        contains: &Option<OneOrMany<String>>,
        equals: &Option<OneOrMany<String>>,
        regex: &Option<String>,
    ) -> Result<Matcher, RulesError> {
        let values =
            |list: &Option<OneOrMany<String>>| list.as_ref().map(|l| l.as_slice().to_vec());
        let (contains, equals) = (values(contains), values(equals));
        let empty_list = [&contains, &equals]
            .into_iter()
            .flatten()
            .any(Vec::is_empty);
        if empty_list
            || [&contains, &equals]
                .into_iter()
                .flatten()
                .flatten()
                .chain(regex)
                .any(|value| value.trim().is_empty())
        {
            return Err(invalid(rule, format!("{path} must not be empty")));
        }
        match (contains, equals, regex) {
            (Some(c), None, None) => Ok(Matcher::Contains(
                c.iter().map(|value| value.to_lowercase()).collect(),
            )),
            (None, Some(e), None) => Ok(Matcher::Equals(
                e.iter().map(|value| value.trim().to_lowercase()).collect(),
            )),
            (None, None, Some(r)) => regex::RegexBuilder::new(r)
                .case_insensitive(true)
                .build()
                .map(Matcher::Regex)
                .map_err(|e| invalid(rule, format!("{path}: invalid regex: {e}"))),
            _ => Err(invalid(
                rule,
                format!("{path} needs exactly one of contains, equals, regex"),
            )),
        }
    }
}

#[derive(Debug, Clone)]
pub struct CompiledRule {
    pub rule: Rule,
    pub first_seen_at: i64,
    /// What the rule does, its lists resolved, as JSON: editing any of it, or a list it uses, restarts the rule's
    /// clock, so a widened rule never acts on older mail.
    pub definition: String,
    pub(crate) conditions: Conditions,
    /// What the rule's actions wait for, any one of them; empty when they run at once.
    pub(crate) after: Vec<Wait>,
}

/// One value of `actions.after`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Wait {
    Read,
    Span(crate::time::Span),
}

impl Wait {
    pub fn parse(text: &str) -> Result<Wait, String> {
        if text.trim().eq_ignore_ascii_case("read") {
            return Ok(Wait::Read);
        }
        crate::time::Span::parse(text)
            .map(Wait::Span)
            .map_err(|e| format!("{e}, or read"))
    }
}

/// A compiled `Match`: every set condition must hold.
#[derive(Debug, Clone, Default)]
pub(crate) struct Conditions {
    pub(crate) from: Option<Matcher>,
    pub(crate) from_name: Option<Matcher>,
    pub(crate) to: Option<Matcher>,
    pub(crate) cc: Option<Matcher>,
    pub(crate) subject: Option<Matcher>,
    pub(crate) body: Option<Matcher>,
    pub(crate) headers: Vec<(String, Matcher)>,
    /// Lower-cased domains, without a leading `@` or a trailing dot.
    pub(crate) from_domain: Option<Vec<String>>,
    pub(crate) to_domain: Option<Vec<String>>,
    pub(crate) authenticated: Option<bool>,
    pub(crate) older_than: Option<crate::time::Span>,
    pub(crate) younger_than: Option<crate::time::Span>,
    pub(crate) arrived_days: Option<crate::time::Weekdays>,
    pub(crate) arrived_hours: Option<crate::time::Hours>,
    pub(crate) larger_than: Option<u64>,
    pub(crate) read: Option<bool>,
    pub(crate) flagged: Option<bool>,
    pub(crate) to_me: Option<bool>,
    pub(crate) alias: Option<regex::Regex>,
    pub(crate) tag: Option<String>,
    pub(crate) none: Vec<Conditions>,
    pub(crate) any: Vec<Conditions>,
}

impl Conditions {
    fn needs_body(&self) -> bool {
        self.body.is_some()
            || self.none.iter().any(Conditions::needs_body)
            || self.any.iter().any(Conditions::needs_body)
    }

    fn matches_header(&self) -> bool {
        !self.headers.is_empty()
            || self.none.iter().any(Conditions::matches_header)
            || self.any.iter().any(Conditions::matches_header)
    }
}

/// `name`, or `INBOX` for INBOX in any case: RFC 3501 5.1 makes that one name case-insensitive, and servers list it as
/// `INBOX`. Other names keep their case, since `Lists` and `lists` can be two folders.
pub fn inbox_spelled(name: &str) -> &str {
    if name.eq_ignore_ascii_case("INBOX") {
        "INBOX"
    } else {
        name
    }
}

impl CompiledRule {
    pub fn folder(&self) -> &str {
        inbox_spelled(self.rule.folder.as_deref().unwrap_or("INBOX"))
    }

    pub fn applies_to_account(&self, account: &str) -> bool {
        self.rule.account.as_deref().is_none_or(|a| a == account)
    }

    pub fn needs_body(&self) -> bool {
        self.conditions.needs_body()
    }

    /// Whether a `header` condition, at any depth, decides the match; `from`, `to`, `cc` and `subject` are not counted.
    pub fn matches_header(&self) -> bool {
        self.conditions.matches_header()
    }
}

/// Longest rule name, account or folder; agents propose rules, so no field may grow without bound.
const MAX_NAME_CHARS: usize = 255;

/// A name, account, folder or list name is not blank, holds no control characters and has at most `MAX_NAME_CHARS`.
fn check_name(field: &str, value: &str) -> Result<(), String> {
    if value.chars().any(char::is_control) {
        return Err(format!("{field} must not contain control characters"));
    }
    if value.trim().is_empty() {
        return Err(format!("{field} must not be empty"));
    }
    if value.chars().count() > MAX_NAME_CHARS {
        return Err(format!(
            "{field} must be at most {MAX_NAME_CHARS} characters"
        ));
    }
    Ok(())
}

/// Deepest nesting of `none` and `any`; Thunderbird's filters need two levels at most.
const MAX_DEPTH: usize = 8;

/// The compiled rule called `name`, refused when absent or disabled. It keeps `first_seen_at` 0, so it also covers mail
/// that predates the rule.
pub fn enabled_rule(path: &Path, name: &str) -> Result<CompiledRule, RulesError> {
    let rule = compile(&load(path)?)?
        .into_iter()
        .find(|r| r.rule.name == name)
        .ok_or_else(|| RulesError::NotFound(name.to_string()))?;
    if !rule.rule.enabled {
        return Err(RulesError::Disabled(name.to_string()));
    }
    Ok(rule)
}

pub fn compile(file: &RuleFile) -> Result<Vec<CompiledRule>, RulesError> {
    lists::check(&file.lists)?;
    lists::check_proposals(&file.list_proposals)?;
    compile_with(&file.rules, &file.lists)
}

/// `rules` compiled against the lists of the rules file at `path`, as if they were in it: a proposal, or a rule
/// previewed on its own. The file is read only when a rule refers to a list, so a broken file stops no other preview.
pub fn compile_for(path: &Path, rules: &[Rule]) -> Result<Vec<CompiledRule>, RulesError> {
    let lists = match lists::used_by(rules).is_empty() {
        true => Lists::new(),
        false => load(path)?.lists,
    };
    compile_with(rules, &lists)
}

/// `rules` compiled against `lists`, which a whole file's `compile` has checked; a malformed list a rule refers to is
/// still refused, as the rule's own condition.
pub fn compile_with(rules: &[Rule], lists: &Lists) -> Result<Vec<CompiledRule>, RulesError> {
    let mut names = std::collections::HashSet::new();
    rules
        .iter()
        .map(|rule| compile_rule(rule, lists, &mut names))
        .collect()
}

fn compile_rule(
    rule: &Rule,
    lists: &Lists,
    names: &mut std::collections::HashSet<String>,
) -> Result<CompiledRule, RulesError> {
    if rule.name.trim().is_empty() {
        return Err(invalid(&rule.name, "name must not be empty"));
    }
    if !names.insert(rule.name.clone()) {
        return Err(invalid(&rule.name, "duplicate rule name"));
    }
    if rule.actions.is_empty() {
        return Err(invalid(&rule.name, "actions must not be empty"));
    }
    let actions =
        Actions::from_list(&rule.actions).map_err(|reason| invalid(&rule.name, reason))?;
    let strings = [
        ("name", Some(&rule.name)),
        ("account", rule.account.as_ref()),
        ("folder", rule.folder.as_ref()),
    ]
    .into_iter()
    .filter_map(|(field, value)| Some((field, value?)))
    .chain(rule.actions.iter().filter_map(|action| match action {
        Action::Move(folder) => Some(("move folder", folder)),
        _ => None,
    }));
    for (field, value) in strings {
        check_name(field, value).map_err(|reason| invalid(&rule.name, reason))?;
    }
    let folders = rule
        .folder
        .iter()
        .map(|folder| ("folder", folder, false))
        .chain(rule.actions.iter().filter_map(|action| match action {
            Action::Move(folder) => Some(("move folder", folder, true)),
            _ => None,
        }));
    for (field, folder, target) in folders {
        crate::folder_name::check_shape(folder, target)
            .map_err(|reason| invalid(&rule.name, format!("{field} {reason}")))?;
    }
    // In the first level the sender's tag could name any folder, Trash among them.
    if rule.actions.iter().any(|action| {
        matches!(action, Action::Move(folder)
            if folder.split('/').next().is_some_and(|level| level.contains(engine::SUBADDRESS)))
    }) {
        return Err(invalid(
            &rule.name,
            "move folder must keep {subaddress} below a folder of your own, such as Tags/{subaddress}",
        ));
    }
    for action in &rule.actions {
        if let Action::Tag(keyword) = action {
            check_keyword(keyword)
                .map_err(|reason| invalid(&rule.name, format!("tag {reason}")))?;
        }
    }
    let after = rule
        .actions
        .iter()
        .filter_map(|action| match action {
            Action::After(when) => Some(
                Wait::parse(when).map_err(|e| invalid(&rule.name, format!("actions.after: {e}"))),
            ),
            _ => None,
        })
        .collect::<Result<Vec<_>, _>>()?;
    if !after.is_empty() && !rule.actions.iter().any(Action::waits_with_after) {
        return Err(invalid(
            &rule.name,
            "actions.after needs an action to wait with, such as move or delete",
        ));
    }
    let resolved = lists::resolve(&rule.name, &rule.matches, lists, "match")?;
    let definition = serde_json::to_string(&(&rule.account, &rule.folder, &resolved, actions))
        .map_err(|e| invalid(&rule.name, e.to_string()))?;
    Ok(CompiledRule {
        rule: rule.clone(),
        first_seen_at: 0,
        definition,
        conditions: compile_match(&rule.name, &resolved, "match", 0)?,
        after,
    })
}

fn compile_match(
    rule: &str,
    m: &Match,
    path: &str,
    depth: usize,
) -> Result<Conditions, RulesError> {
    if depth > MAX_DEPTH {
        return Err(invalid(
            rule,
            format!("{path} is nested deeper than {MAX_DEPTH}"),
        ));
    }
    let has_condition = m.from.is_some()
        || m.from_name.is_some()
        || m.to.is_some()
        || m.cc.is_some()
        || m.subject.is_some()
        || m.body.is_some()
        || m.header.as_ref().is_some_and(|headers| !headers.is_empty())
        || m.from_domain.is_some()
        || m.to_domain.is_some()
        || m.authenticated.is_some()
        || m.older_than.is_some()
        || m.younger_than.is_some()
        || m.arrived_days.is_some()
        || m.arrived_hours.is_some()
        || m.larger_than.is_some()
        || m.read.is_some()
        || m.flagged.is_some()
        || m.to_me.is_some()
        || m.alias.is_some()
        || m.tag.is_some()
        || m.none.is_some()
        || m.any.is_some();
    if !has_condition {
        let reason = match &m.header {
            Some(_) => format!("{path}.header must not be empty"),
            None => format!("{path} needs at least one condition"),
        };
        return Err(invalid(rule, reason));
    }
    if m.alias
        .as_ref()
        .is_some_and(|alias| alias.trim().is_empty())
    {
        return Err(invalid(rule, format!("{path}.alias must not be empty")));
    }
    if let Some(tag) = &m.tag {
        check_keyword(tag).map_err(|reason| invalid(rule, format!("{path}.tag {reason}")))?;
    }
    let text = |field: &str, t: &Option<TextMatch>| -> Result<Option<Matcher>, RulesError> {
        t.as_ref()
            .map(|t| {
                Matcher::build(
                    rule,
                    &format!("{path}.{field}"),
                    &t.contains,
                    &t.equals,
                    &t.regex,
                )
            })
            .transpose()
    };
    let headers = m
        .header
        .iter()
        .flatten()
        .map(|(name, t)| {
            if name.trim().is_empty() {
                return Err(invalid(rule, format!("{path}.header needs a header name")));
            }
            Matcher::build(
                rule,
                &format!("{path}.header.{name}"),
                &t.contains,
                &t.equals,
                &t.regex,
            )
            .map(|matcher| (name.clone(), matcher))
        })
        .collect::<Result<_, _>>()?;
    let span = |key: &str, text: &Option<String>| {
        text.as_deref()
            .map(|text| {
                crate::time::Span::parse(text)
                    .map_err(|e| invalid(rule, format!("{path}.{key}: {e}")))
            })
            .transpose()
    };
    let arrived_days = m
        .arrived_days
        .as_deref()
        .map(|text| {
            crate::time::Weekdays::parse(text)
                .map_err(|e| invalid(rule, format!("{path}.arrived_days: {e}")))
        })
        .transpose()?;
    let arrived_hours = m
        .arrived_hours
        .as_deref()
        .map(|text| {
            crate::time::Hours::parse(text)
                .map_err(|e| invalid(rule, format!("{path}.arrived_hours: {e}")))
        })
        .transpose()?;
    let larger_than = m
        .larger_than
        .as_deref()
        .map(|text| {
            parse_size(text).ok_or_else(|| {
                invalid(
                    rule,
                    format!(
                        "{path}.larger_than: expected a size such as 500KB or 5MB, got '{text}'"
                    ),
                )
            })
        })
        .transpose()?;
    let alias = m
        .alias
        .as_ref()
        .map(|pattern| {
            regex::Regex::new(&crate::config::alias_regex(pattern))
                .map_err(|e| invalid(rule, format!("{path}.alias: {e}")))
        })
        .transpose()?;
    let branches = |key: &str, list: &Option<Vec<Match>>| -> Result<Vec<Conditions>, RulesError> {
        match list.as_deref() {
            None => Ok(Vec::new()),
            Some([]) => Err(invalid(rule, format!("{path}.{key} must not be empty"))),
            Some(entries) => entries
                .iter()
                .enumerate()
                .map(|(i, entry)| {
                    compile_match(rule, entry, &format!("{path}.{key}[{i}]"), depth + 1)
                })
                .collect(),
        }
    };
    let domains = |key: &str, list: &Option<OneOrMany<Domain>>| {
        let path = format!("{path}.{key}");
        list.as_ref()
            .map(|list| {
                let names = list
                    .as_slice()
                    .iter()
                    .map(|domain| match domain {
                        Domain::Name(name) => Ok(name.clone()),
                        Domain::List(ListRef { list }) => Err(invalid(
                            rule,
                            format!("{path}: list '{list}' is not resolved"),
                        )),
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                domain_list(rule, &path, &names)
            })
            .transpose()
    };
    let any = branches("any", &m.any)?;
    let none = branches("none", &m.none)?;
    Ok(Conditions {
        from: text("from", &m.from)?,
        from_name: text("from_name", &m.from_name)?,
        to: text("to", &m.to)?,
        cc: text("cc", &m.cc)?,
        subject: text("subject", &m.subject)?,
        body: text("body", &m.body)?,
        headers,
        from_domain: domains("from_domain", &m.from_domain)?,
        to_domain: domains("to_domain", &m.to_domain)?,
        authenticated: m.authenticated,
        older_than: span("older_than", &m.older_than)?,
        younger_than: span("younger_than", &m.younger_than)?,
        arrived_days,
        arrived_hours,
        larger_than,
        read: m.read,
        flagged: m.flagged,
        to_me: m.to_me,
        alias,
        tag: m.tag.clone(),
        none,
        any,
    })
}

/// The domains of a `from_domain` or `to_domain`, lower-cased; a leading `@` and a trailing dot are dropped, so
/// `@PayPal.com.` is `paypal.com`. Refused when empty or when a value is no domain.
fn domain_list(rule: &str, path: &str, values: &[String]) -> Result<Vec<String>, RulesError> {
    if values.is_empty() {
        return Err(invalid(rule, format!("{path} must not be empty")));
    }
    values
        .iter()
        .map(|value| {
            let domain = value
                .trim()
                .trim_start_matches('@')
                .trim_end_matches('.')
                .to_lowercase();
            if domain.is_empty() {
                return Err(invalid(rule, format!("{path} must not be empty")));
            }
            let bad = |c: char| c == '@' || c.is_whitespace() || c.is_control();
            if domain.contains(bad) || domain.starts_with('.') || domain.contains("..") {
                return Err(invalid(
                    rule,
                    format!(
                        "{path}: '{}' is not a domain such as example.com",
                        value.escape_default()
                    ),
                ));
            }
            if domain.chars().count() > MAX_NAME_CHARS {
                return Err(invalid(
                    rule,
                    format!("{path} must be at most {MAX_NAME_CHARS} characters"),
                ));
            }
            Ok(domain)
        })
        .collect()
}

/// Bytes in `500`, `500B`, `2KB`, `5 MB` or `1GB`, ignoring case; units are powers of 1024, as in Thunderbird.
fn parse_size(text: &str) -> Option<u64> {
    let text = text.trim();
    let digits_end = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(digits_end);
    let shift = match unit.trim().to_ascii_uppercase().as_str() {
        "" | "B" => 0,
        "KB" => 10,
        "MB" => 20,
        "GB" => 30,
        _ => return None,
    };
    number.parse::<u64>().ok()?.checked_mul(1 << shift)
}

/// An IMAP keyword is an atom (RFC 3501): printable ASCII without specials. It goes into the STORE command verbatim, so
/// this check is what keeps a proposed tag from injecting IMAP syntax.
pub(crate) fn check_keyword(keyword: &str) -> Result<(), String> {
    const ATOM_SPECIALS: &[char] = &['(', ')', '{', ' ', '%', '*', '"', '\\', ']'];
    if keyword.is_empty() {
        return Err("must not be empty".into());
    }
    if keyword.len() > MAX_NAME_CHARS {
        return Err(format!("must be at most {MAX_NAME_CHARS} characters"));
    }
    if !keyword
        .chars()
        .all(|c| c.is_ascii_graphic() && !ATOM_SPECIALS.contains(&c))
    {
        return Err("must be printable ASCII without spaces, backslashes or ( ) { % * \" ]".into());
    }
    if keyword.eq_ignore_ascii_case(engine::RESTORED_KEYWORD) {
        return Err("is reserved for restored mail".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actor_names_are_the_names_direct_actions_log_under() {
        for name in ["cli", "cli:me", "gui", "mcp", "mcp:claude-desktop"] {
            assert!(is_actor(name), "{name}");
        }
        for name in ["travel", "client", "guide", "mcpx"] {
            assert!(!is_actor(name), "{name}");
        }
    }

    #[test]
    fn schema_file_is_current() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../docs/src/rules.schema.json"
        );
        if std::env::var_os("POSTVAK_BLESS").is_some() {
            std::fs::write(path, schema()).unwrap();
        }
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            schema(),
            "docs/src/rules.schema.json is stale; run POSTVAK_BLESS=1 cargo test"
        );
    }

    #[test]
    fn schema_describes_rules_and_rejects_unknown_keys() {
        let text = schema();
        let schema: serde_json::Value = serde_json::from_str(&text).unwrap();
        let rule = &schema["$defs"]["Rule"];
        assert_eq!(rule["additionalProperties"], false);
        assert!(rule["properties"]["match"].is_object());
        let keys: Vec<&String> = schema["$defs"]["Actions"]["properties"]
            .as_object()
            .unwrap()
            .keys()
            .collect();
        assert_eq!(
            keys,
            [
                "after", "archive", "delete", "flagged", "junk", "move", "notify", "read", "stop",
                "tag"
            ],
            "actions only users take stay out of the schema"
        );
    }

    pub(crate) const SAMPLE: &str = r#"
[[rules]]
name = "purge sign-in codes"
account = "work"
match.from = { regex = "no-?reply@" }
match.subject = { regex = "sign.?in|verification code|magic link" }
match.older_than = "1h"
match.read = true
actions = { delete = true }

[[rules]]
name = "github to folder"
match.header.List-Id = { contains = "github.com" }
actions = { move = "Lists/GitHub", read = true }

[[rules]]
name = "shop alias"
enabled = false
proposed_by = "cli:test"
match.alias = "*@shop.example.com"
actions = { move = "Shopping", notify = true }
"#;

    #[test]
    fn parses_sample() {
        let file = parse(SAMPLE).unwrap();
        assert_eq!(file.rules.len(), 3);
        let github = &file.rules[1];
        assert_eq!(
            github.actions,
            vec![Action::Move("Lists/GitHub".into()), Action::MarkRead]
        );
        assert_eq!(
            github
                .matches
                .header
                .as_ref()
                .unwrap()
                .keys()
                .collect::<Vec<_>>(),
            ["List-Id"]
        );
        assert!(!file.rules[2].enabled);
        assert_eq!(file.rules[2].proposed_by.as_deref(), Some("cli:test"));
    }

    #[test]
    fn compile_sets_defaults() {
        let compiled = compile(&parse(SAMPLE).unwrap()).unwrap();
        assert_eq!(compiled[0].folder(), "INBOX");
        assert!(compiled[0].applies_to_account("work"));
        assert!(!compiled[0].applies_to_account("home"));
        assert!(compiled[1].applies_to_account("anything"));
        assert!(!compiled[0].needs_body());
        assert_eq!(
            compiled[0].conditions.older_than,
            Some(crate::time::Span::Seconds(3600))
        );
    }

    #[test]
    fn actions_are_a_table_whose_false_values_undo() {
        let file = parse(
            "[[rules]]\nname = \"x\"\nmatch.read = true\nactions = { read = false, flagged = false, tag = [\"$a\", \"$b\"], notify = false, stop = true, delete = false }\n",
        )
        .unwrap();
        assert_eq!(
            file.rules[0].actions,
            [
                Action::MarkUnread,
                Action::Unflag,
                Action::Tag("$a".into()),
                Action::Tag("$b".into()),
                Action::Silent,
                Action::Stop,
            ]
        );
        assert_eq!(
            parse(&toml::to_string(&file).unwrap()).unwrap().rules[0].actions,
            file.rules[0].actions
        );
    }

    #[test]
    fn actions_refuse_unknown_keys_and_what_a_table_cannot_say() {
        let rule =
            |actions: &str| format!("[[rules]]\nname = \"x\"\nmatch.read = true\n{actions}\n");
        assert!(parse(&rule("actions = { mark_read = true }")).is_err());
        assert!(parse(&rule("actions = [\"delete\"]")).is_err());
        let refused = |actions: Vec<Action>| {
            let mut file = parse(&rule("actions = { flagged = true }")).unwrap();
            file.rules[0].actions = actions;
            match compile(&file) {
                Err(RulesError::Invalid { reason, .. }) => reason,
                other => panic!("{other:?}"),
            }
        };
        assert_eq!(
            refused(vec![Action::Move("A".into()), Action::Move("B".into())]),
            "actions.move is given twice"
        );
        assert_eq!(
            refused(vec![Action::Notify, Action::Silent]),
            "actions.notify is given twice"
        );
        assert_eq!(
            refused(vec![Action::Archive, Action::Move("A".into())]),
            "actions cannot both move and archive"
        );
        assert_eq!(
            refused(vec![Action::Junk, Action::Archive]),
            "actions cannot both junk and move or archive"
        );
        assert_eq!(
            refused(vec![Action::NotJunk]),
            "not_junk is not a rule action"
        );
        assert_eq!(refused(Vec::new()), "actions must not be empty");
    }

    #[test]
    fn junk_is_a_rule_action() {
        let file = parse(
            "[[rules]]\nname = \"x\"\nmatch.read = true\nactions = { junk = true, read = true }\n",
        )
        .unwrap();
        assert_eq!(file.rules[0].actions, [Action::Junk, Action::MarkRead]);
        compile(&file).unwrap();
        assert!(toml::to_string(&file).unwrap().contains("junk = true"));
    }

    #[test]
    fn rejects_bad_regex_with_rule_name() {
        let bad = SAMPLE.replace("no-?reply@", "(unclosed");
        match parse(&bad).and_then(|f| compile(&f)) {
            Err(RulesError::Invalid { rule, reason }) => {
                assert_eq!(rule, "purge sign-in codes");
                assert!(reason.contains("regex"), "{reason}");
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn rejects_text_match_without_exactly_one_operator() {
        let two = SAMPLE.replace(
            r#"{ regex = "no-?reply@" }"#,
            r#"{ regex = "a", contains = "b" }"#,
        );
        assert!(matches!(
            parse(&two).and_then(|f| compile(&f)),
            Err(RulesError::Invalid { .. })
        ));
        let none = SAMPLE.replace(r#"{ regex = "no-?reply@" }"#, "{ }");
        assert!(matches!(
            parse(&none).and_then(|f| compile(&f)),
            Err(RulesError::Invalid { .. })
        ));
    }

    #[test]
    fn rejects_rule_without_conditions_or_actions_or_unknown_action() {
        let empty_match = "[[rules]]\nname = \"x\"\nmatch = {}\nactions = { delete = true }\n";
        assert!(matches!(
            parse(empty_match).and_then(|f| compile(&f)),
            Err(RulesError::Invalid { .. })
        ));
        let no_actions = "[[rules]]\nname = \"x\"\nmatch.read = true\nactions = {}\n";
        assert!(matches!(
            parse(no_actions).and_then(|f| compile(&f)),
            Err(RulesError::Invalid { .. })
        ));
        let unknown = "[[rules]]\nname = \"x\"\nmatch.read = true\nactions.explode = true\n";
        assert!(matches!(parse(unknown), Err(RulesError::Parse(_))));
    }

    #[test]
    fn enabled_rule_finds_by_name_and_refuses_absent_or_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rules.toml");
        std::fs::write(&path, SAMPLE).unwrap();
        let rule = enabled_rule(&path, "github to folder").unwrap();
        assert_eq!(rule.rule.name, "github to folder");
        let error = |name| enabled_rule(&path, name).err().map(|e| e.to_string());
        assert_eq!(error("nope").as_deref(), Some("no rule named 'nope'"));
        assert_eq!(
            error("shop alias").as_deref(),
            Some("rule 'shop alias' is disabled; approve or enable it first")
        );
    }

    #[test]
    fn rejects_bad_duration_and_duplicate_names() {
        let bad = SAMPLE.replace("\"1h\"", "\"soon\"");
        assert!(matches!(
            parse(&bad).and_then(|f| compile(&f)),
            Err(RulesError::Invalid { .. })
        ));
        let dup = SAMPLE.replace(
            "name = \"github to folder\"",
            "name = \"purge sign-in codes\"",
        );
        assert!(matches!(
            parse(&dup).and_then(|f| compile(&f)),
            Err(RulesError::Invalid { .. })
        ));
    }

    #[test]
    fn load_missing_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            load(&dir.path().join("rules.toml"))
                .unwrap()
                .rules
                .is_empty()
        );
    }

    #[test]
    fn matcher_semantics() {
        assert!(Matcher::Contains(vec!["github".into()]).is_match("Lists GitHub Dev"));
        assert!(Matcher::Equals(vec!["a@b.c".into()]).is_match("A@B.C"));
        assert!(!Matcher::Equals(vec!["a@b.c".into()]).is_match("xa@b.c"));
        assert!(Matcher::Regex(regex::Regex::new("^no-?reply").unwrap()).is_match("noreply@x"));
    }

    #[test]
    fn equals_ignores_case_beyond_ascii() {
        let equals = |value: &str| {
            let wanted = Some(OneOrMany::One(value.to_string()));
            Matcher::build("r", "match.from", &None, &wanted, &None).unwrap()
        };
        assert!(equals("jürgen").is_match("JÜRGEN"));
        assert!(equals("JÜRGEN").is_match("jürgen"));
        assert!(equals("jürgen@x.de").is_address_match("Jürgen <JÜRGEN@x.de>"));
    }

    #[test]
    fn rejects_unknown_top_level_key() {
        let typo = "[[rule]]\nname = \"x\"\nmatch.read = true\nactions = { flagged = true }\n";
        assert!(matches!(parse(typo), Err(RulesError::Parse(_))));
    }

    #[test]
    fn rejects_empty_values() {
        let rule = |matcher: &str, action: &str| {
            format!("[[rules]]\nname = \"x\"\n{matcher}\nactions = {{ {action} }}\n")
        };
        let cases = [
            rule("match.subject = { contains = \"\" }", "delete = true"),
            rule("match.subject = { equals = \"  \" }", "delete = true"),
            rule("match.subject = { regex = \"\" }", "delete = true"),
            rule(
                "match.header = { \" \" = { contains = \"a\" } }",
                "delete = true",
            ),
            rule("match.read = true", "move = \"\""),
            rule("match.alias = \"\"", "delete = true"),
        ];
        for text in cases {
            assert!(
                matches!(
                    parse(&text).and_then(|f| compile(&f)),
                    Err(RulesError::Invalid { .. })
                ),
                "{text}"
            );
        }
    }

    #[test]
    fn rejects_blank_and_overlong_names_and_folders() {
        let long = "a".repeat(MAX_NAME_CHARS + 1);
        let rule = |extra: &str, name: &str, target: &str| {
            format!(
                "[[rules]]\nname = \"{name}\"\n{extra}match.read = true\nactions = {{ move = \"{target}\" }}\n"
            )
        };
        let cases = [
            (
                rule("folder = \"\"\n", "x", "Done"),
                "folder must not be empty",
            ),
            (
                rule("folder = \"   \"\n", "x", "Done"),
                "folder must not be empty",
            ),
            (
                rule("account = \" \"\n", "x", "Done"),
                "account must not be empty",
            ),
            (
                rule(&format!("folder = \"{long}\"\n"), "x", "Done"),
                "folder must be at most 255",
            ),
            (rule("", &long, "Done"), "name must be at most 255"),
            (rule("", "x", &long), "move folder must be at most 255"),
        ];
        for (text, want) in cases {
            match parse(&text).and_then(|f| compile(&f)) {
                Err(RulesError::Invalid { reason, .. }) => {
                    assert!(reason.contains(want), "{reason} should contain {want}")
                }
                other => panic!("expected Invalid ({want}), got {other:?}"),
            }
        }
        // Exactly at the limit is fine.
        let at_limit = "a".repeat(MAX_NAME_CHARS);
        compile(&parse(&rule("", &at_limit, &at_limit)).unwrap()).unwrap();
    }

    #[test]
    fn rejects_folder_names_servers_refuse_or_misread() {
        let rule = |extra: &str, target: &str| {
            format!(
                "[[rules]]\nname = \"x\"\n{extra}match.read = true\nactions.move = \"{target}\"\n"
            )
        };
        let cases = [
            (rule("", "Lists/*"), "move folder must not contain * or %"),
            (rule("", "100%"), "move folder must not contain * or %"),
            (
                rule("", "Lists/"),
                "move folder must not have an empty level",
            ),
            (
                rule("", "/Lists"),
                "move folder must not have an empty level",
            ),
            (
                rule("", "Lists//GitHub"),
                "move folder must not have an empty level",
            ),
            (
                rule("", " Lists"),
                "move folder level ' Lists' must not start or end with whitespace",
            ),
            (
                rule("", "Lists "),
                "move folder level 'Lists ' must not start or end with whitespace",
            ),
            (
                rule("", "Lists / GitHub"),
                "move folder level 'Lists ' must not start or end with whitespace",
            ),
            (
                rule("", "{subaddress}"),
                "move folder must keep {subaddress} below a folder of your own, such as Tags/{subaddress}",
            ),
            (
                rule("", "x{subaddress}/y"),
                "move folder must keep {subaddress} below a folder of your own, such as Tags/{subaddress}",
            ),
            (
                rule("folder = \"Lists/ GitHub\"\n", "Done"),
                "folder level ' GitHub' must not start or end with whitespace",
            ),
        ];
        for (text, want) in cases {
            match parse(&text).and_then(|f| compile(&f)) {
                Err(RulesError::Invalid { reason, .. }) => {
                    assert!(
                        reason.starts_with(want),
                        "{reason} should start with {want}"
                    )
                }
                other => panic!("expected Invalid ({want}), got {other:?}"),
            }
        }
        // A watched folder may hold what a move target may not: it exists already, so the server accepted it.
        for ok in [
            rule("folder = \"100%\"\n", "Lists/GitHub"),
            rule("folder = \"Lists/\"\n", "Archief/\u{c9}\u{e9}n"),
            rule("", "Lists - Old/Q&A"),
        ] {
            compile(&parse(&ok).unwrap()).unwrap();
        }
    }

    #[test]
    fn rejects_control_characters_in_agent_strings() {
        let cases = [
            "[[rules]]\nname = \"x\"\nmatch.read = true\nactions = { move = \"Codes\\u001b[2J\" }\n",
            "[[rules]]\nname = \"x\\u001b]0;pwned\"\nmatch.read = true\nactions = { flagged = true }\n",
        ];
        for text in cases {
            match parse(text).and_then(|f| compile(&f)) {
                Err(RulesError::Invalid { reason, .. }) => {
                    assert!(reason.contains("control characters"), "{reason}")
                }
                other => panic!("expected Invalid for {text}, got {other:?}"),
            }
        }
    }

    const NESTED: &str = r#"
[[rules]]
name = "tagged"
match.from = { contains = "shop" }
match.none = [{ subject = { contains = ["receipt", "invoice"] } }]
match.any = [{ to_me = true }, { tag = "$label1" }]
match.header = { List-Id = { contains = "shop" }, X-Mailer = { contains = "bulk" } }
actions = { tag = "$label4", move = "Shop" }
"#;

    #[test]
    fn parses_none_any_tag_value_and_header_lists() {
        let file = parse(NESTED).unwrap();
        let rule = &file.rules[0];
        assert_eq!(
            rule.actions,
            vec![Action::Move("Shop".into()), Action::Tag("$label4".into())]
        );
        assert_eq!(rule.matches.any.as_ref().unwrap().len(), 2);
        assert_eq!(rule.matches.none.as_ref().unwrap().len(), 1);
        assert_eq!(rule.matches.header.as_ref().unwrap().len(), 2);
        let compiled = compile(&file).unwrap();
        assert_eq!(compiled[0].conditions.headers.len(), 2);
        let again = parse(&toml::to_string(&file).unwrap()).unwrap();
        assert_eq!(again.rules[0].actions, rule.actions);
        assert_eq!(again.rules[0].matches.any.as_ref().unwrap().len(), 2);
        assert_eq!(
            toml::to_string(&again).unwrap(),
            toml::to_string(&file).unwrap()
        );
    }

    #[test]
    fn single_header_table_still_parses() {
        let compiled = compile(&parse(SAMPLE).unwrap()).unwrap();
        assert_eq!(compiled[1].conditions.headers.len(), 1);
    }

    #[test]
    fn nested_matches_are_validated_with_their_path() {
        let rule = |matcher: &str| {
            format!("[[rules]]\nname = \"x\"\n{matcher}\nactions = {{ flagged = true }}\n")
        };
        let deep = format!(
            "match.none = {}{}",
            "[{ none = ".repeat(MAX_DEPTH),
            "[{ read = true }]".to_string() + &" }]".repeat(MAX_DEPTH)
        );
        let cases = [
            (
                rule("match.none = [{}]"),
                "match.none[0] needs at least one condition",
            ),
            (rule("match.none = []"), "match.none must not be empty"),
            (
                rule("match.subject = { contains = [] }"),
                "match.subject must not be empty",
            ),
            (
                rule("match.subject = { contains = [\"a\", \" \"] }"),
                "match.subject must not be empty",
            ),
            (rule("match.header = {}"), "match.header must not be empty"),
            (rule("match.any = []"), "match.any must not be empty"),
            (
                rule("match.any = [{}]"),
                "match.any[0] needs at least one condition",
            ),
            (
                rule("match.none = [{ subject = { regex = \"(\" } }]"),
                "match.none[0].subject: invalid regex",
            ),
            (
                rule("match.any = [{ read = true }, { from = { contains = \"\" } }]"),
                "match.any[1].from must not be empty",
            ),
            (rule(&deep), "nested deeper than"),
        ];
        for (text, want) in cases {
            match parse(&text).and_then(|f| compile(&f)) {
                Err(RulesError::Invalid { reason, .. }) => {
                    assert!(reason.contains(want), "{reason} should contain {want}")
                }
                other => panic!("expected Invalid ({want}), got {other:?}"),
            }
        }
    }

    #[test]
    fn tags_must_be_imap_keywords() {
        let action = |tag: &str| {
            format!("[[rules]]\nname = \"x\"\nmatch.read = true\nactions = {{ tag = \"{tag}\" }}\n")
        };
        let matcher = |tag: &str| {
            format!(
                "[[rules]]\nname = \"x\"\nmatch.tag = \"{tag}\"\nactions = {{ flagged = true }}\n"
            )
        };
        for bad in [
            "",
            "two words",
            "\\\\Seen",
            "a)b",
            "x\\r\\nA1 LOGOUT",
            "naïve",
            "$PostvakRestored",
            "$postvakRESTORED",
        ] {
            for text in [action(bad), matcher(bad)] {
                assert!(
                    matches!(
                        parse(&text).and_then(|f| compile(&f)),
                        Err(RulesError::Invalid { .. })
                    ),
                    "{text}"
                );
            }
        }
        for good in ["$label1", "Important", "$Junk", "todo-later"] {
            compile(&parse(&action(good)).unwrap()).unwrap();
            compile(&parse(&matcher(good)).unwrap()).unwrap();
        }
    }

    #[test]
    fn larger_than_takes_a_size_with_a_unit() {
        let rule = |size: &str| {
            format!(
                "[[rules]]\nname = \"x\"\nmatch.larger_than = \"{size}\"\nactions = {{ flagged = true }}\n"
            )
        };
        let size =
            |text: &str| compile(&parse(&rule(text)).unwrap()).map(|c| c[0].conditions.larger_than);
        assert_eq!(size("500").unwrap(), Some(500));
        assert_eq!(size("500B").unwrap(), Some(500));
        assert_eq!(size("2kb").unwrap(), Some(2048));
        assert_eq!(size("5 MB").unwrap(), Some(5 << 20));
        assert_eq!(size("1GB").unwrap(), Some(1 << 30));
        for bad in ["", "MB", "5TB", "-1MB", "1.5MB", "99999999999999999999"] {
            match size(bad) {
                Err(RulesError::Invalid { reason, .. }) => {
                    assert!(reason.contains("match.larger_than"), "{reason}")
                }
                other => panic!("expected Invalid for {bad:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn arrived_days_and_hours_are_checked_when_compiled() {
        let rule = |cond: &str| {
            format!("[[rules]]\nname = \"x\"\n{cond}\nactions = {{ flagged = true }}\n")
        };
        compile(
            &parse(&rule(
                "match.arrived_days = \"mon-fri\"\nmatch.arrived_hours = \"9:00-17:30\"",
            ))
            .unwrap(),
        )
        .unwrap();
        for (cond, key) in [
            ("match.arrived_days = \"weekend\"", "match.arrived_days"),
            ("match.arrived_hours = \"9-17\"", "match.arrived_hours"),
        ] {
            match compile(&parse(&rule(cond)).unwrap()) {
                Err(RulesError::Invalid { reason, .. }) => {
                    assert!(reason.contains(key), "{reason}")
                }
                other => panic!("expected Invalid for {cond}, got {other:?}"),
            }
        }
    }

    #[test]
    fn flagged_and_stop_parse() {
        let text = "[[rules]]\nname = \"x\"\nmatch.flagged = true\nactions = { read = true, stop = true }\n";
        let compiled = compile(&parse(text).unwrap()).unwrap();
        assert_eq!(compiled[0].conditions.flagged, Some(true));
        assert_eq!(
            compiled[0].rule.actions,
            vec![Action::MarkRead, Action::Stop]
        );
    }

    #[test]
    fn domain_conditions_take_one_or_more_domains() {
        let text = "[[rules]]\nname = \"x\"\nmatch.from_domain = [\"@PayPal.com.\", \"paypal.nl\"]\nmatch.to_domain = \"Example.org\"\nmatch.authenticated = true\nactions = { flagged = true }\n";
        let compiled = compile(&parse(text).unwrap()).unwrap();
        let c = &compiled[0].conditions;
        assert_eq!(
            c.from_domain.as_deref(),
            Some(&["paypal.com".to_string(), "paypal.nl".to_string()][..])
        );
        assert_eq!(
            c.to_domain.as_deref(),
            Some(&["example.org".to_string()][..])
        );
        assert_eq!(c.authenticated, Some(true));
        assert!(!compiled[0].matches_header());
        let rule = |value: &str| {
            format!(
                "[[rules]]\nname = \"x\"\nmatch.from_domain = {value}\nactions = {{ flagged = true }}\n"
            )
        };
        for (value, want) in [
            ("[]", "match.from_domain must not be empty"),
            ("\" \"", "match.from_domain must not be empty"),
            ("\"@\"", "match.from_domain must not be empty"),
            ("\"a@paypal.com\"", "is not a domain"),
            ("\"pay pal.com\"", "is not a domain"),
            ("\".paypal.com\"", "is not a domain"),
            ("\"paypal..com\"", "is not a domain"),
        ] {
            match parse(&rule(value)).and_then(|f| compile(&f)) {
                Err(RulesError::Invalid { reason, .. }) => {
                    assert!(reason.contains(want), "{reason} should contain {want}")
                }
                other => panic!("expected Invalid for {value}, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_file_in_the_old_syntax_is_told_how_to_rewrite_it() {
        let old = "[[rules]]\nname = \"x\"\nmatch.seen = true\nactions = [\"delete\"]\n";
        match parse(old) {
            Err(RulesError::Parse(message)) => {
                assert!(message.contains("old rules syntax"), "{message}")
            }
            other => panic!("{other:?}"),
        }
        match parse("[[rules]]\nname = \"x\"\nmatch.read = true\nactions = { explode = true }\n") {
            Err(RulesError::Parse(message)) => {
                assert!(!message.contains("old rules syntax"), "{message}")
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn user_only_actions_are_not_rule_actions() {
        for action in ["not_junk", "trash"] {
            let text =
                format!("[[rules]]\nname = \"x\"\nmatch.read = true\nactions.{action} = true\n");
            assert!(parse(&text).is_err(), "{action}");
        }
    }

    #[test]
    fn after_round_trips_and_refuses_what_it_cannot_wait_for() {
        let toml = "[[rules]]\nname = \"r\"\nmatch.read = false\nactions = { move = \"Shop\", after = [\"read\", \"2d\"] }\n";
        let file = parse(toml).unwrap();
        assert_eq!(
            file.rules[0].actions,
            [
                Action::Move("Shop".into()),
                Action::After("read".into()),
                Action::After("2d".into())
            ]
        );
        let compiled = compile(&file).unwrap();
        assert_eq!(
            compiled[0].after,
            [
                Wait::Read,
                Wait::Span(crate::time::Span::Seconds(2 * 86_400))
            ]
        );
        let again = parse(&toml::to_string(&file).unwrap()).unwrap();
        assert_eq!(again.rules[0].actions, file.rules[0].actions);
        let refused = |actions: &str| {
            let text =
                format!("[[rules]]\nname = \"r\"\nmatch.read = false\nactions = {{ {actions} }}\n");
            compile(&parse(&text).unwrap()).unwrap_err().to_string()
        };
        assert_eq!(
            refused("move = \"Shop\", after = \"soon\""),
            "rule 'r': actions.after: expected a span such as 30min, 1h, 2d, 3w, 6mo or 1y, got 'soon', or read"
        );
        assert_eq!(
            refused("stop = true, notify = false, after = \"1h\""),
            "rule 'r': actions.after needs an action to wait with, such as move or delete"
        );
    }
}
