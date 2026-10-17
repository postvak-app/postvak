//! Each main view drawn by Slint's software renderer and compared with `tests/snapshots/gui_*.png`, and the grid the
//! views line up on, from element positions.
//!
//! The software renderer and the bundled fonts draw the same pixels on every machine, so these run in every
//! `cargo test`. The views show local times: with a local offset other than zero the pixels are not compared (run
//! with `TZ=UTC`, as CI does). `UPDATE_SNAPSHOTS=1` rewrites the PNGs; a mismatch writes `{name}.new.png` beside the
//! reference.
use std::path::PathBuf;

use super::fixture::{AT, mail};
use super::state::{Focus, View};
use super::test_support::{Fixture, Open, Wires, message};
use crate::store::LogEntry;
use crate::sync::{Activity, Event};
use crate::trash::Trash;

const RULES: &str = "[[rules]]\nname = \"newsletters\"\nmatch.from = { contains = \"news@\" }\nactions = [\"archive\"]\n\n[[rules]]\nname = \"receipts\"\naccount = \"work\"\nmatch.subject = { contains = \"receipt\" }\nactions = [{ move = \"Receipts\" }]\n\n[[rules]]\nname = \"codes\"\nenabled = false\nproposed_by = \"agent\"\nmatch.subject = { contains = \"verification code\" }\nactions = [\"delete\"]\n";

/// Whether local times read as UTC, now and at the fixture time; a zone such as Europe/London is UTC in winter but
/// not in September.
fn utc() -> bool {
    let fixture = chrono::DateTime::from_timestamp(AT, 0)
        .unwrap()
        .with_timezone(&chrono::Local);
    chrono::Local::now().offset().local_minus_utc() == 0 && fixture.offset().local_minus_utc() == 0
}

/// Compares the window with `tests/snapshots/{name}.png`.
fn snapshot(open: &Open, name: &str) {
    open.tick();
    let png = open.png();
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/snapshots")
        .join(format!("{name}.png"));
    if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
        assert!(
            utc(),
            "snapshots show local times; rewrite them with TZ=UTC"
        );
        std::fs::write(&path, &png).unwrap();
        return;
    }
    if !utc() {
        eprintln!("{name}: not compared, as the local offset is not zero; run with TZ=UTC");
        return;
    }
    let reference = std::fs::read(&path).unwrap_or_default();
    if pixels(&reference) != pixels(&png) {
        let new = path.with_extension("new.png");
        std::fs::write(&new, &png).unwrap();
        panic!(
            "{name} differs from {}; see {}",
            path.display(),
            new.display()
        );
    }
}

/// The decoded pixels, so a different PNG encoder does not count as a change.
fn pixels(png: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let mut reader = png::Decoder::new(std::io::Cursor::new(png))
        .read_info()
        .ok()?;
    let mut buffer = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buffer).ok()?;
    buffer.truncate(info.buffer_size());
    Some((info.width, info.height, buffer))
}

/// The window over `fx` with one account up to date and the other offline, so the status bar holds still.
fn open(fx: &Fixture) -> (Open, Wires) {
    let (open, wires) = fx.window();
    let events = [
        Event::Activity {
            account: "home".into(),
            activity: Activity::Offline {
                reason: "no network".into(),
                retry_at: AT + 300,
            },
        },
        Event::Activity {
            account: "work".into(),
            activity: Activity::Idle { since: AT },
        },
    ];
    for event in events {
        wires.events.send(event).unwrap();
    }
    open.tick();
    (open, wires)
}

fn work_inbox() -> View {
    View::Folder {
        account: 1,
        folder: "INBOX".into(),
    }
}

/// INBOX of `work` with the newest message open in the body pane.
fn inbox(theme: &str) -> (Fixture, Open, Wires) {
    let fx = Fixture::mailbox(theme);
    let (open, wires) = open(&fx);
    open.with(|live| live.app.select_view(work_inbox()));
    open.tick();
    assert!(open.window().get_body_text().contains("Shall we try"));
    (fx, open, wires)
}

#[test]
fn inbox_light() {
    let (_fx, open, _wires) = inbox("light");
    snapshot(&open, "gui_inbox_light");
}

#[test]
fn inbox_dark() {
    let (_fx, open, _wires) = inbox("dark");
    snapshot(&open, "gui_inbox_dark");
}

