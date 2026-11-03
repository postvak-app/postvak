//! The SMTP client: [`SmtpOps`], its real implementation over lettre ([`Lettre`]) and the recording fake tests use.

use std::path::Path;
use std::time::Duration;

use lettre::transport::smtp::authentication::{Credentials, Mechanism};
use lettre::transport::smtp::client::{
    AsyncSmtpConnection, Certificate, CertificateStore, TlsParameters,
};
use lettre::transport::smtp::commands::Ehlo;
use lettre::transport::smtp::extension::ClientId;

use super::{Capabilities, Envelope, SmtpServer};
use crate::config::SmtpTls;
use crate::credentials::Secret;

/// Why an SMTP connection or send did not go through.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SmtpError {
    /// The server said to try again later (a 4xx reply).
    #[error("the server said to try again later: {0}")]
    Temporary(String),
    /// The server refused for good (a 5xx reply), such as a refused recipient.
    #[error("the server refused the message: {0}")]
    Permanent(String),
    /// The login was refused.
    #[error("the SMTP login was refused: {0}")]
    Auth(String),
    /// The server could not be reached, or TLS failed.
    #[error("could not connect to the SMTP server: {0}")]
    Connect(String),
}

impl SmtpError {
    /// Whether trying again later may work.
    pub fn is_temporary(&self) -> bool {
        matches!(self, SmtpError::Temporary(_) | SmtpError::Connect(_))
    }
}

/// An SMTP client. Each call opens a connection of its own, encrypted before anything else is said.
pub trait SmtpOps {
    /// Connects, says EHLO and logs in; sends nothing.
    fn check_login(
        &mut self,
        server: &SmtpServer,
        secret: &Secret,
        ca_file: Option<&Path>,
    ) -> Result<Capabilities, SmtpError>;

    /// Sends `raw` to the envelope's recipients: every recipient is accepted before the message is, or none gets it.
    fn send(
        &mut self,
        server: &SmtpServer,
        secret: &Secret,
        ca_file: Option<&Path>,
        envelope: &Envelope,
        raw: &[u8],
    ) -> Result<Capabilities, SmtpError>;
}

/// How long a login check may take.
const CHECK_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a send may take, a large message on a slow line included.
const SEND_TIMEOUT: Duration = Duration::from_secs(300);
/// How long the server may stay silent on one command.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(60);

/// The real client, over lettre on a current-thread tokio runtime of its own, as the IMAP side does.
#[derive(Debug, Default, Clone, Copy)]
pub struct Lettre;

impl SmtpOps for Lettre {
    fn check_login(
        &mut self,
        server: &SmtpServer,
        secret: &Secret,
        ca_file: Option<&Path>,
    ) -> Result<Capabilities, SmtpError> {
        run(CHECK_TIMEOUT, server, async {
            let (mut conn, caps) = open(server, secret, ca_file).await?;
            let _ = conn.quit().await;
            Ok(caps)
        })
    }

    fn send(
        &mut self,
        server: &SmtpServer,
        secret: &Secret,
        ca_file: Option<&Path>,
        envelope: &Envelope,
        raw: &[u8],
    ) -> Result<Capabilities, SmtpError> {
        let envelope = lettre_envelope(envelope)?;
        run(SEND_TIMEOUT, server, async {
            let (mut conn, caps) = open(server, secret, ca_file).await?;
            if let Some(limit) = caps.size
                && raw.len() as u64 > limit
            {
                let _ = conn.quit().await;
                return Err(SmtpError::Permanent(format!(
                    "the message is {} bytes and the server takes at most {limit}",
                    raw.len()
                )));
            }
            conn.send(&envelope, raw)
                .await
                .map_err(|e| classify_error(&e))?;
            let _ = conn.quit().await;
            Ok(caps)
        })
    }
}

