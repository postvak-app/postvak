use std::io;

use crate::folder_name;
use crate::mail_ops::{MailError, MailOps, delete_message, delete_messages};
use crate::message::{self, body_text};
use crate::rules::engine::{Context, Plan, PlannedAction, evaluate, flag_change};
use crate::rules::{Action, CompiledRule};
use crate::store::{
    Folder, LogEntry, Message, Store, StoreError, has_flag, structure_columns, with_flag,
};
use crate::trash::Trash;

#[derive(Debug, thiserror::Error)]
pub enum ApplyError {
    #[error(transparent)]
    Mail(#[from] MailError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("could not write trash file: {0}")]
    Trash(#[from] io::Error),
    #[error("the server has no Archive folder")]
    NoArchiveFolder,
    #[error("the server has no Junk folder")]
    NoJunkFolder,
    #[error("tag {0}")]
    InvalidTag(String),
    #[error("message {folder}/{uid} could not be fetched for backup")]
    RawUnavailable { folder: String, uid: u32 },
    #[error("{0} cannot hold messages")]
    NoSelect(String),
}

pub fn ensure_raw(
    msg: &Message,
    ops: &mut dyn MailOps,
    store: &Store,
) -> Result<Vec<u8>, ApplyError> {
    if let Some(raw) = store.raw(&msg.folder, msg.uid)? {
        return Ok(raw);
    }
    download(msg, ops, store).map(|(raw, _)| raw)
}

/// The message's body text, downloading the message once when the store lacks it.
pub fn ensure_body_text(
    msg: &Message,
    ops: &mut dyn MailOps,
    store: &Store,
) -> Result<String, ApplyError> {
    if let Some(raw) = store.raw(&msg.folder, msg.uid)? {
        return Ok(body_text(&raw));
    }
    download(msg, ops, store).map(|(_, text)| text)
}

/// Reads the message's attachment names and part types into `msg`, from the stored message when it is here and from
/// its BODYSTRUCTURE alone otherwise. A structure the server does not give leaves them unknown.
pub fn ensure_structure(
    msg: &mut Message,
    ops: &mut dyn MailOps,
    store: &Store,
) -> Result<(), ApplyError> {
    let structure = match store
        .raw(&msg.folder, msg.uid)?
        .and_then(|raw| message::structure(&raw))
    {
        Some(structure) => Some(structure),
        None => ops.fetch_structures(&[msg.uid])?.remove(&msg.uid),
    };
    let Some(structure) = structure else {
        return Ok(());
    };
    store.set_structure(&msg.folder, msg.uid, &structure)?;
    (msg.attachment_names, msg.part_types) = structure_columns(&structure);
    Ok(())
}

/// Fetches the whole message and stores it with its body text.
fn download(
    msg: &Message,
    ops: &mut dyn MailOps,
    store: &Store,
) -> Result<(Vec<u8>, String), ApplyError> {
    let raw = ops
        .fetch_raw(msg.uid)?
        .ok_or_else(|| ApplyError::RawUnavailable {
            folder: msg.folder.clone(),
            uid: msg.uid,
        })?;
    let text = body_text(&raw);
    store.set_raw(&msg.folder, msg.uid, &raw, &text)?;
    Ok((raw, text))
}

/// What the rules do to `msg`, as `evaluate` says, except that mail a rule moved out of its folder before is not moved
/// again. So a message that comes back, dragged by hand or moved by another client's rule, stays, and rules moving it
/// between two folders, on one machine or two, stop after one round.
pub fn plan_for(
    rules: &[CompiledRule],
    msg: &Message,
    ctx: &Context,
    store: &Store,
) -> Result<Plan, StoreError> {
    let plan = evaluate(rules, msg, ctx);
    let waiting = plan.waiting.iter().map(|(planned, _)| planned);
    let moves = plan.actions.iter().chain(waiting).any(|planned| {
        matches!(
            planned.action,
            Action::Move(_) | Action::Archive | Action::Junk
        )
    });
    if !moves || !store.rule_moved_out(msg)? {
        return Ok(plan);
    }
    let kept = Context {
        keep_in_folder: true,
        ..*ctx
    };
    Ok(evaluate(rules, msg, &kept))
}

/// A delete or user delete wins over everything else in the plan. Otherwise flag actions run first, then only the first
/// move or archive. Returns how many actions ran; a flag already in the wanted state is skipped without a log row.
pub fn apply(
    plan: &Plan,
    msg: &Message,
    ops: &mut dyn MailOps,
    store: &Store,
    trash: &Trash,
    now: i64,
) -> Result<usize, ApplyError> {
    // Direct actions reach here from the daemon socket without passing the rules compiler, and a tag goes into STORE verbatim.
    for planned in &plan.actions {
        if let Action::Tag(keyword) = &planned.action {
            crate::rules::check_keyword(keyword).map_err(ApplyError::InvalidTag)?;
        }
    }
    if let Some(planned) = plan
        .actions
        .iter()
        .find(|planned| matches!(planned.action, Action::Delete | Action::Trash))
    {
        let trash_folder = match planned.action {
            Action::Trash => move_target(store, &msg.folder, &Action::Trash)?,
            _ => None,
        };
        match trash_folder {
            Some(folder) => {
                store.log_action(&log_entry(planned, msg, Some(&folder), now))?;
                move_to(msg, &folder, ops, store)?;
            }
            None => delete(planned, msg, ops, store, trash, now)?,
        }
        return Ok(1);
    }
    if let Some(planned) = plan
        .actions
        .iter()
        .find(|planned| matches!(planned.action, Action::Junk | Action::NotJunk))
    {
        junk(planned, msg, ops, store, now)?;
        return Ok(1);
    }
    let first_move = first_move(plan, msg, store)?;
    let mut current = msg.clone();
    let mut executed = 0;
    for planned in &plan.actions {
        let Some((flag, wanted)) = flag_change(&planned.action) else {
            continue;
        };
        if has_flag(&current.flags, flag) == wanted {
            continue;
        }
        store.log_action(&log_entry(planned, msg, None, now))?;
        set_flag(&mut current, flag, wanted, ops, store)?;
        executed += 1;
    }
    if let Some((planned, target)) = first_move {
        store.log_action(&log_entry(planned, msg, Some(&target), now))?;
        move_to(&current, &target, ops, store)?;
        // After the move: a failed one must not keep the rule from trying again.
        if !crate::rules::is_actor(&planned.rule) {
            store.record_rule_move(msg)?;
        }
        executed += 1;
    }
    Ok(executed)
}

/// The actions of `plan` that `apply` would carry out on `msg`, in plan order, without doing anything: a delete or junk
/// report alone, or else each flag change not already in effect and the first move or archive unless it is into the
/// message's own folder. Fails as `apply` would before running anything, such as an archive with no Archive folder.
pub fn would_run<'a>(
    plan: &'a Plan,
    msg: &Message,
    store: &Store,
) -> Result<Vec<&'a PlannedAction>, ApplyError> {
    let alone = |wins: fn(&Action) -> bool| plan.actions.iter().find(|p| wins(&p.action));
    if let Some(planned) = alone(|a| matches!(a, Action::Delete | Action::Trash))
        .or_else(|| alone(|a| matches!(a, Action::Junk | Action::NotJunk)))
    {
        return Ok(vec![planned]);
    }
    let first_move = first_move(plan, msg, store)?.map(|(planned, _)| planned);
    let mut flags = msg.flags.clone();
    let mut running = Vec::new();
    for planned in &plan.actions {
        match flag_change(&planned.action) {
            Some((flag, wanted)) if has_flag(&flags, flag) != wanted => {
                flags = with_flag(&flags, flag, wanted);
                running.push(planned);
            }
            Some(_) => {}
            None if first_move.is_some_and(|first| std::ptr::eq(first, planned)) => {
                running.push(planned);
            }
            None => {}
        }
    }
    Ok(running)
}

