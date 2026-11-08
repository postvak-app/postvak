//! The composer without a toolkit: its fields, checks, questions and what it asks the daemon to send.

use std::io;
use std::path::{Path, PathBuf};

use postvak_core::config::Identity;
use postvak_core::paths::{Paths, write_atomic};
use postvak_core::send::build::{bare_address, mailbox_text};
use postvak_core::send::check::{CheckError, DEFAULT_SIZE_LIMIT, check_from, valid_address};
use postvak_core::send::compose::{Body, HeaderEdits, Prefilled, SendKind, SendRequest};
use postvak_core::send::drafts::{DraftKind, OpenedDraft, bracketed};
use postvak_core::send::edit::split_recipients;
use postvak_core::send::mailto::Mailto;
use postvak_core::send::reply::{self, ForwardAs, ReplyTo};
use postvak_core::store::KnownAddress;
use serde::{Deserialize, Serialize};

/// Send's hover hint, with its key.
pub(crate) fn send_hint() -> &'static str {
    if cfg!(target_os = "macos") {
        "Send · Cmd+Enter"
    } else {
        "Send · Ctrl+Enter"
    }
}

/// Save draft's hover hint, with its key.
pub(crate) fn save_hint() -> &'static str {
    if cfg!(target_os = "macos") {
        "Save draft · Cmd+S"
    } else {
        "Save draft · Ctrl+S"
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum ComposeKind {
    New,
    Reply(ReplyTo),
    Forward,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Field {
    Attachments,
    Bcc,
    Cc,
    From,
    /// A line above the fields that is no error, such as the one a restored composer shows.
    Note,
    Send,
    Subject,
    To,
}

/// The message a reply or forward answers; `raw` is empty until the prefill arrives, and in a recovery file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct OriginalRef {
    pub folder: String,
    pub uid: u32,
    #[serde(skip)]
    pub raw: Vec<u8>,
}

/// The composer's copy in the account's Drafts: its Message-ID, and earlier copies a save could not delete, each `<id>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ComposeDraft {
    pub message_id: String,
    pub left: Vec<String>,
}

impl ComposeDraft {
    /// Every copy in Drafts, which the next save, a send or a discard deletes.
    pub(crate) fn ids(&self) -> Vec<String> {
        std::iter::once(self.message_id.clone())
            .chain(self.left.iter().cloned())
            .collect()
    }
}

/// The fields as the composer opened, to tell whether the user changed anything.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Snapshot {
    bcc: String,
    cc: String,
    files: Vec<(PathBuf, u64)>,
    forward_as_attachment: bool,
    from: String,
    kept_parts: Vec<bool>,
    subject: String,
    text: String,
    to: String,
}

/// What the window edits as text: the address fields, From, Subject and the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ComposeField {
    Bcc,
    Cc,
    From,
    Subject,
    Text,
    To,
}

/// A composer's recovery file holds all of it but `loading`, `error` and the original's raw bytes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Composer {
    pub id: u64,
    pub account: String,
    pub kind: ComposeKind,
    pub original: Option<OriginalRef>,
    pub from: String,
    pub from_choices: Vec<String>,
    pub to: String,
    pub cc: String,
    pub bcc: String,
    pub show_cc_bcc: bool,
    pub subject: String,
    pub text: String,
    /// Each attached file with its size, read once as it was added.
    pub files: Vec<(PathBuf, u64)>,
    /// The original's attachments as (name, size, whether an inline forward takes it along).
    pub forward_parts: Vec<(String, u64, bool)>,
    pub forward_as_attachment: bool,
    /// The threading headers the prefill gave, kept for a reply that goes on as new mail when its original is gone.
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    #[serde(skip)]
    pub loading: bool,
    #[serde(default)]
    pub sending: bool,
    /// Restored from a file written while it was being sent, so its first Send asks before sending it again.
    #[serde(default)]
    pub may_have_gone: bool,
    #[serde(skip)]
    pub error: Option<(Field, String)>,
    #[serde(default)]
    pub draft: Option<ComposeDraft>,
    #[serde(skip)]
    pub saving: bool,
    /// The largest message the account's server takes, in bytes.
    pub size_limit: u64,
    opened_as: Snapshot,
}

fn join_addresses(addresses: &[String]) -> String {
    addresses.join(", ")
}

/// What tells the size error apart from the attachments' other errors.
const OVER_LIMIT: &str = " attached; the limit is ";

/// Puts back what the user typed into a field while the composer was loading, when it differs from `opened`.
fn keep_typed(field: &mut String, opened: &str, typed: String) {
    if typed != opened {
        *field = typed;
    }
}

fn size_in_words(bytes: u64) -> String {
    crate::body::size(usize::try_from(bytes).unwrap_or(usize::MAX))
}

fn choices_of(identity: &Identity) -> Vec<String> {
    std::iter::once(identity.address().to_string())
        .chain(
            identity
                .alias_patterns()
                .iter()
                .filter(|alias| !alias.contains('*'))
                .cloned(),
        )
        .collect()
}

impl Composer {
    fn empty(id: u64, account: &str, kind: ComposeKind) -> Self {
        Composer {
            id,
            account: account.to_string(),
            kind,
            original: None,
            from: String::new(),
            from_choices: Vec::new(),
            to: String::new(),
            cc: String::new(),
            bcc: String::new(),
            show_cc_bcc: false,
            subject: String::new(),
            text: String::new(),
            files: Vec::new(),
            forward_parts: Vec::new(),
            forward_as_attachment: false,
            in_reply_to: None,
            references: Vec::new(),
            loading: false,
            sending: false,
            may_have_gone: false,
            error: None,
            draft: None,
            saving: false,
            size_limit: DEFAULT_SIZE_LIMIT as u64,
            opened_as: Snapshot::default(),
        }
    }

    fn answering(id: u64, account: &str, kind: ComposeKind, folder: &str, uid: u32) -> Self {
        Composer {
            original: Some(OriginalRef {
                folder: folder.to_string(),
                uid,
                raw: Vec::new(),
            }),
            loading: true,
            ..Self::empty(id, account, kind)
        }
    }

    pub(crate) fn reply(id: u64, account: &str, folder: &str, uid: u32, to: ReplyTo) -> Self {
        Self::answering(id, account, ComposeKind::Reply(to), folder, uid)
    }

    pub(crate) fn forward(id: u64, account: &str, folder: &str, uid: u32) -> Self {
        Self::answering(id, account, ComposeKind::Forward, folder, uid)
    }

