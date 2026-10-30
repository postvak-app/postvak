//! Sender statistics: a folder's stored messages grouped by sender address or registrable domain, with their counts,
//! dates, a few subjects and the rules that already match them. What `senders` shows and what `rules coverage` says
//! about mail no rule matches.
use std::collections::HashMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::message::bare_addresses;
use crate::rules::CompiledRule;
use crate::rules::engine::{Context, folder_needs_body, matching};
use crate::store::{Message, Store, StoreError};

/// How many subjects a group shows, newest first.
pub const EXAMPLE_SUBJECTS: usize = 3;

/// What a group of senders has in common.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum GroupBy {
    /// The sender's address, lowercased
    #[default]
    Address,
    /// The registrable domain of the sender's address: mail.example.co.uk and example.co.uk are both example.co.uk
    Domain,
}

/// One sender, or one domain, and its mail in the folder.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Sender {
    /// The address or domain; empty for mail without a sender address.
    pub sender: String,
    pub count: u32,
    pub unread: u32,
    /// The arrival time of the oldest and the newest message, as Unix seconds.
    pub first: i64,
    pub last: i64,
    /// Up to `EXAMPLE_SUBJECTS` distinct subjects, newest first.
    pub subjects: Vec<String>,
    /// The rules that match at least one message of the group, in rules.toml order.
    pub rules: Vec<String>,
}

/// The group `from` belongs to: its first address, or that address's registrable domain. A host the public suffix
/// list does not know, such as `localhost`, is its own domain.
pub fn sender_key(from: &str, by: GroupBy) -> String {
    let Some(address) = bare_addresses(from).into_iter().next() else {
        return String::new();
    };
    match by {
        GroupBy::Address => address,
        GroupBy::Domain => {
            let host = address
                .rsplit_once('@')
                .map_or("", |(_, host)| host)
                .trim_end_matches('.');
            psl::domain_str(host).unwrap_or(host).to_string()
        }
    }
}

/// `messages` grouped by sender, the largest group first, then the most recent. A message counts toward a rule when
/// `matching` says so under `ctx`; disabled rules never match. `messages` come newest first, as the store reads
/// them, so the example subjects are the newest.
pub fn senders(
    messages: &[Message],
    rules: &[CompiledRule],
    ctx: &Context,
    by: GroupBy,
) -> Vec<Sender> {
    let mut groups: Vec<(Sender, Vec<bool>)> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for msg in messages {
        let key = sender_key(msg.from_addr.as_deref().unwrap_or(""), by);
        let at = *index.entry(key.clone()).or_insert_with(|| {
            groups.push((
                Sender {
                    sender: key,
                    count: 0,
                    unread: 0,
                    first: msg.internaldate,
                    last: msg.internaldate,
                    subjects: Vec::new(),
                    rules: Vec::new(),
                },
                vec![false; rules.len()],
            ));
            groups.len() - 1
        });
        let (group, matched) = &mut groups[at];
        group.count += 1;
        group.unread += u32::from(!msg.is_seen());
        group.first = group.first.min(msg.internaldate);
        group.last = group.last.max(msg.internaldate);
        let subject = msg.subject.as_deref().unwrap_or("").trim();
        if group.subjects.len() < EXAMPLE_SUBJECTS
            && !subject.is_empty()
            && !group.subjects.iter().any(|s| s == subject)
        {
            group.subjects.push(subject.to_string());
        }
        for (at, _) in matching(rules, msg, ctx) {
            matched[at] = true;
        }
    }
    let mut out: Vec<Sender> = groups
        .into_iter()
        .map(|(mut group, matched)| {
            group.rules = rules
                .iter()
                .zip(matched)
                .filter(|(_, matched)| *matched)
                .map(|(rule, _)| rule.rule.name.clone())
                .collect();
            group
        })
        .collect();
    out.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then(b.last.cmp(&a.last))
            .then_with(|| a.sender.cmp(&b.sender))
    });
    out
}

