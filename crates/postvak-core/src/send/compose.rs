//! What `postvak send` does, out of the CLI: build or read the message, check it, decide when it goes, and queue it in
//! the daemon's outbox.

use std::path::PathBuf;

use anyhow::{Context as _, anyhow, bail};

use super::build::{self, Draft, FileAttachment};
use super::check::{self, Recipients as CheckRecipients};
use super::outbox::{Entry, Original};
use super::reply::{self, ForwardAs, ForwardOptions, OriginalFlag, ReplyTo};
use super::{Envelope, sendmail, sent, server_for};
use crate::config::{self, AccountConfig, Config, Identity};
use crate::daemon::client::LazyClient;
use crate::daemon::wire::SendOutcome;
use crate::paths::Paths;
use crate::store::Store;

/// Where the text of a message comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Body {
    /// Read from stdin or `--body-file`.
    Given(String),
    /// Written in the editor.
    Edit,
}

/// What to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendKind {
    New {
        to: Vec<String>,
        cc: Vec<String>,
        bcc: Vec<String>,
        subject: String,
    },
    Reply {
        folder: String,
        uid: u32,
        to: ReplyTo,
    },
    Forward {
        folder: String,
        uid: u32,
        as_: ForwardAs,
        to: Vec<String>,
        cc: Vec<String>,
        bcc: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendRequest {
    pub account: Option<String>,
    pub kind: SendKind,
    pub body: Body,
    /// `--from`: one of the account's addresses; unset, the account's or the one the original went to.
    pub from: Option<String>,
    pub attachments: Vec<PathBuf>,
    /// Skip the undo delay.
    pub now: bool,
    pub save_sent: Option<bool>,
}

/// A message in the outbox, for the line the CLI prints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Queued {
    pub account: String,
    pub id: String,
    pub due: i64,
    /// The first recipient's name, or address when it has none.
    pub first_recipient: String,
    /// How many recipients besides the first.
    pub more: usize,
}

/// `folder/uid` as `--reply` and `--forward` take it; the folder may hold slashes.
pub fn parse_message_ref(text: &str) -> anyhow::Result<(String, u32)> {
    let (folder, uid) = text
        .rsplit_once('/')
        .ok_or_else(|| anyhow!("'{text}' is not <folder>/<uid>, such as INBOX/42"))?;
    let uid = uid
        .parse()
        .map_err(|_| anyhow!("'{uid}' in '{text}' is not a message uid"))?;
    if folder.is_empty() {
        bail!("'{text}' names no folder");
    }
    Ok((folder.to_string(), uid))
}

fn stored_original(
    paths: &Paths,
    client: &LazyClient,
    account: &AccountConfig,
    folder: &str,
    uid: u32,
) -> anyhow::Result<Vec<u8>> {
    let store = Store::open_account(paths, &account.name)?;
    let folder = crate::folder_name::known(&store, folder)?
        .map(|f| f.name)
        .unwrap_or_else(|| folder.to_string());
    let message = store
        .message(&folder, uid)?
        .ok_or_else(|| anyhow!("no message {folder}/{uid} in account '{}'", account.name))?;
    client.raw_message(&account.name, &store, &message)
}

fn read_attachments(files: &[PathBuf]) -> anyhow::Result<Vec<FileAttachment>> {
    files
        .iter()
        .map(|path| {
            let bytes =
                std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("attachment")
                .to_string();
            Ok(FileAttachment {
                content_type: build::content_type_for(&name).to_string(),
                name,
                bytes,
            })
        })
        .collect()
}

/// The original a reply or forward answers, and the flag it gets once the answer went.
type Answered = Option<(Vec<u8>, OriginalFlag)>;

