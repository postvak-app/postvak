//! The message list without a toolkit: one row per thread, followed by its members when expanded; columns, dates and
//! the accessible row names. The window draws it (`ui/list.slint`, `rows.rs`).
use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Datelike, TimeZone};

use crate::message::clean;
use crate::rules::Action;
use crate::store::{Message, MessageSummary, ThreadSummary};

use super::folders;
use super::theme::ROW;

/// Threads loaded per folder. ponytail: older mail is reachable through search; page by date if that is not enough.
pub const THREAD_LIMIT: u32 = 10_000;
/// Results shown for one search.
pub const SEARCH_LIMIT: u32 = 500;

pub type RowKey = (String, u32);

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    /// Index into `App::accounts`: the account the message lives in.
    pub account: usize,
    pub count: u32,
    pub date: i64,
    pub flagged: bool,
    pub folder: String,
    pub from: String,
    pub member: bool,
    pub subject: String,
    /// Set on thread rows, so actions and Right/Left reach the whole thread.
    pub thread_id: Option<String>,
    pub to: String,
    pub uid: u32,
    pub unread: bool,
}

impl Row {
    pub fn key(&self) -> RowKey {
        (self.folder.clone(), self.uid)
    }

    /// The key across accounts: a uid is only unique within one account's folder.
    pub fn id(&self) -> (usize, RowKey) {
        (self.account, self.key())
    }

    pub fn from_message(account: usize, message: &Message) -> Row {
        Row {
            account,
            count: 1,
            date: message.internaldate,
            flagged: message.is_flagged(),
            folder: message.folder.clone(),
            from: message.from_addr.clone().unwrap_or_default(),
            member: false,
            subject: message.subject.clone().unwrap_or_default(),
            thread_id: None,
            to: message.to_addr.clone().unwrap_or_default(),
            uid: message.uid,
            unread: !message.is_seen(),
        }
    }

    fn from_summary(account: usize, folder: &str, message: &MessageSummary, member: bool) -> Row {
        Row {
            account,
            count: 1,
            date: message.date,
            flagged: message.is_flagged(),
            folder: folder.to_string(),
            from: message.from.clone(),
            member,
            subject: message.subject.clone(),
            thread_id: None,
            to: message.to.clone(),
            uid: message.uid,
            unread: !message.is_seen(),
        }
    }
}

#[derive(Default)]
pub(crate) struct ListState {
    pub cursor: usize,
    /// Expanded threads by account and thread id: mail sent to two accounts shares its thread id.
    pub expanded: HashMap<(usize, String), Vec<MessageSummary>>,
    /// Set when a key moved the cursor, so this frame scrolls it into view.
    pub follow_cursor: bool,
    pub hits: Vec<Row>,
    pub marked: HashSet<(usize, RowKey)>,
    pub rows: Vec<Row>,
    /// Each thread with the account it is in.
    pub threads: Vec<(usize, ThreadSummary)>,
    /// Scroll offset and height of the list from the last frame.
    pub viewport: (f32, f32),
}

/// One row per thread describing its latest message, followed by its members when expanded.
pub fn build_rows(
    folder: &str,
    threads: &[(usize, ThreadSummary)],
    expanded: &HashMap<(usize, String), Vec<MessageSummary>>,
) -> Vec<Row> {
    let mut rows = Vec::with_capacity(threads.len());
    for (account, thread) in threads {
        let mut row = Row::from_summary(*account, folder, &thread.latest, false);
        row.count = thread.count;
        row.flagged = thread.flagged;
        row.thread_id = Some(thread.thread_id.clone());
        row.unread = thread.unread;
        rows.push(row);
        if let Some(members) = expanded.get(&(*account, thread.thread_id.clone())) {
            rows.extend(
                members
                    .iter()
                    .map(|m| Row::from_summary(*account, folder, m, true)),
            );
        }
    }
    rows
}

/// What a sent action will do to a row, shown until its `ActionDone` arrives.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Optimistic {
    Flagged(bool),
    Hidden,
    Seen(bool),
}

impl Optimistic {
    pub fn of(action: &Action) -> Option<Optimistic> {
        match action {
            Action::Archive | Action::Delete | Action::Move(_) | Action::Trash => {
                Some(Optimistic::Hidden)
            }
            Action::Flag => Some(Optimistic::Flagged(true)),
            Action::MarkRead => Some(Optimistic::Seen(true)),
            Action::MarkUnread => Some(Optimistic::Seen(false)),
            Action::Unflag => Some(Optimistic::Flagged(false)),
            Action::Notify | Action::Silent | Action::Tag(_) => None,
        }
    }
}

