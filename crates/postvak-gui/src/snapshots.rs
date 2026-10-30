//! Each main view drawn by Slint's software renderer and compared with `tests/snapshots/gui_*.png`, and the grid the
//! views line up on, from element positions.
//!
//! The software renderer and the bundled fonts draw the same pixels on every machine, so these run in every
//! `cargo test`. The views show local times: with a local offset other than zero the pixels are not compared (run
//! with `TZ=UTC`, as CI does). `UPDATE_SNAPSHOTS=1` rewrites the PNGs; a mismatch writes `{name}.new.png` beside the
//! reference.
use std::path::PathBuf;

use super::fixture::{AT, mail};
use super::state::{Focus, UiAction, View};
use super::test_support::{Fixture, Open, Wires, message};
use postvak_core::store::LogEntry;
use postvak_core::sync::{Activity, Event};
use postvak_core::trash::Trash;

const RULES: &str = "[[rules]]\nname = \"newsletters\"\nmatch.from = { contains = \"news@\" }\nactions = { archive = true }\n\n[[rules]]\nname = \"receipts\"\naccount = \"work\"\nmatch.subject = { contains = \"receipt\" }\nactions = { move = \"Receipts\" }\n\n[[rules]]\nname = \"codes\"\nenabled = false\nproposed_by = \"agent\"\nmatch.subject = { contains = \"verification code\" }\nactions = { delete = true }\n";

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
    // Past every fade, such as a dialog's as it opens: one period of the syncing pulse, so the pulse shows as before.
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_secs(2));
    let png = open.png();
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/snapshots")
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
    open.receive(&wires, events);
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

/// `work` fetching INBOX's headers part of the way, so the status bar shows its progress.
fn syncing(theme: &str) -> (Fixture, Open, Wires) {
    let (fx, open, wires) = inbox(theme);
    home_up_to_date(&wires);
    fetching(&wires, 12_500, 48_213);
    open.tick();
    // The fill glides to its value.
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_secs(1));
    (fx, open, wires)
}

#[test]
fn syncing_light() {
    let (_fx, open, _wires) = syncing("light");
    snapshot(&open, "gui_syncing_light");
}

#[test]
fn syncing_dark() {
    let (_fx, open, _wires) = syncing("dark");
    snapshot(&open, "gui_syncing_dark");
}

/// With sender images on, GitHub's DMARC-passing mail shows its logo in the reader instead of the initials.
#[test]
fn a_sender_logo() {
    let fx = Fixture::mailbox("light");
    fx.append_config("sender_images = true\n");
    let mut github = mail(
        96,
        "GitHub <noreply@github.com>",
        "[postvak] Pull request merged",
        "",
        "Merged into main.",
    );
    github.headers = super::test_support::branded("INBOX", 96, "x").headers;
    fx.add("work", github);
    super::test_support::cache_github_logo(&fx.paths);
    let (open, _wires) = open(&fx);
    open.with(|live| live.app.select_view(work_inbox()));
    open.tick_until(|open| open.window().get_body_info().has_logo);
    snapshot(&open, "gui_sender_logo");
}

/// A message with attachments in a reader wide enough to label its actions.
#[test]
fn reader_attachments() {
    let fx = Fixture::mailbox("light");
    let raw = "From: Northwind Rail <tickets@northwind-rail.example>\r\nSubject: Your e-ticket: Utrecht to Koeln\r\n\
        MIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"b\"\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\n\
        Your ticket for Thursday is attached.\r\n--b\r\nContent-Type: application/pdf\r\n\
        Content-Disposition: attachment; filename=\"e-ticket-UTR-CGN.pdf\"\r\n\r\nticket\r\n--b\r\n\
        Content-Type: application/pdf\r\nContent-Disposition: attachment; filename=\"conditions.pdf\"\r\n\r\n\
        conditions\r\n--b--\r\n";
    fx.add(
        "work",
        mail(
            97,
            "Northwind Rail <tickets@northwind-rail.example>",
            "Your e-ticket: Utrecht to Koeln",
            "",
            "Your ticket for Thursday is attached.",
        ),
    );
    fx.store("work")
        .set_raw(
            "INBOX",
            97,
            raw.as_bytes(),
            "Your ticket for Thursday is attached.",
        )
        .unwrap();
    let (open, _wires) = open(&fx);
    resize(&open, (1600.0, 800.0));
    open.with(|live| live.app.select_view(work_inbox()));
    open.tick_until(|open| {
        slint::Model::row_count(&open.window().get_body_info().attachments) == 2
    });
    snapshot(&open, "gui_reader_attachments");
}

