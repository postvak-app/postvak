//! Live sending tests against the Mailpit SMTP servers and Dovecot in tests/dovecot/compose.yml, skipped unless
//! POSTVAK_TEST_IMAP_HOST is set.

// Test code: unwrap, expect and panic are how a test fails.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use postvak_core::config::{AccountConfig, PasswordSource, SmtpConfig, SmtpTls};
use postvak_core::credentials::Secret;
use postvak_core::mail_ops::MailOps;
use postvak_core::mail_ops::imap::ImapOps;
use postvak_core::send::smtp::{Lettre, SmtpError, SmtpOps};
use postvak_core::send::{Capabilities, Envelope, SmtpServer};

const PASSWORD: &str = "postvak-test";
const IMAP_PORT: u16 = 10993;
const SMTP_STARTTLS: u16 = 10587;
const SMTP_IMPLICIT: u16 = 10465;
/// The API of the STARTTLS server, which refuses recipients outside example.com and example.org.
const API: u16 = 18025;

fn host() -> Option<String> {
    let host = std::env::var("POSTVAK_TEST_IMAP_HOST")
        .ok()
        .filter(|h| !h.is_empty());
    if host.is_none() {
        eprintln!("POSTVAK_TEST_IMAP_HOST unset; live SMTP test skipped");
    }
    host
}

fn ca_file() -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/dovecot/certs/ca.pem"
    ))
}

fn unique(test: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{test}-{nanos}")
}

fn server(host: &str, port: u16, tls: SmtpTls) -> SmtpServer {
    SmtpServer {
        host: host.into(),
        port,
        tls,
        username: "anyone@example.com".into(),
    }
}

/// A GET to Mailpit's API, the body as text.
fn api(host: &str, path: &str) -> String {
    let mut stream = std::net::TcpStream::connect((host, API)).unwrap();
    write!(stream, "GET {path} HTTP/1.0\r\nHost: {host}\r\n\r\n").unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_string())
        .unwrap_or_default()
}

