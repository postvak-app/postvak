use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use async_imap::Session;
use async_imap::extensions::idle::IdleResponse;
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
        .map_err(|(e, _)| MailError::Auth(e.to_string()))?;
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

    fn add_flags(&mut self, uid: u32, flags: &[&str]) -> MailResult<()> {
        store_flags(self, uid, '+', flags)
    }

    fn remove_flags(&mut self, uid: u32, flags: &[&str]) -> MailResult<()> {
        store_flags(self, uid, '-', flags)
    }

    fn expunge(&mut self, uid: u32) -> MailResult<()> {
        let has_uidplus = self.has_uidplus;
        self.run(self.command_timeout, async |session| {
            if has_uidplus {
                let _: Vec<_> = session
                    .uid_expunge(uid.to_string())
                    .await
                    .map_err(proto)?
                    .try_collect()
                    .await
                    .map_err(proto)?;
            } else {
                refuse_if_others_deleted(session, uid).await?;
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

    fn move_message(&mut self, uid: u32, to: &str) -> MailResult<()> {
        let (has_move, has_uidplus) = (self.has_move, self.has_uidplus);
        if has_move {
            return self.run(self.command_timeout, async |session| {
                session.uid_mv(uid.to_string(), to).await.map_err(proto)
            });
        }
        self.run(self.command_timeout, async |session| {
            // Before the copy: once copied, a refused expunge would leave the message in both folders.
            if !has_uidplus {
                refuse_if_others_deleted(session, uid).await?;
            }
            session.uid_copy(uid.to_string(), to).await.map_err(proto)
        })?;
        self.add_flags(uid, &["\\Deleted"])?;
        self.expunge(uid)
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

/// Without UIDPLUS only a plain EXPUNGE exists, which would also remove every other message marked deleted here.
///
/// ponytail: another client can still flag a message between this search and the EXPUNGE.
async fn refuse_if_others_deleted(session: &mut ImapSession, uid: u32) -> MailResult<()> {
    let deleted = session.uid_search("DELETED").await.map_err(proto)?;
    let others = deleted.iter().filter(|&&other| other != uid).count();
    if others > 0 {
        return Err(MailError::Protocol(format!(
            "refusing to expunge uid {uid}: the server lacks UIDPLUS, so EXPUNGE would also remove {others} other \
             message(s) marked deleted in this folder"
        )));
    }
    Ok(())
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

fn store_flags(ops: &mut ImapOps, uid: u32, sign: char, flags: &[&str]) -> MailResult<()> {
    ops.run(ops.command_timeout, async |session| {
        let query = format!("{sign}FLAGS.SILENT ({})", flags.join(" "));
        let _: Vec<_> = session
            .uid_store(uid.to_string(), query)
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
    use crate::config::PasswordSource;

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