/// A message the rule `travel` moved from INBOX to Travel, open in Travel, with the rule in rules.toml.
fn filed(theme: &str) -> (Fixture, Open, Wires) {
    let fx = Fixture::mailbox(theme);
    fx.folder("work", "Travel", None);
    std::fs::write(
        fx.paths.rules_file(),
        "[[rules]]\nname = \"travel\"\nmatch.from = { contains = \"rail\" }\nactions = { move = \"Travel\" }\n",
    )
    .unwrap();
    let ticket = mail(
        97,
        "Northwind Rail <tickets@northwind-rail.example>",
        "Your e-ticket: Utrecht to Koeln",
        "",
        "Your ticket for Thursday is attached.",
    );
    let store = fx.store("work");
    store
        .log_action(&LogEntry {
            id: 0,
            at: ticket.internaldate + 60,
            rule_name: "travel".into(),
            folder: "INBOX".into(),
            uid: 97,
            message_id: ticket.message_id.clone(),
            subject: ticket.subject.clone(),
            action: "move:Travel".into(),
            trash_file: None,
        })
        .unwrap();
    store.record_rule_move(&ticket).unwrap();
    fx.add(
        "work",
        postvak_core::store::Message {
            folder: "Travel".into(),
            uid: 3,
            ..ticket
        },
    );
    let (open, wires) = open(&fx);
    open.with(|live| {
        live.app.select_view(View::Folder {
            account: 1,
            folder: "Travel".into(),
        })
    });
    open.tick_until(|open| !open.labels_containing("by rule travel").is_empty());
    (fx, open, wires)
}

#[test]
fn reader_provenance_light() {
    let (_fx, open, _wires) = filed("light");
    snapshot(&open, "gui_reader_provenance_light");
}

#[test]
fn reader_provenance_dark() {
    let (_fx, open, _wires) = filed("dark");
    open.click("Why?");
    snapshot(&open, "gui_reader_provenance_dark");
}

/// The Rules view with a proposal whose dry run finds cached mail, and `newsletters` last fired on the fixture's date,
/// more than 30 days ago: a fixed time keeps the snapshot from changing with the clock.
#[test]
fn rules_hits_and_proposal() {
    let fx = Fixture::mailbox("light");
    std::fs::write(fx.paths.rules_file(), RULES).unwrap();
    fx.add(
        "work",
        mail(
            98,
            "Bank <no-reply@bank.example>",
            "Your verification code",
            "",
            "123456",
        ),
    );
    let store = fx.store("work");
    for uid in [1, 2, 3] {
        store
            .log_action(&LogEntry {
                id: 0,
                at: AT - i64::from(uid) * 3_600,
                rule_name: "newsletters".into(),
                folder: "INBOX".into(),
                uid,
                message_id: None,
                subject: None,
                action: "archive".into(),
                trash_file: None,
            })
            .unwrap();
    }
    let (open, _wires) = open(&fx);
    open.with(|live| live.app.select_view(View::Rules));
    open.tick_until(|open| {
        !open.labels_containing("would be deleted").is_empty()
            && slint::Model::row_data(&open.window().get_rules(), 0).is_some_and(|r| r.hits == "0")
    });
    snapshot(&open, "gui_rules_hits_and_proposal");
}

