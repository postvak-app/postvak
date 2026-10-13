//! Each main view rendered to a PNG and compared with `tests/snapshots/gui_*.png`.
//!
//! Rendering differs between GPUs, drivers and backends (Metal, Vulkan, lavapipe) in anti-aliasing and glyph
//! coverage, so one set of reference images can only match one renderer. The references are drawn by Mesa's lavapipe
//! (a software Vulkan driver) with `TZ=UTC`, which is deterministic and what the Linux CI job installs. Everywhere
//! else, and in a plain `cargo test`, these tests build the views and stop before rendering, so a machine without a
//! GPU or a Vulkan driver still passes. Set `POSTVAK_SNAPSHOTS=1` to compare, or `UPDATE_SNAPSHOTS=1` to rewrite
//! the PNGs, then look at what changed.
use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

use super::App;
use super::app::View;
use super::test_support::{Fixture, Wires, message};
use crate::store::{Folder, LogEntry, Message};
use crate::sync::{Activity, Event};
use crate::trash::Trash;

/// Every fixture time: 2026-09-21 13:46 UTC, more than a week before any run, so the list shows the date.
const AT: i64 = 1_790_000_000;

const RULES: &str = "[[rules]]\nname = \"newsletters\"\nmatch.from = { contains = \"news@\" }\nactions = [\"archive\"]\n\n[[rules]]\nname = \"receipts\"\naccount = \"work\"\nmatch.subject = { contains = \"receipt\" }\nactions = [{ move = \"Receipts\" }]\n\n[[rules]]\nname = \"codes\"\nenabled = false\nproposed_by = \"agent\"\nmatch.subject = { contains = \"verification code\" }\nactions = [\"delete\"]\n";

fn enabled() -> bool {
    std::env::var_os("UPDATE_SNAPSHOTS").is_some()
        || std::env::var("POSTVAK_SNAPSHOTS").is_ok_and(|v| v == "1")
}

/// Compares the frame with `tests/snapshots/{name}.png` when snapshots are enabled.
fn snapshot(harness: &mut Harness<'static, App>, name: &str) {
    if !enabled() {
        return;
    }
    // Both instants matter: the fixtures are dated September and the status clock shows now, and a zone such as
    // Europe/London is UTC in winter but not in September.
    let fixture = chrono::DateTime::from_timestamp(AT, 0).unwrap();
    for (when, offset) in [
        ("now", chrono::Local::now().offset().local_minus_utc()),
        (
            "at the fixture time",
            fixture
                .with_timezone(&chrono::Local)
                .offset()
                .local_minus_utc(),
        ),
    ] {
        assert_eq!(
            offset, 0,
            "snapshots show local times, and the local offset {when} is not zero; run them with TZ=UTC"
        );
    }
    harness.snapshot(name);
}

fn mail(uid: u32, from: &str, subject: &str, flags: &str, body: &str) -> Message {
    let mut m = message("INBOX", uid, subject);
    let at = AT - i64::from(100 - uid) * 3_600;
    m.from_addr = Some(from.into());
    m.to_addr = Some("Robin Example <robin@example.com>".into());
    m.flags = flags.into();
    m.body_text = Some(body.into());
    m.date = Some(at);
    m.internaldate = at;
    m
}

