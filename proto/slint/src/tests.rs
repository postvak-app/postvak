//! The window under Slint's testing backend: each test thread gets its own headless window, mock time, element
//! search by accessible label, and snapshots from the software renderer, so nothing here needs a GPU or a display.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use i_slint_backend_testing::ElementHandle;
use postvak::rules::Action;
use postvak::sync::Command;
use slint::platform::{Key, WindowEvent};
use slint::{ComponentHandle, Model as _, PhysicalSize, SharedString};

use crate::MailWindow;
use crate::app::{App, READ_DELAY, key_help};
use crate::fixture::test_model;

struct Open {
    _dir: tempfile::TempDir,
    ui: MailWindow,
    app: Rc<RefCell<App>>,
    commands: Receiver<(String, Command)>,
}

/// This test thread's headless backend: mock time, no event loop, and Slint's software renderer, so text has real
/// metrics from the bundled fonts and `take_snapshot` draws.
fn init() {
    use i_slint_backend_testing::{TestingBackend, TestingBackendOptions};
    slint::platform::set_platform(Box::new(TestingBackend::new(TestingBackendOptions {
        mock_time: true,
        threading: false,
        renderer_name: Some("software".into()),
    })))
    .unwrap();
}

/// The snapshot inbox in a 1280×800 window at `scale`.
fn open(newsletter: bool, scale: f32) -> Open {
    init();
    let (dir, model, commands) = test_model(newsletter);
    let ui = MailWindow::new().unwrap();
    ui.window().dispatch_event(WindowEvent::ScaleFactorChanged {
        scale_factor: scale,
    });
    ui.window().set_size(PhysicalSize::new(
        (1280.0 * scale) as u32,
        (800.0 * scale) as u32,
    ));
    ui.show().unwrap();
    let app = App::new(&ui, model);
    ui.invoke_focus_list();
    i_slint_backend_testing::mock_elapsed_time(Duration::ZERO);
    Open {
        _dir: dir,
        ui,
        app,
        commands,
    }
}

/// Presses and releases each space-separated key: a word for the arrows and Esc, else the text itself.
fn press(ui: &MailWindow, keys: &str) {
    for key in keys.split(' ') {
        let text: SharedString = match key {
            "down" => Key::DownArrow.into(),
            "up" => Key::UpArrow.into(),
            "right" => Key::RightArrow.into(),
            "left" => Key::LeftArrow.into(),
            "escape" => Key::Escape.into(),
            other => other.into(),
        };
        ui.window()
            .dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
        ui.window()
            .dispatch_event(WindowEvent::KeyReleased { text });
    }
    i_slint_backend_testing::mock_elapsed_time(Duration::ZERO);
}

fn state(app: &Rc<RefCell<App>>) -> (usize, usize, Option<u32>) {
    let app = app.borrow();
    let m = app.model.borrow();
    (
        m.cursor,
        m.rows.len(),
        m.rows.get(m.cursor).map(|r| r.count),
    )
}

#[test]
fn keys_move_the_cursor_open_a_thread_and_archive_it() {
    let Open {
        ui, app, commands, ..
    } = open(false, 1.0);
    press(&ui, "j");
    // Row 1 is the offsite thread: two messages.
    assert_eq!(state(&app), (1, 5, Some(2)));
    assert_eq!(ui.get_cursor(), 1);
    press(&ui, "right");
    assert_eq!(state(&app).1, 7);
    assert_eq!(ui.get_rows().row_count(), 7);
    press(&ui, "left e");
    let sent: Vec<_> = commands.try_iter().collect();
    let [(_, Command::Apply { uids, action, .. })] = sent.as_slice() else {
        panic!("expected one Apply, got {sent:?}");
    };
    assert_eq!((uids.as_slice(), action), (&[93, 94][..], &Action::Archive));
    assert_eq!(state(&app).1, 4);
    press(&ui, "k down down");
    assert_eq!(state(&app).0, 2);
    press(&ui, "?");
    assert!(ui.get_help(), "? opens the key table");
    let next = key_help()
        .into_iter()
        .find(|k| k.what == "Next message")
        .unwrap();
    assert_eq!(
        next.keys, "j, DownArrow",
        "the table reads its keys from the keymap"
    );
    press(&ui, "x escape");
    assert!(!ui.get_help());
    assert_eq!(
        app.borrow().model.borrow().marked.len(),
        1,
        "Esc closed the help first"
    );
    press(&ui, "escape");
    let m = (app.borrow().model.borrow().marked.len(), state(&app).0);
    assert_eq!(m, (0, 2), "the cursor stays");
}

#[test]
fn rows_and_the_search_field_carry_accessible_labels() {
    let Open { ui, .. } = open(false, 1.0);
    assert_eq!(
        ElementHandle::find_by_accessible_label(&ui, "Search").count(),
        1
    );
    let label = ui.get_rows().row_data(0).unwrap().label;
    assert!(
        label.starts_with("Unread, Linus Example, Lunch on Friday?"),
        "{label}"
    );
    let rows = ElementHandle::find_by_accessible_label(&ui, &label).count();
    assert_eq!(
        rows, 1,
        "the row is in the accessibility tree under its label"
    );
}

