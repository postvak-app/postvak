//! JSON rows shared by the CLI's `--json` and the MCP results; each row carries its account.
use serde_json::{Value, json};

use crate::actions::Planned;
use crate::config::AccountConfig;
use crate::daemon::wire::Status;
use crate::message::{HeaderField, clean_for_agent};
use crate::rules::Rule;
use crate::rules::coverage::Coverage;
use crate::senders::Sender;
use crate::store::{Folder, Message};

pub fn with_account(account: &str, row: &impl serde::Serialize) -> serde_json::Result<Value> {
    let mut value = serde_json::to_value(row)?;
    if let Some(object) = value.as_object_mut() {
        object.insert("account".into(), account.into());
    }
    Ok(value)
}

pub fn folder(account: &str, folder: &Folder, total: u32, unread: u32) -> Value {
    json!({ "account": account, "folder": folder.name, "total": total, "unread": unread, "special_use": folder.special_use })
}

/// An account's listing row; leaves out the password source, which may be a command holding a secret.
pub fn account(account: &AccountConfig) -> Value {
    json!({ "account": account.name, "address": account.address, "aliases": account.aliases, "host": account.host, "port": account.port, "username": account.username })
}

/// Each message's indent in its thread, relative to the thread's shallowest message and capped at 4.
pub fn depths(thread: &[Message]) -> Vec<usize> {
    let base = thread.iter().map(Message::thread_depth).min().unwrap_or(0);
    thread
        .iter()
        .map(|m| (m.thread_depth() - base).min(4))
        .collect()
}

/// A message's row (or a search hit's, which is a message plus `match`): its account, never its body text, and in a
/// thread view its `depth`. Every `--json` listing and MCP tool that shows messages builds its rows with this.
pub fn message(
    account: &str,
    message: &impl serde::Serialize,
    depth: Option<usize>,
) -> serde_json::Result<Value> {
    let mut row = with_account(account, message)?;
    if let Some(object) = row.as_object_mut() {
        object.remove("body_text");
        if let Some(depth) = depth {
            object.insert("depth".into(), depth.into());
        }
    }
    Ok(row)
}

/// Every key of a message row, as `message` builds it.
const MESSAGE_FIELDS: &[&str] = &[
    "account",
    "folder",
    "uid",
    "message_id",
    "from_addr",
    "to_addr",
    "cc_addr",
    "delivered_to",
    "in_reply_to",
    "refs",
    "thread_id",
    "subject",
    "date",
    "internaldate",
    "flags",
    "size",
];
/// What `compact` keeps of a message row; a thread row keeps its `depth` too and a search hit its `match`.
const COMPACT: &[&str] = &[
    "account",
    "folder",
    "uid",
    "date",
    "from_addr",
    "subject",
    "flags",
];

/// Which listing a row comes from, which decides the keys beyond a message's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    /// A `list` row.
    Message,
    /// A `list` row in a thread view: a message with its `depth`.
    Thread,
    /// A `search` row: a message with its `match` and, for a body match, its `snippet`.
    Hit,
}

impl RowKind {
    fn extra(self) -> &'static [&'static str] {
        match self {
            RowKind::Message => &[],
            RowKind::Thread => &["depth"],
            RowKind::Hit => &["match", "snippet"],
        }
    }

    fn compact_extra(self) -> &'static [&'static str] {
        match self {
            RowKind::Message => &[],
            RowKind::Thread => &["depth"],
            RowKind::Hit => &["match"],
        }
    }
}

/// The keys a listing row keeps: every key, or the ones asked for and always `account`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Fields(Option<Vec<String>>);

/// A name `Fields::parse` does not know.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown field '{name}'; fields are compact, {}", allowed.join(", "))]
pub struct UnknownField {
    pub name: String,
    pub allowed: Vec<&'static str>,
}

