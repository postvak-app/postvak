//! What leaves rules.toml valid but not doing what its author meant: a conflicting copy a file sync left beside it,
//! a rule for an account this device does not have, a rule moving mail to the folder it watches, and a rule using
//! `authenticated` for an account that trusts no server's `Authentication-Results`.
use std::fs;
use std::path::Path;

use super::{Action, Match, Rule};

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
            "[[rules]]\nname = \"lists\"\nfolder = \"Lists\"\nmatch.seen = true\nactions = [{ move = \"Lists\" }]\n\n\
             [[rules]]\nname = \"inbox\"\nmatch.seen = true\nactions = [\"mark_read\", { move = \"Inbox\" }]\n\n\
             [[rules]]\nname = \"case\"\nfolder = \"Lists\"\nmatch.seen = true\nactions = [{ move = \"lists\" }]\n\n\
             [[rules]]\nname = \"away\"\nmatch.seen = true\nactions = [{ move = \"Lists\" }]\n",
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

    #[test]
    fn a_rule_using_authenticated_for_an_account_that_trusts_no_server_is_named() {
        let dir = tempfile::tempdir().unwrap();
        let rules = parse(
            "[[rules]]\nname = \"junk\"\nmatch.from_domain = \"paypal.com\"\nmatch.authenticated = false\nactions = [\"flag\"]\n\n\
             [[rules]]\nname = \"deep\"\naccount = \"work\"\nmatch.seen = true\nmatch.none = [{ any = [{ authenticated = true }] }]\nactions = [\"flag\"]\n\n\
             [[rules]]\nname = \"home only\"\naccount = \"home\"\nmatch.authenticated = true\nactions = [\"flag\"]\n\n\
             [[rules]]\nname = \"plain\"\nmatch.seen = true\nactions = [\"flag\"]\n",
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
}
