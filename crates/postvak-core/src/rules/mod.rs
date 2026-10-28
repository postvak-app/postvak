pub mod apply;
pub mod edit;

pub use edit::rule_toml;
pub mod engine;
mod warnings;

pub use warnings::warnings;

use std::io;
use std::path::Path;
use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum RulesError {
    #[error("rules.toml: {0}")]
    Parse(String),
    #[error("rule '{rule}': {reason}")]
    Invalid { rule: String, reason: String },
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
    /// Conditions that must all hold; at least one
    #[serde(rename = "match")]
    pub matches: Match,
    /// What to do; at least one
    pub actions: Vec<Action>,
}

impl Rule {
    /// What the rule does, as JSON: editing any of it restarts the rule's clock, so a widened rule never acts on older mail.
    pub fn definition(&self) -> Result<String, RulesError> {
        serde_json::to_string(&(&self.account, &self.folder, &self.matches, &self.actions)).map_err(
            |e| RulesError::Invalid {
                rule: self.name.clone(),
                reason: e.to_string(),
            },
        )
    }
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Match {
    /// The From header
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<TextMatch>,
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
    /// Any header, by name; a list must all match
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<OneOrMany<HeaderMatch>>,
    /// The From address's domain or any subdomain of it, never the display name; a list matches any of them
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_domain: Option<OneOrMany<String>>,
    /// The domain, or any subdomain, of an address in the To header; a list matches any of them
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_domain: Option<OneOrMany<String>>,
    /// An Authentication-Results header from a server in the account's trusted_authserv_ids says DMARC passed for the From domain, or without a DMARC verdict, DKIM passed for a domain aligned with it; never true without that list
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authenticated: Option<bool>,
    /// Arrived at least this long ago, e.g. 30m, 1h, 2days
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub older_than: Option<String>,
    /// Larger than this size, e.g. 500KB, 5MB; a bare number is bytes and 1KB is 1024 bytes
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub larger_than: Option<String>,
    /// Read (true) or unread (false)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seen: Option<bool>,
    /// Flagged (true) or not (false)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flagged: Option<bool>,
    /// To, Cc or Delivered-To holds your address or an alias
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_me: Option<bool>,
    /// Sent to this alias; * is a wildcard
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    /// Carries this IMAP keyword, which Thunderbird shows as a tag (e.g. $label1); case-insensitive
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    /// Holds when no entry holds; an entry holds when all its conditions do
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub none: Option<Vec<Match>>,
    /// Holds when at least one of these holds
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

/// Exactly one of contains, equals, regex
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TextMatch {
    /// Case-insensitive substring; a list matches any of them
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contains: Option<OneOrMany<String>>,
    /// The whole value, case-insensitive; on from, to and cc also any single address; a list matches any of them
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub equals: Option<OneOrMany<String>>,
    /// Rust regex syntax; (?i) makes it case-insensitive
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regex: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HeaderMatch {
    /// Header name, e.g. List-Id
    pub name: String,
    /// Case-insensitive substring; a list matches any of them
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contains: Option<OneOrMany<String>>,
    /// The whole value, case-insensitive; on from, to and cc also any single address; a list matches any of them
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub equals: Option<OneOrMany<String>>,
    /// Rust regex syntax; (?i) makes it case-insensitive
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regex: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
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
    #[serde(skip)]
    MarkUnread,
    #[serde(skip)]
    Unflag,
    /// A user delete: to the Trash folder, or deleted with a backup when there is none or the message is already in it.
    #[serde(skip)]
    Trash,
    /// A user report: tagged $Junk, which trains server spam filters, and moved to the Junk folder.
    #[serde(skip)]
    Junk,
    /// Undoes a wrong `Junk`: tagged $NotJunk and moved to INBOX.
    #[serde(skip)]
    NotJunk,
    /// Move to this folder, creating it if needed
    #[serde(rename = "move")]
    Move(String),
    /// Add this IMAP keyword, which Thunderbird shows as a tag (e.g. $label1)
    #[serde(rename = "tag")]
    Tag(String),
}

impl Action {
    /// The label of the command a user gave for this action: `delete` sends `Trash` outside the Trash folder.
    pub fn command_label(&self) -> String {
        match self {
            Action::Trash => Action::Delete.label(),
            _ => self.label(),
        }
    }

    pub fn label(&self) -> String {
        match self {
            Action::Delete => "delete".into(),
            Action::MarkRead => "mark_read".into(),
            Action::Flag => "flag".into(),
            Action::Archive => "archive".into(),
            Action::Notify => "notify".into(),
            Action::Silent => "silent".into(),
            Action::Stop => "stop".into(),
            Action::MarkUnread => "mark_unread".into(),
            Action::Unflag => "unflag".into(),
            Action::Trash => "trash".into(),
            Action::Junk => "junk".into(),
            Action::NotJunk => "not_junk".into(),
            Action::Move(folder) => format!("move:{folder}"),
            Action::Tag(keyword) => format!("tag:{keyword}"),
        }
    }
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
    toml::from_str(text).map_err(|e| RulesError::Parse(e.to_string()))
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
            (None, None, Some(r)) => regex::Regex::new(r)
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
    pub(crate) conditions: Conditions,
}

/// A compiled `Match`: every set condition must hold.
#[derive(Debug, Clone, Default)]
pub(crate) struct Conditions {
    pub(crate) from: Option<Matcher>,
    pub(crate) to: Option<Matcher>,
    pub(crate) cc: Option<Matcher>,
    pub(crate) subject: Option<Matcher>,
    pub(crate) body: Option<Matcher>,
    pub(crate) headers: Vec<(String, Matcher)>,
    /// Lower-cased domains, without a leading `@` or a trailing dot.
    pub(crate) from_domain: Option<Vec<String>>,
    pub(crate) to_domain: Option<Vec<String>>,
    pub(crate) authenticated: Option<bool>,
    pub(crate) older_than: Option<Duration>,
    pub(crate) larger_than: Option<u64>,
    pub(crate) seen: Option<bool>,
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

impl CompiledRule {
    pub fn folder(&self) -> &str {
        self.rule.folder.as_deref().unwrap_or("INBOX")
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

/// Deepest nesting of `not` and `any`; Thunderbird's filters need two levels at most.
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
    let mut names = std::collections::HashSet::new();
    file.rules
        .iter()
        .map(|rule| compile_rule(rule, &mut names))
        .collect()
}

fn compile_rule(
    rule: &Rule,
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
        if value.chars().any(char::is_control) {
            return Err(invalid(
                &rule.name,
                format!("{field} must not contain control characters"),
            ));
        }
        if value.trim().is_empty() {
            return Err(invalid(&rule.name, format!("{field} must not be empty")));
        }
        if value.chars().count() > MAX_NAME_CHARS {
            return Err(invalid(
                &rule.name,
                format!("{field} must be at most {MAX_NAME_CHARS} characters"),
            ));
        }
    }
    for action in &rule.actions {
        if let Action::Tag(keyword) = action {
            check_keyword(keyword)
                .map_err(|reason| invalid(&rule.name, format!("tag {reason}")))?;
        }
    }
    Ok(CompiledRule {
        rule: rule.clone(),
        first_seen_at: 0,
        conditions: compile_match(&rule.name, &rule.matches, "match", 0)?,
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
    let headers = m
        .header
        .as_ref()
        .map(OneOrMany::as_slice)
        .unwrap_or_default();
    let has_condition = m.from.is_some()
        || m.to.is_some()
        || m.cc.is_some()
        || m.subject.is_some()
        || m.body.is_some()
        || !headers.is_empty()
        || m.from_domain.is_some()
        || m.to_domain.is_some()
        || m.authenticated.is_some()
        || m.older_than.is_some()
        || m.larger_than.is_some()
        || m.seen.is_some()
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
    let headers = headers
        .iter()
        .map(|h| {
            if h.name.trim().is_empty() {
                return Err(invalid(
                    rule,
                    format!("{path}.header.name must not be empty"),
                ));
            }
            Matcher::build(
                rule,
                &format!("{path}.header"),
                &h.contains,
                &h.equals,
                &h.regex,
            )
            .map(|matcher| (h.name.clone(), matcher))
        })
        .collect::<Result<_, _>>()?;
    let older_than = m
        .older_than
        .as_ref()
        .map(|s| {
            humantime::parse_duration(s)
                .map_err(|e| invalid(rule, format!("{path}.older_than: {e}")))
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
    let domains = |key: &str, list: &Option<OneOrMany<String>>| {
        list.as_ref()
            .map(|list| domain_list(rule, &format!("{path}.{key}"), list.as_slice()))
            .transpose()
    };
    let any = branches("any", &m.any)?;
    let none = branches("none", &m.none)?;
    Ok(Conditions {
        from: text("from", &m.from)?,
        to: text("to", &m.to)?,
        cc: text("cc", &m.cc)?,
        subject: text("subject", &m.subject)?,
        body: text("body", &m.body)?,
        headers,
        from_domain: domains("from_domain", &m.from_domain)?,
        to_domain: domains("to_domain", &m.to_domain)?,
        authenticated: m.authenticated,
        older_than,
        larger_than,
        seen: m.seen,
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
        assert!(
            !text.contains("mark_unread"),
            "CLI-only actions stay out of the schema"
        );
    }

    pub(crate) const SAMPLE: &str = r#"
[[rules]]
name = "purge sign-in codes"
account = "work"
match.from = { regex = "no-?reply@" }
match.subject = { regex = "(?i)sign.?in|verification code|magic link" }
match.older_than = "1h"
match.seen = true
actions = ["delete"]

[[rules]]
name = "github to folder"
match.header = { name = "List-Id", contains = "github.com" }
actions = [{ move = "Lists/GitHub" }, "mark_read"]

[[rules]]
name = "shop alias"
enabled = false
proposed_by = "cli:test"
match.alias = "*@shop.example.com"
actions = [{ move = "Shopping" }, "notify"]
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
            github.matches.header.as_ref().unwrap().as_slice()[0].name,
            "List-Id"
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
            Some(Duration::from_secs(3600))
        );
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
        let empty_match = "[[rules]]\nname = \"x\"\nmatch = {}\nactions = [\"delete\"]\n";
        assert!(matches!(
            parse(empty_match).and_then(|f| compile(&f)),
            Err(RulesError::Invalid { .. })
        ));
        let no_actions = "[[rules]]\nname = \"x\"\nmatch.seen = true\nactions = []\n";
        assert!(matches!(
            parse(no_actions).and_then(|f| compile(&f)),
            Err(RulesError::Invalid { .. })
        ));
        let unknown = "[[rules]]\nname = \"x\"\nmatch.seen = true\nactions = [\"explode\"]\n";
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
        let typo = "[[rule]]\nname = \"x\"\nmatch.seen = true\nactions = [\"flag\"]\n";
        assert!(matches!(parse(typo), Err(RulesError::Parse(_))));
    }

    #[test]
    fn rejects_empty_values() {
        let rule = |matcher: &str, action: &str| {
            format!("[[rules]]\nname = \"x\"\n{matcher}\nactions = [{action}]\n")
        };
        let cases = [
            rule("match.subject = { contains = \"\" }", "\"delete\""),
            rule("match.subject = { equals = \"  \" }", "\"delete\""),
            rule("match.subject = { regex = \"\" }", "\"delete\""),
            rule(
                "match.header = { name = \"\", contains = \"a\" }",
                "\"delete\"",
            ),
            rule("match.seen = true", "{ move = \"\" }"),
            rule("match.alias = \"\"", "\"delete\""),
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
                "[[rules]]\nname = \"{name}\"\n{extra}match.seen = true\nactions = [{{ move = \"{target}\" }}]\n"
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
    fn rejects_control_characters_in_agent_strings() {
        let cases = [
            "[[rules]]\nname = \"x\"\nmatch.seen = true\nactions = [{ move = \"Codes\\u001b[2J\" }]\n",
            "[[rules]]\nname = \"x\\u001b]0;pwned\"\nmatch.seen = true\nactions = [\"flag\"]\n",
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
match.header = [{ name = "List-Id", contains = "shop" }, { name = "X-Mailer", contains = "bulk" }]
actions = [{ tag = "$label4" }, { move = "Shop" }]
"#;

    #[test]
    fn parses_none_any_tag_value_and_header_lists() {
        let file = parse(NESTED).unwrap();
        let rule = &file.rules[0];
        assert_eq!(
            rule.actions,
            vec![Action::Tag("$label4".into()), Action::Move("Shop".into())]
        );
        assert_eq!(rule.matches.any.as_ref().unwrap().len(), 2);
        assert_eq!(rule.matches.none.as_ref().unwrap().len(), 1);
        assert_eq!(rule.matches.header.as_ref().unwrap().as_slice().len(), 2);
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
        let rule =
            |matcher: &str| format!("[[rules]]\nname = \"x\"\n{matcher}\nactions = [\"flag\"]\n");
        let deep = format!(
            "match.none = {}{}",
            "[{ none = ".repeat(MAX_DEPTH),
            "[{ seen = true }]".to_string() + &" }]".repeat(MAX_DEPTH)
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
            (rule("match.header = []"), "match.header must not be empty"),
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
                rule("match.any = [{ seen = true }, { from = { contains = \"\" } }]"),
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
            format!(
                "[[rules]]\nname = \"x\"\nmatch.seen = true\nactions = [{{ tag = \"{tag}\" }}]\n"
            )
        };
        let matcher = |tag: &str| {
            format!("[[rules]]\nname = \"x\"\nmatch.tag = \"{tag}\"\nactions = [\"flag\"]\n")
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
                "[[rules]]\nname = \"x\"\nmatch.larger_than = \"{size}\"\nactions = [\"flag\"]\n"
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
    fn flagged_and_stop_parse() {
        let text =
            "[[rules]]\nname = \"x\"\nmatch.flagged = true\nactions = [\"mark_read\", \"stop\"]\n";
        let compiled = compile(&parse(text).unwrap()).unwrap();
        assert_eq!(compiled[0].conditions.flagged, Some(true));
        assert_eq!(
            compiled[0].rule.actions,
            vec![Action::MarkRead, Action::Stop]
        );
    }

    #[test]
    fn domain_conditions_take_one_or_more_domains() {
        let text = "[[rules]]\nname = \"x\"\nmatch.from_domain = [\"@PayPal.com.\", \"paypal.nl\"]\nmatch.to_domain = \"Example.org\"\nmatch.authenticated = true\nactions = [\"flag\"]\n";
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
            format!("[[rules]]\nname = \"x\"\nmatch.from_domain = {value}\nactions = [\"flag\"]\n")
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
    fn cli_only_actions_are_not_rule_actions() {
        for action in ["mark_unread", "trash", "unflag"] {
            let text =
                format!("[[rules]]\nname = \"x\"\nmatch.seen = true\nactions = [\"{action}\"]\n");
            assert!(parse(&text).is_err(), "{action}");
        }
    }
}
