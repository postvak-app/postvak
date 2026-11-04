//! `mailto:` links (RFC 6068). Only the recipients, subject and body are read, so a link cannot set other headers.

use percent_encoding::percent_decode_str;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Mailto {
    pub bcc: Vec<String>,
    pub body: String,
    pub cc: Vec<String>,
    pub subject: String,
    pub to: Vec<String>,
}

pub fn parse(url: &str) -> Option<Mailto> {
    let url = url.split('#').next().unwrap_or_default();
    let (scheme, rest) = url.split_once(':')?;
    if !scheme.eq_ignore_ascii_case("mailto") {
        return None;
    }
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
    let mut mailto = Mailto::default();
    push_addresses(&mut mailto.to, path);
    for field in query.split('&') {
        let (name, value) = field.split_once('=').unwrap_or((field, ""));
        match name.to_ascii_lowercase().as_str() {
            "bcc" => push_addresses(&mut mailto.bcc, value),
            "body" => mailto.body = decode(value).replace("\r\n", "\n").replace('\r', "\n"),
            "cc" => push_addresses(&mut mailto.cc, value),
            "subject" => mailto.subject = single_line(&decode(value)),
            "to" => push_addresses(&mut mailto.to, value),
            _ => {}
        }
    }
    Some(mailto)
}

fn decode(value: &str) -> String {
    percent_decode_str(value).decode_utf8_lossy().into_owned()
}

fn single_line(text: &str) -> String {
    text.chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect()
}

fn push_addresses(list: &mut Vec<String>, value: &str) {
    let decoded: String = decode(value)
        .chars()
        .filter(|character| !character.is_control())
        .collect();
    list.extend(
        decoded
            .split(',')
            .map(str::trim)
            .filter(|address| !address.is_empty())
            .map(String::from),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_address() {
        let mailto = parse("mailto:a@x.org").expect("mailto");
        assert_eq!(mailto.to, ["a@x.org"]);
        assert!(mailto.subject.is_empty() && mailto.body.is_empty());
    }

    #[test]
    fn several_recipients_and_fields_are_decoded() {
        let mailto = parse(
            "MAILTO:a@x.org,b@x.org?cc=c%40x.org&Subject=Hi%20there&body=Line%0D%0Aline&bcc=d@x.org&to=e@x.org",
        )
        .expect("mailto");
        assert_eq!(mailto.to, ["a@x.org", "b@x.org", "e@x.org"]);
        assert_eq!(mailto.cc, ["c@x.org"]);
        assert_eq!(mailto.bcc, ["d@x.org"]);
        assert_eq!(mailto.subject, "Hi there");
        assert_eq!(mailto.body, "Line\nline");
    }

    #[test]
    fn other_headers_are_ignored() {
        let mailto =
            parse("mailto:a@x.org?from=evil@x.org&in-reply-to=x&subject=S").expect("mailto");
        assert_eq!(
            mailto,
            Mailto {
                to: vec!["a@x.org".into()],
                subject: "S".into(),
                ..Mailto::default()
            }
        );
    }

    #[test]
    fn not_a_mailto_url_is_none() {
        assert_eq!(parse("https://x.org"), None);
        assert_eq!(parse("a@x.org"), None);
    }

    #[test]
    fn the_fragment_is_cut() {
        assert_eq!(
            parse("mailto:a@x.org?subject=Hi#frag")
                .expect("mailto")
                .subject,
            "Hi"
        );
        assert_eq!(parse("mailto:a@x.org#x").expect("mailto").to, ["a@x.org"]);
    }

    #[test]
    fn control_characters_cannot_add_headers() {
        let mailto = parse("mailto:a%0D%0A@x.org?subject=x%0D%0ABcc:evil@x.org").expect("mailto");
        assert_eq!(mailto.subject, "x  Bcc:evil@x.org");
        assert!(mailto.bcc.is_empty());
        assert_eq!(mailto.to, ["a@x.org"]);
    }

    #[test]
    fn a_lone_carriage_return_in_the_body_is_a_line_break() {
        assert_eq!(
            parse("mailto:a@x.org?body=a%0Db%0D%0Ac")
                .expect("mailto")
                .body,
            "a\nb\nc"
        );
    }

    #[test]
    fn malformed_escapes_stay_literal() {
        let mailto = parse("mailto:a@x.org?subject=100%zz&body=ends%").expect("mailto");
        assert_eq!(mailto.subject, "100%zz");
        assert_eq!(mailto.body, "ends%");
    }
}