    pub(crate) fn new_mail(
        id: u64,
        account: &str,
        accounts: &[(String, Identity)],
        mailto: Option<Mailto>,
    ) -> Self {
        let mut composer = Self::empty(id, account, ComposeKind::New);
        composer.from = accounts
            .iter()
            .find(|(name, _)| name == account)
            .map(|(_, identity)| identity.address().to_string())
            .unwrap_or_default();
        composer.from_choices = accounts
            .iter()
            .flat_map(|(name, identity)| {
                choices_of(identity)
                    .into_iter()
                    .map(move |address| format!("{name}\t{address}"))
            })
            .collect();
        if let Some(mailto) = mailto {
            composer.show_cc_bcc = !mailto.cc.is_empty() || !mailto.bcc.is_empty();
            composer.to = join_addresses(&mailto.to);
            composer.cc = join_addresses(&mailto.cc);
            composer.bcc = join_addresses(&mailto.bcc);
            composer.subject = mailto.subject;
            composer.text = mailto.body;
        }
        composer.opened_as = composer.snapshot();
        composer
    }

    /// A draft read back from Drafts: a Postvak reply or forward over its original at `original_at`, other drafts as
    /// new mail, over the message they answer when `original_at` names one. Its own files are written to `files_dir`.
    pub(crate) fn from_draft(
        id: u64,
        account: &str,
        identities: &[(String, Identity)],
        opened: OpenedDraft,
        original_at: Option<(String, u32)>,
        files_dir: &Path,
    ) -> io::Result<Self> {
        let kind = match opened.kind {
            DraftKind::New => ComposeKind::New,
            DraftKind::Reply(to) => ComposeKind::Reply(to),
            DraftKind::Forward(_) => ComposeKind::Forward,
        };
        let mut composer = Self::empty(id, account, kind);
        composer.original = original_at.map(|(folder, uid)| OriginalRef {
            folder,
            uid,
            raw: opened.original.clone().unwrap_or_default(),
        });
        if kind == ComposeKind::Forward {
            let raw = opened.original.as_deref().unwrap_or_default();
            composer.forward_parts = reply::forward_attachments(raw)
                .into_iter()
                .enumerate()
                .map(|(index, (name, size))| {
                    let kept = opened
                        .forward_parts
                        .as_ref()
                        .is_none_or(|parts| parts.contains(&index));
                    (name, size, kept)
                })
                .collect();
        }
        composer.forward_as_attachment = opened.kind == DraftKind::Forward(ForwardAs::Attachment);
        composer.from = opened.from;
        composer.from_choices = (identities.iter())
            .find(|(name, _)| name == account)
            .map(|(_, identity)| choices_of(identity))
            .unwrap_or_default();
        composer.show_cc_bcc = !opened.cc.is_empty() || !opened.bcc.is_empty();
        composer.to = join_addresses(&opened.to);
        composer.cc = join_addresses(&opened.cc);
        composer.bcc = join_addresses(&opened.bcc);
        composer.subject = opened.subject;
        composer.text = opened.text;
        composer.in_reply_to = opened.in_reply_to;
        composer.references = opened.references;
        composer.draft = bracketed(&opened.message_id).map(|message_id| ComposeDraft {
            message_id,
            left: Vec::new(),
        });
        let notes: Vec<&str> = (opened.note.iter().copied())
            .chain(composer.draft.is_none().then_some(NO_MESSAGE_ID))
            .collect();
        composer.error = (!notes.is_empty()).then(|| (Field::Note, notes.join(" ")));
        // A folder per file, so two files of one name both keep it.
        for (index, (name, bytes)) in opened.files.into_iter().enumerate() {
            let dir = files_dir.join(index.to_string());
            std::fs::create_dir_all(&dir)?;
            let path = dir.join(safe_name(&name));
            std::fs::write(&path, &bytes)?;
            composer.files.push((path, bytes.len() as u64));
        }
        composer.opened_as = composer.snapshot();
        Ok(composer)
    }

