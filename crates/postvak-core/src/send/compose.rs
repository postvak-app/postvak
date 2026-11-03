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
use crate::store::{SmtpState, Store};

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

/// The headers the composer edited; they replace what the builder chose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderEdits {
    pub from: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub subject: String,
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
    /// The raw message a reply or forward answers, when the caller has it; unset, it is looked up by folder and uid.
    pub original: Option<Vec<u8>>,
    pub headers: Option<HeaderEdits>,
    /// Applies only to an inline forward: the attachments to take along, by index into [`reply::forward_attachments`]; unset, all.
    pub forward_parts: Option<Vec<usize>>,
}

/// What a composer opens with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prefilled {
    pub account: String,
    pub draft: Draft,
    pub original: Option<Vec<u8>>,
    /// The original's attachments as (file name, size), in the order [`reply::AttachmentFilter::Only`] indexes.
    pub attachments: Vec<(String, u64)>,
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

/// The flag the original gets once the answer went; None for new mail.
fn flag_for(kind: &SendKind) -> Option<OriginalFlag> {
    match kind {
        SendKind::New { .. } => None,
        SendKind::Reply { .. } => Some(OriginalFlag::Answered),
        SendKind::Forward { .. } => Some(OriginalFlag::Forwarded),
    }
}

/// The stored original a reply or forward answers, and the flag it gets once the answer went; None for new mail.
fn original_for(
    paths: &Paths,
    client: &LazyClient,
    account: &AccountConfig,
    kind: &SendKind,
) -> anyhow::Result<Option<(Vec<u8>, OriginalFlag)>> {
    let (Some(flag), SendKind::Reply { folder, uid, .. } | SendKind::Forward { folder, uid, .. }) =
        (flag_for(kind), kind)
    else {
        return Ok(None);
    };
    Ok(Some((
        stored_original(paths, client, account, folder, *uid)?,
        flag,
    )))
}

/// The draft for `kind` with `body`, answering `original` when it is a reply or forward.
fn draft_for(
    identity: &Identity,
    kind: &SendKind,
    original: &[u8],
    body: &str,
    forward_parts: Option<&[usize]>,
) -> anyhow::Result<Draft> {
    Ok(match kind {
        SendKind::New {
            to,
            cc,
            bcc,
            subject,
        } => {
            let from = identity.address().to_string();
            Draft {
                from_name: identity.display_name_for(&from).map(str::to_string),
                from,
                to: to.clone(),
                cc: cc.clone(),
                bcc: bcc.clone(),
                subject: subject.clone(),
                text: body.to_string(),
                ..Default::default()
            }
        }
        SendKind::Reply { to, .. } => reply::reply(original, *to, identity, body)?,
        SendKind::Forward {
            as_, to, cc, bcc, ..
        } => {
            let recipients = reply::Recipients {
                to: to.clone(),
                cc: cc.clone(),
                bcc: bcc.clone(),
            };
            let options = ForwardOptions {
                as_: *as_,
                attachments: forward_parts.map_or(reply::AttachmentFilter::All, |parts| {
                    reply::AttachmentFilter::Only(parts.to_vec())
                }),
                ..Default::default()
            };
            reply::forward(original, recipients, identity, body, &options)?
        }
    })
}

fn set_from(draft: &mut Draft, from: &str, identity: &Identity) -> anyhow::Result<()> {
    check::check_from(from, identity)?;
    draft.from_name = identity.display_name_for(from).map(str::to_string);
    draft.from = from.trim().to_string();
    Ok(())
}

fn apply_headers(
    draft: &mut Draft,
    edits: &HeaderEdits,
    identity: &Identity,
) -> anyhow::Result<()> {
    set_from(draft, &edits.from, identity)?;
    draft.to = edits.to.clone();
    draft.cc = edits.cc.clone();
    draft.bcc = edits.bcc.clone();
    draft.subject = edits.subject.clone();
    Ok(())
}

/// What a composer opens with for `kind`: the draft with an empty body and the original it answers.
pub fn prefill(
    config: &Config,
    paths: &Paths,
    client: &LazyClient,
    account: Option<&str>,
    kind: &SendKind,
) -> anyhow::Result<Prefilled> {
    let account = config::single(&config.accounts, account)?;
    let identity = account.identity()?;
    let original = original_for(paths, client, account, kind)?.map(|(raw, _)| raw);
    let raw = original.as_deref().unwrap_or_default();
    let draft = draft_for(&identity, kind, raw, "", None)?;
    let attachments = match kind {
        SendKind::Forward {
            as_: ForwardAs::Inline,
            ..
        } => reply::forward_attachments(raw),
        _ => Vec::new(),
    };
    Ok(Prefilled {
        account: account.name.clone(),
        draft,
        original,
        attachments,
    })
}

