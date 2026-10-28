//! Rules coverage: which of a folder's stored messages the rules match, which no rule matches, and which several
//! rules contest, with what the real precedence (a delete wins, then the first move; `stop`) would do. Read-only:
//! nothing is fetched, logged or changed.
use serde::Serialize;

use crate::config::Identity;
use crate::rules::apply::plan_for;
use crate::rules::engine::{Context, Mode, Plan, folder_needs_body, rule_matches};
use crate::rules::{Action, CompiledRule, RuleFile, RulesError, compile};
use crate::senders::{GroupBy, Sender, senders};
use crate::store::{Message, Store, StoreError};

/// One message several rules match.
#[derive(Debug, Clone, PartialEq)]
pub struct Contest {
    pub message: Message,
    /// Every rule that matches it, in rules.toml order, whether or not it gets to act.
    pub rules: Vec<String>,
    /// What would run, rule by rule, after a delete, `stop` and the first move have had their say.
    pub acts: Vec<Act>,
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
/// for a human too, each where it stands in the file.
pub fn rules(file: &RuleFile, include_pending: bool) -> Result<Vec<CompiledRule>, RulesError> {
    let mut file = file.clone();
    if include_pending {
        for rule in &mut file.rules {
            rule.enabled |= rule.proposed_by.is_some();
        }
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
        let matching: Vec<String> = rules
            .iter()
            .filter(|rule| rule_matches(rule, &msg, ctx))
            .map(|rule| rule.rule.name.clone())
            .collect();
        match matching.len() {
            0 => {
                coverage.unmatched += 1;
                unmatched.push(msg);
            }
            1 => coverage.matched += 1,
            _ => {
                coverage.matched += 1;
                coverage.contested += 1;
                let acts = acts(&plan_for(rules, &msg, ctx, store)?, &msg);
                coverage.contests.push(Contest {
                    message: msg,
                    rules: matching,
                    acts,
                });
            }
        }
    }
    coverage.unmatched_senders = senders(&unmatched, &[], ctx, by);
    Ok(coverage)
}

/// What `apply` would run of `plan`: a delete alone, or else every flag change and the first move or archive. When that
/// first move is into the message's own folder, `apply` moves nothing, and no later move takes its place.
fn acts(plan: &Plan, msg: &Message) -> Vec<Act> {
    let act = |planned: &crate::rules::engine::PlannedAction| Act {
        rule: planned.rule.clone(),
        action: planned.action.label(),
    };
    if let Some(delete) = plan
        .actions
        .iter()
        .find(|p| matches!(p.action, Action::Delete | Action::Trash))
    {
        return vec![act(delete)];
    }
    let mut moved = false;
    plan.actions
        .iter()
        .filter(|p| match &p.action {
            Action::Move(target) => !std::mem::replace(&mut moved, true) && *target != msg.folder,
            Action::Archive => !std::mem::replace(&mut moved, true),
            _ => true,
        })
        .map(act)
        .collect()
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
actions = [{ move = "Deliveries" }]

[[rules]]
name = "shop"
match.from = { contains = "@shop.example" }
actions = ["flag", { move = "Shop" }]

[[rules]]
name = "noise"
match.from = { contains = "@noise.example" }
actions = ["delete"]

[[rules]]
name = "noise read"
match.from = { contains = "@noise.example" }
actions = ["mark_read"]

[[rules]]
name = "proposal"
enabled = false
proposed_by = "cli:agent"
match.from = { contains = "friend@" }
actions = ["archive"]

[[rules]]
name = "off by hand"
enabled = false
match.from = { contains = "friend@" }
actions = ["flag"]
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
        assert_eq!(acts, [("delivery", "move:Deliveries"), ("shop", "flag")]);
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
    fn a_first_move_into_the_folder_itself_moves_nothing_and_no_later_move_takes_its_place() {
        let store = store(&[(1, "news@list.example", "Weekly")]);
        let file = parse(
            r#"
[[rules]]
name = "stay"
match.from = { contains = "@list.example" }
actions = ["flag", { move = "INBOX" }]

[[rules]]
name = "elsewhere"
match.subject = { regex = "." }
actions = [{ move = "Lists" }]
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
                action: "flag".into()
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
actions = ["mark_read", "stop"]

[[rules]]
name = "everything"
match.subject = { regex = "." }
actions = ["archive"]
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
                action: "mark_read".into()
            }]
        );
    }
}
