//! Rules in plain language, and rules as the editor's rows of conditions.
use postvak_core::message::{bare_addresses, clean};
use postvak_core::rules::{Action, Domain, ListRef, Match, NamedList, OneOrMany, Rule, TextMatch};

/// `subject contains "a" or "b"`: a text condition, whose list matches any of its values.
fn text_label(field: &str, t: &TextMatch) -> String {
    match &t.list {
        Some(list) => format!("{field} in list {list}"),
        None => format!("{field} {}", NamedList::Text(t.clone()).words()),
    }
}

/// Each condition of `m` in words, in the order of the rules docs; `any` and `none` list their entries, each entry's
/// conditions joined by "and".
pub(crate) fn match_labels(m: &Match) -> Vec<String> {
    let mut labels = Vec::new();
    for (field, condition) in [
        ("from", &m.from),
        ("to", &m.to),
        ("cc", &m.cc),
        ("subject", &m.subject),
        ("body", &m.body),
    ] {
        if let Some(t) = condition {
            labels.push(text_label(field, t));
        }
    }
    for (name, t) in m.header.iter().flatten() {
        labels.push(text_label(&format!("header {name}"), t));
    }
    for (field, domains) in [("from", &m.from_domain), ("to", &m.to_domain)] {
        if let Some(domains) = domains {
            let domains: Vec<String> = (domains.as_slice().iter())
                .map(|domain| match domain {
                    Domain::Name(name) => name.clone(),
                    Domain::List(ListRef { list }) => format!("list {list}"),
                })
                .collect();
            labels.push(format!("{field} domain {}", domains.join(" or ")));
        }
    }
    labels.extend(m.authenticated.map(|authenticated| {
        if authenticated {
            "authenticated"
        } else {
            "not authenticated"
        }
        .to_string()
    }));
    labels.extend(m.older_than.as_ref().map(|v| format!("older than {v}")));
    labels.extend(m.younger_than.as_ref().map(|v| format!("younger than {v}")));
    labels.extend(m.larger_than.as_ref().map(|v| format!("larger than {v}")));
    labels.extend(
        m.read
            .map(|read| if read { "read" } else { "unread" }.to_string()),
    );
    labels.extend(
        m.flagged
            .map(|flagged| if flagged { "flagged" } else { "not flagged" }.to_string()),
    );
    labels.extend(
        m.to_me
            .map(|to_me| if to_me { "to me" } else { "not to me" }.to_string()),
    );
    labels.extend(m.alias.as_ref().map(|v| format!("alias {v}")));
    labels.extend(m.tag.as_ref().map(|v| format!("tag {v}")));
    for (word, entries) in [("any", &m.any), ("none", &m.none)] {
        if let Some(entries) = entries {
            let entries: Vec<String> = entries
                .iter()
                .map(|e| match_labels(e).join(" and "))
                .collect();
            labels.push(format!("{word} of ({})", entries.join("; ")));
        }
    }
    labels
}

pub(crate) fn action_label(action: &Action) -> String {
    match action {
        Action::Flag => "flag".into(),
        Action::MarkRead => "mark read".into(),
        Action::MarkUnread => "mark unread".into(),
        Action::Silent => "silent".into(),
        Action::Unflag => "unflag".into(),
        Action::Move(folder) => format!("move to {folder}"),
        Action::Tag(keyword) => format!("tag {keyword}"),
        Action::After(when) => format!("after {when}"),
        other => other.label(),
    }
}

/// `subject contains "code" → delete`: what a rule matches and what it does, one line, without control characters.
pub(crate) fn summary(rule: &Rule) -> String {
    let actions: Vec<String> = rule.actions.iter().map(action_label).collect();
    clean(
        &format!(
            "{} → {}",
            match_labels(&rule.matches).join(", "),
            actions.join(", ")
        ),
        false,
    )
}