/// The account's stored SMTP state and the server it sends through; an error that says how to set one up when it has
/// none.
fn sending_server(
    paths: &Paths,
    account: &AccountConfig,
) -> anyhow::Result<(Option<SmtpState>, super::SmtpServer)> {
    let state = Store::open_account(paths, &account.name)?.smtp_state()?;
    let server = server_for(account, state.as_ref())
        .ok_or_else(|| anyhow!("{}", crate::help::sending_not_set_up(&account.name)))?;
    Ok((state, server))
}

/// Puts a checked message in the daemon's outbox, due at `due`.
fn queue(
    client: &LazyClient,
    account: &AccountConfig,
    checked: &check::Checked,
    due: i64,
    save_sent: bool,
    original: Option<Original>,
    first_recipient: String,
) -> anyhow::Result<Queued> {
    let envelope = Envelope {
        from: checked.from.clone(),
        to: checked.recipients.clone(),
    };
    let entry = Entry::new(envelope, due, save_sent, original);
    let (id, due) = client
        .client()?
        .queue_send(&account.name, &checked.keep, &entry)?;
    Ok(Queued {
        account: account.name.clone(),
        id,
        due,
        first_recipient,
        more: checked.recipients.len().saturating_sub(1),
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
    let (state, server) = sending_server(paths, account)?;
    let given = match &request.body {
        Body::Given(text) => text.clone(),
        Body::Edit => String::new(),
    };
    let original = match request.original.clone() {
        Some(raw) => flag_for(&request.kind).map(|flag| (raw, flag)),
        None => original_for(paths, client, account, &request.kind)?,
    };
    let forward_parts = request.forward_parts.as_deref();
    let raw = original.as_ref().map_or(&[][..], |(raw, _)| raw.as_slice());
    let mut draft = draft_for(&identity, &request.kind, raw, &given, forward_parts)?;
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
                    draft_for(
                        &identity,
                        &request.kind,
                        raw,
                        user.trim_end(),
                        forward_parts,
                    )?
                    .html
                }
                None => None,
            };
        }
        draft = written;
    }
    if let Some(edits) = &request.headers {
        apply_headers(&mut draft, edits, &identity)?;
    }
    if let Some(from) = &request.from {
        set_from(&mut draft, from, &identity)?;
    }
    draft
        .attachments
        .extend(read_attachments(&request.attachments)?);
    let now = crate::time::now();
    let keep = build::build(&draft, now)?;
    let args = CheckRecipients {
        from_headers: true,
        ..Default::default()
    };
    let caps = state.as_ref().map(SmtpState::capabilities);
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
        let message_id = crate::message::parse_headers(&raw).message_id?;
        Some(Original { message_id, flag })
    });
    let first = draft
        .to
        .iter()
        .chain(&draft.cc)
        .chain(&draft.bcc)
        .next()
        .map(|r| crate::message::display_name(r))
        .unwrap_or_default();
    queue(
        client,
        account,
        &checked,
        now + delay,
        save_sent,
        original,
        first,
    )
}