    /// Takes the prefill's fields but not its text: that holds the quote, which `compose` adds under the user's text.
    pub(crate) fn fill(&mut self, prefilled: Prefilled, identity: &Identity) {
        let draft = prefilled.draft;
        if let Some(original) = self.original.as_mut() {
            original.raw = prefilled.original.unwrap_or_default();
        }
        let typed = self.snapshot();
        self.from = draft.from;
        self.from_choices = choices_of(identity);
        self.show_cc_bcc |= !draft.cc.is_empty() || !draft.bcc.is_empty();
        self.to = join_addresses(&draft.to);
        self.cc = join_addresses(&draft.cc);
        self.bcc = join_addresses(&draft.bcc);
        self.subject = draft.subject;
        self.text = self.opened_as.text.clone();
        self.in_reply_to = draft.in_reply_to;
        self.references = draft.references;
        self.forward_parts = prefilled
            .attachments
            .into_iter()
            .map(|(name, size)| (name, size, true))
            .collect();
        self.loading = false;
        let filled = self.snapshot();
        let opened = std::mem::replace(&mut self.opened_as, filled);
        keep_typed(&mut self.from, &opened.from, typed.from);
        keep_typed(&mut self.to, &opened.to, typed.to);
        keep_typed(&mut self.cc, &opened.cc, typed.cc);
        keep_typed(&mut self.bcc, &opened.bcc, typed.bcc);
        keep_typed(&mut self.subject, &opened.subject, typed.subject);
        keep_typed(&mut self.text, &opened.text, typed.text);
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            bcc: self.bcc.clone(),
            cc: self.cc.clone(),
            files: self.files.clone(),
            forward_as_attachment: self.forward_as_attachment,
            from: self.from.clone(),
            kept_parts: self.forward_parts.iter().map(|part| part.2).collect(),
            subject: self.subject.clone(),
            text: self.text.clone(),
            to: self.to.clone(),
        }
    }

    /// Takes a choice from `from_choices`: `account\taddress` for new mail, else an address of the account.
    pub(crate) fn choose_from(&mut self, choice: &str) {
        match choice.split_once('\t') {
            Some((account, address)) => {
                self.account = account.to_string();
                self.from = address.to_string();
            }
            None => self.from = choice.to_string(),
        }
    }

    /// Takes what the user typed into `field`, and drops an error that was about it.
    pub(crate) fn set(&mut self, field: ComposeField, value: String) {
        let about = match field {
            ComposeField::Bcc => Some(Field::Bcc),
            ComposeField::Cc => Some(Field::Cc),
            ComposeField::From => Some(Field::From),
            ComposeField::Subject => Some(Field::Subject),
            ComposeField::Text => None,
            ComposeField::To => Some(Field::To),
        };
        match field {
            ComposeField::Bcc => self.bcc = value,
            ComposeField::Cc => self.cc = value,
            ComposeField::From => self.choose_from(&value),
            ComposeField::Subject => self.subject = value,
            ComposeField::Text => self.text = value,
            ComposeField::To => self.to = value,
        }
        if about.is_some() && self.error.as_ref().map(|e| e.0) == about {
            self.error = None;
        }
    }

    /// The From choices as the drop-down shows them: the address alone, new mail's `account\taddress` too.
    pub(crate) fn choice_labels(&self) -> Vec<&str> {
        self.from_choices
            .iter()
            .map(|choice| {
                choice
                    .split_once('\t')
                    .map_or(choice.as_str(), |(_, address)| address)
            })
            .collect()
    }

    /// Which of `from_choices` From is; None when it was typed, as a wildcard alias is.
    pub(crate) fn choice_index(&self) -> Option<usize> {
        self.from_choices
            .iter()
            .position(|choice| match choice.split_once('\t') {
                Some((account, address)) => account == self.account && address == self.from,
                None => *choice == self.from,
            })
    }

    /// The attached files as their names and sizes in words.
    pub(crate) fn file_rows(&self) -> Vec<(String, String)> {
        self.files
            .iter()
            .map(|(path, size)| {
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                (name.into_owned(), size_in_words(*size))
            })
            .collect()
    }

    /// The original's attachments as their names, sizes in words and whether they go along.
    pub(crate) fn part_rows(&self) -> Vec<(&str, String, bool)> {
        self.forward_parts
            .iter()
            .map(|(name, size, kept)| (name.as_str(), size_in_words(*size), *kept))
            .collect()
    }

    /// What a reply or forward becomes when its original is gone from the store: new mail with the same fields,
    /// still threaded.
    pub(crate) fn without_original(&mut self) {
        self.kind = ComposeKind::New;
        self.original = None;
        self.forward_parts.clear();
        self.forward_as_attachment = false;
    }

    pub(crate) fn changed(&self) -> bool {
        self.snapshot() != self.opened_as
    }

    /// What leaving asks, or Discard when `discarding`: Discard of a saved draft always asks, as it deletes the draft.
    pub(crate) fn leave_question(&self, discarding: bool) -> Option<&'static str> {
        if discarding && self.draft.is_some() {
            return Some("Delete this draft?");
        }
        self.changed().then_some("Discard this message?")
    }

    /// `saved`, this composer as it was when saved, is in Drafts now as `draft`; what was typed since still counts as
    /// a change.
    pub(crate) fn saved_as(&mut self, saved: &Composer, draft: ComposeDraft) {
        self.draft = Some(draft);
        self.saving = false;
        self.opened_as = saved.snapshot();
    }

    /// Whether Save draft can start: not while it loads or is sent.
    pub(crate) fn can_save(&self) -> bool {
        !self.loading && !self.sending
    }

    /// Counts `message_id` among its copies in Drafts, for a save that may reach the server without answering.
    pub(crate) fn may_hold(&mut self, message_id: String) {
        match &mut self.draft {
            Some(draft) => draft.left.push(message_id),
            None => {
                self.draft = Some(ComposeDraft {
                    message_id,
                    left: Vec::new(),
                })
            }
        }
    }

    pub(crate) fn split_addresses(field: &str) -> Vec<String> {
        split_recipients(field)
    }

    fn address_field(&self, field: Field) -> Option<&str> {
        match field {
            Field::Bcc => Some(&self.bcc),
            Field::Cc => Some(&self.cc),
            Field::To => Some(&self.to),
            _ => None,
        }
    }

    /// The first bad address of a To, Cc or Bcc field, in words; None for any other field.
    pub(crate) fn field_error(&self, field: Field) -> Option<String> {
        Self::split_addresses(self.address_field(field)?)
            .iter()
            .find_map(|recipient| {
                valid_address(&bare_address(recipient))
                    .err()
                    .map(|_| CheckError::BadAddress(recipient.clone()).to_string())
            })
    }

    /// What the attached files, at the sizes they had when added, and the kept parts of the original add up to.
    pub(crate) fn attachment_total(&self) -> u64 {
        let files: u64 = self.files.iter().map(|(_, size)| size).sum();
        let kept: u64 = self
            .forward_parts
            .iter()
            .filter(|part| part.2)
            .map(|part| part.1)
            .sum();
        files + kept
    }

    pub(crate) fn check(&self, identity: &Identity, limit: u64) -> Result<(), (Field, String)> {
        let fields = [Field::To, Field::Cc, Field::Bcc];
        if fields.iter().all(|field| {
            self.address_field(*field)
                .is_none_or(|v| Self::split_addresses(v).is_empty())
        }) {
            return Err((Field::To, "Add a recipient.".to_string()));
        }
        for field in fields {
            if let Some(message) = self.field_error(field) {
                return Err((field, message));
            }
        }
        check_from(&self.from, identity).map_err(|e| (Field::From, e.to_string()))?;
        match self.too_much(limit) {
            Some(error) => Err((Field::Attachments, error)),
            None => Ok(()),
        }
    }

    /// Why the attachments are more than `limit` takes; None when they fit.
    fn too_much(&self, limit: u64) -> Option<String> {
        let total = self.attachment_total();
        (total > limit).then(|| {
            format!(
                "{}{OVER_LIMIT}{}.",
                size_in_words(total),
                size_in_words(limit)
            )
        })
    }

    /// Shows the size error on the attachments as soon as they are too much, and takes it away once they fit.
    /// Another field's error, or another one about the attachments, stays.
    pub(crate) fn check_size(&mut self) {
        let own = self.error.as_ref().is_some_and(|(field, error)| {
            *field == Field::Attachments && error.contains(OVER_LIMIT)
        });
        match self.too_much(self.size_limit) {
            Some(error) if own || matches!(self.error, None | Some((Field::Note, _))) => {
                self.error = Some((Field::Attachments, error));
            }
            None if own => self.error = None,
            _ => {}
        }
    }

    /// What is attached out of what the server takes, as "3 KB of 25.0 MB"; empty while nothing is.
    pub(crate) fn size_line(&self) -> String {
        match self.attachment_total() {
            0 => String::new(),
            total => format!(
                "{} of {}",
                size_in_words(total),
                size_in_words(self.size_limit)
            ),
        }
    }

    pub(crate) fn request(&self) -> SendRequest {
        let to = Self::split_addresses(&self.to);
        let cc = Self::split_addresses(&self.cc);
        let bcc = Self::split_addresses(&self.bcc);
        let (folder, uid) = self
            .original
            .as_ref()
            .map(|original| (original.folder.clone(), original.uid))
            .unwrap_or_default();
        let kind = match self.kind {
            ComposeKind::New => SendKind::New {
                to: to.clone(),
                cc: cc.clone(),
                bcc: bcc.clone(),
                subject: self.subject.clone(),
            },
            ComposeKind::Reply(reply_to) => SendKind::Reply {
                folder,
                uid,
                to: reply_to,
            },
            ComposeKind::Forward => SendKind::Forward {
                folder,
                uid,
                as_: if self.forward_as_attachment {
                    ForwardAs::Attachment
                } else {
                    ForwardAs::Inline
                },
                to: to.clone(),
                cc: cc.clone(),
                bcc: bcc.clone(),
            },
        };
        let forward_parts = (self.kind == ComposeKind::Forward).then(|| {
            self.forward_parts
                .iter()
                .enumerate()
                .filter(|(_, part)| part.2)
                .map(|(index, _)| index)
                .collect()
        });
        SendRequest {
            account: Some(self.account.clone()),
            kind,
            body: Body::Given(self.text.clone()),
            from: Some(self.from.clone()),
            attachments: self.files.iter().map(|(path, _)| path.clone()).collect(),
            now: false,
            save_sent: None,
            // Without its raw bytes, as after a restart, the original is fetched again by folder and uid.
            original: (self.original.as_ref())
                .map(|original| original.raw.clone())
                .filter(|raw| !raw.is_empty()),
            headers: Some(HeaderEdits {
                from: self.from.clone(),
                to,
                cc,
                bcc,
                subject: self.subject.clone(),
                in_reply_to: self.in_reply_to.clone(),
                references: self.references.clone(),
            }),
            forward_parts,
            draft: self
                .draft
                .as_ref()
                .map(ComposeDraft::ids)
                .unwrap_or_default(),
        }
    }
}

