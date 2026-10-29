//! The body pane without a toolkit: header dates and names, initials, sizes, the links in plain text, the missing-body
//! line, the selection, and the source window's rows. The window draws the pane from them.
use std::ops::Range;
use std::sync::Arc;

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use postvak_core::message::{clean, display_name, header_value};
use postvak_core::store::Message;
use postvak_core::sync::Activity;
use postvak_core::time::local_time;

use super::avatar;
use super::list;
use super::state::{Account, App};

/// The body pane for the shown message, in words.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Pane {
    /// Instead of a message: "Pick a message to read it" or that it left the store.
    pub empty: Option<&'static str>,
    /// A line under `empty`: what to press to get going.
    pub hint: Option<&'static str>,
    pub subject: Option<String>,
    pub initials: String,
    /// Which avatar fill the initials sit on.
    pub tint: usize,
    /// The sender's logo record and its SVG, which the window draws instead of the initials.
    pub logo: Option<(String, Arc<str>)>,
    pub name: String,
    pub address: Option<String>,
    pub short_date: String,
    pub full_date: String,
    pub to: Option<String>,
    pub cc: Option<String>,
    /// The raw message is here, so Save .eml and View source work.
    pub downloaded: bool,
    /// Why the body is not shown yet.
    pub missing: String,
    /// The message has an HTML part: the More menu offers it as text or as HTML.
    pub has_html: bool,
    pub shows_html: bool,
    /// Why HTML shows as text, which disables Show as HTML.
    pub text_note: Option<&'static str>,
    /// The HTML is adapted to the dark theme, or would be: the More menu offers Original colours.
    pub adapts: bool,
    /// The switch is on: this message's HTML is in its sender's colours on a white page.
    pub original_colours: bool,
    /// Each attachment's name and size, such as "report.txt" and "2 KB".
    pub attachments: Vec<(String, String)>,
    /// The attachments are listed before the whole message is here, so they cannot be saved yet.
    pub attachments_pending: bool,
    pub saved: Option<String>,
    /// The message has a List-Unsubscribe header: the More menu offers Unsubscribe.
    pub unsubscribe: bool,
    /// The sender accepted the one-click unsubscribe: a Delete button shows beside the note.
    pub unsubscribed: bool,
    /// There is an address to reply to: Reply shows.
    pub reply: bool,
    /// The links in the text that open, in order, once each.
    pub links: Vec<String>,
}

/// The most links the pane lists; a sender can write hundreds of thousands.
const MAX_LINKS: usize = 200;

/// The links in `text` that open, in order, once each, at most `MAX_LINKS`.
pub(crate) fn links(text: Option<&str>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    text.iter()
        .flat_map(|text| links_in(text))
        .filter(|url| seen.insert(*url))
        .map(str::to_string)
        .take(MAX_LINKS)
        .collect()
}

/// Under "Pick a message to read it", drawn with keycaps: the keys `keys.rs` binds to the next message, search and the
/// key list.
pub(crate) const NOTHING_SELECTED_HINT: &str =
    "Or press j for the next one, / to search, ? for every key.";

/// What the body pane shows for `app`.
pub(crate) fn pane(app: &App) -> Pane {
    let Some(body) = &app.body else {
        return Pane {
            empty: Some("Pick a message to read it"),
            hint: Some(NOTHING_SELECTED_HINT),
            ..Pane::default()
        };
    };
    let Some(message) = &body.message else {
        return Pane {
            empty: Some(match body.reading {
                Some(_) => "Loading…",
                None => "This message is no longer in the local store.",
            }),
            ..Pane::default()
        };
    };
    let at = message.date.unwrap_or(message.internaldate);
    let from = message.from_addr.as_deref().unwrap_or_default();
    let (name, address) = name_and_address(from);
    let (initials, tint) = avatar::of(from);
    let header = |value: &Option<String>| value.as_deref().map(|v| clean(v, false));
    let attachments = body
        .attachments
        .iter()
        .map(|attachment| {
            let name = attachment
                .name
                .as_deref()
                .map(|name| clean(name, false))
                .unwrap_or_else(|| format!("attachment {}", attachment.index));
            (name, size(attachment.size))
        })
        .collect();
    Pane {
        empty: None,
        hint: None,
        subject: header(&message.subject),
        initials,
        tint,
        logo: body.brand.as_ref().and_then(|brand| {
            let svg = app.logos.svg(brand)?;
            Some((brand.record_name(), svg.clone()))
        }),
        name: clean(&name, false),
        address: address.map(|a| clean(&a, false)),
        short_date: local_time(at, SHORT_DATE),
        full_date: local_time(at, HEADER_DATE),
        to: header(&message.to_addr),
        cc: header(&message.cc_addr),
        downloaded: message.body_text.is_some(),
        missing: missing_text(&app.accounts[body.account]),
        has_html: body.html.is_some(),
        shows_html: body.shows_html(),
        text_note: body.text_note,
        adapts: app.offers_original_colours(),
        original_colours: body.original_colours,
        attachments,
        attachments_pending: body.attachments_pending,
        saved: body.saved.as_deref().map(|s| clean(s, false)),
        unsubscribe: postvak_core::unsubscribe::method(&message.headers).is_some(),
        unsubscribed: body.unsubscribed,
        reply: reply_url(message).is_some(),
        links: body.links.clone(),
    }
}

/// A selected message as the selection lists it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Selected {
    pub who: String,
    pub subject: String,
    pub date: String,
    pub unread: bool,
}

/// With two or more messages selected, the body pane shows them instead of one: "3 messages selected", and each one.
/// ponytail: words every selected row on each frame; fine for thousands, page it if Select all on huge folders drags.
pub(crate) fn selection(app: &App) -> Option<(String, Vec<Selected>)> {
    let rows = app.selected_rows();
    if rows.len() < 2 {
        return None;
    }
    let now = chrono::Local::now();
    let recipient = app.shows_recipient();
    let selected = rows
        .iter()
        .map(|row| {
            let columns = list::columns(row, None, recipient, false, &now);
            Selected {
                who: columns.who,
                subject: columns.subject,
                date: columns.date,
                unread: row.unread,
            }
        })
        .collect();
    let title = format!(
        "{} selected",
        super::numbers::counted(rows.len() as u64, "message", "messages")
    );
    Some((title, selected))
}

