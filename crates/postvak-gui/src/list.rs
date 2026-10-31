//! The message list without a toolkit: one row per thread, followed by its members when expanded; columns, dates and
//! the accessible row names. The window draws it (`ui/list.slint`, `rows.rs`).
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::Arc;

use chrono::{DateTime, Datelike, TimeZone};

use postvak_core::bimi::Brand;
use postvak_core::message::{clean, display_name};
use postvak_core::rules::Action;
use postvak_core::store::{Message, MessageSummary, ThreadSummary};

use super::folders;
use postvak_core::store::Filter;

use super::state::{App, UiAction, View};
use super::theme::LIST_ROW;

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
    /// A search hit that matched only through a close match, such as a corrected typo; listed after the exact ones.
    pub close: bool,
    /// For a search hit that matched in its body: the text around the match, shown in place of the subject.
    pub snippet: Option<String>,
    /// The headers' guess that it carries an attachment.
    pub attachment: bool,
    /// The start of the body text; empty until the body is indexed.
    pub preview: String,
    /// The sender's logo record and its SVG, drawn instead of the initials; set while the row is in view.
    pub logo: Option<(String, Arc<str>)>,
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
            close: false,
            snippet: None,
            attachment: false,
            preview: String::new(),
            logo: None,
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
            close: false,
            snippet: None,
            attachment: message.attachment,
            preview: message.preview.clone(),
            logo: None,
        }
    }
}

#[derive(Default)]
pub(crate) struct ListState {
    pub cursor: usize,
    /// Expanded threads by account and thread id: mail sent to two accounts shares its thread id.
    pub expanded: HashMap<(usize, String), Vec<MessageSummary>>,
    /// Set when the cursor moved, so the list scrolls it into view; cleared once the list reports where it scrolled.
    pub follow_cursor: bool,
    pub hits: Vec<Row>,
    pub marked: HashSet<(usize, RowKey)>,
    /// What the servers found for the current query, by account; the rows come from the store.
    pub server_hits: Vec<(usize, RowKey)>,
    /// How many accounts' servers were asked for the current query and have not answered.
    pub server_searching: usize,
    /// Whether the servers were asked for the current query.
    pub server_asked: bool,
    /// The accounts whose server could not search for the current query; their local results stand.
    pub server_failed: Vec<String>,
    /// Over the accounts the current search covers: messages whose body is not indexed yet, and messages in all.
    pub unindexed: u64,
    pub covered: u64,
    pub rows: Vec<Row>,
    /// Each thread with the account it is in.
    pub threads: Vec<(usize, ThreadSummary)>,
    /// Scroll offset and height of the list from the last frame.
    pub viewport: (f32, f32),
    /// The brand of each row that came into view, None for one without; read once, as each needs a store query.
    pub brands: HashMap<(usize, RowKey), Option<Brand>>,
    /// Set when a scroll brought a logo into view, so the window draws the rows again.
    pub logos_changed: bool,
}

impl ListState {
    /// Whether the cursor's row is in view, or as much of it as a list lower than a row shows; not before the list
    /// has reported its height.
    pub fn cursor_shown(&self, search: bool) -> bool {
        let (offset, height) = self.viewport;
        let (above, own) = self.strips(search);
        height > 0.0
            && (height < LIST_ROW
                || offset_showing(self.cursor, LIST_ROW, above, own, offset, height) == offset)
    }

    /// The heading strips the cursor's row needs room for: those of the rows above it, and its own.
    pub fn strips(&self, search: bool) -> (f32, f32) {
        let now = chrono::Local::now();
        let all = strips_above(&self.rows, self.cursor, search, &now);
        let own = usize::from(section(&self.rows, self.cursor, search, &now).is_some());
        let strip = super::theme::SECTION;
        ((all - own) as f32 * strip, own as f32 * strip)
    }
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
            Action::Archive
            | Action::Delete
            | Action::Junk
            | Action::Move(_)
            | Action::NotJunk
            | Action::Trash => Some(Optimistic::Hidden),
            Action::Flag => Some(Optimistic::Flagged(true)),
            Action::MarkRead => Some(Optimistic::Seen(true)),
            Action::MarkUnread => Some(Optimistic::Seen(false)),
            Action::Unflag => Some(Optimistic::Flagged(false)),
            Action::After(_) | Action::Notify | Action::Silent | Action::Stop | Action::Tag(_) => {
                None
            }
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

/// The list's heading: the folder's name, or All inboxes; none in the rules views.
pub(crate) fn title(view: &View) -> Option<&str> {
    match view {
        View::AllInboxes => Some("All inboxes"),
        View::Folder { folder, .. } => Some(folders::label(folder)),
        _ => None,
    }
}

/// The search field's hint.
pub(crate) fn search_hint(view: &View) -> &'static str {
    if *view == View::AllInboxes {
        "Search every account"
    } else {
        "Search this account"
    }
}

/// The line under the search field.
pub(crate) fn results(app: &App) -> String {
    let note = if app.list.server_searching > 0 {
        "searching the server…".to_string()
    } else if let [account] = app.list.server_failed.as_slice() {
        format!("the server search failed for {account}")
    } else if !app.list.server_failed.is_empty() {
        format!(
            "the server search failed for {}",
            super::numbers::counted(app.list.server_failed.len() as u64, "account", "accounts")
        )
    } else if app.list.server_asked {
        "with what the server found".to_string()
    } else if app.list.unindexed > 0 {
        format!(
            "Enter also searches the server for the {} not indexed yet",
            super::numbers::count(app.list.unindexed)
        )
    } else {
        format!(
            "in {}",
            super::numbers::counted(app.list.covered, "message", "messages")
        )
    };
    let count = super::numbers::counted(app.list.rows.len() as u64, "result", "results");
    format!("{count} · {note}")
}

/// What the banner above an inbox's rows says while rule proposals wait for approval; None in other views, in a
/// search and with nothing waiting. One proposal is named, with how much mail it would touch once its dry run is in.
pub(crate) fn banner(app: &App) -> Option<String> {
    let inbox = match &app.view {
        View::AllInboxes => true,
        View::Folder { folder, .. } => folder.eq_ignore_ascii_case("INBOX"),
        _ => false,
    };
    if !inbox || app.search.is_some() {
        return None;
    }
    let waiting: Vec<_> = (app.rules.file.rules.iter())
        .filter(|rule| super::rules::is_proposal(rule))
        .collect();
    let lists = &app.rules.file.list_proposals;
    let rule = match (waiting.as_slice(), lists.as_slice()) {
        ([], []) => return None,
        ([], [list]) => {
            let who = agent(&list.proposed_by);
            return Some(format!(
                "{who} suggests changing list {}.",
                clean(&list.name, false)
            ));
        }
        ([rule], []) => rule,
        (_, []) => return Some(format!("{} rules waiting for you", waiting.len())),
        _ => {
            return Some(format!(
                "{} proposals waiting for you",
                waiting.len() + lists.len()
            ));
        }
    };
    let who = agent(rule.proposed_by.as_deref().unwrap_or_default());
    let suggests = suggestion(rule);
    let affects = app
        .rules
        .dry_runs
        .get(&rule.name)
        .filter(|run| run.error.is_none())
        .map(|run| {
            let noun = super::numbers::noun(run.count as u64, "message", "messages");
            format!(
                " Would affect {} {noun}.",
                super::numbers::count(run.count as u64)
            )
        })
        .unwrap_or_default();
    Some(format!("{who} suggests {suggests}.{affects}"))
}