/// Where each open composer is kept, so a crash or quit loses nothing: `<state>/compose/<id>.json`.
pub(crate) fn recovery_dir(paths: &Paths) -> PathBuf {
    paths.state_dir.join("compose")
}

fn recovery_file(dir: &Path, id: u64) -> PathBuf {
    dir.join(format!("{id}.json"))
}

pub(crate) fn save_recovery(dir: &Path, composer: &Composer) -> io::Result<()> {
    let json = serde_json::to_vec(composer).map_err(io::Error::other)?;
    write_atomic(&recovery_file(dir, composer.id), &json)
}

/// Where a composer opened from a draft keeps the draft's own files: `<state>/compose/<id>/files`.
pub(crate) fn files_dir(dir: &Path, id: u64) -> PathBuf {
    dir.join(id.to_string()).join("files")
}

/// The line of an opened draft without a Message-ID, which no save or send can find to replace.
pub(crate) const NO_MESSAGE_ID: &str =
    "This draft has no Message-ID, so saving or sending it leaves the old copy in Drafts.";

/// The longest file name a draft's attachment is written under, in bytes.
const NAME_MAX: usize = 100;

/// A file name from a stranger as one that stays in its directory: no path, no leading dot, no control characters,
/// and short enough for any file system.
fn safe_name(name: &str) -> String {
    let last = name.rsplit(['/', '\\']).next().unwrap_or_default();
    let clean: String = last.chars().filter(|c| !c.is_control()).collect();
    let clean = clean.trim().trim_start_matches('.').trim();
    if clean.is_empty() {
        return "attachment".to_string();
    }
    if clean.len() <= NAME_MAX {
        return clean.to_string();
    }
    let (stem, extension) = split_extension(clean);
    let room = NAME_MAX.saturating_sub(extension.len());
    let mut cut = room.min(stem.len());
    while !stem.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}{extension}", &stem[..cut])
}

/// `name` as stem and extension, the dot with the extension; a name with a dot only in front has none.
fn split_extension(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(dot) if dot > 0 && name.len() - dot <= 10 => name.split_at(dot),
        _ => (name, ""),
    }
}

/// Removes the draft files of composers without a recovery file, as one sent before a quit leaves them.
pub(crate) fn sweep_files(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let ids = entries
        .flatten()
        .filter_map(|entry| entry.file_name().to_str()?.parse::<u64>().ok());
    for id in ids.collect::<Vec<_>>() {
        remove_unkept_files(dir, id);
    }
}

/// Removes composer `id`'s draft files unless its recovery file still names them; those wait for the next start.
pub(crate) fn remove_unkept_files(dir: &Path, id: u64) {
    if !recovery_file(dir, id).exists() {
        remove_files(dir, id);
    }
}

/// The composers left in `dir`, oldest first; a file that cannot be read stays there and is skipped.
pub(crate) fn load_recoveries(dir: &Path) -> Vec<Composer> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut composers: Vec<Composer> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|e| e == "json"))
        .filter_map(|path| {
            let read = std::fs::read(&path).ok();
            let composer = read.and_then(|bytes| serde_json::from_slice(&bytes).ok());
            if composer.is_none() {
                // Only the name: a parse error can quote what the file holds.
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                log::warn!("could not read the unsent message in {name}; it was left there");
            }
            composer
        })
        .collect();
    composers.sort_by_key(|composer| composer.id);
    composers
}

/// Removes composer `id`'s recovery file and the draft files it held.
pub(crate) fn remove_recovery(dir: &Path, id: u64) {
    remove_recovery_file(dir, id);
    remove_files(dir, id);
}

/// Removes composer `id`'s recovery file only, keeping its draft files for an undo that reopens it.
pub(crate) fn remove_recovery_file(dir: &Path, id: u64) {
    match std::fs::remove_file(recovery_file(dir, id)) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => {
            log::warn!("could not remove the unsent message {id}.json: {e}");
        }
        _ => {}
    }
}

/// Removes the draft files composer `id` held.
pub(crate) fn remove_files(dir: &Path, id: u64) {
    match std::fs::remove_dir_all(dir.join(id.to_string())) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => {
            log::warn!("could not remove the files of the unsent message {id}: {e}");
        }
        _ => {}
    }
}

/// How many addresses completion offers.
pub(crate) const COMPLETIONS: usize = 8;

