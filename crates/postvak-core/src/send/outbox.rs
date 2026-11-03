//! The outbox: each message waiting to go, or gone but with work left on the IMAP side, as two files in
//! `<state>/outbox/<account>/`: `<id>.eml`, the message as kept (Bcc included), and `<id>.json`, its [`Entry`]. The
//! `.eml` is written first, so an entry never names a message that is not there. Every write is synced before it
//! returns, so an accepted message survives a crash, and one that may have reached the server is never sent again.

use std::fs;
use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::Envelope;
use super::reply::OriginalFlag;
use crate::paths::{Paths, write_atomic};

/// Why a message stopped where it is: the reason a crash leaves.
pub const MAY_HAVE_BEEN_SENT: &str = "may have been sent; check Sent before retrying";

/// Where a message is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum State {
    /// Waiting for its due time; it can still be cancelled.
    Waiting,
    /// Being handed to the server; too late to cancel.
    Sending,
    /// The server took it; the Sent copy or the flag on the original is still to do.
    Sent,
    /// Not sent, with why; `retrying` while a temporary failure is tried again.
    Failed { reason: String, retrying: bool },
}

/// The message a reply or forward answers, found again by Message-ID when it is time to flag it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Original {
    pub message_id: String,
    pub flag: OriginalFlag,
}

/// One message in the outbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub envelope: Envelope,
    /// When it goes, as a Unix time.
    pub due: i64,
    #[serde(flatten)]
    pub state: State,
    /// Whether to save a copy in Sent once it went.
    pub save_sent: bool,
    #[serde(default)]
    pub original: Option<Original>,
    /// How many times the server was tried.
    #[serde(default)]
    pub attempts: u32,
    /// When the server was first tried, for giving up on temporary failures.
    #[serde(default)]
    pub first_try: Option<i64>,
    /// Whether the Sent copy is done (or not wanted).
    #[serde(default)]
    pub copied: bool,
    /// Whether the original is flagged (or there is none).
    #[serde(default)]
    pub flagged: bool,
}

impl Entry {
    /// A new entry for a message to `envelope`, due at `due`.
    pub fn new(envelope: Envelope, due: i64, save_sent: bool, original: Option<Original>) -> Entry {
        Entry {
            id: new_id(),
            envelope,
            due,
            state: State::Waiting,
            save_sent,
            original,
            attempts: 0,
            first_try: None,
            copied: false,
            flagged: false,
        }
    }