impl Fields {
    /// The keys `names` asks for, where `compact` stands for the usual few; no names keeps every key.
    pub fn parse(names: &[String], kind: RowKind) -> Result<Fields, UnknownField> {
        if names.is_empty() {
            return Ok(Fields(None));
        }
        let allowed: Vec<&'static str> =
            MESSAGE_FIELDS.iter().chain(kind.extra()).copied().collect();
        let mut keep = vec!["account".to_string()];
        for name in names.iter().map(|n| n.trim()) {
            let named: Vec<&str> = if name == "compact" {
                COMPACT
                    .iter()
                    .chain(kind.compact_extra())
                    .copied()
                    .collect()
            } else if allowed.contains(&name) {
                vec![name]
            } else {
                return Err(UnknownField {
                    name: name.to_string(),
                    allowed,
                });
            };
            for key in named {
                if !keep.iter().any(|k| k == key) {
                    keep.push(key.to_string());
                }
            }
        }
        Ok(Fields(Some(keep)))
    }

    /// `row` with only the kept keys.
    pub fn apply(&self, mut row: Value) -> Value {
        if let (Some(keep), Some(object)) = (&self.0, row.as_object_mut()) {
            object.retain(|key, _| keep.iter().any(|k| k == key));
        }
        row
    }
}

/// The row of a `list` page, with the kept `fields`.
pub fn list_row(row: &crate::page::ListRow, fields: &Fields) -> serde_json::Result<Value> {
    Ok(fields.apply(message(&row.account, &row.message, row.depth)?))
}

/// The row of a `search` page, with the kept `fields`.
pub fn search_row(row: &crate::page::SearchRow, fields: &Fields) -> serde_json::Result<Value> {
    Ok(fields.apply(message(&row.account, &row.hit, None)?))
}

/// What a count of `list` or `search` returns.
pub fn count(count: u64) -> Value {
    json!({ "count": count })
}

/// The last line of a `--json` page that has a next one: the cursor to pass back.
pub fn next(cursor: &str) -> Value {
    json!({ "next": cursor })
}

/// A message's headers: its row without body text, and every header field in order; `headers --json` and the MCP
/// `headers` tool share it.
pub fn headers(account: &str, msg: &Message, fields: &[HeaderField]) -> serde_json::Result<Value> {
    Ok(json!({ "message": message(account, msg, None)?, "headers": fields }))
}

/// What `show --json` and the MCP `show` tool return: the message row, its plain `body` and whether the body was cut.
pub fn show(
    account: &str,
    msg: &Message,
    body: &str,
    truncated: bool,
) -> serde_json::Result<Value> {
    Ok(json!({ "message": message(account, msg, None)?, "body": body, "truncated": truncated }))
}

/// A rule's listing row: its whole definition as `rules propose` takes it, and `position`, its 0-based place in
/// rules.toml, where an earlier rule runs first. `account` is the rule's own scope, `null` for every account.
pub fn rule(rule: &Rule, position: usize) -> serde_json::Result<Value> {
    let mut row = serde_json::to_value(rule)?;
    if let Value::Object(map) = &mut row {
        map.insert("position".into(), position.into());
        for (key, value) in [
            ("account", &rule.account),
            ("folder", &rule.folder),
            ("proposed_by", &rule.proposed_by),
        ] {
            map.insert(key.into(), json!(value));
        }
    }
    Ok(row)
}

/// One row of a dry run: what `rule` would do to the message.
pub fn planned_row(account: &str, planned: &Planned) -> Value {
    json!({
        "account": account, "rule": planned.rule, "folder": planned.message.folder,
        "uid": planned.message.uid, "action": planned.action.label(), "subject": planned.message.subject,
    })
}

/// One group of sender statistics, in `folder` of `account`.
pub fn sender(account: &str, folder: &str, sender: &Sender) -> serde_json::Result<Value> {
    let mut row = with_account(account, sender)?;
    row["folder"] = folder.into();
    Ok(row)
}

