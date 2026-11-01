//! Direct actions on chosen messages, each sent to the server for all of them at once: mark, move, archive and delete, through the same `apply` as rules, which the
//! daemon's sync loop runs; plus the dry-run and rule preview helpers the CLI and the MCP server share.
use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::config::{AccountConfig, Identity};
use crate::mail_ops::{MailError, MailOps, SelectInfo, Structure};
use crate::message::{self, Attachment, HtmlBody, InlinePart, clean};
use crate::paths::Paths;
use crate::rules::apply::{ApplyError, apply_many, ensure_raw, move_target, plan_for};
use crate::rules::engine::{Context, Due, Mode, PlannedAction, folder_reads_index, folder_rules};
use crate::rules::{Action, CompiledRule, RulesError};
use crate::search::Term;
use crate::store::{Folder, Hit, Matched, Message, Store, StoreError};
use crate::sync::LostConnection;
use crate::trash::Trash;

mod undo;
pub use undo::{UndoOutcome, UndoReport, UndoRow, newest_direct_action, preview_undo, undo};

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
    #[error("no action {0} in the log; `postvak log` shows each action's number")]
    NoSuchAction(i64),
    #[error("action {0} was undone already")]
    AlreadyUndone(i64),
    #[error("attachment {index} of {folder}/{uid}: {reason}")]
    Attachment {
        folder: String,
        uid: u32,
        index: usize,
        reason: String,
    },
}

/// The outcome for one uid: how many actions ran, or why none did.
pub type UidResult = (u32, Result<usize, ActionError>);

/// How a folder the server just selected compares with the store's copy of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Selected {
    /// The stored uids name this folder's messages.
    Synced,
    /// The store only holds a placeholder (UIDVALIDITY 0), as a rule move into a folder never synced leaves; it has no
    /// uids to act on until a sync adopts it.
    Placeholder,
    /// The server's UIDVALIDITY is not the stored one, so the stored uids would name unrelated messages.
    Changed,
}

/// The one UIDVALIDITY check before acting on stored uids; `info` is what SELECTing `stored`'s folder answered.
pub(crate) fn check_selected(stored: &Folder, info: &SelectInfo) -> Selected {
    if stored.uidvalidity == 0 {
        Selected::Placeholder
    } else if stored.uidvalidity == info.uidvalidity {
        Selected::Synced
    } else {
        Selected::Changed
    }
}

/// Selects `folder`, refusing when the stored uids belong to another UIDVALIDITY and would hit unrelated messages.
pub(crate) fn select_synced(
    ops: &mut dyn MailOps,
    store: &Store,
    folder: &str,
) -> Result<(), ActionError> {
    let stored = store
        .folder(folder)?
        .ok_or_else(|| ActionError::UnknownFolder(folder.to_string()))?;
    match check_selected(&stored, &ops.select(folder)?) {
        Selected::Synced => Ok(()),
        Selected::Placeholder => Err(ActionError::UnknownFolder(folder.to_string())),
        Selected::Changed => Err(ActionError::FolderChanged(folder.to_string())),
    }
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
/// Above this size a previewed message is not fetched whole until something needs it whole; its attachments are
/// fetched one by one when asked.
pub const ON_DEMAND_MIN_SIZE: u32 = 5 * 1024 * 1024;

/// What the reader shows of a message before the whole of it is here: its HTML and what its attachments are. Its text
/// is in the store by then, as the indexer would have stored it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Preview {
    pub html: Option<HtmlBody>,
    pub attachments: Vec<Attachment>,
    /// The whole message follows; above `ON_DEMAND_MIN_SIZE` it waits until asked, and so do its attachments.
    pub whole: bool,
}

/// Downloads and indexes one message body, refusing when the folder changed on the server since the last sync. With
/// `preview`, a large message's text, HTML and `cid:` images are fetched first and handed to it, so a reader can show
/// them while the rest arrives; above `ON_DEMAND_MIN_SIZE` the rest is left on the server.
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
            Ok(Some(preview)) if !preview.whole => {
                show(preview);
                return Ok(());
            }
            Ok(Some(preview)) => show(preview),
            Ok(None) => {}
            Err(e) if e.is_lost_connection() => return Err(e),
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
            Some(&structure),
            crate::time::now(),
        )?;
    }
    Ok(Some(Preview {
        html,
        attachments: message::attachments_of_structure(&structure.attachments),
        whole: msg.size.is_none_or(|size| size <= ON_DEMAND_MIN_SIZE),
    }))
}

