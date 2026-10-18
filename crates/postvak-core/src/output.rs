//! JSON rows shared by the CLI's `--json` and the MCP results; each row carries its account.
use serde_json::{Value, json};

use crate::actions::Planned;
use crate::config::AccountConfig;
use crate::daemon::wire::Status;
use crate::rules::Rule;
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

/// A rule's listing row; `account` is the rule's own scope, `null` for every account.
pub fn rule(rule: &Rule) -> Value {
    json!({ "name": rule.name, "enabled": rule.enabled, "proposed_by": rule.proposed_by, "account": rule.account, "folder": rule.folder })
}

/// One row of a dry run: what `rule` would do to the message.
pub fn planned_row(account: &str, planned: &Planned) -> Value {
    json!({
        "account": account, "rule": planned.rule, "folder": planned.message.folder,
        "uid": planned.message.uid, "action": planned.action.label(), "subject": planned.message.subject,
    })
}

/// The outcome of running a rule over an account's existing mail.
pub fn rule_applied(account: &str, evaluated: usize, actions: usize, errors: &[String]) -> Value {
    json!({ "account": account, "evaluated": evaluated, "actions": actions, "errors": errors })
}

/// The daemon's status as it serialises; the CLI's `daemon status --json` and the MCP `status` tool share it.
pub fn status(status: &Status) -> serde_json::Result<Value> {
    serde_json::to_value(status)
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
    fn with_account_adds_the_account_key() {
        let row = with_account("work", &serde_json::json!({ "uid": 1 })).unwrap();
        assert_eq!(row, serde_json::json!({ "account": "work", "uid": 1 }));
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
            serde_json::json!({ "account": "work", "rule": "codes", "folder": "INBOX", "uid": 7, "action": "flag", "subject": "Your code" })
        );
    }

    #[test]
    fn rule_applied_rows_carry_counts_and_errors() {
        assert_eq!(
            rule_applied("work", 12, 3, &["x".to_string()]),
            serde_json::json!({ "account": "work", "evaluated": 12, "actions": 3, "errors": ["x"] })
        );
    }
}