/// Rows of sent actions as they will be: moved rows hidden, read and flag changes shown, a later edit over an earlier.
/// `pending[i]` is account i's edits, as `App::accounts` is ordered.
pub fn apply_pending(rows: &mut Vec<Row>, pending: &[&HashMap<RowKey, Vec<Optimistic>>]) {
    if pending.iter().all(|edits| edits.is_empty()) {
        return;
    }
    rows.retain(|row| !edits_of(pending, row).contains(&Optimistic::Hidden));
    for row in rows {
        for edit in edits_of(pending, row) {
            match edit {
                Optimistic::Flagged(flagged) => row.flagged = *flagged,
                Optimistic::Seen(seen) => row.unread = !seen,
                Optimistic::Hidden => {}
            }
        }
    }
}

fn edits_of<'a>(pending: &[&'a HashMap<RowKey, Vec<Optimistic>>], row: &Row) -> &'a [Optimistic] {
    pending
        .get(row.account)
        .and_then(|edits| edits.get(&row.key()))
        .map_or(&[][..], Vec::as_slice)
}

/// "Alice <alice@x>, bob@y" is "Alice"; without a name, the first address.
pub fn display_name(field: &str) -> String {
    let field = field.trim();
    if let Some(rest) = field.strip_prefix('"')
        && let Some((name, _)) = rest.split_once('"')
        && !name.trim().is_empty()
    {
        return name.trim().to_string();
    }
    let first = field.split(',').next().unwrap_or("").trim();
    match first.split_once('<') {
        Some((name, address)) => {
            let name = name.trim().trim_matches('"').trim();
            if name.is_empty() {
                address.trim_end_matches('>').trim().to_string()
            } else {
                name.to_string()
            }
        }
        None => first.to_string(),
    }
}

/// The time today, weekday and time within the past week, day and month this year, else day, month and year.
pub fn list_date<Tz: TimeZone>(ts: i64, now: &DateTime<Tz>) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let Some(at) = now.timezone().timestamp_opt(ts, 0).single() else {
        return String::new();
    };
    let days = now
        .date_naive()
        .signed_duration_since(at.date_naive())
        .num_days();
    let format = match days {
        0 => "%H:%M",
        1..=6 => "%a %H:%M",
        _ if at.year() == now.year() => "%-d %b",
        _ => "%-d %b %Y",
    };
    at.format(format).to_string()
}

/// One list row split into what each column draws, cleaned of control characters.
pub struct Columns {
    pub account: Option<String>,
    pub unread: bool,
    pub flagged: bool,
    pub marked: bool,
    pub who: String,
    pub subject: String,
    pub date: String,
    pub member: bool,
}

/// The unread dot's cell, then the flag's or mark's, before the sender.
const DOT: f32 = 12.0;
const MARKER: f32 = 20.0;
const MARKERS: f32 = DOT + MARKER;
const GAP: f32 = 12.0;
/// The list pane's narrowest width, so the unread dot and the flag never reach the date.
pub const MIN_WIDTH: f32 = 220.0;
/// A row's height: the sender and date, then the subject.
pub const LIST_ROW: f32 = 2.0 * ROW;

/// The sender's width in a row of `total` px whose date needs `date` px; the subject below has the full width.
pub fn sender_width(total: f32, date: f32) -> f32 {
    (total - MARKERS - date - GAP).max(0.0)
}

pub fn columns<Tz: TimeZone>(
    row: &Row,
    account: Option<&str>,
    marked: bool,
    recipient: bool,
    in_search: bool,
    now: &DateTime<Tz>,
) -> Columns
where
    Tz::Offset: std::fmt::Display,
{
    let who = display_name(if recipient { &row.to } else { &row.from });
    let mut subject = String::new();
    if in_search {
        subject.push_str(&format!("[{}] ", folders::label(&row.folder)));
    }
    subject.push_str(&row.subject);
    if row.count > 1 {
        subject.push_str(&format!(" ({})", row.count));
    }
    Columns {
        account: account.map(|name| clean(name, false)),
        unread: row.unread,
        flagged: row.flagged,
        marked,
        who: clean(&who, false),
        subject: clean(&subject, false),
        date: list_date(row.date, now),
        member: row.member,
    }
}