/// The ids of the messages Mailpit holds with `subject` in theirs.
fn arrived(host: &str, subject: &str) -> Vec<String> {
    let query: String = subject
        .bytes()
        .map(|b| match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'.' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect();
    let body = api(host, &format!("/api/v1/search?query=subject:%22{query}%22"));
    let value: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
    value["messages"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|m| m["ID"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn wait_for<T>(what: &str, mut found: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(value) = found() {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn message(from: &str, to: &str, subject: &str, extra: &str) -> Vec<u8> {
    format!(
        "From: {from}\r\nTo: {to}\r\n{extra}Subject: {subject}\r\nDate: Mon, 2 Nov 2026 10:00:00 +0000\r\n\
         Message-ID: <{subject}@example.com>\r\n\r\nhello\r\n"
    )
    .into_bytes()
}

#[test]
fn check_login_reads_size_and_8bitmime() {
    let Some(host) = host() else { return };
    for (port, tls) in [
        (SMTP_STARTTLS, SmtpTls::Starttls),
        (SMTP_IMPLICIT, SmtpTls::Implicit),
    ] {
        let caps = Lettre
            .check_login(
                &server(&host, port, tls),
                &Secret::new("x".into()),
                Some(&ca_file()),
            )
            .unwrap_or_else(|e| panic!("{port}: {e}"));
        // Mailpit says `SIZE 0` (no limit) and offers SMTPUTF8 but not 8BITMIME.
        assert_eq!(
            caps,
            Capabilities {
                size: None,
                eight_bit_mime: false
            },
            "{port}"
        );
    }
    // The wrong kind of TLS for the port is refused before any login.
    let wrong = Lettre.check_login(
        &server(&host, SMTP_STARTTLS, SmtpTls::Implicit),
        &Secret::new("x".into()),
        Some(&ca_file()),
    );
    assert!(matches!(wrong, Err(SmtpError::Connect(_))), "{wrong:?}");
}

#[test]
fn a_refused_recipient_sends_to_nobody() {
    let Some(host) = host() else { return };
    let subject = unique("refused");
    let envelope = Envelope {
        from: "me@example.com".into(),
        to: vec!["ok@example.com".into(), "reject@nowhere.test".into()],
    };
    let raw = message(
        "me@example.com",
        "ok@example.com, reject@nowhere.test",
        &subject,
        "",
    );
    let result = Lettre.send(
        &server(&host, SMTP_STARTTLS, SmtpTls::Starttls),
        &Secret::new("x".into()),
        Some(&ca_file()),
        &envelope,
        &raw,
    );
    assert!(matches!(result, Err(SmtpError::Permanent(_))), "{result:?}");
    std::thread::sleep(Duration::from_millis(500));
    assert!(arrived(&host, &subject).is_empty());
}

/// A home with one account on the Dovecot and Mailpit servers, its user fresh.
struct Home {
    dir: tempfile::TempDir,
    account: AccountConfig,
}

impl Home {
    fn new(host: &str, test: &str) -> Home {
        let dir = tempfile::tempdir().unwrap();
        let username = format!("{}@example.com", unique(test));
        let account = AccountConfig {
            port: IMAP_PORT,
            notify: false,
            ca_file: Some(ca_file()),
            smtp: Some(SmtpConfig {
                save_sent: Some(true),
                ..SmtpConfig::new(host, SmtpTls::Starttls)
            }),
            ..AccountConfig::new(
                "live",
                host,
                &username,
                PasswordSource::Command {
                    command: format!("printf {PASSWORD}"),
                },
            )
        };
        let paths = postvak_core::paths::Paths::under(dir.path());
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        std::fs::write(paths.config_file(), config_toml(&account)).unwrap();
        let home = Home { dir, account };
        // The mailbox exists once logged in; Sent is where the copy goes.
        home.imap().create_folder("Sent").unwrap();
        home
    }

    fn imap(&self) -> ImapOps {
        ImapOps::connect(&self.account, &Secret::new(PASSWORD.into()))
            .unwrap_or_else(|e| panic!("{e}"))
    }

    fn postvak(&self, args: &[&str], stdin: &str) -> std::process::Output {
        let mut child = Command::new(env!("CARGO_BIN_EXE_postvak"))
            .args(args)
            .env("POSTVAK_HOME", self.dir.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "postvak {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    /// The messages in `folder`: flags and raw bytes.
    fn folder(&self, folder: &str) -> Vec<(Vec<String>, Vec<u8>)> {
        let mut imap = self.imap();
        imap.select(folder).unwrap();
        let uids = imap.search_uids(1).unwrap();
        let (Some(&first), Some(&last)) = (uids.first(), uids.last()) else {
            return Vec::new();
        };
        imap.fetch_envelopes(first, last)
            .unwrap()
            .into_iter()
            .map(|e| (e.flags, imap.fetch_raw(e.uid).unwrap().unwrap_or_default()))
            .collect()
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = Command::new(env!("CARGO_BIN_EXE_postvak"))
            .args(["daemon", "stop"])
            .env("POSTVAK_HOME", self.dir.path())
            .output();
    }
}

/// config.toml for `account`, sending through the STARTTLS server without an undo delay.
fn config_toml(account: &AccountConfig) -> String {
    let mut text = String::new();
    {
        text.push_str(&format!(
            "[[accounts]]\nname = \"{}\"\nhost = \"{}\"\nport = {}\nusername = \"{}\"\npassword = {{ command = \"printf {PASSWORD}\" }}\nnotify = false\nca_file = \"{}\"\n\n[accounts.smtp]\nhost = \"{}\"\nport = {SMTP_STARTTLS}\ntls = \"starttls\"\nsave_sent = true\n\n",
            account.name,
            account.host,
            account.port,
            account.username,
            ca_file().display(),
            account.host,
        ));
    }
    text.push_str("[sending]\nundo_secs = 0\n");
    text
}

fn header_of(raw: &[u8], name: &str) -> Option<String> {
    postvak_core::message::header_value(raw, name)
}

#[test]
fn send_then_sent_copy_and_answered_flag() {
    let Some(host) = host() else { return };
    let home = Home::new(&host, "reply");
    let subject = unique("original");
    let original = message(
        "Linus <linus@example.org>",
        &home.account.username,
        &subject,
        "",
    );
    home.imap().append("INBOX", &original, &[]).unwrap();
    home.postvak(&["sync"], "");
    let out = home.postvak(&["send", "--now", "--reply", "INBOX/1"], "Sounds good.\n");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("Sending to Linus now"),
        "{out:?}"
    );

    let ids = wait_for("the reply in Mailpit", || {
        Some(arrived(&host, &format!("Re: {subject}"))).filter(|ids| !ids.is_empty())
    });
    assert_eq!(ids.len(), 1);
    let raw = api(&host, &format!("/api/v1/message/{}/raw", ids[0]));
    assert_eq!(
        header_of(raw.as_bytes(), "In-Reply-To").as_deref(),
        Some(format!("<{subject}@example.com>").as_str())
    );

    let sent = wait_for("the copy in Sent", || {
        let sent = home.folder("Sent");
        (!sent.is_empty()).then_some(sent)
    });
    assert_eq!(sent.len(), 1);
    assert!(sent[0].0.iter().any(|f| f == "\\Seen"), "{:?}", sent[0].0);
    wait_for("the \\Answered flag", || {
        home.folder("INBOX")[0]
            .0
            .iter()
            .any(|f| f == "\\Answered")
            .then_some(())
    });
}

#[test]
fn bcc_is_not_in_what_arrives_but_is_in_sent() {
    let Some(host) = host() else { return };
    let home = Home::new(&host, "bcc");
    home.postvak(&["sync"], "");
    let subject = unique("bcc");
    home.postvak(
        &[
            "send",
            "--now",
            "--to",
            "a@example.com",
            "--bcc",
            "hidden@example.org",
            "--subject",
            &subject,
        ],
        "Just between us.\n",
    );
    let ids = wait_for("the message in Mailpit", || {
        Some(arrived(&host, &subject)).filter(|ids| !ids.is_empty())
    });
    // Mailpit writes a Bcc header of its own for envelope recipients the headers do not name, so what it stores cannot
    // show what was transmitted (the unit tests of `check` and `sender` do); it shows who got the message.
    let summary = api(&host, &format!("/api/v1/message/{}", ids[0]));
    assert!(
        summary.contains("hidden@example.org"),
        "the Bcc recipient got it too: {summary}"
    );
    let sent = wait_for("the copy in Sent", || {
        let sent = home.folder("Sent");
        (!sent.is_empty()).then_some(sent)
    });
    assert_eq!(
        header_of(&sent[0].1, "Bcc").as_deref(),
        Some("hidden@example.org")
    );
}

#[test]
fn a_forward_rule_reaches_the_sink_with_the_loop_header() {
    let Some(host) = host() else { return };
    let home = Home::new(&host, "forward");
    let paths = postvak_core::paths::Paths::under(home.dir.path());
    let marker = unique("invoice");
    std::fs::write(
        paths.rules_file(),
        format!(
            "[[rules]]\nname = \"invoices\"\nmatch.subject = {{ contains = \"{marker}\" }}\nactions.forward = {{ to = \"books@example.org\", note = \"For the records\" }}\n"
        ),
    )
    .unwrap();
    let out = home.postvak(&["rules", "approve", "invoices"], "");
    assert!(
        String::from_utf8_lossy(&out.stdout)
            .contains("approved 'invoices' forwarding to books@example.org"),
        "{out:?}"
    );
    let original = message(
        "Shop <billing@example.org>",
        &home.account.username,
        &marker,
        "",
    );
    home.imap().append("INBOX", &original, &[]).unwrap();
    home.postvak(&["sync"], "");

    let ids = wait_for("the forward in Mailpit", || {
        Some(arrived(&host, &format!("Fwd: {marker}"))).filter(|ids| !ids.is_empty())
    });
    assert_eq!(ids.len(), 1);
    let raw = api(&host, &format!("/api/v1/message/{}/raw", ids[0]));
    assert_eq!(
        header_of(raw.as_bytes(), "X-Postvak-Forwarded").as_deref(),
        Some(home.account.username.as_str())
    );
    assert!(raw.contains("For the records"), "{raw}");

    // A second sync forwards nothing more: the message was forwarded once.
    home.postvak(&["sync"], "");
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(arrived(&host, &format!("Fwd: {marker}")).len(), 1);
}
