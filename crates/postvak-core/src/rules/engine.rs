use crate::config::Identity;
use crate::message::{auth, bare_addresses, header_text};
use crate::rules::{Action, CompiledRule, Conditions};
use crate::store::{Message, has_flag};

/// Set by `trash restore`; rules never act on, or notify about, mail carrying it.
pub const RESTORED_KEYWORD: &str = "$PostvakRestored";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    ApplyExisting,
}

#[derive(Clone, Copy)]
pub struct Context<'a> {
    pub account: &'a str,
    pub identity: &'a Identity,
    pub now: i64,
    pub mode: Mode,
    pub notify_default: bool,
    /// The message came back to a folder a rule moved it out of: no rule moves or archives it again.
    pub keep_in_folder: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlannedAction {
    pub rule: String,
    pub action: Action,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Plan {
    pub actions: Vec<PlannedAction>,
    pub notify: bool,
}

pub fn folder_needs_body(rules: &[CompiledRule], account: &str, folder: &str) -> bool {
    rules.iter().any(|r| {
        r.rule.enabled
            && r.applies_to_account(account)
            && crate::folder_name::rule_names(r.folder(), folder)
            && r.needs_body()
    })
}

pub fn evaluate(rules: &[CompiledRule], msg: &Message, ctx: &Context) -> Plan {
    if has_flag(&msg.flags, RESTORED_KEYWORD) {
        return Plan::default();
    }
    let mut plan = Plan::default();
    let mut explicit_notify: Option<bool> = None;
    for rule in rules {
        if !rule_matches(rule, msg, ctx) {
            continue;
        }
        let mut stop = false;
        for action in &rule.rule.actions {
            match action {
                Action::Notify => _ = explicit_notify.get_or_insert(true),
                Action::Silent => _ = explicit_notify.get_or_insert(false),
                Action::Stop => stop = true,
                Action::Move(_) | Action::Archive if ctx.keep_in_folder => {}
                _ => plan.actions.push(PlannedAction {
                    rule: rule.rule.name.clone(),
                    action: action.clone(),
                }),
            }
            if *action == Action::Delete {
                stop = true;
            }
        }
        if stop {
            break;
        }
    }
    let deleted = plan.actions.iter().any(|a| a.action == Action::Delete);
    plan.notify = !deleted
        && match explicit_notify {
            Some(explicit) => explicit,
            None => {
                ctx.notify_default
                    && !plan.actions.iter().any(|a| {
                        matches!(a.action, Action::Delete | Action::Move(_) | Action::Archive)
                    })
            }
        };
    plan
}

/// Whether `rule` alone would match `msg`: the checks `evaluate` makes before it plans a rule's actions. Whether an
/// earlier rule's `stop` or delete keeps it from acting is left to `evaluate`. What coverage and sender statistics
/// report as a rule's match.
pub fn rule_matches(rule: &CompiledRule, msg: &Message, ctx: &Context) -> bool {
    !has_flag(&msg.flags, RESTORED_KEYWORD)
        && rule.rule.enabled
        && rule.applies_to_account(ctx.account)
        && crate::folder_name::rule_names(rule.folder(), &msg.folder)
        && (ctx.mode != Mode::Normal || msg.internaldate >= rule.first_seen_at)
        // An unknown body is neither a match nor a mismatch, so `none` cannot turn a failed fetch into a hit.
        && (!rule.needs_body() || msg.body_text.is_some())
        && matches(&rule.conditions, msg, ctx)
}

fn field(value: &Option<String>) -> &str {
    value.as_deref().unwrap_or("")
}

/// Whether an address of the field, not its display name, is in one of `domains` or a subdomain of one.
fn in_any_domain(addresses: &str, domains: &[String]) -> bool {
    bare_addresses(addresses)
        .iter()
        .filter_map(|address| auth::address_domain(address))
        .any(|domain| {
            domains
                .iter()
                .any(|parent| auth::in_domain(&domain, parent))
        })
}

fn matches(c: &Conditions, msg: &Message, ctx: &Context) -> bool {
    if let Some(m) = &c.from
        && !m.is_address_match(field(&msg.from_addr))
    {
        return false;
    }
    if let Some(m) = &c.to
        && !m.is_address_match(field(&msg.to_addr))
    {
        return false;
    }
    if let Some(m) = &c.cc
        && !m.is_address_match(field(&msg.cc_addr))
    {
        return false;
    }
    if let Some(m) = &c.subject
        && !m.is_match(field(&msg.subject))
    {
        return false;
    }
    if let Some(m) = &c.body
        && !m.is_match(field(&msg.body_text))
    {
        return false;
    }
    for (name, m) in &c.headers {
        match header_text(&msg.headers, name) {
            Some(value) if m.is_match(&value) => {}
            _ => return false,
        }
    }
    if let Some(domains) = &c.from_domain
        && !in_any_domain(field(&msg.from_addr), domains)
    {
        return false;
    }
    if let Some(domains) = &c.to_domain
        && !in_any_domain(field(&msg.to_addr), domains)
    {
        return false;
    }
    if let Some(authenticated) = c.authenticated
        && ctx
            .identity
            .authenticated(&msg.headers, field(&msg.from_addr))
            != authenticated
    {
        return false;
    }
    if let Some(span) = c.older_than
        && span
            .before(ctx.now)
            .is_none_or(|cutoff| msg.internaldate > cutoff)
    {
        return false;
    }
    if let Some(span) = c.younger_than
        && span
            .before(ctx.now)
            .is_some_and(|cutoff| msg.internaldate <= cutoff)
    {
        return false;
    }
    if let Some(min_size) = c.larger_than
        && msg.size.is_none_or(|size| u64::from(size) <= min_size)
    {
        return false;
    }
    if let Some(read) = c.read
        && msg.is_seen() != read
    {
        return false;
    }
    if let Some(flagged) = c.flagged
        && msg.is_flagged() != flagged
    {
        return false;
    }
    if let Some(tag) = &c.tag
        && !has_flag(&msg.flags, tag)
    {
        return false;
    }
    if c.to_me.is_some() || c.alias.is_some() {
        let recipients: Vec<String> = [&msg.to_addr, &msg.cc_addr, &msg.delivered_to]
            .into_iter()
            .flat_map(|f| bare_addresses(field(f)))
            .collect();
        if let Some(to_me) = c.to_me
            && recipients.iter().any(|r| ctx.identity.is_me(r)) != to_me
        {
            return false;
        }
        if let Some(alias) = &c.alias
            && !recipients.iter().any(|r| alias.is_match(r))
        {
            return false;
        }
    }
    if c.none.iter().any(|entry| matches(entry, msg, ctx)) {
        return false;
    }
    c.any.is_empty() || c.any.iter().any(|branch| matches(branch, msg, ctx))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AccountConfig, PasswordSource};
    use crate::rules::{compile, parse};

    const HOUR: i64 = 3600;

    fn identity() -> Identity {
        let password = PasswordSource::Keyring { keyring: true };
        AccountConfig {
            aliases: vec!["*@shop.example.com".into()],
            ..AccountConfig::new("work", "h", "pieter@example.com", password)
        }
        .identity()
        .unwrap()
    }

    fn rules(toml: &str, first_seen_at: i64) -> Vec<CompiledRule> {
        let mut compiled = compile(&parse(toml).unwrap()).unwrap();
        for r in &mut compiled {
            r.first_seen_at = first_seen_at;
        }
        compiled
    }

    fn msg(from: &str, to: &str, subject: &str, internaldate: i64, seen: bool) -> Message {
        let list_id = if from.contains("github") {
            "List-Id: dev <dev.github.com>\r\n"
        } else {
            ""
        };
        let headers = format!("From: {from}\r\nTo: {to}\r\nSubject: {subject}\r\n{list_id}\r\n");
        Message {
            folder: "INBOX".into(),
            uid: 1,
            message_id: Some("m1@x".into()),
            from_addr: Some(from.into()),
            to_addr: Some(to.into()),
            thread_id: "m1@x".into(),
            subject: Some(subject.into()),
            date: Some(internaldate),
            internaldate,
            flags: if seen { "\\Seen".into() } else { String::new() },
            headers: headers.into_bytes(),
            ..Default::default()
        }
    }

    fn ctx<'a>(id: &'a Identity, now: i64) -> Context<'a> {
        Context {
            account: "work",
            identity: id,
            now,
            mode: Mode::Normal,
            notify_default: true,
            keep_in_folder: false,
        }
    }

    const PURGE: &str = r#"
