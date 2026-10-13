//! A temp Postvak home with stores and an in-memory daemon client, for the GUI tests; and `Driver`, which works the app
//! as the window would: keys through the keymap, actions, ticks, and a clock the test moves.
use std::cell::Cell;
use std::rc::Rc;
use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

use i_slint_backend_testing::ElementHandle;
use slint::ComponentHandle;

use tempfile::TempDir;

use crate::config::Config;
use crate::daemon::Client;
use crate::store::{Message, Store};
use crate::sync::{Command, Event};

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

    fn build(home: impl FnOnce(&std::path::Path) -> anyhow::Result<Home>) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let home = home(dir.path()).unwrap();
        Fixture { home, _dir: dir }
    }

    pub fn append_config(&self, text: &str) {
        self.home.append_config(text).unwrap();
    }

    pub fn store(&self, account: &str) -> Store {
        self.home.store(account).unwrap()
    }

    pub fn folder(&self, account: &str, name: &str, special_use: Option<&str>) {
        self.home.folder(account, name, special_use).unwrap();
    }

    pub fn add(&self, account: &str, message: Message) {
        self.home.add(account, message).unwrap();
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
        Live::install(app, window, Box::new(move || now.get()));
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

    pub fn window(&self) -> ui::MailWindow {
        with_live(|live| live.window.clone_strong()).unwrap()
    }

    /// Moves the window's clock and Slint's timers on by `seconds`.
    pub fn advance(&self, seconds: f64) {
        self.clock.set(self.clock.get() + seconds);
        i_slint_backend_testing::mock_elapsed_time(Duration::from_secs_f64(seconds));
    }

    /// The elements whose accessible label is `label`.
    pub fn find(&self, label: &str) -> Vec<ElementHandle> {
        ElementHandle::find_by_accessible_label(&self.window(), label).collect()
    }
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
        self.app.apply(action, self.now);
        self.tick();
        // The window focuses the search field when asked to.
        if self.app.focus_search && self.app.search.is_some() {
            self.app.apply(UiAction::SearchFocused(true), self.now);
        }
    }

    /// Presses each space-separated key in the focused pane: "down", "up", "left", "right", "tab", "escape",
    /// "delete", "backspace", "enter", "ctrl+r", or the text it types. While the search field or the move picker has
    /// the keyboard, text goes there and Enter does what it does there.
    pub fn press(&mut self, keys: &str) {
        for key in keys.split(' ') {
            if self.app.move_picker.is_some() || self.app.search_focused {
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
                text => Press::text(text),
            };
            let typing = self.app.search_focused || self.app.move_picker.is_some();
            if let Some(action) = keys::action(&press, self.app.focus, typing) {
                self.apply(action);
            }
        }
    }

    /// Text typed into the move picker's filter or the search field, as their `edited` callbacks send it.
    fn type_text(&mut self, text: &str) {
        if let Some(filter) = &self.app.move_picker {
            let filter = format!("{filter}{text}");
            self.apply(UiAction::MoveFilter(filter));
        } else if let Some(query) = &self.app.search {
            let query = format!("{query}{text}");
            self.apply(UiAction::SearchFor(query));
        }
    }

    /// Enter in the move picker moves to the first folder shown; in the search field it opens the first hit.
    fn enter(&mut self) {
        if self.app.move_picker.is_some() {
            if let Some(first) = self.app.move_targets().first() {
                self.apply(UiAction::MoveTo(first.to_string()));
            }
        } else {
            self.apply(UiAction::SelectRow(0));
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