/// Who proposed something, as the banner names them: Claude, an agent, or the proposer's own name.
fn agent(proposed_by: &str) -> String {
    match super::rules::proposer(proposed_by) {
        who if who.starts_with("claude") => "Claude".to_string(),
        "" | "agent" | "mcp" => "An agent".to_string(),
        who => clean(who, false),
    }
}

/// What a proposal would do, said with its name as the canvas says it: "archiving kettle newsletters", from its first
/// action that changes mail.
fn suggestion(rule: &postvak_core::rules::Rule) -> String {
    use postvak_core::rules::Action;
    let name = clean(&rule.name, false);
    let said = |action: &Action| match action {
        Action::Archive => Some(format!("archiving {name}")),
        Action::Delete => Some(format!("deleting {name}")),
        Action::Flag => Some(format!("flagging {name}")),
        Action::Junk => Some(format!("marking {name} as junk")),
        Action::MarkRead => Some(format!("marking {name} read")),
        Action::Move(folder) => Some(format!(
            "moving {name} to {}",
            clean(folders::label(folder), false)
        )),
        Action::Tag(keyword) => Some(format!("tagging {name} {}", clean(keyword, false))),
        Action::Trash => Some(format!("moving {name} to Trash")),
        _ => None,
    };
    rule.actions
        .iter()
        .find_map(said)
        .unwrap_or_else(|| format!("the rule {name}"))
}

/// What an empty list says instead of rows; nothing before a search query is typed.
pub(crate) fn empty(view: &View, search: Option<&str>, filter: Filter) -> String {
    let kind =
        filter_label(filter).map_or(String::new(), |label| format!("{} ", label.to_lowercase()));
    let inbox = match view {
        View::AllInboxes => true,
        View::Folder { folder, .. } => folder.eq_ignore_ascii_case("INBOX"),
        _ => false,
    };
    match search {
        Some("") => String::new(),
        Some(_) => "No mail matches.".into(),
        None if inbox && filter == Filter::default() => "Inbox zero".into(),
        None if *view == View::AllInboxes => format!("No {kind}mail in any inbox."),
        None => format!("No {kind}mail in this folder."),
    }
}

/// The chosen filter's name beside the list's title; none for All, which needs no reminder.
pub(crate) fn filter_label(filter: Filter) -> Option<&'static str> {
    FILTERS
        .iter()
        .find(|(_, f)| *f == filter && filter != Filter::default())
        .map(|(label, _)| *label)
}

/// The list's filter menu, top to bottom.
pub(crate) const FILTERS: [(&str, Filter); 4] = [
    (
        "All",
        Filter {
            unread: false,
            flagged: false,
            attachments: false,
        },
    ),
    (
        "Unread",
        Filter {
            unread: true,
            flagged: false,
            attachments: false,
        },
    ),
    (
        "Flagged",
        Filter {
            unread: false,
            flagged: true,
            attachments: false,
        },
    ),
    (
        "Attachments",
        Filter {
            unread: false,
            flagged: false,
            attachments: true,
        },
    ),
];

/// The text beside the pointer while rows are dragged: how many marked rows move, else the row's subject, shortened.
pub(crate) fn drag_label(marked: usize, subject: &str) -> String {
    const LONGEST: usize = 40;
    if marked > 1 {
        super::numbers::counted(marked as u64, "message", "messages")
    } else if subject.chars().count() > LONGEST {
        format!("{}…", subject.chars().take(LONGEST - 1).collect::<String>())
    } else {
        subject.to_string()
    }
}

