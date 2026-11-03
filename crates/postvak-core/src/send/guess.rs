//! Guessing an account's SMTP server from its IMAP host: a few likely servers in the account's own domain, each tried
//! by logging in over TLS, never in plain text.

use std::net::IpAddr;

use super::{Capabilities, SmtpOps, SmtpServer, guess_is_current};
use crate::config::{AccountConfig, SmtpTls};
use crate::credentials::Secret;
use crate::store::SmtpState;

/// The servers to try for an IMAP host, in order: `smtp.<domain>`, then the IMAP host itself, where `<domain>` is the
/// IMAP host without a leading `imap.` or `mail.`; each on port 465 with implicit TLS, then 587 with STARTTLS. Only
/// hosts in the IMAP host's registrable domain; an IP address, or a host without a registrable domain, only itself.
pub fn candidates(imap_host: &str, username: &str) -> Vec<SmtpServer> {
    let imap_host = imap_host.trim().trim_end_matches('.');
    let lower = imap_host.to_ascii_lowercase();
    let mut hosts = Vec::new();
    let ip = lower
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<IpAddr>()
        .is_ok();
    match registrable(&lower) {
        Some(own) if !ip => {
            let domain = ["imap.", "mail."]
                .iter()
                .find_map(|prefix| lower.strip_prefix(prefix))
                .unwrap_or(&lower);
            for host in [format!("smtp.{domain}"), lower.clone()] {
                if registrable(&host) == Some(own) && !hosts.contains(&host) {
                    hosts.push(host);
                }
            }
        }
        _ => hosts.push(lower.clone()),
    }
    hosts
        .into_iter()
        .flat_map(|host| {
            [SmtpTls::Implicit, SmtpTls::Starttls].map(|tls| SmtpServer {
                host: host.clone(),
                port: tls.default_port(),
                tls,
                username: username.to_string(),
            })
        })
        .collect()
}

fn registrable(host: &str) -> Option<&str> {
    psl::domain_str(host)
}

/// The first candidate for `account` that logs in, with what it offered; None when none does.
pub fn guess(
    account: &AccountConfig,
    secret: &Secret,
    smtp: &mut dyn SmtpOps,
) -> Option<(SmtpServer, Capabilities)> {
    candidates(&account.host, &account.username)
        .into_iter()
        .find_map(|server| {
            match smtp.check_login(&server, secret, account.ca_file.as_deref()) {
                Ok(caps) => Some((server, caps)),
                Err(e) => {
                    // The reason names the host and the server's words, never the password.
                    log::info!(
                        "{}: no SMTP at {}:{}: {e}",
                        account.name,
                        server.host,
                        server.port
                    );
                    None
                }
            }
        })
}

/// What to store after a guess for `account` made at `now`: the server found, or that none was.
pub fn state_after(
    account: &AccountConfig,
    found: Option<&(SmtpServer, Capabilities)>,
    now: i64,
) -> SmtpState {
    SmtpState {
        host: found.map(|(server, _)| server.host.clone()),
        port: found.map(|(server, _)| server.port),
        tls: found.map(|(server, _)| server.tls),
        guessed_for_host: account.host.clone(),
        guessed_for_user: account.username.clone(),
        tried_at: now,
        size: found.and_then(|(_, caps)| caps.size),
        eight_bit_mime: found.is_some_and(|(_, caps)| caps.eight_bit_mime),
        sent_copy_problem: None,
    }
}

