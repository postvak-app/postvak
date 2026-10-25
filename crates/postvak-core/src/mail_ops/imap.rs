use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use async_imap::Session;
use async_imap::extensions::idle::IdleResponse;
use async_imap::imap_proto::{
    BodyContentCommon, BodyStructure, ContentEncoding, MessageSection, SectionPath,
};
use async_imap::types::{Capabilities, Fetch, Flag, NameAttribute};
use futures_util::TryStreamExt;
use rustls::pki_types::{CertificateDer, pem::PemObject};
use tokio::net::TcpStream;
use tokio::runtime::Runtime;
use tokio_rustls::client::TlsStream;

use crate::config::AccountConfig;
use crate::credentials::Secret;
use crate::mail_ops::{
    Envelope, FlagUpdate, IdleOutcome, MailError, MailOps, MailResult, RemoteFolder, SelectInfo,
    Structure, TextPart,
};

type ImapSession = Session<TlsStream<TcpStream>>;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// A server that stops answering mid-command would otherwise hold the account's thread until keepalive gives up, or forever.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(120);
/// A peer that vanished (a NAT entry dropped during sleep) is noticed after about 60 + 4 × 15 seconds instead of never.
const KEEPALIVE_IDLE: Duration = Duration::from_secs(60);
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);
const KEEPALIVE_RETRIES: u32 = 4;

pub struct ImapOps {
    rt: Runtime,
    session: Option<ImapSession>,
    has_idle: bool,
    has_move: bool,
    has_uidplus: bool,
    has_condstore: bool,
    command_timeout: Duration,
}

impl ImapOps {
    pub fn connect(account: &AccountConfig, secret: &Secret) -> MailResult<ImapOps> {
        ImapOps::connect_within(account, secret, CONNECT_TIMEOUT)
    }

    fn connect_within(
        account: &AccountConfig,
        secret: &Secret,
        limit: Duration,
    ) -> MailResult<ImapOps> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| MailError::Io(e.to_string()))?;
        let opened = rt.block_on(async {
            tokio::time::timeout(limit, open_session(account, secret))
                .await
                .map_err(|_| {
                    MailError::Connect(format!(
                        "{}:{} did not answer within {limit:?}",
                        account.host, account.port
                    ))
                })?
        });
        let (session, caps) = match opened {
            Ok(opened) => opened,
            Err(error) => {
                // Dropping the runtime would wait for a blocking DNS lookup past the limit.
                rt.shutdown_background();
                return Err(error);
            }
        };
        Ok(ImapOps {
            rt,
            session: Some(session),
            has_idle: caps.has_str("IDLE"),
            has_move: caps.has_str("MOVE"),
            has_uidplus: caps.has_str("UIDPLUS"),
            // QRESYNC implies CONDSTORE (RFC 7162).
            has_condstore: caps.has_str("CONDSTORE") || caps.has_str("QRESYNC"),
            command_timeout: COMMAND_TIMEOUT,
        })
    }

    /// FETCHes of a whole chunk of headers, every flag or a large body, and APPENDs of a large message.
    fn bulk_timeout(&self) -> Duration {
        self.command_timeout * 5
    }

    /// A command on one message gets the usual limit; one on a batch the bulk one.
    fn batch_timeout(&self, uids: &[u32]) -> Duration {
        if uids.len() > 1 {
            self.bulk_timeout()
        } else {
            self.command_timeout
        }
    }

    /// Runs one command, giving up after `limit` as a lost connection so the sync loop reconnects.
    fn run<T>(
        &mut self,
        limit: Duration,
        command: impl AsyncFnOnce(&mut ImapSession) -> MailResult<T>,
    ) -> MailResult<T> {
        let session = self
            .session
            .as_mut()
            .ok_or_else(|| MailError::Io("session closed".into()))?;
        let bounded = async { tokio::time::timeout(limit, command(session)).await };
        match self.rt.block_on(bounded) {
            Ok(result) => result,
            Err(_) => {
                // A late reply would be read as the next command's, so this session is done.
                self.session = None;
                Err(MailError::Io(format!(
                    "no answer within {limit:?}; whether the command ran is unknown until the next sync"
                )))
            }
        }
    }

    fn uid_search_sorted(&mut self, query: String) -> MailResult<Vec<u32>> {
        self.run(self.command_timeout, async |session| {
            let mut uids: Vec<u32> = session
                .uid_search(query)
                .await
                .map_err(proto)?
                .into_iter()
                .collect();
            uids.sort_unstable();
            Ok(uids)
        })
    }

    #[cfg(any(test, feature = "testing"))]
    pub fn has_move(&self) -> bool {
        self.has_move
    }

    #[cfg(any(test, feature = "testing"))]
    pub fn delete_folder(&mut self, name: &str) -> MailResult<()> {
        self.run(self.command_timeout, async |session| {
            session.delete(name).await.map_err(proto)
        })
    }
}

/// The public web roots, plus the account's own CA when it sets `ca_file`.
fn root_store(ca_file: Option<&Path>) -> MailResult<rustls::RootCertStore> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let Some(path) = ca_file else {
        return Ok(roots);
    };
    let invalid =
        |reason: String| MailError::Connect(format!("ca_file {}: {reason}", path.display()));
    let certs = CertificateDer::pem_file_iter(path)
        .and_then(|certs| certs.collect::<Result<Vec<_>, _>>())
        .map_err(|e| invalid(e.to_string()))?;
    let (added, _) = roots.add_parsable_certificates(certs);
    if added == 0 {
        return Err(invalid("no certificate found".into()));
    }
    Ok(roots)
}