/// Two accounts with a few folders and a filled inbox.
fn mailbox(theme: &str) -> Fixture {
    let fx = Fixture::new(&["home", "work"]);
    fx.append_config(&format!("[ui]\ntheme = \"{theme}\"\n"));
    for (name, special) in [
        ("Archive", Some("Archive")),
        ("Sent", Some("Sent")),
        ("Trash", Some("Trash")),
        ("Receipts", None),
    ] {
        fx.folder("work", name, special);
    }
    fx.folder("home", "Archive", Some("Archive"));
    for name in ["Projects/Postvak", "Projects/Website"] {
        let nested = Folder {
            name: name.into(),
            uidvalidity: 1,
            last_uid: 0,
            special_use: None,
            delimiter: Some("/".into()),
        };
        fx.store("work").upsert_folder(&nested).unwrap();
    }
    let lunch = "Hi Robin,\n\nShall we try the new place on the corner this Friday at noon?\nThe menu is at https://example.com/menu and they take bookings.\n\nCheers,\nLinus";
    let inbox = [
        (
            90,
            "Ada Lovelace <ada@example.com>",
            "Notes on the analytical engine",
            "\\Seen",
            "",
        ),
        (
            91,
            "Example Shop <shop@example.com>",
            "Your receipt for order 1042",
            "\\Seen",
            "",
        ),
        (
            92,
            "Weekly News <news@example.org>",
            "This week: ten things about tea",
            "",
            "1. Green tea\n2. Black tea\n3. Mint tea",
        ),
        (
            93,
            "Grace Hopper <grace@example.com>",
            "Planning the offsite",
            "\\Seen",
            "Shall we meet in Utrecht on the 14th?",
        ),
        (
            94,
            "Alan Turing <alan@example.com>",
            "Re: Planning the offsite",
            "\\Seen \\Flagged",
            "The 14th works for me.",
        ),
        (
            95,
            "Linus Example <linus@example.com>",
            "Lunch on Friday?",
            "",
            lunch,
        ),
    ];
    for (uid, from, subject, flags, body) in inbox {
        let mut m = mail(uid, from, subject, flags, body);
        if uid == 93 || uid == 94 {
            m.thread_id = "<offsite@example.com>".into();
        }
        fx.add("work", m);
    }
    let photos = mail(
        96,
        "Mum <mum@example.net>",
        "Photos from the weekend",
        "",
        "",
    );
    fx.add("home", photos);
    fx
}

/// The app over `fx` with one account up to date and the other offline, so the status bar holds still.
fn open(fx: &Fixture) -> (Harness<'static, App>, Wires) {
    let (mut harness, wires) = fx.harness();
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
    harness.run();
    (harness, wires)
}

/// INBOX of `work` with the newest message open in the body pane.
fn inbox(theme: &str) -> (Fixture, Harness<'static, App>, Wires) {
    let fx = mailbox(theme);
    let (mut harness, wires) = open(&fx);
    harness.state_mut().select_view(View::Folder {
        account: 1,
        folder: "INBOX".into(),
    });
    harness.run();
    assert!(harness.query_by_label_contains("Shall we try").is_some());
    (fx, harness, wires)
}

fn press(harness: &mut Harness<'static, App>, text: &str) {
    harness.event(egui::Event::Text(text.into()));
    harness.run();
}

#[test]
fn inbox_light() {
    let (_fx, mut harness, _wires) = inbox("light");
    snapshot(&mut harness, "gui_inbox_light");
}

#[test]
fn inbox_dark() {
    let (_fx, mut harness, _wires) = inbox("dark");
    snapshot(&mut harness, "gui_inbox_dark");
}

#[test]
fn all_inboxes() {
    let fx = mailbox("dark");
    let (mut harness, _wires) = open(&fx);
    harness.state_mut().select_view(View::AllInboxes);
    harness.run();
    assert!(harness.query_by_label_contains("home · Mum").is_some());
    assert!(
        harness
            .query_by_label_contains("work · Linus Example")
            .is_some()
    );
    snapshot(&mut harness, "gui_all_inboxes");
}

#[test]
fn inbox_with_marks_and_an_expanded_thread() {
    let (_fx, mut harness, _wires) = inbox("light");
    for key in ["x", "j", "x"] {
        press(&mut harness, key);
    }
    harness.key_press(egui::Key::ArrowRight);
    harness.run();
    // Checked here too, so runs without PNG comparison still catch a regression in marking or expanding.
    let list = &harness.state().list;
    for uid in [95, 94] {
        assert!(
            list.marked.contains(&(1, ("INBOX".to_string(), uid))),
            "{uid} not marked"
        );
    }
    for uid in [94, 93] {
        assert!(
            list.rows.iter().any(|row| row.member && row.uid == uid),
            "thread member {uid} not shown"
        );
    }
    snapshot(&mut harness, "gui_inbox_thread");
}