/// Whether the daemon should guess for `account`: it has no `[smtp]` table, and no guess was tried for its current
/// IMAP host and login.
pub fn needs_guess(account: &AccountConfig, state: Option<&SmtpState>) -> bool {
    account.smtp.is_none() && !state.is_some_and(|state| guess_is_current(account, state))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{PasswordSource, SmtpConfig};
    use crate::send::server_for;
    use crate::send::smtp::{RecordingSmtp, SmtpError};

    fn hosts(servers: &[SmtpServer]) -> Vec<(&str, u16)> {
        servers.iter().map(|s| (s.host.as_str(), s.port)).collect()
    }

    #[test]
    fn candidates_strip_imap_and_mail() {
        let servers = candidates("imap.example.com", "me");
        assert_eq!(
            hosts(&servers),
            [
                ("smtp.example.com", 465),
                ("smtp.example.com", 587),
                ("imap.example.com", 465),
                ("imap.example.com", 587)
            ]
        );
        assert_eq!(servers[0].tls, SmtpTls::Implicit);
        assert_eq!(servers[1].tls, SmtpTls::Starttls);
        assert!(servers.iter().all(|s| s.username == "me"));
        assert_eq!(
            candidates("mail.example.co.uk", "me")[0].host,
            "smtp.example.co.uk"
        );
        assert_eq!(
            candidates("IMAP.Example.com", "me")[0].host,
            "smtp.example.com"
        );
    }

    #[test]
    fn candidates_stay_in_the_registrable_domain() {
        for host in [
            "imap.example.com",
            "mail.example.com",
            "imap.eu.example.com",
        ] {
            for server in candidates(host, "me") {
                assert!(
                    server.host == "example.com" || server.host.ends_with(".example.com"),
                    "{host} → {}",
                    server.host
                );
            }
        }
        assert_eq!(
            hosts(&candidates("example.com", "me")),
            [
                ("smtp.example.com", 465),
                ("smtp.example.com", 587),
                ("example.com", 465),
                ("example.com", 587)
            ]
        );
        // `imap.co.uk` is under a public suffix: `smtp.co.uk` would be someone else's.
        assert!(
            candidates("imap.co.uk", "me")
                .iter()
                .all(|s| s.host == "imap.co.uk")
        );
    }

    #[test]
    fn an_ip_host_is_only_tried_itself() {
        let servers = candidates("192.0.2.1", "me");
        assert_eq!(hosts(&servers), [("192.0.2.1", 465), ("192.0.2.1", 587)]);
        assert_eq!(
            hosts(&candidates("localhost", "me")),
            [("localhost", 465), ("localhost", 587)]
        );
    }

    fn account() -> AccountConfig {
        AccountConfig::new(
            "work",
            "imap.example.com",
            "me@example.com",
            PasswordSource::Keyring { keyring: true },
        )
    }

    #[test]
    fn guess_takes_the_first_that_logs_in() {
        let caps = Capabilities {
            size: Some(1000),
            eight_bit_mime: true,
        };
        let mut smtp = RecordingSmtp::default();
        smtp.script.extend([
            Err(SmtpError::Connect("refused".into())),
            Err(SmtpError::Auth("no".into())),
            Ok(caps),
        ]);
        let (server, found) = guess(&account(), &Secret::new("x".into()), &mut smtp).unwrap();
        assert_eq!(
            (server.host.as_str(), server.port),
            ("imap.example.com", 465)
        );
        assert_eq!(found, caps);
        assert_eq!(smtp.logins.len(), 3);
        assert!(smtp.sent.is_empty());

        let mut none = RecordingSmtp::default();
        none.script
            .extend((0..4).map(|_| Err(SmtpError::Connect("x".into()))));
        assert_eq!(guess(&account(), &Secret::new("x".into()), &mut none), None);
        assert_eq!(none.logins.len(), 4);
    }

    #[test]
    fn server_for_prefers_config_then_a_current_guess() {
        let mut account = account();
        assert_eq!(server_for(&account, None), None);
        let found = (
            candidates("imap.example.com", "me@example.com")[1].clone(),
            Capabilities::default(),
        );
        let state = state_after(&account, Some(&found), 1);
        assert_eq!(server_for(&account, Some(&state)), Some(found.0.clone()));
        let failed = state_after(&account, None, 1);
        assert_eq!(server_for(&account, Some(&failed)), None);

        account.smtp = Some(SmtpConfig {
            username: Some("other".into()),
            ..SmtpConfig::new("smtp.config.test", SmtpTls::Starttls)
        });
        assert_eq!(
            server_for(&account, Some(&state)),
            Some(SmtpServer {
                host: "smtp.config.test".into(),
                port: 587,
                tls: SmtpTls::Starttls,
                username: "other".into()
            })
        );
    }

    #[test]
    fn needs_guess_again_after_the_imap_host_changes() {
        let mut account = account();
        assert!(needs_guess(&account, None));
        let tried = state_after(&account, None, 1);
        assert!(!needs_guess(&account, Some(&tried)));
        account.host = "imap.example.org".into();
        assert!(needs_guess(&account, Some(&tried)));
        assert_eq!(
            server_for(
                &account,
                Some(&state_after(
                    &self::account(),
                    Some(&(
                        candidates("imap.example.com", "me")[0].clone(),
                        Capabilities::default()
                    )),
                    1
                ))
            ),
            None
        );
        account.username = "other".into();
        account.host = "imap.example.com".into();
        assert!(needs_guess(&account, Some(&tried)));
        account.smtp = Some(SmtpConfig::new("smtp.example.com", SmtpTls::Implicit));
        assert!(!needs_guess(&account, None));
    }
}
