//! The composer without a toolkit: its fields, checks, questions and what it asks the daemon to send.

use std::path::PathBuf;

use postvak_core::config::Identity;
use postvak_core::send::build::bare_address;
use postvak_core::send::check::{CheckError, check_from, valid_address};
use postvak_core::send::compose::{Body, HeaderEdits, Prefilled, SendKind, SendRequest};
use postvak_core::send::edit::split_recipients;
use postvak_core::send::mailto::Mailto;
use postvak_core::send::reply::{ForwardAs, ReplyTo};

/// Send's hover hint, with its key.
pub(crate) fn send_hint() -> &'static str {
    if cfg!(target_os = "macos") {
        "Send · Cmd+Enter"
    } else {
        "Send · Ctrl+Enter"
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    Send,
    Subject,
    To,
}

/// The message a reply or forward answers; `raw` is empty until the prefill arrives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OriginalRef {
    pub folder: String,
    pub uid: u32,
    pub raw: Vec<u8>,
}

/// The fields as the composer opened, to tell whether the user changed anything.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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

#[derive(Debug, Clone, PartialEq)]
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
    pub loading: bool,
    pub sending: bool,
    pub error: Option<(Field, String)>,
    opened_as: Snapshot,
}

fn join_addresses(addresses: &[String]) -> String {
    addresses.join(", ")
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
            loading: false,
            sending: false,
            error: None,
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

    pub(crate) fn fill(&mut self, prefilled: Prefilled, identity: &Identity) {
        let draft = prefilled.draft;
        if let Some(original) = self.original.as_mut() {
            original.raw = prefilled.original.unwrap_or_default();
        }
        self.from = draft.from;
        self.from_choices = choices_of(identity);
        self.show_cc_bcc = !draft.cc.is_empty() || !draft.bcc.is_empty();
        self.to = join_addresses(&draft.to);
        self.cc = join_addresses(&draft.cc);
        self.bcc = join_addresses(&draft.bcc);
        self.subject = draft.subject;
        self.text = draft.text;
        self.forward_parts = prefilled
            .attachments
            .into_iter()
            .map(|(name, size)| (name, size, true))
            .collect();
        self.loading = false;
        self.opened_as = self.snapshot();
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

    pub(crate) fn changed(&self) -> bool {
        self.snapshot() != self.opened_as
    }

    pub(crate) fn leave_question(&self) -> Option<&'static str> {
        self.changed().then_some("Discard this message?")
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
        if let Some((path, _)) = self.files.iter().find(|(path, _)| !path.is_file()) {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            return Err((Field::Attachments, format!("{name} can't be read.")));
        }
        let total = self.attachment_total();
        if total > limit {
            return Err((
                Field::Attachments,
                format!(
                    "{} attached; the limit is {}.",
                    size_in_words(total),
                    size_in_words(limit)
                ),
            ));
        }
        Ok(())
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
            original: self.original.as_ref().map(|original| original.raw.clone()),
            headers: Some(HeaderEdits {
                from: self.from.clone(),
                to,
                cc,
                bcc,
                subject: self.subject.clone(),
            }),
            forward_parts,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use postvak_core::config::{AccountConfig, AliasEntry, PasswordSource};
    use postvak_core::send::build::Draft;
    use postvak_core::send::mailto::Mailto;

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
        assert_eq!(composer.text, "> quoted");
        assert_eq!(
            composer.original.as_ref().map(|o| (o.uid, o.raw.clone())),
            Some((7, b"raw".to_vec()))
        );
        assert!(!composer.changed());
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
        assert_eq!(new_mail().leave_question(), None);
    }

    #[test]
    fn a_typed_word_asks_before_leaving() {
        let mut composer = new_mail();
        composer.text.push_str("hi");
        assert_eq!(composer.leave_question(), Some("Discard this message?"));
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
    fn an_unreadable_attachment_fails_the_check() {
        let mut composer = new_mail();
        composer.to = "a@example.org".into();
        composer.files = vec![(PathBuf::from("/nonexistent/gone.txt"), 0)];
        let (field, message) = composer.check(&identity(), LIMIT).unwrap_err();
        assert_eq!(field, Field::Attachments);
        assert!(message.contains("gone.txt"));
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