/// The right-click menu of a message row, in three groups; its items act like the toolbar, on the marked rows or this
/// one.
pub(crate) fn menu(row: &Row, in_junk: bool) -> [Vec<(&'static str, UiAction)>; 3] {
    let (read, read_label) = if row.unread {
        (Action::MarkRead, "Mark as read")
    } else {
        (Action::MarkUnread, "Mark as unread")
    };
    let (flag, flag_label) = if row.flagged {
        (Action::Unflag, "Unflag")
    } else {
        (Action::Flag, "Flag")
    };
    [
        vec![
            (read_label, UiAction::Act(read)),
            (flag_label, UiAction::Act(flag)),
        ],
        vec![
            ("Archive", UiAction::Act(Action::Archive)),
            ("Move to…", UiAction::OpenMovePicker),
            ("Delete", UiAction::Act(Action::Trash)),
            (
                if in_junk { "Not junk" } else { "Junk" },
                UiAction::Act(Action::Junk),
            ),
        ],
        vec![
            ("View source", UiAction::OpenSource),
            ("Save .eml", UiAction::SaveEml),
            ("Save .eml as…", UiAction::SaveEmlAs),
        ],
    ]
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
    pub who: String,
    pub subject: String,
    /// A thread's message count, for the pill beside the sender; None for one message.
    pub count: Option<String>,
    pub date: String,
    /// The body's start on one line.
    pub preview: String,
    /// The avatar's letters and which of `Theme.avatars` they sit on.
    pub initials: String,
    pub tint: usize,
}

/// The list pane's narrowest width, so the unread dot and the flag never reach the date and a long title keeps room
/// for the filter chip; the body pane keeps it too.
pub const MIN_WIDTH: f32 = 320.0;

pub fn columns<Tz: TimeZone>(
    row: &Row,
    account: Option<&str>,
    recipient: bool,
    in_search: bool,
    now: &DateTime<Tz>,
) -> Columns
where
    Tz::Offset: std::fmt::Display,
{
    let party = if recipient { &row.to } else { &row.from };
    let who = display_name(party);
    let mut subject = String::new();
    if in_search {
        subject.push_str(&format!("[{}] ", folders::label(&row.folder)));
    }
    // A body hit shows where it matched; the subject did not, or it would have matched there.
    subject.push_str(row.snippet.as_deref().unwrap_or(&row.subject));
    let (initials, tint) = super::avatar::of(party);
    Columns {
        account: account.map(|name| clean(name, false)),
        who: clean(&who, false),
        subject: clean(&subject, false),
        count: (row.count > 1).then(|| row.count.to_string()),
        date: list_date(row.date, now),
        preview: clean(
            &row.preview.split_whitespace().collect::<Vec<_>>().join(" "),
            false,
        ),
        initials,
        tint,
    }
}

/// Unread dot, flag, then the row's `columns` on one line.
pub fn row_text(row: &Row, columns: &Columns) -> String {
    let mut text = String::from(if row.unread { "• " } else { "   " });
    if row.flagged {
        text.push_str("⚑ ");
    }
    if row.member {
        text.push_str("      ");
    }
    if let Some(account) = &columns.account {
        text.push_str(&format!("{account} · "));
    }
    let count = columns
        .count
        .as_ref()
        .map_or(String::new(), |count| format!(" ({count})"));
    text.push_str(&format!(
        "{} — {}{count}  ·  {}",
        columns.who, columns.subject, columns.date
    ));
    if row.close {
        text.push_str(" · close match");
    }
    text
}

/// The scroll offset that shows the cursor row, moving the view as little as possible. `above` is what sits above
/// the row besides whole rows, the heading strips of the rows before it; `own` is the row's own heading strip, which
/// shows with it.
pub fn offset_showing(
    cursor: usize,
    row_height: f32,
    above: f32,
    own: f32,
    offset: f32,
    height: f32,
) -> f32 {
    let top = cursor as f32 * row_height + above;
    let bottom = top + own + row_height;
    if top < offset {
        top
    } else if bottom > offset + height {
        bottom - height
    } else {
        offset
    }
}

/// The rows the list shows at `viewport` (offset and height): each is `LIST_ROW` high, below its heading strip if it
/// has one.
pub fn visible<Tz: TimeZone>(
    rows: &[Row],
    search: bool,
    (offset, height): (f32, f32),
    now: &DateTime<Tz>,
) -> Range<usize>
where
    Tz::Offset: std::fmt::Display,
{
    let (mut first, mut top) = (None, 0.0);
    for index in 0..rows.len() {
        if section(rows, index, search, now).is_some() {
            top += super::theme::SECTION;
        }
        if top >= offset + height {
            return first.unwrap_or(index)..index;
        }
        top += LIST_ROW;
        if first.is_none() && top > offset {
            first = Some(index);
        }
    }
    first.unwrap_or(rows.len())..rows.len()
}

/// The folder All inboxes shows from each account; sync stores and queries it by this exact name.
pub const INBOX: &str = "INBOX";

/// Search hits of several accounts: the exact ones newest first, then the close ones newest first, at most `limit`;
/// each account's own order is kept on equal dates.
pub fn merge_hits(mut hits: Vec<Row>, limit: u32) -> Vec<Row> {
    hits.sort_by_key(|row| (row.close, std::cmp::Reverse(row.date)));
    hits.truncate(limit as usize);
    hits
}

/// The label above the first close match, when the list has one.
pub const CLOSE_MATCHES: &str = "Close matches";

/// Which date group a row dated `ts` falls in: Today, Yesterday, This week, then its month, with the year once it is
/// not this year's.
pub fn date_section<Tz: TimeZone>(ts: i64, now: &DateTime<Tz>) -> String
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
    match days {
        ..=0 => "Today".into(),
        1 => "Yesterday".into(),
        2..=6 => "This week".into(),
        _ if at.year() == now.year() => at.format("%B").to_string(),
        _ => at.format("%B %Y").to_string(),
    }
}

/// The heading above row `index`: in a search the "Close matches" strip on the first close hit; otherwise the date
/// group of a thread whose group differs from the thread above it. Members of an expanded thread never get one.
pub fn section<Tz: TimeZone>(
    rows: &[Row],
    index: usize,
    search: bool,
    now: &DateTime<Tz>,
) -> Option<String>
where
    Tz::Offset: std::fmt::Display,
{
    if search {
        return (divider(rows) == Some(index)).then(|| CLOSE_MATCHES.to_string());
    }
    let row = rows.get(index).filter(|row| !row.member)?;
    let group = date_section(row.date, now);
    let above = rows[..index].iter().rev().find(|row| !row.member);
    match above {
        Some(above) if date_section(above.date, now) == group => None,
        _ => Some(group),
    }
}

/// How many heading strips sit above the bottom of row `cursor`, its own included, for the scroll offset.
pub fn strips_above<Tz: TimeZone>(
    rows: &[Row],
    cursor: usize,
    search: bool,
    now: &DateTime<Tz>,
) -> usize
where
    Tz::Offset: std::fmt::Display,
{
    (0..=cursor.min(rows.len().saturating_sub(1)))
        .filter(|&index| section(rows, index, search, now).is_some())
        .count()
}

