//! A temp Postvak home with stores and an in-memory daemon client, for the GUI tests; and `Driver`, which works the app
//! as the window would: keys through the keymap, actions, ticks, and a clock the test moves.
use std::cell::Cell;
use std::rc::Rc;
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use i_slint_backend_testing::ElementHandle;
use slint::ComponentHandle;
use slint::platform::WindowAdapter;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType, Rgb565Pixel};

use tempfile::TempDir;

use postvak_core::config::Config;
use postvak_core::daemon::Client;
use postvak_core::store::Message;
use postvak_core::sync::{Command, Event};

use super::fixture::{self, Home};
use super::keys::{self, Named, Press};
use super::state::{App, UiAction, session};
use super::{LIVE, Live, ui, with_live};

pub(crate) struct Fixture {
    home: Home,
    _dir: TempDir,
}

impl std::ops::Deref for Fixture {
    type Target = Home;

    fn deref(&self) -> &Home {
        &self.home
    }
}

/// The daemon's ends of the app's client: the commands it sent, and a sender for test events; dropping `events` reads
/// as the daemon going away.
pub(crate) struct Wires {
    pub commands: Receiver<(String, Command)>,
    pub events: Sender<Event>,
}

impl Wires {
    pub fn sent(&self) -> Vec<(String, Command)> {
        self.commands.try_iter().collect()
    }

    /// Sends `events` as the daemon would; the app takes them on its next tick.
    pub fn send(&self, events: impl IntoIterator<Item = Event>) {
        for event in events {
            self.events.send(event).unwrap();
        }
    }
}

impl Fixture {
    /// A config entry and an empty INBOX per account.
    pub fn new(accounts: &[&str]) -> Fixture {
        Fixture::build(|root| Home::new(root, accounts))
    }

    /// The snapshot mailbox: two accounts with a few folders and a filled inbox.
    pub fn mailbox(theme: &str) -> Fixture {
        Fixture::build(|root| fixture::mailbox(root, theme))
    }

    fn build(home: impl FnOnce(&std::path::Path) -> Home) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let home = home(dir.path());
        Fixture { home, _dir: dir }
    }

    /// Puts a directory where the account's store should be, so it fails to open.
    pub fn break_store(&self, account: &str) {
        let db = self.paths.mail_db(account);
        std::fs::remove_file(&db).unwrap();
        std::fs::create_dir_all(&db).unwrap();
    }

    /// The app over this home with no daemon behind it: what it sends shows on `Wires`, events are injected there.
    pub fn app(&self) -> (App, Wires) {
        let config = Config::load(&self.paths.config_file()).unwrap();
        let names: Vec<&str> = self.accounts.iter().map(String::as_str).collect();
        let (client, commands, events) = Client::in_memory(&names);
        let (client, received, states) = session(client, Box::new(|| {})).unwrap();
        let mut app = App::new(&config, self.paths.clone(), client, received, states);
        app.reconnect = |_, _| Err(anyhow::anyhow!("no daemon in tests"));
        app.fetch_release = || Err(anyhow::anyhow!("no network in tests"));
        app.read_open_at_login = |_| Ok(false);
        app.set_open_at_login = |_, _| Ok(String::new());
        app.ask_save_path = |_, _, _| {
            let (_, answer) = std::sync::mpsc::channel();
            answer
        };
        app.check_login = |_, _, _, _| Ok(());
        app.logos.net = std::sync::Arc::new(Offline);
        // Reloads after events apply in the tick that started them, so tests see their rows at once; the tests of
        // reloads that outlast a frame set a budget of their own.
        app.list_budget = std::time::Duration::from_secs(60);
        app.body_budget = std::time::Duration::from_secs(60);
        app.post_unsubscribe = |url| match url.contains("refuse") {
            true => Err("HTTP 500".into()),
            false => Ok(()),
        };
        app.launch = |target, _| {
            LAUNCHED.with_borrow_mut(|launched| launched.push(target.to_string_lossy().into()));
            Ok(())
        };
        app.downloads = self.paths.cache_dir.join("downloads");
        std::fs::create_dir_all(&app.downloads).unwrap();
        (app, Wires { commands, events })
    }

    /// The app over this home, worked as the window works it, with the clock at 0.
    pub fn driver(&self) -> (Driver, Wires) {
        let (app, wires) = self.app();
        let mut driver = Driver { app, now: 0.0 };
        driver.tick();
        (driver, wires)
    }
}

