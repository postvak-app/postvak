//! Direct actions on chosen messages, each sent to the server for all of them at once: mark, move, archive and delete, through the same `apply` as rules, which the
//! daemon's sync loop runs; plus the dry-run and rule preview helpers the CLI and the MCP server share.
use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::config::{AccountConfig, Identity};
use crate::mail_ops::{MailError, MailOps, Structure};
use crate::message::{self, Attachment, HtmlBody, InlinePart, clean};
use crate::rules::apply::{ApplyError, apply_many, ensure_raw, move_target, plan_for};
use crate::rules::engine::{Context, Mode, PlannedAction};
use crate::rules::{Action, CompiledRule};
use crate::search::Term;
use crate::store::{Hit, Matched, Message, Store, StoreError};
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
pub(crate) fn select_synced(
    ops: &mut dyn MailOps,
    store: &Store,
    folder: &str,
) -> Result<(), ActionError> {
    let stored = store
        .folder(folder)?
        .ok_or_else(|| ActionError::UnknownFolder(folder.to_string()))?;
    if ops.select(folder)?.uidvalidity != stored.uidvalidity {
        return Err(ActionError::FolderChanged(folder.to_string()));
    }
    Ok(())
}

/// Runs `action` on each uid in `folder`, logged under `rule_name`, as one server command per batch where it can. One
/// result per uid, so a missing message does not stop the others.
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
    let planned = PlannedAction {
        rule: rule_name.to_string(),
        action: action.clone(),
    };
    let mut results: Vec<UidResult> = Vec::new();
    let mut found = Vec::new();
    let mut seen = HashSet::new();
    for &uid in uids.iter().filter(|&&uid| seen.insert(uid)) {
        match store.message(folder, uid) {
            Ok(Some(msg)) => found.push(msg),
            Ok(None) => results.push((
                uid,
                Err(ActionError::NotFound {
                    folder: folder.to_string(),
                    uid,
                }),
            )),
            Err(e) => results.push((uid, Err(e.into()))),
        }
    }
    let applied = apply_many(&planned, &found, ops, store, trash, now);
    results.extend(
        applied
            .into_iter()
            .map(|(uid, result)| (uid, result.map_err(ActionError::from))),
    );
    // In the order asked, so a caller can pair them with what it sent.
    results.sort_by_key(|(uid, _)| uids.iter().position(|asked| asked == uid));
    Ok(results)
}

/// Below this size a message is fetched whole at once: one round trip beats the several a preview takes.
pub const PREVIEW_MIN_SIZE: u32 = 256 * 1024;

/// What the reader shows of a message before the whole of it is here: its HTML and what its attachments are. Its text
/// is in the store by then, as the indexer would have stored it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Preview {
    pub html: Option<HtmlBody>,
    pub attachments: Vec<Attachment>,
}

/// Downloads and indexes one message body, refusing when the folder changed on the server since the last sync. With
/// `preview`, a large message's text, HTML and `cid:` images are fetched first and handed to it, so a reader can show
/// them while the rest arrives.
pub fn fetch_body(
    ops: &mut dyn MailOps,
    store: &Store,
    folder: &str,
    uid: u32,
    preview: Option<&mut dyn FnMut(Preview)>,
) -> Result<(), ActionError> {
    select_synced(ops, store, folder)?;
    let msg = store
        .message(folder, uid)?
        .ok_or_else(|| ActionError::NotFound {
            folder: folder.to_string(),
            uid,
        })?;
    if let Some(show) = preview
        && msg.size.is_some_and(|size| size > PREVIEW_MIN_SIZE)
        && !store.has_raw(folder, uid)?
    {
        match fetch_preview(ops, store, &msg) {
            Ok(Some(preview)) => show(preview),
            Ok(None) => {}
            Err(e) if crate::sync::connection_lost(&e) => return Err(e),
            // The whole message follows anyway.
            Err(e) => log::warn!("{folder}/{uid}: preview failed: {e}"),
        }
    }
    ensure_raw(&msg, ops, store)?;
    Ok(())
}

/// Fetches the structure of `msg` and the parts a reader shows; stores its text as the indexer would, unless the
/// indexer already did. `None` when the structure names no text, so there is nothing to show before the whole message.
fn fetch_preview(
    ops: &mut dyn MailOps,
    store: &Store,
    msg: &Message,
) -> Result<Option<Preview>, ActionError> {
    let Some(structure) = ops.fetch_structures(&[msg.uid])?.remove(&msg.uid) else {
        return Ok(None);
    };
    if structure.text_parts.is_empty() {
        return Ok(None);
    }
    let shown_html = structure
        .text_parts
        .iter()
        .find(|part| part.html && !part.nested);
    let text_known = msg.body_text.is_some();
    let mut wanted = if text_known {
        Vec::new()
    } else {
        crate::sync::index::wanted_parts(&structure)
    };
    if let Some(html) = shown_html
        && !wanted.contains(&html)
    {
        wanted.push(html);
    }
    let mut parts = Vec::new();
    for part in wanted {
        if let Some(bytes) = ops.fetch_part(msg.uid, &part.section)? {
            parts.push((part.clone(), bytes));
        }
    }
    let html = shown_html.and_then(|shown| {
        let (part, bytes) = parts.iter().find(|(part, _)| part == shown)?;
        message::part_html(part, bytes)
    });
    let html = match html {
        Some(html) => Some(with_inline_parts(ops, msg.uid, &structure, html)?),
        None => None,
    };
    if !text_known {
        // Within an alternative the plain part wins, so the HTML fetched for the reader leaves the text as indexed.
        store.set_indexed(
            &msg.folder,
            msg.uid,
            Some(&message::text_from_parts(&parts)),
            &structure.attachment_names,
            crate::time::now(),
        )?;
    }
    Ok(Some(Preview {
        html,
        attachments: message::attachments_of_structure(&structure.attachments),
    }))
}