/// "would delete 4 cached messages": what a preview on cached mail found. `needs_body` is
/// `CompiledRule::needs_body`, which also looks inside `any` and `none`.
pub(crate) fn preview_text(rule: &Rule, matched: usize, needs_body: bool) -> String {
    if matched == 0 {
        return "matches no cached message".into();
    }
    let verbs: Vec<String> = rule
        .actions
        .iter()
        .filter(|action| !matches!(action, Action::After(_)))
        .map(|action| match action {
            Action::Move(_) => "move".into(),
            Action::Notify => "notify about".into(),
            Action::Silent => "silence".into(),
            Action::Stop => "stop later rules for".into(),
            Action::Tag(_) => "tag".into(),
            other => action_label(other),
        })
        .collect();
    let target = rule
        .actions
        .iter()
        .find_map(|action| match action {
            Action::Move(folder) => Some(format!(" to {folder}")),
            _ => None,
        })
        .unwrap_or_default();
    let matched = matched as u64;
    let count = super::numbers::count(matched);
    let noun = super::numbers::noun(matched, "message", "messages");
    let body = if needs_body {
        "; only mail whose body is downloaded is checked"
    } else {
        ""
    };
    clean(
        &format!(
            "would {} {count} cached {noun}{target}{body}",
            verbs.join(" and ")
        ),
        false,
    )
}

/// A condition's key in rules.toml.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Field {
    From,
    To,
    Cc,
    Subject,
    Body,
    Header,
    FromDomain,
    ToDomain,
    Authenticated,
    OlderThan,
    YoungerThan,
    LargerThan,
    Read,
    Flagged,
    ToMe,
    Alias,
    Tag,
}

impl Field {
    /// In the order the rules docs list them.
    pub const ALL: [Field; 17] = [
        Field::From,
        Field::To,
        Field::Cc,
        Field::Subject,
        Field::Body,
        Field::Header,
        Field::FromDomain,
        Field::ToDomain,
        Field::Authenticated,
        Field::OlderThan,
        Field::YoungerThan,
        Field::LargerThan,
        Field::Read,
        Field::Flagged,
        Field::ToMe,
        Field::Alias,
        Field::Tag,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Field::Alias => "alias",
            Field::Authenticated => "authenticated",
            Field::Body => "body",
            Field::Cc => "cc",
            Field::Flagged => "flagged",
            Field::From => "from",
            Field::FromDomain => "from domain",
            Field::Header => "header",
            Field::LargerThan => "larger than",
            Field::OlderThan => "older than",
            Field::Read => "read",
            Field::Subject => "subject",
            Field::Tag => "tag",
            Field::To => "to",
            Field::ToDomain => "to domain",
            Field::ToMe => "to me",
            Field::YoungerThan => "younger than",
        }
    }

    /// Fields that take contains, equals or regex.
    pub fn is_text(self) -> bool {
        matches!(
            self,
            Field::Body | Field::Cc | Field::From | Field::Header | Field::Subject | Field::To
        )
    }

    /// Fields whose value is yes or no.
    pub fn is_flag(self) -> bool {
        matches!(
            self,
            Field::Authenticated | Field::Flagged | Field::Read | Field::ToMe
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Op {
    Contains,
    Equals,
    Regex,
}

impl Op {
    pub const ALL: [Op; 3] = [Op::Contains, Op::Equals, Op::Regex];

    pub fn label(self) -> &'static str {
        match self {
            Op::Contains => "contains",
            Op::Equals => "equals",
            Op::Regex => "matches regex",
        }
    }
}

/// One row of a rule's conditions.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Condition {
    pub field: Field,
    /// The header's name; used by `Field::Header` only.
    pub header: String,
    /// Used by text fields only.
    pub op: Op,
    /// The text, duration, size, alias or tag; "true" or "false" for the yes-or-no fields.
    pub value: String,
}

impl Condition {
    pub fn new(field: Field) -> Condition {
        Condition {
            field,
            header: String::new(),
            op: Op::Contains,
            value: if field.is_flag() {
                "true".into()
            } else {
                String::new()
            },
        }
    }

    /// This condition on `field`: the value stays unless it switches between yes/no and text.
    pub fn with_field(&self, field: Field) -> Condition {
        if field.is_flag() != self.field.is_flag() {
            return Condition::new(field);
        }
        Condition {
            field,
            ..self.clone()
        }
    }
}