/// A network that is never there, so no test looks up a real sender's logo.
pub(crate) struct Offline;

impl postvak_core::bimi::Net for Offline {
    fn txt(&self, _name: &str) -> anyhow::Result<Vec<String>> {
        Err(anyhow::anyhow!("no network in tests"))
    }

    fn get(&self, _url: &str) -> anyhow::Result<Vec<u8>> {
        Err(anyhow::anyhow!("no network in tests"))
    }
}

/// A message from GitHub that passed DMARC, so it may show GitHub's logo.
pub(crate) fn branded(folder: &str, uid: u32, subject: &str) -> Message {
    Message {
        from_addr: Some("GitHub <noreply@github.com>".into()),
        headers: format!(
            "Authentication-Results: mx.example.com; dmarc=pass header.from=github.com\r\nFrom: GitHub <noreply@github.com>\r\nSubject: {subject}\r\n\r\n"
        )
        .into_bytes(),
        ..fixture::message(folder, uid, subject)
    }
}

/// A red square in the logo cache for github.com, as if fetched just now.
pub(crate) fn cache_github_logo(paths: &postvak_core::paths::Paths) {
    let brand = postvak_core::bimi::Brand {
        domain: "github.com".into(),
        selector: "default".into(),
    };
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10"><rect width="10" height="10" fill="#d1242f"/></svg>"##;
    let now = postvak_core::time::now();
    postvak_core::bimi::cache_logo(&paths.cache_dir, &brand, now, Some(svg)).unwrap();
}

thread_local! {
    static LAUNCHED: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// The files and URLs the app opened on this test thread, since the last call.
pub(crate) fn launched() -> Vec<String> {
    LAUNCHED.with_borrow_mut(std::mem::take)
}

/// This test thread's headless Slint: mock time and the software renderer, so text has real metrics from the bundled
/// fonts and snapshots draw. Each test runs on its own thread, so each sets it once.
pub(crate) fn headless() {
    use i_slint_backend_testing::{TestingBackend, TestingBackendOptions};
    let backend = TestingBackend::new(TestingBackendOptions {
        mock_time: true,
        threading: false,
        renderer_name: Some("software".into()),
    });
    let _ = slint::platform::set_platform(Box::new(backend));
}

/// Like `headless`, but the window is Slint's `MinimalSoftwareWindow`, which keeps the redraws the testing backend
/// drops; `drawn` draws it when something asked for that, and says whether anything did.
pub(crate) fn redraws() {
    struct Platform(i_slint_backend_testing::TestingBackend);
    impl slint::platform::Platform for Platform {
        fn create_window_adapter(
            &self,
        ) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
            let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
            DRAWN.with_borrow_mut(|slot| *slot = Some(window.clone()));
            Ok(window)
        }
        fn duration_since_start(&self) -> Duration {
            self.0.duration_since_start()
        }
    }
    use i_slint_backend_testing::{TestingBackend, TestingBackendOptions};
    let backend = TestingBackend::new(TestingBackendOptions {
        mock_time: true,
        threading: false,
        renderer_name: None,
    });
    slint::platform::set_platform(Box::new(Platform(backend))).unwrap();
}

thread_local! {
    static DRAWN: std::cell::RefCell<Option<Rc<MinimalSoftwareWindow>>> = const { std::cell::RefCell::new(None) };
}

/// Draws the window `redraws` made if anything asked for a redraw since the last time; whether it did.
pub(crate) fn drawn() -> bool {
    let window = DRAWN.with_borrow(|slot| slot.clone()).unwrap();
    window.draw_if_needed(|renderer| {
        let size = window.window().size();
        let mut pixels = vec![Rgb565Pixel::default(); (size.width * size.height) as usize];
        renderer.render(&mut pixels, size.width as usize);
    })
}

