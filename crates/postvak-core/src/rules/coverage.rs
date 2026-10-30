//! Rules coverage: which of a folder's stored messages the rules match, which no rule matches, and which several
//! rules contest, with what the real precedence (a delete wins, then the first move; `stop`) would do. Read-only:
//! nothing is fetched, logged or changed.
use std::collections::{HashMap, HashSet};

use serde::Serialize;

use crate::config::Identity;
use crate::rules::apply::{ApplyError, plan_for, would_run};
use crate::rules::engine::{Context, Mode, Plan, folder_needs_body, matching};
use crate::rules::{CompiledRule, Rule, RuleFile, RulesError, compile};
use crate::senders::{GroupBy, Sender, senders};
use crate::store::{Message, Store, StoreError};

/// One message several rules match.
#[derive(Debug, Clone, PartialEq)]
pub struct Contest {
    pub message: Message,
    /// Every rule that matches it, in rules.toml order, whether or not it gets to act.
    pub rules: Vec<String>,
    /// What would run, rule by rule, after a delete, `stop` and the first move have had their say; a flag change already
    /// in effect does not run.
    pub acts: Vec<Act>,
    /// Why nothing would run at all, such as an archive on an account with no Archive folder; `acts` is then empty.
    pub fails: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Act {
    pub rule: String,
    pub action: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Coverage {
    pub messages: u32,
    /// Messages at least one rule matches; contested ones included.
    pub matched: u32,
    pub unmatched: u32,
    /// Messages two or more rules match.
    pub contested: u32,
    /// The senders of the unmatched mail, the largest group first.
    pub unmatched_senders: Vec<Sender>,
    /// Every contested message, newest first.
    pub contests: Vec<Contest>,
}

/// How coverage and sender statistics evaluate rules: over mail that predates them, as `rules test` does, without
/// notifying.
pub fn context<'a>(account: &'a str, identity: &'a Identity, now: i64) -> Context<'a> {
    Context {
        account,
        identity,
        now,
        mode: Mode::ApplyExisting,
        notify_default: false,
        keep_in_folder: false,
    }
}

/// The rules of `file` that coverage runs: the enabled ones, and with `include_pending` the proposals still waiting
/// for a human too, each where it stands in the file, and a pending edit in place of the rule it edits.
pub fn rules(file: &RuleFile, include_pending: bool) -> Result<Vec<CompiledRule>, RulesError> {
    let mut file = file.clone();
    if include_pending {
        for rule in &mut file.rules {
            rule.enabled |= rule.proposed_by.is_some();
        }
        // An edit whose rule is gone stays where it is, as a proposal of its own.
        let names: HashSet<String> = file.rules.iter().map(|r| r.name.clone()).collect();
        let is_edit = |r: &Rule| {
            r.proposed_by.is_some() && r.replaces.as_ref().is_some_and(|t| names.contains(t))
        };
        let (edits, kept): (Vec<Rule>, Vec<Rule>) = file.rules.into_iter().partition(is_edit);
        let mut edits: HashMap<String, Rule> = edits
            .into_iter()
            .filter_map(|r| Some((r.replaces.clone()?, r)))
            .collect();
        file.rules = kept
            .into_iter()
            .map(|r| edits.remove(&r.name).unwrap_or(r))
            .collect();
    }
    let mut compiled = compile(&file)?;
    compiled.retain(|r| r.rule.enabled);
    Ok(compiled)
}

/// Coverage of `folder`'s stored mail. Body text is read only when a rule that could match the folder tests it.
pub fn in_folder(
    store: &Store,
    rules: &[CompiledRule],
    ctx: &Context,
    folder: &str,
    by: GroupBy,
) -> Result<Coverage, StoreError> {
    let with_body = folder_needs_body(rules, ctx.account, folder);
    let messages = store.messages_between(folder, None, None, with_body)?;
    let mut coverage = Coverage {
        messages: u32::try_from(messages.len()).unwrap_or(u32::MAX),
        ..Coverage::default()
    };
    let mut unmatched = Vec::new();
    for msg in messages {
        let names: Vec<String> = matching(rules, &msg, ctx)
            .map(|(_, rule)| rule.rule.name.clone())
            .collect();
        match names.len() {
            0 => {
                coverage.unmatched += 1;
                unmatched.push(msg);
            }
            1 => coverage.matched += 1,
            _ => {
                coverage.matched += 1;
                coverage.contested += 1;
                let (acts, fails) = acts(&plan_for(rules, &msg, ctx, store)?, &msg, store)?;
                coverage.contests.push(Contest {
                    message: msg,
                    rules: names,
                    acts,
                    fails,
                });
            }
        }
    }
    coverage.unmatched_senders = senders(&unmatched, &[], ctx, by);
    Ok(coverage)
}

/// What `apply` would run of `plan`, as `would_run` says, or why `apply` would fail before running anything.
fn acts(
    plan: &Plan,
    msg: &Message,
    store: &Store,
) -> Result<(Vec<Act>, Option<String>), StoreError> {
    match would_run(plan, msg, store) {
        Ok(running) => Ok((
            running
                .into_iter()
                .map(|planned| Act {
                    rule: planned.rule.clone(),
                    action: planned.action.label(),
                })
                .collect(),
            None,
        )),
        Err(ApplyError::Store(e)) => Err(e),
        Err(e) => Ok((Vec::new(), Some(e.to_string()))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AccountConfig, PasswordSource};
    use crate::rules::parse;

    fn identity() -> Identity {
        let password = PasswordSource::Keyring { keyring: true };
        AccountConfig::new("work", "h", "me@example.com", password)
            .identity()
            .unwrap()
    }

    fn store(messages: &[(u32, &str, &str)]) -> Store {
        let store = Store::open_in_memory().unwrap();
        store
            .upsert_folder(&crate::store::Folder {
                name: "INBOX".into(),
                uidvalidity: 1,
                last_uid: 0,
                special_use: None,
                delimiter: None,
            })
            .unwrap();
        for &(uid, from, subject) in messages {
            store
                .insert_message(&Message {
                    folder: "INBOX".into(),
                    uid,
                    message_id: Some(format!("{uid}@x")),
                    thread_id: format!("{uid}@x"),
                    from_addr: Some(from.into()),
                    subject: Some(subject.into()),
                    internaldate: i64::from(uid) * 100,
                    ..Default::default()
                })
                .unwrap();
        }
        store
    }

    const RULES: &str = r#"
[[rules]]
name = "delivery"
match.subject = { regex = "(?i)deliver" }
actions = { move = "Deliveries" }

[[rules]]
name = "shop"
match.from = { contains = "@shop.example" }
actions = { flagged = true, move = "Shop" }

[[rules]]
name = "noise"
match.from = { contains = "@noise.example" }
actions = { delete = true }

[[rules]]
name = "noise read"
match.from = { contains = "@noise.example" }
actions = { read = true }

[[rules]]
name = "proposal"
enabled = false
proposed_by = "cli:agent"
match.from = { contains = "friend@" }
actions = { archive = true }

[[rules]]
name = "off by hand"
enabled = false
match.from = { contains = "friend@" }
actions = { flagged = true }
"#;

    #[test]
    fn counts_matched_unmatched_and_contested_with_what_acts() {
        let store = store(&[
            (1, "orders@shop.example", "Your delivery is on its way"),
            (2, "orders@shop.example", "Receipt"),
            (3, "a@noise.example", "Buy now"),
            (4, "friend@home.example", "Dinner?"),
            (5, "friend@home.example", "Photos"),
            (6, "boss@work.example", "Plan"),
        ]);
        let id = identity();
        let ctx = context("work", &id, 10_000);
        let rules = rules(&parse(RULES).unwrap(), false).unwrap();
        assert_eq!(rules.len(), 4, "disabled rules and proposals left out");
        let coverage = in_folder(&store, &rules, &ctx, "INBOX", GroupBy::Address).unwrap();
        assert_eq!(
            (
                coverage.messages,
                coverage.matched,
                coverage.unmatched,
                coverage.contested
            ),
            (6, 3, 3, 2)
        );
        let senders: Vec<(&str, u32)> = coverage
            .unmatched_senders
            .iter()
            .map(|s| (s.sender.as_str(), s.count))
            .collect();
        assert_eq!(
            senders,
            [("friend@home.example", 2), ("boss@work.example", 1)]
        );
        // Newest first; the delete wins over the later rule's flag change, and stops it.
        let noise = &coverage.contests[0];
        assert_eq!(noise.message.uid, 3);
        assert_eq!(noise.rules, ["noise", "noise read"]);
        assert_eq!(
            noise.acts,
            [Act {
                rule: "noise".into(),
                action: "delete".into()
            }]
        );
        // The earlier delivery rule takes the move the shop rule was written for; the shop rule's flag still runs.
        let delivery = &coverage.contests[1];
        assert_eq!(delivery.message.uid, 1);
        assert_eq!(delivery.rules, ["delivery", "shop"]);
        let acts: Vec<(&str, &str)> = delivery
            .acts
            .iter()
            .map(|a| (a.rule.as_str(), a.action.as_str()))
            .collect();
        assert_eq!(acts, [("delivery", "move:Deliveries"), ("shop", "flagged")]);
    }

    /// #411: a rule that matches on a tag an earlier rule sets counts as a match, as it acts in the same pass.
    #[test]
    fn a_rule_matching_an_earlier_rules_tag_contests_the_message() {
        let store = store(&[(1, "news@list.example", "Weekly")]);
        let contest = contest(
            &store,
            "INBOX",
            r#"
[[rules]]
name = "tag"
match.from = { contains = "@list.example" }
actions.tag = "$list"

[[rules]]
name = "route"
match.tag = "$list"
actions.read = true
"#,
        );
        assert_eq!(contest.rules, ["tag", "route"]);
        assert_eq!(acted(&contest), [("tag", "tag:$list"), ("route", "read")]);
    }

    #[test]
    fn pending_proposals_count_when_asked_and_keep_their_place() {
        let store = store(&[(4, "friend@home.example", "Dinner?")]);
        let id = identity();
        let ctx = context("work", &id, 10_000);
        let rules = rules(&parse(RULES).unwrap(), true).unwrap();
        let names: Vec<&str> = rules.iter().map(|r| r.rule.name.as_str()).collect();
        assert_eq!(
            names,
            ["delivery", "shop", "noise", "noise read", "proposal"]
        );
        let coverage = in_folder(&store, &rules, &ctx, "INBOX", GroupBy::Domain).unwrap();
        assert_eq!((coverage.matched, coverage.unmatched), (1, 0));
    }

    #[test]
    fn a_pending_edit_runs_in_place_of_its_rule_when_asked() {
        let file = parse(&format!(
            "{RULES}\n[[rules]]\nname = \"delivery (edit)\"\nenabled = false\nproposed_by = \"cli:agent\"\nreplaces = \"delivery\"\nmatch.from = {{ contains = \"orders@\" }}\nactions = {{ read = true }}\n"
        ))
        .unwrap();
        let names = |pending| -> Vec<String> {
            rules(&file, pending)
                .unwrap()
                .into_iter()
                .map(|r| r.rule.name)
                .collect()
        };
        assert_eq!(names(false), ["delivery", "shop", "noise", "noise read"]);
        assert_eq!(
            names(true),
            ["delivery (edit)", "shop", "noise", "noise read", "proposal"]
        );
    }

    #[test]
    fn a_first_move_into_the_folder_itself_moves_nothing_and_no_later_move_takes_its_place() {
        let store = store(&[(1, "news@list.example", "Weekly")]);
        let file = parse(
            r#"
[[rules]]
name = "stay"
match.from = { contains = "@list.example" }
actions = { flagged = true, move = "INBOX" }

[[rules]]
name = "elsewhere"
match.subject = { regex = "." }
actions = { move = "Lists" }
"#,
        )
        .unwrap();
        let id = identity();
        let ctx = context("work", &id, 10_000);
        let rules = rules(&file, false).unwrap();
        let coverage = in_folder(&store, &rules, &ctx, "INBOX", GroupBy::Address).unwrap();
        assert_eq!(coverage.contests[0].rules, ["stay", "elsewhere"]);
        assert_eq!(
            coverage.contests[0].acts,
            [Act {
                rule: "stay".into(),
                action: "flagged".into()
            }]
        );
    }

    #[test]
    fn a_stop_leaves_later_rules_matching_but_not_acting() {
        let store = store(&[(1, "news@list.example", "Weekly")]);
        let file = parse(
            r#"
[[rules]]
name = "lists"
match.from = { contains = "@list.example" }
actions = { read = true, stop = true }

[[rules]]
name = "everything"
match.subject = { regex = "." }
actions = { archive = true }
"#,
        )
        .unwrap();
        let id = identity();
        let ctx = context("work", &id, 10_000);
        let rules = rules(&file, false).unwrap();
        let coverage = in_folder(&store, &rules, &ctx, "INBOX", GroupBy::Address).unwrap();
        assert_eq!(coverage.contests[0].rules, ["lists", "everything"]);
        assert_eq!(
            coverage.contests[0].acts,
            [Act {
                rule: "lists".into(),
                action: "read".into()
            }]
        );
    }

    fn contest(store: &Store, folder: &str, toml: &str) -> Contest {
        let id = identity();
        let ctx = context("work", &id, 10_000);
        let rules = rules(&parse(toml).unwrap(), false).unwrap();
        let coverage = in_folder(store, &rules, &ctx, folder, GroupBy::Address).unwrap();
        coverage.contests.into_iter().next().unwrap()
    }

    fn acted(contest: &Contest) -> Vec<(&str, &str)> {
        contest
            .acts
            .iter()
            .map(|a| (a.rule.as_str(), a.action.as_str()))
            .collect()
    }

    const ARCHIVE_AND_READ: &str = r#"
[[rules]]
name = "keep"
match.from = { contains = "@list.example" }
actions = { flagged = true, archive = true }

[[rules]]
name = "read"
match.subject = { regex = "." }
actions = { read = true }
"#;

    #[test]
    fn an_archive_without_an_archive_folder_runs_nothing_and_says_why() {
        let store = store(&[(1, "news@list.example", "Weekly")]);
        let contest = contest(&store, "INBOX", ARCHIVE_AND_READ);
        assert_eq!(contest.rules, ["keep", "read"]);
        assert!(contest.acts.is_empty(), "{:?}", contest.acts);
        assert_eq!(
            contest.fails.as_deref(),
            Some("the server has no Archive folder")
        );
    }

    #[test]
    fn an_archive_in_the_archive_folder_moves_nothing() {
        let store = store(&[]);
        store
            .upsert_folder(&crate::store::Folder {
                name: "Archive".into(),
                uidvalidity: 1,
                last_uid: 0,
                special_use: Some("Archive".into()),
                delimiter: None,
            })
            .unwrap();
        store
            .insert_message(&Message {
                folder: "Archive".into(),
                uid: 1,
                message_id: Some("1@x".into()),
                thread_id: "1@x".into(),
                from_addr: Some("news@list.example".into()),
                subject: Some("Weekly".into()),
                internaldate: 100,
                ..Default::default()
            })
            .unwrap();
        let in_archive =
            ARCHIVE_AND_READ.replace("[[rules]]\n", "[[rules]]\nfolder = \"Archive\"\n");
        let contest = contest(&store, "Archive", &in_archive);
        assert_eq!(contest.fails, None);
        assert_eq!(acted(&contest), [("keep", "flagged"), ("read", "read")]);
    }

    #[test]
    fn a_flag_change_already_in_effect_does_not_act() {
        let store = store(&[]);
        store
            .insert_message(&Message {
                folder: "INBOX".into(),
                uid: 1,
                message_id: Some("1@x".into()),
                thread_id: "1@x".into(),
                from_addr: Some("news@list.example".into()),
                subject: Some("Weekly".into()),
                flags: "\\Seen".into(),
                internaldate: 100,
                ..Default::default()
            })
            .unwrap();
        let contest = contest(
            &store,
            "INBOX",
            r#"
[[rules]]
name = "read"
match.from = { contains = "@list.example" }
actions = { read = true, flagged = true }

[[rules]]
name = "unread"
match.subject = { regex = "." }
actions = { flagged = true, tag = "$later" }
"#,
        );
        // Already read, so mark_read is skipped; the first rule's flag leaves nothing for the second rule's flag to do.
        assert_eq!(
            acted(&contest),
            [("read", "flagged"), ("unread", "tag:$later")]
        );
    }
    #[test]
    fn a_message_restored_from_trash_matches_no_rule() {
        let store = store(&[]);
        store
            .insert_message(&Message {
                folder: "INBOX".into(),
                uid: 1,
                message_id: Some("1@x".into()),
                thread_id: "1@x".into(),
                from_addr: Some("a@noise.example".into()),
                subject: Some("Buy now".into()),
                flags: crate::rules::engine::RESTORED_KEYWORD.into(),
                internaldate: 100,
                ..Default::default()
            })
            .unwrap();
        let id = identity();
        let ctx = context("work", &id, 10_000);
        let rules = rules(&parse(RULES).unwrap(), false).unwrap();
        let coverage = in_folder(&store, &rules, &ctx, "INBOX", GroupBy::Address).unwrap();
        // A sync never acts on it, so the two noise rules neither match nor contest it.
        assert_eq!((coverage.matched, coverage.unmatched), (0, 1));
        assert!(coverage.contests.is_empty(), "{:?}", coverage.contests);
        let restored = store.messages_between("INBOX", None, None, false).unwrap();
        let groups = senders(&restored, &rules, &ctx, GroupBy::Address);
        assert_eq!(groups[0].sender, "a@noise.example");
        assert!(groups[0].rules.is_empty(), "{:?}", groups[0].rules);
    }
}