#[test]
fn inbox_banner() {
    let fx = Fixture::mailbox("light");
    std::fs::write(
        fx.paths.rules_file(),
        "[[rules]]\nname = \"tea\"\nenabled = false\nproposed_by = \"mcp:claude-desktop\"\nmatch.subject = { contains = \"tea\" }\nactions = { archive = true }\n",
    )
    .unwrap();
    let (open, _wires) = open(&fx);
    open.with(|live| live.app.select_view(work_inbox()));
    open.tick_until(|open| !open.labels_containing("Would affect").is_empty());
    snapshot(&open, "gui_inbox_banner");
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

/// A search whose hits are two exact ones and one found through a close match, the strip between them.
fn search_close_matches(theme: &str) -> (Fixture, Open, Wires) {
    let fx = Fixture::mailbox(theme);
    fx.add(
        "work",
        // A uid the mailbox does not use, since `add` ignores a duplicate; below 100, as `mail` dates it from there.
        crate::fixture::mail(
            97,
            "Linus Example <linus@example.com>",
            "Ofsite budget",
            "\\Seen",
            "Numbers for the ofsite.",
        ),
    );
    let (open, wires) = open(&fx);
    open.with(|live| live.app.select_view(work_inbox()));
    open.tick();
    open.press("/ offsite");
    open.tick();
    let shape: Vec<(String, bool)> = open.with(|live| {
        live.app
            .list
            .rows
            .iter()
            .map(|row| (row.subject.clone(), row.close))
            .collect()
    });
    assert_eq!(
        shape,
        [
            ("Re: Planning the offsite".to_string(), false),
            ("Planning the offsite".to_string(), false),
            ("Ofsite budget".to_string(), true),
        ]
    );
    assert_eq!(open.labels_containing("close match").len(), 1);
    // The search field has focus; an inactive window draws no text cursor, whose blink phase differs between
    // platforms (the merge queue's macOS run drew the opposite phase to Linux).
    slint::ComponentHandle::window(&open.window())
        .dispatch_event(slint::platform::WindowEvent::WindowActiveChanged(false));
    (fx, open, wires)
}

#[test]
fn search_close_matches_light() {
    let (_fx, open, _wires) = search_close_matches("light");
    snapshot(&open, "gui_search_close_light");
}

#[test]
fn search_close_matches_dark() {
    let (_fx, open, _wires) = search_close_matches("dark");
    snapshot(&open, "gui_search_close_dark");
}

/// INBOX of `work`, searching a word only the lunch mail's body holds, so its row shows the snippet.
fn search_snippet(theme: &str) -> (Fixture, Open, Wires) {
    let fx = Fixture::mailbox(theme);
    let (open, wires) = open(&fx);
    open.with(|live| live.app.select_view(work_inbox()));
    open.tick();
    open.press("/ menu");
    open.tick();
    let rows: Vec<(String, Option<String>)> = open.with(|live| {
        live.app
            .list
            .rows
            .iter()
            .map(|row| (row.subject.clone(), row.snippet.clone()))
            .collect()
    });
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].0, "Lunch on Friday?");
    assert!(
        rows[0].1.as_deref().is_some_and(|s| s.contains("menu")),
        "{rows:?}"
    );
    // The search field has focus; an inactive window draws no text cursor, whose blink phase differs between
    // platforms.
    slint::ComponentHandle::window(&open.window())
        .dispatch_event(slint::platform::WindowEvent::WindowActiveChanged(false));
    (fx, open, wires)
}

#[test]
fn search_snippet_light() {
    let (_fx, open, _wires) = search_snippet("light");
    snapshot(&open, "gui_search_snippet_light");
}

#[test]
fn search_snippet_dark() {
    let (_fx, open, _wires) = search_snippet("dark");
    snapshot(&open, "gui_search_snippet_dark");
}