#[test]
fn all_inboxes() {
    let fx = Fixture::mailbox("dark");
    let (open, _wires) = open(&fx);
    open.with(|live| live.app.select_view(View::AllInboxes));
    open.tick();
    assert_eq!(open.labels_containing("home · Mum").len(), 1);
    assert_eq!(open.labels_containing("work · Linus Example").len(), 1);
    snapshot(&open, "gui_all_inboxes");
}

#[test]
fn an_empty_folder() {
    let fx = Fixture::mailbox("light");
    let (open, _wires) = open(&fx);
    open.with(|live| {
        live.app.select_view(View::Folder {
            account: 1,
            folder: "Sent".into(),
        })
    });
    open.tick();
    assert_eq!(open.find("No mail in this folder.").len(), 1);
    snapshot(&open, "gui_empty_folder");
}

#[test]
fn inbox_with_marks_and_an_expanded_thread() {
    let (_fx, open, _wires) = inbox("light");
    open.press("x j x right");
    let (marked, members) = open.with(|live| {
        let list = &live.app.list;
        let marked = [95, 94].map(|uid| list.marked.contains(&(1, ("INBOX".to_string(), uid))));
        let members = [94, 93].map(|uid| list.rows.iter().any(|row| row.member && row.uid == uid));
        (marked, members)
    });
    assert_eq!((marked, members), ([true; 2], [true; 2]));
    snapshot(&open, "gui_inbox_thread");
}

#[test]
fn dragging_a_row_over_a_folder() {
    let (_fx, open, _wires) = inbox("light");
    let row = open.labels_containing("Linus Example").remove(0);
    open.drag_over(&row, "Receipts");
    snapshot(&open, "gui_drag");
}

#[test]
fn keys_help() {
    let (_fx, open, _wires) = inbox("light");
    open.press("?");
    assert!(open.with(|live| live.app.help_open));
    snapshot(&open, "gui_keys_help");
}

#[test]
fn rules() {
    let fx = Fixture::mailbox("light");
    std::fs::write(fx.paths.rules_file(), RULES).unwrap();
    let (open, _wires) = open(&fx);
    open.with(|live| live.app.select_view(View::Rules));
    open.tick();
    assert_eq!(open.find("Approve").len(), 1);
    snapshot(&open, "gui_rules");
}

#[test]
fn activity() {
    let fx = Fixture::mailbox("light");
    let entries = [
        (
            0,
            "newsletters",
            "archive",
            "This week: ten things about tea",
        ),
        (
            5,
            "receipts",
            "move Receipts",
            "Your receipt for order 1042",
        ),
        (9, "gui", "flag", "Re: Planning the offsite"),
    ];
    for (minutes, rule, action, subject) in entries {
        let entry = LogEntry {
            id: 0,
            at: AT + minutes * 60,
            rule_name: rule.into(),
            folder: "INBOX".into(),
            uid: 1,
            message_id: None,
            subject: Some(subject.into()),
            action: action.into(),
            trash_file: None,
        };
        fx.store("work").log_action(&entry).unwrap();
    }
    let (open, _wires) = open(&fx);
    open.with(|live| live.app.select_view(View::Activity));
    open.tick();
    assert_eq!(open.find("move Receipts").len(), 1);
    snapshot(&open, "gui_activity");
}

#[test]
fn trash() {
    let fx = Fixture::mailbox("light");
    let dir = fx.paths.trash_dir("work");
    std::fs::create_dir_all(&dir).unwrap();
    let raw = b"From: Example Shop <shop@example.com>\r\nSubject: Your verification code\r\n\r\n123456\r\n";
    Trash::new(dir).save("INBOX", 7, raw, AT).unwrap();
    let (open, _wires) = open(&fx);
    open.with(|live| live.app.select_view(View::Trash));
    open.tick();
    assert_eq!(open.find("Restore").len(), 1);
    snapshot(&open, "gui_trash");
}