/// One message's outcome from `apply_many`: how many actions ran, or why none did.
pub type Applied = (u32, Result<usize, ApplyError>);

/// What `apply` does with a plan of one action, for many messages of one folder at once: a flag change or a move goes to
/// the server as one command per batch of uids rather than one per message. A delete, which saves each message before
/// expunging it, fetches each one but expunges them together. Junk and not-junk, which also swap keywords, and any
/// action whose target cannot be resolved, run per message through `apply`, which says why.
pub fn apply_many(
    planned: &PlannedAction,
    msgs: &[Message],
    ops: &mut dyn MailOps,
    store: &Store,
    trash: &Trash,
    now: i64,
) -> Vec<Applied> {
    let plan = Plan {
        actions: vec![planned.clone()],
        ..Plan::default()
    };
    let per_message = |ops: &mut dyn MailOps| -> Vec<Applied> {
        msgs.iter()
            .map(|msg| (msg.uid, apply(&plan, msg, ops, store, trash, now)))
            .collect()
    };
    let Some(folder) = msgs.first().map(|msg| msg.folder.clone()) else {
        return Vec::new();
    };
    match &planned.action {
        Action::Tag(keyword) if crate::rules::check_keyword(keyword).is_err() => per_message(ops),
        Action::Flag | Action::MarkRead | Action::MarkUnread | Action::Unflag | Action::Tag(_) => {
            set_flag_many(planned, msgs, ops, store, now)
        }
        Action::Move(_) | Action::Archive => {
            let target = match &planned.action {
                Action::Move(target) => Ok(crate::rules::inbox_spelled(target).to_string()),
                _ => archive_folder(store),
            };
            match target {
                // A move into the messages' own folder does nothing, as in `apply`.
                Ok(target) if folder_name::same(&target, &folder) => {
                    msgs.iter().map(|msg| (msg.uid, Ok(0))).collect()
                }
                Ok(target) => move_many(planned, msgs, &target, ops, store, now),
                Err(_) => per_message(ops),
            }
        }
        // Without a Trash folder, or inside it, trashing deletes, as in `apply`.
        Action::Trash => match move_target(store, &folder, &Action::Trash) {
            Ok(Some(target)) => move_many(planned, msgs, &target, ops, store, now),
            Ok(None) => delete_many(planned, msgs, ops, store, trash, now),
            Err(_) => per_message(ops),
        },
        Action::Delete => delete_many(planned, msgs, ops, store, trash, now),
        _ => per_message(ops),
    }
}

/// Logs `planned` for each message; one whose row cannot be written fails alone and is left out.
fn log_each<'a>(
    planned: &PlannedAction,
    msgs: impl IntoIterator<Item = &'a Message>,
    target: Option<&str>,
    store: &Store,
    now: i64,
    results: &mut Vec<Applied>,
) -> Vec<&'a Message> {
    let mut logged = Vec::new();
    for msg in msgs {
        match store.log_action(&log_entry(planned, msg, target, now)) {
            Ok(()) => logged.push(msg),
            Err(e) => results.push((msg.uid, Err(e.into()))),
        }
    }
    logged
}

fn uids_of(msgs: &[&Message]) -> Vec<u32> {
    msgs.iter().map(|msg| msg.uid).collect()
}

/// A failed batch command failed for every message in it.
fn fail_all(msgs: &[&Message], error: &MailError, results: &mut Vec<Applied>) {
    results.extend(
        msgs.iter()
            .map(|msg| (msg.uid, Err(ApplyError::Mail(error.clone())))),
    );
}

fn set_flag_many(
    planned: &PlannedAction,
    msgs: &[Message],
    ops: &mut dyn MailOps,
    store: &Store,
    now: i64,
) -> Vec<Applied> {
    let (flag, wanted) = match &planned.action {
        Action::Flag => ("\\Flagged", true),
        Action::MarkRead => ("\\Seen", true),
        Action::MarkUnread => ("\\Seen", false),
        Action::Unflag => ("\\Flagged", false),
        Action::Tag(keyword) => (keyword.as_str(), true),
        _ => return Vec::new(),
    };
    let mut results = Vec::new();
    // A flag already in the wanted state is skipped without a log row, as in `apply`.
    let (changing, unchanged): (Vec<&Message>, Vec<&Message>) = msgs
        .iter()
        .partition(|msg| has_flag(&msg.flags, flag) != wanted);
    results.extend(unchanged.iter().map(|msg| (msg.uid, Ok(0))));
    let changing = log_each(planned, changing, None, store, now, &mut results);
    if changing.is_empty() {
        return results;
    }
    let uids = uids_of(&changing);
    let sent = if wanted {
        ops.add_flags_to(&uids, &[flag])
    } else {
        ops.remove_flags_from(&uids, &[flag])
    };
    if let Err(e) = sent {
        fail_all(&changing, &e, &mut results);
        return results;
    }
    for msg in changing {
        let flags = with_flag(&msg.flags, flag, wanted);
        let stored = store.update_flags(&msg.folder, msg.uid, &flags);
        results.push((msg.uid, stored.map(|()| 1).map_err(ApplyError::from)));
    }
    results
}

fn move_many(
    planned: &PlannedAction,
    msgs: &[Message],
    target: &str,
    ops: &mut dyn MailOps,
    store: &Store,
    now: i64,
) -> Vec<Applied> {
    let mut results = Vec::new();
    let moving = log_each(planned, msgs, Some(target), store, now, &mut results);
    if moving.is_empty() {
        return results;
    }
    if store
        .is_noselect(&folder_name::nfc(target))
        .unwrap_or(false)
    {
        for msg in moving {
            results.push((msg.uid, Err(ApplyError::NoSelect(target.to_string()))));
        }
        return results;
    }
    let (target, known) = match folder_name::known(store, target) {
        Ok(folder) => spelled(target, folder),
        Err(e) => {
            log::warn!("{target}: could not read the folder ({e}); creating it if missing");
            spelled(target, None)
        }
    };
    let (target, placeholder, refused) = if known {
        (target, false, None)
    } else {
        missing_target(&target, ops, store)
    };
    if let Err(e) = ops.move_messages(&uids_of(&moving), &target) {
        fail_all(&moving, &with_create_error(e, refused), &mut results);
        return results;
    }
    if placeholder && let Err(e) = store.upsert_folder(&new_folder(&target)) {
        log::warn!("{target}: could not record the folder; the next sync adds it: {e}");
    }
    for msg in moving {
        let mut done = store.remove_message(&msg.folder, msg.uid);
        // After the move: a failed one must not keep the rule from trying again. A trash is no rule move.
        if done.is_ok()
            && !crate::rules::is_actor(&planned.rule)
            && matches!(planned.action, Action::Move(_) | Action::Archive)
        {
            done = store.record_rule_move(msg);
        }
        results.push((msg.uid, done.map(|()| 1).map_err(ApplyError::from)));
    }
    results
}