/// A text condition with one operator and one value as a row; `None` for anything else, which the form keeps.
fn row(
    field: Field,
    header: &str,
    contains: Option<&OneOrMany<String>>,
    equals: Option<&OneOrMany<String>>,
    regex: Option<&String>,
) -> Option<Condition> {
    let (op, value) = match (contains, equals, regex) {
        (Some(OneOrMany::One(value)), None, None) => (Op::Contains, value.clone()),
        (None, Some(OneOrMany::One(value)), None) => (Op::Equals, value.clone()),
        (None, None, Some(value)) => (Op::Regex, value.clone()),
        _ => return None,
    };
    Some(Condition {
        field,
        header: header.to_string(),
        op,
        value,
    })
}

/// A rule's conditions as the form's rows, in the order of the rules docs, and the rest of `m` (`any`, `none`, value
/// and header lists), which the form shows read-only and saves as it was.
pub(crate) fn conditions(m: &Match) -> (Vec<Condition>, Match) {
    let mut kept = Match {
        any: m.any.clone(),
        none: m.none.clone(),
        ..Match::default()
    };
    let mut rows = Vec::new();
    for (field, condition, slot) in [
        (Field::From, &m.from, &mut kept.from),
        (Field::To, &m.to, &mut kept.to),
        (Field::Cc, &m.cc, &mut kept.cc),
        (Field::Subject, &m.subject, &mut kept.subject),
        (Field::Body, &m.body, &mut kept.body),
    ] {
        let Some(t) = condition else { continue };
        match row(
            field,
            "",
            t.contains.as_ref(),
            t.equals.as_ref(),
            t.regex.as_ref(),
        ) {
            Some(condition) => rows.push(condition),
            None => *slot = Some(t.clone()),
        }
    }
    for (name, t) in m.header.iter().flatten() {
        match row(
            Field::Header,
            name,
            t.contains.as_ref(),
            t.equals.as_ref(),
            t.regex.as_ref(),
        ) {
            Some(condition) => rows.push(condition),
            None => {
                _ = kept
                    .header
                    .get_or_insert_default()
                    .insert(name.clone(), t.clone())
            }
        }
    }
    let plain = |field, value: String| Condition {
        value,
        ..Condition::new(field)
    };
    for (field, domains, slot) in [
        (Field::FromDomain, &m.from_domain, &mut kept.from_domain),
        (Field::ToDomain, &m.to_domain, &mut kept.to_domain),
    ] {
        match domains {
            Some(OneOrMany::One(Domain::Name(domain))) => rows.push(plain(field, domain.clone())),
            Some(list) => *slot = Some(list.clone()),
            None => {}
        }
    }
    rows.extend(
        m.authenticated
            .map(|v| plain(Field::Authenticated, v.to_string())),
    );
    rows.extend(m.older_than.clone().map(|v| plain(Field::OlderThan, v)));
    rows.extend(m.younger_than.clone().map(|v| plain(Field::YoungerThan, v)));
    rows.extend(m.larger_than.clone().map(|v| plain(Field::LargerThan, v)));
    rows.extend(m.read.map(|v| plain(Field::Read, v.to_string())));
    rows.extend(m.flagged.map(|v| plain(Field::Flagged, v.to_string())));
    rows.extend(m.to_me.map(|v| plain(Field::ToMe, v.to_string())));
    rows.extend(m.alias.clone().map(|v| plain(Field::Alias, v)));
    rows.extend(m.tag.clone().map(|v| plain(Field::Tag, v)));
    (rows, kept)
}

/// The actions "+ Add action" offers, in its order; After, Move to and Tag start with an empty value.
pub(crate) const ACTIONS: [Action; 11] = [
    Action::After(String::new()),
    Action::Archive,
    Action::Delete,
    Action::Flag,
    Action::Junk,
    Action::MarkRead,
    Action::Move(String::new()),
    Action::Notify,
    Action::Silent,
    Action::Stop,
    Action::Tag(String::new()),
];