/// `html` with the parts its `cid:` URLs name, each fetched once.
fn with_inline_parts(
    ops: &mut dyn MailOps,
    uid: u32,
    structure: &Structure,
    html: String,
) -> Result<HtmlBody, ActionError> {
    let lower = html.to_ascii_lowercase();
    let mut inline = Vec::new();
    for part in &structure.attachments {
        let Some(cid) = &part.content_id else {
            continue;
        };
        if !lower.contains(&format!("cid:{}", cid.to_ascii_lowercase())) {
            continue;
        }
        if let Some(bytes) = ops.fetch_part(uid, &part.section)? {
            inline.push(InlinePart {
                cid: cid.clone(),
                bytes: message::part_contents(part, &bytes),
            });
        }
    }
    Ok(HtmlBody { html, inline })
}

/// The most hits one folder adds to a server search, the highest uids first.
const SERVER_HITS_PER_FOLDER: usize = 500;

/// The words of a search query that a server search needs all of; control characters cannot go into IMAP.
pub fn search_words(query: &str) -> Vec<String> {
    crate::search::parse(query, crate::time::now())
        .terms
        .iter()
        .filter_map(|term| match term {
            Term::Word(text) | Term::Phrase(text) => Some(clean(text, false)),
            Term::Not(_) => None,
        })
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
    mut found: Vec<Hit>,
    hits: &[(String, u32)],
    limit: u32,
) -> Result<Vec<Hit>, StoreError> {
    for (folder, uid) in hits {
        if found
            .iter()
            .any(|hit| hit.message.folder == *folder && hit.message.uid == *uid)
        {
            continue;
        }
        if let Some(message) = store.message(folder, *uid)? {
            found.push(Hit {
                message,
                matched: Matched::Exact,
                snippet: None,
            });
        }
    }
    found.sort_by_key(|hit| std::cmp::Reverse((hit.message.internaldate, hit.message.uid)));
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
        keep_in_folder: false,
    };
    let mut planned = Vec::new();
    for folder in store.folders()? {
        for message in store.messages_in_folder(&folder.name)? {
            for PlannedAction { rule, action } in plan_for(rules, &message, &ctx, store)?.actions {
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

/// What every surface says before `rules apply-existing` expunges mail of `account`; None when the rules expunge none.
pub fn expunge_warning(
    rules: &[CompiledRule],
    store: &Store,
    account: &AccountConfig,
    identity: &Identity,
    now: i64,
) -> Result<Option<String>, StoreError> {
    let mut count = 0;
    for p in planned(rules, store, account, identity, now)? {
        count += usize::from(expunges(store, &p)?);
    }
    Ok((count > 0).then(|| {
        format!(
            "{}: would permanently delete {count} message{} from the server; .eml backups kept locally for {} days",
            account.name,
            if count == 1 { "" } else { "s" },
            account.trash_retention_days
        )
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mail_ops::RecordingOps;
    use crate::store::{Folder, Message, has_flag};

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
    fn expunge_warning_counts_what_the_rules_would_expunge() {
        let (_ops, store, _trash, _dir) = setup();
        let password = crate::config::PasswordSource::Keyring { keyring: true };
        let account = AccountConfig::new("work", "h", "me@example.com", password);
        let identity = account.identity().unwrap();
        let warning = |actions: &str| {
            let toml = format!(
                "[[rules]]\nname = \"r\"\nmatch.subject = {{ contains = \"hi\" }}\nactions = [{actions}]\n"
            );
            let rules = crate::rules::compile(&crate::rules::parse(&toml).unwrap()).unwrap();
            expunge_warning(&rules, &store, &account, &identity, 0).unwrap()
        };
        assert_eq!(
            warning("\"delete\"").as_deref(),
            Some(
                "work: would permanently delete 1 message from the server; .eml backups kept locally for 30 days"
            )
        );
        assert_eq!(warning("\"flag\""), None);
    }

    /// `setup` with messages 5, 6 and 7 in INBOX, 6 already read, and an Archive folder.
    fn three_in_inbox() -> (RecordingOps, Store, Trash, tempfile::TempDir) {
        let (mut ops, store, trash, dir) = setup();
        for uid in [6, 7] {
            ops.add_mail(
                "INBOX",
                uid,
                100,
                "Subject: hi\r\n\r\n",
                Some("Subject: hi\r\n\r\nbody"),
            );
            store
                .insert_message(&Message {
                    folder: "INBOX".into(),
                    uid,
                    thread_id: "t".into(),
                    flags: if uid == 6 {
                        "\\Seen".into()
                    } else {
                        String::new()
                    },
                    internaldate: 100,
                    headers: b"Subject: hi\r\n\r\n".to_vec(),
                    ..Default::default()
                })
                .unwrap();
        }
        ops = ops.with_folder("Archive", Some("\\Archive"));
        store
            .upsert_folder(&Folder {
                name: "Archive".into(),
                uidvalidity: 1,
                last_uid: 0,
                special_use: Some("Archive".into()),
                delimiter: None,
            })
            .unwrap();
        (ops, store, trash, dir)
    }

    fn run_on(
        ops: &mut RecordingOps,
        store: &Store,
        trash: &Trash,
        uids: &[u32],
        action: Action,
    ) -> Vec<UidResult> {
        ops.calls.clear();
        run(ops, store, trash, "INBOX", uids, &action, RULE_NAME, 500).unwrap()
    }

    fn counts(results: &[UidResult]) -> Vec<(u32, usize)> {
        results
            .iter()
            .map(|(uid, result)| (*uid, *result.as_ref().unwrap()))
            .collect()
    }

    #[test]
    fn archiving_many_messages_moves_them_in_one_command() {
        let (mut ops, store, trash, _dir) = three_in_inbox();
        let results = run_on(&mut ops, &store, &trash, &[7, 5, 6], Action::Archive);
        assert_eq!(ops.calls, ["select INBOX", "move INBOX 7,5,6 -> Archive"]);
        assert_eq!(counts(&results), [(7, 1), (5, 1), (6, 1)]);
        assert_eq!(store.message_count("INBOX").unwrap(), 0);
        assert_eq!(ops.mail["Archive"].len(), 3);
    }

    #[test]
    fn marking_many_read_stores_the_flag_once_and_skips_those_already_read() {
        let (mut ops, store, trash, _dir) = three_in_inbox();
        let results = run_on(&mut ops, &store, &trash, &[5, 6, 7], Action::MarkRead);
        assert_eq!(ops.calls, ["select INBOX", "add_flags INBOX 5,7 \\Seen"]);
        assert_eq!(counts(&results), [(5, 1), (6, 0), (7, 1)]);
        for uid in [5, 6, 7] {
            let flags = store.message("INBOX", uid).unwrap().unwrap().flags;
            assert!(has_flag(&flags, "\\Seen"), "{uid}: {flags}");
        }
    }

    #[test]
    fn deleting_many_backs_each_up_then_expunges_them_together() {
        let (mut ops, store, trash, _dir) = three_in_inbox();
        let results = run_on(&mut ops, &store, &trash, &[5, 6], Action::Delete);
        assert_eq!(
            ops.calls,
            [
                "select INBOX",
                "fetch_raw INBOX 5",
                "fetch_raw INBOX 6",
                "add_flags INBOX 5,6 \\Deleted",
                "expunge INBOX 5,6"
            ]
        );
        assert_eq!(counts(&results), [(5, 1), (6, 1)]);
        assert_eq!(trash.list().unwrap().len(), 2);
        assert_eq!(store.message_count("INBOX").unwrap(), 1);
    }

    #[test]
    fn archiving_without_an_archive_folder_says_so_for_each_message() {
        let (mut ops, store, trash, _dir) = setup();
        let results = run_on(&mut ops, &store, &trash, &[5], Action::Archive);
        assert!(
            matches!(
                &results[0].1,
                Err(ActionError::Apply(ApplyError::NoArchiveFolder))
            ),
            "{results:?}"
        );
        assert_eq!(ops.calls, ["select INBOX"]);
    }

    #[test]
    fn a_refused_batch_fails_every_message_in_it_and_keeps_their_rows() {
        let (mut ops, store, trash, _dir) = three_in_inbox();
        ops.mail.remove("Archive");
        let results = run_on(&mut ops, &store, &trash, &[5, 7], Action::Archive);
        assert!(
            results.iter().all(|(_, result)| result.is_err()),
            "{results:?}"
        );
        assert_eq!(store.message_count("INBOX").unwrap(), 3);
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
        let local = store
            .message("INBOX", 5)
            .unwrap()
            .into_iter()
            .map(|message| Hit {
                message,
                matched: Matched::Exact,
                snippet: None,
            })
            .collect();
        let hits = [
            ("INBOX".to_string(), 2),
            ("INBOX".to_string(), 5),
            ("INBOX".to_string(), 77),
        ];
        let merged = with_server_hits(&store, local, &hits, 10).unwrap();
        let uids: Vec<u32> = merged.iter().map(|hit| hit.message.uid).collect();
        assert_eq!(uids, [5, 2]);
        assert_eq!(
            with_server_hits(&store, Vec::new(), &hits, 1)
                .unwrap()
                .len(),
            1
        );
    }
}
