//! The body pane without a toolkit: header dates and names, initials, sizes, the links in plain text, the missing-body
//! line, and the source window's rows. The window draws the pane from them.
use std::ops::Range;

use crate::message::clean;
use crate::sync::Activity;

use super::list::display_name;
use super::state::Account;

/// The reader's Date: weekday, day, month, year, time with seconds, and the UTC offset. chrono has no portable zone
/// abbreviation ("CEST"), so the offset stands in for it.
pub const HEADER_DATE: &str = "%A %-d %B %Y, %H:%M:%S (UTC%:z)";
/// The date beside the sender; `HEADER_DATE` shows on hover.
pub const SHORT_DATE: &str = "%a %-d %b %Y, %H:%M";

#[derive(Debug, PartialEq)]
pub enum Segment<'a> {
    Link(&'a str),
    Text(&'a str),
}

/// The only schemes a body link may open; anything else stays text.
const SCHEMES: [&str; 3] = ["http://", "https://", "mailto:"];

/// Splits a line into text and links. A link starts at a word start and ends at whitespace, a quote or an angle
/// bracket, minus trailing punctuation.
pub fn segments(text: &str) -> Vec<Segment<'_>> {
    let mut parts = Vec::new();
    let (mut text_start, mut scan) = (0, 0);
    while let Some(offset) = next_link(&text[scan..]) {
        let start = scan + offset;
        let tail = &text[start..];
        let end = tail
            .find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\''))
            .unwrap_or(tail.len());
        let link = tail[..end].trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']']);
        scan = start + link.len();
        // Trimming can leave a bare scheme, which is not a link.
        if has_target(link) {
            if start > text_start {
                parts.push(Segment::Text(&text[text_start..start]));
            }
            parts.push(Segment::Link(link));
            text_start = scan;
        }
    }
    if text_start < text.len() {
        parts.push(Segment::Text(&text[text_start..]));
    }
    parts
}

pub fn has_target(link: &str) -> bool {
    SCHEMES.iter().any(|scheme| {
        link.strip_prefix(scheme)
            .is_some_and(|target| !target.is_empty())
    })
}

fn next_link(text: &str) -> Option<usize> {
    SCHEMES
        .iter()
        .filter_map(|scheme| {
            text.match_indices(scheme).map(|(at, _)| at).find(|&at| {
                let word_start = text[..at]
                    .chars()
                    .next_back()
                    .is_none_or(|c| !c.is_alphanumeric());
                let more = text[at + scheme.len()..]
                    .chars()
                    .next()
                    .is_some_and(|c| !c.is_whitespace());
                word_start && more
            })
        })
        .min()
}

pub fn size(bytes: usize) -> String {
    match bytes {
        b if b < 1024 => format!("{b} B"),
        b if b < 1024 * 1024 => format!("{} KB", b / 1024),
        b => format!("{:.1} MB", b as f64 / (1024.0 * 1024.0)),
    }
}

/// The sender's name, and the address beside it when there is a name; the address alone otherwise.
pub(crate) fn name_and_address(from: &str) -> (String, Option<String>) {
    let name = display_name(from);
    let address = from
        .split_once('<')
        .and_then(|(_, rest)| rest.split_once('>'))
        .map(|(address, _)| address.trim().to_string())
        .filter(|address| !address.is_empty() && *address != name);
    (name, address)
}

/// The first letters of the sender's first and last name, or of the address when there is no name.
pub(crate) fn initials(from: &str) -> String {
    let name = display_name(from);
    let words: Vec<&str> = name.split_whitespace().collect();
    let first_letter = |word: &&str| word.chars().find(|c| c.is_alphanumeric());
    let letters: String = match words.as_slice() {
        [] => String::new(),
        [only] => first_letter(only).into_iter().collect(),
        [first, .., last] => [first, last].into_iter().filter_map(first_letter).collect(),
    };
    if letters.is_empty() {
        "?".into()
    } else {
        letters.to_uppercase()
    }
}

pub(crate) fn missing_text(account: &Account) -> String {
    if matches!(account.activity, Some(Activity::Offline { .. })) {
        format!("Not downloaded; loads when {} reconnects.", account.name)
    } else {
        "Loading…".into()
    }
}

/// Characters per row of the source window; a longer line is split so every row lays out quickly.
pub(crate) const SOURCE_ROW: usize = 1000;

/// A raw message as the source window shows it: the cleaned text once, and the byte range of each row in it.
pub(crate) struct Source {
    pub rows: Vec<Range<usize>>,
    pub text: String,
}

impl Source {
    /// ponytail: converts the whole message on the UI thread when the window opens; a message of tens of MB stalls one
    /// frame. Build it on a thread if that is ever felt.
    pub fn new(raw: &[u8]) -> Source {
        let text = clean(&String::from_utf8_lossy(raw), true);
        let rows = source_rows(&text);
        Source { rows, text }
    }
}