/// An action as a chip or menu entry names it.
pub(crate) fn action_name(action: &Action) -> &'static str {
    match action {
        Action::After(_) => "After",
        Action::Archive => "Archive",
        Action::Delete => "Delete",
        Action::Flag => "Flag",
        Action::Junk => "Junk",
        Action::MarkRead => "Mark read",
        Action::Move(_) => "Move to",
        Action::Notify => "Notify",
        Action::Silent => "Silent",
        Action::Stop => "Stop",
        Action::Tag(_) => "Tag",
        _ => "",
    }
}

/// Longest name a draft made from a message gets; rules.toml allows 255 characters.
const SEED_NAME_CHARS: usize = 60;

/// The rule editor's working copy. Lives in `App.rule_draft` until Save writes it.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Draft {
    pub account: Option<String>,
    pub actions: Vec<Action>,
    pub conditions: Vec<Condition>,
    /// Set by "Remove rule" on a saved rule; the footer then asks to confirm.
    pub confirm_remove: bool,
    pub enabled: bool,
    /// Why the last save or removal failed; cleared by any edit.
    pub error: Option<String>,
    pub folder: Option<String>,
    /// The conditions the form cannot edit, saved as they were.
    pub kept: Match,
    pub name: String,
    /// The rule's name in rules.toml; None until the draft is first saved.
    pub original: Option<String>,
    pub proposed_by: Option<String>,
    pub replaces: Option<String>,
    /// The last Test result; cleared by any edit.
    pub tested: Option<String>,
}

impl Draft {
    pub fn new() -> Draft {
        Draft {
            enabled: true,
            ..Draft::default()
        }
    }

    pub fn from_rule(rule: &Rule) -> Draft {
        let (conditions, kept) = conditions(&rule.matches);
        Draft {
            account: rule.account.clone(),
            actions: rule.actions.clone(),
            conditions,
            enabled: rule.enabled,
            folder: rule.folder.clone(),
            kept,
            name: rule.name.clone(),
            original: Some(rule.name.clone()),
            proposed_by: rule.proposed_by.clone(),
            replaces: rule.replaces.clone(),
            ..Draft::default()
        }
    }

    /// True when Save would rename a saved rule, which makes it a new rule with a new clock.
    pub fn renames(&self) -> bool {
        self.original
            .as_deref()
            .is_some_and(|original| original != self.name)
    }

    /// True when the form differs from `opened`; Test results, errors and the remove prompt do not count.
    pub fn edited(&self, opened: &Draft) -> bool {
        let form = |draft: &Draft| Draft {
            confirm_remove: false,
            error: None,
            tested: None,
            ..draft.clone()
        };
        form(self) != form(opened)
    }

    /// The rule this draft saves as: the kept conditions plus the rows. `rules::compile` checks the values when the
    /// file is saved; this only refuses what one rule cannot hold.
    pub fn to_rule(&self) -> Result<Rule, String> {
        let mut m = self.kept.clone();
        for c in &self.conditions {
            let twice = match c.field {
                Field::Alias => m.alias.replace(c.value.clone()).is_some(),
                Field::Authenticated => m.authenticated.replace(flag(c)?).is_some(),
                Field::FromDomain => m
                    .from_domain
                    .replace(OneOrMany::One(Domain::Name(c.value.clone())))
                    .is_some(),
                Field::ToDomain => m
                    .to_domain
                    .replace(OneOrMany::One(Domain::Name(c.value.clone())))
                    .is_some(),
                Field::Body => m.body.replace(text_match(c)).is_some(),
                Field::Cc => m.cc.replace(text_match(c)).is_some(),
                Field::Flagged => m.flagged.replace(flag(c)?).is_some(),
                Field::From => m.from.replace(text_match(c)).is_some(),
                Field::Header => m
                    .header
                    .get_or_insert_default()
                    .insert(c.header.clone(), text_match(c))
                    .is_some(),
                Field::LargerThan => m.larger_than.replace(c.value.clone()).is_some(),
                Field::OlderThan => m.older_than.replace(c.value.clone()).is_some(),
                Field::Read => m.read.replace(flag(c)?).is_some(),
                Field::Subject => m.subject.replace(text_match(c)).is_some(),
                Field::Tag => m.tag.replace(c.value.clone()).is_some(),
                Field::To => m.to.replace(text_match(c)).is_some(),
                Field::ToMe => m.to_me.replace(flag(c)?).is_some(),
                Field::YoungerThan => m.younger_than.replace(c.value.clone()).is_some(),
            };
            if twice {
                return Err(format!(
                    "{} appears twice; a rule has one condition per field",
                    c.field.label()
                ));
            }
        }
        Ok(Rule {
            name: self.name.clone(),
            account: self.account.clone(),
            folder: self.folder.clone(),
            enabled: self.enabled,
            proposed_by: self.proposed_by.clone(),
            replaces: self.replaces.clone(),
            matches: m,
            actions: self.actions.clone(),
        })
    }