/// An address field split at its last comma outside a quoted name or `<…>`: what stays, and the address being typed.
pub(crate) fn typed_part(field: &str) -> (&str, &str) {
    let (mut quoted, mut escaped, mut angle, mut cut) = (false, false, false, None);
    for (i, c) in field.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            '<' if !quoted => angle = true,
            '>' if !quoted => angle = false,
            ',' if !quoted && !angle => cut = Some(i),
            _ => {}
        }
    }
    match cut {
        Some(i) => (&field[..i], field[i + 1..].trim_start()),
        None => ("", field.trim_start()),
    }
}

/// `field` with the address being typed replaced by `choice`, and a comma after it for the next.
pub(crate) fn complete(field: &str, choice: &str) -> String {
    match typed_part(field).0.trim_end() {
        "" => format!("{choice}, "),
        kept => format!("{kept}, {choice}, "),
    }
}

/// Every account's matches as one list: an address once, people written to first, then each newest first.
pub(crate) fn merge_known(found: Vec<KnownAddress>) -> Vec<String> {
    let mut merged: Vec<KnownAddress> = Vec::new();
    for known in found {
        match (merged.iter_mut()).find(|other| other.address.eq_ignore_ascii_case(&known.address)) {
            Some(other) => {
                other.written_to |= known.written_to;
                if known.latest > other.latest {
                    other.latest = known.latest;
                    other.name = known.name.or(other.name.take());
                }
            }
            None => merged.push(known),
        }
    }
    merged.sort_by_key(|known| (!known.written_to, std::cmp::Reverse(known.latest)));
    merged.truncate(COMPLETIONS);
    merged
        .iter()
        .map(|known| mailbox_text(known.name.as_deref(), &known.address))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use postvak_core::config::{AccountConfig, AliasEntry, PasswordSource};
    use postvak_core::send::build::Draft;
    use postvak_core::send::mailto::Mailto;

    #[test]
    fn the_typed_part_starts_after_the_last_comma_outside_a_name() {
        assert_eq!(typed_part("an"), ("", "an"));
        assert_eq!(typed_part("a@x.org,  an"), ("a@x.org", "an"));
        assert_eq!(
            typed_part("\"Doe, J\" <j@x.org>, an"),
            ("\"Doe, J\" <j@x.org>", "an")
        );
        assert_eq!(typed_part("\"Doe, J"), ("", "\"Doe, J"));
        assert_eq!(complete("an", "Ann <ann@x.org>"), "Ann <ann@x.org>, ");
    }

    #[test]
    fn merged_matches_name_an_address_once() {
        let known = |address: &str, latest, written_to| KnownAddress {
            name: None,
            address: address.into(),
            latest,
            written_to,
        };
        let merged = merge_known(vec![
            known("ann@x.org", 5, false),
            known("bob@x.org", 1, true),
            known("ANN@x.org", 2, true),
        ]);
        assert_eq!(merged, ["ann@x.org", "bob@x.org"]);
    }

    const LIMIT: u64 = 25_000_000;

    fn identity() -> Identity {
        let mut account = AccountConfig::new(
            "work",
            "imap.example.com",
            "me@example.com",
            PasswordSource::Keyring { keyring: true },
        );
        account.aliases = vec![
            AliasEntry::Pattern("alias@example.com".into()),
            AliasEntry::Pattern("*@shop.example.com".into()),
        ];
        account.identity().unwrap_or_else(|e| panic!("{e}"))
    }

    fn prefilled(draft: Draft, attachments: Vec<(String, u64)>) -> Prefilled {
        Prefilled {
            account: "work".into(),
            draft,
            original: Some(b"raw".to_vec()),
            attachments,
        }
    }

    fn accounts() -> Vec<(String, Identity)> {
        vec![("work".to_string(), identity())]
    }

    fn new_mail() -> Composer {
        Composer::new_mail(1, "work", &accounts(), None)
    }

    /// A file of `size` bytes, as the composer holds it once added.
    fn file_of(dir: &tempfile::TempDir, name: &str, size: u64) -> (PathBuf, u64) {
        let path = dir.path().join(name);
        let file = std::fs::File::create(&path).unwrap_or_else(|e| panic!("{e}"));
        file.set_len(size).unwrap_or_else(|e| panic!("{e}"));
        (path, size)
    }

    #[test]
    fn a_reply_starts_loading_and_fills_from_the_prefill() {
        let mut composer = Composer::reply(3, "work", "INBOX", 7, ReplyTo::Sender);
        assert!(composer.loading);
        let draft = Draft {
            from: "alias@example.com".into(),
            to: vec!["Ann <ann@example.org>".into()],
            subject: "Re: Hi".into(),
            text: "> quoted".into(),
            ..Draft::default()
        };
        composer.fill(prefilled(draft, vec![]), &identity());
        assert!(!composer.loading);
        assert_eq!(composer.from, "alias@example.com");
        assert_eq!(composer.to, "Ann <ann@example.org>");
        assert_eq!(composer.subject, "Re: Hi");
        assert_eq!(composer.text, "", "the quote goes under the text on Send");
        assert_eq!(
            composer.original.as_ref().map(|o| (o.uid, o.raw.clone())),
            Some((7, b"raw".to_vec()))
        );
        assert!(!composer.changed());
    }

    #[test]
    fn what_was_typed_while_loading_stays() {
        let mut composer = Composer::reply(3, "work", "INBOX", 7, ReplyTo::Sender);
        composer.set(ComposeField::Text, "Early words".into());
        composer.set(ComposeField::Subject, "My own subject".into());
        let draft = Draft {
            from: "me@example.com".into(),
            to: vec!["ann@example.org".into()],
            subject: "Re: Hi".into(),
            text: "> quoted".into(),
            ..Draft::default()
        };
        composer.fill(prefilled(draft, vec![]), &identity());
        assert_eq!(composer.text, "Early words");
        assert_eq!(composer.subject, "My own subject");
        assert_eq!(composer.to, "ann@example.org");
        assert!(composer.changed());
    }

    #[test]
    fn reply_all_fills_cc() {
        let mut composer = Composer::reply(3, "work", "INBOX", 7, ReplyTo::All);
        let draft = Draft {
            from: "me@example.com".into(),
            to: vec!["a@example.org".into()],
            cc: vec!["b@example.org".into(), "c@example.org".into()],
            ..Draft::default()
        };
        composer.fill(prefilled(draft, vec![]), &identity());
        assert_eq!(composer.cc, "b@example.org, c@example.org");
        assert!(composer.show_cc_bcc);
    }

    #[test]
    fn a_composer_round_trips_through_its_file() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let mut composer = Composer::forward(3, "work", "INBOX", 7);
        let draft = Draft {
            from: "me@example.com".into(),
            references: vec!["<1@x.org>".into()],
            ..Draft::default()
        };
        composer.fill(prefilled(draft, vec![("a.pdf".into(), 10)]), &identity());
        composer.set(ComposeField::To, "ann@example.org".into());
        composer.set(ComposeField::Text, "See below".into());
        composer.files.push(file_of(&dir, "notes.txt", 4));
        composer.forward_parts[0].2 = false;
        composer.error = Some((Field::Send, "not sent".into()));
        composer.sending = true;
        composer.saving = true;
        composer.draft = Some(ComposeDraft {
            message_id: "<d1@x>".into(),
            left: vec!["<d0@x>".into()],
        });
        save_recovery(dir.path(), &composer).unwrap_or_else(|e| panic!("{e}"));

        let restored = load_recoveries(dir.path());
        composer.error = None;
        composer.saving = false;
        if let Some(original) = composer.original.as_mut() {
            original.raw.clear();
        }
        assert_eq!(restored, [composer]);
        assert!(restored[0].changed(), "it still asks before it goes");
    }

    #[test]
    fn an_unreadable_file_is_skipped() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let mut composer = new_mail();
        composer.set(ComposeField::Text, "Hi".into());
        save_recovery(dir.path(), &composer).unwrap_or_else(|e| panic!("{e}"));
        std::fs::write(dir.path().join("2.json"), "{ not json").unwrap_or_else(|e| panic!("{e}"));
        let restored = load_recoveries(dir.path());
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].text, "Hi");
        assert!(dir.path().join("2.json").exists(), "left for the user");
        remove_recovery(dir.path(), composer.id);
        assert!(load_recoveries(dir.path()).is_empty());
    }

    #[test]
    fn a_reply_without_its_original_goes_on_as_threaded_new_mail() {
        let mut composer = Composer::reply(3, "work", "INBOX", 7, ReplyTo::Sender);
        let draft = Draft {
            from: "me@example.com".into(),
            to: vec!["ann@example.org".into()],
            subject: "Re: lunch".into(),
            in_reply_to: Some("<7@x.org>".into()),
            references: vec!["<7@x.org>".into()],
            ..Draft::default()
        };
        composer.fill(prefilled(draft, Vec::new()), &identity());
        composer.without_original();
        let request = composer.request();
        assert!(
            matches!(request.kind, SendKind::New { .. }),
            "{:?}",
            request.kind
        );
        assert_eq!(request.original, None);
        let headers = request.headers.unwrap_or_else(|| panic!("no headers"));
        assert_eq!(headers.in_reply_to.as_deref(), Some("<7@x.org>"));
        assert_eq!(headers.references, ["<7@x.org>"]);
    }

    #[test]
    fn a_forward_lists_the_originals_attachments_all_kept() {
        let mut composer = Composer::forward(3, "work", "INBOX", 7);
        let draft = Draft {
            from: "me@example.com".into(),
            ..Draft::default()
        };
        let attachments = vec![("a.pdf".to_string(), 10), ("b.png".to_string(), 20)];
        composer.fill(prefilled(draft, attachments), &identity());
        assert_eq!(
            composer.forward_parts,
            vec![("a.pdf".into(), 10, true), ("b.png".into(), 20, true)]
        );
        assert_eq!(composer.attachment_total(), 30);
    }

    #[test]
    fn new_mail_from_a_mailto_link() {
        let mailto = Mailto {
            to: vec!["x@example.org".into(), "y@example.org".into()],
            bcc: vec!["z@example.org".into()],
            subject: "Hello".into(),
            body: "Body".into(),
            ..Mailto::default()
        };
        let composer = Composer::new_mail(1, "work", &accounts(), Some(mailto));
        assert!(!composer.loading);
        assert_eq!(composer.from, "me@example.com");
        assert_eq!(composer.to, "x@example.org, y@example.org");
        assert_eq!(composer.bcc, "z@example.org");
        assert!(composer.show_cc_bcc);
        assert_eq!(composer.subject, "Hello");
        assert_eq!(composer.text, "Body");
        assert!(!composer.changed());
    }

    #[test]
    fn nothing_typed_leaves_without_asking() {
        assert_eq!(new_mail().leave_question(false), None);
    }

    #[test]
    fn a_typed_word_asks_before_leaving() {
        let mut composer = new_mail();
        composer.text.push_str("hi");
        assert_eq!(
            composer.leave_question(false),
            Some("Discard this message?")
        );
    }

    #[test]
    fn check_names_the_field() {
        let mut composer = new_mail();
        assert_eq!(
            composer.check(&identity(), LIMIT).map_err(|e| e.0),
            Err(Field::To)
        );

        composer.to = "a@example.org".into();
        composer.cc = "not an address".into();
        assert_eq!(
            composer.check(&identity(), LIMIT).map_err(|e| e.0),
            Err(Field::Cc)
        );

        composer.cc.clear();
        assert_eq!(composer.check(&identity(), LIMIT), Ok(()));

        composer.from = "other@example.net".into();
        assert_eq!(
            composer.check(&identity(), LIMIT).map_err(|e| e.0),
            Err(Field::From)
        );

        composer.from = "me@example.com".into();
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        composer.files = vec![file_of(&dir, "big", 30_000_000)];
        assert_eq!(
            composer.check(&identity(), LIMIT).map_err(|e| e.0),
            Err(Field::Attachments)
        );
    }

    #[test]
    fn the_request_is_what_the_fields_say() {
        let mut composer = Composer::forward(3, "work", "INBOX", 7);
        let draft = Draft {
            from: "me@example.com".into(),
            ..Draft::default()
        };
        let attachments = vec![("a.pdf".to_string(), 10), ("b.png".to_string(), 20)];
        composer.fill(prefilled(draft, attachments), &identity());
        composer.to = "Ann <ann@example.org>, bob@example.org".into();
        composer.subject = "Fwd: Hi".into();
        composer.text = "see below".into();
        composer.forward_parts[0].2 = false;

        let request = composer.request();
        assert_eq!(request.account.as_deref(), Some("work"));
        assert_eq!(request.body, Body::Given("see below".into()));
        assert_eq!(request.original, Some(b"raw".to_vec()));
        assert_eq!(request.forward_parts, Some(vec![1]));
        assert_eq!(
            request.kind,
            SendKind::Forward {
                folder: "INBOX".into(),
                uid: 7,
                as_: ForwardAs::Inline,
                to: vec!["Ann <ann@example.org>".into(), "bob@example.org".into()],
                cc: vec![],
                bcc: vec![],
            }
        );
        assert_eq!(
            request.headers.map(|h| (h.from, h.to.len(), h.subject)),
            Some(("me@example.com".into(), 2, "Fwd: Hi".into()))
        );

        composer.forward_as_attachment = true;
        assert!(matches!(
            composer.request().kind,
            SendKind::Forward {
                as_: ForwardAs::Attachment,
                ..
            }
        ));
    }

    #[test]
    fn a_wildcard_alias_is_not_a_choice_but_may_be_typed() {
        let mut composer = new_mail();
        assert_eq!(
            composer.from_choices,
            vec!["work\tme@example.com", "work\talias@example.com"],
        );
        composer.to = "a@example.org".into();
        composer.from = "orders@shop.example.com".into();
        assert_eq!(composer.check(&identity(), LIMIT), Ok(()));
        composer.from = "*@shop.example.com".into();
        assert_eq!(
            composer.check(&identity(), LIMIT).map_err(|e| e.0),
            Err(Field::From)
        );
    }

    #[test]
    fn a_named_address_passes_the_check() {
        let mut composer = new_mail();
        composer.to = "Ann Example <ann@example.org>, \"Doe, Jo\" <jo@example.org>".into();
        assert_eq!(composer.field_error(Field::To), None);
        assert_eq!(composer.check(&identity(), LIMIT), Ok(()));
        composer.to = "Ann <not an address>".into();
        assert!(composer.field_error(Field::To).is_some());
    }

    #[test]
    fn a_bad_address_is_marked_when_its_field_is_left() {
        let mut composer = new_mail();
        composer.cc = "ann@example.org, nope".into();
        assert!(composer.field_error(Field::Cc).is_some());
        assert_eq!(composer.field_error(Field::To), None);
        composer.cc = "ann@example.org".into();
        assert_eq!(composer.field_error(Field::Cc), None);
    }

    #[test]
    fn new_mail_lists_every_accounts_addresses_and_follows_the_choice() {
        let home = AccountConfig::new(
            "home",
            "imap.home.test",
            "me@home.test",
            PasswordSource::Keyring { keyring: true },
        )
        .identity()
        .unwrap_or_else(|e| panic!("{e}"));
        let accounts = vec![("work".to_string(), identity()), ("home".to_string(), home)];
        let mut composer = Composer::new_mail(1, "work", &accounts, None);
        assert_eq!(
            composer.from_choices,
            vec![
                "work\tme@example.com",
                "work\talias@example.com",
                "home\tme@home.test"
            ]
        );
        composer.choose_from("home\tme@home.test");
        assert_eq!(
            (composer.account.as_str(), composer.from.as_str()),
            ("home", "me@home.test")
        );
    }

    #[test]
    fn from_shows_as_a_choice_unless_it_was_typed() {
        let mut composer = Composer::reply(3, "work", "INBOX", 7, ReplyTo::Sender);
        let draft = |from: &str| Draft {
            from: from.into(),
            ..Draft::default()
        };
        composer.fill(prefilled(draft("alias@example.com"), vec![]), &identity());
        assert_eq!(
            composer.choice_labels(),
            ["me@example.com", "alias@example.com"]
        );
        assert_eq!(composer.choice_index(), Some(1));
        composer.fill(
            prefilled(draft("deals@shop.example.com"), vec![]),
            &identity(),
        );
        assert_eq!(composer.choice_index(), None);

        let mut new = new_mail();
        assert_eq!(new.choice_labels(), ["me@example.com", "alias@example.com"]);
        assert_eq!(new.choice_index(), Some(0));
        new.account = "home".into();
        assert_eq!(new.choice_index(), None, "the address of another account");
    }

    #[test]
    fn toggling_forward_as_attachment_alone_is_a_change() {
        let mut composer = new_mail();
        composer.forward_as_attachment = true;
        assert!(composer.changed());
    }

    #[test]
    fn the_check_reads_no_files_as_the_worker_does_on_send() {
        let mut composer = new_mail();
        composer.to = "a@example.org".into();
        composer.files = vec![(PathBuf::from("/nonexistent/gone.txt"), 0)];
        assert_eq!(composer.check(&identity(), LIMIT), Ok(()));
    }

    #[test]
    fn files_count_and_show_at_the_size_read_when_added() {
        let mut composer = new_mail();
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let (path, _) = file_of(&dir, "notes.txt", 0);
        composer.files = vec![(path, 2048)];
        assert_eq!(composer.attachment_total(), 2048);
        assert_eq!(
            composer.file_rows(),
            [("notes.txt".to_string(), "2 KB".to_string())]
        );
    }

    #[test]
    fn the_size_check_leaves_other_errors_alone() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let mut composer = new_mail();
        composer.size_limit = LIMIT;
        composer.files = vec![file_of(&dir, "big", 30_000_000)];
        composer.error = Some((Field::To, "Add a recipient.".into()));
        composer.check_size();
        assert_eq!(composer.error.as_ref().map(|e| e.0), Some(Field::To));
        composer.error = None;
        composer.check_size();
        assert_eq!(
            composer.error.as_ref().map(|e| e.0),
            Some(Field::Attachments)
        );
        composer.files.clear();
        composer.check_size();
        assert_eq!(
            composer.error, None,
            "its own error goes once the files fit"
        );
        let unreadable = Some((Field::Attachments, "gone.txt can't be read.".to_string()));
        composer.error = unreadable.clone();
        composer.check_size();
        assert_eq!(composer.error, unreadable);
    }

    #[test]
    fn the_size_error_is_in_megabytes() {
        let mut composer = new_mail();
        composer.to = "a@example.org".into();
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        composer.files = vec![file_of(&dir, "big", 30_000_000)];
        assert_eq!(
            composer.check(&identity(), LIMIT),
            Err((
                Field::Attachments,
                "28.6 MB attached; the limit is 23.8 MB.".to_string()
            ))
        );
    }

    #[test]
    fn a_reply_keeps_its_own_accounts_choices() {
        let mut composer = Composer::reply(3, "work", "INBOX", 7, ReplyTo::Sender);
        composer.fill(prefilled(Draft::default(), vec![]), &identity());
        assert_eq!(
            composer.from_choices,
            vec!["me@example.com", "alias@example.com"]
        );
    }
}