/// Unread dot, flag, sender (recipient in Sent and Drafts), subject, thread count and date, on one line.
pub fn row_text<Tz: TimeZone>(
    row: &Row,
    account: Option<&str>,
    recipient: bool,
    in_search: bool,
    now: &DateTime<Tz>,
) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let who = display_name(if recipient { &row.to } else { &row.from });
    let mut text = String::from(if row.unread { "• " } else { "   " });
    if row.flagged {
        text.push_str("⚑ ");
    }
    if row.member {
        text.push_str("      ");
    }
    if let Some(account) = account {
        text.push_str(&format!("{account} · "));
    }
    if in_search {
        text.push_str(&format!("[{}] ", folders::label(&row.folder)));
    }
    text.push_str(&format!("{who} — {}", row.subject));
    if row.count > 1 {
        text.push_str(&format!(" ({})", row.count));
    }
    text.push_str(&format!("  ·  {}", list_date(row.date, now)));
    clean(&text, false)
}

/// The scroll offset that shows the cursor row, moving the view as little as possible.
pub fn offset_showing(cursor: usize, row_height: f32, offset: f32, height: f32) -> f32 {
    let top = cursor as f32 * row_height;
    let bottom = top + row_height;
    if top < offset {
        top
    } else if bottom > offset + height {
        bottom - height
    } else {
        offset
    }
}

/// The folder All inboxes shows from each account; sync stores and queries it by this exact name.
pub const INBOX: &str = "INBOX";

/// Search hits of several accounts, newest first, at most `limit`; each account's own order is kept on equal dates.
pub fn merge_hits(mut hits: Vec<Row>, limit: u32) -> Vec<Row> {
    hits.sort_by_key(|row| std::cmp::Reverse(row.date));
    hits.truncate(limit as usize);
    hits
}