fn delete_many(
    planned: &PlannedAction,
    msgs: &[Message],
    ops: &mut dyn MailOps,
    store: &Store,
    trash: &Trash,
    now: i64,
) -> Vec<Applied> {
    let mut results = Vec::new();
    let mut saved = Vec::new();
    for msg in msgs {
        match back_up(planned, msg, ops, store, trash, now) {
            Ok(()) => saved.push(msg),
            Err(e) => results.push((msg.uid, Err(e))),
        }
    }
    if saved.is_empty() {
        return results;
    }
    let uids = uids_of(&saved);
    if let Err(e) = delete_messages(ops, &uids) {
        fail_all(&saved, &e, &mut results);
        return results;
    }
    for msg in saved {
        let removed = store.remove_message(&msg.folder, msg.uid);
        results.push((msg.uid, removed.map(|()| 1).map_err(ApplyError::from)));
    }
    results
}

/// The first move or archive in the plan with its resolved target. Resolved before anything runs so a failure leaves no
/// partial effects; a move into the message's own folder is dropped because the next sync would re-add and re-move it.
fn first_move<'a>(
    plan: &'a Plan,
    msg: &Message,
    store: &Store,
) -> Result<Option<(&'a PlannedAction, String)>, ApplyError> {
    let Some(planned) = plan
        .actions
        .iter()
        .find(|planned| matches!(planned.action, Action::Move(_) | Action::Archive))
    else {
        return Ok(None);
    };
    let target = match &planned.action {
        Action::Move(target) => crate::rules::inbox_spelled(target).to_string(),
        _ => archive_folder(store)?,
    };
    Ok((!folder_name::same(&target, &msg.folder)).then_some((planned, target)))
}

/// Swaps the $Junk and $NotJunk keywords, then moves to the Junk folder or back to INBOX. A server that refuses
/// keywords only loses the filter training, so the move still runs.
fn junk(
    planned: &PlannedAction,
    msg: &Message,
    ops: &mut dyn MailOps,
    store: &Store,
    now: i64,
) -> Result<(), ApplyError> {
    let (target, keyword, opposite) = match planned.action {
        Action::Junk => (junk_folder(store)?, "$Junk", "$NotJunk"),
        _ => ("INBOX".to_string(), "$NotJunk", "$Junk"),
    };
    store.log_action(&log_entry(planned, msg, Some(&target), now))?;
    let mut current = msg.clone();
    for (flag, wanted) in [(opposite, false), (keyword, true)] {
        if has_flag(&current.flags, flag) != wanted
            && let Err(e) = set_flag(&mut current, flag, wanted, ops, store)
        {
            log::warn!("{}/{}: could not set {flag}: {e}", msg.folder, msg.uid);
        }
    }
    if target != msg.folder {
        move_to(&current, &target, ops, store)?;
        if planned.action == Action::Junk && !crate::rules::is_actor(&planned.rule) {
            store.record_rule_move(msg)?;
        }
    }
    Ok(())
}

/// `target` is the folder a move takes the message to.
fn log_entry(planned: &PlannedAction, msg: &Message, target: Option<&str>, now: i64) -> LogEntry {
    LogEntry {
        at: now,
        rule_name: planned.rule.clone(),
        folder: msg.folder.clone(),
        uid: msg.uid,
        message_id: msg.message_id.clone(),
        subject: msg.subject.clone(),
        action: planned.action.label(),
        internaldate: Some(msg.internaldate),
        target: target.map(str::to_string),
        ..LogEntry::default()
    }
}

fn delete(
    planned: &PlannedAction,
    msg: &Message,
    ops: &mut dyn MailOps,
    store: &Store,
    trash: &Trash,
    now: i64,
) -> Result<(), ApplyError> {
    back_up(planned, msg, ops, store, trash, now)?;
    delete_message(ops, msg.uid)?;
    store.remove_message(&msg.folder, msg.uid)?;
    Ok(())
}

/// Saves the message as an `.eml` in the trash directory and logs the delete with that file, before anything is expunged.
fn back_up(
    planned: &PlannedAction,
    msg: &Message,
    ops: &mut dyn MailOps,
    store: &Store,
    trash: &Trash,
    now: i64,
) -> Result<(), ApplyError> {
    let raw = ensure_raw(msg, ops, store)?;
    let path = trash.save(&msg.folder, msg.uid, &raw, now)?;
    let mut entry = log_entry(planned, msg, None, now);
    // A user delete that falls back to expunging is logged as what happened, not as a move to Trash.
    entry.action = Action::Delete.label();
    entry.trash_file = Some(path.to_string_lossy().into_owned());
    store.log_action(&entry)?;
    Ok(())
}

fn set_flag(
    current: &mut Message,
    flag: &str,
    wanted: bool,
    ops: &mut dyn MailOps,
    store: &Store,
) -> Result<(), ApplyError> {
    if wanted {
        ops.add_flags(current.uid, &[flag])?;
    } else {
        ops.remove_flags(current.uid, &[flag])?;
    }
    current.flags = with_flag(&current.flags, flag, wanted);
    store.update_flags(&current.folder, current.uid, &current.flags)?;
    Ok(())
}

pub(crate) fn special_folder(store: &Store, role: &str) -> Result<Option<String>, StoreError> {
    Ok(store
        .folders()?
        .into_iter()
        .find(|folder| folder.special_use.as_deref() == Some(role))
        .map(|folder| folder.name))
}

/// The folder `action` moves a message in `folder` to; `None` when it stays or is expunged.
pub fn move_target(
    store: &Store,
    folder: &str,
    action: &Action,
) -> Result<Option<String>, StoreError> {
    let target = match action {
        Action::Archive => special_folder(store, "Archive")?,
        Action::Move(target) => Some(crate::rules::inbox_spelled(target).to_string()),
        Action::Trash => special_folder(store, "Trash")?,
        Action::Junk => special_folder(store, "Junk")?,
        Action::NotJunk => Some("INBOX".to_string()),
        _ => None,
    };
    Ok(target.filter(|target| !folder_name::same(target, folder)))
}

fn archive_folder(store: &Store) -> Result<String, ApplyError> {
    special_folder(store, "Archive")?.ok_or(ApplyError::NoArchiveFolder)
}

fn junk_folder(store: &Store) -> Result<String, ApplyError> {
    special_folder(store, "Junk")?.ok_or(ApplyError::NoJunkFolder)
}

/// Where a move to `target` goes, and whether the store knows that folder: its own spelling of a folder it knows,
/// apart from Unicode normalization, else `target` in NFC, the form a new name should have (RFC 9051 §5.1).
fn spelled(target: &str, known: Option<Folder>) -> (String, bool) {
    match known {
        Some(folder) => (folder.name, true),
        None => (folder_name::nfc(target).into_owned(), false),
    }
}

/// Where a move to `target`, a folder the store does not know, goes; whether to record it until its first sync; and
/// the server's refusal to create it, for the move's error to carry if the move fails too. It is created and
/// subscribed, so clients that list only subscribed folders show the mail moved there. A refused create may mean the
/// folder is there already: before our first sync of it, or under another spelling the store knows on a server that
/// ignores case (Gmail, Exchange), which the move then uses rather than adding a second row for the same folder. A
/// server that respects case creates `target` beside it, as the rule asks.
fn missing_target(
    target: &str,
    ops: &mut dyn MailOps,
    store: &Store,
) -> (String, bool, Option<MailError>) {
    if let Err(e) = ops.create_folder(target) {
        if let Some(existing) = other_case(store, target) {
            log::debug!(
                "{target}: create failed ({e}), moving to {existing}, the server's spelling"
            );
            return (existing, false, None);
        }
        log::debug!("{target}: create failed ({e}), trying the move anyway");
        return (target.to_string(), true, Some(e));
    }
    // The move does not need it, so a refusal only costs the folder its place in those clients.
    if let Err(e) = ops.subscribe_folder(target) {
        log::warn!("{target}: created, but could not subscribe to it: {e}");
    }
    (target.to_string(), true, None)
}