async fn open_session(
    account: &AccountConfig,
    secret: &Secret,
) -> MailResult<(ImapSession, Capabilities)> {
    let tcp = TcpStream::connect((account.host.as_str(), account.port))
        .await
        .map_err(|e| MailError::Connect(e.to_string()))?;
    enable_keepalive(&tcp).map_err(|e| MailError::Connect(e.to_string()))?;
    let roots = root_store(account.ca_file.as_deref())?;
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
    let name = rustls::pki_types::ServerName::try_from(account.host.clone())
        .map_err(|e| MailError::Connect(e.to_string()))?;
    let tls = connector
        .connect(name, tcp)
        .await
        .map_err(|e| MailError::Connect(e.to_string()))?;
    let client = async_imap::Client::new(tls);
    let mut session = client
        .login(&account.username, secret.expose())
        .await
        .map_err(|(e, _)| login_error(e))?;
    let caps = session.capabilities().await.map_err(proto)?;
    Ok((session, caps))
}

fn enable_keepalive(tcp: &TcpStream) -> std::io::Result<()> {
    let params = socket2::TcpKeepalive::new()
        .with_time(KEEPALIVE_IDLE)
        .with_interval(KEEPALIVE_INTERVAL)
        .with_retries(KEEPALIVE_RETRIES);
    socket2::SockRef::from(tcp).set_tcp_keepalive(&params)
}

/// The FETCH item for a section and the path its reply is filed under: `BODY.PEEK[1.2]`, or `BODY.PEEK[TEXT]` for
/// the whole body of a single-part message (section "").
fn section_fetch(section: &str) -> MailResult<(String, SectionPath)> {
    if section.is_empty() {
        return Ok((
            "BODY.PEEK[TEXT]".to_string(),
            SectionPath::Full(MessageSection::Text),
        ));
    }
    let numbers = section
        .split('.')
        .map(str::parse::<u32>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| MailError::Protocol(format!("not a body section: {section}")))?;
    Ok((
        format!("BODY.PEEK[{section}]"),
        SectionPath::Part(numbers, None),
    ))
}

/// What the indexer needs from a BODYSTRUCTURE: its plain and HTML text parts, grouped by the
/// `multipart/alternative` that holds them, and the names of its attachments. A shape this does not understand
/// yields no text parts, and the caller falls back to the whole message.
fn structure_of(body: &BodyStructure<'_>) -> Structure {
    let mut structure = Structure::default();
    let mut groups = 0;
    walk_structure(body, &mut Vec::new(), None, &mut groups, &mut structure);
    structure
}

fn walk_structure(
    body: &BodyStructure<'_>,
    path: &mut Vec<u32>,
    alternative: Option<usize>,
    groups: &mut usize,
    out: &mut Structure,
) {
    match body {
        BodyStructure::Text { common, other, .. } => {
            let subtype = common.ty.subtype.to_ascii_lowercase();
            let text = matches!(subtype.as_str(), "plain" | "html") && !is_attachment(common);
            if text {
                out.text_parts.push(TextPart {
                    section: path
                        .iter()
                        .map(u32::to_string)
                        .collect::<Vec<_>>()
                        .join("."),
                    html: subtype == "html",
                    charset: params_of(common)
                        .iter()
                        .find(|(k, _)| k.eq_ignore_ascii_case("charset"))
                        .map(|(_, v)| v.clone()),
                    transfer_encoding: encoding_name(&other.transfer_encoding),
                    octets: other.octets,
                    alternative,
                });
            } else if let Some(name) = attachment_name(common) {
                out.attachment_names.push(name);
            }
        }
        BodyStructure::Basic { common, .. } => {
            if let Some(name) = attachment_name(common) {
                out.attachment_names.push(name);
            }
        }
        BodyStructure::Message { common, body, .. } => {
            if let Some(name) = attachment_name(common) {
                out.attachment_names.push(name);
            }
            // The parts of an enclosed message are numbered under this part; a single-part one is its part 1.
            if matches!(**body, BodyStructure::Multipart { .. }) {
                walk_structure(body, path, alternative, groups, out);
            } else {
                path.push(1);
                walk_structure(body, path, alternative, groups, out);
                path.pop();
            }
        }
        BodyStructure::Multipart { common, bodies, .. } => {
            let group = if common.ty.subtype.eq_ignore_ascii_case("alternative") {
                *groups += 1;
                Some(*groups)
            } else {
                alternative
            };
            for (i, part) in bodies.iter().enumerate() {
                path.push(i as u32 + 1);
                walk_structure(part, path, group, groups, out);
                path.pop();
            }
        }
    }
}

fn is_attachment(common: &BodyContentCommon<'_>) -> bool {
    common
        .disposition
        .as_ref()
        .is_some_and(|d| d.ty.eq_ignore_ascii_case("attachment"))
}