/// Attachment `index` (1-based, as the reader lists it) of a message: its name as the sender gave it, and its contents.
/// From the stored message when there is one, else fetched from the server on its own.
pub fn fetch_attachment(
    ops: &mut dyn MailOps,
    store: &Store,
    folder: &str,
    uid: u32,
    index: usize,
) -> Result<(Option<String>, Vec<u8>), ActionError> {
    let failed = |reason: String| ActionError::Attachment {
        folder: folder.to_string(),
        uid,
        index,
        reason,
    };
    if let Some(raw) = store.raw(folder, uid)? {
        let name = message::attachments(&raw)
            .into_iter()
            .find(|a| a.index == index)
            .and_then(|a| a.name);
        let (_, contents) = message::attachment(&raw, index).map_err(|e| failed(e.to_string()))?;
        return Ok((name, contents));
    }
    select_synced(ops, store, folder)?;
    if store.message(folder, uid)?.is_none() {
        return Err(ActionError::NotFound {
            folder: folder.to_string(),
            uid,
        });
    }
    let mut structure = ops
        .fetch_structures(&[uid])?
        .remove(&uid)
        .ok_or_else(|| failed("the server gave no structure".into()))?;
    let part = index
        .checked_sub(1)
        .filter(|&i| i < structure.attachments.len())
        .map(|i| structure.attachments.swap_remove(i))
        .ok_or_else(|| failed("no such attachment".into()))?;
    let bytes = ops
        .fetch_part(uid, &part.section)?
        .ok_or_else(|| failed("the server sent nothing for it".into()))?;
    Ok((part.name.clone(), message::part_contents(&part, &bytes)))
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
        return Ok(format!("would apply {}", clean(&action.label(), false)));
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
    /// When the action runs, if the rule's `after` has not come yet.
    pub waits: Option<Due>,
}

/// What `rules` would do to every message cached in `store`, evaluated as `rules apply-existing` would; with
/// `arrived_at`, as if every message arrived then.
pub fn planned(
    rules: &[CompiledRule],
    store: &Store,
    account: &AccountConfig,
    identity: &Identity,
    now: i64,
    arrived_at: Option<i64>,
) -> Result<Vec<Planned>, StoreError> {
    let ctx = Context {
        account: &account.name,
        identity,
        now,
        mode: Mode::ApplyExisting,
        notify_default: false,
        keep_in_folder: false,
        arrived_at,
    };
    let mut planned = Vec::new();
    for folder in store.folders()? {
        if folder_rules(rules, &account.name, &folder.name)
            .next()
            .is_none()
        {
            continue;
        }
        let messages = if folder_reads_index(rules, &account.name, &folder.name) {
            store.messages_in_folder(&folder.name)?
        } else {
            store.messages_without_body(&folder.name, 0)?
        };
        for message in messages {
            let plan = plan_for(rules, &message, &ctx, store)?;
            let now = plan.actions.into_iter().map(|planned| (planned, None));
            let waiting = plan
                .waiting
                .into_iter()
                .map(|(planned, due)| (planned, Some(due)));
            for (PlannedAction { rule, action }, waits) in now.chain(waiting) {
                planned.push(Planned {
                    rule,
                    message: message.clone(),
                    action,
                    waits,
                });
            }
        }
    }
    Ok(planned)
}

