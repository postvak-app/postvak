//! The messages clients and the daemon exchange, one JSON object per line; these types are the protocol.
use std::fmt;

use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

use crate::config::AccountConfig;
use crate::credentials::Secret;
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