    /// Whether the IMAP side has nothing left to do for it.
    pub fn finished(&self) -> bool {
        (self.copied || !self.save_sent) && (self.flagged || self.original.is_none())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CancelError {
    #[error("no message {0} in the outbox")]
    NotFound(String),
    #[error("message {0} is already being sent or was sent; it is too late to cancel it")]
    TooLate(String),
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// A new outbox id: eight hex digits, short enough to type.
pub fn new_id() -> String {
    format!("{:08x}", crate::sync::random_u64() as u32)
}

/// Whether `id` is one [`new_id`] could have made, so it is safe in a file name.
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 32 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

fn not_found(id: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        format!("no message {id} in the outbox"),
    )
}

/// One account's outbox folder.
#[derive(Debug, Clone)]
pub struct Outbox {
    dir: PathBuf,
}

impl Outbox {
    /// The outbox of `account`, created private if it is not there.
    pub fn open(paths: &Paths, account: &str) -> io::Result<Outbox> {
        let dir = paths.outbox_dir(account);
        crate::paths::create_private_dir(&dir)?;
        Ok(Outbox { dir })
    }

    /// The outbox of `account` when it is there, for a reader that must not create it.
    pub fn existing(paths: &Paths, account: &str) -> Option<Outbox> {
        let dir = paths.outbox_dir(account);
        dir.is_dir().then_some(Outbox { dir })
    }

    fn eml(&self, id: &str) -> io::Result<PathBuf> {
        if !valid_id(id) {
            return Err(not_found(id));
        }
        Ok(self.dir.join(format!("{id}.eml")))
    }

    fn json(&self, id: &str) -> io::Result<PathBuf> {
        if !valid_id(id) {
            return Err(not_found(id));
        }
        Ok(self.dir.join(format!("{id}.json")))
    }

    /// Adds a message: `keep` as it is kept (Bcc included), then its entry.
    pub fn add(&self, keep: &[u8], entry: &Entry) -> io::Result<()> {
        if self.json(&entry.id)?.exists() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("outbox id {} is taken", entry.id),
            ));
        }
        write_atomic(&self.eml(&entry.id)?, keep)?;
        self.write(entry)
    }

    /// Writes `entry`, replacing what is there.
    pub fn write(&self, entry: &Entry) -> io::Result<()> {
        let json = serde_json::to_vec_pretty(entry).map_err(io::Error::other)?;
        write_atomic(&self.json(&entry.id)?, &json)
    }

    /// The entry `id`.
    pub fn get(&self, id: &str) -> io::Result<Entry> {
        let text = fs::read(self.json(id)?).map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => not_found(id),
            _ => e,
        })?;
        let mut entry: Entry = serde_json::from_slice(&text).map_err(io::Error::other)?;
        if !self.eml(id)?.exists() && entry.state != State::Sent {
            entry.state = State::Failed {
                reason: "the message file is missing from the outbox".into(),
                retrying: false,
            };
        }
        Ok(entry)
    }

    /// Every entry, the earliest due first. An entry that cannot be read is logged and left out.
    pub fn list(&self) -> io::Result<Vec<Entry>> {
        let mut entries = Vec::new();
        for file in fs::read_dir(&self.dir)? {
            let path = file?.path();
            let Some(id) = path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_suffix(".json"))
                .filter(|id| valid_id(id))
            else {
                continue;
            };
            match self.get(id) {
                Ok(entry) => entries.push(entry),
                Err(e) => log::warn!(
                    "outbox {}: entry {id} cannot be read: {e}",
                    self.dir.display()
                ),
            }
        }
        entries.sort_by(|a, b| a.due.cmp(&b.due).then_with(|| a.id.cmp(&b.id)));
        Ok(entries)
    }

    /// The message `id` as kept.
    pub fn raw(&self, id: &str) -> io::Result<Vec<u8>> {
        fs::read(self.eml(id)?)
    }

    /// Sets the state of `id`, synced before it returns.
    pub fn set_state(&self, id: &str, state: State) -> io::Result<()> {
        let mut entry = self.get(id)?;
        entry.state = state;
        self.write(&entry)
    }

    /// Takes back a message not yet handed to the server: returns it and removes it.
    pub fn cancel(&self, id: &str) -> Result<Vec<u8>, CancelError> {
        let entry = self.get(id).map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => CancelError::NotFound(id.to_string()),
            _ => CancelError::Io(e),
        })?;
        match entry.state {
            State::Sending | State::Sent => Err(CancelError::TooLate(id.to_string())),
            State::Waiting | State::Failed { .. } => {
                let raw = self.raw(id).unwrap_or_default();
                self.remove(id)?;
                Ok(raw)
            }
        }
    }

    /// Removes both files of `id`; the entry first, so a crash leaves at most a stray `.eml`.
    pub fn remove(&self, id: &str) -> io::Result<()> {
        for path in [self.json(id)?, self.eml(id)?] {
            match fs::remove_file(&path) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
        }
        fs::File::open(&self.dir)?.sync_all()
    }

    /// After a restart: a message that was being handed to the server may have reached it, so it is never sent again;
    /// it fails with [`MAY_HAVE_BEEN_SENT`].
    pub fn recover(&self) -> io::Result<()> {
        for entry in self.list()? {
            if entry.state == State::Sending {
                log::warn!(
                    "outbox: {} may have been sent before the daemon stopped",
                    entry.id
                );
                self.set_state(
                    &entry.id,
                    State::Failed {
                        reason: MAY_HAVE_BEEN_SENT.into(),
                        retrying: false,
                    },
                )?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(due: i64) -> Entry {
        Entry::new(
            Envelope {
                from: "me@example.com".into(),
                to: vec!["a@x.org".into()],
            },
            due,
            true,
            Some(Original {
                message_id: "<m@x.org>".into(),
                flag: OriginalFlag::Answered,
            }),
        )
    }

    fn outbox() -> (tempfile::TempDir, Outbox) {
        let dir = tempfile::tempdir().unwrap();
        let outbox = Outbox::open(&Paths::under(dir.path()), "work").unwrap();
        (dir, outbox)
    }

    #[test]
    fn add_then_list_round_trips() {
        let (_dir, outbox) = outbox();
        let (late, early) = (entry(20), entry(10));
        outbox.add(b"late", &late).unwrap();
        outbox.add(b"early", &early).unwrap();
        assert_eq!(outbox.list().unwrap(), [early.clone(), late.clone()]);
        assert_eq!(outbox.raw(&early.id).unwrap(), b"early");
        assert!(outbox.add(b"again", &early).is_err());
        assert!(!new_id().is_empty() && valid_id(&new_id()));
    }

    #[test]
    fn cancel_returns_the_message_and_removes_both_files() {
        let (dir, outbox) = outbox();
        let e = entry(10);
        outbox.add(b"message", &e).unwrap();
        assert_eq!(outbox.cancel(&e.id).unwrap(), b"message");
        assert!(outbox.list().unwrap().is_empty());
        let left: Vec<_> = fs::read_dir(dir.path().join("state/outbox/work"))
            .unwrap()
            .collect();
        assert!(left.is_empty());
        assert!(matches!(
            outbox.cancel(&e.id),
            Err(CancelError::NotFound(_))
        ));
        // A failed one can be taken back too.
        let failed = entry(10);
        outbox.add(b"f", &failed).unwrap();
        outbox
            .set_state(
                &failed.id,
                State::Failed {
                    reason: "no".into(),
                    retrying: false,
                },
            )
            .unwrap();
        assert_eq!(outbox.cancel(&failed.id).unwrap(), b"f");
    }

    #[test]
    fn cancel_while_sending_is_too_late() {
        let (_dir, outbox) = outbox();
        let e = entry(10);
        outbox.add(b"m", &e).unwrap();
        outbox.set_state(&e.id, State::Sending).unwrap();
        assert!(matches!(outbox.cancel(&e.id), Err(CancelError::TooLate(_))));
        outbox.set_state(&e.id, State::Sent).unwrap();
        assert!(matches!(outbox.cancel(&e.id), Err(CancelError::TooLate(_))));
        assert_eq!(outbox.raw(&e.id).unwrap(), b"m");
    }

    #[test]
    fn recover_never_resends_a_sending_message() {
        let (_dir, outbox) = outbox();
        let (sending, waiting) = (entry(10), entry(10));
        outbox.add(b"s", &sending).unwrap();
        outbox.add(b"w", &waiting).unwrap();
        outbox.set_state(&sending.id, State::Sending).unwrap();
        outbox.recover().unwrap();
        assert_eq!(
            outbox.get(&sending.id).unwrap().state,
            State::Failed {
                reason: MAY_HAVE_BEEN_SENT.into(),
                retrying: false
            }
        );
        assert_eq!(outbox.get(&waiting.id).unwrap().state, State::Waiting);
    }

    #[test]
    fn a_json_without_its_eml_is_listed_as_failed() {
        let (dir, outbox) = outbox();
        let e = entry(10);
        outbox.add(b"m", &e).unwrap();
        fs::remove_file(dir.path().join(format!("state/outbox/work/{}.eml", e.id))).unwrap();
        let listed = outbox.list().unwrap();
        assert!(matches!(listed[0].state, State::Failed { .. }));
        // Junk beside the entries is left alone.
        fs::write(dir.path().join("state/outbox/work/notes.txt"), "x").unwrap();
        fs::write(dir.path().join("state/outbox/work/abcd.json"), "not json").unwrap();
        assert_eq!(outbox.list().unwrap().len(), 1);
    }

    #[test]
    fn ids_from_outside_never_leave_the_folder() {
        let (_dir, outbox) = outbox();
        for id in ["../../config/config", "", "a/b", "zz"] {
            assert!(outbox.raw(id).is_err(), "{id}");
            assert!(
                matches!(outbox.cancel(id), Err(CancelError::NotFound(_))),
                "{id}"
            );
        }
    }

    #[test]
    fn finished_needs_the_copy_and_the_flag_only_when_wanted() {
        let mut e = entry(0);
        assert!(!e.finished());
        e.copied = true;
        assert!(!e.finished());
        e.flagged = true;
        assert!(e.finished());
        let bare = Entry {
            save_sent: false,
            original: None,
            ..entry(0)
        };
        assert!(bare.finished());
    }
}