/// INBOX of `work` with the newsletter open as HTML, in a window `width` points wide, once it is painted.
fn newsletter(theme: &str, width: f32) -> (Fixture, Open, Wires) {
    use std::time::{Duration, Instant};

    use slint::ComponentHandle;

    use super::fixture::mail;
    let fx = Fixture::mailbox(theme);
    let mut news = mail(
        99,
        "Postvak Weekly <news@example.org>",
        "Your October roundup",
        "\\Seen",
        "Your October roundup",
    );
    news.headers = b"Subject: Your October roundup\r\nList-Unsubscribe: <https://news.example.org/leave>\r\n\r\n".to_vec();
    fx.add("work", news);
    let raw = super::test_support::raw(include_str!("../../tests/fixtures/newsletter.html"));
    fx.store("work")
        .set_raw("INBOX", 99, &raw, "Your October roundup")
        .unwrap();
    let (open, wires) = open(&fx);
    open.window()
        .window()
        .set_size(slint::LogicalSize::new(width, 800.0));
    open.with(|live| live.app.select_view(work_inbox()));
    let deadline = Instant::now() + Duration::from_secs(30);
    let painted = |open: &Open| {
        open.with(|live| {
            let view = live.app.body.as_ref().and_then(|b| b.html_view.as_ref());
            view.is_some_and(|v| {
                v.page.is_some()
                    && !v.requested.is_empty()
                    && v.requested.iter().all(|s| v.strips.contains_key(s))
            })
        })
    };
    while !painted(&open) {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for the renderer"
        );
        std::thread::sleep(Duration::from_millis(5));
        open.tick();
    }
    (fx, open, wires)
}

#[test]
fn html_light() {
    let (_fx, open, _wires) = newsletter("light", 1280.0);
    snapshot(&open, "gui_html_light");
}

#[test]
fn html_dark() {
    let (_fx, open, _wires) = newsletter("dark", 1280.0);
    snapshot(&open, "gui_html_dark");
}

/// A body pane narrower than the newsletter's 600 px table: its media query stacks the columns.
#[test]
fn html_narrow() {
    let (_fx, open, _wires) = newsletter("light", 1060.0);
    snapshot(&open, "gui_html_narrow");
}

/// The window's grid: (glyph, inset, pad, row).
fn grid(open: &Open) -> (f32, f32, f32, f32) {
    let window = open.window();
    let theme = slint::ComponentHandle::global::<super::ui::Theme>(&window);
    (
        theme.get_glyph(),
        theme.get_inset(),
        theme.get_pad(),
        theme.get_row(),
    )
}

/// The left edge and vertical centre of the one element labelled `label`, or of the one containing it.
fn edge(open: &Open, label: &str) -> (f32, f32) {
    let label = open
        .labels_containing(label)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("no {label:?}"));
    let element = open.get(&label);
    let at = element.absolute_position();
    (at.x, at.y + element.size().height / 2.0)
}

#[test]
fn the_inbox_lines_up_on_the_grid() {
    let (_fx, open, _wires) = inbox("light");
    let (glyph, _, pad, row_height) = grid(&open);
    let archive = edge(&open, "Archive message");
    let title = open
        .find("Inbox")
        .into_iter()
        .map(|e| e.absolute_position().x)
        .fold(0.0, f32::max);
    let row = edge(&open, "Your receipt for order 1042");
    assert_eq!(
        archive.0 + glyph,
        title,
        "the toolbar glyph lines up with the list title"
    );
    assert_eq!(
        row.0 + pad,
        title,
        "the list title lines up with the rows' content"
    );
    assert_eq!(
        archive.1,
        edge(&open, "Search").1,
        "the toolbar's buttons share a line"
    );
    let (rules, activity) = (open.get("Rules"), open.get("Activity"));
    assert_eq!(
        edge(&open, "Sync now").0 + glyph,
        rules.absolute_position().x + pad,
        "status glyph"
    );
    assert_eq!(rules.size().height, row_height, "folder row");
    let pitch = activity.absolute_position().y - rules.absolute_position().y;
    assert_eq!(pitch, row_height + pad / 2.0, "folder row pitch");
    open.with(|live| live.app.focus = Focus::Body);
    assert_eq!(
        edge(&open, "Your receipt for order 1042"),
        row,
        "the focus outline moves no content"
    );
}

#[test]
fn the_list_title_the_readers_actions_and_all_inboxes_share_a_line() {
    let (_fx, open, _wires) = inbox("light");
    let title = open
        .find("Inbox")
        .into_iter()
        .map(|e| e.absolute_position().y + e.size().height / 2.0)
        .fold(f32::MAX, f32::min);
    assert_eq!(edge(&open, "View source").1, title, "the reader's actions");
    assert_eq!(edge(&open, "All inboxes (3)").1, title, "All inboxes");
}