[[rules]]
name = "purge"
match.from = { regex = "no-?reply@" }
match.subject = { regex = "sign.?in|verification code" }
match.older_than = "1h"
match.read = true
actions = { delete = true }
"#;

    #[test]
    fn restored_mail_is_left_alone() {
        let id = identity();
        let rules = rules(
            "[[rules]]\nname = \"codes\"\nmatch.subject = { contains = \"code\" }\nactions = { delete = true }\n",
            0,
        );
        let mut m = msg("noreply@x.com", "me@example.com", "your code", 100, true);
        assert!(!evaluate(&rules, &m, &ctx(&id, 200)).actions.is_empty());
        m.flags = format!("\\Seen {RESTORED_KEYWORD}");
        assert_eq!(evaluate(&rules, &m, &ctx(&id, 200)), Plan::default());
    }

    #[test]
    fn sign_in_code_deleted_only_when_old_and_seen() {
        let id = identity();
        let rules = rules(PURGE, 0);
        let now = 10 * HOUR;
        let fresh = msg(
            "noreply@login.example",
            "pieter@example.com",
            "Your sign-in code",
            now - 600,
            true,
        );
        assert!(evaluate(&rules, &fresh, &ctx(&id, now)).actions.is_empty());
        let old_unseen = msg(
            "noreply@login.example",
            "pieter@example.com",
            "Your sign-in code",
            now - 2 * HOUR,
            false,
        );
        assert!(
            evaluate(&rules, &old_unseen, &ctx(&id, now))
                .actions
                .is_empty()
        );
        let old_seen = msg(
            "noreply@login.example",
            "pieter@example.com",
            "Your sign-in code",
            now - 2 * HOUR,
            true,
        );
        let plan = evaluate(&rules, &old_seen, &ctx(&id, now));
        assert_eq!(
            plan.actions,
            vec![PlannedAction {
                rule: "purge".into(),
                action: Action::Delete
            }]
        );
        assert!(!plan.notify, "deleted mail is silent");
    }

    #[test]
    fn rule_ignores_mail_older_than_its_first_seen_unless_apply_existing() {
        let id = identity();
        let rules = rules(PURGE, 5 * HOUR);
        let now = 10 * HOUR;
        let before_rule = msg("noreply@x", "pieter@example.com", "sign in", 4 * HOUR, true);
        assert!(
            evaluate(&rules, &before_rule, &ctx(&id, now))
                .actions
                .is_empty()
        );
        let mut c = ctx(&id, now);
        c.mode = Mode::ApplyExisting;
        assert_eq!(evaluate(&rules, &before_rule, &c).actions.len(), 1);
    }

    #[test]
    fn mail_kept_in_its_folder_gets_every_action_but_a_move_and_notifies() {
        let id = identity();
        let toml = "[[rules]]\nname = \"lists\"\nmatch.from = { contains = \"alice\" }\nactions = { move = \"Lists\", read = true }\n\n[[rules]]\nname = \"old\"\nmatch.from = { contains = \"alice\" }\nactions = { archive = true }\n";
        let rules = rules(toml, 0);
        let m = msg("alice@x", "pieter@example.com", "hi", HOUR, false);
        let moved = evaluate(&rules, &m, &ctx(&id, 2 * HOUR));
        assert_eq!(moved.actions.len(), 3);
        assert!(!moved.notify);
        let kept = evaluate(
            &rules,
            &m,
            &Context {
                keep_in_folder: true,
                ..ctx(&id, 2 * HOUR)
            },
        );
        let actions: Vec<_> = kept.actions.iter().map(|a| &a.action).collect();
        assert_eq!(actions, [&Action::MarkRead]);
        assert!(kept.notify);
    }

    #[test]
    fn equals_on_address_fields_compares_bare_addresses() {
        let id = identity();
        let from_rule = rules(
            "[[rules]]\nname = \"gh\"\nmatch.from = { equals = \" NoReply@GitHub.com \" }\nactions = { flagged = true }\n",
            0,
        );
        let github = msg(
            "GitHub <noreply@github.com>",
            "pieter@example.com",
            "PR",
            100,
            false,
        );
        assert_eq!(
            evaluate(&from_rule, &github, &ctx(&id, 200)).actions.len(),
            1
        );
        let to_rule = rules(
            "[[rules]]\nname = \"second\"\nmatch.to = { equals = \"b@x\" }\nactions = { flagged = true }\n",
            0,
        );
        let two = msg("a@y", "A <a@x>, B <b@x>", "hi", 100, false);
        assert_eq!(evaluate(&to_rule, &two, &ctx(&id, 200)).actions.len(), 1);
        let subject_rule = rules(
            "[[rules]]\nname = \"s\"\nmatch.subject = { equals = \"pr\" }\nactions = { flagged = true }\n",
            0,
        );
        assert_eq!(
            evaluate(&subject_rule, &github, &ctx(&id, 200))
                .actions
                .len(),
            1
        );
        let other = msg(
            "noreply@github.com.evil",
            "pieter@example.com",
            "PR",
            100,
            false,
        );
        assert!(
            evaluate(&from_rule, &other, &ctx(&id, 200))
                .actions
                .is_empty()
        );
    }

    #[test]
    fn move_and_mark_read_chain_and_header_match() {
        let id = identity();
        let toml = r#"
[[rules]]
name = "github"
match.header.List-Id = { contains = "github.com" }
actions = { move = "Lists/GitHub", read = true }
"#;
        let plan = evaluate(
            &rules(toml, 0),
            &msg("bot@github.com", "pieter@example.com", "PR", 100, false),
            &ctx(&id, 200),
        );
        assert_eq!(
            plan.actions
                .iter()
                .map(|a| a.action.clone())
                .collect::<Vec<_>>(),
            vec![Action::Move("Lists/GitHub".into()), Action::MarkRead]
        );
        assert!(!plan.notify, "moved mail is silent by default");
    }

    #[test]
    fn delete_stops_the_chain() {
        let id = identity();
        let toml = r#"
[[rules]]
name = "first"
match.subject = { contains = "spam" }
actions = { delete = true }

[[rules]]
name = "second"
match.subject = { contains = "spam" }
actions = { flagged = true }
"#;
        let plan = evaluate(
            &rules(toml, 0),
            &msg("a@x", "pieter@example.com", "SPAM offer", 100, false),
            &ctx(&id, 200),
        );
        assert_eq!(plan.actions.len(), 1);
        assert_eq!(plan.actions[0].rule, "first");
    }

    #[test]
    fn account_and_folder_scoping() {
        let id = identity();
        let toml = r#"
[[rules]]
name = "other account"
account = "home"
match.read = false
actions = { flagged = true }

[[rules]]
name = "other folder"
folder = "Archive"
match.read = false
actions = { flagged = true }
"#;
        let plan = evaluate(
            &rules(toml, 0),
            &msg("a@x", "pieter@example.com", "s", 100, false),
            &ctx(&id, 200),
        );
        assert!(plan.actions.is_empty());
    }

    #[test]
    fn a_rule_watching_a_folder_written_in_nfd_runs_in_the_folder_listed_in_nfc() {
        let id = identity();
        let toml = "[[rules]]\nname = \"n\"\nfolder = \"E\u{301}e\u{301}n\"\nmatch.body = { contains = \"x\" }\nactions.flagged = true\n";
        let mut m = msg("a@x", "pieter@example.com", "s", 100, false);
        m.folder = "\u{c9}\u{e9}n".into();
        m.body_text = Some("x".into());
        assert_eq!(
            evaluate(&rules(toml, 0), &m, &ctx(&id, 200)).actions.len(),
            1
        );
        assert!(folder_needs_body(&rules(toml, 0), "work", "\u{c9}\u{e9}n"));
    }

    #[test]
    fn a_rule_for_inbox_in_any_case_watches_inbox() {
        let id = identity();
        let toml = "[[rules]]\nname = \"inbox\"\nfolder = \"Inbox\"\nmatch.read = false\nactions.flagged = true\n";
        let plan = evaluate(
            &rules(toml, 0),
            &msg("a@x", "pieter@example.com", "s", 100, false),
            &ctx(&id, 200),
        );
        assert_eq!(
            plan.actions.len(),
            1,
            "INBOX is case-insensitive (RFC 3501 5.1)"
        );
        assert!(folder_needs_body(
            &rules(
                "[[rules]]\nname = \"b\"\nfolder = \"inbox\"\nmatch.body = { contains = \"x\" }\nactions.flagged = true\n",
                0
            ),
            "work",
            "INBOX"
        ));
    }

    #[test]
    fn to_me_and_alias() {
        let id = identity();
        let toml = r#"
[[rules]]
name = "lists"
match.to_me = false
actions = { move = "Lists" }

[[rules]]
name = "shop"
match.alias = "*@shop.example.com"
actions = { move = "Shopping" }
"#;
        let r = rules(toml, 0);
        let direct = msg("a@x", "Pieter <pieter@example.com>", "s", 100, false);
        assert!(evaluate(&r, &direct, &ctx(&id, 200)).actions.is_empty());
        let list = msg("a@x", "dev@lists.example", "s", 100, false);
        assert_eq!(evaluate(&r, &list, &ctx(&id, 200)).actions[0].rule, "lists");
        let shop = msg("a@x", "orders@shop.example.com", "s", 100, false);
        let plan = evaluate(&r, &shop, &ctx(&id, 200));
        assert_eq!(
            plan.actions
                .iter()
                .map(|a| a.rule.as_str())
                .collect::<Vec<_>>(),
            vec!["shop"]
        );
    }

    #[test]
    fn matches_header_counts_header_conditions_at_any_depth_only() {
        let r = rules(
            r#"
[[rules]]
name = "plain"
match.from = { contains = "a" }
match.subject = { contains = "b" }
actions = { flagged = true }

[[rules]]
name = "top"
match.header.List-Id = { contains = "x" }
actions = { flagged = true }

[[rules]]
name = "nested"
match.any = [{ read = true }, { none = [{ header = { X-Spam = { regex = "." } } }] }]
actions = { flagged = true }
"#,
            0,
        );
        let found: Vec<bool> = r.iter().map(CompiledRule::matches_header).collect();
        assert_eq!(found, [false, true, true]);
    }

    #[test]
    fn body_rule_needs_body_and_matches_when_present() {
        let id = identity();
        let toml = r#"
[[rules]]
name = "unsubscribe"
match.body = { contains = "unsubscribe" }
actions = { flagged = true }
"#;
        let r = rules(toml, 0);
        assert!(folder_needs_body(&r, "work", "INBOX"));
        assert!(!folder_needs_body(&r, "work", "Sent"));
        let mut m = msg("a@x", "pieter@example.com", "s", 100, false);
        assert!(evaluate(&r, &m, &ctx(&id, 200)).actions.is_empty());
        m.body_text = Some("Click here to UNSUBSCRIBE".into());
        assert_eq!(evaluate(&r, &m, &ctx(&id, 200)).actions.len(), 1);
    }

    #[test]
    fn notification_policy() {
        let id = identity();
        let toml = r#"
[[rules]]
name = "github"
match.header.List-Id = { contains = "github.com" }
actions = { move = "Lists/GitHub", notify = true }

[[rules]]
name = "quiet"
match.subject = { contains = "newsletter" }
actions = { notify = false }
"#;
        let r = rules(toml, 0);
        let untouched = msg("a@x", "pieter@example.com", "hello", 100, false);
        assert!(evaluate(&r, &untouched, &ctx(&id, 200)).notify);
        let mut off = ctx(&id, 200);
        off.notify_default = false;
        assert!(!evaluate(&r, &untouched, &off).notify);
        let moved_but_notify = msg("bot@github.com", "pieter@example.com", "PR", 100, false);
        assert!(evaluate(&r, &moved_but_notify, &ctx(&id, 200)).notify);
        let quiet = msg("a@x", "pieter@example.com", "Weekly newsletter", 100, false);
        assert!(!evaluate(&r, &quiet, &ctx(&id, 200)).notify);
    }

    #[test]
    fn the_first_matching_rule_that_says_notify_wins() {
        let id = identity();
        let toml = r#"
[[rules]]
name = "loud"
match.subject = { contains = "deploy" }
actions = { move = "Deploys", notify = true }

[[rules]]
name = "quiet"
match.subject = { contains = "deploy" }
actions = { move = "Elsewhere", notify = false }
"#;
        let plan = evaluate(
            &rules(toml, 0),
            &msg("a@x", "pieter@example.com", "deploy done", 100, false),
            &ctx(&id, 200),
        );
        assert!(plan.notify);
    }

    #[test]
    fn regex_ignores_case_unless_told_otherwise() {
        let m = msg(
            "No-Reply@Shop.example",
            "pieter@example.com",
            "Sign-In code",
            100,
            false,
        );
        assert!(fires(
            "[[rules]]\nname = \"r\"\nmatch.from = { regex = \"no-?reply@\" }\nactions = { flagged = true }\n",
            &m
        ));
        assert!(!fires(
            "[[rules]]\nname = \"r\"\nmatch.subject = { regex = \"(?-i)sign-in\" }\nactions = { flagged = true }\n",
            &m
        ));
    }

    #[test]
    fn deleted_mail_never_notifies() {
        let id = identity();
        let toml = r#"
[[rules]]
name = "loud"
match.subject = { contains = "x" }
actions = { notify = true }

[[rules]]
name = "purge"
match.subject = { contains = "x" }
actions = { delete = true }
"#;
        let plan = evaluate(
            &rules(toml, 0),
            &msg("a@x", "pieter@example.com", "x", 100, false),
            &ctx(&id, 200),
        );
        assert!(!plan.notify);
        assert_eq!(plan.actions.len(), 1);
        assert_eq!(plan.actions[0].action, Action::Delete);
    }

    #[test]
    fn huge_older_than_never_matches() {
        let id = identity();
        let toml = "[[rules]]\nname = \"never\"\nmatch.older_than = \"1000000y\"\nactions = { delete = true }\n";
        let m = msg("a@x", "pieter@example.com", "s", 0, true);
        assert!(
            evaluate(&rules(toml, 0), &m, &ctx(&id, 10 * HOUR))
                .actions
                .is_empty()
        );
    }

    #[test]
    fn to_me_via_delivered_to_and_cc() {
        let id = identity();
        let toml = r#"
[[rules]]
name = "lists"
match.to_me = false
actions = { move = "Lists" }

[[rules]]
name = "shop"
match.alias = "orders@shop.example.com"
actions = { move = "Shopping" }
"#;
        let r = rules(toml, 0);
        let mut delivered = msg("a@x", "dev@lists.example", "s", 100, false);
        delivered.delivered_to = Some("pieter@example.com".into());
        assert!(evaluate(&r, &delivered, &ctx(&id, 200)).actions.is_empty());
        let mut cc = msg("a@x", "pieter@example.com", "s", 100, false);
        cc.cc_addr = Some("Shop <orders@shop.example.com>".into());
        let plan = evaluate(&r, &cc, &ctx(&id, 200));
        assert_eq!(
            plan.actions
                .iter()
                .map(|a| a.rule.as_str())
                .collect::<Vec<_>>(),
            vec!["shop"]
        );
    }

    #[test]
    fn to_me_ignores_an_address_inside_a_quoted_name() {
        let id = identity();
        let r = rules(
            "[[rules]]\nname = \"lists\"\nmatch.to_me = false\nactions = { move = \"Lists\" }\n",
            0,
        );
        let mut m = msg("a@x", "pieter@example.com", "s", 100, false);
        m.to_addr = Some("\"Hi, pieter@example.com\" <list@lists.example>".into());
        assert_eq!(evaluate(&r, &m, &ctx(&id, 200)).actions.len(), 1);
    }

    #[test]
    fn older_than_boundary_is_inclusive() {
        let id = identity();
        let toml =
            "[[rules]]\nname = \"age\"\nmatch.older_than = \"1h\"\nactions = { flagged = true }\n";
        let r = rules(toml, 0);
        let exact = msg("a@x", "pieter@example.com", "s", 1000, false);
        assert_eq!(
            evaluate(&r, &exact, &ctx(&id, 1000 + HOUR)).actions.len(),
            1
        );
        assert!(
            evaluate(&r, &exact, &ctx(&id, 1000 + HOUR - 1))
                .actions
                .is_empty()
        );
    }

    #[test]
    fn younger_than_holds_until_older_than_would() {
        let id = identity();
        let toml = "[[rules]]\nname = \"hold\"\nmatch.younger_than = \"1h\"\nactions = { flagged = true }\n";
        let r = rules(toml, 0);
        let m = msg("a@x", "pieter@example.com", "s", 1000, false);
        assert_eq!(
            evaluate(&r, &m, &ctx(&id, 1000 + HOUR - 1)).actions.len(),
            1
        );
        assert!(evaluate(&r, &m, &ctx(&id, 1000 + HOUR)).actions.is_empty());
        let huge = "[[rules]]\nname = \"always\"\nmatch.younger_than = \"1000000y\"\nactions = { flagged = true }\n";
        assert_eq!(
            evaluate(&rules(huge, 0), &m, &ctx(&id, 10 * HOUR))
                .actions
                .len(),
            1
        );
    }

    fn fires(toml: &str, m: &Message) -> bool {
        let id = identity();
        !evaluate(&rules(toml, 0), m, &ctx(&id, 200))
            .actions
            .is_empty()
    }

    #[test]
    fn none_excludes_mail_any_entry_matches() {
        let toml = r#"
[[rules]]
name = "shop but no receipts"
match.from = { contains = "shop" }
match.none = [{ subject = { contains = "receipt" } }, { from = { contains = "billing" } }]
actions = { flagged = true }
"#;
        assert!(fires(toml, &msg("shop@x", "p@x", "Sale", 100, false)));
        assert!(!fires(
            toml,
            &msg("shop@x", "p@x", "Your receipt", 100, false)
        ));
        assert!(!fires(
            toml,
            &msg("billing@shop.x", "p@x", "Sale", 100, false)
        ));
        assert!(!fires(toml, &msg("bank@x", "p@x", "Sale", 100, false)));
    }

    #[test]
    fn a_none_entry_excludes_only_when_all_its_conditions_hold() {
        let toml = r#"
[[rules]]
name = "x"
match.read = false
match.none = [{ from = { contains = "alice" }, subject = { contains = "lunch" } }]
actions = { flagged = true }
"#;
        assert!(!fires(toml, &msg("alice@x", "p@x", "lunch?", 100, false)));
        assert!(fires(toml, &msg("alice@x", "p@x", "report", 100, false)));
        assert!(fires(toml, &msg("bob@x", "p@x", "lunch?", 100, false)));
    }

    #[test]
    fn a_list_of_values_matches_any_of_them() {
        let toml = r#"
[[rules]]
name = "x"
match.subject = { contains = ["receipt", "INVOICE"] }
match.from = { equals = ["a@x", "b@x"] }
actions = { flagged = true }
"#;
        assert!(fires(
            toml,
            &msg("A <a@x>", "p@x", "Your invoice", 100, false)
        ));
        assert!(fires(toml, &msg("b@x", "p@x", "receipt 12", 100, false)));
        assert!(!fires(toml, &msg("c@x", "p@x", "receipt 12", 100, false)));
        assert!(!fires(toml, &msg("a@x", "p@x", "hello", 100, false)));
    }

    #[test]
    fn any_needs_one_branch_and_the_rest_still_holds() {
        let toml = r#"
[[rules]]
name = "either"
match.read = false
match.any = [{ from = { contains = "alice" } }, { subject = { contains = "urgent" } }]
actions = { flagged = true }
"#;
        assert!(fires(toml, &msg("alice@x", "p@x", "hi", 100, false)));
        assert!(fires(toml, &msg("bob@x", "p@x", "URGENT", 100, false)));
        assert!(!fires(toml, &msg("bob@x", "p@x", "hi", 100, false)));
        assert!(!fires(toml, &msg("alice@x", "p@x", "hi", 100, true)));
    }

    #[test]
    fn every_listed_header_must_match() {
        let toml = r#"
[[rules]]
name = "both"
match.header = { List-Id = { contains = "github.com" }, Subject = { contains = "PR" } }
actions = { flagged = true }
"#;
        assert!(fires(
            toml,
            &msg("bot@github.com", "p@x", "PR 1", 100, false)
        ));
        assert!(!fires(
            toml,
            &msg("bot@github.com", "p@x", "Issue", 100, false)
        ));
        assert!(!fires(toml, &msg("a@x", "p@x", "PR 1", 100, false)));
    }

    #[test]
    fn tag_matches_keywords_ignoring_case() {
        let toml =
            "[[rules]]\nname = \"t\"\nmatch.tag = \"$label1\"\nactions = { flagged = true }\n";
        let mut m = msg("a@x", "p@x", "s", 100, false);
        assert!(!fires(toml, &m));
        m.flags = "\\Seen $Label1".into();
        assert!(fires(toml, &m));
        m.flags = "$label10".into();
        assert!(!fires(toml, &m));
    }

    #[test]
    fn rule_needing_a_missing_body_never_fires_even_negated() {
        let toml = "[[rules]]\nname = \"b\"\nmatch.none = [{ body = { contains = \"keep\" } }]\nactions = { delete = true }\n";
        let r = rules(toml, 0);
        assert!(folder_needs_body(&r, "work", "INBOX"));
        let mut m = msg("a@x", "p@x", "s", 100, false);
        assert!(!fires(toml, &m));
        m.body_text = Some("please keep this".into());
        assert!(!fires(toml, &m));
        m.body_text = Some("junk".into());
        assert!(fires(toml, &m));
    }

    #[test]
    fn flagged_matches_the_flagged_state() {
        let toml = "[[rules]]\nname = \"f\"\nmatch.flagged = true\nactions = { read = true }\n";
        let mut m = msg("a@x", "p@x", "s", 100, false);
        assert!(!fires(toml, &m));
        m.flags = "\\Flagged".into();
        assert!(fires(toml, &m));
        let unflagged = toml.replace("flagged = true", "flagged = false");
        assert!(!fires(&unflagged, &m));
    }

    #[test]
    fn larger_than_compares_the_message_size() {
        let toml = "[[rules]]\nname = \"big\"\nmatch.larger_than = \"1KB\"\nactions = { flagged = true }\n";
        let mut m = msg("a@x", "p@x", "s", 100, false);
        assert!(!fires(toml, &m), "an unknown size is not large");
        m.size = Some(1024);
        assert!(!fires(toml, &m));
        m.size = Some(1025);
        assert!(fires(toml, &m));
    }

    #[test]
    fn stop_keeps_later_rules_from_acting() {
        let id = identity();
        let toml = r#"
[[rules]]
name = "first"
match.subject = { contains = "x" }
actions = { tag = "$label1", stop = true }

[[rules]]
name = "second"
match.subject = { contains = "x" }
actions = { move = "Later" }
"#;
        let plan = evaluate(
            &rules(toml, 0),
            &msg("a@x", "p@x", "x", 100, false),
            &ctx(&id, 200),
        );
        assert_eq!(
            plan.actions,
            vec![PlannedAction {
                rule: "first".into(),
                action: Action::Tag("$label1".into())
            }]
        );
        assert!(plan.notify);
    }

    #[test]
    fn header_matches_the_decoded_value() {
        let toml = "[[rules]]\nname = \"h\"\nmatch.header.X-Team = { contains = \"grüße\" }\nactions = { flagged = true }\n";
        let mut m = msg("a@x", "p@x", "s", 100, false);
        m.headers = b"X-Team: =?UTF-8?Q?Gr=C3=BC=C3=9Fe?=\r\n\r\n".to_vec();
        assert!(fires(toml, &m));
    }

    #[test]
    fn from_domain_matches_the_address_domain_and_its_subdomains_only() {
        let toml = "[[rules]]\nname = \"ms\"\nmatch.from_domain = \"microsoft.com\"\nactions = { flagged = true }\n";
        for (from, hit) in [
            ("Microsoft <account-security@microsoft.com>", true),
            ("x@AccountProtection.Microsoft.com", true),
            ("x@notmicrosoft.com", false),
            ("x@microsoft.com.evil", false),
            ("\"microsoft.com\" <x@scam.example>", false),
            ("\"x@microsoft.com\" <x@scam.example>", false),
            ("microsoft.com", false),
            ("a@scam.example, b@microsoft.com", true),
        ] {
            assert_eq!(
                fires(toml, &msg(from, "p@x", "s", 100, false)),
                hit,
                "{from}"
            );
        }
    }

    #[test]
    fn domain_lists_match_any_and_to_domain_reads_the_to_header() {
        let toml = "[[rules]]\nname = \"pp\"\nmatch.from_domain = [\"paypal.nl\", \"paypal.com\"]\nmatch.to_domain = \"example.org\"\nactions = { flagged = true }\n";
        assert!(fires(
            toml,
            &msg("a@paypal.nl", "Me <me@mail.example.org>", "s", 100, false)
        ));
        assert!(fires(
            toml,
            &msg("a@e.paypal.com", "x@y, me@example.org", "s", 100, false)
        ));
        assert!(!fires(
            toml,
            &msg("a@paypal.de", "me@example.org", "s", 100, false)
        ));
        assert!(!fires(
            toml,
            &msg("a@paypal.nl", "\"me@example.org\" <a@b.c>", "s", 100, false)
        ));
        let mut cc_only = msg("a@paypal.nl", "x@y", "s", 100, false);
        cc_only.cc_addr = Some("me@example.org".into());
        assert!(!fires(toml, &cc_only), "to_domain reads To, as `to` does");
    }

    #[test]
    fn authenticated_believes_only_the_accounts_provider() {
        let account = |trusted: Option<Vec<String>>| {
            let password = PasswordSource::Keyring { keyring: true };
            let mut account = AccountConfig::new("work", "imap.soverin.net", "me@x.test", password);
            account.trusted_authserv_ids = trusted;
            account.identity().unwrap()
        };
        let id = account(Some(vec!["mx.soverin.net".into()]));
        let rule = |want: bool| {
            rules(
                &format!(
                    "[[rules]]\nname = \"a\"\nmatch.from_domain = \"paypal.com\"\nmatch.authenticated = {want}\nactions = {{ flagged = true }}\n"
                ),
                0,
            )
        };
        let with = |headers: &str| {
            let mut m = msg("PayPal <service@paypal.com>", "me@x.test", "s", 100, false);
            m.headers = format!("{headers}From: PayPal <service@paypal.com>\r\n\r\n").into_bytes();
            m
        };
        let fired =
            |want: bool, m: &Message| !evaluate(&rule(want), m, &ctx(&id, 200)).actions.is_empty();
        let cases = [
            (
                "Authentication-Results: mx.soverin.net; dkim=pass header.d=paypal.com; dmarc=pass header.from=paypal.com\r\n",
                true,
            ),
            (
                "Authentication-Results: mx.soverin.net; dkim=pass header.d=mail.paypal.com\r\n",
                true,
            ),
            (
                "Authentication-Results: mx.soverin.net; dkim=pass header.d=sendgrid.net; spf=pass\r\n",
                false,
            ),
            (
                "Authentication-Results: mx.soverin.net; dmarc=fail header.from=paypal.com\r\n",
                false,
            ),
            // Forged by the sender: an authserv-id that is not the provider's.
            (
                "Authentication-Results: mx.paypal.com; dmarc=pass header.from=paypal.com\r\n",
                false,
            ),
            // The provider's header comes first; a pass further down is older, and may be forged.
            (
                "Authentication-Results: mx.soverin.net; dmarc=fail header.from=paypal.com\r\nAuthentication-Results: mx.soverin.net; dmarc=pass header.from=paypal.com\r\n",
                false,
            ),
            // Only the listed server counts, not others in its domain.
            (
                "Authentication-Results: mx.google.com; dmarc=fail header.from=paypal.com\r\nAuthentication-Results: mx2.soverin.net; dmarc=pass header.from=paypal.com\r\n",
                false,
            ),
            ("", false),
        ];
        for (headers, authenticated) in cases {
            let m = with(headers);
            assert_eq!(fired(true, &m), authenticated, "{headers}");
            assert_eq!(fired(false, &m), !authenticated, "{headers}");
        }
        // A regression: without a list nothing is trusted, not even a server in the IMAP host's domain, since on a
        // server that writes no Authentication-Results the topmost one is the sender's.
        let forged = with(
            "Authentication-Results: mx.soverin.net; dkim=pass header.d=paypal.com; dmarc=pass header.from=paypal.com\r\n",
        );
        for trusted in [None, Some(Vec::new())] {
            let id = account(trusted);
            assert!(
                evaluate(&rule(true), &forged, &ctx(&id, 200))
                    .actions
                    .is_empty()
            );
            assert!(
                !evaluate(&rule(false), &forged, &ctx(&id, 200))
                    .actions
                    .is_empty()
            );
        }
        let mut gmail = AccountConfig::new(
            "gmail",
            "imap.gmail.com",
            "me@gmail.com",
            PasswordSource::Keyring { keyring: true },
        );
        gmail.trusted_authserv_ids = Some(vec!["mx.google.com".into()]);
        let gmail = gmail.identity().unwrap();
        let listed = |headers: &str| {
            !evaluate(&rule(true), &with(headers), &ctx(&gmail, 200))
                .actions
                .is_empty()
        };
        assert!(listed(
            "Authentication-Results: mx.google.com; dmarc=pass header.from=paypal.com\r\nAuthentication-Results: mx.google.com; dmarc=fail\r\n"
        ));
        assert!(
            listed(
                "Authentication-Results: spam.example; none\r\nAuthentication-Results: mx.google.com; dkim=pass header.d=paypal.com\r\n"
            ),
            "a listed id counts below a header the list does not name"
        );
        assert!(!listed(
            "Authentication-Results: mx.google.com; dmarc=fail header.from=paypal.com\r\nAuthentication-Results: mx.google.com; dmarc=pass header.from=paypal.com\r\n"
        ));
        assert!(!listed(
            "Authentication-Results: mx.soverin.net; dmarc=pass header.from=paypal.com\r\n"
        ));
    }

    #[test]
    fn disabled_rules_are_skipped() {
        let id = identity();
        let toml = "[[rules]]\nname = \"off\"\nenabled = false\nmatch.read = false\nactions = { flagged = true }\n";
        assert!(
            evaluate(
                &rules(toml, 0),
                &msg("a@x", "p@x", "s", 100, false),
                &ctx(&id, 200)
            )
            .actions
            .is_empty()
        );
    }
}
