//! `postvak send --raw` as a sendmail replacement: the few sendmail arguments mutt, aerc and git send-email pass, and
//! the account a whole message is sent from.

use super::check::Recipients;
use crate::config::AccountConfig;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SendmailError {
    #[error("sendmail option {0} is not supported; postvak takes -i, -oi, -t and -f <address>")]
    Unsupported(String),
    #[error("-f needs an address")]
    MissingFrom,
    #[error("the message has no From address")]
    NoFrom,
    #[error("no account owns From {0}; pass --account")]
    NoAccount(String),
    #[error("several accounts own From {0}; pass --account")]
    SeveralAccounts(String),
    #[error("no account named '{0}'")]
    NoSuchAccount(String),
}

/// Reads sendmail's arguments: `-i` and `-oi` (a lone dot does not end the message; Postvak never reads one), `-t`
/// (the headers name recipients too), `-f <address>` or `-f<address>` (the envelope sender), `--`, and the recipient
/// addresses. Any other option is refused by name. With no addresses, the headers name the recipients.
pub fn parse_args(args: &[String]) -> Result<Recipients, SendmailError> {
    let mut recipients = Recipients::default();
    let mut t = false;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--" => {
                recipients.extra.extend(rest.by_ref().cloned());
                break;
            }
            "-i" | "-oi" => {}
            "-t" => t = true,
            "-f" => {
                let from = rest.next().ok_or(SendmailError::MissingFrom)?;
                recipients.envelope_from = Some(from.clone());
            }
            flag if flag.starts_with("-f") => {
                recipients.envelope_from = Some(flag["-f".len()..].to_string());
            }
            flag if flag.starts_with('-') => {
                return Err(SendmailError::Unsupported(flag.to_string()));
            }
            address => recipients.extra.push(address.to_string()),
        }
    }
    recipients.from_headers = t || recipients.extra.is_empty();
    Ok(recipients)
}

/// The account a whole message goes out from: `chosen` when given, else the one account whose address or an alias is
/// the message's From.
pub fn account_for<'a>(
    raw: &[u8],
    accounts: &'a [AccountConfig],
    chosen: Option<&str>,
) -> Result<&'a AccountConfig, SendmailError> {
    if let Some(name) = chosen {
        return accounts
            .iter()
            .find(|a| a.name == name)
            .ok_or_else(|| SendmailError::NoSuchAccount(name.to_string()));
    }
    let from = crate::message::parse_headers(raw)
        .from
        .and_then(|from| crate::message::bare_addresses(&from).into_iter().next())
        .ok_or(SendmailError::NoFrom)?;
    let mut owners = accounts.iter().filter(|a| {
        a.identity()
            .is_ok_and(|id| !from.contains('*') && id.is_me(&from))
    });
    match (owners.next(), owners.next()) {
        (Some(only), None) => Ok(only),
        (None, _) => Err(SendmailError::NoAccount(from)),
        (Some(_), Some(_)) => Err(SendmailError::SeveralAccounts(from)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PasswordSource;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn git_send_email_arguments() {
        let r = parse_args(&args(&[
            "-i",
            "-f",
            "me@example.com",
            "--",
            "a@x.org",
            "bcc@y.org",
        ]))
        .unwrap();
        assert_eq!(r.extra, ["a@x.org", "bcc@y.org"]);
        assert!(!r.from_headers);
        assert_eq!(r.envelope_from.as_deref(), Some("me@example.com"));
    }

    #[test]
    fn mutt_arguments() {
        let r = parse_args(&args(&["-oi", "a@x.org"])).unwrap();
        assert_eq!(r.extra, ["a@x.org"]);
        assert!(!r.from_headers);
        assert_eq!(r.envelope_from, None);
        let r = parse_args(&args(&["-fme@example.com", "-t", "a@x.org"])).unwrap();
        assert_eq!(r.envelope_from.as_deref(), Some("me@example.com"));
        assert!(r.from_headers);
        // After `--`, a dash is an address.
        assert_eq!(
            parse_args(&args(&["--", "-odd@x.org"])).unwrap().extra,
            ["-odd@x.org"]
        );
    }

    #[test]
    fn no_addresses_reads_the_headers() {
        assert!(parse_args(&[]).unwrap().from_headers);
        assert!(parse_args(&args(&["-i"])).unwrap().from_headers);
        assert_eq!(parse_args(&args(&["-f"])), Err(SendmailError::MissingFrom));
    }

    #[test]
    fn an_unknown_flag_is_refused_by_name() {
        let error = parse_args(&args(&["-bs"])).unwrap_err();
        assert_eq!(error, SendmailError::Unsupported("-bs".into()));
        assert!(error.to_string().contains("-bs"));
    }

    fn accounts() -> Vec<AccountConfig> {
        let account = |name: &str, address: &str, aliases: &[&str]| {
            let mut a = AccountConfig::new(
                name,
                "imap.x.test",
                address,
                PasswordSource::Keyring { keyring: true },
            );
            a.aliases = aliases.iter().map(|a| (*a).into()).collect();
            a
        };
        vec![
            account("work", "me@work.test", &["*@shop.test"]),
            account("home", "me@home.test", &["both@x.test"]),
            account("other", "other@x.test", &["both@x.test"]),
        ]
    }

    fn from(address: &str) -> Vec<u8> {
        format!("From: Me <{address}>\r\nTo: a@x.org\r\n\r\nhi").into_bytes()
    }

    #[test]
    fn account_for_matches_from_by_alias() {
        let accounts = accounts();
        assert_eq!(
            account_for(&from("orders@shop.test"), &accounts, None)
                .unwrap()
                .name,
            "work"
        );
        assert_eq!(
            account_for(&from("ME@home.test"), &accounts, None)
                .unwrap()
                .name,
            "home"
        );
        assert_eq!(
            account_for(&from("stranger@x.test"), &accounts, None),
            Err(SendmailError::NoAccount("stranger@x.test".into()))
        );
        assert_eq!(
            account_for(b"To: a@x.org\r\n\r\n", &accounts, None),
            Err(SendmailError::NoFrom)
        );
    }

    #[test]
    fn account_for_refuses_two_matches() {
        assert_eq!(
            account_for(&from("both@x.test"), &accounts(), None),
            Err(SendmailError::SeveralAccounts("both@x.test".into()))
        );
    }

    #[test]
    fn account_for_honours_the_account_flag() {
        let accounts = accounts();
        assert_eq!(
            account_for(&from("both@x.test"), &accounts, Some("home"))
                .unwrap()
                .name,
            "home"
        );
        assert_eq!(
            account_for(&from("both@x.test"), &accounts, Some("nope")),
            Err(SendmailError::NoSuchAccount("nope".into()))
        );
    }
}
