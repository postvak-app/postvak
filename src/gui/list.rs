//! The middle column: one row per thread, followed by its members when expanded.
use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Datelike, Local, TimeZone};
use eframe::egui;

use crate::message::clean;
use crate::rules::Action;
use crate::store::{Message, MessageSummary, ThreadSummary};

use super::app::{App, UiAction, View};
use super::theme::{self, PAD, Palette, ROW};
use super::{folders, icons, numbers};

/// Threads loaded per folder. ponytail: older mail is reachable through search; page by date if that is not enough.
pub(crate) const THREAD_LIMIT: u32 = 10_000;
/// Results shown for one search.
pub(crate) const SEARCH_LIMIT: u32 = 500;

pub(crate) type RowKey = (String, u32);

pub(crate) fn search_id() -> egui::Id {
    egui::Id::new("search")
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Row {
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
pub(crate) fn build_rows(
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
pub(crate) enum Optimistic {
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
pub(crate) fn apply_pending(rows: &mut Vec<Row>, pending: &[&HashMap<RowKey, Vec<Optimistic>>]) {
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
pub(crate) fn display_name(field: &str) -> String {
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
pub(crate) fn list_date<Tz: TimeZone>(ts: i64, now: &DateTime<Tz>) -> String
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
pub(crate) struct Columns {
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
/// Narrower than this, a column is left out: truncating would still draw its "…", over the date.
const MIN_TEXT: f32 = 16.0;
/// The list pane's narrowest width, so the unread dot and the flag never reach the date.
pub(crate) const MIN_WIDTH: f32 = 220.0;
/// How much further a thread member's sender is indented than its thread row's: a level, as in the folder tree.
const MEMBER_INDENT: f32 = theme::INSET;
/// The account tag's widest, so a long account name leaves room for the sender.
const TAG: f32 = 80.0;
/// A row's height: the sender and date, then the subject.
pub(crate) const LIST_ROW: f32 = 2.0 * ROW;

/// The sender's width in a row of `total` px whose date needs `date` px; the subject below has the full width.
pub(crate) fn sender_width(total: f32, date: f32) -> f32 {
    (total - MARKERS - date - GAP).max(0.0)
}

pub(crate) fn columns<Tz: TimeZone>(
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
pub(crate) fn row_text<Tz: TimeZone>(
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

/// Unread dot, flag or mark, sender and the date right-aligned, then the subject below, both truncated with "…";
/// everything on-accent when the row is selected.
fn draw_row(
    ui: &egui::Ui,
    rect: egui::Rect,
    c: &Columns,
    selected: bool,
    hovered: bool,
    palette: &Palette,
    font: &egui::FontId,
) {
    let painter = ui.painter_at(rect);
    if selected {
        painter.rect_filled(rect, theme::RADIUS, palette.selected);
    } else if hovered {
        painter.rect_filled(rect, theme::RADIUS, palette.hover);
    } else {
        let rule = egui::Stroke::new(1.0, palette.border);
        painter.hline(rect.x_range(), rect.bottom() - 0.5, rule);
    }
    let ink = |color| if selected { palette.on_accent } else { color };
    let first = rect.top() + rect.height() / 3.0;
    let second = rect.top() + 2.0 * rect.height() / 3.0;
    let mut x = rect.left() + PAD;
    if c.unread {
        painter.circle_filled(egui::pos2(x + 3.0, first), 3.0, ink(palette.highlight));
    }
    x += DOT;
    let marker = if c.marked {
        Some(icons::CHECK)
    } else if c.flagged {
        Some(icons::FLAG)
    } else {
        None
    };
    if let Some(marker) = marker {
        painter.text(
            egui::pos2(x + MARKER / 2.0 - 2.0, first),
            egui::Align2::CENTER_CENTER,
            marker,
            font.clone(),
            ink(palette.highlight),
        );
    }
    x += MARKER;
    let date_color = ink(palette.muted);
    let mono = egui::FontId::monospace(font.size - 1.0);
    let date = painter.layout_no_wrap(c.date.clone(), mono, date_color);
    let date_at = egui::pos2(
        rect.right() - PAD - date.size().x,
        first - date.size().y / 2.0,
    );
    let sender_width = sender_width(rect.width() - 2.0 * PAD, date.size().x);
    painter.galley(date_at, date, date_color);
    let (sender_color, subject_color) = if c.unread {
        (palette.text, palette.text)
    } else {
        (palette.secondary, palette.muted)
    };
    let sender_font = if c.unread {
        egui::FontId::new(font.size, theme::named(theme::BOLD))
    } else {
        font.clone()
    };
    let truncated = |text: &str, font: &egui::FontId, width: f32, color| {
        let mut job =
            egui::text::LayoutJob::simple_singleline(text.to_string(), font.clone(), color);
        job.wrap = egui::text::TextWrapping::truncate_at_width(width.max(0.0));
        painter.layout_job(job)
    };
    let indent = if c.member { MEMBER_INDENT } else { 0.0 };
    let x = x + indent;
    let mut sender_x = x;
    let tag_width = TAG.min(sender_width - indent);
    if let Some(account) = &c.account
        && tag_width >= MIN_TEXT
    {
        let small = egui::FontId::new(font.size - 2.0, font.family.clone());
        let tag = truncated(account, &small, tag_width, ink(palette.muted));
        let width = tag.size().x;
        painter.galley(
            egui::pos2(x, first - tag.size().y / 2.0),
            tag,
            ink(palette.muted),
        );
        sender_x += width + PAD;
    }
    let sender_room = sender_width - indent - (sender_x - x);
    if sender_room >= MIN_TEXT {
        let sender = truncated(&c.who, &sender_font, sender_room, ink(sender_color));
        painter.galley(
            egui::pos2(sender_x, first - sender.size().y / 2.0),
            sender,
            ink(sender_color),
        );
    }
    let subject_width = rect.right() - PAD - x;
    if subject_width >= MIN_TEXT {
        let subject = truncated(&c.subject, font, subject_width, ink(subject_color));
        painter.galley(
            egui::pos2(x, second - subject.size().y / 2.0),
            subject,
            ink(subject_color),
        );
    }
}

/// The scroll offset that shows the cursor row, moving the view as little as possible.
pub(crate) fn offset_showing(cursor: usize, row_height: f32, offset: f32, height: f32) -> f32 {
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
pub(crate) const INBOX: &str = "INBOX";

/// Search hits of several accounts, newest first, at most `limit`; each account's own order is kept on equal dates.
pub(crate) fn merge_hits(mut hits: Vec<Row>, limit: u32) -> Vec<Row> {
    hits.sort_by_key(|row| std::cmp::Reverse(row.date));
    hits.truncate(limit as usize);
    hits
}

/// Threads of several accounts in one list, the most recently active first, at most `limit`. The sort is stable, so
/// each account keeps its own order and equal dates keep account order.
pub(crate) fn merge_threads(
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

pub(crate) fn show(app: &App, ui: &mut egui::Ui) -> Vec<UiAction> {
    let mut actions = Vec::new();
    let list = &app.list;
    let recipient = app.shows_recipient();
    let now = Local::now();
    if app.search.is_none() {
        match &app.view {
            View::AllInboxes => title(ui, "All inboxes", &list.rows),
            View::Folder { folder, .. } => title(ui, folders::label(folder), &list.rows),
            _ => {}
        }
    }
    if let Some(query) = &app.search {
        let mut text = query.clone();
        let response = theme::text_field(
            ui,
            egui::TextEdit::singleline(&mut text)
                .id(search_id())
                .hint_text(if app.view == View::AllInboxes {
                    "Search every account"
                } else {
                    "Search this account"
                })
                .desired_width(f32::INFINITY),
        );
        if app.focus_search {
            response.request_focus();
            actions.push(UiAction::SearchFocused);
        }
        if response.changed() {
            actions.push(UiAction::SearchFor(text));
        }
        if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            actions.push(UiAction::SelectRow(0));
        }
        ui.weak(format!(
            "{} results · mail whose body is not downloaded matches on sender, recipients and subject only",
            numbers::count(list.rows.len() as u64)
        ));
    }
    let mut area = egui::ScrollArea::vertical().auto_shrink(false);
    if list.follow_cursor {
        let row_height = LIST_ROW + ui.spacing().item_spacing.y;
        let (offset, height) = list.viewport;
        area = area.vertical_scroll_offset(offset_showing(list.cursor, row_height, offset, height));
    }
    let palette = theme::palette(ui);
    let font = egui::TextStyle::Button.resolve(ui.style());
    let output = area.show_rows(ui, LIST_ROW, list.rows.len(), |ui, range| {
        for index in range {
            let row = &list.rows[index];
            let marked = list.marked.contains(&row.id());
            let selected = index == list.cursor || marked;
            let (rect, response) = ui.allocate_exact_size(
                egui::vec2(ui.available_width(), LIST_ROW),
                egui::Sense::click(),
            );
            let account =
                (app.view == View::AllInboxes).then(|| app.accounts[row.account].name.as_str());
            let c = columns(row, account, marked, recipient, app.search.is_some(), &now);
            draw_row(ui, rect, &c, selected, response.hovered(), palette, &font);
            let mut name = row_text(row, account, recipient, app.search.is_some(), &now);
            if marked {
                name = format!("✔ {name}");
            }
            response.widget_info(|| {
                egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, &name)
            });
            // A right click acts on the row under the pointer, unless it is one of the marked rows.
            if response.clicked() || (response.secondary_clicked() && !marked) {
                actions.push(UiAction::SelectRow(index));
            }
            response.context_menu(|ui| actions.extend(mail_menu(ui, row)));
        }
    });
    actions.push(UiAction::ListViewport {
        offset: output.state.offset.y,
        height: output.inner_rect.height(),
    });
    actions
}

/// The right-click menu of a message row; its items act like the toolbar, on the marked rows or this one.
fn mail_menu(ui: &mut egui::Ui, row: &Row) -> Vec<UiAction> {
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
    let groups = [
        vec![
            (read_label, Some("u"), UiAction::Act(read)),
            (flag_label, Some("s"), UiAction::Act(flag)),
        ],
        vec![
            ("Archive", Some("e"), UiAction::Act(Action::Archive)),
            ("Move to…", Some("m"), UiAction::OpenMovePicker),
            ("Delete", Some("#"), UiAction::Act(Action::Trash)),
        ],
        vec![
            ("View source", None, UiAction::OpenSource),
            ("Save .eml", None, UiAction::SaveEml),
        ],
    ];
    let mut actions = Vec::new();
    for (index, group) in groups.into_iter().enumerate() {
        if index > 0 {
            ui.separator();
        }
        for (label, shortcut, action) in group {
            let button = egui::Button::new(label);
            let button = match shortcut {
                Some(key) => button.shortcut_text(key),
                None => button,
            };
            if ui.add(button).clicked() {
                actions.push(action);
            }
        }
    }
    actions
}

/// The folder's name as a heading, then how many threads it shows and how many are unread.
fn title(ui: &mut egui::Ui, name: &str, rows: &[Row]) {
    let threads: Vec<&Row> = rows.iter().filter(|row| !row.member).collect();
    let unread = threads.iter().filter(|row| row.unread).count();
    let noun = if threads.len() == 1 {
        "thread"
    } else {
        "threads"
    };
    theme::heading_row(ui, |ui| {
        // In line with the rows' content, which sits a padding inside each row.
        ui.add_space(PAD);
        ui.heading(clean(name, false));
        ui.weak(format!(
            "{} {noun}, {} unread",
            numbers::count(threads.len() as u64),
            numbers::count(unread as u64)
        ));
    });
}

/// The `m` popup: type to filter the folders of the selected rows' account, Enter or a click moves. It leaves out the
/// folder the cursor message is in.
pub(crate) fn show_move_picker(app: &App, ctx: &egui::Context) -> Vec<UiAction> {
    let mut actions = Vec::new();
    let (Some(filter), Some(row)) = (&app.move_picker, app.selected_rows().first().copied()) else {
        return actions;
    };
    let folder = app
        .list
        .rows
        .get(app.list.cursor)
        .map_or(&row.folder, |cursor| &cursor.folder);
    let needle = filter.to_lowercase();
    let names: Vec<&str> = app.accounts[row.account]
        .folders
        .iter()
        .map(|f| f.name.as_str())
        .filter(|name| name != folder && name.to_lowercase().contains(&needle))
        .collect();
    egui::Window::new("Move to")
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            let mut text = filter.clone();
            let response = theme::text_field(
                ui,
                egui::TextEdit::singleline(&mut text).hint_text("Folder"),
            );
            response.request_focus();
            if response.changed() {
                actions.push(UiAction::MoveFilter(text));
            }
            if ui.input(|i| i.key_pressed(egui::Key::Enter))
                && let Some(first) = names.first()
            {
                actions.push(UiAction::MoveTo(first.to_string()));
            }
            for name in &names {
                if ui.selectable_label(false, clean(name, false)).clicked() {
                    actions.push(UiAction::MoveTo(name.to_string()));
                }
            }
        });
    actions
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use eframe::egui;
    use egui_kittest::kittest::Queryable;

    use super::*;
    use crate::gui::app::{Focus, View};
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

    fn has_row(harness: &egui_kittest::Harness<'_, crate::gui::App>, uid: u32) -> bool {
        harness.state().list.rows.iter().any(|row| row.uid == uid)
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
    fn rows_name_their_account_in_all_inboxes_only() {
        let fx = Fixture::new(&["home", "work"]);
        fx.add("home", message("INBOX", 1, "from home"));
        let (mut harness, _wires) = fx.harness();
        assert!(
            harness
                .query_by_label_contains("Sender 1 — from home")
                .is_some()
        );
        assert!(harness.query_by_label_contains("home · Sender 1").is_none());
        harness.state_mut().select_view(View::AllInboxes);
        harness.run();
        assert!(
            harness
                .query_by_label_contains("home · Sender 1 — from home")
                .is_some()
        );
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
    fn j_and_k_move_through_rows_newest_first() {
        let fx = Fixture::new(&["work"]);
        for uid in 1..=3 {
            fx.add("work", message("INBOX", uid, &format!("subject {uid}")));
        }
        let (mut harness, _wires) = fx.harness();
        assert!(
            harness
                .query_by_label_contains("Sender 3 — subject 3")
                .is_some()
        );
        let uids: Vec<u32> = harness.state().list.rows.iter().map(|r| r.uid).collect();
        assert_eq!(uids, [3, 2, 1]);
        harness.event(egui::Event::Text("j".into()));
        harness.run();
        harness.key_press(egui::Key::ArrowDown);
        harness.run();
        assert_eq!(harness.state().list.cursor, 2);
        harness.event(egui::Event::Text("j".into()));
        harness.run();
        assert_eq!(
            harness.state().list.cursor,
            2,
            "the cursor stops at the last row"
        );
        harness.event(egui::Event::Text("k".into()));
        harness.run();
        assert_eq!(harness.state().list.cursor, 1);
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
        let (mut harness, _wires) = fx.harness();
        assert_eq!(harness.state().list.rows.len(), 2);
        assert!(harness.query_by_label_contains("thread (2)").is_some());
        harness.key_press(egui::Key::ArrowRight);
        harness.run();
        assert_eq!(harness.state().list.rows.len(), 4);
        harness.key_press(egui::Key::ArrowDown);
        harness.run();
        assert!(harness.state().list.rows[1].member);
        harness.key_press(egui::Key::ArrowLeft);
        harness.run();
        assert_eq!(
            (harness.state().list.rows.len(), harness.state().list.cursor),
            (2, 0)
        );
        assert!(has_row(&harness, 1));
    }

    #[test]
    fn a_marked_row_says_so_in_its_name() {
        let fx = Fixture::new(&["work"]);
        for uid in 1..=2 {
            fx.add("work", message("INBOX", uid, "hi"));
        }
        let (mut harness, _wires) = fx.harness();
        harness.event(egui::Event::Text("x".into()));
        harness.run();
        assert_eq!(
            harness
                .get_all_by(|node| node.label().is_some_and(|l| l.starts_with("✔ ")))
                .count(),
            1
        );
    }

    #[test]
    fn in_a_narrow_row_nothing_is_drawn_over_the_date_or_past_the_edge() {
        for width in [150.0, 130.0] {
            let mut harness = egui_kittest::Harness::builder()
                .with_size(egui::vec2(width + 16.0, LIST_ROW + 16.0))
                .build_ui(move |ui| {
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(width, LIST_ROW), egui::Sense::hover());
                    let c = Columns {
                        account: Some("work".into()),
                        unread: true,
                        flagged: true,
                        marked: false,
                        who: "A very long sender name indeed".into(),
                        subject: "A subject long enough to need truncating twice over".into(),
                        date: "10 Dec 2025".into(),
                        member: true,
                    };
                    let font = egui::TextStyle::Button.resolve(ui.style());
                    draw_row(ui, rect, &c, false, false, &crate::gui::theme::NIGHT, &font);
                });
            harness.run();
            let texts: Vec<(String, egui::Rect)> = harness
                .output()
                .shapes
                .iter()
                .filter_map(|clipped| match &clipped.shape {
                    egui::Shape::Text(text) => Some((
                        text.galley.text().to_string(),
                        text.galley.rect.translate(text.pos.to_vec2()),
                    )),
                    _ => None,
                })
                .collect();
            let date = texts.iter().find(|(t, _)| t == "10 Dec 2025").unwrap().1;
            let subject = texts
                .iter()
                .find(|(t, _)| t.starts_with("A subject"))
                .unwrap()
                .1;
            assert!(
                subject.top() >= date.bottom(),
                "{width}: subject {subject:?} not below the date"
            );
            for (text, rect) in texts.iter().filter(|(t, _)| t != "10 Dec 2025") {
                let beside_the_date = rect.y_range().intersects(date.y_range());
                assert!(
                    !beside_the_date || rect.right() <= date.left(),
                    "{width}: {text:?} {rect:?} vs date {date:?}"
                );
                assert!(
                    rect.right() <= width + 8.0,
                    "{width}: {text:?} {rect:?} past the row"
                );
            }
        }
    }

    #[test]
    fn unread_senders_are_semibold_dates_monospace_and_rows_ruled() {
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(416.0, LIST_ROW + 16.0))
            .build_ui(|ui| {
                // Fonts load a pass after they are set, and the bold family does not exist before.
                if ui.ctx().cumulative_pass_nr() == 0 {
                    ui.ctx().set_fonts(theme::fonts());
                    return;
                }
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(400.0, LIST_ROW), egui::Sense::hover());
                let c = Columns {
                    account: None,
                    unread: true,
                    flagged: false,
                    marked: false,
                    who: "Ada".into(),
                    subject: "Notes".into(),
                    date: "21 Sep".into(),
                    member: false,
                };
                let font = egui::TextStyle::Button.resolve(ui.style());
                draw_row(ui, rect, &c, false, false, &crate::gui::theme::PAPER, &font);
            });
        harness.run();
        let shapes = &harness.output().shapes;
        let family = |wanted: &str| {
            shapes
                .iter()
                .find_map(|clipped| match &clipped.shape {
                    egui::Shape::Text(text) if text.galley.text() == wanted => text
                        .galley
                        .job
                        .sections
                        .first()
                        .map(|s| s.format.font_id.family.clone()),
                    _ => None,
                })
                .unwrap()
        };
        assert_eq!(family("Ada"), theme::named(theme::BOLD));
        assert_eq!(family("Notes"), egui::FontFamily::Proportional);
        assert_eq!(family("21 Sep"), egui::FontFamily::Monospace);
        let ruled = shapes.iter().any(|clipped| {
            matches!(&clipped.shape, egui::Shape::LineSegment { stroke, .. }
                if stroke.color == theme::PAPER.border)
        });
        assert!(ruled, "no rule under the row");
    }

    #[test]
    fn the_list_is_titled_with_the_folder_and_its_counts() {
        let fx = Fixture::new(&["work"]);
        fx.add("work", message("INBOX", 1, "read"));
        let mut unread = message("INBOX", 2, "new");
        unread.flags = String::new();
        fx.add("work", unread);
        let (harness, _wires) = fx.harness();
        assert!(harness.query_by_label("Inbox").is_some());
        assert!(harness.query_by_label("2 threads, 1 unread").is_some());
    }

    #[test]
    fn right_clicking_a_row_selects_it_and_its_menu_archives_it() {
        let fx = Fixture::new(&["work"]);
        fx.add("work", message("INBOX", 1, "older"));
        fx.add("work", message("INBOX", 2, "newer"));
        let (mut harness, wires) = fx.harness();
        harness.get_by_label_contains("older").click_secondary();
        harness.run();
        assert_eq!(harness.state().list.cursor, 1);
        harness.get_by_label("Archive e").click();
        harness.run();
        let archive = crate::sync::Command::Apply {
            folder: "INBOX".into(),
            uids: vec![1],
            action: Action::Archive,
            by: "gui".into(),
        };
        assert_eq!(wires.sent(), [("work".to_string(), archive)]);
        assert!(harness.query_by_label("Archive e").is_none());
    }

    #[test]
    fn x_marks_rows_and_escape_clears_them() {
        let fx = Fixture::new(&["work"]);
        for uid in 1..=2 {
            fx.add("work", message("INBOX", uid, "hi"));
        }
        let (mut harness, _wires) = fx.harness();
        harness.event(egui::Event::Text("x".into()));
        harness.event(egui::Event::Text("j".into()));
        harness.event(egui::Event::Text("x".into()));
        harness.run();
        assert_eq!(harness.state().list.marked.len(), 2);
        harness.key_press(egui::Key::Escape);
        harness.run();
        assert!(harness.state().list.marked.is_empty());
    }

    #[test]
    fn tab_cycles_focus_and_j_in_the_folder_pane_opens_the_next_folder() {
        let fx = Fixture::new(&["work"]);
        fx.folder("work", "Archive", Some("Archive"));
        fx.add("work", message("Archive", 7, "archived"));
        let (mut harness, _wires) = fx.harness();
        harness.key_press(egui::Key::Tab);
        harness.run();
        assert_eq!(harness.state().focus, Focus::Body);
        harness.key_press(egui::Key::Tab);
        harness.run();
        assert_eq!(harness.state().focus, Focus::Folders);
        harness.event(egui::Event::Text("j".into()));
        harness.run();
        assert_eq!(
            harness.state().view,
            View::Folder {
                account: 0,
                folder: "Archive".into()
            }
        );
        assert!(has_row(&harness, 7));
    }

    #[test]
    fn keys_on_an_account_without_a_store_do_nothing() {
        let fx = Fixture::new(&["work"]);
        let db = fx.paths.mail_db("work");
        std::fs::remove_file(&db).unwrap();
        std::fs::create_dir_all(&db).unwrap();
        let (mut harness, _wires) = fx.harness();
        for key in ["j", "k", "x"] {
            harness.event(egui::Event::Text(key.into()));
        }
        harness.key_press(egui::Key::ArrowRight);
        harness.key_press(egui::Key::ArrowLeft);
        harness.run();
        assert!(harness.state().list.rows.is_empty());
        assert!(harness.state().list.marked.is_empty());
        assert_eq!(harness.state().list.cursor, 0);
    }

    #[test]
    fn tab_and_arrows_leave_no_egui_widget_focus_for_space_or_enter_to_click() {
        let fx = Fixture::new(&["work"]);
        fx.folder("work", "Archive", Some("Archive"));
        for uid in 1..=3 {
            fx.add("work", message("INBOX", uid, "hi"));
        }
        let (mut harness, _wires) = fx.harness();
        let view = harness.state().view.clone();
        harness.key_press(egui::Key::Tab);
        harness.run();
        harness.key_press(egui::Key::Space);
        harness.run();
        harness.key_press(egui::Key::Enter);
        harness.run();
        assert_eq!(harness.state().view, view);
        assert_eq!(harness.state().list.cursor, 0);
        assert_eq!(harness.ctx.memory(|m| m.focused()), None);
        harness.key_press(egui::Key::ArrowDown);
        harness.run();
        harness.key_press(egui::Key::Space);
        harness.run();
        assert_eq!(harness.state().list.cursor, 1);
        assert_eq!(harness.ctx.memory(|m| m.focused()), None);
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

    #[test]
    fn slash_searches_the_account_and_escape_returns_to_the_folder() {
        let fx = Fixture::new(&["work"]);
        fx.folder("work", "Archive", Some("Archive"));
        fx.add("work", message("INBOX", 1, "invoice march"));
        fx.add("work", message("INBOX", 2, "lunch"));
        fx.add("work", message("Archive", 3, "invoice april"));
        let (mut harness, wires) = fx.harness();
        harness.event(egui::Event::Text("/".into()));
        harness.run();
        harness.event(egui::Event::Text("invoice".into()));
        harness.run();
        let found: Vec<(String, u32)> = harness.state().list.rows.iter().map(Row::key).collect();
        assert_eq!(
            found,
            [("Archive".to_string(), 3), ("INBOX".to_string(), 1)]
        );
        assert!(
            harness
                .query_by_label_contains("[Archive] Sender 3")
                .is_some()
        );
        assert!(harness.query_by_label_contains("2 results").is_some());
        harness.key_press(egui::Key::Enter);
        harness.run();
        harness.event(egui::Event::Text("e".into()));
        harness.run();
        let archive = crate::sync::Command::Apply {
            folder: "Archive".into(),
            uids: vec![3],
            action: crate::rules::Action::Archive,
            by: "gui".into(),
        };
        assert_eq!(wires.sent(), [("work".to_string(), archive)]);
        harness.key_press(egui::Key::Escape);
        harness.run();
        assert!(harness.state().search.is_none());
        let uids: Vec<u32> = harness.state().list.rows.iter().map(|r| r.uid).collect();
        assert_eq!(uids, [2, 1]);
        assert_eq!(harness.ctx.memory(|m| m.focused()), None);
        harness.event(egui::Event::Text("j".into()));
        harness.run();
        assert_eq!(harness.state().list.cursor, 1);
    }

    #[test]
    fn shortcut_letters_typed_into_search_stay_text() {
        let fx = Fixture::new(&["work"]);
        fx.add("work", message("INBOX", 1, "hello"));
        let (mut harness, wires) = fx.harness();
        harness.event(egui::Event::Text("/".into()));
        harness.run();
        harness.event(egui::Event::Text("e#us".into()));
        harness.run();
        assert!(wires.sent().is_empty());
        assert_eq!(harness.state().search.as_deref(), Some("e#us"));
    }

    #[test]
    fn the_move_picker_in_search_offers_the_inbox_for_an_archived_hit() {
        let fx = Fixture::new(&["work"]);
        fx.folder("work", "Archive", Some("Archive"));
        fx.add("work", message("Archive", 3, "invoice april"));
        let (mut harness, wires) = fx.harness();
        harness.event(egui::Event::Text("/".into()));
        harness.run();
        harness.event(egui::Event::Text("invoice".into()));
        harness.run();
        harness.key_press(egui::Key::Enter);
        harness.run();
        harness.event(egui::Event::Text("m".into()));
        harness.run();
        harness.key_press(egui::Key::Enter);
        harness.run();
        let back = crate::sync::Command::Apply {
            folder: "Archive".into(),
            uids: vec![3],
            action: crate::rules::Action::Move("INBOX".into()),
            by: "gui".into(),
        };
        assert_eq!(wires.sent(), [("work".to_string(), back)]);
    }
}