impl Fixture {
    /// The real window over this home, 1280×800 under the headless backend, with the clock at 0.
    pub fn window(&self) -> (Open, Wires) {
        headless();
        let (app, wires) = self.app();
        let window = ui::MailWindow::new().unwrap();
        window
            .window()
            .set_size(slint::LogicalSize::new(1280.0, 800.0));
        window.show().unwrap();
        let clock = Rc::new(Cell::new(0.0));
        let now = clock.clone();
        Live::install(app, window, None, None, Box::new(move || now.get()));
        (Open { clock }, wires)
    }
}

/// The window opened by `Fixture::window`, until it is dropped.
pub(crate) struct Open {
    clock: Rc<Cell<f64>>,
}

impl Open {
    /// Reads or changes the app and window; the window is shown again afterwards, as after any callback.
    pub fn with<R>(&self, f: impl FnOnce(&mut Live) -> R) -> R {
        let result = with_live(f).unwrap();
        with_live(Live::tick).unwrap();
        result
    }

    /// Sends the daemon's `events`, then ticks so the window takes them.
    pub fn receive(&self, wires: &Wires, events: impl IntoIterator<Item = Event>) {
        wires.send(events);
        self.tick();
    }

    /// Ticks until `done` holds; for answers from other threads.
    pub fn tick_until(&self, done: impl Fn(&Open) -> bool) {
        wait_until(|| {
            done(self) || {
                self.tick();
                false
            }
        });
    }

    /// Takes what the daemon sent, as when the window is woken. Visiting the elements then makes Slint build what the
    /// new state shows, as drawing does in the running window; their `init` callbacks are answered on the next tick.
    pub fn tick(&self) {
        with_live(Live::tick).unwrap();
        let _ = ElementHandle::find_by_element_id(&self.window(), "none");
        // Runs `changed` handlers, as the event loop does.
        i_slint_backend_testing::mock_elapsed_time(Duration::ZERO);
        with_live(Live::tick).unwrap();
    }

    pub fn window(&self) -> ui::MailWindow {
        with_live(|live| live.window.clone_strong()).unwrap()
    }

    /// Moves the window's clock and Slint's timers on by `seconds`.
    pub fn advance(&self, seconds: f64) {
        self.clock.set(self.clock.get() + seconds);
        i_slint_backend_testing::mock_elapsed_time(Duration::from_secs_f64(seconds));
    }

    /// The window drawn by the software renderer, as a PNG.
    pub fn png(&self) -> Vec<u8> {
        let shot = self.window().window().take_snapshot().unwrap();
        let mut png = Vec::new();
        let mut encoder = png::Encoder::new(&mut png, shot.width(), shot.height());
        encoder.set_color(png::ColorType::Rgba);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(shot.as_bytes()).unwrap();
        writer.finish().unwrap();
        png
    }

    /// The elements whose accessible label is `label`.
    pub fn find(&self, label: &str) -> Vec<ElementHandle> {
        // What a screen reader sees: an element hidden from it can still carry the label, as a component that inherits
        // `Text` with `accessible-role: none` does.
        let shown = |e: &ElementHandle| {
            e.accessible_role() != Some(i_slint_backend_testing::AccessibleRole::None)
        };
        once(
            ElementHandle::find_by_accessible_label(&self.window(), label)
                .filter(shown)
                .collect(),
        )
    }

    /// The labels that contain `text`.
    pub fn labels_containing(&self, text: &str) -> Vec<String> {
        let text = text.to_string();
        let found = i_slint_backend_testing::ElementQuery::from_root(&self.window())
            .match_descendants()
            .match_predicate(move |e| {
                let shown = e
                    .accessible_role()
                    .is_some_and(|role| role != i_slint_backend_testing::AccessibleRole::None);
                shown && e.accessible_label().is_some_and(|l| l.contains(&text))
            })
            .find_all();
        once(found)
            .into_iter()
            .filter_map(|e| e.accessible_label().map(String::from))
            .collect()
    }