#[test]
fn the_search_field_takes_letters_and_esc_gives_the_keys_back() {
    let Open { ui, app, .. } = open(false, 1.0);
    let search = |app: &Rc<RefCell<App>>| app.borrow().model.borrow().search.clone();
    press(&ui, "/ o f f s i t e");
    assert_eq!(search(&app).as_deref(), Some("offsite"));
    assert_eq!(state(&app), (0, 2, Some(1)), "letters typed, not acted on");
    press(&ui, "j");
    assert_eq!(search(&app).as_deref(), Some("offsitej"));
    press(&ui, "escape");
    assert_eq!((search(&app), state(&app).1), (None, 5));
    assert_eq!(ui.get_query(), "");
    press(&ui, "j");
    assert_eq!(state(&app).0, 1, "the list has its keys back");
}

/// Polls the render thread until the shown page has a painted strip; tests have no event loop to wake.
fn wait_for_strips(app: &Rc<RefCell<App>>) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while app.borrow().painted_strips() == 0 {
        assert!(Instant::now() < deadline, "no strip within 10 s");
        std::thread::sleep(Duration::from_millis(5));
        app.borrow_mut().receive();
        i_slint_backend_testing::mock_elapsed_time(Duration::ZERO);
    }
}

#[test]
fn the_newsletter_paints_and_becomes_read_a_second_after_it_is_opened() {
    let Open {
        ui, app, commands, ..
    } = open(true, 1.0);
    wait_for_strips(&app);
    assert!(ui.get_show_page());
    assert!(ui.get_page_height() > 500.0);
    // Shown at start, not chosen: it stays unread however long it is on screen.
    i_slint_backend_testing::mock_elapsed_time(READ_DELAY * 2);
    assert!(commands.try_iter().next().is_none());
    // Away and back: chosen now, read a second later and not before.
    press(&ui, "j k");
    i_slint_backend_testing::mock_elapsed_time(READ_DELAY / 2);
    assert!(commands.try_iter().next().is_none());
    i_slint_backend_testing::mock_elapsed_time(READ_DELAY);
    let sent: Vec<_> = commands.try_iter().collect();
    let [(account, Command::Apply { uids, action, .. })] = sent.as_slice() else {
        panic!("expected one Apply, got {sent:?}");
    };
    assert_eq!(
        (account.as_str(), uids.as_slice(), action),
        ("work", &[97][..], &Action::MarkRead)
    );
}

/// The window with the newsletter open, drawn by the software renderer at scale 2.
fn screenshot() -> slint::SharedPixelBuffer<slint::Rgba8Pixel> {
    let Open { ui, app, .. } = open(true, 2.0);
    wait_for_strips(&app);
    ui.window().take_snapshot().unwrap()
}

#[test]
fn the_window_renders_without_a_gpu_and_the_same_twice() {
    let first = screenshot();
    let second = std::thread::spawn(screenshot).join().unwrap();
    let (w, h) = (first.width(), first.height());
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/screenshots");
    std::fs::create_dir_all(&dir).unwrap();
    image::RgbaImage::from_raw(w, h, first.as_bytes().to_vec())
        .unwrap()
        .save(dir.join("inbox.png"))
        .unwrap();
    assert_eq!((w, h), (2560, 1600));
    assert!(first.as_bytes() == second.as_bytes(), "two renders differ");
}

/// p50, p95 and max of `times`, in milliseconds; the same report as `src/gui/bench.rs`.
fn report(name: &str, times: &mut [Duration]) {
    times.sort();
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    let at = |q: f64| times[((times.len() - 1) as f64 * q) as usize];
    println!(
        "{name}: {} frames, p50 {:.2} ms, p95 {:.2} ms, max {:.2} ms",
        times.len(),
        ms(at(0.5)),
        ms(at(0.95)),
        ms(at(1.0))
    );
}

/// Holding `j` on 20 000 messages, each frame drawn by the software renderer (no GPU) and read back.
#[test]
#[ignore = "benchmark, run by hand: cargo test --release hold_j -- --ignored --nocapture"]
fn hold_j_on_20000_messages() {
    init();
    let dir = tempfile::tempdir().unwrap();
    let (home, shown) = crate::fixture::open_home(dir.path(), Some(20_000)).unwrap();
    let (model, _commands) = crate::fixture::open_model(&home, shown).unwrap();
    let ui = MailWindow::new().unwrap();
    ui.window()
        .dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: 2.0 });
    ui.window().set_size(PhysicalSize::new(2560, 1600));
    ui.show().unwrap();
    let app = App::new(&ui, model);
    ui.invoke_focus_list();
    ui.window().take_snapshot().unwrap();
    let mut times = Vec::new();
    for _ in 0..300 {
        let start = Instant::now();
        press(&ui, "j");
        ui.window().take_snapshot().unwrap();
        times.push(start.elapsed());
    }
    assert_eq!(app.borrow().model.borrow().cursor, 300);
    report(
        "Slint, key to frame read back, software renderer, 1280×800 at scale 2",
        &mut times,
    );
}

/// What Slint adds per strip over egui: one copy into a pixel buffer, no format change.
#[test]
#[ignore = "benchmark, run by hand: cargo test --release strip_conversion -- --ignored --nocapture"]
fn strip_conversion_cost() {
    let (width, height) = (2520, postvak::gui::html::STRIP);
    let mut times = Vec::new();
    for _ in 0..50 {
        let pixels = postvak::gui::html::Pixels::new(
            width,
            height,
            vec![128u8; (width * height * 4) as usize],
        )
        .unwrap();
        let start = Instant::now();
        crate::app::strip_image(&pixels);
        times.push(start.elapsed());
    }
    times.sort();
    println!(
        "strip {width}×{height} to a Slint image: p50 {:?}, max {:?}",
        times[25], times[49]
    );
}