/// One range per line of `text`, and one per `SOURCE_ROW` characters of a longer line.
pub(crate) fn source_rows(text: &str) -> Vec<Range<usize>> {
    let mut rows = Vec::new();
    let mut line_start = 0;
    for line in text.split('\n') {
        let mut row_start = line_start;
        for (count, (at, _)) in line.char_indices().enumerate() {
            if count > 0 && count % SOURCE_ROW == 0 {
                rows.push(row_start..line_start + at);
                row_start = line_start + at;
            }
        }
        rows.push(row_start..line_start + line.len());
        line_start += line.len() + 1;
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    const WITH_ATTACHMENT: &str = "From: a@example.com\r\nSubject: report\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"b\"\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\nSee attached\r\n--b\r\nContent-Type: text/plain\r\nContent-Disposition: attachment; filename=\"report.txt\"\r\n\r\nquarterly numbers\r\n--b--\r\n";

    #[test]
    fn the_header_date_is_full_with_seconds_and_offset() {
        use chrono::TimeZone;
        let at = chrono::Utc.timestamp_opt(1_790_000_000, 0).unwrap();
        assert_eq!(
            at.format(HEADER_DATE).to_string(),
            "Monday 21 September 2026, 14:13:20 (UTC+00:00)"
        );
    }

    #[test]
    fn the_short_date_beside_the_sender_keeps_day_and_minute() {
        use chrono::TimeZone;
        let at = chrono::Utc.timestamp_opt(1_790_000_000, 0).unwrap();
        assert_eq!(at.format(SHORT_DATE).to_string(), "Mon 21 Sep 2026, 14:13");
    }

    #[test]
    fn segments_link_only_http_https_and_mailto_at_word_starts() {
        assert_eq!(
            segments("see https://example.com/a?b=1. or mailto:me@example.com, thanks"),
            [
                Segment::Text("see "),
                Segment::Link("https://example.com/a?b=1"),
                Segment::Text(". or "),
                Segment::Link("mailto:me@example.com"),
                Segment::Text(", thanks"),
            ]
        );
        assert_eq!(
            segments("(http://x.test)"),
            [
                Segment::Text("("),
                Segment::Link("http://x.test"),
                Segment::Text(")")
            ]
        );
        for plain in [
            "javascript:alert(1)",
            "file:///etc/passwd",
            "xhttps://evil.test",
            "https:// spaced",
            "ftp://x.test",
            "http://!",
            "mailto:)",
            "mailto:.",
        ] {
            assert_eq!(segments(plain), [Segment::Text(plain)], "{plain}");
        }
    }

    #[test]
    fn a_bare_scheme_after_trimming_is_not_a_link() {
        assert!(
            segments("see https://)")
                .iter()
                .all(|segment| matches!(segment, Segment::Text(_)))
        );
    }

    #[test]
    fn size_is_human_readable() {
        assert_eq!(size(512), "512 B");
        assert_eq!(size(2_048), "2 KB");
        assert_eq!(size(3_500_000), "3.3 MB");
    }

    #[test]
    fn initials_come_from_the_name_else_the_address() {
        assert_eq!(initials("Linus Example <linus@example.com>"), "LE");
        assert_eq!(initials("\"Lovelace, Ada\" <ada@example.com>"), "LA");
        assert_eq!(initials("Ada Augusta King <ada@example.com>"), "AK");
        assert_eq!(initials("<bare@example.com>"), "B");
        assert_eq!(initials("élodie@example.com"), "É");
        assert_eq!(initials(""), "?");
    }

    #[test]
    fn the_sender_splits_into_name_and_address() {
        assert_eq!(
            name_and_address("Linus Example <linus@example.com>"),
            ("Linus Example".into(), Some("linus@example.com".into()))
        );
        assert_eq!(
            name_and_address("\"Lovelace, Ada\" <ada@example.com>"),
            ("Lovelace, Ada".into(), Some("ada@example.com".into()))
        );
        assert_eq!(
            name_and_address("<bare@example.com>"),
            ("bare@example.com".into(), None)
        );
        assert_eq!(
            name_and_address("plain@example.com"),
            ("plain@example.com".into(), None)
        );
    }

    const HOSTILE: &[u8] =
        b"From: a@example.com\r\nSubject: \x1b]0;pwned\x07report\r\n\r\nline one\r\n";

    #[test]
    fn source_rows_split_lines_and_long_lines_on_char_boundaries() {
        assert_eq!(source_rows("ab\ncd"), [0..2, 3..5]);
        assert_eq!(source_rows(""), [Range { start: 0, end: 0 }]);
        let long = "é".repeat(2 * SOURCE_ROW + 500);
        let rows = source_rows(&long);
        let lengths: Vec<usize> = rows
            .iter()
            .map(|r| long[r.clone()].chars().count())
            .collect();
        assert_eq!(lengths, [SOURCE_ROW, SOURCE_ROW, 500]);
    }
}