#[test]
fn inbox_filtered() {
    let (_fx, open, _wires) = inbox("light");
    open.window().invoke_list_filter_clicked(1);
    assert_eq!(open.find("Clear filter").len(), 1);
    snapshot(&open, "gui_inbox_filtered");
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

/// A dialog fades in: drawn as it opens it differs from the same dialog once the fade is over. When the desktop asks
/// for less motion it is whole at once.
#[test]
fn a_dialog_fades_in_unless_the_desktop_asks_for_less_motion() {
    use slint::ComponentHandle;
    for reduced in [false, true] {
        let (_fx, open, _wires) = inbox("light");
        open.window()
            .global::<super::ui::Theme>()
            .set_reduce_motion(reduced);
        open.press("?");
        open.tick();
        let opening = pixels(&open.png());
        i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_secs(2));
        let opened = pixels(&open.png());
        assert_eq!(opening == opened, reduced, "reduced motion {reduced}");
    }
}

#[test]
fn rules() {
    let fx = Fixture::mailbox("light");
    std::fs::write(fx.paths.rules_file(), RULES).unwrap();
    let (open, _wires) = open(&fx);
    open.with(|live| live.app.select_view(View::Rules));
    open.tick();
    open.tick_until(|open| !open.labels_containing("would be deleted").is_empty());
    assert_eq!(open.find("Approve rule").len(), 1);
    snapshot(&open, "gui_rules");
}

#[test]
fn rule_edit_proposal() {
    let fx = Fixture::mailbox("light");
    let edit = "[[rules]]\nname = \"newsletters\"\nmatch.from = { contains = \"news@\" }\nactions = { archive = true }\n\n[[rules]]\nname = \"newsletters (edit)\"\nenabled = false\nproposed_by = \"agent\"\nreplaces = \"newsletters\"\nmatch.from = { contains = \"news@\" }\nactions = { archive = true, read = true }\n";
    std::fs::write(fx.paths.rules_file(), edit).unwrap();
    let (open, _wires) = open(&fx);
    open.with(|live| live.app.select_view(View::Rules));
    open.tick_until(|open| !open.labels_containing("would be archived").is_empty());
    assert_eq!(open.find("Changes rule 'newsletters' · 1 line").len(), 1);
    snapshot(&open, "gui_rule_edit_proposal");
}

#[test]
fn rule_editor() {
    let fx = Fixture::mailbox("light");
    std::fs::write(fx.paths.rules_file(), RULES).unwrap();
    let (open, _wires) = open(&fx);
    open.with(|live| live.app.select_view(View::Rules));
    open.tick();
    open.click("receipts");
    open.tick();
    assert_eq!(open.find("Save").len(), 1);
    snapshot(&open, "gui_rule_editor");
}

#[test]
fn apply_confirm() {
    let fx = Fixture::mailbox("light");
    let tidy = "[[rules]]\nname = \"tidy\"\naccount = \"work\"\nmatch.subject = { contains = \"a\" }\nactions = { delete = true }\n";
    std::fs::write(fx.paths.rules_file(), tidy).unwrap();
    let (open, _wires) = open(&fx);
    open.with(|live| live.app.select_view(View::Rules));
    open.tick();
    open.click("tidy");
    open.click("Apply to existing mail");
    open.tick();
    assert_eq!(open.find("Delete and apply").len(), 1);
    snapshot(&open, "gui_apply_confirm");
}

/// The mailbox with `actions` applied in turn, the folder dialog showing their outcome.
fn folder_dialog(fx: &Fixture, actions: Vec<UiAction>) -> (Open, Wires) {
    let (open, wires) = open(fx);
    open.with(|live| {
        actions
            .into_iter()
            .for_each(|action| live.app.apply(action))
    });
    open.tick();
    (open, wires)
}

#[test]
fn folder_new() {
    let fx = Fixture::mailbox("light");
    let new = vec![
        UiAction::NewFolder(1, Some("Projects/Website".into())),
        UiAction::FolderName("Launch".into()),
    ];
    let (open, _wires) = folder_dialog(&fx, new);
    assert_eq!(open.find("Create").len(), 1);
    snapshot(&open, "gui_folder_new");
}