#[test]
fn the_keys_window_content_sits_inset() {
    let (_fx, open, _wires) = inbox("light");
    let (_, inset, _, _) = grid(&open);
    open.press("?");
    let window = open.get("Keys").absolute_position().x;
    assert_eq!(edge(&open, "j / k, Down / Up").0, window + inset);
}

#[test]
fn with_one_account_the_first_folder_and_first_message_share_a_line() {
    let fx = Fixture::new(&["work"]);
    fx.add("work", message("INBOX", 1, "hello"));
    let (open, _wires) = fx.window();
    let folder = open
        .find("Inbox")
        .into_iter()
        .map(|e| e.absolute_position())
        .fold(f32::MAX, |x, p| x.min(p.x));
    let folder = open
        .find("Inbox")
        .into_iter()
        .find(|e| e.absolute_position().x == folder)
        .unwrap();
    let row = open.get(&open.labels_containing("hello").remove(0));
    assert_eq!(folder.absolute_position().y, row.absolute_position().y);
}

/// The colour drawn at `(x, y)` in the window.
fn pixel(open: &Open, x: f32, y: f32) -> slint::Color {
    let shot = slint::ComponentHandle::window(&open.window())
        .take_snapshot()
        .unwrap();
    let pixel = shot.as_slice()[y as usize * shot.width() as usize + x as usize];
    slint::Color::from_argb_u8(pixel.a, pixel.r, pixel.g, pixel.b)
}

/// The colour drawn just inside the left edge of the row labelled `label`.
fn row_fill(open: &Open, label: &str) -> slint::Color {
    let row = open.get(label);
    let at = row.absolute_position();
    pixel(open, at.x + 2.0, at.y + row.size().height / 2.0)
}

/// The colour at the centre of the unread dot of the row labelled `label`, whether or not the dot shows.
fn unread_dot(open: &Open, label: &str) -> slint::Color {
    let (_, _, pad, _) = grid(open);
    let at = open.get(label).absolute_position();
    let list_row =
        slint::ComponentHandle::global::<super::ui::Theme>(&open.window()).get_list_row();
    pixel(
        open,
        at.x + pad + 3.0,
        at.y + pad + (list_row - 2.0 * pad) / 4.0,
    )
}

/// The full label of the one row containing `text`.
fn row(open: &Open, text: &str) -> String {
    open.labels_containing(text).remove(0)
}

#[test]
fn hovering_a_row_fades_in_and_selecting_it_shows_at_once() {
    let (_fx, open, _wires) = inbox("light");
    let pane = slint::ComponentHandle::global::<super::ui::Theme>(&open.window()).get_pane();
    let row = &open
        .labels_containing("Your receipt for order 1042")
        .remove(0);
    let resting = row_fill(&open, row);
    assert_ne!(resting, pane.hover);

    open.hover(row);
    assert_eq!(
        row_fill(&open, row),
        resting,
        "the fade starts from nothing"
    );
    open.advance(1.0);
    assert_eq!(
        row_fill(&open, row),
        pane.hover,
        "and ends on the hover colour"
    );

    open.tap(row);
    assert_eq!(
        row_fill(&open, row),
        pane.selected,
        "selection does not wait for a fade"
    );
}

#[test]
fn marking_a_message_read_fades_its_dot_out() {
    let (_fx, open, _wires) = inbox("light");
    let lunch = row(&open, "Lunch on Friday?");
    let dot = unread_dot(&open, &lunch);

    open.press("u");
    let lunch = row(&open, "Lunch on Friday?");
    assert_eq!(
        unread_dot(&open, &lunch),
        dot,
        "the fade starts from the dot"
    );
    open.advance(1.0);
    assert_ne!(unread_dot(&open, &lunch), dot, "and ends without it");
}

#[test]
fn mail_that_moves_the_rows_down_shows_their_dots_at_once() {
    let (fx, open, wires) = inbox("light");
    // One in, one out: the list keeps its length, so each row's element now shows the message above it.
    fx.add("work", message("INBOX", 96, "Fresh news"));
    fx.store("work").remove_message("INBOX", 90).unwrap();
    synced(&wires, "work", 1);
    open.tick();
    let rows = [
        "Lunch on Friday?",
        "This week: ten things about tea",
        "Your receipt for order 1042",
    ]
    .map(|text| row(&open, text));
    let at_once = rows.each_ref().map(|label| unread_dot(&open, label));
    open.advance(1.0);
    assert_eq!(
        rows.each_ref().map(|label| unread_dot(&open, label)),
        at_once
    );
}