/// Whether `planned` would remove the message from the server for good now rather than move or flag it, or wait.
pub fn expunges(store: &Store, planned: &Planned) -> Result<bool, StoreError> {
    if planned.waits.is_some() {
        return Ok(false);
    }
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
    for p in planned(rules, store, account, identity, now, None)? {
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

/// The accounts of `accounts` that `rules` apply to: the ones `rules apply-existing` runs in.
pub fn covered_accounts<'a>(
    rules: &[CompiledRule],
    accounts: &[&'a AccountConfig],
) -> Vec<&'a AccountConfig> {
    accounts
        .iter()
        .copied()
        .filter(|account| rules.iter().any(|r| r.applies_to_account(&account.name)))
        .collect()
}

/// [`expunge_warning`] for each of `accounts`, read from its store: what every surface asks to confirm before `rules
/// apply-existing` deletes mail. Empty when nothing would be expunged.
pub fn expunge_warnings(
    paths: &Paths,
    rules: &[CompiledRule],
    accounts: &[&AccountConfig],
    now: i64,
) -> Result<Vec<String>, RulesError> {
    let mut warnings = Vec::new();
    for account in accounts {
        let store = Store::open_account(paths, &account.name)?;
        warnings.extend(expunge_warning(
            rules,
            &store,
            account,
            &account.identity()?,
            now,
        )?);
    }
    Ok(warnings)
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
            "would apply archive"
        );
        assert_eq!(
            planned_effect(&store, &msg, &Action::MarkUnread).unwrap(),
            "would apply read:false"
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
                waits: None,
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
                "[[rules]]\nname = \"r\"\nmatch.subject = {{ contains = \"hi\" }}\nactions = {{ {actions} }}\n"
            );
            let rules = crate::rules::compile(&crate::rules::parse(&toml).unwrap()).unwrap();
            expunge_warning(&rules, &store, &account, &identity, 0).unwrap()
        };
        assert_eq!(
            warning("delete = true").as_deref(),
            Some(
                "work: would permanently delete 1 message from the server; .eml backups kept locally for 30 days"
            )
        );
        assert_eq!(warning("flagged = true"), None);
        assert_eq!(
            warning("delete = true, after = \"read\""),
            None,
            "a waiting delete expunges nothing yet"
        );
    }

    #[test]
    fn planned_rows_say_what_waits_and_until_when() {
        let (_ops, store, _trash, _dir) = setup();
        let password = crate::config::PasswordSource::Keyring { keyring: true };
        let account = AccountConfig::new("work", "h", "me@example.com", password);
        let identity = account.identity().unwrap();
        let toml = "[[rules]]\nname = \"r\"\nmatch.subject = { contains = \"hi\" }\nactions = { flagged = true, after = \"read\" }\n";
        let rules = crate::rules::compile(&crate::rules::parse(toml).unwrap()).unwrap();
        let planned = planned(&rules, &store, &account, &identity, 0, None).unwrap();
        assert_eq!(planned.len(), 1);
        assert_eq!(planned[0].action, Action::Flag);
        let row = crate::output::planned_row("work", &planned[0]);
        assert_eq!(row["waits"], "once read");
    }

    #[test]
    fn planning_reads_bodies_only_where_a_rule_tests_them() {
        let (_ops, store, _trash, _dir) = setup();
        store
            .upsert_folder(&Folder {
                name: "Archive".into(),
                uidvalidity: 1,
                last_uid: 9,
                special_use: None,
                delimiter: None,
            })
            .unwrap();
        for folder in ["INBOX", "Archive"] {
            store
                .insert_message(&Message {
                    folder: folder.into(),
                    uid: 9,
                    thread_id: "t9".into(),
                    subject: Some("hi".into()),
                    headers: b"Subject: hi\r\n\r\n".to_vec(),
                    body_text: Some("invoice".into()),
                    ..Default::default()
                })
                .unwrap();
        }
        let password = crate::config::PasswordSource::Keyring { keyring: true };
        let account = AccountConfig::new("work", "h", "me@example.com", password);
        let identity = account.identity().unwrap();
        let plan = |toml: &str| {
            let rules = crate::rules::compile(&crate::rules::parse(toml).unwrap()).unwrap();
            planned(&rules, &store, &account, &identity, 0, None).unwrap()
        };

        let by_subject = plan(
            "[[rules]]\nname = \"r\"\nmatch.subject = { contains = \"hi\" }\nactions = { flagged = true }\n",
        );
        assert!(by_subject.iter().all(|p| p.message.folder == "INBOX"));
        assert!(by_subject.iter().all(|p| p.message.body_text.is_none()));

        let by_body = plan(
            "[[rules]]\nname = \"r\"\nfolder = \"Archive\"\nmatch.body = { contains = \"invoice\" }\nactions = { flagged = true }\n",
        );
        assert_eq!(by_body.len(), 1);
        assert_eq!(by_body[0].message.folder, "Archive");
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
    fn a_placeholder_folder_is_not_synced_yet_not_changed() {
        let (mut ops, store, trash, _dir) = setup();
        ops = ops.with_folder("Receipts", None);
        // What a rule move into a folder never synced leaves behind.
        store
            .upsert_folder(&Folder {
                name: "Receipts".into(),
                uidvalidity: 0,
                last_uid: 0,
                special_use: None,
                delimiter: None,
            })
            .unwrap();
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
        assert!(matches!(err, ActionError::UnknownFolder(_)), "{err}");
        assert!(err.to_string().contains("not been synced yet"), "{err}");
    }

    #[test]
    fn check_selected_tells_synced_placeholder_and_changed_apart() {
        let info = |uidvalidity| SelectInfo {
            uidvalidity,
            exists: 0,
            highest_modseq: None,
            uidnext: None,
        };
        let stored = |uidvalidity| Folder {
            name: "X".into(),
            uidvalidity,
            last_uid: 0,
            special_use: None,
            delimiter: None,
        };
        assert_eq!(check_selected(&stored(7), &info(7)), Selected::Synced);
        assert_eq!(check_selected(&stored(7), &info(8)), Selected::Changed);
        assert_eq!(check_selected(&stored(0), &info(8)), Selected::Placeholder);
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