/// The senders of `folder`'s stored mail that arrived at or after `after` and before `before`, as `senders` groups
/// them. Body text is read only when a rule that could match the folder's mail tests it.
pub fn in_folder(
    store: &Store,
    rules: &[CompiledRule],
    ctx: &Context,
    folder: &str,
    after: Option<i64>,
    before: Option<i64>,
    by: GroupBy,
) -> Result<Vec<Sender>, StoreError> {
    let with_body = folder_needs_body(rules, ctx.account, folder);
    let messages = store.messages_between(folder, after, before, with_body)?;
    Ok(senders(&messages, rules, ctx, by))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AccountConfig, Identity, PasswordSource};
    use crate::rules::engine::Mode;
    use crate::rules::{compile, parse};

    fn identity() -> Identity {
        let password = PasswordSource::Keyring { keyring: true };
        AccountConfig::new("work", "h", "me@example.com", password)
            .identity()
            .unwrap()
    }

    fn ctx(identity: &Identity) -> Context<'_> {
        Context {
            account: "work",
            identity,
            now: 1_000_000,
            mode: Mode::ApplyExisting,
            notify_default: false,
            keep_in_folder: false,
        }
    }

    fn msg(uid: u32, from: &str, subject: &str, at: i64, seen: bool) -> Message {
        Message {
            folder: "INBOX".into(),
            uid,
            from_addr: Some(from.into()),
            subject: Some(subject.into()),
            internaldate: at,
            flags: if seen { "\\Seen".into() } else { String::new() },
            ..Default::default()
        }
    }

    #[test]
    fn keys_are_the_lowercase_address_or_its_registrable_domain() {
        let from = "Shop <News@Mail.Shop.co.uk>";
        assert_eq!(sender_key(from, GroupBy::Address), "news@mail.shop.co.uk");
        assert_eq!(sender_key(from, GroupBy::Domain), "shop.co.uk");
        assert_eq!(sender_key("a@x.github.com", GroupBy::Domain), "github.com");
        assert_eq!(sender_key("root@localhost", GroupBy::Domain), "localhost");
        assert_eq!(sender_key("", GroupBy::Address), "");
        assert_eq!(sender_key("", GroupBy::Domain), "");
    }

    #[test]
    fn groups_count_unread_dates_subjects_and_matching_rules() {
        let rules = compile(
            &parse(
                r#"
[[rules]]
name = "off"
enabled = false
match.from = { contains = "shop" }
actions = { flagged = true }

[[rules]]
name = "receipts"
match.subject = { contains = "receipt" }
actions = { archive = true }

[[rules]]
name = "news"
match.from = { contains = "news@" }
actions = { read = true }
"#,
            )
            .unwrap(),
        )
        .unwrap();
        let messages = [
            msg(5, "news@shop.example", "Sale", 500, false),
            msg(4, "orders@shop.example", "Your receipt", 400, true),
            msg(3, "news@shop.example", "Sale", 300, true),
            msg(2, "news@shop.example", "Hello", 200, false),
            msg(1, "news@shop.example", "Old", 100, false),
            msg(0, "friend@other.example", "", 50, false),
        ];
        let id = identity();
        let by_address = senders(&messages, &rules, &ctx(&id), GroupBy::Address);
        assert_eq!(
            by_address[0],
            Sender {
                sender: "news@shop.example".into(),
                count: 4,
                unread: 3,
                first: 100,
                last: 500,
                subjects: vec!["Sale".into(), "Hello".into(), "Old".into()],
                rules: vec!["news".into()],
            }
        );
        // Equal counts: the more recent group first.
        assert_eq!(by_address[1].sender, "orders@shop.example");
        assert_eq!(by_address[1].rules, ["receipts"]);
        assert_eq!(by_address[2].sender, "friend@other.example");
        assert!(by_address[2].subjects.is_empty() && by_address[2].rules.is_empty());

        let by_domain = senders(&messages, &rules, &ctx(&id), GroupBy::Domain);
        assert_eq!(by_domain.len(), 2);
        assert_eq!(
            (by_domain[0].sender.as_str(), by_domain[0].count),
            ("shop.example", 5)
        );
        assert_eq!(by_domain[0].rules, ["receipts", "news"]);
    }
}