#[cfg(test)]
mod drafts_opened {
    use super::*;
    use postvak_core::config::{AccountConfig, PasswordSource};
    use postvak_core::send::drafts::{DraftKind, OpenedDraft};

    fn identities() -> Vec<(String, Identity)> {
        let account = AccountConfig::new(
            "work",
            "imap.example.com",
            "me@example.com",
            PasswordSource::Keyring { keyring: true },
        );
        vec![("work".to_string(), account.identity().unwrap())]
    }

    fn foreign() -> OpenedDraft {
        OpenedDraft {
            kind: DraftKind::New,
            original: None,
            shown_original: None,
            from: "me@example.com".into(),
            to: vec!["Ann <ann@x.org>".into()],
            cc: vec!["bob@x.org".into()],
            bcc: Vec::new(),
            subject: "Re: Plans".into(),
            text: "Sure.\n\n> Shall we?".into(),
            files: vec![
                ("../../etc/passwd".into(), b"root".to_vec()),
                (".hidden".into(), b"h".to_vec()),
                ("notes.txt".into(), b"one".to_vec()),
                ("notes.txt".into(), b"two!".to_vec()),
                (format!("{}.pdf", "x".repeat(300)), Vec::new()),
                ("..".into(), b"dots".to_vec()),
            ],
            forward_parts: None,
            in_reply_to: Some("<m1@x.org>".into()),
            references: vec!["<m0@x.org>".into(), "<m1@x.org>".into()],
            note: Some("This draft had only HTML; it opens as text."),
            message_id: "d1@x.org".into(),
        }
    }

