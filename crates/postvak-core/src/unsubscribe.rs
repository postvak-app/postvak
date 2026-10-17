//! Leaving a mailing list: RFC 8058's one-click POST, or else the List-Unsubscribe page or address the reader opens.
use std::time::Duration;

use serde::Serialize;

use crate::message::header_value;
use crate::net::public;

const TIMEOUT: Duration = Duration::from_secs(15);

/// How a message offers unsubscribing, best first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Method {
    /// An https URL that takes the RFC 8058 POST, so Postvak can unsubscribe by itself.
    OneClick(String),
    /// A web page the reader opens.
    Link(String),
    /// A `mailto:` address the reader sends a mail to.
    Mail(String),
}

impl Method {
    pub fn url(&self) -> &str {
        match self {
            Method::OneClick(url) | Method::Link(url) | Method::Mail(url) => url,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Method::OneClick(_) => "one_click",
            Method::Link(_) => "link",
            Method::Mail(_) => "mail",
        }
    }
}

/// What `unsubscribe` did: `done` once the sender accepted the one-click POST; otherwise `url` is for the reader.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Report {
    pub method: &'static str,
    pub url: String,
    pub done: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum UnsubscribeError {
    #[error("the message has no List-Unsubscribe header")]
    NotOffered,
    #[error("the unsubscribe address is not a public web host")]
    NotPublic,
    /// Says nothing about the URL: it identifies the reader.
    #[error("the sender's server did not accept the unsubscribe ({0})")]
    Refused(String),
}

/// How `headers` offers unsubscribing: one-click when an https URL comes with `List-Unsubscribe-Post`, else a web page,
/// else a mail; None without a usable List-Unsubscribe header.
pub fn method(headers: &[u8]) -> Option<Method> {
    let list = header_value(headers, "List-Unsubscribe")?;
    // Folding may put whitespace inside the angle brackets, which no URL contains.
    let uris: Vec<String> = list
        .split(',')
        .filter_map(|part| {
            let uri = part.trim().strip_prefix('<')?.strip_suffix('>')?;
            Some(uri.split_whitespace().collect())
        })
        .collect();
    let scheme = |uri: &String, wanted: &str| {
        uri.split_once(':')
            .is_some_and(|(scheme, _)| scheme.eq_ignore_ascii_case(wanted))
    };
    let post = header_value(headers, "List-Unsubscribe-Post").is_some_and(|value| {
        let value: String = value.split_whitespace().collect();
        value.eq_ignore_ascii_case("List-Unsubscribe=One-Click")
    });
    if post && let Some(url) = uris.iter().find(|uri| scheme(uri, "https")) {
        return Some(Method::OneClick(url.clone()));
    }
    if let Some(url) = uris
        .iter()
        .find(|uri| scheme(uri, "https") || scheme(uri, "http"))
    {
        return Some(Method::Link(url.clone()));
    }
    uris.iter()
        .find(|uri| scheme(uri, "mailto"))
        .map(|uri| Method::Mail(uri.clone()))
}

/// Unsubscribes by one click where `headers` allow it; otherwise reports the page or address for the reader to open.
/// A dry run only reports.
pub fn unsubscribe(headers: &[u8], dry_run: bool) -> Result<Report, UnsubscribeError> {
    let method = method(headers).ok_or(UnsubscribeError::NotOffered)?;
    let done = match &method {
        Method::OneClick(url) if !dry_run => {
            one_click(url)?;
            true
        }
        _ => false,
    };
    Ok(Report {
        method: method.label(),
        url: method.url().to_string(),
        done,
    })
}

/// The RFC 8058 POST to a public host.
pub fn one_click(url: &str) -> Result<(), UnsubscribeError> {
    post_to(url, public)
}