/// The draft for `kind` with `body`, and the original it answers.
fn draft_for(
    paths: &Paths,
    client: &LazyClient,
    account: &AccountConfig,
    identity: &Identity,
    kind: &SendKind,
    body: &str,
) -> anyhow::Result<(Draft, Answered)> {
    Ok(match kind {
        SendKind::New {
            to,
            cc,
            bcc,
            subject,
        } => {
            let from = identity.address().to_string();
            (
                Draft {
                    from_name: identity.display_name_for(&from).map(str::to_string),
                    from,
                    to: to.clone(),
                    cc: cc.clone(),
                    bcc: bcc.clone(),
                    subject: subject.clone(),
                    text: body.to_string(),
                    ..Default::default()
                },
                None,
            )
        }
        SendKind::Reply { folder, uid, to } => {
            let raw = stored_original(paths, client, account, folder, *uid)?;
            let draft = reply::reply(&raw, *to, identity, body)?;
            (draft, Some((raw, OriginalFlag::Answered)))
        }
        SendKind::Forward {
            folder,
            uid,
            as_,
            to,
            cc,
            bcc,
        } => {
            let raw = stored_original(paths, client, account, folder, *uid)?;
            let recipients = reply::Recipients {
                to: to.clone(),
                cc: cc.clone(),
                bcc: bcc.clone(),
            };
            let options = ForwardOptions {
                as_: *as_,
                ..Default::default()
            };
            let draft = reply::forward(&raw, recipients, identity, body, &options)?;
            (draft, Some((raw, OriginalFlag::Forwarded)))
        }
    })
}

/// Builds, checks and queues the message `request` describes; `edit` opens the editor on a text and returns it edited.
pub fn compose(
    config: &Config,
    paths: &Paths,
    client: &LazyClient,
    request: SendRequest,
    edit: impl FnOnce(&str) -> anyhow::Result<String>,
) -> anyhow::Result<Queued> {
    let account = config::single(&config.accounts, request.account.as_deref())?;
    let identity = account.identity()?;
    let state = Store::open_account(paths, &account.name)?.smtp_state()?;
    let server = server_for(account, state.as_ref())
        .ok_or_else(|| anyhow!("{}", crate::help::sending_not_set_up(&account.name)))?;
    let given = match &request.body {
        Body::Given(text) => text.clone(),
        Body::Edit => String::new(),
    };
    let (mut draft, original) =
        draft_for(paths, client, account, &identity, &request.kind, &given)?;
    if request.body == Body::Edit {
        let template = super::edit::template(&draft);
        let edited = edit(&template)?;
        let Some(mut written) = super::edit::read_back(&template, &edited, draft.clone())? else {
            bail!("the message was left empty or unchanged, so nothing was sent");
        };
        // The user's text above an untouched quote keeps the HTML of an HTML original; answers written into the quote
        // send as plain text only.
        if draft.html.is_some() {
            let quote = draft.text.trim_start_matches('\n').trim_end();
            written.html = match written.text.trim_end().strip_suffix(quote) {
                Some(user) => {
                    let (rebuilt, _) = draft_for(
                        paths,
                        client,
                        account,
                        &identity,
                        &request.kind,
                        user.trim_end(),
                    )?;
                    rebuilt.html
                }
                None => None,
            };
        }
        draft = written;
    }
    if let Some(from) = &request.from {
        check::check_from(from, &identity)?;
        draft.from_name = identity.display_name_for(from).map(str::to_string);
        draft.from = from.trim().to_string();
    }
    draft
        .attachments
        .extend(read_attachments(&request.attachments)?);
    let now = crate::time::now();
    let keep = build::build(&draft, now)?;
    let caps = state.as_ref().map(|s| super::Capabilities {
        size: s.size,
        eight_bit_mime: s.eight_bit_mime,
    });
    let args = CheckRecipients {
        from_headers: true,
        ..Default::default()
    };
    let checked = check::check(&keep, &args, &identity, caps.as_ref())?;
    let delay = if request.now {
        0
    } else {
        config.sending.undo_secs as i64
    };
    let save_sent = sent::saves_sent(
        &server,
        request
            .save_sent
            .or(account.smtp.as_ref().and_then(|s| s.save_sent)),
    );
    let original = original.and_then(|(raw, flag)| {
        let id = crate::message::parse_headers(&raw).message_id?;
        Some(Original {
            message_id: id,
            flag,
        })
    });
    let entry = Entry::new(
        Envelope {
            from: checked.from.clone(),
            to: checked.recipients.clone(),
        },
        now + delay,
        save_sent,
        original,
    );
    let (id, due) = client
        .client()?
        .queue_send(&account.name, &checked.keep, &entry)?;
    let first = draft
        .to
        .iter()
        .chain(&draft.cc)
        .chain(&draft.bcc)
        .next()
        .map(|r| shown_name(r))
        .unwrap_or_default();
    Ok(Queued {
        account: account.name.clone(),
        id,
        due,
        first_recipient: first,
        more: checked.recipients.len().saturating_sub(1),
    })
}