/// The reader's Date: weekday, day, month, year, time with seconds, and the UTC offset. chrono has no portable zone
/// abbreviation ("CEST"), so the offset stands in for it.
pub const HEADER_DATE: &str = "%A %-d %B %Y, %H:%M:%S (UTC%:z)";
/// The date beside the sender; `HEADER_DATE` shows on hover.
pub const SHORT_DATE: &str = "%a %-d %b %Y, %H:%M";

/// The only schemes a body link may open; anything else stays text.
const SCHEMES: [&str; 3] = ["http://", "https://", "mailto:"];

/// The links in a line. A link starts at a word start and ends at whitespace, a quote or an angle bracket, minus
/// trailing punctuation.
fn links_in(text: &str) -> Vec<&str> {
    let mut links = Vec::new();
    let mut scan = 0;
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
            links.push(link);
        }
    }
    links
}

pub fn has_target(link: &str) -> bool {
    SCHEMES.iter().any(|scheme| {
        link.strip_prefix(scheme)
            .is_some_and(|target| !target.is_empty())
    })
}

/// Where the next link starts. One pass up to it, so `links_in`, which resumes after each link, stays linear in the text.
fn next_link(text: &str) -> Option<usize> {
    let mut previous = None;
    for (at, c) in text.char_indices() {
        let word_start = previous.is_none_or(|p: char| !p.is_alphanumeric());
        previous = Some(c);
        if !word_start {
            continue;
        }
        let starts_link = SCHEMES.iter().any(|scheme| {
            text[at..]
                .strip_prefix(scheme)
                .is_some_and(|rest| rest.chars().next().is_some_and(|c| !c.is_whitespace()))
        });
        if starts_link {
            return Some(at);
        }
    }
    None
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

/// What a `mailto:` URL may leave as it is; the rest is escaped, since the subject is a stranger's text.
const MAILTO: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~')
    .remove(b'@');

/// The `mailto:` URL that starts a reply in the user's mail app: to the Reply-To address, else the sender's, with
/// "Re:" before the subject and the message it answers; None without an address to write to.
pub(crate) fn reply_url(message: &Message) -> Option<String> {
    let reply_to = header_value(&message.headers, "Reply-To");
    let to = [reply_to.as_deref(), message.from_addr.as_deref()]
        .into_iter()
        .flatten()
        .find_map(address)?;
    let subject = message.subject.as_deref().unwrap_or_default();
    let answered = subject
        .get(..3)
        .is_some_and(|re| re.eq_ignore_ascii_case("re:"));
    let subject = if answered {
        subject.to_string()
    } else {
        format!("Re: {subject}")
    };
    let encode = |text: &str| utf8_percent_encode(text, MAILTO).to_string();
    let mut url = format!("mailto:{}?subject={}", encode(to), encode(&subject));
    if let Some(id) = &message.message_id {
        let id = id.trim().trim_start_matches('<').trim_end_matches('>');
        url.push_str(&format!("&In-Reply-To={}", encode(&format!("<{id}>"))));
    }
    Some(url)
}

/// The one address in a From or Reply-To value, in angle brackets or bare.
fn address(value: &str) -> Option<&str> {
    let address = match value.split_once('<') {
        Some((_, rest)) => rest.split_once('>')?.0,
        None => value,
    };
    let address = address.trim();
    (address.contains('@') && !address.contains([',', ' '])).then_some(address)
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
    match account.activity {
        Some(Activity::Offline { .. }) => {
            format!("Not downloaded; loads when {} reconnects.", account.name)
        }
        Some(Activity::LoginRejected { .. }) => {
            format!(
                "Not downloaded; the server rejected {}'s login.",
                account.name
            )
        }
        _ => "Loading…".into(),
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

    use slint::ComponentHandle;

    use crate::state::{UiAction, View};
    use crate::test_support::{Driver, Fixture, Wires, launched, message, synced, unread, work};
    use postvak_core::rules::Action;
    use postvak_core::sync::{Command, Event};

    #[test]
    fn reply_opens_a_reply_to_the_sender_in_the_mail_app() {
        let fx = Fixture::new(&["work"]);
        fx.add("work", message("INBOX", 1, "lunch"));
        let (open, _wires) = fx.window();
        open.click("Reply");
        assert_eq!(
            launched(),
            ["mailto:sender1@example.com?subject=Re%3A%20lunch&In-Reply-To=%3C1@example.com%3E"]
        );
    }

    #[test]
    fn reply_writes_to_the_reply_to_address_else_the_sender_with_the_subject_and_the_id_it_answers()
    {
        let mut original = message("INBOX", 1, "Tickets & seats?");
        assert_eq!(
            reply_url(&original).as_deref(),
            Some(
                "mailto:sender1@example.com?subject=Re%3A%20Tickets%20%26%20seats%3F&In-Reply-To=%3C1@example.com%3E"
            )
        );
        original.headers = b"Reply-To: Desk <desk@example.com>\r\n\r\n".to_vec();
        original.subject = Some("RE: hello".into());
        original.message_id = None;
        assert_eq!(
            reply_url(&original).as_deref(),
            Some("mailto:desk@example.com?subject=RE%3A%20hello")
        );
        original.headers = Vec::new();
        original.from_addr = Some("not an address".into());
        assert_eq!(reply_url(&original), None);
    }

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
    fn links_are_only_http_https_and_mailto_at_word_starts() {
        assert_eq!(
            links_in("see https://example.com/a?b=1. or mailto:me@example.com, thanks"),
            ["https://example.com/a?b=1", "mailto:me@example.com"]
        );
        assert_eq!(links_in("(http://x.test)"), ["http://x.test"]);
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
            assert!(links_in(plain).is_empty(), "{plain}");
        }
    }

    #[test]
    fn a_body_of_many_links_is_scanned_in_linear_time_and_lists_at_most_the_cap() {
        let repeated = "http://a\n".repeat(250_000);
        let distinct: String = (0..250_000).map(|i| format!("http://a{i}\n")).collect();
        let started = std::time::Instant::now();
        assert_eq!(links(Some(&repeated)), ["http://a"]);
        let found = links(Some(&distinct));
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        assert_eq!(found.len(), MAX_LINKS);
        assert_eq!(found[MAX_LINKS - 1], format!("http://a{}", MAX_LINKS - 1));
    }

    #[test]
    fn a_bare_scheme_after_trimming_is_not_a_link() {
        assert!(links_in("see https://)").is_empty());
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

    fn not_downloaded(mut m: Message) -> Message {
        m.body_text = None;
        m
    }

    fn read(uid: u32) -> (String, Command) {
        work("INBOX", &[uid], Action::MarkRead)
    }

    fn fetch(uid: u32) -> (String, Command) {
        let command = Command::FetchBody {
            folder: "INBOX".into(),
            uid,
            preview: true,
        };
        ("work".to_string(), command)
    }

    fn ready(uid: u32) -> Event {
        Event::BodyReady {
            account: "work".into(),
            folder: "INBOX".into(),
            uid,
            request: 0,
        }
    }

    /// Stores the fetched raw message for uid `uid` with `text` as its body.
    fn downloaded(fx: &Fixture, uid: u32, text: &str) {
        let raw = format!("Subject: later\r\n\r\n{text}\r\n");
        fx.store("work")
            .set_raw("INBOX", uid, raw.as_bytes(), text)
            .unwrap();
    }

    fn body_text(driver: &Driver) -> Option<String> {
        driver.app.body.as_ref().and_then(|b| b.text.clone())
    }

    fn shows_html(open: &crate::test_support::Open) -> bool {
        open.with(|live| live.app.body.as_ref().is_some_and(|b| b.shows_html()))
    }

    #[test]
    fn an_empty_folder_says_so_and_the_reader_names_the_keys_that_start() {
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = fx.window();
        assert_eq!(open.find("Pick a message to read it").len(), 1);
        assert_eq!(open.find(NOTHING_SELECTED_HINT).len(), 1);
    }

    #[test]
    fn the_message_text_is_larger_than_the_interface() {
        let fx = Fixture::new(&["work"]);
        fx.add("work", message("INBOX", 1, "hello there"));
        let (open, _wires) = fx.window();
        let text = open.get("Message text");
        let input = open.window().get_body_text();
        assert_eq!(input, "Body of 1");
        assert!(text.size().height >= 15.0, "{:?}", text.size());
    }

    #[test]
    fn the_subject_is_a_heading_above_the_sender() {
        let fx = Fixture::new(&["work"]);
        fx.add("work", message("INBOX", 1, "hello there"));
        let (open, _wires) = fx.window();
        let sender = open.span("Sender 1");
        // The list row says "hello there" too; the heading is the one beside the sender.
        let heading = open
            .find("hello there")
            .into_iter()
            .filter(|e| e.absolute_position().x > 700.0)
            .map(|e| e.absolute_position().y + e.size().height)
            .next()
            .unwrap();
        assert!(heading <= sender.0, "{heading} vs {sender:?}");
        assert!(open.find("Subject").is_empty());
    }

    #[test]
    fn the_cursor_message_shows_headers_and_text() {
        let fx = Fixture::new(&["work"]);
        fx.add("work", message("INBOX", 1, "hello there"));
        let (open, _wires) = fx.window();
        assert_eq!(open.window().get_body_text(), "Body of 1");
        assert_eq!(open.find("Sender 1").len(), 1);
        assert_eq!(open.find("sender1@example.com").len(), 1);
    }

    /// Regression: a long header widened the reader past the window, clipping the headers and the HTML view.
    #[test]
    fn long_headers_truncate_in_a_narrow_reader() {
        let fx = Fixture::new(&["work"]);
        let mut long = message("INBOX", 1, "hello there");
        long.from_addr = Some(format!(
            "{} <long@example.com>",
            "Very Long Name ".repeat(10)
        ));
        fx.add("work", long);
        let (open, _wires) = fx.window();
        open.window().set_list_width(crate::list::MIN_WIDTH);
        open.window()
            .window()
            .set_size(slint::LogicalSize::new(900.0, 800.0));
        open.tick();
        for label in open.labels_containing("Very Long Name") {
            for element in open.find(&label) {
                let right = element.absolute_position().x + element.size().width;
                assert!(right <= 900.0, "{label}: {right}");
            }
        }
        let text = open.get("Message text");
        assert!(text.absolute_position().x + text.size().width <= 900.0);
    }

    #[test]
    fn an_https_link_opens_in_the_browser() {
        let fx = Fixture::new(&["work"]);
        let mut m = message("INBOX", 1, "links");
        m.body_text = Some("Visit https://example.com/a. Not javascript:alert(1)".into());
        fx.add("work", m);
        let (open, _wires) = fx.window();
        assert!(open.find("javascript:alert(1)").is_empty());
        open.click("https://example.com/a");
        assert_eq!(launched(), ["https://example.com/a"]);
    }

    #[test]
    fn a_missing_body_is_fetched_once_and_shown_when_ready() {
        let fx = Fixture::new(&["work"]);
        fx.add("work", not_downloaded(message("INBOX", 1, "later")));
        let (open, wires) = fx.window();
        assert_eq!(wires.sent(), [fetch(1)]);
        assert_eq!(open.find("Loading…").len(), 1);
        downloaded(&fx, 1, "Fetched text");
        open.receive(&wires, [ready(1)]);
        assert_eq!(open.window().get_body_text(), "Fetched text");
        assert!(wires.sent().is_empty());
    }

    #[test]
    fn a_failed_body_fetch_is_retried_when_the_message_is_shown_again() {
        let fx = Fixture::new(&["work"]);
        fx.add("work", not_downloaded(message("INBOX", 2, "later")));
        fx.add("work", message("INBOX", 1, "other"));
        let (mut driver, wires) = fx.driver();
        assert_eq!(wires.sent().len(), 1);
        let error = Event::Error {
            account: "work".into(),
            message: "boom".into(),
        };
        driver.receive(&wires, [error]);
        driver.press("j k");
        assert_eq!(wires.sent(), [fetch(2)]);
    }

    #[test]
    fn a_fetched_body_keeps_the_read_timer_and_the_hand_marks() {
        let fx = Fixture::new(&["work"]);
        fx.add("work", not_downloaded(unread("INBOX", 1, "later")));
        let (mut driver, wires) = fx.driver();
        driver.press("u u");
        downloaded(&fx, 1, "Fetched");
        driver.receive(&wires, [ready(1)]);
        assert_eq!(body_text(&driver).as_deref(), Some("Fetched"));
        driver.at(100.0);
        assert_eq!(
            wires.sent(),
            [fetch(1), read(1), work("INBOX", &[1], Action::MarkUnread)]
        );
    }

    #[test]
    fn marking_another_row_does_not_stop_the_shown_message_being_read() {
        let fx = Fixture::new(&["work"]);
        fx.add("work", message("INBOX", 1, "old"));
        fx.add("work", unread("INBOX", 2, "new"));
        let (mut driver, wires) = fx.driver();
        driver.at(10.0);
        driver.press("j x k u");
        driver.at(12.0);
        assert_eq!(
            wires.sent(),
            [work("INBOX", &[1], Action::MarkUnread), read(2)]
        );
    }

    #[test]
    fn offline_says_when_the_body_will_load() {
        let fx = Fixture::new(&["work"]);
        fx.add("work", not_downloaded(message("INBOX", 1, "later")));
        let (open, wires) = fx.window();
        let offline = Activity::Offline {
            reason: "timeout".into(),
            retry_at: 1_790_000_300,
        };
        let event = Event::Activity {
            account: "work".into(),
            activity: offline,
        };
        open.receive(&wires, [event]);
        assert_eq!(
            open.find("Not downloaded; loads when work reconnects.")
                .len(),
            1
        );
    }

    #[test]
    fn a_rejected_login_says_why_the_body_does_not_load() {
        let fx = Fixture::new(&["work"]);
        fx.add("work", not_downloaded(message("INBOX", 1, "later")));
        let (open, wires) = fx.window();
        let rejected = Activity::LoginRejected {
            reason: "login failed: bad password".into(),
            retry_at: 1_790_003_600,
        };
        let event = Event::Activity {
            account: "work".into(),
            activity: rejected,
        };
        open.receive(&wires, [event]);
        assert_eq!(
            open.find("Not downloaded; the server rejected work's login.")
                .len(),
            1
        );
    }

    #[test]
    fn a_message_is_marked_read_after_one_second_on_screen() {
        let fx = Fixture::new(&["work"]);
        fx.add("work", unread("INBOX", 1, "new"));
        fx.add("work", message("INBOX", 2, "old"));
        let (mut driver, wires) = fx.driver();
        driver.at(10.0);
        driver.press("j");
        driver.at(10.9);
        assert!(wires.sent().is_empty());
        driver.at(11.1);
        assert_eq!(wires.sent(), [read(1)]);
        driver.at(13.0);
        assert!(wires.sent().is_empty());
    }

    #[test]
    fn the_read_timer_wakes_the_window_without_input() {
        let fx = Fixture::new(&["work"]);
        fx.add("work", unread("INBOX", 1, "new"));
        fx.add("work", message("INBOX", 2, "old"));
        let (open, wires) = fx.window();
        open.press("j");
        open.advance(0.9);
        assert!(wires.sent().is_empty());
        open.advance(0.2);
        assert_eq!(wires.sent(), [read(1)]);
    }

    #[test]
    fn two_selected_messages_show_as_the_selection_and_stay_unread() {
        let fx = Fixture::new(&["work"]);
        fx.add("work", unread("INBOX", 1, "first"));
        fx.add("work", unread("INBOX", 2, "second"));
        let (open, wires) = fx.window();
        open.press("x");
        assert!(
            open.window().get_selection_title().is_empty(),
            "one is not a selection"
        );
        open.press("j x");
        assert_eq!(open.find("2 messages selected").len(), 1);
        assert_eq!(open.find("Actions apply to all of them.").len(), 1);
        let shown: Vec<String> = slint::Model::iter(&open.window().get_selection())
            .map(|s| s.subject.into())
            .collect();
        assert_eq!(shown, ["second", "first"]);
        open.advance(2.0);
        assert!(
            wires.sent().is_empty(),
            "the message under the selection is not read"
        );
        open.click("Clear selection");
        assert!(open.with(|live| live.app.list.marked.is_empty()));
        assert!(open.find("2 messages selected").is_empty());
    }

    #[test]
    fn the_avatar_colour_follows_the_address() {
        let fx = Fixture::new(&["work"]);
        let mut from_linus = message("INBOX", 1, "hi");
        from_linus.from_addr = Some("Linus <linus@example.com>".into());
        fx.add("work", from_linus);
        let (driver, _wires) = fx.driver();
        let pane = pane(&driver.app);
        assert_eq!(pane.tint, crate::avatar::tint("linus@example.com"));
    }

    /// Ticks until the shown message's logo is in.
    fn wait_for_logo(driver: &mut Driver) -> Pane {
        driver.tick_until(|app| pane(app).logo.is_some());
        pane(&driver.app)
    }

    #[test]
    fn a_dmarc_passing_sender_shows_its_logo_instead_of_the_initials() {
        let fx = Fixture::new(&["work"]);
        fx.append_config("[ui]\nsender_images = true\n");
        fx.add("work", crate::test_support::branded("INBOX", 1, "merged"));
        crate::test_support::cache_github_logo(&fx.paths);
        let (mut driver, _wires) = fx.driver();
        let (key, svg) = wait_for_logo(&mut driver).logo.unwrap();
        assert_eq!(key, "default._bimi.github.com");
        assert!(svg.contains("<svg"));
    }

    #[test]
    fn without_a_dmarc_pass_or_with_sender_images_off_the_initials_stay() {
        let fx = Fixture::new(&["work"]);
        fx.append_config("[ui]\nsender_images = true\n");
        let mut spoofed = crate::test_support::branded("INBOX", 1, "merged");
        spoofed.headers = b"From: GitHub <noreply@github.com>\r\n\r\n".to_vec();
        fx.add("work", spoofed);
        crate::test_support::cache_github_logo(&fx.paths);
        let (driver, _wires) = fx.driver();
        assert_eq!(pane(&driver.app).logo, None);
        assert!(!driver.app.logos.started(), "nothing to look up");

        let fx = Fixture::new(&["work"]);
        fx.add("work", crate::test_support::branded("INBOX", 1, "merged"));
        crate::test_support::cache_github_logo(&fx.paths);
        let (driver, _wires) = fx.driver();
        assert_eq!(pane(&driver.app).logo, None);
        assert!(!driver.app.logos.started(), "off by default");
    }

    #[test]
    fn the_window_draws_the_logo_once() {
        let fx = Fixture::new(&["work"]);
        fx.append_config("[ui]\nsender_images = true\n");
        fx.add("work", crate::test_support::branded("INBOX", 1, "merged"));
        crate::test_support::cache_github_logo(&fx.paths);
        let (open, _wires) = fx.window();
        open.tick_until(|open| open.window().get_body_info().has_logo);
        let shown = open.window().get_body_info();
        assert!(shown.has_logo);
        assert_eq!(shown.logo_key, "default._bimi.github.com");
        open.tick();
        assert!(
            open.window().get_body_info().logo == shown.logo,
            "kept, not drawn again"
        );
    }

    #[test]
    fn marking_unread_by_hand_is_not_undone_by_the_delay() {
        let fx = Fixture::new(&["work"]);
        fx.add("work", message("INBOX", 1, "read"));
        let (mut driver, wires) = fx.driver();
        driver.press("u");
        driver.at(30.0);
        assert_eq!(wires.sent(), [work("INBOX", &[1], Action::MarkUnread)]);
    }

    const WITH_ATTACHMENT_RAW: &[u8] = WITH_ATTACHMENT.as_bytes();

    #[test]
    fn save_writes_the_attachment_to_downloads() {
        let fx = Fixture::new(&["work"]);
        fx.add("work", message("INBOX", 1, "report"));
        fx.store("work")
            .set_raw("INBOX", 1, WITH_ATTACHMENT_RAW, "See attached")
            .unwrap();
        let (open, _wires) = fx.window();
        let label = open.labels_containing("report.txt (").remove(0);
        assert!(label.starts_with("report.txt ("), "{label}");
        open.click(&format!("Save {label}"));
        let saved = fx.paths.cache_dir.join("downloads").join("report.txt");
        assert_eq!(
            std::fs::read_to_string(&saved).unwrap().trim_end(),
            "quarterly numbers"
        );
        assert_eq!(open.labels_containing("Saved to").len(), 1);
    }

    #[test]
    fn a_preview_shows_the_html_and_its_attachments_save_once_the_whole_message_is_here() {
        use postvak_core::actions::Preview;
        use postvak_core::message::{Attachment, HtmlBody};
        let fx = Fixture::new(&["work"]);
        fx.add("work", not_downloaded(message("INBOX", 1, "report")));
        let (open, wires) = fx.window();
        let preview = Preview {
            html: Some(HtmlBody {
                html: "<p>See attached</p>".into(),
                inline: Vec::new(),
            }),
            attachments: vec![Attachment {
                index: 1,
                name: Some("report.txt".into()),
                content_type: "text/plain".into(),
                size: 18,
            }],
            whole: true,
        };
        let shown = Event::BodyPreview {
            account: "work".into(),
            folder: "INBOX".into(),
            uid: 1,
            preview: Box::new(preview),
        };
        open.receive(&wires, [shown]);
        assert!(shows_html(&open));
        let label = open.labels_containing("report.txt (").remove(0);
        let save = open.get(&format!("Save {label}"));
        assert_eq!(save.accessible_enabled(), Some(false));
        assert_eq!(
            save.accessible_description().as_deref(),
            Some("Downloading the message…")
        );

        fx.store("work")
            .set_raw("INBOX", 1, WITH_ATTACHMENT_RAW, "See attached")
            .unwrap();
        open.receive(&wires, [ready(1)]);
        assert!(open.with(|live| live.app.previews.is_empty()));
        let label = open.labels_containing("report.txt (").remove(0);
        open.click(&format!("Save {label}"));
        let saved = fx.paths.cache_dir.join("downloads").join("report.txt");
        assert_eq!(
            std::fs::read_to_string(&saved).unwrap().trim_end(),
            "quarterly numbers"
        );
    }

    /// Message 1 shown from a preview of a message too large to download unasked, with a PDF attachment.
    fn on_demand(fx: &Fixture) -> (crate::test_support::Open, Wires) {
        use postvak_core::actions::Preview;
        use postvak_core::message::Attachment;
        fx.add("work", not_downloaded(message("INBOX", 1, "Re: lunch")));
        // The preview stored its text, as the indexer would.
        fx.store("work")
            .set_indexed("INBOX", 1, Some("see the scan"), &[], 0)
            .unwrap();
        let (open, wires) = fx.window();
        let preview = Preview {
            html: None,
            attachments: vec![Attachment {
                index: 1,
                name: Some("scan.pdf".into()),
                content_type: "application/pdf".into(),
                size: 20 * 1024 * 1024,
            }],
            whole: false,
        };
        let shown = Event::BodyPreview {
            account: "work".into(),
            folder: "INBOX".into(),
            uid: 1,
            preview: Box::new(preview),
        };
        open.receive(&wires, [shown, ready(1)]);
        wires.sent();
        (open, wires)
    }

    #[test]
    fn an_attachment_of_a_message_too_large_to_download_is_fetched_when_saved() {
        let fx = Fixture::new(&["work"]);
        let (open, wires) = on_demand(&fx);
        let label = open.labels_containing("scan.pdf (").remove(0);
        open.click(&format!("Save {label}"));
        open.click(&format!("Save {label}"));
        let fetch = Command::FetchAttachment {
            folder: "INBOX".into(),
            uid: 1,
            index: 1,
        };
        assert_eq!(wires.sent(), [("work".to_string(), fetch)]);
        assert_eq!(open.labels_containing("Downloading scan.pdf…").len(), 1);
        let fetched = Event::AttachmentFetched {
            account: "work".into(),
            folder: "INBOX".into(),
            uid: 1,
            index: 1,
            name: Some("../scan.pdf".into()),
            contents: b"PDF".to_vec(),
            request: 0,
        };
        open.receive(&wires, [fetched]);
        let saved = fx.paths.cache_dir.join("downloads").join("scan.pdf");
        assert_eq!(std::fs::read(&saved).unwrap(), b"PDF");
        assert_eq!(open.labels_containing("Saved to").len(), 1);
        assert!(open.with(|live| live.app.fetching.is_empty()));
    }

    #[test]
    fn save_eml_of_a_message_too_large_to_download_downloads_it_first() {
        let fx = Fixture::new(&["work"]);
        let (open, wires) = on_demand(&fx);
        open.choose_more("Save .eml");
        let whole = Command::FetchBody {
            folder: "INBOX".into(),
            uid: 1,
            preview: false,
        };
        assert_eq!(wires.sent(), [("work".to_string(), whole)]);
        assert_eq!(open.labels_containing("Downloading the message…").len(), 1);
        let saved = fx.paths.cache_dir.join("downloads").join("Re- lunch.eml");
        assert!(!saved.exists());
        fx.store("work")
            .set_raw("INBOX", 1, WITH_ATTACHMENT_RAW, "See attached")
            .unwrap();
        open.receive(&wires, [ready(1)]);
        assert_eq!(std::fs::read(&saved).unwrap(), WITH_ATTACHMENT_RAW);
        assert!(open.with(|live| live.app.previews.is_empty()));
    }

    #[test]
    fn previews_of_messages_still_downloading_are_capped() {
        use postvak_core::actions::Preview;
        let fx = Fixture::new(&["work"]);
        let (mut driver, wires) = fx.driver();
        for uid in 1..=10 {
            let shown = Event::BodyPreview {
                account: "work".into(),
                folder: "INBOX".into(),
                uid,
                preview: Box::new(Preview {
                    html: None,
                    attachments: Vec::new(),
                    whole: true,
                }),
            };
            driver.receive(&wires, [shown]);
        }
        let held: Vec<u32> = driver
            .app
            .previews
            .iter()
            .map(|((_, key), _)| key.1)
            .collect();
        assert_eq!(held, [7, 8, 9, 10], "the newest stay");
    }

    #[test]
    fn save_as_writes_where_the_dialog_chose_and_replaces_that_file() {
        let fx = Fixture::new(&["work"]);
        fx.add("work", message("INBOX", 1, "report"));
        fx.store("work")
            .set_raw("INBOX", 1, WITH_ATTACHMENT_RAW, "See attached")
            .unwrap();
        let (open, _wires) = fx.window();
        // The dialog offered the attachment's own name, and the user picked a file that exists.
        open.with(|live| {
            live.app.ask_save_path = |name, folder, _| {
                let (chosen, answer) = std::sync::mpsc::channel();
                let _ = chosen.send(Ok(Some(folder.join(format!("picked-{name}")))));
                answer
            }
        });
        let picked = fx
            .paths
            .cache_dir
            .join("downloads")
            .join("picked-report.txt");
        std::fs::write(&picked, "old").unwrap();
        open.window().invoke_save_attachment_as(0);
        assert_eq!(
            std::fs::read_to_string(&picked).unwrap().trim_end(),
            "quarterly numbers"
        );
        assert_eq!(open.labels_containing("Saved to").len(), 1);
    }

    #[test]
    fn a_cancelled_save_as_writes_nothing_and_says_nothing() {
        let fx = Fixture::new(&["work"]);
        fx.add("work", message("INBOX", 1, "report"));
        fx.store("work")
            .set_raw("INBOX", 1, WITH_ATTACHMENT_RAW, "See attached")
            .unwrap();
        let (open, _wires) = fx.window();
        open.with(|live| {
            live.app.ask_save_path = |_, _, _| {
                let (chosen, answer) = std::sync::mpsc::channel();
                let _ = chosen.send(Ok(None));
                answer
            }
        });
        open.window().invoke_save_attachment_as(0);
        let downloads = fx.paths.cache_dir.join("downloads");
        assert_eq!(std::fs::read_dir(downloads).unwrap().count(), 0);
        assert!(open.labels_containing("Saved to").is_empty());
        assert!(open.labels_containing("Could not save").is_empty());
    }

    #[test]
    fn a_sync_that_adds_newer_mail_keeps_the_cursor_on_its_message() {
        let fx = Fixture::new(&["work"]);
        fx.add("work", message("INBOX", 1, "old"));
        let (mut driver, wires) = fx.driver();
        fx.add("work", unread("INBOX", 2, "new"));
        wires.send([synced("work", 1)]);
        driver.at(10.0);
        driver.at(12.0);
        assert!(wires.sent().is_empty());
        let list = &driver.app.list;
        assert_eq!(list.rows.len(), 2);
        assert_eq!(list.rows[list.cursor].uid, 1);
    }

    #[test]
    fn the_message_shown_at_startup_is_not_marked_read() {
        let fx = Fixture::new(&["work"]);
        fx.add("work", unread("INBOX", 1, "new"));
        let (mut driver, wires) = fx.driver();
        driver.at(10.0);
        driver.at(12.0);
        assert!(wires.sent().is_empty());
    }

    #[test]
    fn the_message_shown_after_switching_folders_is_not_marked_read() {
        let fx = Fixture::new(&["work"]);
        fx.folder("work", "Archive", Some("Archive"));
        let mut archived = message("Archive", 7, "archived");
        archived.flags = String::new();
        fx.add("work", archived);
        let (mut driver, wires) = fx.driver();
        driver.apply(UiAction::SelectView(View::Folder {
            account: 0,
            folder: "Archive".into(),
        }));
        driver.at(10.0);
        driver.at(12.0);
        assert!(wires.sent().is_empty());
    }

    #[test]
    fn an_opened_message_is_marked_read_a_second_after_its_body_arrives() {
        let fx = Fixture::new(&["work"]);
        fx.add("work", message("INBOX", 2, "old"));
        fx.add("work", not_downloaded(unread("INBOX", 1, "later")));
        let (mut driver, wires) = fx.driver();
        driver.at(10.0);
        driver.press("j");
        driver.at(12.0);
        assert_eq!(wires.sent(), [fetch(1)]);
        downloaded(&fx, 1, "Fetched");
        wires.send([ready(1)]);
        driver.at(12.5);
        assert!(wires.sent().is_empty());
        driver.at(13.6);
        assert_eq!(wires.sent(), [read(1)]);
    }

    #[test]
    fn the_row_after_an_archive_is_marked_read_after_a_second() {
        let fx = Fixture::new(&["work"]);
        fx.add("work", unread("INBOX", 1, "next"));
        fx.add("work", message("INBOX", 2, "done"));
        let (mut driver, wires) = fx.driver();
        driver.at(10.0);
        driver.press("e");
        driver.at(12.0);
        assert_eq!(
            wires.sent(),
            [work("INBOX", &[2], Action::Archive), read(1)]
        );
    }

    #[test]
    fn a_search_hit_is_not_opened_while_the_query_is_typed() {
        let fx = Fixture::new(&["work"]);
        fx.add("work", not_downloaded(unread("INBOX", 1, "invoice")));
        fx.add("work", message("INBOX", 2, "lunch"));
        let (mut driver, wires) = fx.driver();
        // The hit sits beside the cursor, so its body was prefetched before the search.
        wires.sent();
        driver.at(10.0);
        driver.press("/ invoice");
        driver.at(12.0);
        assert_eq!(driver.app.list.rows.len(), 1);
        assert!(wires.sent().is_empty());
        assert!(driver.app.body.is_none());
    }

    #[test]
    fn clicking_the_message_already_shown_opens_it() {
        let fx = Fixture::new(&["work"]);
        fx.add("work", unread("INBOX", 1, "new"));
        let (mut driver, wires) = fx.driver();
        driver.at(2.0);
        assert!(wires.sent().is_empty());
        driver.at(10.0);
        driver.apply(UiAction::SelectRow(0));
        driver.at(10.5);
        driver.at(11.1);
        assert_eq!(wires.sent(), [read(1)]);
    }

    #[test]
    fn a_reply_in_the_cursor_thread_keeps_the_cursor_on_that_thread() {
        let fx = Fixture::new(&["work"]);
        for (uid, subject) in [(1, "c"), (2, "b"), (3, "a")] {
            fx.add("work", message("INBOX", uid, subject));
        }
        let (mut driver, wires) = fx.driver();
        driver.press("j");
        let thread_b = "<2@example.com>";
        let mut reply = message("INBOX", 4, "Re: b");
        reply.thread_id = thread_b.into();
        fx.add("work", reply);
        driver.receive(&wires, [synced("work", 1)]);
        let list = &driver.app.list;
        assert_eq!(list.rows[list.cursor].thread_id.as_deref(), Some(thread_b));
        driver.press("e");
        let archive = Command::Apply {
            folder: "INBOX".into(),
            uids: vec![2, 4],
            action: Action::Archive,
            by: "gui".into(),
        };
        assert_eq!(wires.sent(), [("work".to_string(), archive)]);
    }

    #[test]
    fn the_read_delay_starts_when_the_body_text_appears() {
        let fx = Fixture::new(&["work"]);
        fx.add("work", message("INBOX", 2, "old"));
        fx.add("work", not_downloaded(unread("INBOX", 1, "later")));
        let (mut driver, wires) = fx.driver();
        driver.at(10.0);
        driver.press("j");
        assert_eq!(wires.sent(), [fetch(1)]);
        downloaded(&fx, 1, "Fetched");
        wires.send([ready(1)]);
        driver.at(20.0);
        driver.at(20.5);
        assert!(wires.sent().is_empty());
        driver.at(21.1);
        assert_eq!(wires.sent(), [read(1)]);
    }

    #[test]
    fn save_eml_writes_the_raw_message_to_downloads() {
        let fx = Fixture::new(&["work"]);
        fx.add("work", message("INBOX", 1, "Re: lunch"));
        fx.store("work")
            .set_raw("INBOX", 1, WITH_ATTACHMENT_RAW, "See attached")
            .unwrap();
        let (open, _wires) = fx.window();
        open.choose_more("Save .eml");
        let saved = fx.paths.cache_dir.join("downloads").join("Re- lunch.eml");
        assert_eq!(std::fs::read(&saved).unwrap(), WITH_ATTACHMENT_RAW);
        assert_eq!(open.labels_containing("Saved to").len(), 1);
    }

    fn list_mail(fx: &Fixture, list_headers: &str) {
        let mut news = message("INBOX", 1, "news");
        news.headers = format!("Subject: news\r\n{list_headers}\r\n\r\n").into_bytes();
        fx.add("work", news);
    }

    #[test]
    fn unsubscribe_posts_one_click_and_says_whether_the_sender_accepted() {
        let fx = Fixture::new(&["work"]);
        let post = "List-Unsubscribe-Post: List-Unsubscribe=One-Click";
        list_mail(
            &fx,
            &format!("List-Unsubscribe: <https://list.example/u>\r\n{post}"),
        );
        let (open, _wires) = fx.window();
        open.choose_more("Unsubscribe");
        open.tick_until(|open| !open.labels_containing("Unsubscribed").is_empty());
        assert!(crate::test_support::launched().is_empty());
        let fx = Fixture::new(&["work"]);
        list_mail(
            &fx,
            &format!("List-Unsubscribe: <https://list.example/refuse>\r\n{post}"),
        );
        let (open, _wires) = fx.window();
        open.choose_more("Unsubscribe");
        open.tick_until(|open| !open.labels_containing("Could not unsubscribe").is_empty());
        assert!(open.find(DELETE_UNSUBSCRIBED).is_empty());
    }

    const DELETE_UNSUBSCRIBED: &str = "Delete the unsubscribed message";

    #[test]
    fn an_accepted_unsubscribe_offers_to_delete_the_message() {
        let fx = Fixture::new(&["work"]);
        list_mail(
            &fx,
            "List-Unsubscribe: <https://list.example/u>\r\nList-Unsubscribe-Post: List-Unsubscribe=One-Click",
        );
        let (open, wires) = fx.window();
        assert!(open.find(DELETE_UNSUBSCRIBED).is_empty());
        open.choose_more("Unsubscribe");
        open.tick_until(|open| !open.find(DELETE_UNSUBSCRIBED).is_empty());
        wires.sent();
        open.click(DELETE_UNSUBSCRIBED);
        let trash = Command::Apply {
            folder: "INBOX".into(),
            uids: vec![1],
            action: Action::Trash,
            by: "gui".into(),
        };
        assert_eq!(wires.sent(), [("work".to_string(), trash)]);
    }

    #[test]
    fn without_one_click_unsubscribe_opens_the_page_and_without_a_header_it_is_not_offered() {
        let fx = Fixture::new(&["work"]);
        list_mail(&fx, "List-Unsubscribe: <https://list.example/page>");
        let (open, _wires) = fx.window();
        open.choose_more("Unsubscribe");
        assert_eq!(
            crate::test_support::launched(),
            ["https://list.example/page"]
        );
        let fx = Fixture::new(&["work"]);
        list_mail(&fx, "X-Other: 1");
        let (open, _wires) = fx.window();
        open.click("More actions");
        assert!(open.find("Unsubscribe").is_empty());
    }

    #[test]
    fn reader_actions_wait_for_the_download_and_never_fetch_on_their_own() {
        let fx = Fixture::new(&["work"]);
        fx.add("work", not_downloaded(message("INBOX", 1, "later")));
        let (open, wires) = fx.window();
        assert_eq!(wires.sent(), [fetch(1)]);
        for name in ["Save .eml", "View source"] {
            open.choose_more(name);
        }
        assert!(wires.sent().is_empty());
        let downloads = fx.paths.cache_dir.join("downloads");
        assert_eq!(std::fs::read_dir(downloads).unwrap().count(), 0);
        assert!(open.with(|live| live.app.body.as_ref().is_some_and(|b| b.source.is_none())));
    }

    #[test]
    fn the_source_window_shows_the_raw_message_cleaned_for_copying() {
        let fx = Fixture::new(&["work"]);
        fx.add("work", message("INBOX", 1, "report"));
        fx.store("work")
            .set_raw("INBOX", 1, HOSTILE, "line one")
            .unwrap();
        let (open, _wires) = fx.window();
        open.choose_more("View source");
        assert_eq!(open.find("Subject: ]0;pwnedreport").len(), 1);
        assert_eq!(open.find("Copy").len(), 1);
        let copied = open.window().get_source_text();
        assert_eq!(
            copied,
            "From: a@example.com\nSubject: ]0;pwnedreport\n\nline one\n"
        );
        open.press("escape");
        assert!(open.find("Subject: ]0;pwnedreport").is_empty());
    }

    #[test]
    fn a_megabyte_line_opens_as_short_rows() {
        let fx = Fixture::new(&["work"]);
        fx.add("work", message("INBOX", 1, "minified"));
        let raw = format!("Subject: minified\r\n\r\n{}\r\n", "x".repeat(1 << 20));
        fx.store("work")
            .set_raw("INBOX", 1, raw.as_bytes(), "x")
            .unwrap();
        let (open, _wires) = fx.window();
        open.choose_more("View source");
        let (rows, short) = open.with(|live| {
            let source = live.app.body.as_ref().unwrap().source.as_ref().unwrap();
            let short = source
                .rows
                .iter()
                .all(|r| source.text[r.clone()].chars().count() <= SOURCE_ROW);
            (source.rows.len(), short)
        });
        assert!(rows > (1 << 20) / SOURCE_ROW);
        assert!(short);
        assert_eq!(open.find("Subject: minified").len(), 1);
    }

    #[test]
    fn moving_to_another_message_closes_the_source() {
        let fx = Fixture::new(&["work"]);
        fx.add("work", message("INBOX", 1, "older"));
        fx.add("work", message("INBOX", 2, "report"));
        fx.store("work")
            .set_raw("INBOX", 2, HOSTILE, "line one")
            .unwrap();
        let (open, _wires) = fx.window();
        open.choose_more("View source");
        assert_eq!(open.find("Subject: ]0;pwnedreport").len(), 1);
        open.press("j");
        assert!(open.with(|live| live.app.body.as_ref().is_some_and(|b| b.source.is_none())));
        assert!(open.find("Subject: ]0;pwnedreport").is_empty());
    }

    #[test]
    fn the_text_html_switch_toggles_like_v_and_hides_without_html() {
        use crate::test_support::add_html;
        let fx = Fixture::new(&["work"]);
        fx.add("work", message("INBOX", 1, "plain"));
        add_html(&fx, 2, "<p>rich words</p>", "plain words");
        let (open, _wires) = fx.window();
        assert!(shows_html(&open));
        open.choose_more("Show as text");
        assert!(!shows_html(&open));
        assert_eq!(open.window().get_body_text(), "plain words");
        open.choose_more("Show as HTML");
        assert!(shows_html(&open));
        open.press("j");
        open.click("More actions");
        assert!(open.find("Show as HTML").is_empty());
        assert!(open.find("Show as text").is_empty());
    }

    #[test]
    fn html_too_large_to_render_disables_the_html_side() {
        use crate::html::MAX_HTML;
        use crate::test_support::add_html;
        let fx = Fixture::new(&["work"]);
        add_html(
            &fx,
            1,
            &format!("<p>{}</p>", "x".repeat(MAX_HTML)),
            "plain words",
        );
        let (open, _wires) = fx.window();
        // The item is disabled, and the action declines too.
        open.choose_more("Show as HTML");
        assert!(!shows_html(&open));
    }

    #[test]
    fn the_source_window_shows_the_raw_message_in_either_view() {
        use crate::test_support::add_html;
        let fx = Fixture::new(&["work"]);
        add_html(&fx, 1, "<p>rich words</p>", "plain words");
        let (open, _wires) = fx.window();
        open.choose_more("View source");
        let part = "Content-Type: text/html; charset=utf-8";
        assert_eq!(open.find(part).len(), 1);
        open.press("v");
        assert!(!shows_html(&open));
        assert_eq!(open.find(part).len(), 1);
    }

    fn reading_hello(fx: &Fixture) -> (crate::test_support::Open, Wires) {
        fx.add("work", message("INBOX", 1, "hello there"));
        let (open, wires) = fx.window();
        open.tick();
        (open, wires)
    }

    fn draft_conditions(open: &crate::test_support::Open) -> Vec<crate::draft::Condition> {
        open.with(|live| live.app.rule_draft.clone().unwrap().conditions)
    }

    #[test]
    fn the_rule_button_on_the_sender_opens_an_unsaved_draft_for_that_address() {
        use crate::draft::{Condition, Field, Op};
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = reading_hello(&fx);
        open.click("Rule from sender");
        assert_eq!(open.with(|live| live.app.view.clone()), View::Rules);
        assert_eq!(
            draft_conditions(&open),
            [Condition {
                field: Field::From,
                header: String::new(),
                op: Op::Equals,
                value: "sender1@example.com".into(),
            }]
        );
        assert!(open.with(|live| live.app.rule_draft.clone().unwrap().original.is_none()));
        assert!(
            !fx.paths.rules_file().exists(),
            "nothing is written before Save"
        );
    }

    #[test]
    fn the_rule_button_asks_before_replacing_an_edited_draft() {
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = reading_hello(&fx);
        let inbox = open.with(|live| live.app.view.clone());
        open.click("Rule from sender");
        open.with(|live| {
            live.app
                .apply(UiAction::EditDraft(Box::new(crate::draft::Draft {
                    name: "half done".into(),
                    ..live.app.rule_draft.clone().unwrap()
                })))
        });
        open.with(|live| live.app.select_view(inbox.clone()));
        open.tick();
        open.click("Rule from sender");
        assert_eq!(open.find("This rule has unsaved changes.").len(), 1);
        assert_eq!(
            open.with(|live| live.app.view.clone()),
            inbox,
            "the prompt shows over the reader"
        );
        open.click("Discard");
        assert_eq!(open.with(|live| live.app.view.clone()), View::Rules);
        let name = open.with(|live| live.app.rule_draft.clone().unwrap().name);
        assert_eq!(name, "from sender1@example.com");
    }
}