/// The one folder the store knows whose name is `target` in another case, apart from Unicode normalization too; none
/// when two are, since which one a server took `target` for is then unknown.
fn other_case(store: &Store, target: &str) -> Option<String> {
    let lower = folder_name::nfc(target).to_lowercase();
    let folders = store
        .folders()
        .inspect_err(|e| log::debug!("{target}: could not read the folders: {e}"))
        .ok()?;
    let mut matches = folders.into_iter().filter(|f| {
        !folder_name::same(&f.name, target) && folder_name::nfc(&f.name).to_lowercase() == lower
    });
    let found = matches.next()?;
    matches.next().is_none().then_some(found.name)
}

/// A failed move's error, with why creating its folder failed before it: a bad name, a wrong namespace or a missing
/// permission explains a move's "no such mailbox".
fn with_create_error(error: MailError, refused: Option<MailError>) -> MailError {
    match refused {
        Some(refused) => error
            .map_reason(|reason| format!("{reason}; creating the folder failed first: {refused}")),
        None => error,
    }
}

/// A folder a move created, until its first sync fills in the rest.
fn new_folder(name: &str) -> Folder {
    Folder {
        name: name.to_string(),
        uidvalidity: 0,
        last_uid: 0,
        special_use: None,
        delimiter: None,
    }
}

