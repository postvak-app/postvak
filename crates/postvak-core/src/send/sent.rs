//! After the server took a message: the copy in Sent and the flag on the original, done by the account's IMAP thread.

use super::SmtpServer;
use super::drafts::SaveDraftError;
use super::outbox::Original;
use crate::mail_ops::{MailError, MailOps};
use crate::store::{Folder, Store};
use crate::sync::LostConnection;

/// SMTP hosts whose provider files a copy of what it sends in Sent itself, where a copy of ours would be a second one.
const FILES_SENT_MAIL: &[&str] = &[
    "smtp.gmail.com",
    "smtp-mail.outlook.com",
    "smtp.office365.com",
];

/// Whether to save a copy in Sent: `setting` when set; else off for providers that file one themselves.
pub fn saves_sent(smtp: &SmtpServer, setting: Option<bool>) -> bool {
    setting.unwrap_or_else(|| {
        let host = smtp.host.trim_end_matches('.').to_ascii_lowercase();
        !(FILES_SENT_MAIL.contains(&host.as_str()) || host.ends_with(".google.com"))
    })
}

/// The folder a copy of sent mail goes to: the one marked `\Sent`, else one named `Sent`.
pub fn sent_folder(folders: &[Folder]) -> Option<String> {
    folders
        .iter()
        .find(|folder| folder.special_use.as_deref() == Some("Sent"))
        .or_else(|| {
            folders
                .iter()
                .find(|folder| crate::folder_name::same(&folder.name, "Sent"))
        })
        .map(|folder| folder.name.clone())
}

/// What is left to do for a sent message, and what is done.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finish {
    /// The message as kept, Bcc included.
    pub raw: Vec<u8>,
    pub save_sent: bool,
    pub copied: bool,
    pub original: Option<Original>,
    pub flagged: bool,
    /// The copies in Drafts to delete, each `<message-id>`.
    pub draft: Vec<String>,
    pub draft_deleted: bool,
}

/// Saves the Sent copy, flags the original and deletes the draft, each unless done; returns (copied, flagged,
/// draft_deleted). A step the server refuses is given up, logged and, for the copy, recorded for the status; only a
/// lost connection is an error, to try again after reconnecting.
pub fn finish(
    ops: &mut dyn MailOps,
    store: &Store,
    account: &str,
    id: &str,
    job: &Finish,
) -> Result<(bool, bool, bool), MailError> {
    let mut copied = job.copied || !job.save_sent;
    if !copied {
        let folders = store.folders().unwrap_or_default();
        match sent_folder(&folders) {
            Some(folder) => match ops.append(&folder, &job.raw, &["\\Seen"]) {
                Ok(()) => {
                    log::info!("{account}: saved {id} in {folder}");
                    let _ = store.set_sent_copy_problem(None);
                }
                Err(e) if e.is_lost_connection() => return Err(e),
                Err(e) => {
                    log::warn!("{account}: the copy of {id} in {folder} failed: {e}");
                    let _ = store
                        .set_sent_copy_problem(Some(&format!("the copy in {folder} failed: {e}")));
                }
            },
            None => {
                log::warn!("{account}: no Sent folder for the copy of {id}");
                let _ = store.set_sent_copy_problem(Some("no Sent folder"));
            }
        }
        copied = true;
    }
    let mut flagged = job.flagged;
    if !flagged {
        if let Some(original) = &job.original {
            let message_id = original
                .message_id
                .trim()
                .trim_start_matches('<')
                .trim_end_matches('>');
            // Looked up again now: a rule may have moved it while the message waited.
            match store.latest_by_message_id(message_id) {
                Ok(Some(message)) => {
                    let flag = original.flag.flag();
                    let done = ops
                        .select(&message.folder)
                        .and_then(|_| ops.add_flags(message.uid, &[flag]));
                    match done {
                        Ok(()) => log::info!("{account}: flagged the original of {id}"),
                        Err(e) if e.is_lost_connection() => return Err(e),
                        // A server that keeps no keywords refuses `$Forwarded`; nothing to tell.
                        Err(e) => {
                            log::info!("{account}: the original of {id} was not flagged: {e}")
                        }
                    }
                }
                _ => log::info!("{account}: the original of {id} is gone; not flagged"),
            }
        }
        flagged = true;
    }
    if !job.draft_deleted && !job.draft.is_empty() {
        match super::drafts::save(ops, store, account, None, &job.draft) {
            Ok(_) => {}
            Err(SaveDraftError::Mail(e)) if e.is_lost_connection() => return Err(e),
            Err(e) => log::warn!(
                "{account}: draft {} of {id} was not deleted: {e}",
                job.draft.join(", ")
            ),
        }
    }
    Ok((copied, flagged, true))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SmtpTls;

    fn server(host: &str) -> SmtpServer {
        SmtpServer {
            host: host.into(),
            port: 465,
            tls: SmtpTls::Implicit,
            username: "me".into(),
        }
    }

    #[test]
    fn gmail_and_outlook_file_sent_mail_themselves() {
        for host in [
            "smtp.gmail.com",
            "smtp-relay.google.com",
            "smtp-mail.outlook.com",
            "SMTP.Office365.com",
        ] {
            assert!(!saves_sent(&server(host), None), "{host}");
        }
        assert!(saves_sent(&server("smtp.example.com"), None));
        assert!(saves_sent(&server("smtp.gmail.com"), Some(true)));
        assert!(!saves_sent(&server("smtp.example.com"), Some(false)));
    }

    fn folder(name: &str, special_use: Option<&str>) -> Folder {
        Folder {
            name: name.into(),
            uidvalidity: 1,
            last_uid: 0,
            special_use: special_use.map(str::to_string),
            delimiter: None,
        }
    }

    #[test]
    fn sent_folder_prefers_special_use_then_the_name() {
        let folders = [folder("Sent", None), folder("Sent Items", Some("Sent"))];
        assert_eq!(sent_folder(&folders).as_deref(), Some("Sent Items"));
        assert_eq!(sent_folder(&folders[..1]).as_deref(), Some("Sent"));
        assert_eq!(sent_folder(&[folder("sent", None)]), None);
        assert_eq!(sent_folder(&[]), None);
    }
}
