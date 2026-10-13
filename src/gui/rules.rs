//! The Rules, Activity and Trash views: what replaces the list and body columns.
use std::io::Read;
use std::path::Path;

use crate::message::parse_headers;
use crate::paths::Paths;
use crate::rules::{self, Rule, RuleFile};
use crate::store::LogEntry;
use crate::trash::{Trash, TrashEntry};

use super::state::Account;

/// Entries shown in the Activity view.
const LOG_LIMIT: u32 = 500;

/// How much of a backup is read to find its sender and subject.
const HEADER_BYTES: u64 = 64 * 1024;

pub(crate) struct RulesState {
    pub error: Option<String>,
    pub rules: Vec<Rule>,
}

impl RulesState {
    /// The file's rules, or the previous ones when it does not parse. A rule that does not compile is reported but
    /// still listed, so it can be switched off.
    pub fn load(path: &Path, previous: Vec<Rule>) -> RulesState {
        match rules::load(path) {
            Ok(file) => RulesState {
                error: rules::compile(&file).err().map(|e| e.to_string()),
                rules: file.rules,
            },
            Err(e) => RulesState {
                error: Some(e.to_string()),
                rules: previous,
            },
        }
    }
}

pub(crate) fn is_pending(rule: &Rule) -> bool {
    !rule.enabled && rule.proposed_by.is_some()
}

pub(crate) fn toml_source(rule: &Rule) -> String {
    toml::to_string(&RuleFile {
        rules: vec![rule.clone()],
    })
    .unwrap_or_else(|e| e.to_string())
}

pub(crate) struct TrashRow {
    pub account: usize,
    pub entry: TrashEntry,
    pub from: String,
    pub subject: String,
}

/// Every account's rule and action log, newest first.
pub(crate) fn activity_log(accounts: &[Account]) -> Vec<(String, LogEntry)> {
    let mut entries = Vec::new();
    for account in accounts {
        let Ok(store) = &account.store else { continue };
        match store.log(LOG_LIMIT) {
            Ok(found) => {
                entries.extend(found.into_iter().map(|entry| (account.name.clone(), entry)));
            }
            Err(e) => log::warn!("[{}] could not read the log: {e}", account.name),
        }
    }
    entries.sort_by_key(|(_, entry)| std::cmp::Reverse(entry.at));
    entries.truncate(LOG_LIMIT as usize);
    entries
}

/// Every account's `.eml` backups, newest first, with sender and subject from their headers.
pub(crate) fn trash_rows(accounts: &[Account], paths: &Paths) -> Vec<TrashRow> {
    let mut rows = Vec::new();
    for (index, account) in accounts.iter().enumerate() {
        match Trash::new(paths.trash_dir(&account.name)).list() {
            Ok(entries) => rows.extend(entries.into_iter().map(|entry| {
                let (from, subject) = eml_summary(&entry.path);
                TrashRow {
                    account: index,
                    entry,
                    from,
                    subject,
                }
            })),
            Err(e) => log::warn!("[{}] could not list the trash: {e}", account.name),
        }
    }
    rows.sort_by_key(|row| std::cmp::Reverse(row.entry.saved_at));
    rows
}

/// Sender and subject of a backup; reads only the start of the file.
fn eml_summary(path: &Path) -> (String, String) {
    let mut head = Vec::new();
    if let Ok(file) = std::fs::File::open(path) {
        let _ = file.take(HEADER_BYTES).read_to_end(&mut head);
    }
    let parsed = parse_headers(&head);
    (
        parsed.from.unwrap_or_default(),
        parsed.subject.unwrap_or_default(),
    )
}