#[test]
fn keys_help() {
    let (_fx, mut harness, _wires) = inbox("light");
    press(&mut harness, "?");
    assert!(harness.state().help_open);
    snapshot(&mut harness, "gui_keys_help");
}

#[test]
fn rules() {
    let fx = mailbox("light");
    std::fs::write(fx.paths.rules_file(), RULES).unwrap();
    let (mut harness, _wires) = open(&fx);
    harness.state_mut().select_view(View::Rules);
    harness.run();
    assert!(harness.query_by_label("Approve").is_some());
    snapshot(&mut harness, "gui_rules");
}

#[test]
fn activity() {
    let fx = mailbox("light");
    for (minutes, rule, action, subject) in [
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
    ] {
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
    let (mut harness, _wires) = open(&fx);
    harness.state_mut().select_view(View::Activity);
    harness.run();
    assert!(harness.query_by_label("move Receipts").is_some());
    snapshot(&mut harness, "gui_activity");
}

#[test]
fn trash() {
    let fx = mailbox("light");
    let dir = fx.paths.trash_dir("work");
    std::fs::create_dir_all(&dir).unwrap();
    let raw = b"From: Example Shop <shop@example.com>\r\nSubject: Your verification code\r\n\r\n123456\r\n";
    Trash::new(dir).save("INBOX", 7, raw, AT).unwrap();
    let (mut harness, _wires) = open(&fx);
    harness.state_mut().select_view(View::Trash);
    harness.run();
    assert!(harness.query_by_label("Restore").is_some());
    snapshot(&mut harness, "gui_trash");
}

/// INBOX of `work` with the newsletter fixture open as HTML, in a window `width` points wide.
#[cfg(feature = "html")]
fn newsletter(theme: &str, width: f32) -> (Fixture, Harness<'static, App>, Wires) {
    use super::html::view_tests::{NEWSLETTER, painted, raw, wait_for};
    let fx = mailbox(theme);
    let news = mail(
        99,
        "Postvak Weekly <news@example.org>",
        "Your October roundup",
        "\\Seen",
        "Your October roundup",
    );
    fx.add("work", news);
    fx.store("work")
        .set_raw("INBOX", 99, &raw(NEWSLETTER), "Your October roundup")
        .unwrap();
    let (mut harness, wires) = open(&fx);
    harness.set_size(egui::vec2(width, 800.0));
    harness.state_mut().select_view(View::Folder {
        account: 1,
        folder: "INBOX".into(),
    });
    wait_for(&mut harness, painted);
    (fx, harness, wires)
}

#[cfg(feature = "html")]
#[test]
fn html_light() {
    let (_fx, mut harness, _wires) = newsletter("light", 1280.0);
    snapshot(&mut harness, "gui_html_light");
}

#[cfg(feature = "html")]
#[test]
fn html_dark() {
    let (_fx, mut harness, _wires) = newsletter("dark", 1280.0);
    snapshot(&mut harness, "gui_html_dark");
}

/// A body pane narrower than the newsletter's 600 px table: its media query stacks the columns.
#[cfg(feature = "html")]
#[test]
fn html_narrow() {
    let (_fx, mut harness, _wires) = newsletter("light", 1060.0);
    snapshot(&mut harness, "gui_html_narrow");
}

/// Where `text` was painted.
fn painted(harness: &Harness<'static, App>, text: &str) -> egui::Rect {
    harness
        .output()
        .shapes
        .iter()
        .find_map(|clipped| match &clipped.shape {
            egui::Shape::Text(shape) if shape.galley.text() == text => {
                Some(shape.galley.rect.translate(shape.pos.to_vec2()))
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("{text:?} not painted"))
}

#[test]
fn the_inbox_lines_up_on_the_grid() {
    use super::theme::{CONTROL, ICON, INSET, PAD, ROW};
    use egui::containers::panel::PanelState;
    let (_fx, mut harness, _wires) = inbox("light");
    let panel = |id: &str| {
        PanelState::load(&harness.ctx, egui::Id::new(id))
            .unwrap()
            .outer_rect
    };
    let glyph = (CONTROL - ICON) / 2.0;
    let list_content = panel("list").left() + INSET;
    let archive = harness.get_by_label("Archive message").rect();
    assert_eq!(archive.left() + glyph, list_content, "toolbar glyph");
    assert_eq!(
        painted(&harness, "Inbox").left(),
        list_content,
        "list title"
    );
    assert_eq!(
        archive.center().y,
        panel("toolbar").center().y,
        "toolbar centred"
    );
    let rules = harness.get_by_label("Rules").rect();
    let sync = harness.get_by_label("Sync now").rect();
    assert_eq!(sync.left() + glyph, rules.left() + PAD, "status glyph");
    let activity = harness.get_by_label("Activity").rect();
    assert_eq!(rules.height(), ROW, "folder row");
    assert_eq!(
        activity.top() - rules.top(),
        ROW + PAD / 2.0,
        "folder row pitch"
    );
    let row = harness
        .get_by_label_contains("Your receipt for order 1042")
        .rect();
    harness.state_mut().focus = super::app::Focus::Body;
    harness.run();
    let unfocused = harness
        .get_by_label_contains("Your receipt for order 1042")
        .rect();
    assert_eq!(row, unfocused, "the focus outline moves no content");
}

#[test]
fn headers_fields_and_popups_line_up_on_the_grid() {
    use super::theme::{INSET, PAD, ROW};
    use egui::containers::panel::PanelState;
    let (_fx, mut harness, _wires) = inbox("light");
    let list = PanelState::load(&harness.ctx, egui::Id::new("list"))
        .unwrap()
        .outer_rect;
    let title = painted(&harness, "Inbox");
    let source = harness.get_by_label("View source").rect();
    assert_eq!(
        title.center().y,
        source.center().y,
        "list title and reader actions share a line"
    );
    assert_eq!(
        harness.get_by_label("All inboxes (3)").rect().center().y,
        title.center().y,
        "All inboxes and the list title share a line"
    );
    let avatar = harness
        .output()
        .shapes
        .iter()
        .find_map(|clipped| match &clipped.shape {
            egui::Shape::Circle(circle) if circle.radius == INSET => Some(circle.center.y),
            _ => None,
        })
        .unwrap();
    let lines = (painted(&harness, "to").center().y - ROW - PAD / 2.0
        + painted(&harness, "to").center().y)
        / 2.0;
    assert_eq!(
        avatar, lines,
        "avatar centred on the sender and recipient lines"
    );
    press(&mut harness, "?");
    let keys = painted(&harness, "j / k, Down / Up").left();
    let window = harness
        .output()
        .shapes
        .iter()
        .find_map(|clipped| match &clipped.shape {
            egui::Shape::Rect(rect)
                if rect.stroke.width > 0.0 && rect.rect.left() < list.left() =>
            {
                Some(rect.rect)
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(keys, window.left() + INSET, "window content");
    harness.key_press(egui::Key::Escape);
    press(&mut harness, "/");
    let hint = painted(&harness, "Search this account");
    let field = harness
        .output()
        .shapes
        .iter()
        .find_map(|clipped| match &clipped.shape {
            egui::Shape::Rect(rect)
                if rect.fill == super::theme::PAPER.faint && rect.rect.contains(hint.center()) =>
            {
                Some(rect.rect)
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(field.height(), ROW, "text field height");
    assert_eq!(hint.left(), field.left() + PAD, "text field content");
}

#[test]
fn with_one_account_the_first_folder_and_first_message_share_a_line() {
    use egui::containers::panel::PanelState;
    let fx = Fixture::new(&["work"]);
    fx.add("work", message("INBOX", 1, "hello"));
    let (harness, _wires) = fx.harness();
    let list = PanelState::load(&harness.ctx, egui::Id::new("list"))
        .unwrap()
        .outer_rect;
    let first_folder = harness
        .query_all_by_label("Inbox")
        .map(|node| node.rect())
        .find(|rect| rect.right() < list.left())
        .unwrap();
    let first_row = harness
        .query_all_by_label_contains("hello")
        .map(|node| node.rect())
        .find(|rect| rect.right() < list.right())
        .unwrap();
    assert_eq!(first_folder.top(), first_row.top());
}