    #[test]
    fn a_foreign_draft_opens_whole_with_its_files() {
        let dir = tempfile::tempdir().unwrap();
        let files = files_dir(dir.path(), 4);
        let composer =
            Composer::from_draft(4, "work", &identities(), foreign(), None, &files).unwrap();
        assert_eq!(composer.kind, ComposeKind::New);
        assert!(composer.original.is_none());
        assert_eq!(composer.to, "Ann <ann@x.org>");
        assert_eq!(composer.cc, "bob@x.org");
        assert!(composer.show_cc_bcc);
        assert_eq!(composer.text, "Sure.\n\n> Shall we?");
        assert_eq!(composer.in_reply_to.as_deref(), Some("<m1@x.org>"));
        assert_eq!(composer.references, ["<m0@x.org>", "<m1@x.org>"]);
        assert_eq!(
            composer.error,
            Some((
                Field::Note,
                "This draft had only HTML; it opens as text.".into()
            ))
        );
        assert_eq!(
            composer.draft,
            Some(ComposeDraft {
                message_id: "<d1@x.org>".into(),
                left: Vec::new()
            })
        );
        let names: Vec<String> = composer
            .file_rows()
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert_eq!(names[..4], ["passwd", "hidden", "notes.txt", "notes.txt"]);
        assert!(
            names[4].ends_with(".pdf") && names[4].len() <= 100,
            "{}",
            names[4]
        );
        assert_eq!(names[5], "attachment");
        for (index, (path, size)) in composer.files.iter().enumerate() {
            assert_eq!(path.parent(), Some(files.join(index.to_string()).as_path()));
            assert_eq!(std::fs::metadata(path).unwrap().len(), *size);
        }
        assert_eq!(std::fs::read(&composer.files[3].0).unwrap(), b"two!");
        assert!(!composer.changed(), "nothing typed yet");
    }

