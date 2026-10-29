use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use async_imap::extensions::idle::IdleResponse;
use async_imap::imap_proto::{
    BodyContentCommon, BodyContentSinglePart, BodyStructure, Capability, ContentEncoding,
    MailboxDatum, MessageSection, Response, ResponseCode, SectionPath, StatusAttribute,
};
use async_imap::types::UnsolicitedResponse;
use async_imap::types::{Capabilities, Fetch, Flag, NameAttribute};
use async_imap::{Client, Session};
use futures_util::TryStreamExt;
use rustls::pki_types::{CertificateDer, pem::PemObject};
use tokio::net::TcpStream;
use tokio::runtime::Runtime;

use crate::config::{AccountConfig, Tls};
use crate::credentials::Secret;
use crate::mail_ops::{
    Envelope, FlagUpdate, IdleOutcome, MailError, MailOps, MailResult, PartRef, RemoteFolder,
    SelectInfo, StatusInfo, Structure, TextPart, utf7,
};

/// TCP, TLS over TCP, or TLS over TCP after STARTTLS.
trait Transport: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + std::fmt::Debug {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + std::fmt::Debug> Transport
    for T
{
}

type ImapSession = Session<Box<dyn Transport>>;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// Where a server that has no implicit TLS port usually offers STARTTLS.
const STARTTLS_PORT: u16 = 143;
/// How long a failed login test waits for the STARTTLS port before it says nothing about it.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
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
    has_list_status: bool,
    has_special_use: bool,
    encrypted: bool,
    selected: Option<String>,
    command_timeout: Duration,
    names: FolderNames,
}

impl ImapOps {
    pub fn connect(account: &AccountConfig, secret: &Secret) -> MailResult<ImapOps> {
        ImapOps::connect_within(account, secret, CONNECT_TIMEOUT)
    }

    /// Logs in and out again, for the test before an account is added. When implicit TLS cannot connect but the host
    /// offers STARTTLS on port 143, the error says to use that.
    pub fn test_login(account: &AccountConfig, secret: &Secret) -> MailResult<()> {
        ImapOps::test_login_with(account, secret, STARTTLS_PORT)
    }

    fn test_login_with(
        account: &AccountConfig,
        secret: &Secret,
        starttls_port: u16,
    ) -> MailResult<()> {
        match ImapOps::connect(account, secret) {
            Ok(_) => Ok(()),
            Err(MailError::Connect(reason))
                if account.tls() == Tls::Implicit
                    && account.port != starttls_port
                    && offers_starttls_at(&account.host, starttls_port) =>
            {
                Err(MailError::Connect(format!(
                    "{reason}; the server offers STARTTLS on port {starttls_port}, so try that port with STARTTLS"
                )))
            }
            Err(error) => Err(error),
        }
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
        let (session, caps, encrypted) = match opened {
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
            has_list_status: caps.has_str("LIST-STATUS"),
            has_special_use: caps.has_str("SPECIAL-USE"),
            encrypted,
            selected: None,
            command_timeout: COMMAND_TIMEOUT,
            names: FolderNames::default(),
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

    /// The STATUS items a sync compares: HIGHESTMODSEQ only where the server has CONDSTORE.
    fn status_items(&self) -> &'static str {
        if self.has_condstore {
            "(MESSAGES UIDNEXT UIDVALIDITY HIGHESTMODSEQ)"
        } else {
            "(MESSAGES UIDNEXT UIDVALIDITY)"
        }
    }

    #[cfg(any(test, feature = "testing"))]
    pub fn has_list_status(&self) -> bool {
        self.has_list_status
    }

    #[cfg(any(test, feature = "testing"))]
    pub fn has_move(&self) -> bool {
        self.has_move
    }

    #[cfg(any(test, feature = "testing"))]
    pub fn delete_folder(&mut self, name: &str) -> MailResult<()> {
        let name = self.wire(name);
        self.run(self.command_timeout, async |session| {
            session.delete(&name).await.map_err(proto)
        })
    }

    /// `name` as the server spells it. Only a listing tells which names the server keeps as they came, so a name that
    /// may be one of them lists the folders first on a connection that has not yet: commands queued while offline run
    /// before the sync lists.
    fn wire(&mut self, name: &str) -> String {
        if self.names.needs_list(name) {
            if let Err(e) = self.list_folders() {
                log::debug!("could not list the folders before using {name}: {e}");
            }
            self.names.listed = true;
        }
        self.names.wire(name)
    }
}

/// How one connection spells folder names between UTF-8, which everything above `MailOps` uses, and the wire.
#[derive(Default)]
struct FolderNames {
    /// Names sent as they are rather than in modified UTF-7: those the server listed that are not modified UTF-7 (raw
    /// UTF-8, a sloppy encoding), and a modified UTF-7 one whose decoded name a raw listed folder already has.
    verbatim: HashSet<String>,
    /// Whether the folders were listed on this connection, so `verbatim` is known.
    listed: bool,
}

impl FolderNames {
    /// `name` as the server spells it: modified UTF-7 (RFC 3501 §5.1.3), unless the server listed it otherwise.
    fn wire(&self, name: &str) -> String {
        if self.verbatim.contains(name) {
            name.to_string()
        } else {
            utf7::encode(name)
        }
    }