/// Rules coverage of `folder` in `account`: the counts, the unmatched mail's top senders and the contested messages,
/// each a message row with every matching rule and what would act.
pub fn coverage(account: &str, folder: &str, coverage: &Coverage) -> serde_json::Result<Value> {
    let contested = coverage
        .contests
        .iter()
        .map(|contest| {
            Ok(json!({
                "message": message(account, &contest.message, None)?,
                "rules": contest.rules,
                "acts": contest.acts,
                "fails": contest.fails,
            }))
        })
        .collect::<serde_json::Result<Vec<Value>>>()?;
    Ok(json!({
        "account": account, "folder": folder, "messages": coverage.messages, "matched": coverage.matched,
        "unmatched": coverage.unmatched, "contested": coverage.contested,
        "unmatched_senders": coverage.unmatched_senders, "contested_messages": contested,
    }))
}

/// The daemon's status as it serialises; the CLI's `daemon status --json` and the MCP `status` tool share it.
pub fn status(status: &Status) -> serde_json::Result<Value> {
    serde_json::to_value(status)
}

/// Every string in `value` without control or invisible characters, so an agent reading `--json` or an MCP result sees no hidden text; layout (newlines, tabs) is kept below a key named `body` or `body_text`.
pub fn clean_value(value: Value, keep_layout: bool) -> Value {
    match value {
        Value::String(s) => Value::String(clean_for_agent(&s, keep_layout)),
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|item| clean_value(item, keep_layout))
                .collect(),
        ),
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, v)| {
                    let layout = key == "body" || key == "body_text";
                    (key, clean_value(v, layout))
                })
                .collect(),
        ),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Message;

    fn message(uid: u32, refs: Option<&str>) -> Message {
        Message {
            folder: "INBOX".into(),
            uid,
            refs: refs.map(str::to_string),
            thread_id: "t".into(),
            ..Default::default()
        }
    }

    #[test]
    fn rule_rows_carry_the_whole_rule_and_its_position() {
        let rule: Rule = serde_json::from_str(
            r#"{"name": "a", "match": {"subject": {"contains": "x"}}, "actions": {"move": "X"}}"#,
        )
        .unwrap();
        assert_eq!(
            super::rule(&rule, 3).unwrap(),
            serde_json::json!({
                "name": "a", "position": 3, "enabled": true, "account": null, "folder": null, "proposed_by": null,
                "match": {"subject": {"contains": "x"}}, "actions": {"move": "X"},
            })
        );
    }

    #[test]
    fn with_account_adds_the_account_key() {
        let row = with_account("work", &serde_json::json!({ "uid": 1 })).unwrap();
        assert_eq!(row, serde_json::json!({ "account": "work", "uid": 1 }));
    }

    #[test]
    fn message_rows_carry_the_account_and_depth_but_no_body() {
        let msg = Message {
            uid: 4,
            body_text: Some("secret".into()),
            ..Default::default()
        };
        let row = super::message("work", &msg, Some(2)).unwrap();
        assert_eq!(
            (row["account"].as_str(), row["uid"].as_u64()),
            (Some("work"), Some(4))
        );
        assert_eq!(row["depth"], 2);
        assert!(row.get("body_text").is_none());
        assert!(
            super::message("work", &msg, None)
                .unwrap()
                .get("depth")
                .is_none()
        );
    }

    #[test]
    fn the_field_list_names_every_key_of_a_message_row() {
        let row = super::message("work", &Message::default(), None).unwrap();
        let mut keys: Vec<&str> = row
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        let mut known = MESSAGE_FIELDS.to_vec();
        known.sort_unstable();
        assert_eq!(keys, known);
    }

    #[test]
    fn fields_keep_what_was_asked_and_always_the_account() {
        let names = |list: &[&str]| list.iter().map(|n| n.to_string()).collect::<Vec<_>>();
        let msg = Message {
            uid: 4,
            subject: Some("Hi".into()),
            ..Default::default()
        };
        let row = super::message("work", &msg, Some(1)).unwrap();
        let fields = Fields::parse(&names(&["uid", "subject"]), RowKind::Message).unwrap();
        assert_eq!(
            fields.apply(row.clone()),
            serde_json::json!({ "account": "work", "uid": 4, "subject": "Hi" })
        );
        let compact = Fields::parse(&names(&["compact"]), RowKind::Thread).unwrap();
        let kept = compact.apply(row.clone());
        let mut keys: Vec<&str> = kept
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "account",
                "date",
                "depth",
                "flags",
                "folder",
                "from_addr",
                "subject",
                "uid"
            ]
        );
        assert_eq!(
            Fields::parse(&[], RowKind::Message)
                .unwrap()
                .apply(row.clone()),
            row
        );
        let with_snippet = Fields::parse(&names(&["compact", "snippet"]), RowKind::Hit).unwrap();
        assert_eq!(
            with_snippet,
            Fields(Some(names(&[
                "account",
                "folder",
                "uid",
                "date",
                "from_addr",
                "subject",
                "flags",
                "match",
                "snippet"
            ])))
        );
        let unknown = Fields::parse(&names(&["match"]), RowKind::Message).unwrap_err();
        assert_eq!(unknown.name, "match");
        assert!(
            unknown
                .to_string()
                .starts_with("unknown field 'match'; fields are compact, account, folder")
        );
        assert!(Fields::parse(&names(&["body_text"]), RowKind::Hit).is_err());
    }

    #[test]
    fn show_has_the_message_with_its_account_a_body_and_truncated() {
        let msg = Message {
            uid: 4,
            body_text: Some("indexed".into()),
            ..Default::default()
        };
        let shown = show("work", &msg, "hello", false).unwrap();
        assert_eq!(
            shown["message"],
            super::message("work", &msg, None).unwrap()
        );
        assert_eq!(shown["message"]["account"], "work");
        assert_eq!(shown["body"], "hello");
        assert_eq!(shown["truncated"], false);
        assert_eq!(shown.as_object().unwrap().len(), 3);
    }

    #[test]
    fn depths_are_relative_to_the_shallowest_and_capped_at_four() {
        let thread = vec![
            message(1, Some("a")),
            message(2, Some("a b")),
            message(3, Some("a b c d e f g")),
        ];
        assert_eq!(depths(&thread), vec![0, 1, 4]);
    }

    #[test]
    fn headers_rows_leave_out_the_body_and_list_every_field() {
        let message = Message {
            uid: 4,
            body_text: Some("secret".into()),
            ..Default::default()
        };
        let fields = [HeaderField {
            name: "List-Id".into(),
            value: "x.github.com".into(),
        }];
        let row = headers("work", &message, &fields).unwrap();
        assert_eq!(row["message"]["account"], "work");
        assert_eq!(row["message"]["uid"], 4);
        assert!(row["message"].get("body_text").is_none());
        assert_eq!(
            row["headers"],
            serde_json::json!([{ "name": "List-Id", "value": "x.github.com" }])
        );
    }

    #[test]
    fn planned_rows_name_the_rule_folder_uid_and_action() {
        let planned = Planned {
            rule: "codes".into(),
            message: Message {
                folder: "INBOX".into(),
                uid: 7,
                subject: Some("Your code".into()),
                ..Default::default()
            },
            action: crate::rules::Action::Flag,
        };
        assert_eq!(
            planned_row("work", &planned),
            serde_json::json!({ "account": "work", "rule": "codes", "folder": "INBOX", "uid": 7, "action": "flagged", "subject": "Your code" })
        );
    }

    #[test]
    fn clean_value_strips_hidden_text_and_keeps_body_layout() {
        let hidden: String = "ignore"
            .chars()
            .map(|c| char::from_u32(0xE0000 + c as u32).unwrap())
            .collect();
        let row = serde_json::json!({
            "subject": format!("Invoice{hidden}\u{200B}\u{202E}"),
            "keys": [{ "name": format!("a\n{hidden}b") }],
            "body_text": format!("line\n\tnext{hidden}"),
            "uid": 3,
        });
        assert_eq!(
            clean_value(row, false),
            serde_json::json!({ "subject": "Invoice", "keys": [{ "name": "ab" }], "body_text": "line\n\tnext", "uid": 3 })
        );
    }
}
