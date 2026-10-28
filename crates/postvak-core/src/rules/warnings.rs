//! What leaves rules.toml valid but not doing what its author meant: a conflicting copy a file sync left beside it,
//! a rule for an account this device does not have, a rule watching a folder no account syncs, and a rule moving mail
//! to the folder it watches.
use std::fs;
use std::path::Path;

use super::{Action, Rule};
use crate::paths::Paths;
use crate::store::Store;

/// An account set up here, and the folders it syncs; `synced` is `None` until a pass went through every folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountFolders {
    pub name: String,
    pub synced: Option<Vec<String>>,
}

/// What `warnings` needs of each account, read from the stores. An account without a store, or whose store does not
/// open, has not synced yet as far as the warnings go: better silent than wrong.
pub fn account_folders(paths: &Paths, names: &[&str]) -> Vec<AccountFolders> {
    names
        .iter()
        .map(|&name| {
            let db = paths.mail_db(name);
            // Opening creates a missing store, and a check should leave no files behind.
            let synced = db
                .exists()
                .then(|| Store::open(&db).and_then(|store| store.synced_folders()))
                .and_then(|read| {
                    read.unwrap_or_else(|e| {
                        log::debug!("{name}: could not read the synced folders: {e}");
                        None
                    })
                });
            AccountFolders {
                name: name.to_string(),
                synced,
            }
        })
        .collect()
}

/// One line per problem: conflicting copies by name, then rules in file order; empty when there is nothing to say.
pub fn warnings(path: &Path, rules: &[Rule], accounts: &[AccountFolders]) -> Vec<String> {
    let mut lines: Vec<String> = conflict_copies(path)
        .into_iter()
        .map(|name| {
            format!(
                "{name} looks like a conflicting copy left by a file sync; move what you want to keep into rules.toml \
                 and delete it"
            )
        })
        .collect();
    for rule in rules {
        if let Some(account) = rule.account.as_deref()
            && !accounts.iter().any(|a| a.name == account)
        {
            lines.push(format!(
                "rule '{}' is for account '{account}', which is not set up here, so it never runs",
                rule.name
            ));
        }
        let watched = rule.folder.as_deref().unwrap_or("INBOX");
        if let Some(line) = unsynced_folder(rule, watched, accounts) {
            lines.push(line);
        }
        for action in &rule.actions {
            if let Action::Move(target) = action
                && same_folder(target, watched)
            {
                lines.push(format!(
                    "rule '{}' moves mail to '{target}', the folder it watches, so the move does nothing",
                    rule.name
                ));
            }
        }
    }
    lines
}

/// The rule's watched folder, when every account it is for has synced all its folders and none of them is that one.
/// One account lacking the folder is no warning for a rule shared by every account: it runs on the others.
fn unsynced_folder(rule: &Rule, watched: &str, accounts: &[AccountFolders]) -> Option<String> {
    let mut covered = accounts
        .iter()
        .filter(|a| rule.account.as_deref().is_none_or(|name| name == a.name))
        .peekable();
    covered.peek()?;
    for account in covered {
        if account
            .synced
            .as_ref()?
            .iter()
            .any(|f| same_folder(f, watched))
        {
            return None;
        }
    }
    let whom = match rule.account.as_deref() {
        Some(account) => format!("account '{account}' does not sync"),
        None => "no account syncs".to_string(),
    };
    Some(format!(
        "rule '{}' watches '{watched}', which {whom}, so it never runs",
        rule.name
    ))
}

/// INBOX is the same folder in any case (RFC 3501 §5.1); other names may differ by case alone on some servers.
fn same_folder(a: &str, b: &str) -> bool {
    a == b || (a.eq_ignore_ascii_case("INBOX") && b.eq_ignore_ascii_case("INBOX"))
}