    /// Whether `name` could be one the server keeps as it came, which is unknown until the folders are listed.
    fn needs_list(&self, name: &str) -> bool {
        !self.listed && (!name.is_ascii() || name.contains('&'))
    }

    /// The names a full listing gave, in UTF-8 and in its order. One that is not modified UTF-7 is kept as it came. When
    /// a raw name and a modified UTF-7 one decode alike, the raw one keeps the name and the other its wire form, so
    /// both stay reachable.
    fn learn(&mut self, wire: Vec<String>) -> Vec<String> {
        let decoded: Vec<Option<String>> = wire.iter().map(|name| utf7::decode(name)).collect();
        let raw: HashSet<&str> = wire
            .iter()
            .zip(&decoded)
            .filter(|(_, decoded)| decoded.is_none())
            .map(|(name, _)| name.as_str())
            .collect();
        let mut verbatim = HashSet::new();
        let local = wire
            .iter()
            .zip(&decoded)
            .map(|(name, decoded)| match decoded {
                Some(plain) if !raw.contains(plain.as_str()) => plain.clone(),
                Some(plain) => {
                    log::warn!("the server lists both {plain} and {name}, which spells the same name; showing it as {name}");
                    verbatim.insert(name.clone());
                    name.clone()
                }
                None => {
                    verbatim.insert(name.clone());
                    name.clone()
                }
            })
            .collect();
        self.verbatim = verbatim;
        self.listed = true;
        local
    }

