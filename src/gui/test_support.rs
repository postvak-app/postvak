//! A temp Postvak home with stores and an in-memory daemon client, for the GUI tests.
use std::sync::mpsc::{Receiver, Sender};

use eframe::egui;
use egui_kittest::Harness;
use tempfile::TempDir;

use crate::config::Config;
use crate::daemon::Client;
use crate::store::{Message, Store};
use crate::sync::{Command, Event};

use super::App;
use super::app::session;
use super::fixture::{self, Home};

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

    /// The app over this home, drawn headless at 1280×800; each frame advances input time by 10 ms.
    pub fn harness(&self) -> (Harness<'static, App>, Wires) {
        let config = Config::load(&self.paths.config_file()).unwrap();
        let names: Vec<&str> = self.accounts.iter().map(String::as_str).collect();
        let (client, commands, events) = Client::in_memory(&names);
        let (client, received, states) = session(client, Box::new(|| {})).unwrap();
        let mut app = App::new(&config, self.paths.clone(), client, received, states);
        app.reconnect = |_, _| Err(anyhow::anyhow!("no daemon in tests"));
        app.fetch_release = || Err(anyhow::anyhow!("no network in tests"));
        app.downloads = self.paths.cache_dir.join("downloads");
        std::fs::create_dir_all(&app.downloads).unwrap();
        let harness = Harness::builder()
            .with_size(egui::vec2(1280.0, 800.0))
            .with_step_dt(0.01)
            .build_ui_state(|ui, app: &mut App| app.show(ui), app);
        (harness, Wires { commands, events })
    }
}

pub(crate) use super::fixture::message;