/// Runs `work` on a runtime of its own, giving up after `limit`.
fn run<T>(
    limit: Duration,
    server: &SmtpServer,
    work: impl Future<Output = Result<T, SmtpError>>,
) -> Result<T, SmtpError> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| SmtpError::Connect(e.to_string()))?;
    rt.block_on(async {
        tokio::time::timeout(limit, work).await.unwrap_or_else(|_| {
            Err(SmtpError::Connect(format!(
                "{}:{} did not finish within {limit:?}",
                server.host, server.port
            )))
        })
    })
}

fn lettre_envelope(envelope: &Envelope) -> Result<lettre::address::Envelope, SmtpError> {
    let address = |text: &str| {
        text.parse::<lettre::Address>()
            .map_err(|e| SmtpError::Permanent(format!("'{text}' is not an address: {e}")))
    };
    let from = address(&envelope.from)?;
    let to = envelope
        .to
        .iter()
        .map(|to| address(to))
        .collect::<Result<Vec<_>, _>>()?;
    lettre::address::Envelope::new(Some(from), to).map_err(|e| SmtpError::Permanent(e.to_string()))
}

/// Connects, encrypts, says EHLO and logs in.
async fn open(
    server: &SmtpServer,
    secret: &Secret,
    ca_file: Option<&Path>,
) -> Result<(AsyncSmtpConnection, Capabilities), SmtpError> {
    let hello = ClientId::default();
    let tls = tls_parameters(&server.host, ca_file)?;
    let address = (server.host.as_str(), server.port);
    let mut conn = match server.tls {
        SmtpTls::Implicit => AsyncSmtpConnection::connect_tokio1(
            address,
            Some(COMMAND_TIMEOUT),
            &hello,
            Some(tls),
            None,
        )
        .await
        .map_err(|e| connect_error(&e))?,
        SmtpTls::Starttls => {
            let mut conn = AsyncSmtpConnection::connect_tokio1(
                address,
                Some(COMMAND_TIMEOUT),
                &hello,
                None,
                None,
            )
            .await
            .map_err(|e| connect_error(&e))?;
            if !conn.can_starttls() {
                let _ = conn.quit().await;
                return Err(SmtpError::Connect(format!(
                    "{}:{} does not offer STARTTLS, so the login would not be encrypted",
                    server.host, server.port
                )));
            }
            conn.starttls(tls, &hello)
                .await
                .map_err(|e| connect_error(&e))?;
            conn
        }
    };
    // lettre keeps only the EHLO keywords it knows; ask again for SIZE.
    let ehlo = conn
        .command(Ehlo::new(hello))
        .await
        .map_err(|e| classify_error(&e))?;
    let caps = capabilities(ehlo.message());
    let credentials = Credentials::new(server.username.clone(), secret.expose().to_string());
    conn.auth(&[Mechanism::Plain, Mechanism::Login], &credentials)
        .await
        .map_err(|e| match classify_error(&e) {
            temporary @ SmtpError::Temporary(_) => temporary,
            other => SmtpError::Auth(other.reason().to_string()),
        })?;
    Ok((conn, caps))
}

fn tls_parameters(host: &str, ca_file: Option<&Path>) -> Result<TlsParameters, SmtpError> {
    let mut builder =
        TlsParameters::builder(host.to_string()).certificate_store(CertificateStore::WebpkiRoots);
    if let Some(path) = ca_file {
        let invalid =
            |reason: String| SmtpError::Connect(format!("ca_file {}: {reason}", path.display()));
        let pem = std::fs::read(path).map_err(|e| invalid(e.to_string()))?;
        let certificate = Certificate::from_pem(&pem).map_err(|e| invalid(e.to_string()))?;
        builder = builder.add_root_certificate(certificate);
    }
    builder
        .build_rustls()
        .map_err(|e| SmtpError::Connect(e.to_string()))
}

/// SIZE and 8BITMIME from the lines of an EHLO reply.
fn capabilities<'a>(lines: impl Iterator<Item = &'a str>) -> Capabilities {
    let mut caps = Capabilities::default();
    for line in lines {
        let mut words = line.split_whitespace();
        match words.next().map(str::to_ascii_uppercase).as_deref() {
            Some("SIZE") => {
                caps.size = words
                    .next()
                    .and_then(|size| size.parse::<u64>().ok())
                    .filter(|size| *size > 0);
            }
            Some("8BITMIME") => caps.eight_bit_mime = true,
            _ => {}
        }
    }
    caps
}

