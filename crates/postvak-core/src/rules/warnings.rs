//! What leaves rules.toml valid but not doing what its author meant: a conflicting copy a file sync left beside it,
//! a rule for an account this device does not have, a rule moving mail to the folder it watches, a rule using
//! `authenticated` for an account that trusts no server's `Authentication-Results`, and a folder written the way IMAP
//! spells it on the wire, as Postvak showed non-ASCII names before it decoded them. `folder_warnings` adds what an
//! account's own folders show: a move target written for another hierarchy or namespace. `watched_folder_warnings`
//! adds a rule watching a folder no account it is for syncs, and a move to a special folder's usual name where the
//! account keeps that role in another folder, once those accounts have synced all their folders.
use std::fs;
use std::path::Path;

use super::{Action, Match, Rule};
use crate::folder_name;
use crate::paths::Paths;
use crate::store::{Folder, Store};

/// One line per problem: conflicting copies by name, then rules in file order; empty when there is nothing to say.
/// `accounts` are the accounts set up here, and `untrusting` those that trust no authserv-id
/// ([`Identity::can_authenticate`](crate::config::Identity::can_authenticate)).
pub fn warnings(
    path: &Path,
    rules: &[Rule],
    accounts: &[&str],
    untrusting: &[&str],
) -> Vec<String> {
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
        if uses_authenticated(&rule.matches) {
            for account in untrusting
                .iter()
                .filter(|a| rule.account.as_deref().is_none_or(|own| own == **a))
            {
                lines.push(format!(
                    "rule '{}' uses authenticated, but account '{account}' trusts no server's \
                     Authentication-Results, so there authenticated = true never holds and false always does; list \
                     your provider's server in its trusted_authserv_ids",
                    rule.name
                ));
            }
        }
        let named =
            rule.folder
                .iter()
                .chain(rule.actions.iter().filter_map(|action| match action {
                    Action::Move(target) => Some(target),
                    _ => None,
                }));
        for name in named {
            if let Some(plain) = folder_name::wire_spelling(name) {
                lines.push(format!(
                    "rule '{}' names folder '{name}', which is how IMAP spells '{plain}' on the wire; Postvak now \
                     takes folder names in plain text, and finds '{plain}' by the old spelling only while the account \
                     has that folder: write '{plain}'",
                    rule.name
                ));
            }
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

/// Whether `authenticated` is a condition, at any depth of `any` and `none`.
fn uses_authenticated(m: &Match) -> bool {
    m.authenticated.is_some()
        || [&m.any, &m.none]
            .into_iter()
            .flatten()
            .flatten()
            .any(uses_authenticated)
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

/// An account's folders, as `folder_warnings` reads them from its store.
struct FolderLayout {
    account: String,
    folders: Vec<Folder>,
}

/// What a move target's name gets wrong for the folders of the accounts its rule runs on, read from their stores: see
/// `layout_warnings`. An account without a store, or whose store does not open, has nothing to say yet. Each store is
/// opened, so a caller holding them open uses [`open_folder_warnings`] instead.
pub fn folder_warnings(paths: &Paths, rules: &[Rule], accounts: &[impl AsRef<str>]) -> Vec<String> {
    let stores: Vec<(&str, Store)> = accounts
        .iter()
        .filter_map(|account| {
            let account = account.as_ref();
            let db = paths.mail_db(account);
            // Opening creates a missing store, and a check should leave no files behind.
            if !db.exists() {
                return None;
            }
            Store::open(&db)
                .inspect_err(|e| log::debug!("{account}: could not open the store: {e}"))
                .ok()
                .map(|store| (account, store))
        })
        .collect();
    let open: Vec<(&str, &Store)> = stores
        .iter()
        .map(|(account, store)| (*account, store))
        .collect();
    open_folder_warnings(rules, &open)
}

/// [`folder_warnings`] over stores already open, as (account, store).
pub fn open_folder_warnings(rules: &[Rule], stores: &[(&str, &Store)]) -> Vec<String> {
    let layouts: Vec<FolderLayout> = stores
        .iter()
        .filter_map(|(account, store)| {
            let folders = store
                .folders()
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

/// An account set up here, and the folders it syncs; `synced` is `None` until a pass without errors went through
/// every folder the server listed, since before that a folder may only not be reached yet.
#[derive(Debug, Clone, PartialEq)]
pub struct AccountFolders {
    pub name: String,
    pub synced: Option<Vec<Folder>>,
}

impl AccountFolders {
    /// What `store` knows of account `name`'s folders. Without a store, or when it cannot be read, nothing is known
    /// yet: better silent than wrong.
    pub fn read(name: &str, store: Option<&Store>) -> AccountFolders {
        let synced = store.and_then(|store| {
            store
                .synced_folders()
                .inspect_err(|e| log::debug!("{name}: could not read the synced folders: {e}"))
                .ok()
                .flatten()
        });
        AccountFolders {
            name: name.to_string(),
            synced,
        }
    }
}

/// [`AccountFolders::read`] for each of `names`, opening only the stores that exist, so a check creates no files.
pub fn account_folders(paths: &Paths, names: &[impl AsRef<str>]) -> Vec<AccountFolders> {
    names
        .iter()
        .map(|name| {
            let name = name.as_ref();
            let db = paths.mail_db(name);
            let store = db
                .exists()
                .then(|| {
                    Store::open(&db)
                        .inspect_err(|e| log::debug!("{name}: could not open the store: {e}"))
                        .ok()
                })
                .flatten();
            AccountFolders::read(name, store.as_ref())
        })
        .collect()
}

/// One line per rule, in file order, watching a folder that no account it is for syncs, such as a misspelled name:
/// only once every one of those accounts has synced all its folders. A rule for every account that one account can
/// use stays quiet, since it runs there. INBOX matches in any case, and a folder as the engine finds it
/// ([`folder_name::rule_names`]). After each rule's line come those of [`role_by_name`] for its moves.
pub fn watched_folder_warnings(rules: &[Rule], accounts: &[AccountFolders]) -> Vec<String> {
    let mut lines = Vec::new();
    for rule in rules {
        lines.extend(unsynced_folder(rule, accounts));
        for action in &rule.actions {
            if let Action::Move(target) = action {
                lines.extend(role_by_name(rule, target, accounts));
            }
        }
    }
    lines
}

fn unsynced_folder(rule: &Rule, accounts: &[AccountFolders]) -> Option<String> {
    let watched = rule.folder.as_deref().unwrap_or("INBOX");
    let mut covered = accounts
        .iter()
        .filter(|a| rule.account.as_deref().is_none_or(|name| name == a.name))
        .peekable();
    // A rule for an account not set up here has its own warning.
    covered.peek()?;
    for account in covered {
        if account.synced.as_ref()?.iter().any(|folder| {
            same_folder(&folder.name, watched) || folder_name::rule_names(watched, &folder.name)
        }) {
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
/// the account keeps that role in another folder: only once that account has synced all its folders.
fn role_by_name(rule: &Rule, target: &str, accounts: &[AccountFolders]) -> Vec<String> {
    let Some(role) = role_of(target) else {
        return Vec::new();
    };
    accounts
        .iter()
        .filter(|a| rule.account.as_deref().is_none_or(|name| name == a.name))
        .filter_map(|account| {
            let synced = account.synced.as_ref()?;
            if synced
                .iter()
                .any(|f| same_folder(&f.name, target) || folder_name::rule_names(target, &f.name))
            {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::parse;

    const RULES: &str = "[[rules]]\nname = \"codes\"\naccount = \"work\"\nmatch.read = true\nactions = { flagged = true }\n\n\
                         [[rules]]\nname = \"all\"\nmatch.read = true\nactions = { flagged = true }\n\n\
                         [[rules]]\nname = \"old\"\naccount = \"werk\"\nmatch.read = true\nactions = { flagged = true }\n";

    #[test]
    fn a_rule_for_an_account_not_set_up_here_is_named() {
        let dir = tempfile::tempdir().unwrap();
        let rules = parse(RULES).unwrap().rules;
        assert_eq!(
            warnings(
                &dir.path().join("rules.toml"),
                &rules,
                &["work", "home"],
                &[]
            ),
            ["rule 'old' is for account 'werk', which is not set up here, so it never runs"]
        );
    }

    #[test]
    fn a_rule_moving_mail_to_the_folder_it_watches_is_named() {
        let dir = tempfile::tempdir().unwrap();
        let rules = parse(
            "[[rules]]\nname = \"lists\"\nfolder = \"Lists\"\nmatch.read = true\nactions = { move = \"Lists\" }\n\n\
             [[rules]]\nname = \"inbox\"\nmatch.read = true\nactions = { read = true, move = \"Inbox\" }\n\n\
             [[rules]]\nname = \"case\"\nfolder = \"Lists\"\nmatch.read = true\nactions = { move = \"lists\" }\n\n\
             [[rules]]\nname = \"away\"\nmatch.read = true\nactions = { move = \"Lists\" }\n",
        )
        .unwrap()
        .rules;
        assert_eq!(
            warnings(&dir.path().join("rules.toml"), &rules, &[], &[]),
            [
                "rule 'lists' moves mail to 'Lists', the folder it watches, so the move does nothing",
                "rule 'inbox' moves mail to 'Inbox', the folder it watches, so the move does nothing",
            ],
            "only INBOX is the same in any case: 'lists' may be another folder than 'Lists'"
        );
    }

    /// Before folder names were decoded the folder list showed `&AMkA6Q-n` for `Één`, and rules were written so.
    #[test]
    fn a_folder_written_in_its_wire_spelling_is_named() {
        let dir = tempfile::tempdir().unwrap();
        let rules = parse(
            "[[rules]]\nname = \"watch\"\nfolder = \"&AMkA6Q-n\"\nmatch.read = true\nactions.flagged = true\n\n\
             [[rules]]\nname = \"move\"\nmatch.read = true\nactions.move = \"Archief/&AMkA6Q-n\"\n\n\
             [[rules]]\nname = \"amp\"\nmatch.read = true\nactions.move = \"Q&-A\"\n\n\
             [[rules]]\nname = \"plain\"\nfolder = \"R&D\"\nmatch.read = true\nactions.move = \"Archief/\u{c9}\u{e9}n\"\n",
        )
        .unwrap()
        .rules;
        assert_eq!(
            warnings(&dir.path().join("rules.toml"), &rules, &[], &[]),
            [("watch", "&AMkA6Q-n", "\u{c9}\u{e9}n"), ("move", "Archief/&AMkA6Q-n", "Archief/\u{c9}\u{e9}n"), ("amp", "Q&-A", "Q&A")]
                .map(|(rule, name, plain)| format!(
                    "rule '{rule}' names folder '{name}', which is how IMAP spells '{plain}' on the wire; Postvak \
                     now takes folder names in plain text, and finds '{plain}' by the old spelling only while the \
                     account has that folder: write '{plain}'"
                )),
            "'R&D' is not modified UTF-7, and a plain name is fine"
        );
    }

    #[test]
    fn a_rule_using_authenticated_for_an_account_that_trusts_no_server_is_named() {
        let dir = tempfile::tempdir().unwrap();
        let rules = parse(
            "[[rules]]\nname = \"junk\"\nmatch.from_domain = \"paypal.com\"\nmatch.authenticated = false\nactions = { flagged = true }\n\n\
             [[rules]]\nname = \"deep\"\naccount = \"work\"\nmatch.read = true\nmatch.none = [{ any = [{ authenticated = true }] }]\nactions = { flagged = true }\n\n\
             [[rules]]\nname = \"home only\"\naccount = \"home\"\nmatch.authenticated = true\nactions = { flagged = true }\n\n\
             [[rules]]\nname = \"plain\"\nmatch.read = true\nactions = { flagged = true }\n",
        )
        .unwrap()
        .rules;
        let tail = "trusts no server's Authentication-Results, so there authenticated = true never holds and false \
                    always does; list your provider's server in its trusted_authserv_ids";
        assert_eq!(
            warnings(
                &dir.path().join("rules.toml"),
                &rules,
                &["work", "home", "gmail"],
                &["work", "gmail"]
            ),
            [
                format!("rule 'junk' uses authenticated, but account 'work' {tail}"),
                format!("rule 'junk' uses authenticated, but account 'gmail' {tail}"),
                format!("rule 'deep' uses authenticated, but account 'work' {tail}"),
            ]
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
            warnings(
                &dir.path().join("rules.toml"),
                &rules,
                &["work", "werk"],
                &[]
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
        assert!(warnings(&path, &[], &[], &[]).is_empty());
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
                format!(
                    "[[rules]]\nname = \"r{i}\"\nmatch.read = true\nactions.move = \"{target}\"\n\n"
                )
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
            "[[rules]]\nname = \"home\"\naccount = \"home\"\nmatch.read = true\nactions.move = \"A/B\"\n",
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
    fn a_move_to_a_special_folders_name_names_the_folder_with_that_role() {
        let rules = parse(
            "[[rules]]\nname = \"bin\"\nmatch.read = true\nactions.move = \"Trash\"\n\n\
             [[rules]]\nname = \"spam\"\nmatch.read = true\nactions.move = \"Spam\"\n\n\
             [[rules]]\nname = \"keep\"\nmatch.read = true\nactions.move = \"Old/archive\"\n\n\
             [[rules]]\nname = \"sent\"\nmatch.read = true\nactions.move = \"Sent\"\n\n\
             [[rules]]\nname = \"lists\"\nmatch.read = true\nactions.move = \"Lists\"\n",
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
            watched_folder_warnings(&rules, &[gmail, own]),
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
            watched_folder_warnings(&rules, &[first_sync]).is_empty(),
            "before a complete pass, the target may exist"
        );
    }

    #[test]
    fn a_rule_watching_a_folder_no_account_syncs_is_named_once_every_account_synced() {
        let rules = parse(
            "[[rules]]\nname = \"typo\"\nfolder = \"Newsleters\"\nmatch.read = true\nactions.flagged = true\n\n\
             [[rules]]\nname = \"work lists\"\naccount = \"work\"\nfolder = \"Lists\"\nmatch.read = true\n\
             actions.flagged = true\n\n\
             [[rules]]\nname = \"shared\"\nfolder = \"Lists\"\nmatch.read = true\nactions.flagged = true\n\n\
             [[rules]]\nname = \"inbox\"\nfolder = \"Inbox\"\nmatch.read = true\nactions.flagged = true\n\n\
             [[rules]]\nname = \"nfd\"\nfolder = \"Re\u{301}sume\u{301}s\"\nmatch.read = true\n\
             actions.flagged = true\n\n\
             [[rules]]\nname = \"elsewhere\"\naccount = \"werk\"\nfolder = \"Nope\"\nmatch.read = true\n\
             actions.flagged = true\n",
        )
        .unwrap()
        .rules;
        let home = synced("home", &["INBOX", "Lists", "Résumés"]);
        let work = synced("work", &["INBOX"]);
        assert_eq!(
            watched_folder_warnings(&rules, &[work.clone(), home.clone()]),
            [
                "rule 'typo' watches 'Newsleters', which no account syncs, so it never runs",
                "rule 'work lists' watches 'Lists', which account 'work' does not sync, so it never runs",
            ],
            "'shared' runs on home; INBOX matches in any case, a name in any normalization form; an account not set \
             up here has its own warning"
        );
        let first_sync = AccountFolders {
            synced: None,
            ..home
        };
        assert_eq!(
            watched_folder_warnings(&rules, &[work, first_sync]),
            [
                "rule 'work lists' watches 'Lists', which account 'work' does not sync, so it never runs"
            ],
            "an account still in its first sync may yet have the folder"
        );
    }

    #[test]
    fn synced_folders_are_read_from_existing_stores_only() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let store = Store::open_account(&paths, "work").unwrap();
        store
            .upsert_folder(&layout("work", "/", &["INBOX"]).folders[0])
            .unwrap();
        assert_eq!(
            account_folders(&paths, &["work"])[0].synced,
            None,
            "no pass went through every folder yet"
        );
        store.set_folders_listed(1).unwrap();
        drop(store);
        assert_eq!(
            account_folders(&paths, &["work", "new"]),
            [
                synced("work", &["INBOX"]),
                AccountFolders {
                    name: "new".into(),
                    synced: None
                }
            ]
        );
        assert!(!paths.mail_db("new").exists(), "a check creates no store");
    }
}