/// The RFC 8058 POST. The RFC forbids a redirect in answer, so only a 2xx counts; the host check is given so tests can
/// serve from this machine.
fn post_to(url: &str, allowed: impl Fn(&str) -> bool) -> Result<(), UnsubscribeError> {
    if !allowed(url) {
        return Err(UnsubscribeError::NotPublic);
    }
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .max_redirects(0)
        .http_status_as_error(false)
        .timeout_global(Some(TIMEOUT))
        .build()
        .into();
    let response = agent
        .post(url)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .send("List-Unsubscribe=One-Click")
        .map_err(|_| UnsubscribeError::Refused("no answer".into()))?;
    let status = response.status();
    if status.is_success() {
        Ok(())
    } else {
        Err(UnsubscribeError::Refused(format!(
            "HTTP {}",
            status.as_u16()
        )))
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    use super::*;

    fn headers(lines: &str) -> Vec<u8> {
        format!("Subject: news\r\n{lines}\r\n\r\n").into_bytes()
    }

    #[test]
    fn one_click_wins_then_a_page_then_a_mail() {
        let both = "List-Unsubscribe: <mailto:leave@list.example>, <https://list.example/u?id=1>";
        let post = "List-Unsubscribe-Post: List-Unsubscribe=One-Click";
        assert_eq!(
            method(&headers(&format!("{both}\r\n{post}"))),
            Some(Method::OneClick("https://list.example/u?id=1".into()))
        );
        assert_eq!(
            method(&headers(both)),
            Some(Method::Link("https://list.example/u?id=1".into()))
        );
        let http_post = "List-Unsubscribe: <http://list.example/u>\r\nList-Unsubscribe-Post: List-Unsubscribe=One-Click";
        assert_eq!(
            method(&headers(http_post)),
            Some(Method::Link("http://list.example/u".into())),
            "one-click needs https"
        );
        assert_eq!(
            method(&headers(
                "List-Unsubscribe: <MAILTO:leave@list.example?subject=bye>"
            )),
            Some(Method::Mail("MAILTO:leave@list.example?subject=bye".into()))
        );
        let folded = "List-Unsubscribe: <https://list.example/\r\n u?id=2>";
        assert_eq!(
            method(&headers(folded)),
            Some(Method::Link("https://list.example/u?id=2".into()))
        );
        assert_eq!(
            method(&headers("List-Unsubscribe: <javascript:alert(1)>")),
            None
        );
        assert_eq!(method(&headers("X-Other: 1")), None);
    }

    #[test]
    fn a_dry_run_or_a_link_only_reports() {
        let one_click = headers(
            "List-Unsubscribe: <https://list.example/u>\r\nList-Unsubscribe-Post: List-Unsubscribe=One-Click",
        );
        let report = unsubscribe(&one_click, true).unwrap();
        assert_eq!(
            report,
            Report {
                method: "one_click",
                url: "https://list.example/u".into(),
                done: false
            }
        );
        let link = unsubscribe(
            &headers("List-Unsubscribe: <https://list.example/u>"),
            false,
        )
        .unwrap();
        assert_eq!((link.method, link.done), ("link", false));
        assert!(matches!(
            unsubscribe(&headers("X-Other: 1"), false),
            Err(UnsubscribeError::NotOffered)
        ));
    }

    /// A server on a free local port that answers one request with `status` and hands the request back. The body can
    /// arrive in a later segment than the headers, so it reads until the body is in or the client stops.
    fn serve_once(status: &'static str) -> (String, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (send, request) = mpsc::channel();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut received = Vec::new();
            let mut buffer = [0; 2048];
            while !received.ends_with(b"One-Click") {
                match stream.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => received.extend_from_slice(&buffer[..read]),
                }
            }
            let _ = send.send(String::from_utf8_lossy(&received).into_owned());
            let reply =
                format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            let _ = stream.write_all(reply.as_bytes());
        });
        (format!("http://127.0.0.1:{port}/u?id=7"), request)
    }

    #[test]
    fn the_post_says_one_click_and_only_a_success_counts() {
        let anywhere = |_: &str| true;
        let (url, request) = serve_once("200 OK");
        post_to(&url, anywhere).unwrap();
        let request = request.recv().unwrap();
        assert!(request.starts_with("POST /u?id=7 "), "{request}");
        assert!(request.ends_with("List-Unsubscribe=One-Click"), "{request}");
        let (url, _) = serve_once("302 Found");
        assert!(
            matches!(post_to(&url, anywhere), Err(UnsubscribeError::Refused(e)) if e == "HTTP 302")
        );
        let (url, _) = serve_once("200 OK");
        assert!(matches!(
            post_to(&url, public),
            Err(UnsubscribeError::NotPublic)
        ));
    }
}