    /// The one element labelled `label`.
    pub fn get(&self, label: &str) -> ElementHandle {
        let mut found = self.find(label);
        assert_eq!(found.len(), 1, "elements labelled {label:?}");
        found.remove(0)
    }

    /// Clicks the one element labelled `label`, as a screen reader would.
    pub fn click(&self, label: &str) {
        self.get(label).invoke_accessible_default_action();
    }

    /// Clicks the middle of the one element labelled `label` with the mouse, for elements without an accessible action.
    pub fn tap(&self, label: &str) {
        self.press_on(label, slint::platform::PointerEventButton::Left);
    }

    /// Right-clicks the middle of the one element labelled `label`.
    pub fn right_tap(&self, label: &str) {
        self.press_on(label, slint::platform::PointerEventButton::Right);
    }

    /// Moves the mouse over the middle of the one element labelled `label`, without clicking.
    pub fn hover(&self, label: &str) {
        self.window()
            .window()
            .dispatch_event(slint::platform::WindowEvent::PointerMoved {
                position: self.middle(label),
            });
        self.tick();
    }

    fn middle(&self, label: &str) -> slint::LogicalPosition {
        let element = self.get(label);
        let (at, size) = (element.absolute_position(), element.size());
        slint::LogicalPosition::new(at.x + size.width / 2.0, at.y + size.height / 2.0)
    }

    fn press_on(&self, label: &str, button: slint::platform::PointerEventButton) {
        use slint::platform::WindowEvent;
        let position = self.middle(label);
        let window = self.window();
        window
            .window()
            .dispatch_event(WindowEvent::PointerMoved { position });
        window
            .window()
            .dispatch_event(WindowEvent::PointerPressed { position, button });
        window
            .window()
            .dispatch_event(WindowEvent::PointerReleased { position, button });
        self.tick();
    }

    /// Drags the one element labelled `from` with the mouse and drops it on the one labelled `to`.
    pub fn drag(&self, from: &str, to: &str) {
        let to = self.drag_over(from, to);
        self.window()
            .window()
            .dispatch_event(slint::platform::WindowEvent::PointerReleased {
                position: to,
                button: slint::platform::PointerEventButton::Left,
            });
        self.tick();
    }

    /// Drags the one element labelled `from` with the mouse over the one labelled `to`, and holds it there; returns
    /// where the pointer is.
    pub fn drag_over(&self, from: &str, to: &str) -> slint::LogicalPosition {
        use slint::platform::{PointerEventButton, WindowEvent};
        let middle = |label: &str| {
            let element = self.get(label);
            let (at, size) = (element.absolute_position(), element.size());
            slint::LogicalPosition::new(at.x + size.width / 2.0, at.y + size.height / 2.0)
        };
        let (from, to) = (middle(from), middle(to));
        let button = PointerEventButton::Left;
        let window = self.window();
        let send = |event| window.window().dispatch_event(event);
        send(WindowEvent::PointerMoved { position: from });
        send(WindowEvent::PointerPressed {
            position: from,
            button,
        });
        // The first move starts the drag, the second hovers the target.
        send(WindowEvent::PointerMoved { position: to });
        send(WindowEvent::PointerMoved { position: to });
        self.tick();
        to
    }

    /// The top and bottom of the one element labelled `label`, in window coordinates.
    pub fn span(&self, label: &str) -> (f32, f32) {
        let element = self.get(label);
        let top = element.absolute_position().y;
        (top, top + element.size().height)
    }

    /// Presses each space-separated key, as `Driver::press` spells them, on the window.
    pub fn press(&self, keys: &str) {
        use slint::platform::{Key, WindowEvent};
        let window = self.window();
        for key in keys.split(' ') {
            let text: slint::SharedString = match key {
                "down" => Key::DownArrow.into(),
                "up" => Key::UpArrow.into(),
                "left" => Key::LeftArrow.into(),
                "right" => Key::RightArrow.into(),
                "tab" => Key::Tab.into(),
                "escape" => Key::Escape.into(),
                "delete" => Key::Delete.into(),
                "backspace" => Key::Backspace.into(),
                "enter" => Key::Return.into(),
                text => text.into(),
            };
            window
                .window()
                .dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
            window
                .window()
                .dispatch_event(WindowEvent::KeyReleased { text });
        }
    }
}