/// Threads of several accounts in one list, the most recently active first, at most `limit`. The sort is stable, so
/// each account keeps its own order and equal dates keep account order.
pub fn merge_threads(
    per_account: Vec<(usize, Vec<ThreadSummary>)>,
    limit: u32,
) -> Vec<(usize, ThreadSummary)> {
    let mut threads: Vec<(usize, ThreadSummary)> = per_account
        .into_iter()
        .flat_map(|(account, found)| found.into_iter().map(move |thread| (account, thread)))
        .collect();
    threads.sort_by_key(|(_, thread)| std::cmp::Reverse(thread.latest.date));
    threads.truncate(limit as usize);
    threads
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::*;
    use crate::gui::test_support::{Fixture, message};
    use crate::store::{MessageSummary, ThreadSummary};

    #[test]
    fn merge_hits_is_newest_first_and_capped() {
        let hit = |account, uid| Row::from_message(account, &message("INBOX", uid, "hit"));
        let merged = merge_hits(vec![hit(0, 3), hit(0, 1), hit(1, 4), hit(1, 2)], 3);
        let shape: Vec<(usize, u32)> = merged.iter().map(|r| (r.account, r.uid)).collect();
        assert_eq!(shape, [(1, 4), (0, 3), (1, 2)]);
    }

    #[test]
    fn merge_threads_is_newest_first_across_accounts_and_capped() {
        let thread = |uid: u32| ThreadSummary {
            thread_id: format!("t{uid}"),
            latest: summary(uid, ""),
            count: 1,
            unread: false,
            flagged: false,
        };
        let merged = merge_threads(
            vec![
                (0, vec![thread(5), thread(1)]),
                (1, vec![thread(3), thread(1)]),
            ],
            3,
        );
        let shape: Vec<(usize, u32)> = merged
            .iter()
            .map(|(account, t)| (*account, t.latest.uid))
            .collect();
        assert_eq!(
            shape,
            [(0, 5), (1, 3), (0, 1)],
            "equal dates keep account order; the cap drops the rest"
        );
    }

    fn summary(uid: u32, flags: &str) -> MessageSummary {
        MessageSummary {
            uid,
            from: format!("Sender {uid} <s{uid}@example.com>"),
            to: "me@example.com".into(),
            subject: format!("subject {uid}"),
            date: 1_790_000_000 + i64::from(uid),
            flags: flags.into(),
        }
    }

    #[test]
    fn build_rows_puts_members_under_an_expanded_thread() {
        let threads = vec![
            (
                0,
                ThreadSummary {
                    thread_id: "t".into(),
                    latest: summary(5, ""),
                    count: 2,
                    unread: true,
                    flagged: false,
                },
            ),
            (
                0,
                ThreadSummary {
                    thread_id: "u".into(),
                    latest: summary(3, "\\Seen \\Flagged"),
                    count: 1,
                    unread: false,
                    flagged: true,
                },
            ),
        ];
        let mut expanded = HashMap::new();
        assert_eq!(build_rows("INBOX", &threads, &expanded).len(), 2);
        expanded.insert(
            (0, "t".to_string()),
            vec![summary(4, "\\Seen"), summary(5, "")],
        );
        let rows = build_rows("INBOX", &threads, &expanded);
        let shape: Vec<(u32, bool, bool, u32)> = rows
            .iter()
            .map(|r| (r.uid, r.member, r.unread, r.count))
            .collect();
        assert_eq!(
            shape,
            [
                (5, false, true, 2),
                (4, true, false, 1),
                (5, true, true, 1),
                (3, false, false, 1)
            ]
        );
        assert_eq!(rows[0].thread_id.as_deref(), Some("t"));
        assert!(rows[3].flagged && rows[1].thread_id.is_none());
    }

    #[test]
    fn list_date_is_time_today_weekday_and_time_this_week_day_month_this_year_else_with_year() {
        let now = Utc.with_ymd_and_hms(2026, 10, 6, 12, 0, 0).unwrap();
        let at = |y, mo, d, h, mi| {
            Utc.with_ymd_and_hms(y, mo, d, h, mi, 0)
                .unwrap()
                .timestamp()
        };
        assert_eq!(list_date(at(2026, 10, 6, 9, 30), &now), "09:30");
        assert_eq!(list_date(at(2026, 10, 4, 18, 0), &now), "Sun 18:00");
        assert_eq!(list_date(at(2026, 9, 3, 8, 0), &now), "3 Sep");
        assert_eq!(list_date(at(2025, 12, 10, 8, 0), &now), "10 Dec 2025");
    }

    #[test]
    fn columns_split_markers_sender_subject_and_date() {
        let now = Utc.with_ymd_and_hms(2026, 10, 6, 12, 0, 0).unwrap();
        let mut row = Row::from_message(0, &message("INBOX", 1, "Lunch?"));
        row.unread = true;
        row.flagged = true;
        row.count = 3;
        row.from = "Linus Example <linus@example.com>".into();
        row.date = Utc
            .with_ymd_and_hms(2026, 10, 6, 9, 13, 0)
            .unwrap()
            .timestamp();
        let c = columns(&row, None, false, false, true, &now);
        assert!(c.unread && c.flagged && !c.marked);
        assert_eq!(c.who, "Linus Example");
        assert_eq!(c.subject, "[Inbox] Lunch? (3)");
        assert_eq!(c.date, "09:13");
    }

    #[test]
    fn the_account_tag_comes_before_the_sender() {
        let now = Utc.with_ymd_and_hms(2026, 10, 6, 12, 0, 0).unwrap();
        let row = Row::from_message(1, &message("INBOX", 1, "hello"));
        let text = row_text(&row, Some("work"), false, false, &now);
        assert!(text.starts_with("   work · Sender 1 — hello"), "{text}");
        assert!(row_text(&row, None, false, false, &now).starts_with("   Sender 1 — hello"));
        assert_eq!(
            columns(&row, Some("work"), false, false, false, &now)
                .account
                .as_deref(),
            Some("work")
        );
        assert_eq!(columns(&row, None, false, false, false, &now).account, None);
    }

    #[test]
    fn the_sender_keeps_clear_of_the_date_and_never_goes_negative() {
        assert_eq!(sender_width(600.0, 80.0), 600.0 - 32.0 - 80.0 - 12.0);
        assert_eq!(sender_width(100.0, 80.0), 0.0);
    }

    #[test]
    fn display_name_prefers_the_name_then_the_address() {
        assert_eq!(display_name("Alice <alice@example.com>"), "Alice");
        assert_eq!(
            display_name("\"Doe, John\" <john@example.com>, bob@example.com"),
            "Doe, John"
        );
        assert_eq!(display_name("<bare@example.com>"), "bare@example.com");
        assert_eq!(
            display_name("plain@example.com, other@example.com"),
            "plain@example.com"
        );
        assert_eq!(display_name(""), "");
    }

    #[test]
    fn offset_showing_scrolls_only_as_far_as_needed() {
        assert_eq!(offset_showing(0, 20.0, 0.0, 100.0), 0.0);
        assert_eq!(offset_showing(4, 20.0, 0.0, 100.0), 0.0);
        assert_eq!(offset_showing(5, 20.0, 0.0, 100.0), 20.0);
        assert_eq!(offset_showing(1, 20.0, 60.0, 100.0), 20.0);
    }

    #[test]
    fn row_text_strips_control_characters_from_headers() {
        let row = Row {
            account: 0,
            count: 1,
            date: 0,
            flagged: true,
            folder: "INBOX".into(),
            from: "Evil\u{1b}[2J <e@example.com>".into(),
            member: false,
            subject: "\u{1b}[31mred\u{7} alert".into(),
            thread_id: None,
            to: String::new(),
            uid: 1,
            unread: true,
        };
        let now = Utc.with_ymd_and_hms(2026, 10, 6, 12, 0, 0).unwrap();
        let text = row_text(&row, None, false, false, &now);
        assert!(!text.chars().any(char::is_control), "{text:?}");
        assert!(
            text.contains("[31mred alert") && text.starts_with("• ⚑ "),
            "{text}"
        );
    }

    #[test]
    fn apply_pending_hides_and_overrides_rows() {
        let thread = |id: &str, uid| ThreadSummary {
            thread_id: id.into(),
            latest: summary(uid, ""),
            count: 1,
            unread: true,
            flagged: false,
        };
        let threads = vec![
            (0, thread("a", 1)),
            (0, thread("b", 2)),
            (0, thread("c", 3)),
        ];
        let mut rows = build_rows("INBOX", &threads, &HashMap::new());
        let pending = HashMap::from([
            (("INBOX".to_string(), 1), vec![Optimistic::Hidden]),
            (("INBOX".to_string(), 2), vec![Optimistic::Seen(true)]),
            (
                ("INBOX".to_string(), 3),
                vec![
                    Optimistic::Flagged(true),
                    Optimistic::Flagged(false),
                    Optimistic::Flagged(true),
                ],
            ),
        ]);
        apply_pending(&mut rows, &[&pending]);
        let shape: Vec<(u32, bool, bool)> =
            rows.iter().map(|r| (r.uid, r.unread, r.flagged)).collect();
        assert_eq!(shape, [(2, false, false), (3, true, true)]);
    }

    #[test]
    fn apply_pending_reads_each_rows_own_account() {
        let thread = |id: &str| ThreadSummary {
            thread_id: id.into(),
            latest: summary(1, ""),
            count: 1,
            unread: true,
            flagged: false,
        };
        let mut rows = build_rows(
            "INBOX",
            &[(0, thread("a")), (1, thread("b"))],
            &HashMap::new(),
        );
        let home: HashMap<RowKey, Vec<Optimistic>> = HashMap::new();
        let work = HashMap::from([(("INBOX".to_string(), 1), vec![Optimistic::Hidden])]);
        apply_pending(&mut rows, &[&home, &work]);
        let left: Vec<(usize, u32)> = rows.iter().map(|r| (r.account, r.uid)).collect();
        assert_eq!(left, [(0, 1)]);
    }

    #[test]
    fn expanding_a_thread_in_one_account_leaves_the_same_thread_in_another_alone() {
        let thread = ThreadSummary {
            thread_id: "t".into(),
            latest: summary(5, ""),
            count: 2,
            unread: true,
            flagged: false,
        };
        let threads = vec![(0, thread.clone()), (1, thread)];
        let expanded = HashMap::from([(
            (1, "t".to_string()),
            vec![summary(4, "\\Seen"), summary(5, "")],
        )]);
        let rows = build_rows("INBOX", &threads, &expanded);
        let shape: Vec<(usize, u32, bool)> =
            rows.iter().map(|r| (r.account, r.uid, r.member)).collect();
        assert_eq!(
            shape,
            [(0, 5, false), (1, 5, false), (1, 4, true), (1, 5, true)]
        );
    }

}
