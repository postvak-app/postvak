pub mod imap;

use std::sync::atomic::AtomicBool;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
pub struct RemoteFolder {
    pub name: String,
    pub special_use: Option<String>,
    pub delimiter: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SelectInfo {
    pub uidvalidity: u32,
    /// Messages in the folder (EXISTS).
    pub exists: u32,
    /// HIGHESTMODSEQ when the server supports CONDSTORE for this folder.
    pub highest_modseq: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Envelope {
    pub uid: u32,
    pub flags: Vec<String>,
    pub internaldate: i64,
    pub size: Option<u32>,
    pub headers: Vec<u8>,
}

/// A message's shape, as far as the indexer needs it: where its text is and what its attachments are called.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Structure {
    pub text_parts: Vec<TextPart>,
    pub attachment_names: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TextPart {
    /// The IMAP section, e.g. "1" or "1.2"; "" for a message that is one text part, fetched as `BODY[TEXT]`.
    pub section: String,
    pub html: bool,
    pub charset: Option<String>,
    /// The Content-Transfer-Encoding as the server reports it, e.g. "base64"; None for 7bit.
    pub transfer_encoding: Option<String>,
    pub octets: u32,
    /// The `multipart/alternative` this part belongs to, numbered per message, so the caller keeps one part of each.
    pub alternative: Option<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FlagUpdate {
    pub uid: u32,
    pub flags: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdleOutcome {
    NewMail,
    Timeout,
    Interrupted,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum MailError {
    #[error("could not connect: {0}")]
    Connect(String),
    #[error("login failed: {0}")]
    Auth(String),
    #[error("server error: {0}")]
    Protocol(String),
    #[error("connection lost: {0}")]
    Io(String),
}

pub type MailResult<T> = Result<T, MailError>;

pub trait MailOps {
    fn list_folders(&mut self) -> MailResult<Vec<RemoteFolder>>;
    fn select(&mut self, folder: &str) -> MailResult<SelectInfo>;
    /// Uids >= `from_uid` in the selected folder, ascending.
    fn search_uids(&mut self, from_uid: u32) -> MailResult<Vec<u32>>;
    /// Uids in the selected folder whose headers or body contain every word, ignoring case, ascending.
    fn search_text(&mut self, words: &[String]) -> MailResult<Vec<u32>>;
    /// Envelopes with `first <= uid <= last` in the selected folder.
    fn fetch_envelopes(&mut self, first: u32, last: u32) -> MailResult<Vec<Envelope>>;
    /// Current flags for uids <= `upto_uid` in the selected folder; with `changed_since`, only those whose modseq is above it.
    fn fetch_flags(
        &mut self,
        upto_uid: u32,
        changed_since: Option<u64>,
    ) -> MailResult<Vec<FlagUpdate>>;
    fn fetch_raw(&mut self, uid: u32) -> MailResult<Option<Vec<u8>>>;
    /// The message's structure from `BODYSTRUCTURE`; `None` when the selected folder has no such uid.
    fn fetch_structure(&mut self, uid: u32) -> MailResult<Option<Structure>>;
    /// The bytes of one section as sent, transfer encoding included; `None` when the server returns nothing for it.
    fn fetch_part(&mut self, uid: u32, section: &str) -> MailResult<Option<Vec<u8>>>;
    fn add_flags(&mut self, uid: u32, flags: &[&str]) -> MailResult<()>;
    fn remove_flags(&mut self, uid: u32, flags: &[&str]) -> MailResult<()>;
    fn expunge(&mut self, uid: u32) -> MailResult<()>;
    /// Moves the message. Its new uid is not known here; the next sync of `to` finds it.
    fn move_message(&mut self, uid: u32, to: &str) -> MailResult<()>;
    fn create_folder(&mut self, name: &str) -> MailResult<()>;
    /// Appends a message carrying `flags`, e.g. a keyword.
    fn append(&mut self, folder: &str, raw: &[u8], flags: &[&str]) -> MailResult<()>;
    /// `add_flags` for many messages; a server does it in one command per batch of uids.
    fn add_flags_to(&mut self, uids: &[u32], flags: &[&str]) -> MailResult<()> {
        uids.iter().try_for_each(|&uid| self.add_flags(uid, flags))
    }
    /// `remove_flags` for many messages; a server does it in one command per batch of uids.
    fn remove_flags_from(&mut self, uids: &[u32], flags: &[&str]) -> MailResult<()> {
        uids.iter()
            .try_for_each(|&uid| self.remove_flags(uid, flags))
    }
    /// `expunge` for many messages; a server does it in one command per batch of uids.
    fn expunge_all(&mut self, uids: &[u32]) -> MailResult<()> {
        uids.iter().try_for_each(|&uid| self.expunge(uid))
    }
    /// `move_message` for many messages; a server does it in one command per batch of uids.
    fn move_messages(&mut self, uids: &[u32], to: &str) -> MailResult<()> {
        uids.iter().try_for_each(|&uid| self.move_message(uid, to))
    }
    /// Waits for new mail in the selected folder, the timeout, or `interrupt` becoming true.
    fn idle(&mut self, timeout: Duration, interrupt: &AtomicBool) -> MailResult<IdleOutcome>;
}

#[cfg(any(test, feature = "testing"))]
pub use recording::RecordingOps;

#[cfg(any(test, feature = "testing"))]
mod recording {
    use std::collections::{HashMap, VecDeque};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::time::Duration;

    use super::*;

    #[derive(Debug, Default)]
    pub struct RecordingOps {
        pub folders: Vec<RemoteFolder>,
        pub uidvalidity: HashMap<String, u32>,
        pub mail: HashMap<String, Vec<Envelope>>,
        pub raw: HashMap<(String, u32), Vec<u8>>,
        /// A message's structure; without one, a message with `raw` is a single plain-text part.
        pub structures: HashMap<(String, u32), Structure>,
        /// The bytes of a section; without one, section "" is `raw` after its headers.
        pub parts: HashMap<(String, u32, String), Vec<u8>>,
        /// Answers every `fetch_structure` with None, like a server whose BODYSTRUCTURE cannot be read.
        pub no_structure: bool,
        pub calls: Vec<String>,
        pub idle_outcomes: VecDeque<IdleOutcome>,
        /// Makes `fetch_envelopes` fail once this many calls succeeded, like a connection dropping mid-sync.
        pub fail_fetch_after: Option<usize>,
        /// Makes the next `add_flags`, `append`, `fetch_raw`, `fetch_structure` or `fetch_part` fail with this error, once.
        pub fail_next: Option<MailError>,
        /// Makes the next `expunge` fail with this error, once, like a server refusing it after a COPY.
        pub fail_next_expunge: Option<MailError>,
        /// Makes `add_flags` panic, like a bug in a command.
        pub panic_on_add_flags: bool,
        /// With no queued outcome, `idle` sets this flag and returns instead of waiting for a wake-up.
        pub shutdown_when_idle_empty: Option<Arc<AtomicBool>>,
        /// Listing folders sets this flag, like a stop arriving in the middle of a pass.
        pub shutdown_on_list_folders: Option<Arc<AtomicBool>>,
        /// A non-zero value becomes INBOX's uidvalidity at its next select; a test can set it while the fake is borrowed.
        pub next_inbox_uidvalidity: Option<Arc<AtomicU32>>,
        /// Reports no HIGHESTMODSEQ and ignores CHANGEDSINCE, unlike a CONDSTORE server.
        pub no_condstore: bool,
        /// Per message the flags last seen and the modseq they got, so tests may edit `mail` directly.
        modseqs: HashMap<(String, u32), (Vec<String>, u64)>,
        highest_modseq: HashMap<String, u64>,
        envelope_fetches: usize,
        selected: String,
        next_uid: HashMap<String, u32>,
    }

    impl RecordingOps {
        pub fn with_folder(mut self, name: &str, special_use: Option<&str>) -> RecordingOps {
            self.folders.push(RemoteFolder {
                name: name.into(),
                special_use: special_use.map(str::to_string),
                delimiter: None,
            });
            self.uidvalidity.insert(name.into(), 1);
            self.mail.entry(name.into()).or_default();
            self
        }

        pub fn add_mail(
            &mut self,
            folder: &str,
            uid: u32,
            internaldate: i64,
            headers: &str,
            raw: Option<&str>,
        ) {
            self.mail.entry(folder.into()).or_default().push(Envelope {
                uid,
                flags: vec![],
                internaldate,
                size: raw.map(|r| r.len() as u32),
                headers: headers.as_bytes().to_vec(),
            });
            if let Some(raw) = raw {
                self.raw
                    .insert((folder.into(), uid), raw.as_bytes().to_vec());
            }
            let next = self.next_uid.entry(folder.into()).or_insert(1);
            *next = (*next).max(uid + 1);
        }

        /// Gives every message whose flags changed since last looked at a new modseq; returns the folder's highest.
        fn refresh_modseqs(&mut self, folder: &str) -> u64 {
            let highest = self.highest_modseq.entry(folder.into()).or_insert(1);
            for env in self.mail.get(folder).into_iter().flatten() {
                let key = (folder.to_string(), env.uid);
                if self
                    .modseqs
                    .get(&key)
                    .is_none_or(|(flags, _)| *flags != env.flags)
                {
                    *highest += 1;
                    self.modseqs.insert(key, (env.flags.clone(), *highest));
                }
            }
            *highest
        }

        /// Runs a batch command as the single ones it stands for, recorded as one call, as a server gets it.
        fn batch(
            &mut self,
            record: String,
            run: impl FnOnce(&mut RecordingOps) -> MailResult<()>,
        ) -> MailResult<()> {
            let before = self.calls.len();
            let result = run(self);
            self.calls.truncate(before);
            self.calls.push(record);
            result
        }

        fn envelope_mut(&mut self, uid: u32) -> MailResult<&mut Envelope> {
            let folder = self.selected.clone();
            self.mail
                .get_mut(&folder)
                .and_then(|list| list.iter_mut().find(|e| e.uid == uid))
                .ok_or_else(|| MailError::Protocol(format!("no uid {uid} in {folder}")))
        }
    }

    /// The body of a raw message: what follows the first blank line.
    /// Uids as a recorded call lists them: `3` for one, `1,2,5` for many.
    fn uid_list(uids: &[u32]) -> String {
        uids.iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(",")
    }

    fn body_after_headers(raw: &[u8]) -> &[u8] {
        raw.windows(4)
            .position(|w| w == b"\r\n\r\n")
            .map(|at| &raw[at + 4..])
            .or_else(|| {
                raw.windows(2)
                    .position(|w| w == b"\n\n")
                    .map(|at| &raw[at + 2..])
            })
            .unwrap_or(&[])
    }

    impl MailOps for RecordingOps {
        fn list_folders(&mut self) -> MailResult<Vec<RemoteFolder>> {
            self.calls.push("list_folders".into());
            if let Some(shutdown) = &self.shutdown_on_list_folders {
                shutdown.store(true, Ordering::Release);
            }
            Ok(self.folders.clone())
        }

        fn select(&mut self, folder: &str) -> MailResult<SelectInfo> {
            self.calls.push(format!("select {folder}"));
            if folder == "INBOX"
                && let Some(next) = &self.next_inbox_uidvalidity
            {
                match next.swap(0, Ordering::AcqRel) {
                    0 => {}
                    changed => {
                        self.uidvalidity.insert(folder.into(), changed);
                    }
                }
            }
            let uidvalidity = *self
                .uidvalidity
                .get(folder)
                .ok_or_else(|| MailError::Protocol(format!("no folder {folder}")))?;
            self.selected = folder.to_string();
            let highest_modseq = self.refresh_modseqs(folder);
            Ok(SelectInfo {
                uidvalidity,
                exists: self.mail.get(folder).map_or(0, |list| list.len() as u32),
                highest_modseq: (!self.no_condstore).then_some(highest_modseq),
            })
        }

        fn search_uids(&mut self, from_uid: u32) -> MailResult<Vec<u32>> {
            self.calls
                .push(format!("search_uids {} {from_uid}", self.selected));
            let mut uids: Vec<u32> = self
                .mail
                .get(&self.selected)
                .map(|list| list.iter().map(|e| e.uid).collect())
                .unwrap_or_default();
            uids.sort_unstable();
            let max = uids.last().copied().unwrap_or(0);
            // Real servers answer `N:*` with the highest message when N exceeds it.
            if from_uid > max {
                return Ok(uids.into_iter().filter(|&uid| uid == max).collect());
            }
            Ok(uids.into_iter().filter(|&uid| uid >= from_uid).collect())
        }

        fn search_text(&mut self, words: &[String]) -> MailResult<Vec<u32>> {
            self.calls
                .push(format!("search_text {} {}", self.selected, words.join(" ")));
            let folder = self.selected.clone();
            let text = |e: &Envelope| {
                let raw = self.raw.get(&(folder.clone(), e.uid)).unwrap_or(&e.headers);
                String::from_utf8_lossy(raw).to_lowercase()
            };
            let mut uids: Vec<u32> = self
                .mail
                .get(&folder)
                .into_iter()
                .flatten()
                .filter(|e| words.iter().all(|w| text(e).contains(&w.to_lowercase())))
                .map(|e| e.uid)
                .collect();
            uids.sort_unstable();
            Ok(uids)
        }

        fn fetch_envelopes(&mut self, first: u32, last: u32) -> MailResult<Vec<Envelope>> {
            self.calls
                .push(format!("fetch_envelopes {} {first} {last}", self.selected));
            if self
                .fail_fetch_after
                .is_some_and(|n| self.envelope_fetches >= n)
            {
                return Err(MailError::Io("fetch_envelopes failed".into()));
            }
            self.envelope_fetches += 1;
            let mut out: Vec<Envelope> = self
                .mail
                .get(&self.selected)
                .map(|list| {
                    list.iter()
                        .filter(|e| (first..=last).contains(&e.uid))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            out.sort_by_key(|e| e.uid);
            Ok(out)
        }

        fn fetch_flags(
            &mut self,
            upto_uid: u32,
            changed_since: Option<u64>,
        ) -> MailResult<Vec<FlagUpdate>> {
            let since = changed_since
                .map(|m| format!(" since {m}"))
                .unwrap_or_default();
            self.calls
                .push(format!("fetch_flags {} {upto_uid}{since}", self.selected));
            let folder = self.selected.clone();
            self.refresh_modseqs(&folder);
            let changed = |uid: u32| {
                changed_since.is_none_or(|since| {
                    self.modseqs
                        .get(&(folder.clone(), uid))
                        .is_some_and(|(_, modseq)| *modseq > since)
                })
            };
            Ok(self
                .mail
                .get(&folder)
                .map(|l| {
                    l.iter()
                        .filter(|e| e.uid <= upto_uid && changed(e.uid))
                        .map(|e| FlagUpdate {
                            uid: e.uid,
                            flags: e.flags.clone(),
                        })
                        .collect()
                })
                .unwrap_or_default())
        }

        fn fetch_raw(&mut self, uid: u32) -> MailResult<Option<Vec<u8>>> {
            self.calls
                .push(format!("fetch_raw {} {uid}", self.selected));
            self.fail_next.take().map_or(Ok(()), Err)?;
            Ok(self.raw.get(&(self.selected.clone(), uid)).cloned())
        }

        fn fetch_structure(&mut self, uid: u32) -> MailResult<Option<Structure>> {
            self.calls
                .push(format!("fetch_structure {} {uid}", self.selected));
            self.fail_next.take().map_or(Ok(()), Err)?;
            if self.no_structure {
                return Ok(None);
            }
            let key = (self.selected.clone(), uid);
            if let Some(structure) = self.structures.get(&key) {
                return Ok(Some(structure.clone()));
            }
            Ok(self.raw.get(&key).map(|raw| Structure {
                text_parts: vec![TextPart {
                    octets: body_after_headers(raw).len() as u32,
                    ..TextPart::default()
                }],
                attachment_names: Vec::new(),
            }))
        }

        fn fetch_part(&mut self, uid: u32, section: &str) -> MailResult<Option<Vec<u8>>> {
            self.calls
                .push(format!("fetch_part {} {uid} {section}", self.selected));
            self.fail_next.take().map_or(Ok(()), Err)?;
            let key = (self.selected.clone(), uid, section.to_string());
            if let Some(part) = self.parts.get(&key) {
                return Ok(Some(part.clone()));
            }
            if !section.is_empty() {
                return Ok(None);
            }
            Ok(self
                .raw
                .get(&(self.selected.clone(), uid))
                .map(|raw| body_after_headers(raw).to_vec()))
        }

        fn add_flags(&mut self, uid: u32, flags: &[&str]) -> MailResult<()> {
            self.calls.push(format!(
                "add_flags {} {uid} {}",
                self.selected,
                flags.join(" ")
            ));
            assert!(!self.panic_on_add_flags, "add_flags panicked");
            self.fail_next.take().map_or(Ok(()), Err)?;
            let env = self.envelope_mut(uid)?;
            for f in flags {
                if !env.flags.iter().any(|x| x == f) {
                    env.flags.push(f.to_string());
                }
            }
            // A server bumps the modseq on every STORE, so a later edit that restores the old flags still shows.
            let folder = self.selected.clone();
            self.refresh_modseqs(&folder);
            Ok(())
        }

        fn remove_flags(&mut self, uid: u32, flags: &[&str]) -> MailResult<()> {
            self.calls.push(format!(
                "remove_flags {} {uid} {}",
                self.selected,
                flags.join(" ")
            ));
            let env = self.envelope_mut(uid)?;
            env.flags.retain(|f| !flags.contains(&f.as_str()));
            let folder = self.selected.clone();
            self.refresh_modseqs(&folder);
            Ok(())
        }

        fn expunge(&mut self, uid: u32) -> MailResult<()> {
            self.calls.push(format!("expunge {} {uid}", self.selected));
            self.fail_next_expunge.take().map_or(Ok(()), Err)?;
            let folder = self.selected.clone();
            let is_deleted = self.mail.get(&folder).is_some_and(|list| {
                list.iter()
                    .any(|e| e.uid == uid && e.flags.iter().any(|f| f == "\\Deleted"))
            });
            if is_deleted {
                if let Some(list) = self.mail.get_mut(&folder) {
                    list.retain(|e| e.uid != uid);
                }
                self.raw.remove(&(folder, uid));
            }
            Ok(())
        }

        fn move_message(&mut self, uid: u32, to: &str) -> MailResult<()> {
            self.calls
                .push(format!("move {} {uid} -> {to}", self.selected));
            if !self.mail.contains_key(to) {
                return Err(MailError::Protocol(format!("no folder {to}")));
            }
            let folder = self.selected.clone();
            let mut env = self.envelope_mut(uid)?.clone();
            if let Some(list) = self.mail.get_mut(&folder) {
                list.retain(|e| e.uid != uid);
            }
            let raw = self.raw.remove(&(folder, uid));
            let next = self.next_uid.entry(to.into()).or_insert(1);
            let new_uid = *next;
            *next += 1;
            env.uid = new_uid;
            // `to` exists: checked above.
            self.mail.entry(to.into()).or_default().push(env);
            if let Some(raw) = raw {
                self.raw.insert((to.into(), new_uid), raw);
            }
            Ok(())
        }

        fn create_folder(&mut self, name: &str) -> MailResult<()> {
            self.calls.push(format!("create_folder {name}"));
            if self.mail.contains_key(name) {
                return Err(MailError::Protocol(format!("[ALREADYEXISTS] {name}")));
            }
            self.folders.push(RemoteFolder {
                name: name.into(),
                special_use: None,
                delimiter: None,
            });
            self.uidvalidity.insert(name.into(), 1);
            self.mail.entry(name.into()).or_default();
            Ok(())
        }

        fn append(&mut self, folder: &str, raw: &[u8], flags: &[&str]) -> MailResult<()> {
            self.calls
                .push(format!("append {folder} {} bytes", raw.len()));
            self.fail_next.take().map_or(Ok(()), Err)?;
            let Some(list) = self.mail.get_mut(folder) else {
                return Err(MailError::Protocol(format!("no folder {folder}")));
            };
            let uid = *self.next_uid.get(folder).unwrap_or(&1);
            let end = raw
                .windows(4)
                .position(|w| w == b"\r\n\r\n")
                .map(|i| i + 4)
                .unwrap_or(raw.len());
            list.push(Envelope {
                uid,
                flags: flags.iter().map(|f| f.to_string()).collect(),
                internaldate: 0,
                size: Some(raw.len() as u32),
                headers: raw[..end].to_vec(),
            });
            self.raw.insert((folder.into(), uid), raw.to_vec());
            self.next_uid.insert(folder.into(), uid + 1);
            Ok(())
        }

        fn add_flags_to(&mut self, uids: &[u32], flags: &[&str]) -> MailResult<()> {
            let record = format!(
                "add_flags {} {} {}",
                self.selected,
                uid_list(uids),
                flags.join(" ")
            );
            self.batch(record, |ops| {
                uids.iter().try_for_each(|&uid| ops.add_flags(uid, flags))
            })
        }

        fn remove_flags_from(&mut self, uids: &[u32], flags: &[&str]) -> MailResult<()> {
            let record = format!(
                "remove_flags {} {} {}",
                self.selected,
                uid_list(uids),
                flags.join(" ")
            );
            self.batch(record, |ops| {
                uids.iter()
                    .try_for_each(|&uid| ops.remove_flags(uid, flags))
            })
        }

        fn expunge_all(&mut self, uids: &[u32]) -> MailResult<()> {
            let record = format!("expunge {} {}", self.selected, uid_list(uids));
            self.batch(record, |ops| {
                uids.iter().try_for_each(|&uid| ops.expunge(uid))
            })
        }

        fn move_messages(&mut self, uids: &[u32], to: &str) -> MailResult<()> {
            let record = format!("move {} {} -> {to}", self.selected, uid_list(uids));
            self.batch(record, |ops| {
                uids.iter().try_for_each(|&uid| ops.move_message(uid, to))
            })
        }

        fn idle(&mut self, timeout: Duration, interrupt: &AtomicBool) -> MailResult<IdleOutcome> {
            self.calls.push(format!("idle {}", self.selected));
            if interrupt.load(Ordering::Acquire) {
                return Ok(IdleOutcome::Interrupted);
            }
            if let Some(outcome) = self.idle_outcomes.pop_front() {
                return Ok(outcome);
            }
            if let Some(shutdown) = &self.shutdown_when_idle_empty {
                shutdown.store(true, Ordering::Relaxed);
                return Ok(IdleOutcome::Interrupted);
            }
            // Like a real server: wait until someone wakes the session, or the timeout passes.
            let deadline = std::time::Instant::now() + timeout;
            while !interrupt.load(Ordering::Acquire) {
                if std::time::Instant::now() >= deadline {
                    return Ok(IdleOutcome::Timeout);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(IdleOutcome::Interrupted)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_search_uids_mimics_star_semantics() {
        let mut ops = RecordingOps::default().with_folder("INBOX", None);
        ops.add_mail("INBOX", 3, 10, "Subject: a\r\n\r\n", None);
        ops.add_mail("INBOX", 9, 20, "Subject: b\r\n\r\n", None);
        ops.select("INBOX").unwrap();
        assert_eq!(ops.search_uids(1).unwrap(), [3, 9]);
        assert_eq!(ops.search_uids(4).unwrap(), [9]);
        // Real servers answer `N:*` with the highest message when N exceeds it.
        assert_eq!(ops.search_uids(50).unwrap(), [9]);
        assert_eq!(ops.calls.last().unwrap(), "search_uids INBOX 50");
    }

    #[test]
    fn fake_fetch_envelopes_returns_the_range_and_can_fail_later() {
        let mut ops = RecordingOps::default().with_folder("INBOX", None);
        for uid in [1, 5, 9] {
            ops.add_mail("INBOX", uid, 10, "Subject: a\r\n\r\n", None);
        }
        ops.select("INBOX").unwrap();
        ops.fail_fetch_after = Some(1);
        let uids: Vec<u32> = ops
            .fetch_envelopes(2, 9)
            .unwrap()
            .iter()
            .map(|e| e.uid)
            .collect();
        assert_eq!(uids, [5, 9]);
        assert_eq!(ops.calls.last().unwrap(), "fetch_envelopes INBOX 2 9");
        assert!(matches!(ops.fetch_envelopes(1, 1), Err(MailError::Io(_))));
    }

    #[test]
    fn fake_move_moves_the_message_and_records_call() {
        let mut ops = RecordingOps::default()
            .with_folder("INBOX", None)
            .with_folder("Archive", Some("Archive"));
        ops.add_mail("INBOX", 1, 10, "Subject: a\r\n\r\n", Some("raw"));
        ops.select("INBOX").unwrap();
        ops.move_message(1, "Archive").unwrap();
        assert!(ops.mail["INBOX"].is_empty());
        assert_eq!(ops.raw.get(&("Archive".into(), 1)).unwrap(), b"raw");
        assert_eq!(ops.calls.last().unwrap(), "move INBOX 1 -> Archive");
    }

    #[test]
    fn fake_store_bumps_the_modseq_even_when_a_later_edit_restores_the_flags() {
        let mut ops = RecordingOps::default().with_folder("INBOX", None);
        ops.add_mail("INBOX", 1, 10, "Subject: a\r\n\r\n", None);
        let before = ops.select("INBOX").unwrap().highest_modseq.unwrap();
        ops.add_flags(1, &["\\Seen"]).unwrap();
        ops.mail.get_mut("INBOX").unwrap()[0].flags.clear();
        ops.select("INBOX").unwrap();
        let changed: Vec<u32> = ops
            .fetch_flags(1, Some(before))
            .unwrap()
            .iter()
            .map(|u| u.uid)
            .collect();
        assert_eq!(changed, [1]);
    }

    #[test]
    fn fake_expunge_requires_deleted_flag() {
        let mut ops = RecordingOps::default().with_folder("INBOX", None);
        ops.add_mail("INBOX", 1, 10, "Subject: a\r\n\r\n", Some("raw"));
        ops.select("INBOX").unwrap();
        ops.expunge(1).unwrap();
        assert_eq!(ops.mail["INBOX"].len(), 1);
        assert!(ops.raw.contains_key(&("INBOX".into(), 1)));
        ops.add_flags(1, &["\\Deleted"]).unwrap();
        ops.expunge(1).unwrap();
        assert!(ops.mail["INBOX"].is_empty());
        assert!(ops.raw.is_empty());
        assert_eq!(ops.calls.last().unwrap(), "expunge INBOX 1");
    }

    #[test]
    fn fake_append_keeps_bytes_and_rejects_unknown_folder() {
        let mut ops = RecordingOps::default().with_folder("Backup", None);
        let raw = b"Subject: a\r\n\r\n\xFFbody";
        ops.append("Backup", raw, &["$PostvakRestored"]).unwrap();
        assert_eq!(ops.raw[&("Backup".into(), 1)], raw);
        assert_eq!(ops.mail["Backup"][0].headers, b"Subject: a\r\n\r\n");
        assert_eq!(ops.mail["Backup"][0].flags, ["$PostvakRestored"]);
        assert!(matches!(
            ops.append("Nope", raw, &[]),
            Err(MailError::Protocol(_))
        ));
    }
}