/// Each element once: the query visits the rows of a `ListView` several times over. Two elements with the same label
/// at the same spot count as one too, so `get` cannot tell those apart.
pub(crate) fn once(elements: Vec<ElementHandle>) -> Vec<ElementHandle> {
    let mut seen = Vec::new();
    let mut unique = Vec::new();
    for element in elements {
        let position = element.absolute_position();
        let key = (
            element.accessible_label(),
            position.x.to_bits(),
            position.y.to_bits(),
        );
        if !seen.contains(&key) {
            seen.push(key);
            unique.push(element);
        }
    }
    unique
}

impl Drop for Open {
    fn drop(&mut self) {
        LIVE.with_borrow_mut(Option::take);
    }
}

/// The app as the window drives it, without a window: keys go through the keymap, every action is followed by a tick,
/// and time moves only when the test says so.
pub(crate) struct Driver {
    pub app: App,
    /// Seconds on the window's clock.
    pub now: f64,
}

impl Driver {
    /// Events in, timers, reloads and the shown body, as when the window is woken.
    pub fn tick(&mut self) {
        self.app.tick(self.now);
    }

    pub fn apply(&mut self, action: UiAction) {
        self.app.apply(action);
        self.tick();
    }

    /// Sends the daemon's `events`, then ticks so the app takes them.
    pub fn receive(&mut self, wires: &Wires, events: impl IntoIterator<Item = Event>) {
        wires.send(events);
        self.tick();
    }

    /// Ticks until `done` holds; for answers from other threads.
    pub fn tick_until(&mut self, done: impl Fn(&App) -> bool) {
        wait_until(|| {
            done(&self.app) || {
                self.tick();
                false
            }
        });
    }

    /// Presses each space-separated key in the focused pane: "down", "up", "left", "right", "tab", "escape",
    /// "delete", "backspace", "enter", "ctrl+r", or the text it types. While the search field or the move picker has
    /// the keyboard, text goes there and Enter does what it does there.
    pub fn press(&mut self, keys: &str) {
        for key in keys.split(' ') {
            if self.app.typing() {
                match key {
                    "enter" => {
                        self.enter();
                        continue;
                    }
                    "escape" | "ctrl+r" => {}
                    text => {
                        self.type_text(text);
                        continue;
                    }
                }
            }
            let press = match key {
                "down" => Press::named(Named::Down),
                "up" => Press::named(Named::Up),
                "left" => Press::named(Named::Left),
                "right" => Press::named(Named::Right),
                "tab" => Press::named(Named::Tab),
                "escape" => Press::named(Named::Escape),
                "delete" => Press::named(Named::Delete),
                "backspace" => Press::named(Named::Backspace),
                "ctrl+r" => Press {
                    command: true,
                    ..Press::text("r")
                },
                "ctrl+a" => Press {
                    command: true,
                    ..Press::text("a")
                },
                text => Press::text(text),
            };
            if let Some(action) = keys::action(&press, self.app.focus, self.app.typing()) {
                self.apply(action);
            }
        }
    }

    /// Text typed into the folder dialog's name, the move picker's filter or the search field, as their `edited`
    /// callbacks send it.
    fn type_text(&mut self, text: &str) {
        if let Some(dialog) = &self.app.folder_dialog {
            let name = format!("{}{text}", dialog.name);
            self.apply(UiAction::FolderName(name));
        } else if let Some(filter) = &self.app.move_picker {
            let filter = format!("{filter}{text}");
            self.apply(UiAction::MoveFilter(filter));
        } else if let Some(query) = &self.app.search {
            let query = format!("{query}{text}");
            self.apply(UiAction::SearchFor(query));
        }
    }