/// sendmail's exit code for what became of a `--raw` message: 0 sent, 75 (`EX_TEMPFAIL`) tried again later, 69
/// (`EX_UNAVAILABLE`) refused.
pub fn exit_code(outcome: &SendOutcome) -> u8 {
    match outcome {
        SendOutcome::Sent => 0,
        SendOutcome::Temporary(_) => 75,
        SendOutcome::Permanent(_) => 69,
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
    let (state, _) = sending_server(paths, account)?;
    let caps = state.as_ref().map(SmtpState::capabilities);
    let checked = check::check(raw, &recipients, &identity, caps.as_ref())?;
    let first = checked.recipients.first().cloned().unwrap_or_default();
    queue(
        client,
        account,
        &checked,
        crate::time::now(),
        save_sent,
        None,
        first,
    )
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
) -> anyhow::Result<SendOutcome> {
    let queued = compose_raw(config, paths, client, raw, args, account, save_sent)?;
    client.client()?.await_send(&queued.account, &queued.id)
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
            original: None,
            headers: None,
            forward_parts: None,
        }
    }

    fn reply_kind(uid: u32) -> SendKind {
        SendKind::Reply {
            folder: "INBOX".into(),
            uid,
            to: ReplyTo::Sender,
        }
    }

    fn queued_message(h: &Home) -> (String, Option<String>) {
        let entry = h.client.client().unwrap().outbox().unwrap().remove(0).1;
        let message_id = entry.original.map(|o| o.message_id);
        (entry.envelope.to.join(","), message_id)
    }

    #[test]
    fn edited_headers_replace_the_builders() {
        let h = home();
        let mut edited = request(reply_kind(7));
        edited.original = Some(ORIGINAL.to_vec());
        edited.headers = Some(HeaderEdits {
            from: "me@example.com".into(),
            to: vec!["c@x.org".into()],
            cc: vec!["d@x.org".into()],
            bcc: vec!["e@x.org".into()],
            subject: "New".into(),
        });
        compose(&h.config, &h.daemon.paths, &h.client, edited, no_editor).unwrap();
        let (_, entry) = h.client.client().unwrap().outbox().unwrap().remove(0);
        assert_eq!(entry.envelope.from, "me@example.com");
        assert_eq!(entry.envelope.to, ["c@x.org", "d@x.org", "e@x.org"]);
        let raw = std::fs::read(
            h.daemon
                .paths
                .outbox_dir("work")
                .join(format!("{}.eml", entry.id)),
        )
        .unwrap();
        let header = |name| crate::message::header_text(&raw, name);
        assert_eq!(header("Subject").as_deref(), Some("New"));
        assert_eq!(header("To").as_deref(), Some("c@x.org"));
        assert_eq!(header("Cc").as_deref(), Some("d@x.org"));
        assert_eq!(header("Bcc").as_deref(), Some("e@x.org"));
        assert!(header("From").unwrap().contains("me@example.com"));
        assert_eq!(header("In-Reply-To").as_deref(), Some("<m1@x.org>"));
        assert!(String::from_utf8_lossy(&raw).contains("> Shall we?"));
    }

    #[test]
    fn a_preloaded_original_is_used_without_the_store() {
        let h = home();
        let mut preloaded = request(reply_kind(999));
        preloaded.original = Some(ORIGINAL.to_vec());
        compose(&h.config, &h.daemon.paths, &h.client, preloaded, no_editor).unwrap();
        assert_eq!(
            queued_message(&h),
            ("linus@x.org".into(), Some("m1@x.org".into()))
        );
        assert!(
            compose(
                &h.config,
                &h.daemon.paths,
                &h.client,
                request(reply_kind(999)),
                no_editor
            )
            .is_err()
        );
    }

    #[test]
    fn without_edits_the_request_is_the_clis() {
        let h = home();
        let mut preloaded = request(reply_kind(7));
        preloaded.original = Some(ORIGINAL.to_vec());
        compose(&h.config, &h.daemon.paths, &h.client, preloaded, no_editor).unwrap();
        compose(
            &h.config,
            &h.daemon.paths,
            &h.client,
            request(reply_kind(7)),
            no_editor,
        )
        .unwrap();
        let outbox = h.client.client().unwrap().outbox().unwrap();
        assert_eq!(outbox.len(), 2);
        assert_eq!(outbox[0].1.envelope, outbox[1].1.envelope);
        assert_eq!(outbox[0].1.original, outbox[1].1.original);
    }

    #[test]
    fn prefill_has_the_recipients_and_an_empty_text_above_the_quote() {
        let h = home();
        let prefilled = prefill(
            &h.config,
            &h.daemon.paths,
            &h.client,
            Some("work"),
            &reply_kind(7),
        )
        .unwrap();
        assert_eq!(prefilled.account, "work");
        assert_eq!(prefilled.draft.to, ["Linus Example <linus@x.org>"]);
        assert_eq!(prefilled.draft.subject, "Re: Plans");
        assert!(
            prefilled.draft.text.starts_with("\n\nOn "),
            "{:?}",
            prefilled.draft.text
        );
        assert!(prefilled.draft.text.contains("> Shall we?"));
        assert_eq!(prefilled.original.as_deref(), Some(ORIGINAL));
        assert!(prefilled.attachments.is_empty());
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
        assert_eq!(exit_code(&run()), 0);
        h.smtp.with(|s| {
            s.script
                .push_back(Err(crate::send::SmtpError::Temporary("451 later".into())))
        });
        let temporary = run();
        assert_eq!(exit_code(&temporary), 75);
        assert_eq!(h.client.client().unwrap().outbox().unwrap().len(), 1);
        h.smtp.with(|s| {
            s.script
                .push_back(Err(crate::send::SmtpError::Permanent("550 no".into())))
        });
        assert_eq!(exit_code(&run()), 69);
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