fn params_of(common: &BodyContentCommon<'_>) -> Vec<(String, String)> {
    common
        .ty
        .params
        .iter()
        .flatten()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// The part's file name: the disposition's `filename`, else the content type's `name`, decoded as headers are.
fn attachment_name(common: &BodyContentCommon<'_>) -> Option<String> {
    let disposition: Vec<(String, String)> = common
        .disposition
        .as_ref()
        .and_then(|d| d.params.as_ref())
        .into_iter()
        .flatten()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    crate::message::param(&disposition, "filename")
        .or_else(|| crate::message::param(&params_of(common), "name"))
}

fn encoding_name(encoding: &ContentEncoding<'_>) -> Option<String> {
    Some(
        match encoding {
            ContentEncoding::SevenBit => return None,
            ContentEncoding::EightBit => "8bit",
            ContentEncoding::Binary => "binary",
            ContentEncoding::Base64 => "base64",
            ContentEncoding::QuotedPrintable => "quoted-printable",
            ContentEncoding::Other(other) => other.as_ref(),
        }
        .to_string(),
    )
}

/// `Auth` only when the server refused these credentials: a NO to LOGIN, unless its code (RFC 5530) says the server
/// cannot check them now. Anything else, such as a dropped connection or a BYE for too many connections, is passing.
fn login_error(e: async_imap::error::Error) -> MailError {
    match e {
        async_imap::error::Error::No(text) if !login_unavailable(&text) => MailError::Auth(text),
        other => MailError::Connect(format!("during login: {other}")),
    }
}

/// Whether a NO to LOGIN carries a code for a passing state rather than wrong credentials.
fn login_unavailable(text: &str) -> bool {
    let text = text.to_ascii_uppercase();
    ["UNAVAILABLE", "SERVERBUG", "LIMIT", "INUSE"]
        .iter()
        .any(|code| text.contains(&format!("[{code}")))
}

fn proto(e: async_imap::error::Error) -> MailError {
    match e {
        async_imap::error::Error::Io(e) => MailError::Io(e.to_string()),
        other => MailError::Protocol(other.to_string()),
    }
}

fn flag_to_string(flag: &Flag<'_>) -> String {
    match flag {
        Flag::Seen => "\\Seen".into(),
        Flag::Answered => "\\Answered".into(),
        Flag::Flagged => "\\Flagged".into(),
        Flag::Deleted => "\\Deleted".into(),
        Flag::Draft => "\\Draft".into(),
        Flag::Recent => "\\Recent".into(),
        Flag::MayCreate => "\\*".into(),
        Flag::Custom(s) => s.to_string(),
    }
}

fn special_use(attrs: &[NameAttribute<'_>]) -> Option<String> {
    attrs.iter().find_map(|a| match a {
        NameAttribute::Trash => Some("Trash".into()),
        NameAttribute::Sent => Some("Sent".into()),
        NameAttribute::Junk => Some("Junk".into()),
        NameAttribute::Drafts => Some("Drafts".into()),
        NameAttribute::Archive => Some("Archive".into()),
        NameAttribute::All => Some("All".into()),
        NameAttribute::Flagged => Some("Flagged".into()),
        NameAttribute::Extension(s) => match s.trim_start_matches('\\') {
            x @ ("Trash" | "Sent" | "Junk" | "Drafts" | "Archive" | "All" | "Flagged"
            | "Important") => Some(x.to_string()),
            _ => None,
        },
        _ => None,
    })
}

/// None for partial responses such as an unsolicited FLAGS-only FETCH, which would otherwise store an empty message.
fn envelope_from(fetch: &Fetch) -> Option<Envelope> {
    Some(Envelope {
        uid: fetch.uid?,
        flags: fetch.flags().map(|f| flag_to_string(&f)).collect(),
        internaldate: fetch.internal_date()?.timestamp(),
        size: fetch.size,
        headers: fetch.header()?.to_vec(),
    })
}

impl MailOps for ImapOps {
    fn has_uidplus(&self) -> bool {
        self.has_uidplus
    }

    fn list_folders(&mut self) -> MailResult<Vec<RemoteFolder>> {
        self.run(self.command_timeout, async |session| {
            let names: Vec<_> = session
                .list(Some(""), Some("*"))
                .await
                .map_err(proto)?
                .try_collect()
                .await
                .map_err(proto)?;
            Ok(names
                .iter()
                .filter(|n| {
                    !n.attributes()
                        .iter()
                        .any(|a| matches!(a, NameAttribute::NoSelect))
                })
                .map(|n| RemoteFolder {
                    name: n.name().to_string(),
                    special_use: special_use(n.attributes()),
                    delimiter: n.delimiter().map(str::to_string),
                })
                .collect())
        })
    }

    fn select(&mut self, folder: &str) -> MailResult<SelectInfo> {
        let condstore = self.has_condstore;
        self.run(self.command_timeout, async |session| {
            let mailbox = if condstore {
                session.select_condstore(folder).await
            } else {
                session.select(folder).await
            }
            .map_err(proto)?;
            Ok(SelectInfo {
                uidvalidity: mailbox.uid_validity.unwrap_or(0),
                exists: mailbox.exists,
                highest_modseq: mailbox.highest_modseq,
            })
        })
    }

    fn search_uids(&mut self, from_uid: u32) -> MailResult<Vec<u32>> {
        self.uid_search_sorted(format!("UID {from_uid}:*"))
    }

    fn search_text(&mut self, words: &[String]) -> MailResult<Vec<u32>> {
        self.uid_search_sorted(text_query(words))
    }

    fn fetch_envelopes(&mut self, first: u32, last: u32) -> MailResult<Vec<Envelope>> {
        self.run(self.bulk_timeout(), async |session| {
            let fetches: Vec<Fetch> = session
                .uid_fetch(
                    format!("{first}:{last}"),
                    "(UID FLAGS INTERNALDATE RFC822.SIZE BODY.PEEK[HEADER])",
                )
                .await
                .map_err(proto)?
                .try_collect()
                .await
                .map_err(proto)?;
            Ok(fetches
                .iter()
                .filter_map(envelope_from)
                .filter(|e| (first..=last).contains(&e.uid))
                .collect())
        })
    }

    fn fetch_flags(
        &mut self,
        upto_uid: u32,
        changed_since: Option<u64>,
    ) -> MailResult<Vec<FlagUpdate>> {
        if upto_uid == 0 {
            return Ok(vec![]);
        }
        let query = match changed_since {
            Some(modseq) => format!("(UID FLAGS) (CHANGEDSINCE {modseq})"),
            None => "(UID FLAGS)".into(),
        };
        self.run(self.bulk_timeout(), async |session| {
            let fetches: Vec<Fetch> = session
                .uid_fetch(format!("1:{upto_uid}"), query)
                .await
                .map_err(proto)?
                .try_collect()
                .await
                .map_err(proto)?;
            Ok(fetches
                .iter()
                .filter_map(|f| {
                    Some(FlagUpdate {
                        uid: f.uid?,
                        flags: f.flags().map(|x| flag_to_string(&x)).collect(),
                    })
                })
                .collect())
        })
    }

    fn fetch_raw(&mut self, uid: u32) -> MailResult<Option<Vec<u8>>> {
        self.run(self.bulk_timeout(), async |session| {
            let fetches: Vec<Fetch> = session
                .uid_fetch(uid.to_string(), "(UID BODY.PEEK[])")
                .await
                .map_err(proto)?
                .try_collect()
                .await
                .map_err(proto)?;
            Ok(fetches
                .iter()
                .find(|f| f.uid == Some(uid))
                .and_then(|f| f.body().map(<[u8]>::to_vec)))
        })
    }

    fn fetch_structure(&mut self, uid: u32) -> MailResult<Option<Structure>> {
        self.run(self.command_timeout, async |session| {
            let fetches: Vec<Fetch> = session
                .uid_fetch(uid.to_string(), "(UID BODYSTRUCTURE)")
                .await
                .map_err(proto)?
                .try_collect()
                .await
                .map_err(proto)?;
            Ok(fetches
                .iter()
                .find(|f| f.uid == Some(uid))
                .and_then(Fetch::bodystructure)
                .map(structure_of))
        })
    }

    fn fetch_part(&mut self, uid: u32, section: &str) -> MailResult<Option<Vec<u8>>> {
        let (item, path) = section_fetch(section)?;
        self.run(self.bulk_timeout(), async |session| {
            let fetches: Vec<Fetch> = session
                .uid_fetch(uid.to_string(), format!("(UID {item})"))
                .await
                .map_err(proto)?
                .try_collect()
                .await
                .map_err(proto)?;
            Ok(fetches
                .iter()
                .find(|f| f.uid == Some(uid))
                .and_then(|f| f.section(&path).map(<[u8]>::to_vec)))
        })
    }

    fn add_flags(&mut self, uid: u32, flags: &[&str]) -> MailResult<()> {
        self.add_flags_to(&[uid], flags)
    }

    fn remove_flags(&mut self, uid: u32, flags: &[&str]) -> MailResult<()> {
        self.remove_flags_from(&[uid], flags)
    }

    fn expunge(&mut self, uid: u32) -> MailResult<()> {
        self.expunge_all(&[uid])
    }

    fn move_message(&mut self, uid: u32, to: &str) -> MailResult<()> {
        self.move_messages(&[uid], to)
    }

    fn add_flags_to(&mut self, uids: &[u32], flags: &[&str]) -> MailResult<()> {
        uids.chunks(UID_BATCH)
            .try_for_each(|batch| store_flags(self, batch, '+', flags))
    }

    fn remove_flags_from(&mut self, uids: &[u32], flags: &[&str]) -> MailResult<()> {
        uids.chunks(UID_BATCH)
            .try_for_each(|batch| store_flags(self, batch, '-', flags))
    }

    fn expunge_all(&mut self, uids: &[u32]) -> MailResult<()> {
        uids.chunks(UID_BATCH)
            .try_for_each(|batch| expunge(self, batch))
    }

    fn move_messages(&mut self, uids: &[u32], to: &str) -> MailResult<()> {
        uids.chunks(UID_BATCH)
            .try_for_each(|batch| move_batch(self, batch, to))
    }

    fn create_folder(&mut self, name: &str) -> MailResult<()> {
        self.run(self.command_timeout, async |session| {
            session.create(name).await.map_err(proto)
        })
    }

    fn append(&mut self, folder: &str, raw: &[u8], flags: &[&str]) -> MailResult<()> {
        let flags = (!flags.is_empty()).then(|| format!("({})", flags.join(" ")));
        self.run(self.bulk_timeout(), async |session| {
            session
                .append(folder, flags.as_deref(), None, raw)
                .await
                .map_err(proto)
        })
    }

    fn idle(&mut self, timeout: Duration, interrupt: &AtomicBool) -> MailResult<IdleOutcome> {
        if interrupt.load(Ordering::Acquire) {
            return Ok(IdleOutcome::Interrupted);
        }
        if !self.has_idle {
            return Ok(sleep_until(timeout, interrupt));
        }
        let timeout = timeout.min(Duration::from_secs(29 * 60));
        let session = self
            .session
            .take()
            .ok_or_else(|| MailError::Io("session closed".into()))?;
        let (outcome, session) = self.rt.block_on(async {
            let mut handle = session.idle();
            handle.init().await.map_err(proto)?;
            // async-imap's timeout only bounds silence; server keepalives reset it, so also keep an absolute deadline.
            let (wait, stop) = handle.wait_with_timeout(timeout);
            let mut stop = Some(stop);
            let deadline = tokio::time::sleep(timeout);
            tokio::pin!(deadline);
            let outcome = {
                let mut wait = std::pin::pin!(wait);
                loop {
                    tokio::select! {
                        res = &mut wait => break match res.map_err(proto)? {
                            IdleResponse::NewData(_) => IdleOutcome::NewMail,
                            IdleResponse::Timeout => IdleOutcome::Timeout,
                            IdleResponse::ManualInterrupt => IdleOutcome::Interrupted,
                        },
                        _ = &mut deadline => {
                            drop(stop.take());
                            break IdleOutcome::Timeout;
                        },
                        _ = tokio::time::sleep(Duration::from_millis(50)) => {
                            if interrupt.load(Ordering::Acquire) {
                                // Dropping the StopSource makes `wait` resolve with ManualInterrupt.
                                drop(stop.take());
                            }
                        }
                    }
                }
            };
            let session = handle.done().await.map_err(proto)?;
            Ok::<_, MailError>((outcome, session))
        })?;
        self.session = Some(session);
        Ok(outcome)
    }
}

/// Removes the source of a COPY; on failure clears `\\Deleted` again, so the source stays as it was next to the copy.
fn expunge(ops: &mut ImapOps, uids: &[u32]) -> MailResult<()> {
    let has_uidplus = ops.has_uidplus;
    let set = uid_set(uids);
    ops.run(ops.batch_timeout(uids), async |session| {
        if has_uidplus {
            let _: Vec<_> = session
                .uid_expunge(set)
                .await
                .map_err(proto)?
                .try_collect()
                .await
                .map_err(proto)?;
        } else {
            refuse_if_others_deleted(session, uids).await?;
            let _: Vec<_> = session
                .expunge()
                .await
                .map_err(proto)?
                .try_collect()
                .await
                .map_err(proto)?;
        }
        Ok(())
    })
}

fn move_batch(ops: &mut ImapOps, uids: &[u32], to: &str) -> MailResult<()> {
    let (has_move, has_uidplus) = (ops.has_move, ops.has_uidplus);
    let set = uid_set(uids);
    if has_move {
        return ops.run(ops.batch_timeout(uids), async |session| {
            session.uid_mv(set, to).await.map_err(proto)
        });
    }
    ops.run(ops.batch_timeout(uids), async |session| {
        // Before the copy: once copied, a refused expunge would leave the messages in both folders.
        if !has_uidplus {
            refuse_if_others_deleted(session, uids).await?;
        }
        session.uid_copy(set, to).await.map_err(proto)
    })?;
    delete_copied(ops, uids, to)
}

fn delete_copied(ops: &mut impl MailOps, uids: &[u32], to: &str) -> MailResult<()> {
    let Err(err) = super::delete_messages(ops, uids) else {
        return Ok(());
    };
    let kept = |reason: String| format!("copied to {to}, original kept: {reason}");
    Err(match err {
        MailError::Connect(reason) => MailError::Connect(kept(reason)),
        MailError::Auth(reason) => MailError::Auth(kept(reason)),
        MailError::Protocol(reason) => MailError::Protocol(kept(reason)),
        MailError::Io(reason) => MailError::Io(kept(reason)),
    })
}

/// Without UIDPLUS only a plain EXPUNGE exists, which would also remove every other message marked deleted here.
///
/// ponytail: another client can still flag a message between this search and the EXPUNGE.
async fn refuse_if_others_deleted(session: &mut ImapSession, uids: &[u32]) -> MailResult<()> {
    let deleted = session.uid_search("DELETED").await.map_err(proto)?;
    let others = deleted.iter().filter(|other| !uids.contains(other)).count();
    if others > 0 {
        return Err(MailError::Protocol(format!(
            "refusing to expunge uid {}: the server lacks UIDPLUS, so EXPUNGE would also remove {others} other \
             message(s) marked deleted in this folder",
            uid_set(uids)
        )));
    }
    Ok(())
}

/// Uids per command: a set of this many stays far below the command lengths servers accept, even when no two are
/// adjacent.
const UID_BATCH: usize = 500;

/// A uid set for a command: ascending, with runs written as ranges, `1:3,7,9:10`.
fn uid_set(uids: &[u32]) -> String {
    let mut sorted = uids.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let mut parts: Vec<String> = Vec::new();
    let mut run: Option<(u32, u32)> = None;
    for uid in sorted {
        run = match run {
            Some((first, last)) if last.checked_add(1) == Some(uid) => Some((first, uid)),
            Some(done) => {
                parts.push(range(done));
                Some((uid, uid))
            }
            None => Some((uid, uid)),
        };
    }
    parts.extend(run.map(range));
    parts.join(",")
}

fn range((first, last): (u32, u32)) -> String {
    if first == last {
        first.to_string()
    } else {
        format!("{first}:{last}")
    }
}

/// The timer loop for servers without IDLE: sleeps in short ticks so shutdown stays responsive.
fn sleep_until(timeout: Duration, interrupt: &AtomicBool) -> IdleOutcome {
    let deadline = Instant::now() + timeout;
    loop {
        if interrupt.load(Ordering::Acquire) {
            return IdleOutcome::Interrupted;
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return IdleOutcome::Timeout;
        }
        std::thread::sleep(left.min(Duration::from_millis(50)));
    }
}

fn store_flags(ops: &mut ImapOps, uids: &[u32], sign: char, flags: &[&str]) -> MailResult<()> {
    let set = uid_set(uids);
    ops.run(ops.batch_timeout(uids), async |session| {
        let query = format!("{sign}FLAGS.SILENT ({})", flags.join(" "));
        let _: Vec<_> = session
            .uid_store(set, query)
            .await
            .map_err(proto)?
            .try_collect()
            .await
            .map_err(proto)?;
        Ok(())
    })
}

/// `UID SEARCH` criteria that every word must match, as quoted strings.
///
/// ponytail: non-ASCII goes in a quoted string, which Dovecot, Gmail and Fastmail accept; a strict RFC 3501 server wants a
/// literal, which async-imap's `uid_search` cannot send.
fn text_query(words: &[String]) -> String {
    let criteria: Vec<String> = words
        .iter()
        .map(|word| {
            format!(
                "TEXT \"{}\"",
                word.replace('\\', "\\\\").replace('"', "\\\"")
            )
        })
        .collect();
    format!("CHARSET UTF-8 {}", criteria.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_plain_no_to_login_means_rejected_credentials() {
        use async_imap::error::Error;
        let no = |info: &str| Error::No(format!("code: None, info: Some({info:?})"));
        for rejected in [
            no("[AUTHENTICATIONFAILED] Authentication failed."),
            no("Invalid credentials"),
            no("[ALERT] Application-specific password required"),
        ] {
            assert!(matches!(login_error(rejected), MailError::Auth(_)));
        }
        for passing in [
            no("[UNAVAILABLE] Temporary authentication failure."),
            no("[unavailable] backend down"),
            no("[SERVERBUG] Internal error"),
            no("[LIMIT] Too many connections"),
            no("[INUSE] Mailbox in use"),
            Error::ConnectionLost,
            Error::Bad("code: None, info: Some(\"LOGIN disabled\")".into()),
            Error::Io(std::io::Error::other("reset")),
        ] {
            assert!(matches!(login_error(passing), MailError::Connect(_)));
        }
    }

    #[test]
    fn a_uid_set_writes_runs_as_ranges_in_order() {
        assert_eq!(uid_set(&[7]), "7");
        assert_eq!(uid_set(&[9, 1, 2, 3, 7, 10, 2]), "1:3,7,9:10");
        assert_eq!(
            uid_set(&[u32::MAX, u32::MAX - 1]),
            format!("{}:{}", u32::MAX - 1, u32::MAX)
        );
    }
    use crate::config::PasswordSource;
    use crate::mail_ops::RecordingOps;
    use async_imap::imap_proto::{AttributeValue, Response};

    /// The structure a server's BODYSTRUCTURE reply describes.
    fn structure(bodystructure: &str) -> Structure {
        let line = format!("* 1 FETCH (UID 1 BODYSTRUCTURE {bodystructure})\r\n");
        let (_, response) = Response::parse(line.as_bytes()).unwrap_or_else(|e| panic!("{e}"));
        let Response::Fetch(_, attrs) = response else {
            panic!("not a fetch: {response:?}");
        };
        attrs
            .iter()
            .find_map(|attr| match attr {
                AttributeValue::BodyStructure(body) => Some(structure_of(body)),
                _ => None,
            })
            .unwrap()
    }

    fn sections(structure: &Structure) -> Vec<(&str, bool, Option<usize>)> {
        structure
            .text_parts
            .iter()
            .map(|p| (p.section.as_str(), p.html, p.alternative))
            .collect()
    }

    const PLAIN: &str =
        r#"("TEXT" "PLAIN" ("CHARSET" "utf-8") NIL NIL "7BIT" 12 1 NIL NIL NIL NIL)"#;
    const HTML: &str =
        r#"("TEXT" "HTML" ("CHARSET" "utf-8") NIL NIL "QUOTED-PRINTABLE" 30 1 NIL NIL NIL NIL)"#;
    const PDF: &str = r#"("APPLICATION" "PDF" ("NAME" "Q3 report.pdf") NIL NIL "BASE64" 1024 NIL ("ATTACHMENT" ("FILENAME" "Q3 report.pdf")) NIL NIL)"#;

    #[test]
    fn a_single_text_part_is_the_whole_body() {
        let s = structure(
            r#"("TEXT" "HTML" ("CHARSET" "iso-8859-1") NIL NIL "BASE64" 40 1 NIL NIL NIL NIL)"#,
        );
        assert_eq!(sections(&s), [("", true, None)]);
        assert_eq!(s.text_parts[0].charset.as_deref(), Some("iso-8859-1"));
        assert_eq!(s.text_parts[0].transfer_encoding.as_deref(), Some("base64"));
        assert_eq!(s.text_parts[0].octets, 40);
        assert!(s.attachment_names.is_empty());
    }

    #[test]
    fn an_alternative_groups_its_parts_and_mixed_names_its_attachments() {
        let s = structure(&format!(
            r#"({PLAIN}{HTML} "ALTERNATIVE" ("BOUNDARY" "b1") NIL NIL NIL)"#
        ));
        assert_eq!(sections(&s), [("1", false, Some(1)), ("2", true, Some(1))]);
        assert_eq!(
            s.text_parts[0].transfer_encoding, None,
            "7bit is the default"
        );

        let s = structure(&format!(
            r#"({PLAIN}{PDF} "MIXED" ("BOUNDARY" "b2") NIL NIL NIL)"#
        ));
        assert_eq!(sections(&s), [("1", false, None)]);
        assert_eq!(s.attachment_names, ["Q3 report.pdf"]);

        let s = structure(&format!(
            r#"(({PLAIN}{HTML} "ALTERNATIVE" ("BOUNDARY" "b1") NIL NIL NIL)("IMAGE" "PNG" ("NAME" "x.png") NIL NIL "BASE64" 10 NIL NIL NIL NIL) "MIXED" ("BOUNDARY" "b2") NIL NIL NIL)"#
        ));
        assert_eq!(
            sections(&s),
            [("1.1", false, Some(1)), ("1.2", true, Some(1))]
        );
        assert_eq!(
            s.attachment_names,
            ["x.png"],
            "a name without a disposition still counts"
        );
    }

    #[test]
    fn a_message_without_text_has_only_names_and_a_text_attachment_is_not_body() {
        let s = structure(PDF);
        assert!(s.text_parts.is_empty());
        assert_eq!(s.attachment_names, ["Q3 report.pdf"]);

        let s = structure(&format!(
            r#"({PLAIN}("TEXT" "PLAIN" ("CHARSET" "utf-8" "NAME" "notes.txt") NIL NIL "7BIT" 99 4 NIL ("ATTACHMENT" ("FILENAME" "notes.txt")) NIL NIL) "MIXED" ("BOUNDARY" "b") NIL NIL NIL)"#
        ));
        assert_eq!(sections(&s), [("1", false, None)]);
        assert_eq!(s.attachment_names, ["notes.txt"]);
    }

    #[test]
    fn encoded_file_names_are_decoded() {
        let s = structure(
            r#"("APPLICATION" "PDF" NIL NIL NIL "BASE64" 10 NIL ("ATTACHMENT" ("FILENAME" "=?utf-8?Q?r=C3=A9sum=C3=A9.pdf?=")) NIL NIL)"#,
        );
        assert_eq!(s.attachment_names, ["résumé.pdf"]);
        let s = structure(
            r#"("APPLICATION" "PDF" NIL NIL NIL "BASE64" 10 NIL ("ATTACHMENT" ("FILENAME*0*" "utf-8''r%C3%A9sum" "FILENAME*1*" "%C3%A9.pdf")) NIL NIL)"#,
        );
        assert_eq!(s.attachment_names, ["résumé.pdf"]);
    }

    #[test]
    fn section_fetches_name_the_part() {
        assert_eq!(
            section_fetch("").unwrap(),
            (
                "BODY.PEEK[TEXT]".into(),
                SectionPath::Full(MessageSection::Text)
            )
        );
        assert_eq!(
            section_fetch("1.2").unwrap(),
            ("BODY.PEEK[1.2]".into(), SectionPath::Part(vec![1, 2], None))
        );
        assert!(section_fetch("1.TEXT").is_err());
    }

    #[test]
    fn a_refused_expunge_after_the_copy_clears_deleted_again() {
        let mut ops = RecordingOps::default().with_folder("INBOX", None);
        ops.add_mail("INBOX", 1, 0, "Subject: a\r\n\r\n", None);
        ops.select("INBOX").unwrap();
        ops.fail_next_expunge = Some(MailError::Protocol("refused".into()));
        let err = delete_copied(&mut ops, &[1], "Archive")
            .unwrap_err()
            .to_string();
        assert!(err.contains("copied to Archive, original kept"), "{err}");
        assert!(err.contains("refused"), "{err}");
        assert!(ops.mail["INBOX"][0].flags.is_empty(), "{:?}", ops.calls);
    }

    #[test]
    fn text_query_quotes_each_word() {
        let words = [
            "plain".to_string(),
            r#"a"b\c"#.to_string(),
            "café".to_string(),
        ];
        assert_eq!(
            text_query(&words),
            r#"CHARSET UTF-8 TEXT "plain" TEXT "a\"b\\c" TEXT "café""#
        );
    }

    #[test]
    fn sleep_until_times_out_or_stops_on_interrupt() {
        let stop = AtomicBool::new(false);
        assert_eq!(
            sleep_until(Duration::from_millis(10), &stop),
            IdleOutcome::Timeout
        );
        stop.store(true, Ordering::Relaxed);
        assert_eq!(
            sleep_until(Duration::from_secs(60), &stop),
            IdleOutcome::Interrupted
        );
    }

    fn test_account(host: &str, port: u16) -> AccountConfig {
        let password = PasswordSource::Keyring { keyring: true };
        AccountConfig {
            port,
            ..AccountConfig::new("test", host, "me@example.com", password)
        }
    }

    const TEST_CA: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/dovecot/certs/ca.pem"
    );
    const TEST_CERT: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/dovecot/certs/tls.crt"
    );
    const TEST_KEY: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/dovecot/certs/tls.key"
    );

    #[test]
    fn root_store_adds_the_ca_file_to_the_public_roots() {
        let public = root_store(None).unwrap().len();
        assert_eq!(
            root_store(Some(Path::new(TEST_CA))).unwrap().len(),
            public + 1
        );
    }

    #[test]
    fn root_store_rejects_a_missing_or_empty_ca_file() {
        let dir = tempfile::tempdir().unwrap();
        let empty = dir.path().join("empty.pem");
        std::fs::write(&empty, "not a certificate\n").unwrap();
        for path in [dir.path().join("missing.pem"), empty] {
            match root_store(Some(&path)) {
                Err(MailError::Connect(message)) => {
                    assert!(
                        message.contains("ca_file") && message.contains(&*path.to_string_lossy()),
                        "{message}"
                    )
                }
                other => panic!(
                    "{path:?}: expected a ca_file error, got {:?}",
                    other.map(|r| r.len())
                ),
            }
        }
    }

    #[test]
    fn connect_gives_up_on_a_server_that_never_answers() {
        // The kernel completes the TCP handshake from the backlog; nobody ever answers the TLS hello.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let started = Instant::now();
        let result = ImapOps::connect_within(
            &test_account("127.0.0.1", port),
            &Secret::new("x".into()),
            Duration::from_millis(300),
        );
        match result {
            Err(MailError::Connect(message)) => {
                assert!(message.contains("did not answer"), "{message}")
            }
            Err(other) => panic!("wrong error: {other}"),
            Ok(_) => panic!("connected to a silent server"),
        }
        assert!(started.elapsed() < Duration::from_secs(5));
        drop(listener);
    }

    /// Answers LOGIN and CAPABILITY over TLS with the test certificate, then holds every later command unanswered.
    fn serve_login_then_stall(listener: std::net::TcpListener) {
        use std::io::{BufRead, BufReader, Write};
        let certs = CertificateDer::pem_file_iter(TEST_CERT)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let key = rustls::pki_types::PrivateKeyDer::from_pem_file(TEST_KEY).unwrap();
        let config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .unwrap();
        let (tcp, _) = listener.accept().unwrap();
        let tls = rustls::ServerConnection::new(Arc::new(config)).unwrap();
        let mut stream = BufReader::new(rustls::StreamOwned::new(tls, tcp));
        stream.get_mut().write_all(b"* OK ready\r\n").unwrap();
        stream.get_mut().flush().unwrap();
        let mut line = String::new();
        while matches!(stream.read_line(&mut line), Ok(n) if n > 0) {
            let mut words = line.split_whitespace();
            let (tag, command) = (words.next().unwrap_or(""), words.next().unwrap_or(""));
            let reply = match command.to_ascii_uppercase().as_str() {
                "LOGIN" => format!("{tag} OK logged in\r\n"),
                "CAPABILITY" => format!("* CAPABILITY IMAP4rev1\r\n{tag} OK done\r\n"),
                _ => String::new(),
            };
            let _ = stream.get_mut().write_all(reply.as_bytes());
            let _ = stream.get_mut().flush();
            line.clear();
        }
    }

    #[test]
    fn a_command_the_server_never_answers_gives_up_as_a_lost_connection() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || serve_login_then_stall(listener));
        let account = AccountConfig {
            ca_file: Some(TEST_CA.into()),
            ..test_account("127.0.0.1", port)
        };
        let mut ops =
            ImapOps::connect_within(&account, &Secret::new("x".into()), Duration::from_secs(5))
                .unwrap();
        ops.command_timeout = Duration::from_millis(300);
        let started = Instant::now();
        match ops.list_folders() {
            Err(MailError::Io(message)) => assert!(message.contains("unknown"), "{message}"),
            other => panic!("expected a lost connection, got {other:?}"),
        }
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(
            matches!(ops.select("INBOX"), Err(MailError::Io(_))),
            "the stalled session was reused"
        );
        drop(ops);
        server.join().unwrap();
    }

    #[test]
    fn keepalive_is_enabled_on_the_socket() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let tcp = TcpStream::connect(addr).await.unwrap();
            enable_keepalive(&tcp).unwrap();
            let socket = socket2::SockRef::from(&tcp);
            assert!(socket.keepalive().unwrap());
            assert_eq!(socket.tcp_keepalive_time().unwrap(), KEEPALIVE_IDLE);
        });
    }
}