/// Files beside `path` named like a sync tool's conflicting copy of it, sorted: Syncthing's
/// `rules.sync-conflict-20261024-101500-ABCDEFG.toml`, Nextcloud's `rules (conflicted copy 2026-10-24 101500).toml`
/// and Dropbox's `rules (Pieter's conflicted copy 2026-10-24).toml`.
fn conflict_copies(path: &Path) -> Vec<String> {
    let (Some(dir), Some(stem), Some(own)) = (
        path.parent(),
        path.file_stem().and_then(|s| s.to_str()),
        path.file_name().and_then(|s| s.to_str()),
    ) else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter(|name| {
            name != own
                && name.starts_with(stem)
                && name.ends_with(".toml")
                && name.to_lowercase().contains("conflict")
        })
        .collect();
    names.sort();
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::parse;

    const RULES: &str = "[[rules]]\nname = \"codes\"\naccount = \"work\"\nmatch.seen = true\nactions = [\"flag\"]\n\n\
                         [[rules]]\nname = \"all\"\nmatch.seen = true\nactions = [\"flag\"]\n\n\
                         [[rules]]\nname = \"old\"\naccount = \"werk\"\nmatch.seen = true\nactions = [\"flag\"]\n";

    /// Accounts whose first complete pass has not ended yet, so no folder is known to be missing.
    fn unsynced(names: &[&str]) -> Vec<AccountFolders> {
        names
            .iter()
            .map(|name| AccountFolders {
                name: name.to_string(),
                synced: None,
            })
            .collect()
    }

    fn synced(name: &str, folders: &[&str]) -> AccountFolders {
        AccountFolders {
            name: name.to_string(),
            synced: Some(folders.iter().map(|f| f.to_string()).collect()),
        }
    }

    #[test]
    fn a_rule_for_an_account_not_set_up_here_is_named() {
        let dir = tempfile::tempdir().unwrap();
        let rules = parse(RULES).unwrap().rules;
        assert_eq!(
            warnings(
                &dir.path().join("rules.toml"),
                &rules,
                &unsynced(&["work", "home"])
            ),
            ["rule 'old' is for account 'werk', which is not set up here, so it never runs"]
        );
    }

    #[test]
    fn a_rule_moving_mail_to_the_folder_it_watches_is_named() {
        let dir = tempfile::tempdir().unwrap();
        let rules = parse(
            "[[rules]]\nname = \"lists\"\nfolder = \"Lists\"\nmatch.seen = true\nactions = [{ move = \"Lists\" }]\n\n\
             [[rules]]\nname = \"inbox\"\nmatch.seen = true\nactions = [\"mark_read\", { move = \"Inbox\" }]\n\n\
             [[rules]]\nname = \"case\"\nfolder = \"Lists\"\nmatch.seen = true\nactions = [{ move = \"lists\" }]\n\n\
             [[rules]]\nname = \"away\"\nmatch.seen = true\nactions = [{ move = \"Lists\" }]\n",
        )
        .unwrap()
        .rules;
        assert_eq!(
            warnings(&dir.path().join("rules.toml"), &rules, &[]),
            [
                "rule 'lists' moves mail to 'Lists', the folder it watches, so the move does nothing",
                "rule 'inbox' moves mail to 'Inbox', the folder it watches, so the move does nothing",
            ],
            "only INBOX is the same in any case: 'lists' may be another folder than 'Lists'"
        );
    }

    #[test]
    fn a_rule_watching_a_folder_no_account_syncs_is_named_once_every_account_synced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rules.toml");
        let rules = parse(
            "[[rules]]\nname = \"typo\"\nfolder = \"Newsleters\"\nmatch.seen = true\nactions = [\"flag\"]\n\n\
             [[rules]]\nname = \"work lists\"\naccount = \"work\"\nfolder = \"Lists\"\nmatch.seen = true\n\
             actions = [\"flag\"]\n\n\
             [[rules]]\nname = \"shared\"\nfolder = \"Lists\"\nmatch.seen = true\nactions = [\"flag\"]\n\n\
             [[rules]]\nname = \"inbox\"\nfolder = \"Inbox\"\nmatch.seen = true\nactions = [\"flag\"]\n",
        )
        .unwrap()
        .rules;
        let home = synced("home", &["INBOX", "Lists"]);
        let work = synced("work", &["INBOX"]);
        assert_eq!(
            warnings(&path, &rules, &[work.clone(), home.clone()]),
            [
                "rule 'typo' watches 'Newsleters', which no account syncs, so it never runs",
                "rule 'work lists' watches 'Lists', which account 'work' does not sync, so it never runs",
            ],
            "'shared' runs on home; INBOX matches in any case"
        );
        let first_sync = AccountFolders {
            synced: None,
            ..home
        };
        assert_eq!(
            warnings(&path, &rules, &[work, first_sync]),
            [
                "rule 'work lists' watches 'Lists', which account 'work' does not sync, so it never runs"
            ],
            "an account still in its first sync may yet have the folder"
        );
    }

    #[test]
    fn folders_are_read_from_existing_stores_only() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let store = Store::open_account(&paths, "work").unwrap();
        store.set_folders_listed(1).unwrap();
        drop(store);
        assert_eq!(
            account_folders(&paths, &["work", "new"]),
            [synced("work", &[]), unsynced(&["new"]).remove(0)]
        );
        assert!(!paths.mail_db("new").exists(), "a check creates no store");
    }

    #[test]
    fn conflicting_copies_from_sync_tools_are_named() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "rules.toml",
            "rules.sync-conflict-20261024-101500-ABCDEFG.toml",
            "rules (conflicted copy 2026-10-24 101500).toml",
            "rules (Pieter's Conflicted Copy 2026-10-24).toml",
            // Not copies of rules.toml.
            "config.sync-conflict-20261024-101500-ABCDEFG.toml",
            "rules.toml.bak",
            "rules-old.toml",
            ".rules.lock",
        ] {
            fs::write(dir.path().join(name), "").unwrap();
        }
        let rules = parse(RULES).unwrap().rules;
        assert_eq!(
            warnings(
                &dir.path().join("rules.toml"),
                &rules,
                &unsynced(&["work", "werk"])
            ),
            [
                "rules (Pieter's Conflicted Copy 2026-10-24).toml looks like a conflicting copy left by a file \
                 sync; move what you want to keep into rules.toml and delete it",
                "rules (conflicted copy 2026-10-24 101500).toml looks like a conflicting copy left by a file \
                 sync; move what you want to keep into rules.toml and delete it",
                "rules.sync-conflict-20261024-101500-ABCDEFG.toml looks like a conflicting copy left by a file \
                 sync; move what you want to keep into rules.toml and delete it",
            ]
        );
    }

    #[test]
    fn a_missing_directory_has_nothing_to_say() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing/rules.toml");
        assert!(warnings(&path, &[], &[]).is_empty());
    }
}