#[test]
fn folder_rename() {
    let fx = Fixture::mailbox("light");
    let site = "[[rules]]\nname = \"site\"\nmatch.subject = { contains = \"site\" }\nactions = { move = \"Projects/Website\" }\n\n[[rules]]\nname = \"deploys\"\naccount = \"work\"\nmatch.subject = { contains = \"deploy\" }\nactions = { move = \"Projects/Website\" }\n";
    std::fs::write(fx.paths.rules_file(), site).unwrap();
    let rename = vec![
        UiAction::RenameFolder(1, "Projects/Website".into()),
        UiAction::FolderName("Site".into()),
    ];
    let (open, _wires) = folder_dialog(&fx, rename);
    assert_eq!(open.find("Rename").len(), 1);
    snapshot(&open, "gui_folder_rename");
}

#[test]
fn folder_delete_not_empty() {
    let fx = Fixture::mailbox("light");
    let delete = vec![
        UiAction::DeleteFolder(1, "Receipts".into()),
        UiAction::FolderConfirm(None),
    ];
    let (open, wires) = folder_dialog(&fx, delete);
    let not_empty = Event::FolderNotEmpty {
        account: "work".into(),
        name: "Receipts".into(),
        count: 12,
        has_trash: true,
        request: 1,
    };
    open.receive(&wires, [not_empty]);
    assert_eq!(open.find("Move 12 messages to Trash").len(), 1);
    snapshot(&open, "gui_folder_delete_not_empty");
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
        (9, "gui", "flagged", "Re: Planning the offsite"),
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

/// The add-account form over the inbox, with the keyboard in its first field and the window inactive.
fn add_account(theme: &str, name: &str) {
    let (_fx, open, _wires) = inbox(theme);
    open.window()
        .invoke_menu_item(super::ui::MenuAction::AddAccount);
    open.tick();
    assert_eq!(open.find("Add account").len(), 1);
    // An inactive window draws no text cursor, whose blink phase differs between platforms.
    slint::ComponentHandle::window(&open.window())
        .dispatch_event(slint::platform::WindowEvent::WindowActiveChanged(false));
    snapshot(&open, name);
}

#[test]
fn add_account_light() {
    add_account("light", "gui_add_account_light");
}

#[test]
fn add_account_dark() {
    add_account("dark", "gui_add_account_dark");
}

/// Preferences over the inbox, at `section`; the work account has an alias and a trusted authserv-id.
fn preferences(theme: &str, section: super::preferences::Section, name: &str) {
    let (fx, open, _wires) = inbox(theme);
    let path = fx.paths.config_file();
    postvak_core::config::update_account(&path, "work", |a| {
        a.aliases = vec!["*@shop.example.com".into()];
        a.trusted_authserv_ids = Some(vec!["mx.example.com".into()]);
    })
    .unwrap();
    open.window()
        .invoke_menu_item(super::ui::MenuAction::Preferences);
    open.with(|live| {
        live.app.apply(UiAction::PreferencesSection(section));
        live.app.apply(UiAction::PreferencesAccount(1));
    });
    open.tick();
    assert_eq!(open.find("Preferences").len(), 1);
    slint::ComponentHandle::window(&open.window())
        .dispatch_event(slint::platform::WindowEvent::WindowActiveChanged(false));
    snapshot(&open, name);
}

#[test]
fn preferences_general_light() {
    preferences(
        "light",
        super::preferences::Section::General,
        "gui_preferences_general_light",
    );
}

#[test]
fn preferences_accounts_dark() {
    preferences(
        "dark",
        super::preferences::Section::Accounts,
        "gui_preferences_accounts_dark",
    );
}

/// INBOX of `work` with the newsletter open as HTML, in a window `width` points wide, once it is painted.
fn newsletter(theme: &str, width: f32) -> (Fixture, Open, Wires) {
    html_message(
        theme,
        width,
        include_str!("../../../tests/fixtures/newsletter.html"),
    )
}

/// INBOX of `work` with a message whose HTML part is `html` open, once it is painted.
fn html_message(theme: &str, width: f32, html: &str) -> (Fixture, Open, Wires) {
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
    let raw = super::test_support::raw(html);
    fx.store("work")
        .set_raw("INBOX", 99, &raw, "Your October roundup")
        .unwrap();
    let (open, wires) = open(&fx);
    open.window()
        .window()
        .set_size(slint::LogicalSize::new(width, 800.0));
    open.with(|live| live.app.select_view(work_inbox()));
    wait_painted(&open);
    (fx, open, wires)
}

/// Ticks the window until the HTML page is laid out and every strip in view is painted.
fn wait_painted(open: &Open) {
    wait_repainted(open, &[]);
}

/// The strips the HTML view shows now. Holding them keeps their memory from being reused by later strips.
fn shown_strips(open: &Open) -> Vec<std::sync::Arc<crate::html::Pixels>> {
    open.with(|live| {
        let view = live.app.body.as_ref().and_then(|b| b.html_view.as_ref());
        view.map(|v| v.strips.values().cloned().collect())
            .unwrap_or_default()
    })
}

/// As `wait_painted`, and none of the strips shown is one of `old`: a restyle keeps the old strips on screen until
/// its layout arrives, so they alone do not say the page was painted again.
fn wait_repainted(open: &Open, old: &[std::sync::Arc<crate::html::Pixels>]) {
    let painted = |open: &Open| {
        open.with(|live| {
            let view = live.app.body.as_ref().and_then(|b| b.html_view.as_ref());
            view.is_some_and(|v| {
                v.page.is_some()
                    && !v.requested.is_empty()
                    && v.requested.iter().all(|s| v.strips.contains_key(s))
                    && !v
                        .strips
                        .values()
                        .any(|new| old.iter().any(|old| std::sync::Arc::ptr_eq(new, old)))
            })
        })
    };
    open.tick_until(painted);
}

#[test]
fn html_light() {
    let (_fx, open, _wires) = newsletter("light", 1280.0);
    snapshot(&open, "gui_html_light");
}

/// The newsletter adapted to the dark theme.
#[test]
fn html_dark() {
    let (_fx, open, _wires) = newsletter("dark", 1280.0);
    snapshot(&open, "gui_html_dark");
}

/// The newsletter in the dark theme with Original colours on: as in the light theme, on a white page.
#[test]
fn html_dark_original_colours() {
    let (_fx, open, _wires) = newsletter("dark", 1280.0);
    let adapted = shown_strips(&open);
    open.window().invoke_toggle_original_colours();
    wait_repainted(&open, &adapted);
    snapshot(&open, "gui_html_dark_original");
}

/// Mail with its own dark-mode CSS, shown as its sender wrote it for the dark theme.
#[test]
fn html_dark_own_css() {
    let (_fx, open, _wires) = html_message(
        "dark",
        1280.0,
        include_str!("../../../tests/fixtures/newsletter-dark-css.html"),
    );
    snapshot(&open, "gui_html_dark_own_css");
}

/// The software renderer casts every rectangle to `i16`, so a page background as tall as a very long message
/// panicked the window (#321). Opens a page of 50,000 px and draws it.
#[test]
fn a_very_tall_page_draws() {
    let html = format!(
        "<html><body>{}</body></html>",
        "<div style=\"height:1000px;background:#eee\">card</div>".repeat(50)
    );
    let (_fx, open, _wires) = html_message("light", 1280.0, &html);
    let height = open.with(|live| {
        let view = live.app.body.as_ref().and_then(|b| b.html_view.as_ref());
        view.and_then(|v| v.page).map_or(0.0, |(_, h)| h)
    });
    assert!(height > 40_000.0, "page is {height} tall");
    assert!(!open.png().is_empty());
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
    let (_, inset, pad, _) = grid(&open);
    let window = open.window();
    let theme = slint::ComponentHandle::global::<super::ui::Theme>(&window);
    let title = open
        .find("Inbox")
        .into_iter()
        .map(|e| e.absolute_position().x)
        .fold(0.0, f32::max);
    assert_eq!(
        edge(&open, "Search").0,
        title,
        "the search field over the list title"
    );
    let list = open.get(&row(&open, "Your receipt for order 1042"));
    assert_eq!(
        list.absolute_position().x - pad,
        title - inset,
        "the rows sit in from the list's edge"
    );
    // The canvas tops them alike: the 32px groups and the 36px field.
    let top = |label: &str| {
        open.get(&open.labels_containing(label).remove(0))
            .absolute_position()
            .y
    };
    assert_eq!(
        top("Archive message"),
        top("Search"),
        "the reader's actions and the search field start on one line"
    );
    assert_eq!(
        edge(&open, "Archive message").1,
        edge(&open, "More actions").1,
        "the reader's actions share a line"
    );
    let (rules, activity) = (open.get("Rules"), open.get("Activity"));
    assert_eq!(rules.size().height, theme.get_folder_row(), "folder row");
    let pitch = activity.absolute_position().y - rules.absolute_position().y;
    assert_eq!(pitch, theme.get_folder_row(), "folder rows touch");
    // The account's dot, `pad * 3 / 4` wide, stands over the folder glyphs and its name follows a pad later.
    let after_dot = rules.absolute_position().x + pad + pad * 3.0 / 4.0 + pad;
    let heading = open.get("work").absolute_position().x;
    assert_eq!(
        heading, after_dot,
        "the account's dot over the folder glyphs"
    );
    let row = edge(&open, "Your receipt for order 1042");
    open.with(|live| live.app.focus = Focus::Body);
    assert_eq!(
        edge(&open, "Your receipt for order 1042"),
        row,
        "the focus outline moves no content"
    );
}

#[test]
fn the_keys_window_content_sits_inset() {
    let (_fx, open, _wires) = inbox("light");
    let (_, inset, _, _) = grid(&open);
    open.press("?");
    let window = open.get("Keys").absolute_position().x;
    assert_eq!(edge(&open, "j / k, Down / Up").0, window + inset);
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
    let element = open.get(label);
    let at = element.absolute_position();
    let list_row =
        slint::ComponentHandle::global::<super::ui::Theme>(&open.window()).get_list_row();
    // Below the row's heading strip, if it has one: after the padding, the avatar and a gap, on the first line.
    let strip = element.size().height - list_row;
    let line = pad * 5.0 / 2.0;
    pixel(
        open,
        at.x + pad * 3.0 / 2.0 + pad * 4.0 + pad * 3.0 / 2.0 + 3.5,
        at.y + strip + (list_row - 3.0 * line) / 2.0 + line / 2.0,
    )
}

/// Brings `home` back online, so the status bar shows `work`'s line rather than the offline one.
fn home_up_to_date(wires: &Wires) {
    let event = Event::Activity {
        account: "home".into(),
        activity: Activity::Idle { since: AT },
    };
    wires.send([event]);
}

/// Tells the window that `work` has fetched `done` of INBOX's `total` headers.
fn fetching(wires: &Wires, done: usize, total: usize) {
    let activity = Activity::FetchingHeaders {
        folder: "INBOX".into(),
        done,
        total,
    };
    let event = Event::Activity {
        account: "work".into(),
        activity,
    };
    wires.send([event]);
}

/// What the status bar's progress hairline tells a screen reader, while it says anything.
fn meter_value(open: &Open) -> Option<String> {
    use i_slint_backend_testing::{AccessibleRole, ElementHandle};
    let meters = ElementHandle::find_by_element_type_name(&open.window(), "Meter").collect();
    let meter = super::test_support::once(meters).remove(0);
    let found = meter
        .query_descendants()
        .match_predicate(|e| e.accessible_role() == Some(AccessibleRole::ProgressIndicator))
        .find_all();
    super::test_support::once(found)
        .pop()?
        .accessible_value()
        .map(String::from)
}

/// The colour on the status bar's progress hairline, `share` of the way along it.
fn meter_at(open: &Open, share: f32) -> slint::Color {
    let meter = one(open, "Meter");
    pixel(
        open,
        meter.left + meter.width * share,
        meter.top + meter.height / 2.0,
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
fn the_sync_progress_glides_forward_jumps_back_and_fades_out() {
    let (_fx, open, wires) = inbox("light");
    home_up_to_date(&wires);
    open.tick();
    let hidden = meter_at(&open, 0.75);

    fetching(&wires, 100, 1000);
    open.tick();
    open.advance(1.0);
    let track = meter_at(&open, 0.75);
    assert_ne!(track, hidden, "a fetch shows the track");
    let filled = meter_at(&open, 0.05);
    assert_ne!(filled, track, "and fills its start");
    assert_eq!(meter_value(&open).as_deref(), Some("10%"));

    fetching(&wires, 900, 1000);
    open.tick();
    assert_eq!(meter_at(&open, 0.75), track, "the fill glides forward");
    open.advance(1.0);
    assert_ne!(meter_at(&open, 0.75), track, "and arrives");

    fetching(&wires, 0, 1000);
    open.tick();
    assert_eq!(
        meter_at(&open, 0.75),
        track,
        "a new fetch starts empty without rewinding"
    );
    assert_eq!(meter_value(&open).as_deref(), Some("0%"));

    fetching(&wires, 1000, 1000);
    open.tick();
    open.advance(1.0);
    let filled_end = meter_at(&open, 0.75);
    let syncing = Event::Activity {
        account: "work".into(),
        activity: Activity::SyncingFolder {
            folder: "Archive".into(),
            index: 2,
            of: 3,
        },
    };
    open.receive(&wires, [syncing]);
    assert_eq!(
        meter_value(&open),
        None,
        "screen readers hear the line only"
    );
    assert_eq!(
        meter_at(&open, 0.75),
        filled_end,
        "the bar fades out holding its last value"
    );
    open.advance(1.0);
    assert_eq!(meter_at(&open, 0.75), hidden, "and ends hidden");
    assert_eq!(meter_at(&open, 0.05), hidden);
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
    wires.send([super::test_support::synced(account, new_messages)]);
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
    let window = open.window();
    let theme = slint::ComponentHandle::global::<super::ui::Theme>(&window);
    for size in sizes(&open) {
        resize(&open, size);
        let heights = [
            ("FolderRow", theme.get_folder_row()),
            ("ListRow", LIST_ROW),
            ("TextButton", CONTROL),
        ];
        for (type_name, height) in heights {
            let found = all(&open, type_name);
            // The inbox's reader has no text buttons; the rule holds wherever they show.
            assert!(
                !found.is_empty() || type_name == "TextButton",
                "{size:?}: no {type_name}"
            );
            for b in found {
                // A list row grows by its heading strip, when it has one; a pill is half a pad taller than a plain
                // text button.
                let strip = match type_name {
                    "ListRow" => super::theme::SECTION,
                    "TextButton" => theme.get_pad() / 2.0,
                    _ => 0.0,
                };
                assert!(
                    b.height == height || b.height == height + strip,
                    "{size:?}: {type_name} {:?} is {}",
                    b.label,
                    b.height
                );
            }
        }
        // The reader's More is as tall as the pill beside it.
        let pill = CONTROL + theme.get_pad() / 2.0;
        for b in all(&open, "IconButton") {
            assert!(
                b.width == b.height && [CONTROL, pill, theme.get_action()].contains(&b.height),
                "{size:?}: {b:?}"
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
        let search = labelled(&open, "Search");
        assert_eq!(search.left - list.left, inset, "{size:?}: search");
        assert_eq!(list.right() - search.right(), inset, "{size:?}: search");
        // The reader's margin widens once there is room for it.
        let margin = heading("Lunch on Friday?") - body.left;
        assert!(
            [inset, 2.0 * inset].contains(&margin),
            "{size:?}: subject at {margin}"
        );
        let archive = labelled(&open, "Archive message").left;
        assert_eq!(
            archive - body.left,
            margin,
            "{size:?}: the reader's actions"
        );
        let source = labelled(&open, "More actions").right();
        assert!(
            body.right() - source >= margin,
            "{size:?}: the reader's tools"
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
