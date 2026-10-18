//! Direct actions on chosen messages: mark, move, archive and delete, through the same `apply` as rules, which the
//! daemon's sync loop runs; plus the dry-run and rule preview helpers the CLI and the MCP server share.
use crate::config::{AccountConfig, Identity};
use crate::mail_ops::{MailError, MailOps};
use crate::message::clean;
use crate::rules::apply::{ApplyError, apply, ensure_raw, move_target};
use crate::rules::engine::{Context, Mode, Plan, PlannedAction, evaluate};
use crate::rules::{Action, CompiledRule};
use crate::store::{Message, Store, StoreError};
use crate::trash::Trash;

/// The rule name the CLI logs direct actions under.
pub const RULE_NAME: &str = "cli";

#[derive(Debug, thiserror::Error)]
pub enum ActionError {
    #[error(transparent)]
    Apply(#[from] ApplyError),
    #[error("{0} changed on the server since the last sync; run `postvak sync` first")]
    FolderChanged(String),
    #[error(transparent)]
    Mail(#[from] MailError),
    #[error("no message {folder}/{uid} in the local store; run `postvak sync` first")]
    NotFound { folder: String, uid: u32 },
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("{0} has not been synced yet; run `postvak sync` first")]
    UnknownFolder(String),
}

/// The outcome for one uid: how many actions ran, or why none did.
pub type UidResult = (u32, Result<usize, ActionError>);

/// Selects `folder`, refusing when the stored uids belong to another UIDVALIDITY and would hit unrelated messages.
fn select_synced(ops: &mut dyn MailOps, store: &Store, folder: &str) -> Result<(), ActionError> {
    let stored = store
        .folder(folder)?
        .ok_or_else(|| ActionError::UnknownFolder(folder.to_string()))?;
    if ops.select(folder)?.uidvalidity != stored.uidvalidity {
        return Err(ActionError::FolderChanged(folder.to_string()));
    }
    Ok(())
}

/// Runs `action` on each uid in `folder`, logged under `rule_name`. One result per uid, so a missing message does not
/// stop the others.
#[allow(clippy::too_many_arguments)]
pub fn run(
    ops: &mut dyn MailOps,
    store: &Store,
    trash: &Trash,
    folder: &str,
    uids: &[u32],
    action: &Action,
    rule_name: &str,
    now: i64,
) -> Result<Vec<UidResult>, ActionError> {
    select_synced(ops, store, folder)?;
    let plan = Plan {
        actions: vec![PlannedAction {
            rule: rule_name.to_string(),
            action: action.clone(),
        }],
        notify: false,
    };
    let mut results = Vec::new();
    for &uid in uids {
        let result = match store.message(folder, uid) {
            Ok(Some(msg)) => apply(&plan, &msg, ops, store, trash, now).map_err(ActionError::from),
            Ok(None) => Err(ActionError::NotFound {
                folder: folder.to_string(),
                uid,
            }),
            Err(e) => Err(e.into()),
        };
        results.push((uid, result));
    }
    Ok(results)
}

/// Downloads and indexes every body `folder` lacks. Returns how many arrived; a message that cannot be fetched is
/// logged and skipped.
pub fn fetch_bodies(
    ops: &mut dyn MailOps,
    store: &Store,
    folder: &str,
) -> Result<usize, ActionError> {
    let missing = store.messages_without_body(folder)?;
    if missing.is_empty() {
        return Ok(0);
    }
    select_synced(ops, store, folder)?;
    let mut fetched = 0;
    for msg in &missing {
        match ensure_raw(msg, ops, store) {
            Ok(_) => fetched += 1,
            Err(e) => log::warn!("{}/{}: body fetch failed: {e}", msg.folder, msg.uid),
        }
    }
    Ok(fetched)
}

/// Downloads and indexes one message body, refusing when the folder changed on the server since the last sync.
pub fn fetch_body(
    ops: &mut dyn MailOps,
    store: &Store,
    folder: &str,
    uid: u32,
) -> Result<(), ActionError> {
    select_synced(ops, store, folder)?;
    let msg = store
        .message(folder, uid)?
        .ok_or_else(|| ActionError::NotFound {
            folder: folder.to_string(),
            uid,
        })?;
    ensure_raw(&msg, ops, store)?;
    Ok(())
}

/// The most hits one folder adds to a server search, the highest uids first.
const SERVER_HITS_PER_FOLDER: usize = 500;

/// The words of a search query that a server search needs all of; control characters cannot go into IMAP.
pub fn search_words(query: &str) -> Vec<String> {
    query
        .split_whitespace()
        .map(|word| clean(word, false))
        .filter(|word| !word.is_empty())
        .collect()
}

/// Searches `folder` on the server for messages whose headers or body contain every word. Returns the uids the store
/// knows; mail not synced yet waits for the next sync.
pub fn search_server(
    ops: &mut dyn MailOps,
    store: &Store,
    folder: &str,
    words: &[String],
) -> Result<Vec<u32>, ActionError> {
    if words.is_empty() {
        return Ok(Vec::new());
    }
    select_synced(ops, store, folder)?;
    let mut hits = Vec::new();
    for uid in ops
        .search_text(words)?
        .into_iter()
        .rev()
        .take(SERVER_HITS_PER_FOLDER)
    {
        if store.message(folder, uid)?.is_some() {
            hits.push(uid);
        }
    }
    Ok(hits)
}

/// Local search results plus the server's hits the store knows, each once, newest first, at most `limit`.
pub fn with_server_hits(
    store: &Store,
    mut found: Vec<Message>,
    hits: &[(String, u32)],
    limit: u32,
) -> Result<Vec<Message>, StoreError> {
    for (folder, uid) in hits {
        if found.iter().any(|m| m.folder == *folder && m.uid == *uid) {
            continue;
        }
        if let Some(message) = store.message(folder, *uid)? {
            found.push(message);
        }
    }
    found.sort_by_key(|m| std::cmp::Reverse((m.internaldate, m.uid)));
    found.truncate(limit as usize);
    Ok(found)
}

/// What a dry run would do to `msg`; a user delete names its outcome, because an expunge cannot be undone on the server.
pub fn planned_effect(store: &Store, msg: &Message, action: &Action) -> Result<String, StoreError> {
    if *action != Action::Trash {
        return Ok(format!("would {}", clean(&action.label(), false)));
    }
    let effect = match move_target(store, &msg.folder, action)? {
        Some(trash) => format!("would move to {}", clean(&trash, false)),
        None => "would delete (expunge, .eml backup kept)".to_string(),
    };
    Ok(effect)
}

/// One action a rule would take on a cached message.
#[derive(Debug, Clone)]
pub struct Planned {
    pub rule: String,
    pub message: Message,
    pub action: Action,
}

/// What `rules` would do to every message cached in `store`, evaluated as `rules apply-existing` would.
pub fn planned(
    rules: &[CompiledRule],
    store: &Store,
    account: &AccountConfig,
    identity: &Identity,
    now: i64,
) -> Result<Vec<Planned>, StoreError> {
    let ctx = Context {
        account: &account.name,
        identity,
        now,
        mode: Mode::ApplyExisting,
        notify_default: false,
    };
    let mut planned = Vec::new();
    for folder in store.folders()? {
        for message in store.messages_in_folder(&folder.name)? {
            for PlannedAction { rule, action } in evaluate(rules, &message, &ctx).actions {
                planned.push(Planned {
                    rule,
                    message: message.clone(),
                    action,
                });
            }
        }
    }
    Ok(planned)
}

/// Whether `planned` would remove the message from the server for good rather than move or flag it.
pub fn expunges(store: &Store, planned: &Planned) -> Result<bool, StoreError> {
    Ok(match planned.action {
        Action::Delete => true,
        Action::Trash => move_target(store, &planned.message.folder, &Action::Trash)?.is_none(),
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mail_ops::RecordingOps;
    use crate::store::{Folder, Message};

    fn setup() -> (RecordingOps, Store, Trash, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let mut ops = RecordingOps::default().with_folder("INBOX", None);
        ops.add_mail(
            "INBOX",
            5,
            100,
            "Subject: hi\r\n\r\n",
            Some("Subject: hi\r\n\r\nbody"),
        );
        let store = Store::open_in_memory().unwrap();
        store
            .upsert_folder(&Folder {
                name: "INBOX".into(),
                uidvalidity: 1,
                last_uid: 5,
                special_use: None,
                delimiter: None,
            })
            .unwrap();
        store
            .insert_message(&Message {
                folder: "INBOX".into(),
                uid: 5,
                thread_id: "t".into(),
                subject: Some("hi".into()),
                internaldate: 100,
                headers: b"Subject: hi\r\n\r\n".to_vec(),
                ..Default::default()
            })
            .unwrap();
        (ops, store, Trash::new(dir.path().join("trash")), dir)
    }

    #[test]
    fn planned_effect_names_a_delete_without_trash() {
        let (_ops, store, _trash, _dir) = setup();
        let msg = store.message("INBOX", 5).unwrap().unwrap();
        assert_eq!(
            planned_effect(&store, &msg, &Action::Trash).unwrap(),
            "would delete (expunge, .eml backup kept)"
        );
        assert_eq!(
            planned_effect(&store, &msg, &Action::Archive).unwrap(),
            "would archive"
        );
    }

    #[test]
    fn expunges_only_deletes_and_trash_without_a_trash_folder() {
        let (_ops, store, _trash, _dir) = setup();
        let message = store.message("INBOX", 5).unwrap().unwrap();
        let expunges_with = |action: Action| {
            let planned = Planned {
                rule: "r".into(),
                message: message.clone(),
                action,
            };
            expunges(&store, &planned).unwrap()
        };
        assert!(expunges_with(Action::Delete));
        assert!(expunges_with(Action::Trash));
        assert!(!expunges_with(Action::Archive));
        assert!(!expunges_with(Action::Flag));
        store
            .upsert_folder(&Folder {
                name: "Trash".into(),
                uidvalidity: 1,
                last_uid: 0,
                special_use: Some("Trash".into()),
                delimiter: None,
            })
            .unwrap();
        assert!(!expunges_with(Action::Trash));
        assert!(expunges_with(Action::Delete));
    }

    #[test]
    fn run_applies_to_each_uid_and_reports_missing_ones() {
        let (mut ops, store, trash, _dir) = setup();
        let results = run(
            &mut ops,
            &store,
            &trash,
            "INBOX",
            &[5, 99],
            &Action::Flag,
            "mcp:test-host",
            500,
        )
        .unwrap();
        assert!(matches!(results[0], (5, Ok(1))));
        assert!(matches!(
            results[1],
            (99, Err(ActionError::NotFound { .. }))
        ));
        assert_eq!(ops.mail["INBOX"][0].flags, ["\\Flagged"]);
        assert_eq!(store.log(10).unwrap()[0].rule_name, "mcp:test-host");
    }

    #[test]
    fn run_refuses_a_folder_whose_uidvalidity_changed() {
        let (mut ops, store, trash, _dir) = setup();
        ops.uidvalidity.insert("INBOX".into(), 2);
        let err = run(
            &mut ops,
            &store,
            &trash,
            "INBOX",
            &[5],
            &Action::Trash,
            RULE_NAME,
            500,
        )
        .unwrap_err();
        assert!(matches!(err, ActionError::FolderChanged(_)));
        assert_eq!(ops.mail["INBOX"].len(), 1);
        assert_eq!(ops.calls, ["select INBOX"]);
    }

    #[test]
    fn run_refuses_a_folder_that_was_never_synced() {
        let (mut ops, store, trash, _dir) = setup();
        let err = run(
            &mut ops,
            &store,
            &trash,
            "Receipts",
            &[5],
            &Action::Flag,
            RULE_NAME,
            500,
        )
        .unwrap_err();
        assert!(matches!(err, ActionError::UnknownFolder(_)));
        assert!(ops.calls.is_empty());
    }

    #[test]
    fn fetch_bodies_indexes_only_missing_bodies() {
        let (mut ops, store, _trash, _dir) = setup();
        assert_eq!(fetch_bodies(&mut ops, &store, "INBOX").unwrap(), 1);
        assert_eq!(store.search("body", None, 10).unwrap().len(), 1);
        let calls = ops.calls.len();
        assert_eq!(fetch_bodies(&mut ops, &store, "INBOX").unwrap(), 0);
        assert_eq!(ops.calls.len(), calls, "nothing left to fetch");
    }

    #[test]
    fn search_server_returns_the_hits_the_store_knows() {
        let (mut ops, store, _trash, _dir) = setup();
        let mut ops = {
            ops.add_mail(
                "INBOX",
                9,
                300,
                "Subject: unsynced\r\n\r\n",
                Some("Subject: unsynced\r\n\r\nbody"),
            );
            ops.with_folder("Archive", None)
        };
        ops.add_mail(
            "Archive",
            7,
            200,
            "Subject: old\r\n\r\n",
            Some("Subject: old\r\n\r\nsome BODY text"),
        );
        ops.add_mail(
            "Archive",
            8,
            250,
            "Subject: other\r\n\r\n",
            Some("Subject: other\r\n\r\nnothing"),
        );
        store
            .upsert_folder(&Folder {
                name: "Archive".into(),
                uidvalidity: 1,
                last_uid: 8,
                special_use: None,
                delimiter: None,
            })
            .unwrap();
        for uid in [7, 8] {
            store
                .insert_message(&Message {
                    folder: "Archive".into(),
                    uid,
                    thread_id: format!("t{uid}"),
                    ..Default::default()
                })
                .unwrap();
        }
        let words = search_words("body \u{7}");
        assert_eq!(words, ["body"]);
        assert_eq!(
            search_server(&mut ops, &store, "Archive", &words).unwrap(),
            [7]
        );
        assert_eq!(
            search_server(&mut ops, &store, "INBOX", &words).unwrap(),
            [5],
            "uid 9 is not synced yet"
        );
        ops.calls.clear();
        assert!(
            search_server(&mut ops, &store, "INBOX", &search_words("  "))
                .unwrap()
                .is_empty()
        );
        assert!(ops.calls.is_empty());
    }

    #[test]
    fn with_server_hits_adds_the_known_ones_newest_first_once() {
        let (_ops, store, _trash, _dir) = setup();
        let older = Message {
            folder: "INBOX".into(),
            uid: 2,
            thread_id: "t2".into(),
            internaldate: 50,
            ..Default::default()
        };
        store.insert_message(&older).unwrap();
        let local = store.message("INBOX", 5).unwrap().into_iter().collect();
        let hits = [
            ("INBOX".to_string(), 2),
            ("INBOX".to_string(), 5),
            ("INBOX".to_string(), 77),
        ];
        let merged = with_server_hits(&store, local, &hits, 10).unwrap();
        let uids: Vec<u32> = merged.iter().map(|m| m.uid).collect();
        assert_eq!(uids, [5, 2]);
        assert_eq!(
            with_server_hits(&store, Vec::new(), &hits, 1)
                .unwrap()
                .len(),
            1
        );
    }
}