    /// Enter in the folder dialog's name confirms it; in the move picker it moves to the first folder shown; in the
    /// search field it opens the first hit.
    fn enter(&mut self) {
        if self.app.folder_dialog.is_some() {
            self.apply(UiAction::FolderConfirm(None));
        } else if self.app.move_picker.is_some() {
            if let Some(first) = self.app.move_targets().first() {
                self.apply(UiAction::MoveTo(first.to_string()));
            }
        } else {
            self.apply(UiAction::SearchAccepted);
        }
    }

    /// The status bar's lines: one per account, then the window's own error.
    pub fn status(&self) -> Vec<String> {
        let accounts = self.app.accounts.iter().map(super::status::account_line);
        accounts.chain(self.app.error.clone()).collect()
    }

    /// Moves the clock to `at` seconds and ticks.
    pub fn at(&mut self, at: f64) {
        self.now = at;
        self.tick();
    }

    /// The rows' (account, uid), top to bottom.
    pub fn ids(&self) -> Vec<(usize, u32)> {
        self.app
            .list
            .rows
            .iter()
            .map(|r| (r.account, r.uid))
            .collect()
    }

    /// The rows' uids, top to bottom.
    pub fn uids(&self) -> Vec<u32> {
        self.app.list.rows.iter().map(|r| r.uid).collect()
    }
}

pub(crate) use super::fixture::message;

/// A message that has not been read.
pub(crate) fn unread(folder: &str, uid: u32, subject: &str) -> Message {
    let mut message = message(folder, uid, subject);
    message.flags = String::new();
    message
}

/// How long a test waits for an answer from another thread before it fails; generous, since a loaded machine renders
/// HTML slowly.
const WAIT: Duration = Duration::from_secs(30);

/// Calls `ready` every few milliseconds until it holds, and fails the test after `WAIT`. `ready` ticks whatever has to
/// take the answer in.
pub(crate) fn wait_until(mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !ready() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for another thread"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// What the window sends to the daemon for `action` on `uids` of `folder` in `account`.
pub(crate) fn command(
    account: &str,
    folder: &str,
    uids: &[u32],
    action: postvak_core::rules::Action,
) -> (String, Command) {
    let apply = Command::Apply {
        folder: folder.into(),
        uids: uids.to_vec(),
        action,
        by: "gui".into(),
    };
    (account.into(), apply)
}

/// `command` for the "work" account, the one most tests use.
pub(crate) fn work(
    folder: &str,
    uids: &[u32],
    action: postvak_core::rules::Action,
) -> (String, Command) {
    command("work", folder, uids, action)
}

/// The daemon's word that `account` finished a sync that found `new_messages`.
pub(crate) fn synced(account: &str, new_messages: usize) -> Event {
    Event::Synced {
        account: account.into(),
        new_messages,
        actions: 0,
        requests: vec![],
        errors: vec![],
    }
}

/// A 1×1 PNG, for the `cid:` image.
const DOT: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFBQIAX8jx0gAAAABJRU5ErkJggg==";

/// A raw message whose HTML is `html`, with a `cid:dot@example.com` image beside it.
pub(crate) fn raw(html: &str) -> Vec<u8> {
    format!(
        "From: news@example.org\r\nSubject: news\r\nMIME-Version: 1.0\r\n\
Content-Type: multipart/related; boundary=\"b\"\r\n\r\n\
--b\r\nContent-Type: text/html; charset=utf-8\r\n\r\n{html}\r\n\
--b\r\nContent-Type: image/png\r\nContent-ID: <dot@example.com>\r\nContent-Transfer-Encoding: base64\r\n\r\n{DOT}\r\n--b--\r\n"
    )
    .into_bytes()
}

/// Adds message `uid` to work's INBOX with `html` as its body and `text` as its extracted text.
pub(crate) fn add_html(fx: &Fixture, uid: u32, html: &str, text: &str) {
    let mut m = message("INBOX", uid, &format!("news {uid}"));
    m.body_text = Some(text.into());
    fx.add("work", m);
    fx.store("work")
        .set_raw("INBOX", uid, &raw(html), text)
        .unwrap();
}
