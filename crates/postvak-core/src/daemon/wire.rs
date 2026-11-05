//! The messages clients and the daemon exchange, one JSON object per line; these types are the protocol.
use std::fmt;

use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

use crate::config::AccountConfig;
use crate::credentials::Secret;
use crate::send::outbox::Entry;
use crate::send::sender::OutboxCounts;
use crate::send::{Capabilities, SmtpServer};
use crate::sync::{Activity, Command, Event};

pub const PROTOCOL: u32 = 1;
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientMessage {
    Hello {
        protocol: u32,
        version: String,
    },
    Subscribe {
        id: u64,
    },
    Status {
        id: u64,
    },
    Shutdown {
        id: u64,
    },
    Command {
        id: u64,
        account: String,
        command: Command,
    },
    /// Logs in as `account` with `password` and logs out again; nothing is saved, so a front end can test an account
    /// before it adds it.
    TestLogin {
        id: u64,
        account: Box<AccountConfig>,
        password: Password,
    },
    /// Puts a message in `account`'s outbox, on disk before the reply.
    Send {
        id: u64,
        account: String,
        #[serde(with = "crate::message::base64_bytes")]
        raw: Vec<u8>,
        entry: Box<Entry>,
    },
    /// Takes back outbox message `outbox_id`, returning it, while it is not yet being sent.
    CancelSend {
        id: u64,
        account: String,
        outbox_id: String,
    },
    /// Sends a waiting or failed outbox message now.
    RetrySend {
        id: u64,
        account: String,
        outbox_id: String,
    },
    /// Answered once outbox message `outbox_id` went, failed, or failed for now and waits to be tried again.
    AwaitSend {
        id: u64,
        account: String,
        outbox_id: String,
    },
    /// Every account's outbox.
    Outbox {
        id: u64,
    },
    /// Guesses the SMTP server of `account`, logging in with `password`, or its own password source when None. The
    /// guess is stored for an account the daemon runs with the same IMAP host and login; config.toml is never written.
    GuessSmtp {
        id: u64,
        account: Box<AccountConfig>,
        password: Option<Password>,
    },
    /// Logs in to `account`'s `[smtp]` server, sending nothing.
    TestSmtp {
        id: u64,
        account: Box<AccountConfig>,
        password: Option<Password>,
    },
}

/// A password on its way to the daemon: its text on the wire, never shown by Debug, wiped on drop.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Password(String);

impl Password {
    pub fn new(secret: &Secret) -> Password {
        Password(secret.expose().to_string())
    }

    pub fn secret(&self) -> Secret {
        Secret::new(self.0.clone())
    }
}

impl fmt::Debug for Password {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

impl Drop for Password {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonMessage {
    Hello {
        protocol: u32,
        version: String,
        pid: u32,
    },
    Reply {
        id: u64,
        outcome: Outcome,
    },
    Event(Event),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Ok(Payload),
    Error(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Payload {
    Done,
    Event(Event),
    Status(Status),
    /// The outbox id a message got and when it is due.
    Queued {
        id: String,
        due: i64,
    },
    /// A message taken back from the outbox.
    Cancelled {
        #[serde(with = "crate::message::base64_bytes")]
        raw: Vec<u8>,
    },
    Outbox(Vec<(String, Entry)>),
    SmtpGuessed(Option<(SmtpServer, Capabilities)>),
    SmtpChecked(Capabilities),
    SendOutcome(SendOutcome),
}

/// What became of a message `AwaitSend` waited for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SendOutcome {
    Sent,
    /// Failed for now; it stays in the outbox and is tried again.
    Temporary(String),
    Permanent(String),
}

/// Whether an account can send, as the status shows it.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SendingStatus {
    #[default]
    Ready,
    /// No `[smtp]` table and no working guess.
    NotSetUp,
    /// Sending works, but the last sent message got no copy in Sent, for this reason.
    SentCopyProblem(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Status {
    pub pid: u32,
    pub version: String,
    pub uptime_secs: u64,
    pub clients: usize,
    pub accounts: Vec<AccountStatus>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountStatus {
    pub name: String,
    pub activity: Option<Activity>,
    /// Whether the server offered UIDPLUS at the account's last login; None before its first.
    pub uidplus: Option<bool>,
    /// Whether the account's last login ran over TLS; false only under `tls = "none"`, None before its first.
    pub encrypted: Option<bool>,
    /// How many of the account's messages wait in the outbox and how many failed.
    #[serde(default)]
    pub outbox: OutboxCounts,
    #[serde(default)]
    pub sending: SendingStatus,
}

impl Status {
    /// Whether `account`'s server lacked UIDPLUS at its last login; false while that is not known yet.
    pub fn lacks_uidplus(&self, account: &str) -> bool {
        self.accounts
            .iter()
            .any(|a| a.name == account && a.uidplus == Some(false))
    }
}

/// One message per line.
pub fn write_line(out: &mut impl std::io::Write, message: &impl Serialize) -> std::io::Result<()> {
    out.write_all(line(message)?.as_bytes())
}

/// The message as one JSON line, newline included; serde_json escapes any newline inside it.
pub(crate) fn line(message: &impl Serialize) -> std::io::Result<String> {
    let mut text = serde_json::to_string(message)?;
    text.push('\n');
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PasswordSource;

    #[test]
    fn a_test_login_carries_its_password_but_debug_never_shows_it() {
        let account = AccountConfig::new(
            "work",
            "imap.example.com",
            "me@example.com",
            PasswordSource::Keyring { keyring: true },
        );
        let message = ClientMessage::TestLogin {
            id: 7,
            account: Box::new(account),
            password: Password::new(&Secret::new("hunter2".into())),
        };
        assert!(!format!("{message:?}").contains("hunter2"));
        let line = line(&message).unwrap();
        let ClientMessage::TestLogin { password, .. } = serde_json::from_str(&line).unwrap() else {
            panic!("a test login");
        };
        assert_eq!(password.secret().expose(), "hunter2");
    }
}