    /// The listed folders with their names in UTF-8.
    fn decoded<T>(&mut self, listed: Vec<(RemoteFolder, T)>) -> Vec<(RemoteFolder, T)> {
        let names = self.learn(listed.iter().map(|(f, _)| f.name.clone()).collect());
        listed
            .into_iter()
            .zip(names)
            .map(|((folder, extra), name)| (RemoteFolder { name, ..folder }, extra))
            .collect()
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
) -> MailResult<(ImapSession, Capabilities, bool)> {
    let tcp = TcpStream::connect((account.host.as_str(), account.port))
        .await
        .map_err(|e| MailError::Connect(e.to_string()))?;
    enable_keepalive(&tcp).map_err(|e| MailError::Connect(e.to_string()))?;
    let tcp: Box<dyn Transport> = Box::new(tcp);
    let (client, encrypted) = match account.tls() {
        Tls::Implicit => (Client::new(tls_handshake(account, tcp).await?), true),
        mode => {
            let mut client = Client::new(tcp);
            if offers_starttls(&mut client).await? {
                client
                    .run_command_and_check_ok("STARTTLS", None)
                    .await
                    .map_err(|e| MailError::Connect(format!("STARTTLS failed: {e}")))?;
                if mode == Tls::None {
                    log::info!(
                        "{}: the server offers STARTTLS, so the login is encrypted despite tls = \"none\"",
                        account.name
                    );
                }
                // Anything the server sent before the handshake is dropped with the plain stream (RFC 3501 6.2.1).
                let tls = tls_handshake(account, client.into_inner()).await?;
                (Client::new(tls), true)
            } else if mode == Tls::None {
                (client, false)
            } else {
                return Err(MailError::Connect(format!(
                    "{}:{} does not offer STARTTLS, so Postvak did not log in",
                    account.host, account.port
                )));
            }
        }
    };
    let mut session = client
        .login(&account.username, secret.expose())
        .await
        .map_err(|(e, _)| login_error(e))?;
    let caps = session.capabilities().await.map_err(proto)?;
    if caps.has_str("ID") {
        // Some providers refuse SELECT from a client that does not say what it is (RFC 2971); the reply is not needed.
        let _ = session
            .id([
                ("name", Some("Postvak")),
                ("version", Some(env!("CARGO_PKG_VERSION"))),
            ])
            .await;
    }
    Ok((session, caps, encrypted))
}

/// TLS over `stream` with the public roots and the account's `ca_file`.
async fn tls_handshake(
    account: &AccountConfig,
    stream: Box<dyn Transport>,
) -> MailResult<Box<dyn Transport>> {
    let roots = root_store(account.ca_file.as_deref())?;
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
    let name = rustls::pki_types::ServerName::try_from(account.host.clone())
        .map_err(|e| MailError::Connect(e.to_string()))?;
    let tls = connector
        .connect(name, stream)
        .await
        .map_err(|e| MailError::Connect(e.to_string()))?;
    Ok(Box::new(tls))
}

/// Whether `host` answers on `port` in plain text and lists STARTTLS; false on any failure or after `PROBE_TIMEOUT`.
fn offers_starttls_at(host: &str, port: u16) -> bool {
    let Ok(rt) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return false;
    };
    let offered = rt.block_on(async {
        let probe = async {
            let tcp = TcpStream::connect((host, port)).await.ok()?;
            let tcp: Box<dyn Transport> = Box::new(tcp);
            offers_starttls(&mut Client::new(tcp)).await.ok()
        };
        tokio::time::timeout(PROBE_TIMEOUT, probe)
            .await
            .ok()
            .flatten()
    });
    // Dropping the runtime would wait for a blocking DNS lookup past the limit.
    rt.shutdown_background();
    offered == Some(true)
}

/// Whether the plain-text greeting or an answer to CAPABILITY lists STARTTLS.
async fn offers_starttls(client: &mut Client<Box<dyn Transport>>) -> MailResult<bool> {
    let (sender, untagged) = async_channel::unbounded();
    client
        .run_command_and_check_ok("CAPABILITY", Some(sender))
        .await
        .map_err(proto)?;
    let mut offered = false;
    while let Ok(UnsolicitedResponse::Other(response)) = untagged.try_recv() {
        let listed = match response.parsed() {
            Response::Capabilities(listed) => listed,
            Response::Data { outcome, .. } => match &outcome.code {
                Some(ResponseCode::Capabilities(listed)) => listed,
                _ => continue,
            },
            _ => continue,
        };
        offered |= listed
            .iter()
            .any(|c| matches!(c, Capability::Atom(atom) if atom.eq_ignore_ascii_case("STARTTLS")));
    }
    Ok(offered)
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

/// What the indexer and the reader need from a BODYSTRUCTURE: its plain and HTML text parts, grouped by the
/// `multipart/alternative` that holds them, the names of its attachments, and where its attachments are. A shape this
/// does not understand yields no text parts, and the caller falls back to the whole message.
fn structure_of(body: &BodyStructure<'_>) -> Structure {
    let mut structure = Structure::default();
    let mut walk = Walk {
        path: Vec::new(),
        alternative: None,
        nested: false,
        groups: 0,
    };
    walk.part(body, &mut structure);
    structure
}

/// Where `structure_of` is in the tree.
struct Walk {
    path: Vec<u32>,
    alternative: Option<usize>,
    /// Inside an enclosed message.
    nested: bool,
    groups: usize,
}

impl Walk {
    fn section(&self) -> String {
        self.path
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(".")
    }

    fn part(&mut self, body: &BodyStructure<'_>, out: &mut Structure) {
        match body {
            BodyStructure::Text { common, other, .. } => {
                let subtype = common.ty.subtype.to_ascii_lowercase();
                let text = matches!(subtype.as_str(), "plain" | "html") && !is_attachment(common);
                if text {
                    out.text_parts.push(TextPart {
                        section: self.section(),
                        html: subtype == "html",
                        charset: params_of(common)
                            .iter()
                            .find(|(k, _)| k.eq_ignore_ascii_case("charset"))
                            .map(|(_, v)| v.clone()),
                        transfer_encoding: encoding_name(&other.transfer_encoding),
                        octets: other.octets,
                        alternative: self.alternative,
                        nested: self.nested,
                    });
                } else {
                    self.attachment(common, other, out);
                }
            }
            BodyStructure::Basic { common, other, .. } => self.attachment(common, other, out),
            BodyStructure::Message {
                common,
                other,
                body,
                ..
            } => {
                self.attachment(common, other, out);
                let nested = std::mem::replace(&mut self.nested, true);
                // The parts of an enclosed message are numbered under this part; a single-part one is its part 1.
                if matches!(**body, BodyStructure::Multipart { .. }) {
                    self.part(body, out);
                } else {
                    self.path.push(1);
                    self.part(body, out);
                    self.path.pop();
                }
                self.nested = nested;
            }
            BodyStructure::Multipart { common, bodies, .. } => {
                let outer = self.alternative;
                if common.ty.subtype.eq_ignore_ascii_case("alternative") {
                    self.groups += 1;
                    self.alternative = Some(self.groups);
                }
                for (i, part) in bodies.iter().enumerate() {
                    self.path.push(i as u32 + 1);
                    self.part(part, out);
                    self.path.pop();
                }
                self.alternative = outer;
            }
        }
    }

    /// A part that is not body text: its name for the index, and outside an enclosed message, the part itself.
    fn attachment(
        &self,
        common: &BodyContentCommon<'_>,
        other: &BodyContentSinglePart<'_>,
        out: &mut Structure,
    ) {
        let name = attachment_name(common);
        if let Some(name) = &name {
            out.attachment_names.push(name.clone());
        }
        if self.nested {
            return;
        }
        let content_id = other
            .id
            .as_deref()
            .map(|id| id.trim().trim_start_matches('<').trim_end_matches('>'))
            .filter(|id| !id.is_empty())
            .map(str::to_string);
        out.attachments.push(PartRef {
            section: self.section(),
            content_type: format!("{}/{}", common.ty.ty, common.ty.subtype).to_ascii_lowercase(),
            name,
            content_id,
            transfer_encoding: encoding_name(&other.transfer_encoding),
            octets: other.octets,
        });
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

/// The folder a LIST answer names, or None for one that cannot be selected: `\Noselect`, or `\NonExistent`, which
/// implies it (RFC 5258 §3.4). Dovecot answers an extended LIST so for the parent of a new `Archief/Één`.
fn listed_folder(
    attrs: &[NameAttribute<'_>],
    delimiter: Option<&str>,
    name: &str,
) -> Option<RemoteFolder> {
    let unselectable = |a: &NameAttribute<'_>| match a {
        NameAttribute::NoSelect => true,
        NameAttribute::Extension(s) => s.eq_ignore_ascii_case("\\NonExistent"),
        _ => false,
    };
    if attrs.iter().any(unselectable) {
        return None;
    }
    Some(RemoteFolder {
        name: name.to_string(),
        special_use: special_use(attrs),
        delimiter: delimiter.map(str::to_string),
    })
}

/// A folder's status from the items of a STATUS answer; a missing number is 0.
fn status_from(attributes: &[StatusAttribute]) -> StatusInfo {
    let mut status = StatusInfo {
        messages: 0,
        uidnext: 0,
        uidvalidity: 0,
        highest_modseq: None,
    };
    for attribute in attributes {
        match attribute {
            StatusAttribute::Messages(n) => status.messages = *n,
            StatusAttribute::UidNext(n) => status.uidnext = *n,
            StatusAttribute::UidValidity(n) => status.uidvalidity = *n,
            StatusAttribute::HighestModSeq(n) => status.highest_modseq = Some(*n),
            _ => {}
        }
    }
    status
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

    fn is_encrypted(&self) -> bool {
        self.encrypted
    }

    fn list_folders(&mut self) -> MailResult<Vec<RemoteFolder>> {
        let listed = self.run(self.command_timeout, async |session| {
            let names: Vec<_> = session
                .list(Some(""), Some("*"))
                .await
                .map_err(proto)?
                .try_collect()
                .await
                .map_err(proto)?;
            Ok(names
                .iter()
                .filter_map(|n| listed_folder(n.attributes(), n.delimiter(), n.name()))
                .collect::<Vec<_>>())
        })?;
        let listed = listed.into_iter().map(|folder| (folder, ())).collect();
        Ok(self
            .names
            .decoded(listed)
            .into_iter()
            .map(|(folder, ())| folder)
            .collect())
    }

    fn list_folders_status(&mut self) -> MailResult<Vec<(RemoteFolder, Option<StatusInfo>)>> {
        if !self.has_list_status {
            let folders = self.list_folders()?;
            return Ok(folders.into_iter().map(|f| (f, None)).collect());
        }
        let special_use = if self.has_special_use {
            "SPECIAL-USE "
        } else {
            ""
        };
        let command = format!(
            "LIST \"\" \"*\" RETURN ({special_use}STATUS {})",
            self.status_items()
        );
        let listed = self.run(self.command_timeout, async |session| {
            let tag = session.run_command(&command).await.map_err(proto)?;
            let mut folders = Vec::new();
            let mut statuses = HashMap::new();
            loop {
                let response = session
                    .read_response()
                    .await
                    .map_err(|e| MailError::Io(e.to_string()))?
                    .ok_or_else(|| MailError::Io("connection closed during LIST".into()))?;
                match response.parsed() {
                    Response::MailboxData(MailboxDatum::List(list)) => folders.extend(
                        listed_folder(&list.name_attributes, list.delimiter.as_deref(), &list.name),
                    ),
                    Response::MailboxData(MailboxDatum::Status { mailbox, status }) => {
                        statuses.insert(mailbox.to_string(), status_from(status));
                    }
                    Response::Done {
                        tag: done,
                        status,
                        outcome,
                    } if *done == tag => {
                        if *status != async_imap::imap_proto::Status::Ok {
                            return Err(MailError::Protocol(format!(
                                "LIST: {status:?} {}",
                                outcome.information.as_deref().unwrap_or_default()
                            )));
                        }
                        break;
                    }
                    _ => {}
                }
            }
            Ok(folders
                .into_iter()
                .map(|folder| {
                    let status = statuses.remove(&folder.name);
                    (folder, status)
                })
                .collect::<Vec<_>>())
        })?;
        Ok(self.names.decoded(listed))
    }

    fn select(&mut self, folder: &str) -> MailResult<SelectInfo> {
        let condstore = self.has_condstore;
        let wire = self.wire(folder);
        let selected = self.run(self.command_timeout, async |session| {
            let mailbox = if condstore {
                session.select_condstore(&wire).await
            } else {
                session.select(&wire).await
            }
            .map_err(proto)?;
            Ok(SelectInfo {
                uidvalidity: mailbox.uid_validity.unwrap_or(0),
                exists: mailbox.exists,
                highest_modseq: mailbox.highest_modseq,
                uidnext: mailbox.uid_next,
            })
        });
        // A failed SELECT leaves no folder selected (RFC 3501 6.3.1).
        self.selected = selected.is_ok().then(|| folder.to_string());
        selected
    }

    fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    fn status(&mut self, folder: &str) -> MailResult<StatusInfo> {
        let items = self.status_items();
        let folder = self.wire(folder);
        self.run(self.command_timeout, async |session| {
            let mailbox = session.status(&folder, items).await.map_err(proto)?;
            Ok(StatusInfo {
                messages: mailbox.exists,
                uidnext: mailbox.uid_next.unwrap_or(0),
                uidvalidity: mailbox.uid_validity.unwrap_or(0),
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

    fn fetch_structures(&mut self, uids: &[u32]) -> MailResult<HashMap<u32, Structure>> {
        if uids.is_empty() {
            return Ok(HashMap::new());
        }
        self.run(self.batch_timeout(uids), async |session| {
            let fetches: Vec<Fetch> = session
                .uid_fetch(uid_set(uids), "(UID BODYSTRUCTURE)")
                .await
                .map_err(proto)?
                .try_collect()
                .await
                .map_err(proto)?;
            Ok(fetches
                .iter()
                .filter_map(|f| Some((f.uid?, structure_of(f.bodystructure()?))))
                .collect())
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
        let wire = self.wire(to);
        uids.chunks(UID_BATCH)
            .try_for_each(|batch| move_batch(self, batch, &wire, to))
    }

    fn create_folder(&mut self, name: &str) -> MailResult<()> {
        let name = self.wire(name);
        self.run(self.command_timeout, async |session| {
            session.create(&name).await.map_err(proto)
        })
    }

    fn subscribe_folder(&mut self, name: &str) -> MailResult<()> {
        let name = self.wire(name);
        self.run(self.command_timeout, async |session| {
            session.subscribe(&name).await.map_err(proto)
        })
    }

    fn append(&mut self, folder: &str, raw: &[u8], flags: &[&str]) -> MailResult<()> {
        let flags = (!flags.is_empty()).then(|| format!("({})", flags.join(" ")));
        let folder = self.wire(folder);
        self.run(self.bulk_timeout(), async |session| {
            session
                .append(&folder, flags.as_deref(), None, raw)
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

/// Moves `uids` to the folder `to`, which the server spells `wire`.
fn move_batch(ops: &mut ImapOps, uids: &[u32], wire: &str, to: &str) -> MailResult<()> {
    let (has_move, has_uidplus) = (ops.has_move, ops.has_uidplus);
    let set = uid_set(uids);
    if has_move {
        return ops.run(ops.batch_timeout(uids), async |session| {
            session.uid_mv(set, wire).await.map_err(proto)
        });
    }
    ops.run(ops.batch_timeout(uids), async |session| {
        // Before the copy: once copied, a refused expunge would leave the messages in both folders.
        if !has_uidplus {
            refuse_if_others_deleted(session, uids).await?;
        }
        session.uid_copy(set, wire).await.map_err(proto)
    })?;
    delete_copied(ops, uids, to)
}

fn delete_copied(ops: &mut impl MailOps, uids: &[u32], to: &str) -> MailResult<()> {
    let Err(err) = super::delete_messages(ops, uids) else {
        return Ok(());
    };
    Err(err.map_reason(|reason| format!("copied to {to}, original kept: {reason}")))
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

    fn learned(wire: &[&str]) -> (FolderNames, Vec<String>) {
        let mut names = FolderNames::default();
        let local = names.learn(wire.iter().map(|n| n.to_string()).collect());
        (names, local)
    }

    #[test]
    fn a_listed_name_that_is_not_modified_utf7_goes_back_as_it_came() {
        let (names, local) = learned(&["INBOX", "Caf\u{e9}", "R&D", "Archief/&AMkA6Q-n"]);
        assert_eq!(
            local,
            ["INBOX", "Caf\u{e9}", "R&D", "Archief/\u{c9}\u{e9}n"]
        );
        assert_eq!(names.wire("Caf\u{e9}"), "Caf\u{e9}");
        assert_eq!(names.wire("R&D"), "R&D");
        assert_eq!(names.wire("Archief/\u{c9}\u{e9}n"), "Archief/&AMkA6Q-n");
        // Not listed: a folder about to be created is sent in modified UTF-7.
        assert_eq!(names.wire("Th\u{e9}"), "Th&AOk-");
        assert_eq!(names.wire("Q&A"), "Q&-A");
    }

    /// A raw `Café` the old Postvak created on a lax server, beside another client's `Caf&AOk-`.
    #[test]
    fn a_raw_name_and_its_modified_utf7_twin_both_stay_reachable() {
        let (names, local) = learned(&["Caf&AOk-", "Caf\u{e9}"]);
        assert_eq!(local, ["Caf&AOk-", "Caf\u{e9}"]);
        assert_eq!(names.wire("Caf&AOk-"), "Caf&AOk-");
        assert_eq!(names.wire("Caf\u{e9}"), "Caf\u{e9}");
    }

    #[test]
    fn a_name_that_may_be_kept_raw_waits_for_a_listing() {
        let names = FolderNames::default();
        assert!(names.needs_list("Caf\u{e9}"));
        assert!(names.needs_list("R&D"));
        assert!(!names.needs_list("Lists/GitHub"));
        let (names, _) = learned(&["INBOX"]);
        assert!(!names.needs_list("Caf\u{e9}"));
        // A later listing forgets what the server no longer lists.
        let (mut names, _) = learned(&["Caf\u{e9}"]);
        names.learn(vec!["INBOX".into()]);
        assert_eq!(names.wire("Caf\u{e9}"), "Caf&AOk-");
    }

    #[test]
    fn a_folder_listed_as_nonexistent_or_noselect_is_left_out() {
        use std::borrow::Cow;
        let listed = |attrs: &[NameAttribute<'_>]| listed_folder(attrs, Some("/"), "Archief");
        assert_eq!(listed(&[NameAttribute::NoSelect]), None);
        for nonexistent in ["\\NonExistent", "\\nonexistent"] {
            let attrs = [
                NameAttribute::Extension(Cow::Borrowed(nonexistent)),
                NameAttribute::Extension(Cow::Borrowed("\\HasChildren")),
            ];
            assert_eq!(listed(&attrs), None);
        }
        let attrs = [NameAttribute::Extension(Cow::Borrowed("\\HasChildren"))];
        assert_eq!(listed(&attrs).map(|f| f.name).as_deref(), Some("Archief"));
    }

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
    fn attachments_are_every_part_but_the_body_text_with_an_enclosed_message_as_one() {
        let logo = r#"("IMAGE" "PNG" NIL "<logo@x>" NIL "BASE64" 400 NIL ("INLINE" NIL) NIL NIL)"#;
        let inner = r#"("MESSAGE" "RFC822" NIL NIL NIL "7BIT" 90 (NIL "inner" NIL NIL NIL NIL NIL NIL NIL NIL) (("TEXT" "PLAIN" NIL NIL NIL "7BIT" 10 1 NIL NIL NIL NIL)("TEXT" "HTML" NIL NIL NIL "7BIT" 20 1 NIL NIL NIL NIL) "ALTERNATIVE" ("BOUNDARY" "i") NIL NIL NIL) 4 NIL NIL NIL NIL)"#;
        let s = structure(&format!(
            r#"((({HTML}{logo} "RELATED" ("BOUNDARY" "r") NIL NIL NIL){PLAIN} "ALTERNATIVE" ("BOUNDARY" "a") NIL NIL NIL){PDF}{inner} "MIXED" ("BOUNDARY" "m") NIL NIL NIL)"#
        ));
        let parts: Vec<_> = s
            .attachments
            .iter()
            .map(|p| {
                (
                    p.section.as_str(),
                    p.content_type.as_str(),
                    p.name.as_deref(),
                    p.content_id.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            parts,
            [
                ("1.1.2", "image/png", None, Some("logo@x")),
                ("2", "application/pdf", Some("Q3 report.pdf"), None),
                ("3", "message/rfc822", None, None),
            ]
        );
        assert_eq!(
            s.attachments[0].transfer_encoding.as_deref(),
            Some("base64")
        );
        assert_eq!(s.attachments[0].octets, 400);
        let text: Vec<_> = s
            .text_parts
            .iter()
            .map(|p| (p.section.as_str(), p.html, p.alternative, p.nested))
            .collect();
        assert_eq!(
            text,
            [
                ("1.1.1", true, Some(1), false),
                ("1.2", false, Some(1), false),
                ("3.1", false, Some(2), true),
                ("3.2", true, Some(2), true),
            ]
        );
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

    fn test_server_tls() -> rustls::ServerConnection {
        let certs = CertificateDer::pem_file_iter(TEST_CERT)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let key = rustls::pki_types::PrivateKeyDer::from_pem_file(TEST_KEY).unwrap();
        let config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .unwrap();
        rustls::ServerConnection::new(Arc::new(config)).unwrap()
    }

    /// Answers LOGIN and CAPABILITY over TLS with the test certificate, then holds every later command unanswered.
    fn serve_login_then_stall(listener: std::net::TcpListener) {
        use std::io::{BufRead, BufReader, Write};
        let (tcp, _) = listener.accept().unwrap();
        let tls = test_server_tls();
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

    /// A plain-text server listing `capabilities` that answers every command OK, and STARTTLS by switching to TLS with
    /// the test certificate. Returns the commands it read, those over TLS as `tls COMMAND`.
    fn serve_plain(listener: std::net::TcpListener, capabilities: &str) -> Vec<String> {
        use std::io::{BufReader, Write};
        let (tcp, _) = listener.accept().unwrap();
        let mut seen = Vec::new();
        let mut plain = BufReader::new(tcp);
        plain.get_mut().write_all(b"* OK ready\r\n").unwrap();
        if answer(&mut plain, capabilities, "", &mut seen) {
            let tls = rustls::StreamOwned::new(test_server_tls(), plain.into_inner());
            answer(&mut BufReader::new(tls), capabilities, "tls ", &mut seen);
        }
        seen
    }

    /// Answers until the client leaves, or until STARTTLS, which returns true.
    fn answer(
        stream: &mut std::io::BufReader<impl std::io::Read + std::io::Write>,
        capabilities: &str,
        prefix: &str,
        seen: &mut Vec<String>,
    ) -> bool {
        use std::io::BufRead;
        let mut line = String::new();
        while matches!(stream.read_line(&mut line), Ok(n) if n > 0) {
            let mut words = line.split_whitespace();
            let tag = words.next().unwrap_or("").to_string();
            let command = words.next().unwrap_or("").to_ascii_uppercase();
            seen.push(format!("{prefix}{command}"));
            let listed = match command.as_str() {
                "CAPABILITY" => format!("* CAPABILITY IMAP4rev1 {capabilities}\r\n"),
                _ => String::new(),
            };
            let _ = write!(stream.get_mut(), "{listed}{tag} OK done\r\n");
            let _ = stream.get_mut().flush();
            if command == "STARTTLS" {
                return true;
            }
            line.clear();
        }
        false
    }

    /// Connects with `tls` to `serve_plain` and returns the result and the commands the server read.
    fn connect_plain(tls: Tls, capabilities: &'static str) -> (MailResult<bool>, Vec<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || serve_plain(listener, capabilities));
        let account = AccountConfig {
            ca_file: Some(TEST_CA.into()),
            tls: Some(tls),
            ..test_account("127.0.0.1", port)
        };
        let result =
            ImapOps::connect_within(&account, &Secret::new("x".into()), Duration::from_secs(5))
                .map(|ops| ops.is_encrypted());
        (result, server.join().unwrap())
    }

    #[test]
    fn starttls_upgrades_before_the_login_and_asks_the_capabilities_again() {
        for tls in [Tls::Starttls, Tls::None] {
            let (result, seen) = connect_plain(tls, "STARTTLS");
            assert!(matches!(result, Ok(true)), "{tls:?}: {result:?}");
            assert_eq!(
                seen,
                ["CAPABILITY", "STARTTLS", "tls LOGIN", "tls CAPABILITY"],
                "{tls:?}"
            );
        }
    }

    #[test]
    fn starttls_refuses_a_server_that_does_not_offer_it_before_sending_the_password() {
        let (result, seen) = connect_plain(Tls::Starttls, "LOGINDISABLED");
        match result {
            Err(MailError::Connect(message)) => assert!(message.contains("STARTTLS"), "{message}"),
            other => panic!("expected a refusal, got {other:?}"),
        }
        assert_eq!(seen, ["CAPABILITY"]);
    }

    #[test]
    fn tls_none_logs_in_in_plain_text_only_where_starttls_is_not_offered() {
        let (result, seen) = connect_plain(Tls::None, "");
        assert!(matches!(result, Ok(false)), "{result:?}");
        assert_eq!(seen, ["CAPABILITY", "LOGIN", "CAPABILITY"]);
    }

    #[test]
    fn the_client_says_its_name_only_to_a_server_that_offers_id() {
        let (_, seen) = connect_plain(Tls::Starttls, "STARTTLS ID");
        assert!(seen.contains(&"tls ID".to_string()), "{seen:?}");
        let (_, seen) = connect_plain(Tls::Starttls, "STARTTLS");
        assert!(!seen.iter().any(|c| c.ends_with("ID")), "{seen:?}");
    }

    #[test]
    fn a_failed_login_test_over_tls_points_to_starttls_where_it_is_offered() {
        let test = |capabilities: &'static str| {
            // Answers TLS's hello in plain text, so implicit TLS cannot connect.
            let plain = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = plain.local_addr().unwrap().port();
            let probed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let starttls_port = probed.local_addr().unwrap().port();
            let servers = [
                std::thread::spawn(move || serve_plain(plain, "")),
                std::thread::spawn(move || serve_plain(probed, capabilities)),
            ];
            let account = AccountConfig {
                ca_file: Some(TEST_CA.into()),
                ..test_account("127.0.0.1", port)
            };
            let result =
                ImapOps::test_login_with(&account, &Secret::new("x".into()), starttls_port);
            for server in servers {
                server.join().unwrap();
            }
            match result {
                Err(MailError::Connect(message)) => message,
                other => panic!("expected a failed connect, got {other:?}"),
            }
        };
        assert!(test("STARTTLS").contains("offers STARTTLS on port"));
        assert!(!test("").contains("STARTTLS"));
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