    #[test]
    fn a_forward_draft_keeps_the_parts_it_named() {
        let dir = tempfile::tempdir().unwrap();
        let original = b"From: a@x.org\r\nMessage-ID: <m1@x.org>\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"b\"\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\nhi\r\n--b\r\nContent-Type: text/plain\r\nContent-Disposition: attachment; filename=\"a.txt\"\r\n\r\naaa\r\n--b\r\nContent-Type: text/plain\r\nContent-Disposition: attachment; filename=\"b.txt\"\r\n\r\nbb\r\n--b--\r\n".to_vec();
        let opened = OpenedDraft {
            kind: DraftKind::Forward(postvak_core::send::reply::ForwardAs::Inline),
            original: Some(original.clone()),
            forward_parts: Some(vec![1]),
            note: None,
            files: Vec::new(),
            ..foreign()
        };
        let at = Some(("INBOX".to_string(), 7));
        let composer = Composer::from_draft(
            5,
            "work",
            &identities(),
            opened,
            at,
            &files_dir(dir.path(), 5),
        )
        .unwrap();
        assert_eq!(composer.kind, ComposeKind::Forward);
        assert!(!composer.forward_as_attachment);
        let original_ref = composer.original.as_ref().unwrap();
        assert_eq!(
            (original_ref.folder.as_str(), original_ref.uid),
            ("INBOX", 7)
        );
        assert_eq!(original_ref.raw, original);
        let kept: Vec<(&str, bool)> = composer
            .part_rows()
            .iter()
            .map(|(n, _, k)| (*n, *k))
            .collect();
        assert_eq!(kept, [("a.txt", false), ("b.txt", true)]);
        assert_eq!(composer.error, None);
    }

    #[test]
    fn a_draft_without_a_message_id_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let opened = OpenedDraft {
            message_id: String::new(),
            note: None,
            ..foreign()
        };
        let composer = Composer::from_draft(
            4,
            "work",
            &identities(),
            opened,
            None,
            &files_dir(dir.path(), 4),
        )
        .unwrap();
        assert_eq!(composer.draft, None);
        assert_eq!(composer.error, Some((Field::Note, NO_MESSAGE_ID.into())));
    }

    #[test]
    fn files_without_a_recovery_file_are_swept() {
        let dir = tempfile::tempdir().unwrap();
        let kept = Composer::from_draft(
            4,
            "work",
            &identities(),
            foreign(),
            None,
            &files_dir(dir.path(), 4),
        )
        .unwrap();
        save_recovery(dir.path(), &kept).unwrap();
        let left = Composer::from_draft(
            5,
            "work",
            &identities(),
            foreign(),
            None,
            &files_dir(dir.path(), 5),
        )
        .unwrap();
        sweep_files(dir.path());
        assert!(kept.files[0].0.exists());
        assert!(!left.files[0].0.exists());
        assert!(!dir.path().join("5").exists());
    }

    #[test]
    fn the_draft_files_go_with_the_composer() {
        let dir = tempfile::tempdir().unwrap();
        let composer = Composer::from_draft(
            4,
            "work",
            &identities(),
            foreign(),
            None,
            &files_dir(dir.path(), 4),
        )
        .unwrap();
        save_recovery(dir.path(), &composer).unwrap();
        assert!(composer.files[0].0.exists());
        remove_recovery(dir.path(), 4);
        assert!(!composer.files[0].0.exists());
        assert!(!dir.path().join("4").exists());
        assert!(load_recoveries(dir.path()).is_empty());
    }
}