/// A recipient's name, or its address when it has none.
fn shown_name(recipient: &str) -> String {
    match recipient.parse::<lettre::message::Mailbox>() {
        Ok(mailbox) => mailbox
            .name
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| mailbox.email.to_string()),
        Err(_) => recipient.to_string(),
    }
}

/// What became of a `--raw` message, and sendmail's exit code for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RawOutcome {
    Sent,
    Temporary(String),
    Permanent(String),
}

impl RawOutcome {
    /// 0 sent, 75 (`EX_TEMPFAIL`) tried again later, 69 (`EX_UNAVAILABLE`) refused.
    pub fn exit_code(&self) -> u8 {
        match self {
            RawOutcome::Sent => 0,
            RawOutcome::Temporary(_) => 75,
            RawOutcome::Permanent(_) => 69,
        }
    }
}

/// Checks a whole message and queues it to go at once, saving no Sent copy unless `save_sent`.
pub fn compose_raw(
    config: &Config,
    paths: &Paths,
    client: &LazyClient,
    raw: &[u8],
    args: &[String],
    account: Option<&str>,
    save_sent: bool,
) -> anyhow::Result<Queued> {
    let recipients = sendmail::parse_args(args)?;
    let account = sendmail::account_for(raw, &config.accounts, account)?;
    let identity = account.identity()?;
    let state = Store::open_account(paths, &account.name)?.smtp_state()?;
    if server_for(account, state.as_ref()).is_none() {
        bail!("{}", crate::help::sending_not_set_up(&account.name));
    }
    let caps = state.as_ref().map(|s| super::Capabilities {
        size: s.size,
        eight_bit_mime: s.eight_bit_mime,
    });
    let checked = check::check(raw, &recipients, &identity, caps.as_ref())?;
    let entry = Entry::new(
        Envelope {
            from: checked.from.clone(),
            to: checked.recipients.clone(),
        },
        crate::time::now(),
        save_sent,
        None,
    );
    let (id, due) = client
        .client()?
        .queue_send(&account.name, &checked.keep, &entry)?;
    Ok(Queued {
        account: account.name.clone(),
        id,
        due,
        first_recipient: checked.recipients.first().cloned().unwrap_or_default(),
        more: checked.recipients.len().saturating_sub(1),
    })
}

