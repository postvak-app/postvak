//! Sending mail: an account's SMTP server and the guess for it, building and checking a message, replies and forwards,
//! the outbox the daemon keeps and the sender thread per account that empties it. See
//! `docs/superpowers/specs/2026-11-02-sending-design.md`.

use serde::{Deserialize, Serialize};

use crate::config::{AccountConfig, SmtpTls};
use crate::store::SmtpState;

pub mod build;
pub mod check;
pub mod guess;
pub mod outbox;
pub mod quote_html;
pub mod reply;
pub mod sendmail;
pub mod smtp;

pub use smtp::{SmtpError, SmtpOps};

/// Where and as whom an account sends: the server and the login.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SmtpServer {
    pub host: String,
    pub port: u16,
    pub tls: SmtpTls,
    pub username: String,
}

/// What the server said about itself in its EHLO reply.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    /// The largest message it takes, in bytes; None when it gave no `SIZE` (or `SIZE 0`).
    pub size: Option<u64>,
    pub eight_bit_mime: bool,
}

/// Who a message is sent from and to, as SMTP says it (`MAIL FROM`, `RCPT TO`); not the headers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    pub from: String,
    pub to: Vec<String>,
}

/// The server `account` sends through: its `[smtp]` table, else the stored guess when it was made for the account's
/// current IMAP host and login and found one; None when sending is not set up.
pub fn server_for(account: &AccountConfig, state: Option<&SmtpState>) -> Option<SmtpServer> {
    if let Some(smtp) = &account.smtp {
        return Some(SmtpServer {
            host: smtp.host.clone(),
            port: smtp.port(),
            tls: smtp.tls,
            username: smtp
                .username
                .clone()
                .unwrap_or_else(|| account.username.clone()),
        });
    }
    let state = state.filter(|state| guess_is_current(account, state))?;
    Some(SmtpServer {
        host: state.host.clone()?,
        port: state.port?,
        tls: state.tls?,
        username: account.username.clone(),
    })
}

/// Whether the stored guess was made for the account's current IMAP host and login.
fn guess_is_current(account: &AccountConfig, state: &SmtpState) -> bool {
    state.guessed_for_host == account.host && state.guessed_for_user == account.username
}