impl SmtpError {
    fn reason(&self) -> &str {
        match self {
            SmtpError::Temporary(reason)
            | SmtpError::Permanent(reason)
            | SmtpError::Auth(reason)
            | SmtpError::Connect(reason) => reason,
        }
    }
}

/// An error before the server said anything to the message: reaching it, or TLS.
fn connect_error(e: &lettre::transport::smtp::Error) -> SmtpError {
    match e.status() {
        Some(_) => classify_error(e),
        None => SmtpError::Connect(e.to_string()),
    }
}

fn classify_error(e: &lettre::transport::smtp::Error) -> SmtpError {
    match e.status() {
        Some(code) => classify(u16::from(code), &e.to_string()),
        None if e.is_client() => SmtpError::Permanent(e.to_string()),
        None => SmtpError::Connect(e.to_string()),
    }
}

/// What a reply code means for the message: try again later, the login, or refused for good.
pub(crate) fn classify(code: u16, text: &str) -> SmtpError {
    match code {
        530 | 534 | 535 | 538 => SmtpError::Auth(text.into()),
        400..=499 => SmtpError::Temporary(text.into()),
        _ => SmtpError::Permanent(text.into()),
    }
}

/// A fake for tests: records what it was asked and answers from `script`, in order, then with success.
#[cfg(any(test, feature = "testing"))]
#[derive(Debug, Default)]
pub struct RecordingSmtp {
    pub sent: Vec<(SmtpServer, Envelope, Vec<u8>)>,
    pub logins: Vec<SmtpServer>,
    pub script: std::collections::VecDeque<Result<Capabilities, SmtpError>>,
}

#[cfg(any(test, feature = "testing"))]
impl RecordingSmtp {
    fn next(&mut self) -> Result<Capabilities, SmtpError> {
        self.script
            .pop_front()
            .unwrap_or(Ok(Capabilities::default()))
    }
}

#[cfg(any(test, feature = "testing"))]
impl SmtpOps for RecordingSmtp {
    fn check_login(
        &mut self,
        server: &SmtpServer,
        _secret: &Secret,
        _ca_file: Option<&Path>,
    ) -> Result<Capabilities, SmtpError> {
        self.logins.push(server.clone());
        self.next()
    }

    fn send(
        &mut self,
        server: &SmtpServer,
        _secret: &Secret,
        _ca_file: Option<&Path>,
        envelope: &Envelope,
        raw: &[u8],
    ) -> Result<Capabilities, SmtpError> {
        let answer = self.next();
        if answer.is_ok() {
            self.sent
                .push((server.clone(), envelope.clone(), raw.to_vec()));
        }
        answer
    }
}

/// A [`RecordingSmtp`] shared between the test and the code under test, which takes its own handle.
#[cfg(any(test, feature = "testing"))]
#[derive(Debug, Default, Clone)]
pub struct SharedSmtp(pub std::sync::Arc<std::sync::Mutex<RecordingSmtp>>);

#[cfg(any(test, feature = "testing"))]
impl SharedSmtp {
    /// Runs `f` on the fake; a poisoned lock is taken over, since a panicking test has already failed.
    pub fn with<T>(&self, f: impl FnOnce(&mut RecordingSmtp) -> T) -> T {
        let mut guard = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        f(&mut guard)
    }
}

#[cfg(any(test, feature = "testing"))]
impl SmtpOps for SharedSmtp {
    fn check_login(
        &mut self,
        server: &SmtpServer,
        secret: &Secret,
        ca_file: Option<&Path>,
    ) -> Result<Capabilities, SmtpError> {
        self.with(|smtp| smtp.check_login(server, secret, ca_file))
    }