/// The row the "Close matches" strip sits on: the first close hit.
pub fn divider(rows: &[Row]) -> Option<usize> {
    rows.iter().position(|row| row.close)
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
    use crate::test_support::message;
    use postvak_core::store::{MessageSummary, ThreadSummary};

    #[test]
    fn merge_hits_is_newest_first_and_capped() {
        let hit = |account, uid| Row::from_message(account, &message("INBOX", uid, "hit"));
        let merged = merge_hits(vec![hit(0, 3), hit(0, 1), hit(1, 4), hit(1, 2)], 3);
        let shape: Vec<(usize, u32)> = merged.iter().map(|r| (r.account, r.uid)).collect();
        assert_eq!(shape, [(1, 4), (0, 3), (1, 2)]);
    }

    #[test]
    fn close_matches_come_after_the_exact_ones_and_the_strip_sits_on_the_first() {
        let hit = |uid, close| {
            let mut row = Row::from_message(0, &message("INBOX", uid, "hit"));
            row.close = close;
            row
        };
        let merged = merge_hits(
            vec![hit(5, true), hit(1, false), hit(9, true), hit(3, false)],
            10,
        );
        let shape: Vec<(u32, bool)> = merged.iter().map(|r| (r.uid, r.close)).collect();
        assert_eq!(shape, [(3, false), (1, false), (9, true), (5, true)]);
        assert_eq!(divider(&merged), Some(2));
        assert_eq!(divider(&merged[..2]), None);
        assert!(
            row_text(
                &merged[2],
                &columns(&merged[2], None, false, true, &Utc::now())
            )
            .ends_with(" · close match")
        );
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
            attachment: false,
            preview: String::new(),
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
        row.preview = "Hi Robin,\n\n  Shall we\ttry\u{1b}[2J lunch?".into();
        let c = columns(&row, None, false, true, &now);
        assert_eq!(c.who, "Linus Example");
        assert_eq!(c.subject, "[Inbox] Lunch?");
        assert_eq!(c.count.as_deref(), Some("3"));
        assert_eq!(c.date, "09:13");
        assert_eq!(c.preview, "Hi Robin, Shall we try[2J lunch?");
        assert_eq!(c.initials, "LE");
        assert!(
            row_text(&row, &c).contains("Lunch? (3)"),
            "screen readers still hear the count"
        );
    }

    #[test]
    fn rows_fall_into_today_yesterday_this_week_then_months() {
        let now = Utc.with_ymd_and_hms(2026, 10, 6, 12, 0, 0).unwrap();
        let at = |y, mo, d| Utc.with_ymd_and_hms(y, mo, d, 9, 0, 0).unwrap().timestamp();
        assert_eq!(date_section(at(2026, 10, 6), &now), "Today");
        assert_eq!(date_section(at(2026, 10, 5), &now), "Yesterday");
        assert_eq!(date_section(at(2026, 10, 1), &now), "This week");
        assert_eq!(date_section(at(2026, 9, 29), &now), "September");
        assert_eq!(date_section(at(2025, 12, 10), &now), "December 2025");
    }

    #[test]
    fn a_heading_starts_each_date_group_and_search_keeps_only_close_matches() {
        let now = Utc.with_ymd_and_hms(2026, 10, 6, 12, 0, 0).unwrap();
        let at = |d, h| {
            Utc.with_ymd_and_hms(2026, 10, d, h, 0, 0)
                .unwrap()
                .timestamp()
        };
        let mut rows: Vec<Row> = [(6, 11), (6, 9), (6, 8), (5, 9)]
            .into_iter()
            .enumerate()
            .map(|(uid, (day, hour))| {
                let mut row = Row::from_message(0, &message("INBOX", uid as u32, "x"));
                row.date = at(day, hour);
                row
            })
            .collect();
        rows[2].member = true;
        let headings: Vec<Option<String>> = (0..rows.len())
            .map(|i| section(&rows, i, false, &now))
            .collect();
        let expected = [
            Some("Today".to_string()),
            None,
            None,
            Some("Yesterday".into()),
        ];
        assert_eq!(headings, expected);
        assert_eq!(strips_above(&rows, 0, false, &now), 1);
        assert_eq!(strips_above(&rows, 3, false, &now), 2);
        rows[3].close = true;
        let headings: Vec<Option<String>> = (0..rows.len())
            .map(|i| section(&rows, i, true, &now))
            .collect();
        assert_eq!(
            headings,
            [None, None, None, Some(CLOSE_MATCHES.to_string())]
        );
        assert_eq!(strips_above(&rows, 2, true, &now), 0);
    }

    #[test]
    fn visible_rows_count_the_heading_strips_above_them() {
        let now = Utc.with_ymd_and_hms(2026, 10, 6, 12, 0, 0).unwrap();
        let today = now.timestamp() - 3600;
        let mut rows: Vec<Row> = (0..4)
            .map(|uid| {
                let mut row = Row::from_message(0, &message("INBOX", uid, "x"));
                row.date = today;
                row
            })
            .collect();
        rows[2].member = true;
        rows[3].date = today - 86_400;
        // Today's strip, rows 0 to 2, Yesterday's strip, row 3: rows span 32-112, 112-192, 192-272 and 304-384.
        assert_eq!(visible(&rows, false, (0.0, 100.0), &now), 0..1);
        assert_eq!(visible(&rows, false, (120.0, 100.0), &now), 1..3);
        assert_eq!(visible(&rows, false, (280.0, 30.0), &now), 3..4);
        assert_eq!(visible(&rows, false, (0.0, 1000.0), &now), 0..4);
        assert!(visible(&rows, false, (0.0, 0.0), &now).is_empty());
    }

    #[test]
    fn the_account_tag_comes_before_the_sender() {
        let now = Utc.with_ymd_and_hms(2026, 10, 6, 12, 0, 0).unwrap();
        let row = Row::from_message(1, &message("INBOX", 1, "hello"));
        let text = row_text(&row, &columns(&row, Some("work"), false, false, &now));
        assert!(text.starts_with("   work · Sender 1 — hello"), "{text}");
        let text = row_text(&row, &columns(&row, None, false, false, &now));
        assert!(text.starts_with("   Sender 1 — hello"), "{text}");
        assert_eq!(
            columns(&row, Some("work"), false, false, &now)
                .account
                .as_deref(),
            Some("work")
        );
        assert_eq!(columns(&row, None, false, false, &now).account, None);
    }

    #[test]
    fn offset_showing_scrolls_only_as_far_as_needed() {
        assert_eq!(offset_showing(0, 20.0, 0.0, 0.0, 0.0, 100.0), 0.0);
        assert_eq!(offset_showing(4, 20.0, 0.0, 0.0, 0.0, 100.0), 0.0);
        assert_eq!(offset_showing(5, 20.0, 0.0, 0.0, 0.0, 100.0), 20.0);
        assert_eq!(offset_showing(1, 20.0, 0.0, 0.0, 60.0, 100.0), 20.0);
        assert_eq!(
            offset_showing(4, 20.0, 8.0, 0.0, 0.0, 100.0),
            8.0,
            "the strip pushes the row down"
        );
        assert_eq!(
            offset_showing(0, 20.0, 0.0, 8.0, 50.0, 100.0),
            0.0,
            "the row's own heading shows with it"
        );
        assert_eq!(offset_showing(4, 20.0, 0.0, 8.0, 0.0, 100.0), 8.0);
    }

    #[test]
    fn a_body_hits_snippet_takes_the_subjects_place_on_the_second_line() {
        let mut row = Row::from_message(0, &message("Archive", 7, "Lunch on Friday?"));
        row.snippet = Some("…the menu is at example.com and they take bookings…".into());
        let now = Utc.with_ymd_and_hms(2026, 10, 24, 12, 0, 0).unwrap();
        let shown = columns(&row, None, false, true, &now);
        assert_eq!(
            shown.subject,
            "[Archive] …the menu is at example.com and they take bookings…"
        );
        row.snippet = None;
        assert_eq!(
            columns(&row, None, false, true, &now).subject,
            "[Archive] Lunch on Friday?"
        );
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
            close: false,
            snippet: None,
            attachment: false,
            preview: String::new(),
            logo: None,
        };
        let now = Utc.with_ymd_and_hms(2026, 10, 6, 12, 0, 0).unwrap();
        let text = row_text(&row, &columns(&row, None, false, false, &now));
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

    use crate::state::Focus;
    use crate::test_support::{Fixture, unread, work};
    use postvak_core::rules::Action;
    use postvak_core::sync::Command;

    fn uids(open: &crate::test_support::Open) -> Vec<u32> {
        open.with(|live| live.app.list.rows.iter().map(|r| r.uid).collect())
    }

    #[test]
    fn a_row_in_view_shows_its_senders_logo() {
        let fx = Fixture::new(&["work"]);
        fx.append_config("[ui]\nsender_images = true\n");
        fx.add("work", crate::test_support::branded("INBOX", 1, "merged"));
        fx.add("work", message("INBOX", 2, "plain"));
        crate::test_support::cache_github_logo(&fx.paths);
        let (mut driver, _wires) = fx.driver();
        assert_eq!(
            driver.app.list.rows[0].uid, 2,
            "the reader shows the plain one"
        );
        assert!(!driver.app.logos.started(), "no row is in view yet");
        driver.apply(UiAction::ListViewport {
            offset: 0.0,
            height: 600.0,
        });
        driver.tick_until(|app| app.list.rows.iter().any(|row| row.logo.is_some()));
        let logos: Vec<_> = driver
            .app
            .list
            .rows
            .iter()
            .map(|row| (row.uid, row.logo.as_ref().map(|(key, _)| key.as_str())))
            .collect();
        assert_eq!(logos, [(2, None), (1, Some("default._bimi.github.com"))]);
    }

    #[test]
    fn the_window_draws_a_rows_logo() {
        use slint::Model;
        let fx = Fixture::new(&["work"]);
        fx.append_config("[ui]\nsender_images = true\n");
        fx.add("work", crate::test_support::branded("INBOX", 1, "merged"));
        crate::test_support::cache_github_logo(&fx.paths);
        let (open, _wires) = fx.window();
        open.tick_until(|open| {
            open.window()
                .get_rows()
                .row_data(0)
                .is_some_and(|row| row.has_logo)
        });
    }

    #[test]
    fn rows_name_their_account_in_all_inboxes_only() {
        let fx = Fixture::new(&["home", "work"]);
        fx.add("home", message("INBOX", 1, "from home"));
        let (open, _wires) = fx.window();
        assert_eq!(open.labels_containing("Sender 1 — from home").len(), 1);
        assert!(open.labels_containing("home · Sender 1").is_empty());
        open.with(|live| live.app.select_view(View::AllInboxes));
        assert_eq!(
            open.labels_containing("home · Sender 1 — from home").len(),
            1
        );
    }

    #[test]
    fn j_and_k_move_through_rows_newest_first() {
        let fx = Fixture::new(&["work"]);
        for uid in 1..=3 {
            fx.add("work", message("INBOX", uid, &format!("subject {uid}")));
        }
        let (open, _wires) = fx.window();
        assert_eq!(open.labels_containing("Sender 3 — subject 3").len(), 1);
        assert_eq!(uids(&open), [3, 2, 1]);
        open.press("j down");
        assert_eq!(open.with(|live| live.app.list.cursor), 2);
        open.press("j");
        assert_eq!(
            open.with(|live| live.app.list.cursor),
            2,
            "the cursor stops at the last row"
        );
        open.press("k");
        assert_eq!(open.with(|live| live.app.list.cursor), 1);
    }

    #[test]
    fn right_expands_a_thread_and_left_collapses_it_from_a_member() {
        let fx = Fixture::new(&["work"]);
        for uid in [4, 5] {
            let mut m = message("INBOX", uid, "thread");
            m.thread_id = "<t@example.com>".into();
            fx.add("work", m);
        }
        fx.add("work", message("INBOX", 1, "alone"));
        let (open, _wires) = fx.window();
        assert_eq!(uids(&open).len(), 2);
        assert_eq!(open.labels_containing("thread (2)").len(), 1);
        open.press("right");
        assert_eq!(uids(&open).len(), 4);
        open.press("down");
        assert!(open.with(|live| live.app.list.rows[1].member));
        open.press("left");
        assert_eq!(
            open.with(|live| (live.app.list.rows.len(), live.app.list.cursor)),
            (2, 0)
        );
        assert!(uids(&open).contains(&1));
    }

    #[test]
    fn a_marked_row_says_so_in_its_name() {
        let fx = Fixture::new(&["work"]);
        for uid in 1..=2 {
            fx.add("work", message("INBOX", uid, "hi"));
        }
        let (open, _wires) = fx.window();
        open.press("x");
        let marked: Vec<String> = open
            .labels_containing("Sender")
            .into_iter()
            .filter(|l| l.starts_with("✔ "))
            .collect();
        assert_eq!(marked.len(), 1, "{marked:?}");
    }

    #[test]
    fn the_list_is_titled_with_the_folder() {
        let fx = Fixture::new(&["work"]);
        fx.add("work", message("INBOX", 1, "read"));
        let unread = unread("INBOX", 2, "new");
        fx.add("work", unread);
        let (open, _wires) = fx.window();
        assert_eq!(
            open.find("Inbox").len(),
            1,
            "the title; the folder row says Inbox (1)"
        );
    }

    #[test]
    fn the_filter_menu_picks_a_filter_and_its_chip_clears_it() {
        let fx = Fixture::new(&["work"]);
        fx.add("work", message("INBOX", 1, "read"));
        fx.add("work", unread("INBOX", 2, "new"));
        let (open, _wires) = fx.window();
        assert!(open.find("Clear filter").is_empty(), "All needs no chip");
        assert!(open.find("Unread").is_empty(), "the menu is closed");
        open.click("Filter messages");
        for label in ["All", "Unread", "Flagged", "Attachments"] {
            assert_eq!(open.find(label).len(), 1, "{label} in the menu");
        }
        // The testing backend does not activate menu items; this is what Unread's does.
        open.window().invoke_list_filter_clicked(1);
        assert_eq!(open.with(|live| live.app.filter), FILTERS[1].1);
        assert_eq!(open.with(|live| live.app.list.rows.len()), 1);
        assert_eq!(
            open.get("Clear filter").accessible_description().unwrap(),
            "Unread"
        );
        open.click("Clear filter");
        assert_eq!(open.with(|live| live.app.filter), Filter::default());
        assert!(open.find("Clear filter").is_empty());
    }

    const PROPOSAL: &str = "[[rules]]\nname = \"codes\"\nenabled = false\nproposed_by = \"mcp:claude-desktop\"\nmatch.subject = { contains = \"code\" }\nactions = { delete = true }\n";

    #[test]
    fn the_inbox_says_a_proposal_waits_and_review_opens_rules() {
        let fx = Fixture::new(&["work"]);
        std::fs::write(fx.paths.rules_file(), PROPOSAL).unwrap();
        fx.add("work", message("INBOX", 1, "your code is 1234"));
        let (open, _wires) = fx.window();
        open.tick_until(|open| !open.labels_containing("Would affect 1 message.").is_empty());
        assert_eq!(
            open.labels_containing("Claude suggests deleting codes. Would affect 1 message.")
                .len(),
            1
        );
        open.click("Review");
        assert_eq!(open.with(|live| live.app.view.clone()), View::Rules);
    }

    #[test]
    fn the_banner_counts_several_and_leaves_other_folders_alone() {
        let fx = Fixture::new(&["work"]);
        fx.folder("work", "Receipts", None);
        let second = PROPOSAL.replace("codes", "more codes");
        std::fs::write(fx.paths.rules_file(), format!("{PROPOSAL}\n{second}")).unwrap();
        let (open, _wires) = fx.window();
        assert_eq!(open.labels_containing("2 rules waiting for you").len(), 1);
        open.click("Receipts");
        assert!(open.find("Review").is_empty());
    }

    #[test]
    fn the_banner_names_a_list_change_and_counts_it_with_rules() {
        let fx = Fixture::new(&["work"]);
        let change = "[lists]\nkids = [\"a.example\"]\n\n[[list_proposals]]\nname = \"kids\"\nproposed_by = \"mcp:claude\"\nvalue = [\"b.example\"]\n";
        std::fs::write(fx.paths.rules_file(), change).unwrap();
        let (open, _wires) = fx.window();
        assert_eq!(
            open.labels_containing("Claude suggests changing list kids.")
                .len(),
            1
        );
        drop(open);
        std::fs::write(fx.paths.rules_file(), format!("{change}\n{PROPOSAL}")).unwrap();
        let (open, _wires) = fx.window();
        assert_eq!(
            open.labels_containing("2 proposals waiting for you").len(),
            1
        );
    }

    #[test]
    fn only_an_unfiltered_inbox_is_at_inbox_zero() {
        let inbox = View::Folder {
            account: 0,
            folder: "INBOX".into(),
        };
        let sent = View::Folder {
            account: 0,
            folder: "Sent".into(),
        };
        let unread = FILTERS[1].1;
        assert_eq!(empty(&inbox, None, Filter::default()), "Inbox zero");
        assert_eq!(
            empty(&View::AllInboxes, None, Filter::default()),
            "Inbox zero"
        );
        assert_eq!(
            empty(&inbox, None, unread),
            "No unread mail in this folder."
        );
        assert_eq!(
            empty(&sent, None, Filter::default()),
            "No mail in this folder."
        );
    }

    #[test]
    fn only_a_filter_other_than_all_is_named() {
        assert_eq!(filter_label(Filter::default()), None);
        assert_eq!(filter_label(FILTERS[3].1), Some("Attachments"));
    }

    #[test]
    fn an_empty_list_says_so() {
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = fx.window();
        assert_eq!(open.find("Inbox zero").len(), 1);
        open.press("/");
        assert_eq!(open.find("Inbox zero").len(), 0, "no query yet");
        open.press("zzz");
        assert_eq!(open.find("No mail matches.").len(), 1);
    }

    #[test]
    fn a_row_shows_the_body_start_and_a_paperclip_for_an_attachment() {
        let fx = Fixture::new(&["work"]);
        let mut attached = message("INBOX", 1, "report");
        attached.headers = b"Content-Type: multipart/mixed; boundary=b\r\n\r\n".to_vec();
        attached.body_text = Some("See the\n  numbers attached.".into());
        fx.add("work", attached);
        let (open, _wires) = fx.window();
        let row = slint::Model::row_data(&open.window().get_rows(), 0).unwrap();
        assert_eq!(row.clip, crate::icons::PAPERCLIP);
        assert_eq!(row.preview, "See the numbers attached.");
        let date = open.with(|live| live.app.list.rows[0].date);
        let group = date_section(date, &chrono::Local::now()).to_uppercase();
        assert_eq!(row.section, group.as_str());
    }

    #[test]
    fn a_list_with_mail_shows_no_placeholder() {
        let fx = Fixture::new(&["work"]);
        fx.add("work", message("INBOX", 1, "hi"));
        let (open, _wires) = fx.window();
        assert_eq!(open.find("Inbox zero").len(), 0);
    }

    #[test]
    fn the_list_scrolls_to_the_cursor_however_it_moved() {
        let fx = Fixture::new(&["work"]);
        for uid in 1..=40 {
            fx.add("work", message("INBOX", uid, "hi"));
        }
        let (open, _wires) = fx.window();
        open.tick();
        let shown = |open: &crate::test_support::Open| {
            let top = -open.window().get_list_y();
            (top, top + open.with(|live| live.app.list.viewport.1))
        };
        assert_eq!(shown(&open).0, 0.0);
        open.with(|live| live.app.apply(crate::state::UiAction::SelectRow(30)));
        open.tick();
        let (top, bottom) = shown(&open);
        let row = 30.0 * crate::theme::LIST_ROW;
        assert!(
            top <= row && row + crate::theme::LIST_ROW <= bottom,
            "{top}..{bottom} hides row 30"
        );
    }

    #[test]
    fn another_folder_starts_at_the_top_of_its_list() {
        let fx = Fixture::new(&["work"]);
        fx.folder("work", "Archive", None);
        for uid in 1..=40 {
            fx.add("work", message("INBOX", uid, "hi"));
            fx.add("work", message("Archive", uid, "hi"));
        }
        let (open, _wires) = fx.window();
        open.tick();
        open.window().set_list_y(-400.0);
        open.tick();
        // The ListView settles near the offset: rows with a heading strip are taller than the ones it estimates.
        assert!(open.with(|live| live.app.list.viewport.0) > 300.0);
        open.with(|live| {
            live.app.select_view(View::Folder {
                account: 0,
                folder: "Archive".into(),
            })
        });
        open.tick();
        assert_eq!(open.window().get_list_y(), 0.0);
    }

    #[test]
    fn a_scroll_by_hand_is_taken_without_showing_the_whole_window_again() {
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = fx.window();
        open.tick();
        crate::with_live(|live| live.app.config_changed = true).unwrap();
        open.window().invoke_list_scrolled(40.0, 600.0);
        open.window().invoke_page_visible(0.0, 600.0);
        assert!(
            !open.window().get_config_changed(),
            "a scroll showed the window again"
        );
        assert_eq!(
            crate::with_live(|live| live.app.list.viewport).unwrap(),
            (40.0, 600.0)
        );
    }

    #[test]
    fn a_rows_menu_acts_on_that_row() {
        let fx = Fixture::new(&["work"]);
        fx.add("work", message("INBOX", 1, "older"));
        fx.add("work", message("INBOX", 2, "newer"));
        let (open, wires) = fx.window();
        let archive = menu(&open.with(|live| live.app.list.rows[1].clone()), false)[1]
            .iter()
            .position(|(title, _)| *title == "Archive")
            .unwrap();
        open.window().invoke_row_menu(1, 1, archive as i32);
        assert_eq!(wires.sent(), [work("INBOX", &[1], Action::Archive)]);
    }

    #[test]
    fn right_clicking_a_row_opens_its_menu() {
        let fx = Fixture::new(&["work"]);
        fx.add("work", message("INBOX", 1, "hello"));
        let (open, _wires) = fx.window();
        assert!(open.find("Archive").is_empty());
        let row = slint::Model::row_data(&open.window().get_rows(), 0)
            .unwrap()
            .name;
        open.right_tap(&row);
        assert!(!open.find("Archive").is_empty());
    }

    #[test]
    fn a_menu_on_a_marked_row_acts_on_every_marked_row() {
        let fx = Fixture::new(&["work"]);
        for uid in 1..=3 {
            fx.add("work", message("INBOX", uid, "hi"));
        }
        let (open, wires) = fx.window();
        open.press("x j x");
        open.window().invoke_row_menu(0, 1, 2);
        assert_eq!(wires.sent(), [work("INBOX", &[2, 3], Action::Trash)]);
    }

    #[test]
    fn a_menu_on_an_unmarked_row_acts_on_that_row_alone() {
        let fx = Fixture::new(&["work"]);
        for uid in 1..=3 {
            fx.add("work", message("INBOX", uid, "hi"));
        }
        let (open, wires) = fx.window();
        open.press("x");
        open.window().invoke_row_menu(2, 1, 2);
        assert_eq!(wires.sent(), [work("INBOX", &[1], Action::Trash)]);
    }

    #[test]
    fn view_source_on_another_row_shows_that_row() {
        let fx = Fixture::new(&["work"]);
        fx.add("work", message("INBOX", 1, "older"));
        fx.add("work", message("INBOX", 2, "newer"));
        let (open, _wires) = fx.window();
        open.window().invoke_row_menu(1, 2, 0);
        let shown = open.with(|live| {
            let body = live.app.body.as_ref().unwrap();
            (body.key.1, body.source.as_ref().map(|s| s.text.clone()))
        });
        assert_eq!(shown.0, 1);
        assert!(shown.1.unwrap().contains("Subject: older"));
    }

    #[test]
    fn save_eml_on_another_row_saves_that_row() {
        let fx = Fixture::new(&["work"]);
        fx.add("work", message("INBOX", 1, "older"));
        fx.add("work", message("INBOX", 2, "newer"));
        let (open, _wires) = fx.window();
        open.window().invoke_row_menu(1, 2, 1);
        let downloads = fx.paths.cache_dir.join("downloads");
        let saved: Vec<_> = std::fs::read_dir(downloads)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(saved, ["older.eml"]);
        assert!(open.with(|live| live.app.body.as_ref().unwrap().saved.is_some()));
    }

    /// The label of the row of message `uid`.
    fn row_of(open: &crate::test_support::Open, uid: u32) -> String {
        open.labels_containing(&format!("Sender {uid} ")).remove(0)
    }

    #[test]
    fn a_dragged_row_shows_its_subject_or_how_many_marked_rows_move() {
        assert_eq!(drag_label(1, "Lunch on Friday?"), "Lunch on Friday?");
        assert_eq!(drag_label(3, "Lunch on Friday?"), "3 messages");
        let long = "a".repeat(50);
        assert_eq!(drag_label(1, &long), format!("{}…", "a".repeat(39)));
    }

    #[test]
    fn dragging_a_row_onto_a_folder_moves_it() {
        let fx = Fixture::new(&["work"]);
        fx.folder("work", "Travel", None);
        fx.add("work", message("INBOX", 1, "hi"));
        let (open, wires) = fx.window();
        open.drag(&row_of(&open, 1), "Travel");
        assert_eq!(
            wires.sent(),
            [work("INBOX", &[1], Action::Move("Travel".into()))]
        );
    }

    #[test]
    fn dragging_a_marked_row_moves_every_marked_row() {
        let fx = Fixture::new(&["work"]);
        fx.folder("work", "Travel", None);
        for uid in 1..=3 {
            fx.add("work", message("INBOX", uid, "hi"));
        }
        let (open, wires) = fx.window();
        open.press("x j x");
        open.drag(&row_of(&open, 2), "Travel");
        assert_eq!(
            wires.sent(),
            [work("INBOX", &[2, 3], Action::Move("Travel".into()))]
        );
    }

    #[test]
    fn dragging_an_unmarked_row_moves_it_alone() {
        let fx = Fixture::new(&["work"]);
        fx.folder("work", "Travel", None);
        for uid in 1..=3 {
            fx.add("work", message("INBOX", uid, "hi"));
        }
        let (open, wires) = fx.window();
        open.press("x");
        open.drag(&row_of(&open, 1), "Travel");
        assert_eq!(
            wires.sent(),
            [work("INBOX", &[1], Action::Move("Travel".into()))]
        );
    }

    #[test]
    fn a_row_dropped_on_its_own_folder_stays() {
        let fx = Fixture::new(&["work"]);
        fx.folder("work", "Travel", None);
        fx.add("work", message("Travel", 1, "hi"));
        let (open, wires) = fx.window();
        let travel = View::Folder {
            account: 0,
            folder: "Travel".into(),
        };
        let row = open.with(|live| {
            live.app.select_view(travel.clone());
            folders::entries(&live.app)
                .iter()
                .position(
                    |e| matches!(e, folders::Entry::Folder(f) if f.view.as_ref() == Some(&travel)),
                )
                .unwrap()
        });
        // The list's title is "Travel" too, so the drop is invoked rather than dragged.
        open.window().invoke_folder_dropped(row as i32, 0);
        assert!(wires.sent().is_empty());
    }

    #[test]
    fn a_row_dropped_on_another_accounts_folder_is_refused() {
        let fx = Fixture::new(&["home", "work"]);
        fx.folder("work", "Travel", None);
        fx.add("home", message("INBOX", 1, "hi"));
        let (open, wires) = fx.window();
        open.with(|live| live.app.select_view(View::AllInboxes));
        open.drag(&row_of(&open, 1), "Travel");
        assert!(wires.sent().is_empty());
        assert_eq!(
            open.with(|live| live.app.error.clone()).as_deref(),
            Some(crate::state::MOVE_OWN_ACCOUNT)
        );
    }

    #[test]
    fn x_marks_rows_and_escape_clears_them() {
        let fx = Fixture::new(&["work"]);
        for uid in 1..=2 {
            fx.add("work", message("INBOX", uid, "hi"));
        }
        let (open, _wires) = fx.window();
        open.press("x j x");
        assert_eq!(open.with(|live| live.app.list.marked.len()), 2);
        open.press("escape");
        assert!(open.with(|live| live.app.list.marked.is_empty()));
    }

    #[test]
    fn tab_cycles_focus_and_j_in_the_folder_pane_opens_the_next_folder() {
        let fx = Fixture::new(&["work"]);
        fx.folder("work", "Archive", Some("Archive"));
        fx.add("work", message("Archive", 7, "archived"));
        let (open, _wires) = fx.window();
        open.press("tab");
        assert_eq!(open.with(|live| live.app.focus), Focus::Body);
        open.press("tab j");
        let archive = View::Folder {
            account: 0,
            folder: "Archive".into(),
        };
        assert_eq!(open.with(|live| live.app.view.clone()), archive);
        assert_eq!(uids(&open), [7]);
    }

    #[test]
    fn keys_on_an_account_without_a_store_do_nothing() {
        let fx = Fixture::new(&["work"]);
        fx.break_store("work");
        let (open, _wires) = fx.window();
        open.press("j k x right left");
        assert!(uids(&open).is_empty());
        assert!(open.with(|live| live.app.list.marked.is_empty() && live.app.list.cursor == 0));
    }

    #[test]
    fn space_and_enter_never_click_a_row_or_a_folder() {
        let fx = Fixture::new(&["work"]);
        fx.folder("work", "Archive", Some("Archive"));
        for uid in 1..=3 {
            fx.add("work", message("INBOX", uid, "hi"));
        }
        let (open, _wires) = fx.window();
        let view = open.with(|live| live.app.view.clone());
        open.press("tab   enter");
        assert_eq!(open.with(|live| live.app.view.clone()), view);
        assert_eq!(open.with(|live| live.app.list.cursor), 0);
        open.press("down  ");
        assert_eq!(open.with(|live| live.app.list.cursor), 1);
    }

    #[test]
    fn slash_searches_the_account_and_escape_returns_to_the_folder() {
        let fx = Fixture::new(&["work"]);
        fx.folder("work", "Archive", Some("Archive"));
        fx.add("work", message("INBOX", 1, "invoice march"));
        fx.add("work", message("INBOX", 2, "lunch"));
        fx.add("work", message("Archive", 3, "invoice april"));
        let (open, wires) = fx.window();
        open.press("/");
        assert_eq!(open.find("Search this account").len(), 1);
        open.press("invoice");
        let found: Vec<RowKey> =
            open.with(|live| live.app.list.rows.iter().map(Row::key).collect());
        assert_eq!(
            found,
            [("Archive".to_string(), 3), ("INBOX".to_string(), 1)]
        );
        assert_eq!(
            open.labels_containing("Sender 3 — [Archive] invoice april")
                .len(),
            1
        );
        assert_eq!(open.labels_containing("2 results").len(), 1);
        open.press("enter e");
        let search = Command::SearchServer {
            folder: None,
            query: "invoice".into(),
        };
        assert_eq!(
            wires.sent(),
            [
                ("work".to_string(), search),
                work("Archive", &[3], Action::Move("INBOX".into()))
            ]
        );
        open.press("escape");
        assert!(open.with(|live| live.app.search.is_none()));
        assert_eq!(uids(&open), [2, 1]);
        open.press("j");
        assert_eq!(open.with(|live| live.app.list.cursor), 1);
    }

    #[test]
    fn shortcut_letters_typed_into_search_stay_text() {
        let fx = Fixture::new(&["work"]);
        fx.add("work", message("INBOX", 1, "hello"));
        let (open, wires) = fx.window();
        open.press("/ e # u s");
        assert!(wires.sent().is_empty());
        assert_eq!(
            open.with(|live| live.app.search.clone()).as_deref(),
            Some("e#us")
        );
    }

    #[test]
    fn the_move_picker_in_search_offers_the_inbox_for_an_archived_hit() {
        let fx = Fixture::new(&["work"]);
        fx.folder("work", "Archive", Some("Archive"));
        fx.add("work", message("Archive", 3, "invoice april"));
        let (open, wires) = fx.window();
        open.press("/ invoice enter m");
        assert_eq!(open.find("Move to").len(), 1);
        open.press("enter");
        let search = Command::SearchServer {
            folder: None,
            query: "invoice".into(),
        };
        assert_eq!(
            wires.sent(),
            [
                ("work".to_string(), search),
                work("Archive", &[3], Action::Move("INBOX".into()))
            ]
        );
        assert!(open.find("Move to").is_empty());
    }

    #[test]
    fn the_move_picker_moves_to_a_clicked_folder_and_escape_closes_it() {
        let fx = Fixture::new(&["work"]);
        fx.folder("work", "Receipts", None);
        fx.folder("work", "Travel", None);
        fx.add("work", message("INBOX", 1, "hello"));
        let (open, wires) = fx.window();
        open.press("m escape");
        assert!(open.with(|live| live.app.move_picker.is_none()));
        open.press("m");
        let picker = open
            .find("Travel")
            .into_iter()
            .find(|e| e.accessible_role() == Some(i_slint_backend_testing::AccessibleRole::Button));
        picker.unwrap().invoke_accessible_default_action();
        assert_eq!(
            wires.sent(),
            [work("INBOX", &[1], Action::Move("Travel".into()))]
        );
    }

    #[test]
    fn clicking_a_row_selects_it() {
        let fx = Fixture::new(&["work"]);
        fx.add("work", message("INBOX", 1, "older"));
        fx.add("work", message("INBOX", 2, "newer"));
        let (open, _wires) = fx.window();
        let older = open.labels_containing("older").remove(0);
        open.click(&older);
        assert_eq!(open.with(|live| live.app.list.cursor), 1);
    }

    #[test]
    fn shift_click_and_command_click_in_the_window_mark_rows() {
        use slint::platform::{Key, WindowEvent};
        use slint::{ComponentHandle, Model};
        let fx = Fixture::new(&["work"]);
        for uid in [1, 2, 3, 4] {
            fx.add("work", message("INBOX", uid, &format!("hello {uid}")));
        }
        let (open, _wires) = fx.window();
        let label = |i: usize| {
            open.window()
                .get_rows()
                .row_data(i)
                .unwrap()
                .name
                .to_string()
        };
        let held = |key: Key, tap: &str| {
            let window = open.window();
            window
                .window()
                .dispatch_event(WindowEvent::KeyPressed { text: key.into() });
            open.tap(tap);
            window
                .window()
                .dispatch_event(WindowEvent::KeyReleased { text: key.into() });
        };
        held(Key::Shift, &label(2));
        assert_eq!(open.with(|live| live.app.list.marked.len()), 3);
        held(Key::Control, &label(3));
        assert_eq!(open.with(|live| live.app.list.marked.len()), 4);
        assert_eq!(open.with(|live| live.app.list.cursor), 2);
    }

    #[test]
    fn the_filter_switch_narrows_the_list_and_all_shows_every_row_again() {
        let fx = Fixture::new(&["work"]);
        fx.add("work", message("INBOX", 1, "read"));
        fx.add("work", crate::test_support::unread("INBOX", 2, "unread"));
        let (open, _wires) = fx.window();
        let pick = |label: &str| {
            let i = FILTERS.iter().position(|(l, _)| *l == label).unwrap();
            open.window().invoke_list_filter_clicked(i as i32);
        };
        pick("Unread");
        assert_eq!(
            open.with(|live| live.app.list.rows.iter().map(|r| r.uid).collect::<Vec<_>>()),
            [2]
        );
        pick("Flagged");
        assert_eq!(open.find("No flagged mail in this folder.").len(), 1);
        pick("All");
        assert_eq!(open.with(|live| live.app.list.rows.len()), 2);
    }
}