    /// A new rule for mail from the sender of the message on screen, given its From, watching its account and folder.
    pub fn from_sender(from: &str, account: &str, folder: &str) -> Draft {
        let address = bare_addresses(from)
            .into_iter()
            .next()
            .unwrap_or_else(|| from.trim().to_string());
        let value = clean(&address, false);
        let name: String = format!("from {value}")
            .chars()
            .take(SEED_NAME_CHARS)
            .collect();
        Draft {
            account: Some(account.to_string()),
            conditions: vec![Condition {
                field: Field::From,
                header: String::new(),
                op: Op::Equals,
                value,
            }],
            folder: (!folder.eq_ignore_ascii_case("INBOX")).then(|| folder.to_string()),
            name: name.trim_end().to_string(),
            ..Draft::new()
        }
    }
}

fn text_match(c: &Condition) -> TextMatch {
    let one = || Some(OneOrMany::One(c.value.clone()));
    match c.op {
        Op::Contains => TextMatch {
            contains: one(),
            ..TextMatch::default()
        },
        Op::Equals => TextMatch {
            equals: one(),
            ..TextMatch::default()
        },
        Op::Regex => TextMatch {
            regex: Some(c.value.clone()),
            ..TextMatch::default()
        },
    }
}

fn flag(c: &Condition) -> Result<bool, String> {
    c.value
        .parse()
        .map_err(|_| format!("{} must be yes or no", c.field.label()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use postvak_core::rules::parse;

    const ALL_FIELDS: &str = r#"
[[rules]]
name = "everything"
match.from = { equals = "a@example.com" }
match.to = { contains = "team" }
match.cc = { regex = "(?i)boss" }
match.subject = { contains = "code" }
match.body = { regex = "\\d{6}" }
match.header.List-Id = { contains = "github.com" }
match.from_domain = "example.com"
match.to_domain = "example.org"
match.authenticated = true
match.older_than = "1h"
match.younger_than = "1d"
match.larger_than = "5MB"
match.read = true
match.flagged = false
match.to_me = false
match.alias = "*@shop.example.com"
match.tag = "$label1"
actions = { move = "Codes", tag = "$label2", read = true, flagged = true, notify = false, stop = true }
"#;

    /// What the form cannot edit: value lists, a header list, `any` and `none`.
    const GROUPED: &str = r#"
[[rules]]
name = "grouped"
match.subject = { contains = ["receipt", "invoice"] }
match.from = { contains = "shop" }
match.header = { List-Id = { contains = "a" }, X-Kind = { equals = "b" } }
match.from_domain = ["paypal.com", "paypal.nl"]
match.any = [{ read = true }, { to_me = false, alias = "x@example.com" }]
match.none = [{ tag = "$label1" }]
actions = { flagged = true }
"#;

    fn rule(text: &str) -> Rule {
        parse(text).unwrap().rules.remove(0)
    }

    #[test]
    fn the_summary_reads_conditions_then_actions() {
        assert_eq!(
            summary(&rule(
                "[[rules]]\nname = \"x\"\nmatch.subject = { contains = \"verification code\" }\nactions = { delete = true }\n"
            )),
            "subject contains \"verification code\" → delete"
        );
        assert_eq!(
            summary(&rule(ALL_FIELDS)),
            "from equals \"a@example.com\", to contains \"team\", cc matches regex \"(?i)boss\", \
             subject contains \"code\", body matches regex \"\\d{6}\", header List-Id contains \"github.com\", \
             from domain example.com, to domain example.org, authenticated, older than 1h, younger than 1d, larger than 5MB, read, not flagged, not to me, alias *@shop.example.com, tag $label1 \
             → move to Codes, mark read, flag, tag $label2, silent, stop"
        );
    }

    #[test]
    fn the_summary_reads_lists_and_groups() {
        assert_eq!(
            summary(&rule(GROUPED)),
            "from contains \"shop\", subject contains \"receipt\" or \"invoice\", header List-Id contains \"a\", \
             header X-Kind equals \"b\", from domain paypal.com or paypal.nl, any of (read; not to me and alias x@example.com), none of (tag $label1) \
             → flag"
        );
    }

    #[test]
    fn list_references_read_by_name_stay_out_of_the_rows_and_save_as_they_were() {
        let referring = rule(
            "[[rules]]\nname = \"x\"\nmatch.subject = { list = \"alerts\" }\n\
             match.from_domain = [{ list = \"kids\" }, \"extra.example\"]\nmatch.to_domain = { list = \"work\" }\n\
             actions = { flagged = true }\n",
        );
        assert_eq!(
            summary(&referring),
            "subject in list alerts, from domain list kids or extra.example, to domain list work → flag"
        );
        let (rows, kept) = conditions(&referring.matches);
        assert!(rows.is_empty(), "{rows:?}");
        assert_eq!(kept, referring.matches);
    }

    #[test]
    fn the_summary_drops_control_characters() {
        let sneaky = rule(
            "[[rules]]\nname = \"x\"\nmatch.subject = { contains = \"\\u001b[2Jcode\" }\nactions = { delete = true }\n",
        );
        assert_eq!(summary(&sneaky), "subject contains \"[2Jcode\" → delete");
    }

    #[test]
    fn the_preview_line_counts_messages_and_names_the_actions() {
        let delete =
            rule("[[rules]]\nname = \"x\"\nmatch.read = true\nactions = { delete = true }\n");
        assert_eq!(
            preview_text(&delete, 4, false),
            "would delete 4 cached messages"
        );
        assert_eq!(
            preview_text(&delete, 1, false),
            "would delete 1 cached message"
        );
        assert_eq!(preview_text(&delete, 0, false), "matches no cached message");
        let filed = rule(
            "[[rules]]\nname = \"x\"\nmatch.read = true\nactions = { move = \"Lists/GitHub\", read = true }\n",
        );
        assert_eq!(
            preview_text(&filed, 3, false),
            "would move and mark read 3 cached messages to Lists/GitHub"
        );
        let flag =
            rule("[[rules]]\nname = \"x\"\nmatch.read = true\nactions = { flagged = true }\n");
        assert_eq!(
            preview_text(&flag, 2, true),
            "would flag 2 cached messages; only mail whose body is downloaded is checked"
        );
    }

    #[test]
    fn every_single_value_condition_becomes_a_row_in_docs_order() {
        let (rows, kept) = conditions(&rule(ALL_FIELDS).matches);
        let fields: Vec<Field> = rows.iter().map(|c| c.field).collect();
        assert_eq!(fields, Field::ALL);
        assert_eq!(
            (rows[0].op, rows[0].value.as_str()),
            (Op::Equals, "a@example.com")
        );
        assert_eq!(
            (rows[5].header.as_str(), rows[5].op),
            ("List-Id", Op::Contains)
        );
        assert_eq!(kept, Match::default());
    }

    #[test]
    fn lists_any_and_none_are_kept_out_of_the_rows_and_each_header_is_a_row() {
        let (rows, kept) = conditions(&rule(GROUPED).matches);
        assert_eq!(
            rows.iter().map(|c| c.field).collect::<Vec<_>>(),
            [Field::From, Field::Header, Field::Header]
        );
        assert!(
            kept.subject.is_some()
                && kept.header.is_none()
                && kept.from_domain.is_some()
                && kept.any.is_some()
                && kept.none.is_some()
        );
        assert!(kept.from.is_none());
    }

    #[test]
    fn a_rule_survives_the_round_trip_through_the_form() {
        let original = rule(&ALL_FIELDS.replace(
            "name = \"everything\"",
            "name = \"everything\"\naccount = \"work\"\nfolder = \"Lists\"\nenabled = false\nproposed_by = \"mcp:agent\"",
        ));
        let back = Draft::from_rule(&original).to_rule().unwrap();
        assert_eq!(
            toml::to_string(&back).unwrap(),
            toml::to_string(&original).unwrap()
        );
        let grouped = rule(GROUPED);
        let back = Draft::from_rule(&grouped).to_rule().unwrap();
        assert_eq!(
            toml::to_string(&back).unwrap(),
            toml::to_string(&grouped).unwrap()
        );
    }

    #[test]
    fn a_row_on_a_kept_field_is_refused() {
        let mut draft = Draft::from_rule(&rule(GROUPED));
        draft.conditions.push(Condition::new(Field::Subject));
        assert_eq!(
            draft.to_rule().unwrap_err(),
            "subject appears twice; a rule has one condition per field"
        );
    }

    #[test]
    fn to_rule_refuses_what_one_rule_cannot_hold() {
        let mut draft = Draft::new();
        draft.conditions = vec![
            Condition::new(Field::Subject),
            Condition::new(Field::Subject),
        ];
        assert_eq!(
            draft.to_rule().unwrap_err(),
            "subject appears twice; a rule has one condition per field"
        );
        draft.conditions = vec![Condition {
            value: "maybe".into(),
            ..Condition::new(Field::Read)
        }];
        assert_eq!(draft.to_rule().unwrap_err(), "read must be yes or no");
    }

    #[test]
    fn changing_a_conditions_field_keeps_the_value_where_it_fits() {
        let subject = Condition {
            value: "code".into(),
            ..Condition::new(Field::Subject)
        };
        assert_eq!(subject.with_field(Field::From).value, "code");
        assert_eq!(subject.with_field(Field::Read).value, "true");
        assert_eq!(
            Condition::new(Field::Read).with_field(Field::Subject).value,
            ""
        );
    }

    #[test]
    fn renaming_is_noticed() {
        let mut draft = Draft::from_rule(&rule(ALL_FIELDS));
        assert!(!draft.renames());
        draft.name = "else".into();
        assert!(draft.renames());
        assert!(!Draft::new().renames(), "a new rule has nothing to rename");
    }

    #[test]
    fn only_form_changes_count_as_edits() {
        let opened = Draft::from_rule(&rule(ALL_FIELDS));
        let mut draft = opened.clone();
        draft.confirm_remove = true;
        draft.error = Some("no such rule".into());
        draft.tested = Some("would archive 1 cached message".into());
        assert!(!draft.edited(&opened));
        draft.name = "else".into();
        assert!(draft.edited(&opened));
    }

    #[test]
    fn every_action_the_menu_offers_has_a_name() {
        assert!(ACTIONS.iter().all(|action| !action_name(action).is_empty()));
    }

    #[test]
    fn a_draft_from_a_sender_matches_their_first_address_in_short_clean_words() {
        let from = Draft::from_sender(
            "Alice <Alice@Example.com>, bob@example.com",
            "work",
            "INBOX",
        );
        assert_eq!(
            from.conditions,
            [Condition {
                field: Field::From,
                header: String::new(),
                op: Op::Equals,
                value: "alice@example.com".into(),
            }]
        );
        assert_eq!(from.name, "from alice@example.com");
        assert_eq!(
            (from.account.as_deref(), from.folder.as_deref()),
            (Some("work"), None)
        );
        assert!(from.original.is_none() && from.actions.is_empty() && from.enabled);

        let long = format!("x\u{1b}[2J{}@example.com", "y".repeat(300));
        let sneaky = Draft::from_sender(&long, "work", "Lists");
        assert_eq!(sneaky.folder.as_deref(), Some("Lists"));
        assert!(sneaky.name.chars().count() <= 60, "{}", sneaky.name);
        assert!(!sneaky.name.contains('\u{1b}') && !sneaky.conditions[0].value.contains('\u{1b}'));
    }
}