    fn send(
        &mut self,
        server: &SmtpServer,
        secret: &Secret,
        ca_file: Option<&Path>,
        envelope: &Envelope,
        raw: &[u8],
    ) -> Result<Capabilities, SmtpError> {
        self.with(|smtp| smtp.send(server, secret, ca_file, envelope, raw))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(port: u16, tls: SmtpTls) -> SmtpServer {
        SmtpServer {
            host: "127.0.0.1".into(),
            port,
            tls,
            username: "me".into(),
        }
    }

    #[test]
    fn reply_codes_map_to_temporary_and_permanent() {
        for code in [421, 450, 451, 452] {
            assert!(
                matches!(classify(code, "x"), SmtpError::Temporary(_)),
                "{code}"
            );
        }
        assert!(matches!(classify(535, "x"), SmtpError::Auth(_)));
        for code in [550, 553, 554] {
            assert!(
                matches!(classify(code, "x"), SmtpError::Permanent(_)),
                "{code}"
            );
        }
        assert!(classify(451, "x").is_temporary());
        assert!(SmtpError::Connect("x".into()).is_temporary());
        assert!(!classify(550, "x").is_temporary());
        assert!(!classify(535, "x").is_temporary());
    }

    #[test]
    fn capabilities_read_size_and_8bitmime() {
        let lines = [
            "smtp.example.com greets you",
            "SIZE 35882577",
            "8BITMIME",
            "AUTH PLAIN",
        ];
        assert_eq!(
            capabilities(lines.into_iter()),
            Capabilities {
                size: Some(35_882_577),
                eight_bit_mime: true
            }
        );
        assert_eq!(
            capabilities(["x", "size 0"].into_iter()),
            Capabilities::default()
        );
    }

    /// A server that greets in plain text and then reads whatever comes.
    fn plain_server(lines: &'static [&'static str]) -> u16 {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let Ok((stream, _)) = listener.accept() else {
                return;
            };
            let mut writer = stream.try_clone().unwrap();
            let _ = writer.write_all(b"220 plain\r\n");
            let mut reader = BufReader::new(stream);
            let mut answers = lines.iter();
            let mut line = String::new();
            while reader.read_line(&mut line).is_ok_and(|n| n > 0) {
                line.clear();
                match answers.next() {
                    Some(answer) => {
                        let _ = writer.write_all(answer.as_bytes());
                    }
                    None => return,
                }
            }
        });
        port
    }

    #[test]
    fn implicit_tls_is_refused_by_a_plain_server() {
        let port = plain_server(&[]);
        let result = Lettre.check_login(
            &server(port, SmtpTls::Implicit),
            &Secret::new("x".into()),
            None,
        );
        assert!(matches!(result, Err(SmtpError::Connect(_))), "{result:?}");
    }

    #[test]
    fn starttls_is_required_before_the_login() {
        let port = plain_server(&["250-plain\r\n250 AUTH PLAIN\r\n"]);
        let result = Lettre.check_login(
            &server(port, SmtpTls::Starttls),
            &Secret::new("x".into()),
            None,
        );
        match result {
            Err(SmtpError::Connect(reason)) => assert!(reason.contains("STARTTLS"), "{reason}"),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn the_recording_fake_returns_its_script_in_order() {
        let mut fake = RecordingSmtp::default();
        fake.script.push_back(Err(SmtpError::Auth("no".into())));
        fake.script.push_back(Ok(Capabilities {
            size: Some(10),
            eight_bit_mime: false,
        }));
        let s = server(465, SmtpTls::Implicit);
        let secret = Secret::new("x".into());
        assert!(fake.check_login(&s, &secret, None).is_err());
        let envelope = Envelope {
            from: "a@x.test".into(),
            to: vec!["b@x.test".into()],
        };
        assert_eq!(
            fake.send(&s, &secret, None, &envelope, b"m").unwrap().size,
            Some(10)
        );
        assert_eq!(
            fake.check_login(&s, &secret, None),
            Ok(Capabilities::default())
        );
        assert_eq!(fake.logins.len(), 2);
        assert_eq!(fake.sent.len(), 1);
        assert_eq!(fake.sent[0].2, b"m");
    }
}
