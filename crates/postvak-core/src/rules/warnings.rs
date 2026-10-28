//! What leaves rules.toml valid but not doing what its author meant: a conflicting copy a file sync left beside it,
//! a rule for an account this device does not have, a rule watching a folder no account syncs, a rule moving mail to
//! the folder it watches, and a move to a special folder's usual name where the account keeps that role elsewhere.
use std::fs;
use std::path::Path;

use super::{Action, Rule};
use crate::paths::Paths;
use crate::store::{Folder, Store};

/// An account set up here, and the folders it syncs; `synced` is `None` until a pass went through every folder.
#[derive(Debug, Clone, PartialEq)]
pub struct AccountFolders {
    pub name: String,
    pub synced: Option<Vec<Folder>>,
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
            if let Action::Move(target) = action {
                lines.extend(role_by_name(rule, target, accounts));
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
            .any(|f| same_folder(&f.name, watched))
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

/// The special-use role a folder of this name usually has, by its last level: `move = "Trash"` on Gmail creates a
/// folder beside `[Gmail]/Trash`, and `move = "Junk"` on Outlook one beside `Junk Email`.
fn role_of(target: &str) -> Option<&'static str> {
    let last = target.rsplit('/').next().unwrap_or(target).to_lowercase();
    Some(match last.as_str() {
        "trash" => "Trash",
        "junk" | "spam" => "Junk",
        "archive" => "Archive",
        "sent" => "Sent",
        "drafts" => "Drafts",
        _ => return None,
    })
}

/// A line per account the rule is for where `target` is not a folder yet and its name is a special folder's, while
/// the account keeps that role in another folder.
fn role_by_name(rule: &Rule, target: &str, accounts: &[AccountFolders]) -> Vec<String> {
    let Some(role) = role_of(target) else {
        return Vec::new();
    };
    accounts
        .iter()
        .filter(|a| rule.account.as_deref().is_none_or(|name| name == a.name))
        .filter_map(|account| {
            let synced = account.synced.as_ref()?;
            if synced.iter().any(|f| same_folder(&f.name, target)) {
                return None;
            }
            let special = synced
                .iter()
                .find(|f| f.special_use.as_deref() == Some(role))?;
            let instead = if role == "Archive" {
                "use the archive action instead".to_string()
            } else {
                format!("move to '{}' instead", special.name)
            };
            Some(format!(
                "rule '{}' moves mail to '{target}', a new folder in account '{}', whose {} folder is '{}'; {instead}",
                rule.name,
                account.name,
                role.to_lowercase(),
                special.name
            ))
        })
        .collect()
}

/// INBOX is the same folder in any case (RFC 3501 §5.1); other names may differ by case alone on some servers.
fn same_folder(a: &str, b: &str) -> bool {
    super::inbox_spelled(a) == super::inbox_spelled(b)
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
        with_roles(
            name,
            &folders.iter().map(|&f| (f, None)).collect::<Vec<_>>(),
        )
    }

    fn with_roles(name: &str, folders: &[(&str, Option<&str>)]) -> AccountFolders {
        AccountFolders {
            name: name.to_string(),
            synced: Some(
                folders
                    .iter()
                    .map(|&(folder, role)| Folder {
                        name: folder.to_string(),
                        uidvalidity: 1,
                        last_uid: 0,
                        special_use: role.map(str::to_string),
                        delimiter: Some("/".into()),
                    })
                    .collect(),
            ),
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
    fn a_move_to_a_special_folders_name_names_the_folder_with_that_role() {
        let dir = tempfile::tempdir().unwrap();
        let rules = parse(
            "[[rules]]\nname = \"bin\"\nmatch.seen = true\nactions = [{ move = \"Trash\" }]\n\n\
             [[rules]]\nname = \"spam\"\nmatch.seen = true\nactions = [{ move = \"Spam\" }]\n\n\
             [[rules]]\nname = \"keep\"\nmatch.seen = true\nactions = [{ move = \"Old/archive\" }]\n\n\
             [[rules]]\nname = \"sent\"\nmatch.seen = true\nactions = [{ move = \"Sent\" }]\n\n\
             [[rules]]\nname = \"lists\"\nmatch.seen = true\nactions = [{ move = \"Lists\" }]\n",
        )
        .unwrap()
        .rules;
        let gmail = with_roles(
            "gmail",
            &[
                ("INBOX", None),
                ("[Gmail]/Trash", Some("Trash")),
                ("[Gmail]/Spam", Some("Junk")),
                ("[Gmail]/Archive", Some("Archive")),
            ],
        );
        // A folder named Sent that already exists is what the rule means, whatever its role.
        let own = with_roles(
            "own",
            &[("INBOX", None), ("Sent", None), ("Sent Mail", Some("Sent"))],
        );
        assert_eq!(
            warnings(&dir.path().join("rules.toml"), &rules, &[gmail, own]),
            [
                "rule 'bin' moves mail to 'Trash', a new folder in account 'gmail', whose trash folder is \
                 '[Gmail]/Trash'; move to '[Gmail]/Trash' instead",
                "rule 'spam' moves mail to 'Spam', a new folder in account 'gmail', whose junk folder is \
                 '[Gmail]/Spam'; move to '[Gmail]/Spam' instead",
                "rule 'keep' moves mail to 'Old/archive', a new folder in account 'gmail', whose archive folder is \
                 '[Gmail]/Archive'; use the archive action instead",
            ]
        );
        let first_sync = AccountFolders {
            name: "gmail".into(),
            synced: None,
        };
        assert!(
            warnings(&dir.path().join("rules.toml"), &rules, &[first_sync]).is_empty(),
            "before a complete pass, the target may exist"
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
