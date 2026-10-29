//! Creating, renaming and deleting a folder on the server and in the store: checks first, then the server, then the
//! store, so a refusal anywhere leaves the store as the server has it.
use std::io;

use crate::folder_name;
use crate::mail_ops::{MailError, MailOps};
use crate::rules::apply::special_folder;
use crate::store::{Folder, Store, StoreError};
use crate::sync::DeleteMode;
use crate::trash::Trash;

#[derive(Debug, thiserror::Error)]
pub enum FolderError {
    #[error("{0}")]
    Protected(String),
    #[error("{0}")]
    Invalid(String),
    #[error("Delete or move its subfolders first.")]
    HasSubfolders,
    #[error("the folder holds {now} messages now, not the {shown} confirmed; nothing was deleted")]
    CountChanged { shown: u32, now: u32 },
    #[error(transparent)]
    Mail(#[from] MailError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("could not back up the folder's mail, so nothing was deleted: {0}")]
    Backup(io::Error),
    #[error("the folder's mail was moved, but deleting the empty folder failed: {0}")]
    LeftEmpty(MailError),
}

/// What a delete did: the folder is gone, or it holds mail and needs a mode and the confirmed count.
#[derive(Debug, Clone, PartialEq)]
pub enum Deleted {
    Done,
    NotEmpty { count: u32, has_trash: bool },
}

/// Creates and subscribes `name`, then stores it; returns its NFC spelling.
pub fn create(ops: &mut dyn MailOps, store: &Store, name: &str) -> Result<String, FolderError> {
    let name = checked_new(store, name)?;
    ops.create_folder(&name)?;
    if let Err(e) = ops.subscribe_folder(&name) {
        log::warn!("{name}: created, but could not subscribe to it: {e}");
    }
    store.upsert_folder(&Folder {
        name: name.clone(),
        // A placeholder until the folder's first sync, as a rule move's new folder is.
        uidvalidity: 0,
        last_uid: 0,
        special_use: None,
        delimiter: delimiter_for(store, &name)?,
    })?;
    Ok(name)
}

/// Renames `from` and the folders under it to `to`; returns `to` in NFC.
pub fn rename(
    ops: &mut dyn MailOps,
    store: &Store,
    from: &str,
    to: &str,
) -> Result<String, FolderError> {
    let from = stored_name(store, from)?;
    refuse_protected(store, &from)?;
    if let Some(child) = folder_name::protected_below(store, &from)? {
        return Err(FolderError::Protected(format!(
            "{from} cannot be renamed: {child} is under it"
        )));
    }
    let to = checked_new(store, to)?;
    ops.rename_folder(&from, &to)?;
    if let Err(e) = ops.subscribe_folder(&to) {
        log::warn!("{to}: renamed, but could not subscribe to it: {e}");
    }
    if let Err(e) = ops.unsubscribe_folder(&from) {
        log::warn!("{from}: renamed, but could not unsubscribe from the old name: {e}");
    }
    store.rename_folder(&from, &to)?;
    Ok(to)
}

/// Deletes `name`. A folder holding mail needs `mode` and `count`, the number of messages the user confirmed; without
/// a mode it is left alone and its count returned.
pub fn delete(
    ops: &mut dyn MailOps,
    store: &Store,
    trash: &Trash,
    name: &str,
    mode: Option<DeleteMode>,
    count: u32,
    now: i64,
) -> Result<Deleted, FolderError> {
    let name = stored_name(store, name)?;
    refuse_protected(store, &name)?;
    if !folder_name::children(store, &name)?.is_empty() {
        return Err(FolderError::HasSubfolders);
    }
    let held = server_count(ops, &name)?;
    let trash_folder = special_folder(store, "Trash")?;
    if held > 0 {
        let Some(mode) = mode else {
            return Ok(Deleted::NotEmpty {
                count: held,
                has_trash: trash_folder.is_some(),
            });
        };
        empty(ops, trash, &name, mode, count, trash_folder, now)?;
    }
    if let Err(e) = ops.delete_folder(&name) {
        return Err(match mode {
            Some(DeleteMode::Trash) if held > 0 => FolderError::LeftEmpty(e),
            _ => FolderError::Mail(e),
        });
    }
    store.remove_folder(&name)?;
    Ok(Deleted::Done)
}

/// Moves every message of `name` to Trash, or backs every one up before DELETE takes them; refuses when the folder
/// does not hold exactly the `count` messages the user confirmed.
fn empty(
    ops: &mut dyn MailOps,
    trash: &Trash,
    name: &str,
    mode: DeleteMode,
    count: u32,
    trash_folder: Option<String>,
    now: i64,
) -> Result<(), FolderError> {
    ops.select(name)?;
    let uids = ops.search_uids(1)?;
    // Checked on the selected folder, so mail arriving since the confirm is never taken.
    let held = u32::try_from(uids.len()).unwrap_or(u32::MAX);
    if held != count {
        return Err(FolderError::CountChanged {
            shown: count,
            now: held,
        });
    }
    match mode {
        DeleteMode::Trash => {
            let Some(trash_folder) = trash_folder else {
                return Err(FolderError::Invalid(
                    "this account has no Trash folder".into(),
                ));
            };
            ops.move_messages(&uids, &trash_folder)?;
        }
        DeleteMode::Permanent => {
            for uid in uids {
                let raw = ops.fetch_raw(uid)?.ok_or_else(|| {
                    FolderError::Backup(io::Error::other(format!(
                        "{name}/{uid}: the server sent no message"
                    )))
                })?;
                trash
                    .save(name, uid, &raw, now)
                    .map_err(FolderError::Backup)?;
            }
        }
    }
    Ok(())
}

/// The server's count of messages in `name`; STATUS of the selected folder is one a server may refuse.
fn server_count(ops: &mut dyn MailOps, name: &str) -> Result<u32, MailError> {
    if ops.selected() == Some(name) {
        return Ok(ops.select(name)?.exists);
    }
    Ok(ops.status(name)?.messages)
}

/// The store's spelling of `name`, or `name` itself when the store does not know it.
fn stored_name(store: &Store, name: &str) -> Result<String, StoreError> {
    Ok(folder_name::known(store, name)?.map_or_else(|| name.to_string(), |folder| folder.name))
}

fn checked_new(store: &Store, name: &str) -> Result<String, FolderError> {
    folder_name::check_new(store, name)
        .map_err(|reason| FolderError::Invalid(format!("cannot use {name}: {reason}")))
}

fn refuse_protected(store: &Store, name: &str) -> Result<(), FolderError> {
    match folder_name::protected(store, name)? {
        Some(role) => Err(FolderError::Protected(format!(
            "{name} is the {role} folder, which cannot be renamed or deleted"
        ))),
        None => Ok(()),
    }
}

/// The delimiter of the stored folder `name` sits under, else INBOX's, which a top-level folder shares.
fn delimiter_for(store: &Store, name: &str) -> Result<Option<String>, StoreError> {
    let folders = store.folders()?;
    let parent = folders.iter().find_map(|folder| {
        let delimiter = folder.delimiter.as_deref().filter(|d| !d.is_empty())?;
        let below = name.strip_prefix(folder.name.as_str())?;
        below.starts_with(delimiter).then(|| delimiter.to_string())
    });
    Ok(parent.or_else(|| {
        folders
            .into_iter()
            .find(|folder| folder.name == "INBOX")
            .and_then(|folder| folder.delimiter)
    }))
}