/// The new uid is not known, so the local row goes and the next sync of the target folder adds it.
fn move_to(
    current: &Message,
    target: &str,
    ops: &mut dyn MailOps,
    store: &Store,
) -> Result<(), ApplyError> {
    if store.is_noselect(&folder_name::nfc(target))? {
        return Err(ApplyError::NoSelect(target.to_string()));
    }
    let (target, known) = spelled(target, folder_name::known(store, target)?);
    let (target, placeholder, refused) = if known {
        (target, false, None)
    } else {
        missing_target(&target, ops, store)
    };
    ops.move_message(current.uid, &target)
        .map_err(|e| with_create_error(e, refused))?;
    if placeholder {
        store.upsert_folder(&new_folder(&target))?;
    }
    store.remove_message(&current.folder, current.uid)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::RULE_NAME;
    use crate::mail_ops::RecordingOps;

    fn setup() -> (RecordingOps, Store, Trash, tempfile::TempDir, Message) {
        let dir = tempfile::tempdir().unwrap();
        let mut ops = RecordingOps::default()
            .with_folder("INBOX", None)
            .with_folder("Archive", Some("Archive"));
        ops.add_mail(
            "INBOX",
            5,
            100,
            "Subject: hi\r\n\r\n",
            Some("Subject: hi\r\n\r\nbody text"),
        );
        ops.select("INBOX").unwrap();
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
            .upsert_folder(&Folder {
                name: "Archive".into(),
                uidvalidity: 1,
                last_uid: 0,
                special_use: Some("Archive".into()),
                delimiter: None,
            })
            .unwrap();
        let msg = Message {
            folder: "INBOX".into(),
            uid: 5,
            message_id: Some("m5@x".into()),
            from_addr: Some("a@x".into()),
            thread_id: "m5@x".into(),
            subject: Some("hi".into()),
            internaldate: 100,
            headers: b"Subject: hi\r\n\r\n".to_vec(),
            ..Default::default()
        };
        store.insert_message(&msg).unwrap();
        let trash = Trash::new(dir.path().join("trash"));
        (ops, store, trash, dir, msg)
    }

    fn plan(actions: Vec<Action>) -> Plan {
        Plan {
            actions: actions
                .into_iter()
                .map(|action| PlannedAction {
                    rule: "r".into(),
                    action,
                })
                .collect(),
            ..Plan::default()
        }
    }

    #[test]
    fn a_tag_differing_only_in_case_is_already_set() {
        let (mut ops, store, trash, _dir, mut msg) = setup();
        msg.flags = "Work".into();
        store.update_flags("INBOX", 5, "Work").unwrap();
        let tag = plan(vec![Action::Tag("work".into())]);
        let n = apply(&tag, &msg, &mut ops, &store, &trash, 500).unwrap();
        assert_eq!(n, 0);
        assert!(
            !ops.calls.iter().any(|c| c.contains("flags")),
            "{:?}",
            ops.calls
        );
        assert!(store.log(10).unwrap().is_empty());
        assert_eq!(store.message("INBOX", 5).unwrap().unwrap().flags, "Work");
        let applied = apply_many(&tag.actions[0], &[msg], &mut ops, &store, &trash, 500);
        assert!(matches!(applied[..], [(5, Ok(0))]));
        assert!(store.log(10).unwrap().is_empty());
    }

    #[test]
    fn removing_a_flag_ignores_case() {
        assert_eq!(with_flag("\\Seen Work", "work", false), "\\Seen");
    }

    #[test]
    fn delete_backs_up_before_expunge_and_logs() {
        let (mut ops, store, trash, _dir, msg) = setup();
        apply(
            &plan(vec![Action::Delete]),
            &msg,
            &mut ops,
            &store,
            &trash,
            500,
        )
        .unwrap();
        let calls = ops.calls.clone();
        let fetch = calls
            .iter()
            .position(|c| c.starts_with("fetch_raw"))
            .unwrap();
        let expunge = calls.iter().position(|c| c.starts_with("expunge")).unwrap();
        assert!(fetch < expunge);
        let saved = trash.list().unwrap();
        assert_eq!(saved.len(), 1);
        assert_eq!(
            std::fs::read(&saved[0].path).unwrap(),
            b"Subject: hi\r\n\r\nbody text"
        );
        assert!(ops.mail["INBOX"].is_empty());
        assert_eq!(store.message("INBOX", 5).unwrap(), None);
        let log = store.log(10).unwrap();
        assert_eq!(log[0].action, "delete");
        assert!(
            log[0]
                .trash_file
                .as_deref()
                .unwrap()
                .ends_with("-INBOX-5.eml")
        );
    }

    #[test]
    fn delete_is_refused_when_raw_cannot_be_fetched() {
        let (mut ops, store, trash, _dir, msg) = setup();
        ops.raw.clear();
        let err = apply(
            &plan(vec![Action::Delete]),
            &msg,
            &mut ops,
            &store,
            &trash,
            500,
        )
        .unwrap_err();
        assert!(matches!(err, ApplyError::RawUnavailable { .. }));
        assert!(!ops.calls.iter().any(|c| c.starts_with("expunge")));
        assert_eq!(ops.mail["INBOX"].len(), 1);
    }

    #[test]
    fn a_refused_expunge_clears_deleted_again() {
        let (mut ops, store, trash, _dir, msg) = setup();
        ops.fail_next_expunge = Some(MailError::Protocol("refused".into()));
        let err = apply(
            &plan(vec![Action::Delete]),
            &msg,
            &mut ops,
            &store,
            &trash,
            500,
        )
        .unwrap_err();
        assert!(err.to_string().contains("refused"), "{err}");
        assert!(ops.mail["INBOX"][0].flags.is_empty(), "{:?}", ops.calls);
        assert!(store.message("INBOX", 5).unwrap().is_some());
    }

    #[test]
    fn delete_wins_over_other_actions() {
        let (mut ops, store, trash, _dir, msg) = setup();
        apply(
            &plan(vec![
                Action::Move("Archive".into()),
                Action::MarkRead,
                Action::Delete,
            ]),
            &msg,
            &mut ops,
            &store,
            &trash,
            500,
        )
        .unwrap();
        assert_eq!(trash.list().unwrap().len(), 1);
        assert!(ops.mail["INBOX"].is_empty());
        assert!(ops.mail["Archive"].is_empty());
        assert!(!ops.calls.iter().any(|c| c.starts_with("move")));
        assert!(
            !ops.calls
                .iter()
                .any(|c| c.starts_with("add_flags") && c.contains("\\Seen"))
        );
        assert_eq!(store.log(10).unwrap().len(), 1);
    }

    #[test]
    fn only_first_move_runs_and_flags_precede_it() {
        let (mut ops, store, trash, _dir, msg) = setup();
        apply(
            &plan(vec![
                Action::Move("Archive".into()),
                Action::MarkRead,
                Action::Move("Other".into()),
            ]),
            &msg,
            &mut ops,
            &store,
            &trash,
            500,
        )
        .unwrap();
        let flag = ops
            .calls
            .iter()
            .position(|c| c == "add_flags INBOX 5 \\Seen")
            .unwrap();
        let moved = ops
            .calls
            .iter()
            .position(|c| c == "move INBOX 5 -> Archive")
            .unwrap();
        assert!(flag < moved);
        assert!(!ops.calls.iter().any(|c| c.contains("Other")));
        let actions: Vec<String> = store
            .log(10)
            .unwrap()
            .into_iter()
            .rev()
            .map(|entry| entry.action)
            .collect();
        assert_eq!(actions, ["read", "move:Archive"]);
    }

    #[test]
    fn move_creates_missing_folder_and_drops_row_for_resync() {
        let (mut ops, store, trash, _dir, msg) = setup();
        apply(
            &plan(vec![Action::Move("Lists/GitHub".into()), Action::MarkRead]),
            &msg,
            &mut ops,
            &store,
            &trash,
            500,
        )
        .unwrap();
        let created = ops
            .calls
            .iter()
            .position(|c| c == "create_folder Lists/GitHub");
        let subscribed = ops
            .calls
            .iter()
            .position(|c| c == "subscribe_folder Lists/GitHub");
        let moved = ops.calls.iter().position(|c| c.starts_with("move"));
        assert!(
            created.is_some() && created < subscribed && subscribed < moved,
            "{:?}",
            ops.calls
        );
        assert!(store.folder("Lists/GitHub").unwrap().is_some());
        assert_eq!(store.message("INBOX", 5).unwrap(), None);
        assert!(
            store.messages_in_folder("Lists/GitHub").unwrap().is_empty(),
            "the next sync of the target folder adds the row"
        );
        assert!(
            ops.mail["Lists/GitHub"][0]
                .flags
                .contains(&"\\Seen".to_string()),
            "mark_read runs before the move, so the server copy carries it"
        );
    }

    #[test]
    fn a_move_in_another_case_on_a_case_insensitive_server_uses_the_folders_own_name() {
        let (mut ops, store, trash, _dir, msg) = setup();
        ops.case_insensitive = true;
        ops.create_folder("Lists").unwrap();
        store.upsert_folder(&new_folder("Lists")).unwrap();
        store
            .upsert_folder(&Folder {
                uidvalidity: 1,
                ..new_folder("Lists")
            })
            .unwrap();
        ops.calls.clear();
        apply(
            &plan(vec![Action::Move("lists".into())]),
            &msg,
            &mut ops,
            &store,
            &trash,
            500,
        )
        .unwrap();
        assert_eq!(ops.mail["Lists"].len(), 1);
        assert_eq!(
            store.folder("lists").unwrap(),
            None,
            "no second row for the same folder"
        );
        assert!(
            ops.calls.iter().any(|c| c == "move INBOX 5 -> Lists"),
            "{:?}",
            ops.calls
        );
    }

    #[test]
    fn a_folder_differing_only_in_case_on_a_case_sensitive_server_is_created() {
        let (mut ops, store, trash, _dir, msg) = setup();
        ops.create_folder("Lists").unwrap();
        store
            .upsert_folder(&Folder {
                uidvalidity: 1,
                ..new_folder("Lists")
            })
            .unwrap();
        apply(
            &plan(vec![Action::Move("lists".into())]),
            &msg,
            &mut ops,
            &store,
            &trash,
            500,
        )
        .unwrap();
        assert_eq!(ops.mail["lists"].len(), 1, "a new folder, as asked");
        assert!(ops.mail["Lists"].is_empty());
    }

    #[test]
    fn a_move_to_inbox_in_another_case_moves_to_inbox() {
        let (mut ops, store, trash, _dir, mut msg) = setup();
        msg.folder = "Archive".into();
        msg.uid = 7;
        store.insert_message(&msg).unwrap();
        ops.add_mail("Archive", 7, 100, "Subject: hi\r\n\r\n", None);
        ops.select("Archive").unwrap();
        ops.calls.clear();
        apply(
            &plan(vec![Action::Move("Inbox".into())]),
            &msg,
            &mut ops,
            &store,
            &trash,
            500,
        )
        .unwrap();
        assert!(
            ops.calls.iter().any(|c| c == "move Archive 7 -> INBOX"),
            "{:?}",
            ops.calls
        );
        assert!(!ops.calls.iter().any(|c| c.starts_with("create_folder")));
        assert_eq!(store.folder("Inbox").unwrap(), None);
    }

    #[test]
    fn a_move_to_inbox_in_any_case_from_inbox_is_skipped() {
        let (mut ops, store, trash, _dir, msg) = setup();
        apply(
            &plan(vec![Action::Move("inbox".into())]),
            &msg,
            &mut ops,
            &store,
            &trash,
            500,
        )
        .unwrap();
        assert!(!ops.calls.iter().any(|c| c.starts_with("move")));
    }

    #[test]
    fn a_move_to_a_folder_that_cannot_hold_messages_is_refused_by_name() {
        let (mut ops, store, trash, _dir, msg) = setup();
        let mut ops_noselect = std::mem::take(&mut ops).with_noselect("[Gmail]");
        store.set_noselect_folders(&["[Gmail]"]).unwrap();
        let moving = plan(vec![Action::Move("[Gmail]".into())]);
        let err = apply(&moving, &msg, &mut ops_noselect, &store, &trash, 500).unwrap_err();
        assert_eq!(err.to_string(), "[Gmail] cannot hold messages");
        let applied = apply_many(
            &moving.actions[0],
            &[msg],
            &mut ops_noselect,
            &store,
            &trash,
            500,
        );
        assert!(
            matches!(&applied[..], [(5, Err(ApplyError::NoSelect(name)))] if name == "[Gmail]"),
            "{applied:?}"
        );
        assert!(
            !ops_noselect
                .calls
                .iter()
                .any(|c| c.starts_with("create_folder") || c.starts_with("move")),
            "{:?}",
            ops_noselect.calls
        );
        assert!(store.message("INBOX", 5).unwrap().is_some());
    }

    #[test]
    fn a_move_of_many_subscribes_to_the_folder_it_created() {
        let (mut ops, store, trash, _dir, msg) = setup();
        let applied = apply_many(
            &plan(vec![Action::Move("Lists".into())]).actions[0],
            &[msg],
            &mut ops,
            &store,
            &trash,
            500,
        );
        assert!(matches!(applied[..], [(5, Ok(1))]), "{applied:?}");
        assert!(ops.calls.iter().any(|c| c == "subscribe_folder Lists"));
    }

    #[test]
    fn a_refused_create_is_reported_with_the_move_that_failed_after_it() {
        let refused = || MailError::Protocol("[CANNOT] Invalid mailbox name".into());
        let (mut ops, store, trash, _dir, msg) = setup();
        ops.fail_next_create = Some(refused());
        let err = apply(
            &plan(vec![Action::Move("Lists".into())]),
            &msg,
            &mut ops,
            &store,
            &trash,
            500,
        )
        .unwrap_err()
        .to_string();
        assert_eq!(
            err,
            "server error: no folder Lists; creating the folder failed first: server error: [CANNOT] Invalid \
             mailbox name"
        );

        // A direct action goes through `apply_many`.
        let (mut ops, store, trash, _dir, msg) = setup();
        ops.fail_next_create = Some(refused());
        let applied = apply_many(
            &plan(vec![Action::Move("Lists".into())]).actions[0],
            &[msg],
            &mut ops,
            &store,
            &trash,
            500,
        );
        let [(5, Err(err))] = &applied[..] else {
            panic!("{applied:?}");
        };
        assert!(
            err.to_string().ends_with(
                "creating the folder failed first: server error: [CANNOT] Invalid mailbox name"
            ),
            "{err}"
        );
    }

    #[test]
    fn a_refused_create_of_a_folder_already_there_is_no_error() {
        let (mut ops, store, trash, _dir, msg) = setup();
        ops.create_folder("Lists").unwrap();
        ops.fail_next_create = Some(MailError::Protocol("[ALREADYEXISTS] Lists".into()));
        apply(
            &plan(vec![Action::Move("Lists".into())]),
            &msg,
            &mut ops,
            &store,
            &trash,
            500,
        )
        .unwrap();
        assert_eq!(ops.mail["Lists"].len(), 1);
    }

    const NFC: &str = "Archief/\u{c9}\u{e9}n";
    const NFD: &str = "Archief/E\u{301}e\u{301}n";

    #[test]
    fn a_new_folder_written_in_nfd_is_created_in_nfc() {
        let (mut ops, store, trash, _dir, msg) = setup();
        let applied = apply_many(
            &plan(vec![Action::Move(NFD.into())]).actions[0],
            std::slice::from_ref(&msg),
            &mut ops,
            &store,
            &trash,
            500,
        );
        assert!(matches!(applied[..], [(5, Ok(1))]), "{applied:?}");
        assert!(
            ops.calls.contains(&format!("create_folder {NFC}")),
            "{:?}",
            ops.calls
        );
        assert_eq!(ops.mail[NFC].len(), 1);
        assert!(store.folder(NFC).unwrap().is_some());
        assert!(!ops.mail.contains_key(NFD));
    }

    #[test]
    fn a_target_in_nfd_moves_to_the_folder_known_in_nfc_and_back() {
        for (known, written) in [(NFC, NFD), (NFD, NFC)] {
            let (mut ops, store, trash, _dir, msg) = setup();
            ops.create_folder(known).unwrap();
            store
                .upsert_folder(&Folder {
                    uidvalidity: 1,
                    ..new_folder(known)
                })
                .unwrap();
            ops.calls.clear();
            apply(
                &plan(vec![Action::Move(written.into())]),
                &msg,
                &mut ops,
                &store,
                &trash,
                500,
            )
            .unwrap();
            assert_eq!(
                ops.calls,
                [format!("move INBOX 5 -> {known}")],
                "the server's own spelling, nothing created"
            );
            assert_eq!(store.folders().unwrap().len(), 3);
        }
    }

    #[test]
    fn a_move_to_the_own_folder_in_another_normalization_does_nothing() {
        let (mut ops, store, trash, _dir, mut msg) = setup();
        msg.folder = NFC.into();
        assert_eq!(
            move_target(&store, &msg.folder, &Action::Move(NFD.into())).unwrap(),
            None
        );
        ops.calls.clear();
        let applied = apply_many(
            &plan(vec![Action::Move(NFD.into())]).actions[0],
            &[msg],
            &mut ops,
            &store,
            &trash,
            500,
        );
        assert!(matches!(applied[..], [(5, Ok(0))]), "{applied:?}");
        assert!(ops.calls.is_empty(), "{:?}", ops.calls);
    }

    #[test]
    fn a_move_to_a_known_folder_creates_and_subscribes_nothing() {
        let (mut ops, store, trash, _dir, msg) = setup();
        let moving = plan(vec![Action::Move("Archive".into())]);
        apply(&moving, &msg, &mut ops, &store, &trash, 500).unwrap();
        assert!(
            !ops.calls
                .iter()
                .any(|c| c.starts_with("create_folder") || c.starts_with("subscribe_folder")),
            "{:?}",
            ops.calls
        );
    }

    #[test]
    fn a_folder_the_create_found_already_there_is_not_subscribed() {
        let (mut ops, store, trash, _dir, msg) = setup();
        // On the server, but not yet synced: the user may have unsubscribed it on purpose.
        ops.create_folder("Lists").unwrap();
        ops.calls.clear();
        apply(
            &plan(vec![Action::Move("Lists".into())]),
            &msg,
            &mut ops,
            &store,
            &trash,
            500,
        )
        .unwrap();
        assert!(ops.calls.iter().any(|c| c == "create_folder Lists"));
        assert!(!ops.calls.iter().any(|c| c.starts_with("subscribe_folder")));
        assert_eq!(ops.mail["Lists"].len(), 1);
    }

    #[test]
    fn a_refused_subscribe_still_moves_the_mail() {
        let (mut ops, store, trash, _dir, msg) = setup();
        ops.fail_next_subscribe = Some(MailError::Protocol("NO not allowed".into()));
        let n = apply(
            &plan(vec![Action::Move("Lists".into())]),
            &msg,
            &mut ops,
            &store,
            &trash,
            500,
        )
        .unwrap();
        assert_eq!(n, 1);
        assert_eq!(ops.mail["Lists"].len(), 1);
        assert_eq!(store.message("INBOX", 5).unwrap(), None);
    }

    #[test]
    fn a_rule_move_is_remembered_and_a_direct_one_or_a_failed_one_is_not() {
        let (mut ops, store, trash, _dir, msg) = setup();
        let mut direct = plan(vec![Action::Archive]);
        direct.actions[0].rule = RULE_NAME.into();
        apply(&direct, &msg, &mut ops, &store, &trash, 500).unwrap();
        assert!(
            !store.rule_moved_out(&msg).unwrap(),
            "the user may move it back and forth"
        );

        let (mut ops, store, trash, _dir, msg) = setup();
        let archive = ops.mail.remove("Archive").unwrap();
        assert!(
            apply(
                &plan(vec![Action::Archive]),
                &msg,
                &mut ops,
                &store,
                &trash,
                500
            )
            .is_err()
        );
        assert!(
            !store.rule_moved_out(&msg).unwrap(),
            "the next pass tries again"
        );
        ops.mail.insert("Archive".into(), archive);
        apply(
            &plan(vec![Action::Archive]),
            &msg,
            &mut ops,
            &store,
            &trash,
            500,
        )
        .unwrap();
        assert!(store.rule_moved_out(&msg).unwrap());
    }

    #[test]
    fn a_direct_move_from_any_front_end_is_not_a_rule_move() {
        for actor in ["gui", "mcp", "mcp:claude-desktop"] {
            let (mut ops, store, trash, _dir, msg) = setup();
            let mut direct = plan(vec![Action::Archive]);
            direct.actions[0].rule = actor.into();
            apply(&direct, &msg, &mut ops, &store, &trash, 500).unwrap();
            assert!(!store.rule_moved_out(&msg).unwrap(), "{actor}, one message");

            let (mut ops, store, trash, _dir, msg) = setup();
            apply_many(
                &direct.actions[0],
                std::slice::from_ref(&msg),
                &mut ops,
                &store,
                &trash,
                500,
            );
            assert!(!store.rule_moved_out(&msg).unwrap(), "{actor}, a batch");
        }
    }

    #[test]
    fn move_into_folder_the_store_does_not_know_yet() {
        let (mut ops, store, trash, _dir, msg) = setup();
        ops = ops.with_folder("Lists", None);
        let executed = apply(
            &plan(vec![Action::Move("Lists".into())]),
            &msg,
            &mut ops,
            &store,
            &trash,
            500,
        )
        .unwrap();
        assert_eq!(executed, 1);
        assert_eq!(ops.mail["Lists"].len(), 1);
        assert!(ops.mail["INBOX"].is_empty());
        assert!(store.folder("Lists").unwrap().is_some());
    }

    #[test]
    fn move_to_own_folder_is_skipped() {
        let (mut ops, store, trash, _dir, msg) = setup();
        apply(
            &plan(vec![Action::Move("INBOX".into())]),
            &msg,
            &mut ops,
            &store,
            &trash,
            500,
        )
        .unwrap();
        assert!(!ops.calls.iter().any(|c| c.starts_with("move")));
        assert!(store.message("INBOX", 5).unwrap().is_some());
        assert!(store.log(10).unwrap().is_empty());
    }

    #[test]
    fn archive_without_folder_changes_nothing() {
        let (mut ops, store, trash, _dir, msg) = setup();
        store
            .upsert_folder(&Folder {
                name: "Archive".into(),
                uidvalidity: 1,
                last_uid: 0,
                special_use: None,
                delimiter: None,
            })
            .unwrap();
        let result = apply(
            &plan(vec![Action::MarkRead, Action::Archive]),
            &msg,
            &mut ops,
            &store,
            &trash,
            500,
        );
        assert!(matches!(result, Err(ApplyError::NoArchiveFolder)));
        assert!(!ops.calls.iter().any(|c| c.starts_with("add_flags")));
        assert!(store.log(10).unwrap().is_empty());
    }

    fn with_junk(ops: RecordingOps, store: &Store) -> RecordingOps {
        store
            .upsert_folder(&Folder {
                name: "Spam".into(),
                uidvalidity: 1,
                last_uid: 0,
                special_use: Some("Junk".into()),
                delimiter: None,
            })
            .unwrap();
        ops.with_folder("Spam", Some("Junk"))
    }

    #[test]
    fn junk_tags_and_moves_to_the_junk_folder() {
        let (ops, store, trash, _dir, mut msg) = setup();
        let mut ops = with_junk(ops, &store);
        ops.select("INBOX").unwrap();
        msg.flags = "$NotJunk".into();
        store.update_flags("INBOX", 5, "$NotJunk").unwrap();
        let ran = apply(
            &plan(vec![Action::Junk]),
            &msg,
            &mut ops,
            &store,
            &trash,
            500,
        )
        .unwrap();
        assert_eq!(ran, 1);
        let calls: Vec<&str> = ops
            .calls
            .iter()
            .map(String::as_str)
            .filter(|c| !c.starts_with("select"))
            .collect();
        assert_eq!(
            calls,
            [
                "remove_flags INBOX 5 $NotJunk",
                "add_flags INBOX 5 $Junk",
                "move INBOX 5 -> Spam"
            ]
        );
        assert!(store.message("INBOX", 5).unwrap().is_none());
        assert_eq!(store.log(10).unwrap()[0].action, "junk");
    }

    #[test]
    fn a_rule_junk_is_remembered_as_a_rule_move_and_a_direct_one_is_not() {
        for (actor, remembered) in [("r", true), ("gui", false), ("mcp:claude-desktop", false)] {
            let (ops, store, trash, _dir, msg) = setup();
            let mut ops = with_junk(ops, &store);
            ops.select("INBOX").unwrap();
            let mut junk = plan(vec![Action::Junk]);
            junk.actions[0].rule = actor.into();
            apply(&junk, &msg, &mut ops, &store, &trash, 500).unwrap();
            assert_eq!(store.rule_moved_out(&msg).unwrap(), remembered, "{actor}");
        }
    }

    #[test]
    fn not_junk_tags_and_moves_back_to_the_inbox() {
        let (ops, store, trash, _dir, _msg) = setup();
        let mut ops = with_junk(ops, &store);
        ops.add_mail("Spam", 7, 100, "Subject: hi\r\n\r\n", None);
        ops.select("Spam").unwrap();
        let msg = Message {
            folder: "Spam".into(),
            uid: 7,
            thread_id: "t".into(),
            flags: "$Junk".into(),
            ..Default::default()
        };
        store.insert_message(&msg).unwrap();
        apply(
            &plan(vec![Action::NotJunk]),
            &msg,
            &mut ops,
            &store,
            &trash,
            500,
        )
        .unwrap();
        let calls: Vec<&str> = ops
            .calls
            .iter()
            .map(String::as_str)
            .filter(|c| !c.starts_with("select"))
            .collect();
        assert_eq!(
            calls,
            [
                "remove_flags Spam 7 $Junk",
                "add_flags Spam 7 $NotJunk",
                "move Spam 7 -> INBOX"
            ]
        );
    }

    #[test]
    fn junk_still_moves_when_the_server_refuses_keywords() {
        let (ops, store, trash, _dir, msg) = setup();
        let mut ops = with_junk(ops, &store);
        ops.select("INBOX").unwrap();
        ops.fail_next = Some(MailError::Protocol("no keywords".into()));
        apply(
            &plan(vec![Action::Junk]),
            &msg,
            &mut ops,
            &store,
            &trash,
            500,
        )
        .unwrap();
        assert!(ops.calls.iter().any(|c| c == "move INBOX 5 -> Spam"));
    }

    #[test]
    fn junk_without_a_junk_folder_changes_nothing() {
        let (mut ops, store, trash, _dir, msg) = setup();
        let result = apply(
            &plan(vec![Action::Junk]),
            &msg,
            &mut ops,
            &store,
            &trash,
            500,
        );
        assert!(matches!(result, Err(ApplyError::NoJunkFolder)));
        assert!(
            !ops.calls
                .iter()
                .any(|c| c.starts_with("add_flags") || c.starts_with("move"))
        );
        assert!(store.log(10).unwrap().is_empty());
    }

    #[test]
    fn delete_refused_when_trash_write_fails() {
        let (mut ops, store, _trash, dir, msg) = setup();
        let blocked = dir.path().join("not-a-dir");
        std::fs::write(&blocked, b"").unwrap();
        let err = apply(
            &plan(vec![Action::Delete]),
            &msg,
            &mut ops,
            &store,
            &Trash::new(blocked),
            500,
        )
        .unwrap_err();
        assert!(matches!(err, ApplyError::Trash(_)));
        assert!(
            !ops.calls
                .iter()
                .any(|c| c.starts_with("add_flags") || c.starts_with("expunge"))
        );
        assert_eq!(ops.mail["INBOX"].len(), 1);
        assert!(store.message("INBOX", 5).unwrap().is_some());
    }

    #[test]
    fn archive_uses_special_use_folder_or_errors() {
        let (mut ops, store, trash, _dir, msg) = setup();
        apply(
            &plan(vec![Action::Archive]),
            &msg,
            &mut ops,
            &store,
            &trash,
            500,
        )
        .unwrap();
        assert_eq!(ops.mail["Archive"].len(), 1);
        let (mut ops2, store2, trash2, _dir2, msg2) = setup();
        store2
            .upsert_folder(&Folder {
                name: "Archive".into(),
                uidvalidity: 1,
                last_uid: 0,
                special_use: None,
                delimiter: None,
            })
            .unwrap();
        assert!(matches!(
            apply(
                &plan(vec![Action::Archive]),
                &msg2,
                &mut ops2,
                &store2,
                &trash2,
                500
            ),
            Err(ApplyError::NoArchiveFolder)
        ));
    }

    #[test]
    fn flag_and_mark_read_update_server_and_store() {
        let (mut ops, store, trash, _dir, msg) = setup();
        apply(
            &plan(vec![Action::Flag, Action::MarkRead]),
            &msg,
            &mut ops,
            &store,
            &trash,
            500,
        )
        .unwrap();
        let m = store.message("INBOX", 5).unwrap().unwrap();
        assert_eq!(m.flags, "\\Flagged \\Seen");
        assert_eq!(store.log(10).unwrap().len(), 2);
    }

    #[test]
    fn tag_adds_the_keyword_once() {
        let (mut ops, store, trash, _dir, mut msg) = setup();
        let tag = plan(vec![Action::Tag("$label1".into())]);
        assert_eq!(apply(&tag, &msg, &mut ops, &store, &trash, 500).unwrap(), 1);
        assert!(ops.calls.iter().any(|c| c == "add_flags INBOX 5 $label1"));
        assert_eq!(store.message("INBOX", 5).unwrap().unwrap().flags, "$label1");
        assert_eq!(store.log(10).unwrap()[0].action, "tag:$label1");
        msg.flags = "$label1".into();
        assert_eq!(apply(&tag, &msg, &mut ops, &store, &trash, 500).unwrap(), 0);
    }

    #[test]
    fn a_tag_that_is_not_an_imap_keyword_is_refused_before_the_server() {
        let (mut ops, store, trash, _dir, msg) = setup();
        let bad = plan(vec![Action::Tag("x) \\Deleted".into())]);
        assert!(matches!(
            apply(&bad, &msg, &mut ops, &store, &trash, 500),
            Err(ApplyError::InvalidTag(_))
        ));
        assert!(!ops.calls.iter().any(|c| c.starts_with("add_flags")));
        assert!(store.log(10).unwrap().is_empty());
    }

    fn with_trash_folder(ops: RecordingOps, store: &Store) -> RecordingOps {
        store
            .upsert_folder(&Folder {
                name: "Trash".into(),
                uidvalidity: 1,
                last_uid: 0,
                special_use: Some("Trash".into()),
                delimiter: None,
            })
            .unwrap();
        ops.with_folder("Trash", Some("Trash"))
    }

    #[test]
    fn mark_unread_and_unflag_remove_flags_once() {
        let (mut ops, store, trash, _dir, mut msg) = setup();
        ops.add_flags(5, &["\\Seen", "\\Flagged"]).unwrap();
        store.update_flags("INBOX", 5, "\\Seen \\Flagged").unwrap();
        msg.flags = "\\Seen \\Flagged".into();
        let unset = plan(vec![Action::MarkUnread, Action::Unflag]);
        assert_eq!(
            apply(&unset, &msg, &mut ops, &store, &trash, 500).unwrap(),
            2
        );
        assert!(ops.mail["INBOX"][0].flags.is_empty());
        let stored = store.message("INBOX", 5).unwrap().unwrap();
        assert_eq!(stored.flags, "");
        assert_eq!(
            apply(&unset, &stored, &mut ops, &store, &trash, 500).unwrap(),
            0
        );
        let actions: Vec<String> = store
            .log(10)
            .unwrap()
            .into_iter()
            .map(|e| e.action)
            .collect();
        assert_eq!(actions, ["flagged:false", "read:false"]);
    }

    #[test]
    fn user_delete_moves_to_the_trash_folder() {
        let (ops, store, trash, _dir, msg) = setup();
        let mut ops = with_trash_folder(ops, &store);
        assert_eq!(
            apply(
                &plan(vec![Action::Trash]),
                &msg,
                &mut ops,
                &store,
                &trash,
                500
            )
            .unwrap(),
            1
        );
        assert_eq!(ops.mail["Trash"].len(), 1);
        assert!(ops.mail["INBOX"].is_empty());
        assert_eq!(store.message("INBOX", 5).unwrap(), None);
        assert!(!ops.calls.iter().any(|c| c.starts_with("expunge")));
        assert!(trash.list().unwrap().is_empty());
        assert_eq!(store.log(10).unwrap()[0].action, "trash");
    }

    #[test]
    fn rule_delete_expunges_even_with_a_trash_folder() {
        let (ops, store, trash, _dir, msg) = setup();
        let mut ops = with_trash_folder(ops, &store);
        apply(
            &plan(vec![Action::Delete]),
            &msg,
            &mut ops,
            &store,
            &trash,
            500,
        )
        .unwrap();
        assert!(ops.mail["Trash"].is_empty());
        assert!(ops.mail["INBOX"].is_empty());
        assert!(!ops.calls.iter().any(|c| c.starts_with("move")));
        assert!(ops.calls.iter().any(|c| c.starts_with("expunge")));
        assert_eq!(trash.list().unwrap().len(), 1);
        assert_eq!(store.log(10).unwrap()[0].action, "delete");
    }

    #[test]
    fn user_delete_without_trash_folder_expunges_with_backup() {
        let (mut ops, store, trash, _dir, msg) = setup();
        apply(
            &plan(vec![Action::Trash]),
            &msg,
            &mut ops,
            &store,
            &trash,
            500,
        )
        .unwrap();
        assert!(ops.mail["INBOX"].is_empty());
        assert_eq!(trash.list().unwrap().len(), 1);
        let log = store.log(10).unwrap();
        assert!(log[0].trash_file.is_some());
        assert_eq!(
            log[0].action, "delete",
            "the log says the message was expunged"
        );
    }

    #[test]
    fn user_delete_inside_the_trash_folder_expunges_with_backup() {
        let (mut ops, store, trash, _dir, msg) = setup();
        store
            .upsert_folder(&Folder {
                name: "INBOX".into(),
                uidvalidity: 1,
                last_uid: 5,
                special_use: Some("Trash".into()),
                delimiter: None,
            })
            .unwrap();
        apply(
            &plan(vec![Action::Trash]),
            &msg,
            &mut ops,
            &store,
            &trash,
            500,
        )
        .unwrap();
        assert!(!ops.calls.iter().any(|c| c.starts_with("move")));
        assert!(ops.mail["INBOX"].is_empty());
        assert_eq!(trash.list().unwrap().len(), 1);
    }
}