/// [`compose_raw`], then waits for the server's answer, as sendmail does.
pub fn compose_raw_and_wait(
    config: &Config,
    paths: &Paths,
    client: &LazyClient,
    raw: &[u8],
    args: &[String],
    account: Option<&str>,
    save_sent: bool,
) -> anyhow::Result<RawOutcome> {
    let queued = compose_raw(config, paths, client, raw, args, account, save_sent)?;
    Ok(
        match client.client()?.await_send(&queued.account, &queued.id)? {
            SendOutcome::Sent => RawOutcome::Sent,
            SendOutcome::Temporary(reason) => RawOutcome::Temporary(reason),
            SendOutcome::Permanent(reason) => RawOutcome::Permanent(reason),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::Client;
    use crate::daemon::test_support::{TestDaemon, options, recording_connector, recording_smtp};
    use crate::send::smtp::SharedSmtp;
    use crate::store::Message;
    use std::time::{Duration, Instant};

    struct Home {
        daemon: TestDaemon,
        smtp: SharedSmtp,
        config: Config,
        client: LazyClient,
    }

    const ORIGINAL: &[u8] = b"From: Linus Example <linus@x.org>\r\nTo: me@example.com\r\nSubject: Plans\r\nDate: Mon, 2 Nov 2026 10:00:00 +0000\r\nMessage-ID: <m1@x.org>\r\n\r\nShall we?\r\n";

    fn home() -> Home {
        let smtp = SharedSmtp::default();
        let daemon = TestDaemon::start_with(crate::daemon::Options {
            smtp: recording_smtp(smtp.clone()),
            ..options(recording_connector(), None)
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while smtp.with(|s| s.logins.len()) < 2 {
            assert!(Instant::now() < deadline, "the daemon never guessed");
            std::thread::sleep(Duration::from_millis(20));
        }
        // The guess stores asynchronously after logging in.
        while Store::open_account(&daemon.paths, "work")
            .unwrap()
            .smtp_state()
            .unwrap()
            .is_none()
        {
            assert!(Instant::now() < deadline, "the guess was never stored");
            std::thread::sleep(Duration::from_millis(20));
        }
        let config = Config::load(&daemon.paths.config_file()).unwrap();
        let client =
            LazyClient::with_client(&daemon.paths, Client::connect(&daemon.paths).unwrap());
        let store = Store::open_account(&daemon.paths, "work").unwrap();
        let parsed = crate::message::parse_headers(ORIGINAL);
        store
            .insert_message(&Message {
                folder: "INBOX".into(),
                uid: 7,
                message_id: parsed.message_id.clone(),
                from_addr: parsed.from.clone(),
                subject: parsed.subject.clone(),
                thread_id: "m1@x.org".into(),
                internaldate: 1,
                headers: ORIGINAL.to_vec(),
                ..Default::default()
            })
            .unwrap();
        store.set_raw("INBOX", 7, ORIGINAL, "Shall we?").unwrap();
        Home {
            daemon,
            smtp,
            config,
            client,
        }
    }

    fn request(kind: SendKind) -> SendRequest {
        SendRequest {
            account: Some("work".into()),
            kind,
            body: Body::Given("Yes!\n".into()),
            from: None,
            attachments: Vec::new(),
            now: false,
            save_sent: None,
        }
    }

    fn no_editor(_: &str) -> anyhow::Result<String> {
        panic!("no editor in this test")
    }

    #[test]
    fn compose_reply_fetches_the_original_and_queues() {
        let h = home();
        let reply = SendKind::Reply {
            folder: "INBOX".into(),
            uid: 7,
            to: ReplyTo::Sender,
        };
        let queued = compose(
            &h.config,
            &h.daemon.paths,
            &h.client,
            request(reply),
            no_editor,
        )
        .unwrap();
        assert_eq!(queued.first_recipient, "Linus Example");
        assert_eq!(queued.more, 0);
        assert!(queued.due >= crate::time::now() + 9);
        let outbox = h.client.client().unwrap().outbox().unwrap();
        assert_eq!(outbox.len(), 1);
        let entry = &outbox[0].1;
        assert_eq!(entry.envelope.to, ["linus@x.org"]);
        assert_eq!(
            entry.original.as_ref().map(|o| o.message_id.as_str()),
            Some("m1@x.org")
        );
        assert!(entry.save_sent);
        assert_eq!(h.smtp.with(|s| s.sent.len()), 0);
    }

    #[test]
    fn compose_refuses_before_queueing() {
        let h = home();
        let mut bad = request(SendKind::New {
            to: vec!["a@x.org".into()],
            cc: vec![],
            bcc: vec![],
            subject: "s".into(),
        });
        bad.from = Some("stranger@else.org".into());
        let error = compose(&h.config, &h.daemon.paths, &h.client, bad, no_editor).unwrap_err();
        assert!(
            error.to_string().contains("neither the account's address"),
            "{error}"
        );
        let none = request(SendKind::New {
            to: vec![],
            cc: vec![],
            bcc: vec![],
            subject: "s".into(),
        });
        assert!(compose(&h.config, &h.daemon.paths, &h.client, none, no_editor).is_err());
        assert!(h.client.client().unwrap().outbox().unwrap().is_empty());
    }

    #[test]
    fn an_edit_left_unchanged_queues_nothing() {
        let h = home();
        let mut edited = request(SendKind::Reply {
            folder: "INBOX".into(),
            uid: 7,
            to: ReplyTo::Sender,
        });
        edited.body = Body::Edit;
        let error = compose(&h.config, &h.daemon.paths, &h.client, edited.clone(), |t| {
            Ok(t.to_string())
        })
        .unwrap_err();
        assert!(error.to_string().contains("left empty or unchanged"));
        let queued = compose(&h.config, &h.daemon.paths, &h.client, edited, |t| {
            Ok(t.replacen("\n\n", "\n\nSure.", 1))
        })
        .unwrap();
        assert_eq!(queued.first_recipient, "Linus Example");
    }

    #[test]
    fn raw_exit_codes() {
        let h = home();
        let raw = b"From: me@example.com\r\nTo: a@x.org\r\nDate: Mon, 2 Nov 2026 10:00:00 +0000\r\nMessage-ID: <r@example.com>\r\n\r\nhi\r\n";
        let run = || {
            compose_raw_and_wait(&h.config, &h.daemon.paths, &h.client, raw, &[], None, false)
                .unwrap()
        };
        assert_eq!(run().exit_code(), 0);
        h.smtp.with(|s| {
            s.script
                .push_back(Err(crate::send::SmtpError::Temporary("451 later".into())))
        });
        let temporary = run();
        assert_eq!(temporary.exit_code(), 75);
        assert_eq!(h.client.client().unwrap().outbox().unwrap().len(), 1);
        h.smtp.with(|s| {
            s.script
                .push_back(Err(crate::send::SmtpError::Permanent("550 no".into())))
        });
        assert_eq!(run().exit_code(), 69);
    }

    #[test]
    fn raw_saves_no_sent_copy_unless_asked() {
        let h = home();
        let raw = b"From: me@example.com\r\nTo: a@x.org\r\nDate: Mon, 2 Nov 2026 10:00:00 +0000\r\nMessage-ID: <r@example.com>\r\n\r\nhi\r\n";
        let far = |save| {
            // Queued only: a far due time would need --now's opposite, so look at the entry right away.
            compose_raw(
                &h.config,
                &h.daemon.paths,
                &h.client,
                raw,
                &["-i".into()],
                None,
                save,
            )
            .unwrap()
        };
        let not_saved = far(false);
        let saved = far(true);
        let entries = h.client.client().unwrap().outbox().unwrap();
        let save_of = |id: &str| {
            entries
                .iter()
                .find(|(_, e)| e.id == id)
                .map(|(_, e)| e.save_sent)
        };
        // Either still listed with the flag as asked, or already sent and gone.
        assert_ne!(save_of(&not_saved.id), Some(true));
        assert_ne!(save_of(&saved.id), Some(false));
    }

    #[test]
    fn message_refs_take_folders_with_slashes() {
        assert_eq!(parse_message_ref("INBOX/42").unwrap(), ("INBOX".into(), 42));
        assert_eq!(
            parse_message_ref("Lists/rust/7").unwrap(),
            ("Lists/rust".into(), 7)
        );
        assert!(parse_message_ref("42").is_err());
        assert!(parse_message_ref("INBOX/x").is_err());
        assert!(parse_message_ref("/3").is_err());
    }
}
