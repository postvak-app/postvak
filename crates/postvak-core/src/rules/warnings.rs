//! What leaves rules.toml valid but not doing what its author meant: a conflicting copy a file sync left beside it,
//! a rule for an account this device does not have, and a rule moving mail to the folder it watches. `folder_warnings`
//! adds what an account's own folders show: a move target written for another hierarchy or namespace.
use std::fs;
use std::path::Path;

use super::{Action, Rule};
use crate::folder_name;
use crate::paths::Paths;
use crate::store::{Folder, Store};

/// One line per problem: conflicting copies by name, then rules in file order; empty when there is nothing to say.
pub fn warnings(path: &Path, rules: &[Rule], accounts: &[&str]) -> Vec<String> {
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
            && !accounts.contains(&account)
        {
            lines.push(format!(
                "rule '{}' is for account '{account}', which is not set up here, so it never runs",
                rule.name
            ));
        }
        let watched = rule.folder.as_deref().unwrap_or("INBOX");
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

/// An account's folders, as `folder_warnings` reads them from its store.
struct FolderLayout {
    account: String,
    folders: Vec<Folder>,
}

/// What a move target's name gets wrong for the folders of the accounts its rule runs on, read from their stores: see
/// `layout_warnings`. An account without a store, or whose store does not open, has nothing to say yet.
pub fn folder_warnings(paths: &Paths, rules: &[Rule], accounts: &[impl AsRef<str>]) -> Vec<String> {
    let layouts: Vec<FolderLayout> = accounts
        .iter()
        .filter_map(|account| {
            let account = account.as_ref();
            let db = paths.mail_db(account);
            // Opening creates a missing store, and a check should leave no files behind.
            if !db.exists() {
                return None;
            }
            let folders = Store::open(&db)
                .and_then(|store| store.folders())
                .inspect_err(|e| log::debug!("{account}: could not read the folders: {e}"))
                .ok()?;
            Some(FolderLayout {
                account: account.to_string(),
                folders,
            })
        })
        .collect();
    layout_warnings(rules, &layouts)
}

/// One line per move target, in file order, that does not fit an account the rule runs on: a `/` on a server whose
/// hierarchy delimiter is another character, which makes one folder with `/` in its name or is refused (RFC 3501
/// §5.1), or a top-level name on a server that keeps every folder under `INBOX` + delimiter, where creating it
/// usually fails. Neither is an error: the same rules.toml can serve accounts on other servers, and `/` is a legal
/// character in a name on a `.` server. A target the account already has is fine as it is.
fn layout_warnings(rules: &[Rule], layouts: &[FolderLayout]) -> Vec<String> {
    let mut lines = Vec::new();
    for rule in rules {
        let targets = rule.actions.iter().filter_map(|action| match action {
            Action::Move(target) => Some(target.as_str()),
            _ => None,
        });
        for target in targets {
            for layout in layouts
                .iter()
                .filter(|l| rule.account.as_deref().is_none_or(|a| a == l.account))
            {
                lines.extend(layout_warning(&rule.name, target, layout));
            }
        }
    }
    lines
}

fn layout_warning(rule: &str, target: &str, layout: &FolderLayout) -> Option<String> {
    // A move's placeholder is not the server's word on anything.
    let synced: Vec<&Folder> = layout
        .folders
        .iter()
        .filter(|f| f.uidvalidity != 0)
        .collect();
    if target.eq_ignore_ascii_case("INBOX")
        || synced.iter().any(|f| folder_name::same(&f.name, target))
    {
        return None;
    }
    let account = &layout.account;
    let delimiter = delimiter(&synced)?;
    if delimiter != "/" && target.contains('/') {
        return Some(format!(
            "rule '{rule}' moves mail to '{target}', but account '{account}' separates folder levels with \
             '{delimiter}', so the server makes one folder with '/' in its name or refuses it; write '{}' for a \
             subfolder",
            target.replace('/', delimiter)
        ));
    }
    let others = synced
        .iter()
        .filter(|f| !f.name.eq_ignore_ascii_case("INBOX"))
        .map(|f| (f.name.as_str(), f.delimiter.as_deref()));
    if folder_name::under_inbox(others)
        && folder_name::inbox_prefix(target, Some(delimiter)).is_none()
    {
        return Some(format!(
            "rule '{rule}' moves mail to '{target}', but every folder of account '{account}' is under \
             'INBOX{delimiter}', where the server usually wants new folders too; write 'INBOX{delimiter}{target}'"
        ));
    }
    None
}

/// The account's hierarchy delimiter: INBOX's, else the first folder's that has one.
fn delimiter<'a>(synced: &[&'a Folder]) -> Option<&'a str> {
    let of = |f: &&'a Folder| f.delimiter.as_deref().filter(|d| !d.is_empty());
    synced
        .iter()
        .find(|f| f.name.eq_ignore_ascii_case("INBOX"))
        .and_then(of)
        .or_else(|| synced.iter().find_map(of))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::parse;

    const RULES: &str = "[[rules]]\nname = \"codes\"\naccount = \"work\"\nmatch.seen = true\nactions = [\"flag\"]\n\n\
                         [[rules]]\nname = \"all\"\nmatch.seen = true\nactions = [\"flag\"]\n\n\
                         [[rules]]\nname = \"old\"\naccount = \"werk\"\nmatch.seen = true\nactions = [\"flag\"]\n";

    #[test]
    fn a_rule_for_an_account_not_set_up_here_is_named() {
        let dir = tempfile::tempdir().unwrap();
        let rules = parse(RULES).unwrap().rules;
        assert_eq!(
            warnings(&dir.path().join("rules.toml"), &rules, &["work", "home"]),
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
            warnings(&dir.path().join("rules.toml"), &rules, &["work", "werk"]),
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

    fn layout(account: &str, delimiter: &str, names: &[&str]) -> FolderLayout {
        FolderLayout {
            account: account.into(),
            folders: names
                .iter()
                .map(|name| Folder {
                    name: name.to_string(),
                    uidvalidity: 1,
                    last_uid: 0,
                    special_use: None,
                    delimiter: Some(delimiter.into()),
                })
                .collect(),
        }
    }

    fn moves(targets: &[&str]) -> Vec<Rule> {
        let rules: String = targets
            .iter()
            .enumerate()
            .map(|(i, target)| {
                format!("[[rules]]\nname = \"r{i}\"\nmatch.seen = true\nactions = [{{ move = \"{target}\" }}]\n\n")
            })
            .collect();
        parse(&rules).unwrap().rules
    }

    #[test]
    fn a_slash_on_a_server_with_another_delimiter_is_named() {
        let rules = moves(&["Folder/Sub", "Plain", "Known/Sub"]);
        let dot = layout("courier", ".", &["INBOX", "Known/Sub", "Work"]);
        let slash = layout("dovecot", "/", &["INBOX", "Work"]);
        assert_eq!(
            layout_warnings(&rules, &[dot, slash]),
            [
                "rule 'r0' moves mail to 'Folder/Sub', but account 'courier' separates folder levels with '.', so \
                 the server makes one folder with '/' in its name or refuses it; write 'Folder.Sub' for a subfolder"
            ],
            "a folder the account has is fine as it is"
        );
    }

    #[test]
    fn a_top_level_target_on_a_server_keeping_folders_under_inbox_is_named() {
        let rules = moves(&["Lists", "INBOX.Lists", "inbox.Work", "Inbox"]);
        let namespaced = layout("courier", ".", &["INBOX", "INBOX.Sent", "INBOX.Work"]);
        let mixed = layout("other", ".", &["INBOX", "INBOX.Sent", "Work"]);
        let empty = layout("new", ".", &["INBOX"]);
        assert_eq!(
            layout_warnings(&rules, &[namespaced, mixed, empty]),
            [
                "rule 'r0' moves mail to 'Lists', but every folder of account 'courier' is under 'INBOX.', where \
                 the server usually wants new folders too; write 'INBOX.Lists'"
            ]
        );
    }

    #[test]
    fn a_rule_for_one_account_is_checked_against_that_account_only() {
        let rules = parse(
            "[[rules]]\nname = \"home\"\naccount = \"home\"\nmatch.seen = true\nactions = [{ move = \"A/B\" }]\n",
        )
        .unwrap()
        .rules;
        assert!(layout_warnings(&rules, &[layout("work", ".", &["INBOX"])]).is_empty());
        assert_eq!(
            layout_warnings(&rules, &[layout("home", ".", &["INBOX"])]).len(),
            1
        );
    }

    #[test]
    fn placeholders_and_accounts_without_a_store_say_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let store = Store::open_account(&paths, "work").unwrap();
        // A folder a move created, before its first sync: not the server's word on the delimiter.
        store
            .upsert_folder(&Folder {
                name: "Lists".into(),
                uidvalidity: 0,
                last_uid: 0,
                special_use: None,
                delimiter: Some(".".into()),
            })
            .unwrap();
        drop(store);
        let rules = moves(&["A/B"]);
        assert!(folder_warnings(&paths, &rules, &["work", "new"]).is_empty());
        assert!(!paths.mail_db("new").exists(), "a check creates no store");
        let store = Store::open_account(&paths, "work").unwrap();
        store
            .upsert_folder(&layout("work", ".", &["INBOX"]).folders[0])
            .unwrap();
        drop(store);
        assert_eq!(folder_warnings(&paths, &rules, &["work"]).len(), 1);
    }
}