/// Tells the window that `account` synced `new_messages`.
fn synced(wires: &Wires, account: &str, new_messages: usize) {
    let event = Event::Synced {
        account: account.into(),
        new_messages,
        actions: 0,
        requests: vec![],
        errors: vec![],
    };
    wires.events.send(event).unwrap();
}

#[test]
fn mail_that_arrives_fades_in() {
    let (fx, open, wires) = inbox("light");
    let tea = row(&open, "This week: ten things about tea");
    let dot = unread_dot(&open, &tea);
    let resting = row_fill(&open, &tea);

    fx.add(
        "work",
        mail(96, "Sender <sender@example.com>", "Fresh news", "", ""),
    );
    synced(&wires, "work", 1);
    open.tick();
    let fresh = row(&open, "Fresh news");
    assert_eq!(
        unread_dot(&open, &fresh),
        resting,
        "the new row starts transparent"
    );
    open.advance(1.0);
    assert_eq!(unread_dot(&open, &fresh), dot, "and ends opaque");
}

#[test]
fn switching_views_shows_the_rows_at_once() {
    let (_fx, open, _wires) = inbox("light");
    open.with(|live| {
        live.app.select_view(View::Folder {
            account: 0,
            folder: "INBOX".into(),
        });
    });
    open.tick();
    let photos = row(&open, "Photos from the weekend");
    let at_once = unread_dot(&open, &photos);
    open.advance(1.0);
    assert_eq!(unread_dot(&open, &photos), at_once);
}

/// The sizes the layout holds at: the narrowest window its panes allow, the default, wide.
fn sizes(open: &Open) -> [(f32, f32); 3] {
    let (_, _, pad, _) = grid(open);
    let window = open.window();
    let panes = window.get_folders_width() + window.get_list_width() + window.get_list_min_width();
    [
        (2.0 * pad + panes, 600.0),
        (1280.0, 800.0),
        (1920.0, 1200.0),
    ]
}

/// Where an element is drawn, in window coordinates.
#[derive(Debug)]
struct Bounds {
    label: String,
    left: f32,
    top: f32,
    width: f32,
    height: f32,
}

impl Bounds {
    fn new(label: String, element: &i_slint_backend_testing::ElementHandle) -> Bounds {
        let (at, size) = (element.absolute_position(), element.size());
        Bounds {
            label,
            left: at.x,
            top: at.y,
            width: size.width,
            height: size.height,
        }
    }

    fn right(&self) -> f32 {
        self.left + self.width
    }

    fn bottom(&self) -> f32 {
        self.top + self.height
    }

    fn across(&self, pane: &Bounds) -> bool {
        self.left >= pane.left && self.right() <= pane.right()
    }

    fn overlaps(&self, pane: &Bounds) -> bool {
        self.right() > pane.left
            && self.left < pane.right()
            && self.bottom() > pane.top
            && self.top < pane.bottom()
    }

    fn within(&self, outer: &Bounds) -> bool {
        self.across(outer) && self.top >= outer.top && self.bottom() <= outer.bottom()
    }
}

/// Every element of component `type_name` in the window, each once.
fn all(open: &Open, type_name: &str) -> Vec<Bounds> {
    use i_slint_backend_testing::ElementHandle;
    let found = ElementHandle::find_by_element_type_name(&open.window(), type_name).collect();
    super::test_support::once(found)
        .iter()
        .map(|e| {
            let label = e.accessible_label().map_or(type_name.into(), String::from);
            Bounds::new(label, e)
        })
        .collect()
}

/// The one element of component `type_name`.
fn one(open: &Open, type_name: &str) -> Bounds {
    let mut found = all(open, type_name);
    assert_eq!(found.len(), 1, "{type_name} elements");
    found.remove(0)
}

/// The one element labelled `label`.
fn labelled(open: &Open, label: &str) -> Bounds {
    Bounds::new(label.into(), &open.get(label))
}

fn resize(open: &Open, (width, height): (f32, f32)) {
    use slint::ComponentHandle;
    open.window()
        .window()
        .set_size(slint::LogicalSize::new(width, height));
    open.tick();
}

