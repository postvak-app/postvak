//! How often each rule acted, summed over the accounts' stores: what `rules list`, MCP `rules_list` and the Rules view
//! show beside each rule.
use std::collections::HashMap;

use crate::store::{Store, StoreError};

/// How far back `last_30d` counts, in seconds.
pub const WINDOW: i64 = 30 * 86_400;

/// One rule's hits: the messages it acted on in the last 30 days, and when it last acted, if ever.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Hits {
    pub last_30d: u64,
    pub last_fired: Option<i64>,
}

/// Each rule's hits over `stores`, keyed by rule name, as of `now`.
pub fn total(stores: &[&Store], now: i64) -> Result<HashMap<String, Hits>, StoreError> {
    let mut hits: HashMap<String, Hits> = HashMap::new();
    for store in stores {
        for found in store.rule_hits(now - WINDOW)? {
            let rule = hits.entry(found.rule).or_default();
            rule.last_30d += found.messages;
            rule.last_fired = rule.last_fired.max(Some(found.last_fired));
        }
    }
    Ok(hits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{LogEntry, Store};

    fn log(store: &Store, rule: &str, uid: u32, at: i64) {
        store
            .log_action(&LogEntry {
                id: 0,
                at,
                rule_name: rule.into(),
                folder: "INBOX".into(),
                uid,
                message_id: None,
                subject: None,
                action: "read".into(),
                trash_file: None,
            })
            .unwrap();
    }

    #[test]
    fn counts_add_up_across_accounts() {
        let now = 100 * 86_400;
        let (work, home) = (
            Store::open_in_memory().unwrap(),
            Store::open_in_memory().unwrap(),
        );
        log(&work, "travel", 1, now - 10);
        log(&work, "travel", 2, now - 20);
        log(&home, "travel", 1, now - 5);
        log(&home, "old", 1, now - 40 * 86_400);
        let hits = total(&[&work, &home], now).unwrap();
        assert_eq!(
            hits["travel"],
            Hits {
                last_30d: 3,
                last_fired: Some(now - 5)
            }
        );
        assert_eq!(
            hits["old"],
            Hits {
                last_30d: 0,
                last_fired: Some(now - 40 * 86_400)
            }
        );
    }
}