#[test]
fn rows_and_buttons_keep_the_grid_at_every_size() {
    use super::theme::{CONTROL, LIST_ROW};
    let (_fx, open, _wires) = inbox("light");
    let (_, _, _, row) = grid(&open);
    for size in sizes(&open) {
        resize(&open, size);
        let heights = [
            ("FolderRow", row),
            ("ListRow", LIST_ROW),
            ("TextButton", row),
        ];
        for (type_name, height) in heights {
            let found = all(&open, type_name);
            assert!(!found.is_empty(), "{size:?}: no {type_name}");
            for b in found {
                assert_eq!(b.height, height, "{size:?}: {type_name} {:?}", b.label);
            }
        }
        for b in all(&open, "IconButton") {
            assert_eq!(
                (b.width, b.height),
                (CONTROL, CONTROL),
                "{size:?}: {:?}",
                b.label
            );
        }
    }
}

#[test]
fn content_sits_inset_from_its_pane_at_every_size() {
    let (_fx, open, _wires) = inbox("light");
    let (_, inset, pad, _) = grid(&open);
    for size in sizes(&open) {
        resize(&open, size);
        let folders = one(&open, "FolderPane");
        for row in all(&open, "FolderRow") {
            let content = row.left + pad;
            assert_eq!(
                content - folders.left,
                inset,
                "{size:?}: folder {:?}",
                row.label
            );
        }
        let headings = all(&open, "Heading");
        let heading = |label: &str| headings.iter().find(|h| h.label == label).unwrap().left;
        let (list, body) = (one(&open, "ListPane"), one(&open, "BodyPane"));
        assert_eq!(heading("Inbox") - list.left, inset, "{size:?}: list title");
        assert_eq!(
            heading("Lunch on Friday?") - body.left,
            inset,
            "{size:?}: subject"
        );
        let actions = labelled(&open, "View source").right();
        assert_eq!(
            body.right() - actions,
            inset,
            "{size:?}: the reader's actions"
        );
        let search = labelled(&open, "Search").right();
        assert_eq!(
            one(&open, "Toolbar").right() - search,
            inset,
            "{size:?}: search"
        );
    }
}

/// Every pane, bar, button and heading lies inside the window, and what the list and body panes draw inside them.
fn assert_nothing_overflows(open: &Open, (width, height): (f32, f32)) {
    let window = Bounds {
        label: "window".into(),
        left: 0.0,
        top: 0.0,
        width,
        height,
    };
    let drawn = [
        "BodyPane",
        "FolderPane",
        "Heading",
        "IconButton",
        "ListPane",
        "StatusBar",
        "TextButton",
        "Toolbar",
    ];
    for type_name in drawn {
        for b in all(open, type_name) {
            assert!(
                b.within(&window),
                "{width}×{height}: {b:?} overflows the window"
            );
        }
    }
    let (list, body) = (one(open, "ListPane"), one(open, "BodyPane"));
    let in_panes = ["Heading", "IconButton", "ListRow", "TextButton"];
    for b in in_panes.iter().flat_map(|type_name| all(open, type_name)) {
        let pane = [&list, &body].into_iter().find(|p| b.overlaps(p));
        if let Some(pane) = pane {
            assert!(b.across(pane), "{width}×{height}: {b:?} overflows {pane:?}");
        }
    }
}

#[test]
fn nothing_overflows_the_window_or_its_pane_at_any_size() {
    let (_fx, open, _wires) = inbox("light");
    for size in sizes(&open) {
        resize(&open, size);
        assert_nothing_overflows(&open, size);
    }
}

#[test]
fn the_panes_hold_their_content_at_their_minimum_widths() {
    use super::{list, theme::CONTROL};
    let (_fx, open, _wires) = inbox("light");
    let window = open.window();
    window.set_folders_width(CONTROL);
    window.set_list_width(list::MIN_WIDTH);
    let narrowest = sizes(&open)[0];
    resize(&open, narrowest);
    assert_eq!(one(&open, "ListPane").width, list::MIN_WIDTH);
    assert_eq!(one(&open, "BodyPane").width, list::MIN_WIDTH);
    assert_nothing_overflows(&open, narrowest);
}

/// A window narrower than its panes, which the windowing system does not allow: the body pane keeps its minimum
/// rather than vanishing.
#[test]
fn the_body_pane_keeps_its_minimum_width() {
    let (_fx, open, _wires) = inbox("light");
    resize(&open, (320.0, 600.0));
    assert_eq!(one(&open, "BodyPane").width, super::list::MIN_WIDTH);
}
