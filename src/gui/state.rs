//! App state and the only code that changes it: `handle` for daemon events, `apply` for what the window asks, `tick`
//! for time. No toolkit here: `bind.rs` shows this state in the Slint window and turns its callbacks into `UiAction`s.
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::SystemTime;

use crate::config::{self, Config, Theme};
use crate::daemon::Client;
use crate::daemon::client::NewerDaemon;
use crate::daemon::wire::AccountStatus;
use crate::message::{self, Attachment, HtmlBody, clean};
use crate::paths::Paths;
use crate::rules::{self as rule_file, Action, RulesError};
use crate::store::{LogEntry, Message, Store, StoreError, has_flag};
use crate::sync::{Activity, Command, Event};
use crate::update;

use super::body;
use super::folders;
use super::html::{self, HtmlState, Renderer, Reply, Wake};
use super::list::{self, ListState, Optimistic, Row, RowKey, THREAD_LIMIT};
use super::rules::{self, RulesState, TrashRow};

/// The status line when `m` is pressed with marked rows from more than one account.
pub(crate) const MOVE_ONE_ACCOUNT: &str =
    "move works on one account at a time; mark messages from one account";

/// The status line while the daemon is gone.
pub(crate) const DAEMON_LOST: &str = "background sync stopped — reconnecting";

/// How `NewerDaemon` reads on the status line, which a reconnect clears.
const NEWER_DAEMON: &str = "the daemon is version ";

/// Lines kept for the history window.
const HISTORY: usize = 50;

/// Seconds between checks of rules.toml and config.toml for outside edits.
pub(crate) const POLL: f64 = 2.0;

/// How long a message stays on screen before it is marked read.
pub(crate) const READ_DELAY: f64 = 1.0;

/// Seconds between attempts to reach the daemon again.
const RECONNECT: f64 = 5.0;

/// A daemon client, its event subscription, and each account's state and activity when it connected.
pub(crate) type Connection = (Client, Receiver<Event>, Vec<AccountStatus>);

/// Called after each daemon event and once the daemon hung up, so an idle window still shows them.
pub(crate) type Waker = Box<dyn Fn() + Send>;

/// Connects to the daemon, starting it when none answers.
pub(crate) fn connect(paths: &Paths, wake: Waker) -> anyhow::Result<Connection> {
    session(Client::connect_or_start(paths)?, wake)
}

/// Subscribes before asking for the states, so no event between the two is lost.
pub(crate) fn session(client: Client, wake: Waker) -> anyhow::Result<Connection> {
    let events = client.subscribe_waking(wake)?;
    let states = client.status()?.accounts;
    Ok((client, events, states))
}

pub(crate) struct BodyState {
    pub account: usize,
    /// Set when the user chose this message, not when a view change or reload put it on screen; only then does it
    /// become read.
    pub armed: bool,
    pub attachments: Vec<Attachment>,
    /// The message's HTML, when it has some and this build renders it.
    pub html: Option<HtmlBody>,
    /// The HTML view, once the panel asked for a layout; `None` while the text view shows.
    pub html_view: Option<HtmlState>,
    pub key: RowKey,
    pub message: Option<Message>,
    /// Set once mark-read was sent, or the user marked read or unread by hand, so the delay does not act again.
    pub read_sent: bool,
    pub saved: Option<String>,
    pub shown_at: f64,
    /// Set by `v`: the text view for this message even though it has HTML.
    pub show_text: bool,
    /// The raw message, while the source window shows it.
    pub source: Option<body::Source>,
    /// Why the HTML could not be shown; the text view shows instead, under this note.
    pub text_note: Option<&'static str>,
    /// The body text cleaned once on load, since the panel draws it every frame.
    pub text: Option<String>,
}

impl BodyState {
    /// True when the panel shows the HTML view rather than the text.
    pub fn shows_html(&self) -> bool {
        self.html.is_some() && !self.show_text && self.text_note.is_none()
    }
}

pub(crate) struct HistoryLine {
    pub at: i64,
    pub text: String,
}

pub(crate) struct Account {
    pub activity: Option<Activity>,
    pub error: Option<String>,
    pub folders: Vec<FolderRow>,
    pub name: String,
    /// Edits of sent commands per row, oldest first; each `ActionDone` removes the oldest of its uids.
    pub pending: HashMap<RowKey, Vec<Optimistic>>,
    pub queued: usize,
    pub store: Result<Store, String>,
}

impl Account {
    /// True while the sync thread works, for the spinner beside the account.
    pub fn busy(&self) -> bool {
        !matches!(
            self.activity,
            None | Some(
                Activity::Idle { .. } | Activity::NotRunning { .. } | Activity::Offline { .. }
            )
        )
    }

    pub fn reload_folders(&mut self) {
        let Ok(store) = &self.store else { return };
        match folder_rows(store, &self.pending) {
            Ok(rows) => self.folders = rows,
            Err(e) => log::warn!("[{}] could not list folders: {e}", self.name),
        }
    }
}

/// The folders with their unread counts as the pending edits will leave them.
fn folder_rows(
    store: &Store,
    pending: &HashMap<RowKey, Vec<Optimistic>>,
) -> Result<Vec<FolderRow>, StoreError> {
    let mut delta: HashMap<&str, i64> = HashMap::new();
    for ((folder, uid), edits) in pending {
        let Some(stored) = store.message(folder, *uid)? else {
            continue;
        };
        let was_unread = !has_flag(&stored.flags, "\\Seen");
        let unread = edits.iter().fold(was_unread, |unread, edit| match edit {
            Optimistic::Hidden => false,
            Optimistic::Seen(seen) => !seen,
            Optimistic::Flagged(_) => unread,
        });
        *delta.entry(folder).or_default() += i64::from(unread) - i64::from(was_unread);
    }
    let mut folders = store.folders()?;
    folders::sort(&mut folders);
    folders
        .into_iter()
        .map(|f| {
            let unread =
                i64::from(store.unread_count(&f.name)?) + delta.get(f.name.as_str()).unwrap_or(&0);
            Ok(FolderRow {
                delimiter: f.delimiter,
                special_use: f.special_use,
                unread: u32::try_from(unread).unwrap_or(0),
                name: f.name,
            })
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FolderRow {
    pub delimiter: Option<String>,
    pub name: String,
    pub special_use: Option<String>,
    pub unread: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum View {
    Activity,
    /// Every account's INBOX in one list.
    AllInboxes,
    Folder {
        account: usize,
        folder: String,
    },
    Rules,
    Trash,
}

impl View {
    /// The views with a message list and a reader.
    pub(crate) fn lists_mail(&self) -> bool {
        matches!(self, View::AllInboxes | View::Folder { .. })
    }
}

/// What the window asks for; `App::apply` carries it out.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum UiAction {
    Act(Action),
    ApproveRule(String),
    Collapse,
    CloseSource,
    Escape,
    Expand,
    /// The pointer over the HTML page, in page points, or `None` once it left.
    HtmlHover(Option<[f32; 2]>),
    /// Select the HTML page's text between two points, in page points.
    HtmlSelect {
        from: [f32; 2],
        to: [f32; 2],
    },
    /// The part of the HTML page on screen, in page points.
    HtmlVisible {
        top: f32,
        bottom: f32,
    },
    /// The width (points) and scale the HTML view would lay out at now.
    HtmlWidth {
        scale: f32,
        width: f32,
    },
    ListViewport {
        offset: f32,
        height: f32,
    },
    MarkFolderRead(usize, String),
    MoveCursor(isize),
    MoveFilter(String),
    MoveTo(String),
    NextFocus,
    OpenMovePicker,
    OpenRulesFile,
    OpenSource,
    RejectRule(String),
    Restore(usize, PathBuf),
    SaveAttachment(usize),
    SaveEml,
    SearchFocused,
    SearchFor(String),
    SelectRow(usize),
    SelectView(View),
    SetRuleEnabled(String, bool),
    SetTheme(Theme),
    StartSearch,
    StepFolder(isize),
    SyncNow,
    ToggleFlag,
    ToggleFolder(usize, String),
    ToggleHelp,
    ToggleHistory,
    ToggleHtml,
    ToggleMark,
    ToggleRead,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Focus {
    Body,
    Folders,
    List,
}

/// The release check: off in config, waiting for the first frame, running on its thread, or done with the version to
/// announce.
pub(crate) enum UpdateCheck {
    Off,
    Waiting,
    Running(Receiver<Option<String>>),
    Done(Option<String>),
}

pub struct App {
    pub(crate) accounts: Vec<Account>,
    pub(crate) activity_log: Vec<(String, LogEntry)>,
    pub(crate) body: Option<BodyState>,
    pub(crate) client: Client,
    /// Folder tree branches folded shut, by account index and path; kept for the window's lifetime.
    pub(crate) collapsed: HashSet<(usize, String)>,
    pub(crate) config_changed: bool,
    pub(crate) config_mtime: Option<SystemTime>,
    /// Set when the event stream ended; cleared once a reconnect succeeds.
    pub(crate) daemon_lost: bool,
    pub(crate) downloads: PathBuf,
    pub(crate) error: Option<String>,
    pub(crate) events: Receiver<Event>,
    pub(crate) fetch_release: fn() -> anyhow::Result<String>,
    pub(crate) focus: Focus,
    pub(crate) focus_search: bool,
    pub(crate) help_open: bool,
    /// Numbers HTML layouts, so a reply for an older one is dropped.
    pub(crate) html_generation: u64,
    pub(crate) history: VecDeque<HistoryLine>,
    pub(crate) history_open: bool,
    pub(crate) last_poll: f64,
    pub(crate) last_reconnect: f64,
    pub(crate) list: ListState,
    pub(crate) move_picker: Option<String>,
    pub(crate) paths: Paths,
    /// Set by the user's own navigation; the next body `sync_body` loads is armed.
    pub(crate) pending_arm: bool,
    /// The HTML render thread, started for the first HTML message.
    pub(crate) renderer: Option<Renderer>,
    pub(crate) reconnect: fn(&Paths, Waker) -> anyhow::Result<Connection>,
    /// The result of the reconnect attempt in flight; at most one runs.
    pub(crate) reconnecting: Option<Receiver<anyhow::Result<Connection>>>,
    pub(crate) requested: HashSet<(usize, RowKey)>,
    pub(crate) rules: RulesState,
    pub(crate) rules_mtime: Option<SystemTime>,
    pub(crate) search: Option<String>,
    /// Set by the window while the search field has the keyboard, so the list is not opened while a query is typed.
    pub(crate) search_focused: bool,
    /// The earliest time something here wants `tick` again (the read delay, a settling width, polling); the window
    /// sets a timer for it.
    pub(crate) next_tick: Option<f64>,
    /// Wakes the window from another thread (a reconnect, the release check, the HTML renderer).
    pub(crate) wake: Wake,
    pub(crate) theme: Theme,
    pub(crate) trash: Vec<TrashRow>,
    pub(crate) update: UpdateCheck,
    pub(crate) view: View,
    pub(crate) view_dirty: bool,
}

impl App {
    pub fn new(
        config: &Config,
        paths: Paths,
        client: Client,
        events: Receiver<Event>,
        states: Vec<AccountStatus>,
    ) -> App {
        let accounts = states
            .into_iter()
            .map(|AccountStatus { activity, name }| {
                let store = Store::open_account(&paths, &name)
                    .map_err(|e| format!("could not open the store: {e}"));
                let mut account = Account {
                    activity,
                    error: None,
                    folders: Vec::new(),
                    name,
                    pending: HashMap::new(),
                    queued: 0,
                    store,
                };
                account.reload_folders();
                account
            })
            .collect();
        let rules_path = paths.rules_file();
        let rules = RulesState::load(&rules_path, Vec::new());
        let (config_mtime, rules_mtime) = (mtime(&paths.config_file()), mtime(&rules_path));
        let mut app = App {
            accounts,
            activity_log: Vec::new(),
            body: None,
            client,
            collapsed: HashSet::new(),
            config_changed: false,
            config_mtime,
            daemon_lost: false,
            downloads: downloads_dir(),
            error: None,
            events,
            fetch_release: update::fetch,
            focus: Focus::List,
            focus_search: false,
            help_open: false,
            html_generation: 0,
            history: VecDeque::new(),
            history_open: false,
            last_poll: 0.0,
            last_reconnect: 0.0,
            list: ListState::default(),
            move_picker: None,
            paths,
            pending_arm: false,
            reconnect: connect,
            renderer: None,
            reconnecting: None,
            requested: HashSet::new(),
            rules,
            rules_mtime,
            search: None,
            search_focused: false,
            next_tick: None,
            wake: Arc::new(|| {}),
            theme: config.ui.theme,
            trash: Vec::new(),
            update: if config.ui.check_updates {
                UpdateCheck::Waiting
            } else {
                UpdateCheck::Off
            },
            view: View::Folder {
                account: 0,
                folder: "INBOX".into(),
            },
            view_dirty: false,
        };
        app.reload_view();
        app
    }

    /// Everything that happens with time: daemon events and render replies in, reconnects, the release check, reloads,
    /// file polling, the shown body and its read delay. The window calls it when woken, on its timer and after input.
    pub fn tick(&mut self, now: f64) {
        self.receive(now);
        self.receive_html();
        self.keep_daemon(now);
        self.check_release();
        if std::mem::take(&mut self.view_dirty) {
            self.reload_view();
        }
        self.poll_files(now);
        let typing_search = self.focus_search || self.search_focused;
        self.sync_body(now, typing_search);
        self.mark_read_after_delay(now);
        self.request_tick(self.last_poll + POLL);
        if self.daemon_lost {
            self.request_tick(self.last_reconnect + RECONNECT);
        }
    }

    /// Asks for `tick` at `at` (seconds on the window's clock) at the latest.
    pub(crate) fn request_tick(&mut self, at: f64) {
        self.next_tick = Some(self.next_tick.map_or(at, |next| next.min(at)));
    }

    /// When the window should call `tick` next; it takes the time and the request starts over.
    pub fn take_next_tick(&mut self) -> Option<f64> {
        self.next_tick.take()
    }

    fn receive(&mut self, now: f64) {
        while !self.daemon_lost {
            match self.events.try_recv() {
                Ok(event) => self.handle(event, now),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.daemon_lost = true;
                    self.last_reconnect = now;
                    self.note_error(None, DAEMON_LOST.into());
                }
            }
        }
    }

    /// While the daemon is gone: takes the result of the attempt in flight, or starts one every 5 s.
    fn keep_daemon(&mut self, now: f64) {
        if !self.daemon_lost {
            return;
        }
        let Some(attempt) = &self.reconnecting else {
            if now - self.last_reconnect >= RECONNECT {
                self.last_reconnect = now;
                self.reconnecting = Some(self.start_reconnect());
            }
            return;
        };
        match attempt.try_recv() {
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => self.reconnecting = None,
            Ok(Err(e)) => {
                log::warn!("could not reach the daemon: {e:#}");
                self.reconnecting = None;
                if let Some(newer) = e.downcast_ref::<NewerDaemon>() {
                    let text = newer.to_string();
                    if self.error.as_deref() != Some(text.as_str()) {
                        self.note_error(None, text);
                    }
                }
            }
            Ok(Ok(connection)) => {
                self.reconnecting = None;
                self.reconnected(connection);
            }
        }
    }

    /// Connects on a background thread so the window never waits; the window is woken with the result.
    fn start_reconnect(&self) -> Receiver<anyhow::Result<Connection>> {
        let (done, attempt) = mpsc::channel();
        let (reconnect, paths, wake) = (self.reconnect, self.paths.clone(), self.wake.clone());
        std::thread::spawn(move || {
            let events_wake = wake.clone();
            let connection = reconnect(&paths, Box::new(move || events_wake()));
            let _ = done.send(connection);
            wake();
        });
        attempt
    }

    /// Starts the release check at the first tick, after the test fixture has swapped `fetch_release`, then takes its
    /// answer.
    fn check_release(&mut self) {
        let next = match &self.update {
            UpdateCheck::Waiting => {
                let (done, answer) = mpsc::channel();
                let (cache, fetch, wake) =
                    (self.paths.update_check(), self.fetch_release, self.wake.clone());
                std::thread::spawn(move || {
                    let _ = done.send(update::check(
                        &cache,
                        crate::time::now(),
                        env!("CARGO_PKG_VERSION"),
                        fetch,
                    ));
                    wake();
                });
                Some(UpdateCheck::Running(answer))
            }
            UpdateCheck::Running(answer) => match answer.try_recv() {
                Ok(newer) => Some(UpdateCheck::Done(newer)),
                Err(TryRecvError::Disconnected) => Some(UpdateCheck::Done(None)),
                Err(TryRecvError::Empty) => None,
            },
            UpdateCheck::Off | UpdateCheck::Done(_) => None,
        };
        if let Some(next) = next {
            self.update = next;
        }
    }

    /// The newer release to announce, once the check has found one.
    pub(crate) fn newer_release(&self) -> Option<&str> {
        match &self.update {
            UpdateCheck::Done(newer) => newer.as_deref(),
            _ => None,
        }
    }

    /// Commands sent before the loss may never be answered, so their edits go and the view reloads from the store, which
    /// the daemon may have written meanwhile.
    fn reconnected(&mut self, (client, events, states): Connection) {
        self.client = client;
        self.events = events;
        self.daemon_lost = false;
        if self
            .error
            .as_deref()
            .is_some_and(|error| error == DAEMON_LOST || error.starts_with(NEWER_DAEMON))
        {
            self.error = None;
        }
        self.requested.clear();
        for account in &mut self.accounts {
            if let Some(status) = states.iter().find(|status| status.name == account.name) {
                account.activity = status.activity.clone();
            }
            if account.error.as_deref() == Some(DAEMON_LOST) {
                account.error = None;
            }
            account.pending.clear();
            account.queued = 0;
            account.reload_folders();
        }
        self.push_history("background sync reconnected".into());
        self.view_dirty = true;
    }

    fn handle(&mut self, event: Event, now: f64) {
        let name = match &event {
            Event::Activity { account, .. }
            | Event::ActionDone { account, .. }
            | Event::BodiesFetched { account, .. }
            | Event::BodyReady { account, .. }
            | Event::CommandFailed { account, .. }
            | Event::Error { account, .. }
            | Event::NewMail { account, .. }
            | Event::Restored { account, .. }
            | Event::RuleApplied { account, .. }
            | Event::Synced { account, .. } => account,
        };
        let Some(index) = self.accounts.iter().position(|a| &a.name == name) else {
            return;
        };
        match event {
            Event::Activity { activity, .. } => {
                // Progress lives on the status line; ticks would evict errors from the history.
                if !matches!(
                    activity,
                    Activity::FetchingBodies { .. } | Activity::FetchingHeaders { .. }
                ) {
                    let line = format!("{}: {activity}", self.accounts[index].name);
                    self.push_history(line);
                }
                let account = &mut self.accounts[index];
                // Offline follows the Error that caused it and must not wipe it.
                if !matches!(activity, Activity::Offline { .. }) {
                    account.error = None;
                }
                account.activity = Some(activity);
            }
            Event::NewMail { .. } => {}
            // Request 0 is this window's own outcome. A daemon id is another client's, or the broadcast copy of our own
            // failure; the client drops the copy of our own `ActionDone`.
            Event::CommandFailed { request, .. } if request != 0 => {}
            Event::ActionDone { request, .. } if request != 0 => self.refresh(index),
            Event::CommandFailed { message, .. } => {
                // A refusal carries no uids, so every edit of the account goes and the store shows what is true.
                let account = &mut self.accounts[index];
                account.pending.clear();
                account.queued = 0;
                self.note_error(Some(index), message);
                self.refresh(index);
            }
            Event::Error { message, .. } => self.note_error(Some(index), message),
            Event::BodiesFetched { .. } | Event::RuleApplied { .. } | Event::Synced { .. } => {
                self.refresh(index)
            }
            Event::ActionDone {
                folder, results, ..
            } => {
                let account = &mut self.accounts[index];
                account.queued = account.queued.saturating_sub(1);
                for (uid, _) in &results {
                    let key = (folder.clone(), *uid);
                    if let Some(edits) = account.pending.get_mut(&key) {
                        edits.remove(0);
                        if edits.is_empty() {
                            account.pending.remove(&key);
                        }
                    }
                }
                let failed: Vec<&String> = results
                    .iter()
                    .filter_map(|(_, result)| result.as_ref().err())
                    .collect();
                if let Some(first) = failed.first() {
                    let line = format!(
                        "{} of {} messages failed: {first}",
                        failed.len(),
                        results.len()
                    );
                    self.note_error(Some(index), line);
                }
                self.refresh(index);
            }
            Event::BodyReady { folder, uid, .. } => self.reload_body(index, (folder, uid), now),
            Event::Restored { folder, .. } => {
                let line = format!(
                    "{}: restored to {}",
                    self.accounts[index].name,
                    clean(&folder, false)
                );
                self.push_history(line);
                self.refresh(index);
            }
        }
    }

    /// The account's mail changed: reload its unread counts, and the shown rows at the start of the next frame.
    pub(crate) fn refresh(&mut self, index: usize) {
        self.accounts[index].reload_folders();
        if self.view_account() == Some(index)
            || matches!(self.view, View::AllInboxes | View::Activity | View::Trash)
        {
            self.view_dirty = true;
        }
    }

    /// Records an error in the history and on the account's status line, or on the app's own line.
    pub(crate) fn note_error(&mut self, account: Option<usize>, message: String) {
        let message = clean(&message, false);
        let line = match account {
            Some(index) => {
                // A body fetch that failed is retried the next time its message is shown.
                self.requested.retain(|(account, _)| *account != index);
                self.accounts[index].error = Some(message.clone());
                format!("{}: {message}", self.accounts[index].name)
            }
            None => {
                self.error = Some(message.clone());
                message
            }
        };
        self.push_history(line);
    }

    fn push_history(&mut self, text: String) {
        if self.history.back().is_some_and(|last| last.text == text) {
            return;
        }
        self.history.push_back(HistoryLine {
            at: crate::time::now(),
            text,
        });
        while self.history.len() > HISTORY {
            self.history.pop_front();
        }
    }

    pub fn apply(&mut self, action: UiAction, now: f64) {
        match action {
            UiAction::Act(action) => self.act(action),
            UiAction::ApproveRule(name) => {
                let accounts: Vec<String> = self.accounts.iter().map(|a| a.name.clone()).collect();
                let paths = self.paths.clone();
                self.edit_rules(|_| {
                    rule_file::edit::approve_from_now(&paths, &accounts, &name, crate::time::now())
                });
            }
            UiAction::CloseSource => {
                if let Some(body) = &mut self.body {
                    body.source = None;
                }
            }
            UiAction::Collapse => self.collapse(),
            UiAction::HtmlHover(at) => self.html_hover(at),
            UiAction::HtmlSelect { from, to } => self.html_select(from, to),
            UiAction::HtmlVisible { top, bottom } => self.html_visible(top, bottom),
            UiAction::HtmlWidth { scale, width } => self.html_width(scale, width, now),
            UiAction::Escape => {
                if self.move_picker.is_some() {
                    self.move_picker = None;
                } else if let Some(body) = self.body.as_mut().filter(|b| b.source.is_some()) {
                    body.source = None;
                } else if self.help_open {
                    self.help_open = false;
                } else if self.history_open {
                    self.history_open = false;
                } else if self.search.is_some() {
                    self.search = None;
                    self.list = ListState::default();
                    self.reload_view();
                } else {
                    self.list.marked.clear();
                }
            }
            UiAction::Expand => self.expand(),
            UiAction::ListViewport { offset, height } => {
                self.list.viewport = (offset, height);
                self.list.follow_cursor = false;
            }
            UiAction::MarkFolderRead(account, folder) => self.mark_folder_read(account, folder),
            UiAction::MoveCursor(delta) => {
                self.list.cursor = step(self.list.cursor, delta, self.list.rows.len());
                self.list.follow_cursor = true;
                self.pending_arm = true;
            }
            UiAction::MoveFilter(filter) => self.move_picker = Some(filter),
            UiAction::MoveTo(folder) => {
                self.move_picker = None;
                if self.selected_account_count() > 1 {
                    self.note_error(None, MOVE_ONE_ACCOUNT.into());
                } else {
                    self.act(Action::Move(folder));
                }
            }
            UiAction::NextFocus => {
                self.focus = match self.focus {
                    Focus::Body => Focus::Folders,
                    Focus::Folders => Focus::List,
                    Focus::List => Focus::Body,
                }
            }
            UiAction::OpenMovePicker => match self.selected_account_count() {
                0 => {}
                1 => self.move_picker = Some(String::new()),
                _ => self.note_error(None, MOVE_ONE_ACCOUNT.into()),
            },
            UiAction::OpenSource => self.open_source(),
            UiAction::OpenRulesFile => self.open_rules_file(),
            UiAction::RejectRule(name) => {
                self.edit_rules(|path| rule_file::edit::reject(path, &name));
            }
            UiAction::Restore(account, file) => {
                // A failed restore reloads the view, which lists the backup again.
                self.trash.retain(|row| row.entry.path != file);
                if !self.send(account, Command::Restore { file }) {
                    self.note_error(Some(account), DAEMON_LOST.into());
                    self.refresh(account);
                }
            }
            UiAction::SaveAttachment(index) => self.save_attachment(index),
            UiAction::SaveEml => self.save_eml(),
            UiAction::SearchFocused => self.focus_search = false,
            UiAction::SearchFor(query) => {
                self.search = Some(query);
                self.list.cursor = 0;
                self.list.rows.clear();
                self.reload_view();
            }
            UiAction::SelectRow(index) => {
                self.list.cursor = index;
                self.focus = Focus::List;
                self.pending_arm = true;
                self.arm_shown_body(index);
            }
            UiAction::SelectView(view) => self.select_view(view),
            UiAction::SetRuleEnabled(name, enabled) => {
                self.edit_rules(|path| rule_file::edit::set_enabled(path, &name, enabled));
            }
            UiAction::SetTheme(theme) => {
                self.theme = theme;
                let path = self.paths.config_file();
                match config::save_theme(&path, theme) {
                    Ok(()) => self.config_mtime = mtime(&path),
                    Err(e) => self.note_error(None, format!("could not save the theme: {e}")),
                }
            }
            UiAction::StartSearch => {
                if self.view.lists_mail() {
                    self.search = Some(String::new());
                    self.focus_search = true;
                    self.list = ListState::default();
                    self.reload_view();
                }
            }
            UiAction::StepFolder(delta) => {
                let entries = self.tree_entries(true);
                let at = entries
                    .iter()
                    .position(|v| *v == self.view)
                    .or_else(|| self.nearest_shown_above(&entries))
                    .unwrap_or(0);
                let view = entries[step(at, delta, entries.len())].clone();
                if view != self.view {
                    self.select_view(view);
                }
            }
            UiAction::SyncNow => self.sync_all(),
            UiAction::ToggleFolder(account, path) => {
                if !self.collapsed.remove(&(account, path.clone())) {
                    self.collapsed.insert((account, path));
                }
            }
            UiAction::ToggleFlag => {
                if let Some(flagged) = self.selected_rows().first().map(|row| row.flagged) {
                    self.act(if flagged {
                        Action::Unflag
                    } else {
                        Action::Flag
                    });
                }
            }
            UiAction::ToggleHelp => self.help_open = !self.help_open,
            UiAction::ToggleHtml => {
                if let Some(body) = self.body.as_mut().filter(|b| b.html.is_some()) {
                    body.show_text = !body.show_text;
                }
            }
            UiAction::ToggleHistory => {
                self.history_open = !self.history_open;
                self.error = None;
            }
            UiAction::ToggleMark => {
                if let Some(id) = self.list.rows.get(self.list.cursor).map(list::Row::id)
                    && !self.list.marked.remove(&id)
                {
                    self.list.marked.insert(id);
                }
            }
            UiAction::ToggleRead => {
                if let Some(unread) = self.selected_rows().first().map(|row| row.unread) {
                    self.act(if unread {
                        Action::MarkRead
                    } else {
                        Action::MarkUnread
                    });
                }
            }
        }
    }

    pub(crate) fn select_view(&mut self, view: View) {
        self.view = view;
        self.move_picker = None;
        self.search = None;
        self.list = ListState::default();
        self.reload_view();
    }

    pub(crate) fn view_account(&self) -> Option<usize> {
        match self.view {
            View::Folder { account, .. } => Some(account),
            _ => None,
        }
    }

    /// Sent and Drafts list the recipient instead of the sender.
    pub(crate) fn shows_recipient(&self) -> bool {
        let View::Folder { account, folder } = &self.view else {
            return false;
        };
        self.accounts[*account].folders.iter().any(|f| {
            &f.name == folder && matches!(f.special_use.as_deref(), Some("Drafts" | "Sent"))
        })
    }

    /// With one account, All inboxes would only repeat its INBOX.
    pub(crate) fn shows_all_inboxes(&self) -> bool {
        self.accounts.len() > 1
    }

    /// The folder the list shows: the view's own, or INBOX in every account.
    fn list_folder(&self) -> Option<&str> {
        match &self.view {
            View::AllInboxes => Some(list::INBOX),
            View::Folder { folder, .. } => Some(folder.as_str()),
            _ => None,
        }
    }

    /// Whether `folder` is the account's Archive folder, where archiving moves a message back to the inbox.
    pub(crate) fn is_archive(&self, account: usize, folder: &str) -> bool {
        self.accounts[account]
            .folders
            .iter()
            .any(|f| f.name == folder && f.special_use.as_deref() == Some("Archive"))
    }

    /// Loads the shown view's rows from the store. Runs on view changes and when events mark the view dirty, never
    /// per frame: `thread_summaries` scans the folder.
    pub(crate) fn reload_view(&mut self) {
        let Some(folder) = self.list_folder().map(str::to_string) else {
            match self.view {
                View::Activity => self.activity_log = rules::activity_log(&self.accounts),
                View::Trash => self.trash = rules::trash_rows(&self.accounts, &self.paths),
                _ => {}
            }
            return;
        };
        let covered: Vec<usize> = match self.view {
            View::Folder { account, .. } => vec![account],
            _ => (0..self.accounts.len()).collect(),
        };
        // A store that failed to open shows its error in the folder pane and adds no rows.
        let stores: Vec<(usize, &Store)> = covered
            .into_iter()
            .filter_map(|index| {
                self.accounts[index]
                    .store
                    .as_ref()
                    .ok()
                    .map(|store| (index, store))
            })
            .collect();
        if let Some(query) = &self.search {
            let mut hits = Vec::new();
            for &(account, store) in &stores {
                match store.search(query, None, list::SEARCH_LIMIT) {
                    Ok(found) => hits.extend(found.iter().map(|m| Row::from_message(account, m))),
                    Err(e) => log::warn!("[{}] search failed: {e}", self.accounts[account].name),
                }
            }
            self.list.hits = list::merge_hits(hits, list::SEARCH_LIMIT);
        } else {
            let mut found = Vec::new();
            for &(account, store) in &stores {
                match store.thread_summaries(&folder, THREAD_LIMIT) {
                    Ok(threads) => found.push((account, threads)),
                    Err(e) => {
                        log::warn!(
                            "[{}] {folder}: could not load threads: {e}",
                            self.accounts[account].name
                        );
                        let previous = self
                            .list
                            .threads
                            .iter()
                            .filter(|(owner, _)| *owner == account)
                            .map(|(_, thread)| thread.clone())
                            .collect();
                        found.push((account, previous));
                    }
                }
            }
            self.list.threads = list::merge_threads(found, THREAD_LIMIT);
            let expanded: Vec<(usize, String)> = self.list.expanded.keys().cloned().collect();
            for key in expanded {
                let members = stores
                    .iter()
                    .find(|(account, _)| *account == key.0)
                    .and_then(|(_, store)| store.thread_members(&folder, &key.1).ok());
                match members {
                    Some(members) if members.len() > 1 => {
                        self.list.expanded.insert(key, members);
                    }
                    _ => {
                        self.list.expanded.remove(&key);
                    }
                }
            }
        }
        self.rebuild_rows();
    }

    /// A click on the row already shown opens it; `shown_at` stays, so a message long on screen is read at once.
    fn arm_shown_body(&mut self, index: usize) {
        let Some((account, key)) = self.list.rows.get(index).map(Row::id) else {
            return;
        };
        if let Some(body) = &mut self.body
            && body.account == account
            && body.key == key
        {
            body.armed = true;
        }
    }

    /// Loads the cursor's message when the cursor moved to another one; nothing while a search query is typed.
    fn sync_body(&mut self, now: f64, typing_search: bool) {
        if typing_search {
            self.body = None;
            return;
        }
        let armed = std::mem::take(&mut self.pending_arm);
        let current = self.list.rows.get(self.list.cursor).map(Row::id);
        if current == self.body.as_ref().map(|b| (b.account, b.key.clone())) {
            return;
        }
        self.body = current.map(|(account, key)| self.load_body(account, key, now, armed));
        self.prefetch_neighbours();
    }

    /// The message, its attachments and its HTML as stored now.
    fn read_stored(&self, account: usize, key: &RowKey) -> Stored {
        let Ok(store) = &self.accounts[account].store else {
            return (None, Vec::new(), None);
        };
        let raw = store.raw(&key.0, key.1).ok().flatten();
        let attachments = raw.as_deref().map(message::attachments).unwrap_or_default();
        let html = raw.as_deref().and_then(html::html_of);
        (
            store.message(&key.0, key.1).ok().flatten(),
            attachments,
            html,
        )
    }

    /// Refreshes the shown message after its body arrived; the read delay restarts when the text first appears.
    fn reload_body(&mut self, account: usize, key: RowKey, now: f64) {
        if self
            .body
            .as_ref()
            .is_none_or(|b| b.account != account || b.key != key)
        {
            return;
        }
        let (message, attachments, html) = self.read_stored(account, &key);
        if let Some(body) = &mut self.body {
            let text = body_text(message.as_ref());
            if body.text.is_none() && text.is_some() {
                body.shown_at = now;
            }
            body.text = text;
            body.message = message;
            body.attachments = attachments;
            if body.html != html {
                body.text_note = too_large(html.as_ref());
                body.html = html;
                body.html_view = None;
            }
        }
    }

    /// Asks the sync thread for a body once per message.
    fn request_body(&mut self, account: usize, key: &RowKey) {
        if self.requested.insert((account, key.clone())) {
            let (folder, uid) = key.clone();
            self.send(account, Command::FetchBody { folder, uid });
        }
    }

    /// Asks for the missing bodies of the rows beside the cursor, each from its own account, so stepping to them
    /// shows text at once.
    fn prefetch_neighbours(&mut self) {
        let cursor = self.list.cursor;
        let missing: Vec<(usize, RowKey)> = [cursor.checked_sub(1), Some(cursor + 1)]
            .into_iter()
            .flatten()
            .filter_map(|index| self.list.rows.get(index))
            .filter(|row| match &self.accounts[row.account].store {
                Ok(store) => matches!(
                    store.message(&row.folder, row.uid),
                    Ok(Some(m)) if m.body_text.is_none()
                ),
                Err(_) => false,
            })
            .map(Row::id)
            .collect();
        for (account, key) in missing {
            self.request_body(account, &key);
        }
    }

    /// The stored message and its attachments; asks the sync thread for a missing body once per message.
    fn load_body(&mut self, account: usize, key: RowKey, now: f64, armed: bool) -> BodyState {
        let (stored, attachments, html) = self.read_stored(account, &key);
        if stored.as_ref().is_some_and(|m| m.body_text.is_none()) {
            self.request_body(account, &key);
        }
        let text = body_text(stored.as_ref());
        BodyState {
            account,
            armed,
            attachments,
            text_note: too_large(html.as_ref()),
            html,
            html_view: None,
            key,
            message: stored,
            read_sent: false,
            saved: None,
            show_text: false,
            shown_at: now,
            source: None,
            text,
        }
    }

    /// Marks the shown message read once the user opened it and its text has been on screen for `READ_DELAY`.
    fn mark_read_after_delay(&mut self, now: f64) {
        let Some(body) = &mut self.body else { return };
        if !body.armed {
            return;
        }
        // The delay counts from when the text appears (`reload_body`), not from "Loading…".
        if body.message.as_ref().is_some_and(|m| m.body_text.is_none()) {
            return;
        }
        let account = &self.accounts[body.account];
        let last_edit = account.pending.get(&body.key).and_then(|edits| {
            edits.iter().rev().find_map(|edit| match edit {
                Optimistic::Seen(seen) => Some(*seen),
                _ => None,
            })
        });
        let seen = last_edit.unwrap_or_else(|| body.message.as_ref().is_none_or(Message::is_seen));
        if seen || body.read_sent {
            return;
        }
        let waited = now - body.shown_at;
        if waited < READ_DELAY {
            let at = body.shown_at + READ_DELAY;
            self.request_tick(at);
            return;
        }
        let (index, (folder, uid)) = (body.account, body.key.clone());
        if let Some(body) = &mut self.body {
            body.read_sent = true;
        }
        self.send_apply(
            index,
            folder,
            vec![uid],
            Action::MarkRead,
            Optimistic::Seen(true),
        );
        self.rebuild_rows();
    }

    /// The shown message's raw bytes from the store, or `None` while it is not downloaded.
    fn shown_raw(&self) -> Option<Vec<u8>> {
        let body = self.body.as_ref()?;
        let store = self.accounts[body.account].store.as_ref().ok()?;
        store.raw(&body.key.0, body.key.1).ok().flatten()
    }

    fn save_attachment(&mut self, index: usize) {
        let raw = self.shown_raw();
        let note =
            saved_note(raw.map(|raw| message::save_attachment(&raw, index, &self.downloads)));
        if let Some(body) = &mut self.body {
            body.saved = Some(note);
        }
    }

    fn save_eml(&mut self) {
        let raw = self.shown_raw();
        let Some(body) = &mut self.body else { return };
        let subject = body.message.as_ref().and_then(|m| m.subject.as_deref());
        body.saved =
            Some(saved_note(raw.map(|raw| {
                message::save_eml(&raw, subject, &self.downloads)
            })));
    }

    fn open_source(&mut self) {
        let raw = self.shown_raw();
        let Some(body) = &mut self.body else { return };
        match raw {
            Some(raw) => body.source = Some(body::Source::new(&raw)),
            None => {
                body.saved = Some("Could not show the source: the message is not downloaded".into())
            }
        }
    }

    /// Takes the render thread's replies for the shown layout; replies for an older one are dropped.
    fn receive_html(&mut self) {
        let Some(renderer) = &self.renderer else {
            return;
        };
        for reply in renderer.replies() {
            let Some(body) = self.body.as_mut() else {
                continue;
            };
            let Some(view) = body
                .html_view
                .as_mut()
                .filter(|v| v.generation == reply.generation())
            else {
                continue;
            };
            match reply {
                Reply::Failed { .. } => {
                    body.text_note = Some(html::FAILED);
                    body.html_view = None;
                }
                Reply::Laid {
                    height,
                    remote,
                    width,
                    ..
                } => {
                    if height > html::MAX_HEIGHT {
                        body.text_note = Some(html::TOO_LARGE);
                        body.html_view = None;
                        continue;
                    }
                    view.page = Some((width, height));
                    view.remote = remote;
                    view.strips.clear();
                    view.requested.clear();
                    view.hover = None;
                    view.hovered_at = None;
                    view.selected_at = None;
                    view.selection = None;
                }
                Reply::Link { href, .. } => view.hover = href,
                Reply::Selected { text, .. } => {
                    view.selection = text;
                    // Paint the strips again with the highlight; the old ones stay on screen until then.
                    view.requested.clear();
                }
                Reply::Strip { image, strip, .. } => {
                    view.strips.insert(strip, Arc::new(image));
                }
            }
        }
    }

    /// Starts the HTML layout of the shown message, or a new one once the panel width or scale held still for
    /// `html::SETTLE`; until then the old layout stays on screen.
    fn html_width(&mut self, scale: f32, width: f32, now: f64) {
        let Some(body) = self.body.as_mut().filter(|b| b.shows_html()) else {
            return;
        };
        let mut settle_at = None;
        let fresh = match &mut body.html_view {
            None => true,
            Some(view) if view.sent == (scale, width) => {
                view.wanted = None;
                false
            }
            Some(view) => match view.wanted {
                Some((wanted, since)) if wanted == (scale, width) => {
                    if now - since < html::SETTLE {
                        settle_at = Some(since + html::SETTLE);
                    }
                    now - since >= html::SETTLE
                }
                _ => {
                    view.wanted = Some(((scale, width), now));
                    settle_at = Some(now + html::SETTLE);
                    false
                }
            },
        };
        let Some(source) = body.html.as_ref().filter(|_| fresh) else {
            if let Some(at) = settle_at {
                self.request_tick(at);
            }
            return;
        };
        self.html_generation += 1;
        let generation = self.html_generation;
        let load = html::Load {
            generation,
            html: source.html.clone(),
            inline: source.inline.clone(),
            scale,
            width,
        };
        match &mut body.html_view {
            Some(view) => {
                view.generation = generation;
                view.sent = (scale, width);
                view.wanted = None;
            }
            None => body.html_view = Some(HtmlState::new(generation, scale, width)),
        }
        self.renderer
            .get_or_insert_with(|| Renderer::start(self.wake.clone()))
            .send(html::Request::Load(load));
    }

    /// Asks for the strips around what is on screen and lets go of those far from it.
    fn html_visible(&mut self, top: f32, bottom: f32) {
        let (Some(view), Some(renderer)) = (
            self.body.as_mut().and_then(|b| b.html_view.as_mut()),
            self.renderer.as_mut(),
        ) else {
            return;
        };
        let Some(page) = view.page else { return };
        let scale = view.sent.0;
        for strip in html::wanted_strips(top, bottom, page.1, scale) {
            if view.requested.insert(strip) {
                renderer.send(html::Request::Paint {
                    generation: view.generation,
                    strip,
                });
            }
        }
        for strip in html::far_strips(view.strips.keys().copied(), top, bottom, scale) {
            view.strips.remove(&strip);
            view.requested.remove(&strip);
        }
    }

    /// Asks which link is under the pointer; the answer arrives as `Reply::Link`.
    fn html_hover(&mut self, at: Option<[f32; 2]>) {
        let (Some(view), Some(renderer)) = (
            self.body.as_mut().and_then(|b| b.html_view.as_mut()),
            self.renderer.as_mut(),
        ) else {
            return;
        };
        view.hovered_at = at;
        match at {
            Some(at) => renderer.send(html::Request::Hit {
                generation: view.generation,
                x: at[0],
                y: at[1],
            }),
            None => view.hover = None,
        }
    }

    /// Selects the page's text between two points; the answer arrives as `Reply::Selected`.
    fn html_select(&mut self, from: [f32; 2], to: [f32; 2]) {
        let (Some(view), Some(renderer)) = (
            self.body.as_mut().and_then(|b| b.html_view.as_mut()),
            self.renderer.as_mut(),
        ) else {
            return;
        };
        view.selected_at = Some((from, to));
        renderer.send(html::Request::Select {
            generation: view.generation,
            from,
            to,
        });
    }

    /// Rows from the cached threads, without a store query. The cursor stays on its message; when that row is gone it
    /// keeps its index, so the next row is selected.
    pub(crate) fn rebuild_rows(&mut self) {
        let Some(folder) = self.list_folder().map(str::to_string) else {
            return;
        };
        let current = self.list.rows.get(self.list.cursor).cloned();
        let mut rows = if self.search.is_some() {
            self.list.hits.clone()
        } else {
            list::build_rows(&folder, &self.list.threads, &self.list.expanded)
        };
        let pending: Vec<_> = self.accounts.iter().map(|a| &a.pending).collect();
        list::apply_pending(&mut rows, &pending);
        let kept = current.and_then(|cursor| rows.iter().position(|row| same_row(row, &cursor)));
        self.list.rows = rows;
        self.list.cursor =
            kept.unwrap_or_else(|| self.list.cursor.min(self.list.rows.len().saturating_sub(1)));
    }

    /// Sends `action` for the marked rows, or the cursor row, each to its own account, and shows its effect before the
    /// server confirms it.
    fn act(&mut self, action: Action) {
        let Some(optimistic) = Optimistic::of(&action) else {
            return;
        };
        let targets = self.targets();
        if targets.is_empty() {
            return;
        }
        if matches!(action, Action::MarkRead | Action::MarkUnread)
            && let Some(body) = &mut self.body
            && targets.iter().any(|(account, folder, uids)| {
                *account == body.account && *folder == body.key.0 && uids.contains(&body.key.1)
            })
        {
            body.read_sent = true;
        }
        self.pending_arm |= optimistic == Optimistic::Hidden;
        for (account, folder, uids) in targets {
            let action = if matches!(action, Action::Archive) && self.is_archive(account, &folder) {
                Action::Move("INBOX".into())
            } else {
                action.clone()
            };
            self.send_apply(account, folder, uids, action, optimistic);
        }
        self.list.marked.clear();
        self.rebuild_rows();
    }

    fn mark_folder_read(&mut self, account: usize, folder: String) {
        let uids = match &self.accounts[account].store {
            Ok(store) => store.unread_uids(&folder),
            Err(_) => return,
        };
        match uids {
            Ok(uids) if uids.is_empty() => {}
            Ok(uids) => {
                self.send_apply(
                    account,
                    folder,
                    uids,
                    Action::MarkRead,
                    Optimistic::Seen(true),
                );
                self.rebuild_rows();
            }
            Err(e) => self.note_error(Some(account), e.to_string()),
        }
    }

    /// Sends one `Apply` and records its optimistic edit, which the matching `ActionDone` clears.
    pub(crate) fn send_apply(
        &mut self,
        account: usize,
        folder: String,
        uids: Vec<u32>,
        action: Action,
        optimistic: Optimistic,
    ) {
        let keys: Vec<RowKey> = uids.iter().map(|&uid| (folder.clone(), uid)).collect();
        if self.send(
            account,
            Command::Apply {
                folder,
                uids,
                action,
                by: "gui".into(),
            },
        ) {
            let state = &mut self.accounts[account];
            state.queued += 1;
            for key in keys {
                state.pending.entry(key).or_default().push(optimistic);
            }
            state.reload_folders();
        } else {
            self.note_error(Some(account), DAEMON_LOST.into());
        }
    }

    pub(crate) fn send(&self, account: usize, command: Command) -> bool {
        self.client.send(&self.accounts[account].name, command)
    }

    /// Notices edits to rules.toml and config.toml.
    fn poll_files(&mut self, now: f64) {
        if now - self.last_poll < POLL {
            return;
        }
        self.last_poll = now;
        if mtime(&self.paths.rules_file()) != self.rules_mtime {
            self.rules_changed();
        }
        if mtime(&self.paths.config_file()) != self.config_mtime {
            self.config_changed = true;
        }
    }

    /// Reloads the rules view; the daemon notices the change itself and syncs every account.
    fn rules_changed(&mut self) {
        let path = self.paths.rules_file();
        self.rules_mtime = mtime(&path);
        self.rules = RulesState::load(&path, std::mem::take(&mut self.rules.rules));
    }

    fn edit_rules(&mut self, edit: impl FnOnce(&Path) -> Result<(), RulesError>) {
        match edit(&self.paths.rules_file()) {
            Ok(()) => self.rules_changed(),
            Err(e) => self.note_error(None, e.to_string()),
        }
    }

    fn open_rules_file(&mut self) {
        let path = self.paths.rules_file();
        let opened = (|| -> std::io::Result<()> {
            if !path.exists() {
                crate::paths::write_atomic(&path, b"")?;
            }
            // `open -t` uses the default text editor, so a missing `.toml` association still opens one.
            let (opener, flags): (&str, &[&str]) = if cfg!(target_os = "macos") {
                ("open", &["-t"])
            } else {
                ("xdg-open", &[])
            };
            let mut child = std::process::Command::new(opener)
                .args(flags)
                .arg(&path)
                .spawn()?;
            std::thread::spawn(move || match child.wait() {
                Ok(status) if !status.success() => log::warn!("{opener} rules.toml: {status}"),
                Ok(_) => {}
                Err(e) => log::warn!("{opener} rules.toml: {e}"),
            });
            Ok(())
        })();
        if let Err(e) = opened {
            self.note_error(None, format!("could not open rules.toml: {e}"));
        }
    }

    pub(crate) fn sync_all(&mut self) {
        for index in 0..self.accounts.len() {
            self.send(index, Command::SyncNow);
        }
    }

    fn selected_account_count(&self) -> usize {
        let accounts: BTreeSet<usize> =
            self.selected_rows().iter().map(|row| row.account).collect();
        accounts.len()
    }

    /// The rows an action covers: the marked rows, or else the cursor row.
    pub(crate) fn selected_rows(&self) -> Vec<&Row> {
        if self.list.marked.is_empty() {
            self.list.rows.get(self.list.cursor).into_iter().collect()
        } else {
            self.list
                .rows
                .iter()
                .filter(|row| self.list.marked.contains(&row.id()))
                .collect()
        }
    }

    /// The uids each action covers, per account and folder: the marked rows or the cursor row, a thread row standing
    /// for every message of its thread in that folder.
    fn targets(&self) -> Vec<(usize, String, Vec<u32>)> {
        let mut by_folder: BTreeMap<(usize, String), BTreeSet<u32>> = BTreeMap::new();
        for row in self.selected_rows() {
            let uids = by_folder
                .entry((row.account, row.folder.clone()))
                .or_default();
            let members = match (&row.thread_id, &self.accounts[row.account].store) {
                (Some(thread), Ok(store)) if row.count > 1 => {
                    store.thread_members(&row.folder, thread).ok()
                }
                _ => None,
            };
            match members {
                Some(members) => uids.extend(members.iter().map(|member| member.uid)),
                None => {
                    uids.insert(row.uid);
                }
            }
        }
        by_folder
            .into_iter()
            .map(|((account, folder), uids)| (account, folder, uids.into_iter().collect()))
            .collect()
    }

    fn expand(&mut self) {
        let Some(row) = self.list.rows.get(self.list.cursor) else {
            return;
        };
        let (Some(thread), folder) = (row.thread_id.clone(), row.folder.clone()) else {
            return;
        };
        let key = (row.account, thread);
        if row.count < 2 || self.list.expanded.contains_key(&key) {
            return;
        }
        if let Ok(store) = &self.accounts[key.0].store
            && let Ok(members) = store.thread_members(&folder, &key.1)
        {
            self.list.expanded.insert(key, members);
            self.rebuild_rows();
        }
    }

    /// Collapses the thread the cursor is in and puts the cursor on its thread row.
    fn collapse(&mut self) {
        let rows = &self.list.rows;
        if rows.is_empty() {
            return;
        }
        let from = self.list.cursor.min(rows.len() - 1);
        let Some(parent) = (0..=from).rev().find(|&i| rows[i].thread_id.is_some()) else {
            return;
        };
        let Some(thread) = rows[parent].thread_id.clone() else {
            return;
        };
        if self
            .list
            .expanded
            .remove(&(rows[parent].account, thread))
            .is_some()
        {
            self.list.cursor = parent;
            self.rebuild_rows();
        }
    }

    /// Where a hidden view (in a folded branch) sits in `shown`: at the nearest shown entry above it.
    fn nearest_shown_above(&self, shown: &[View]) -> Option<usize> {
        let all = self.tree_entries(false);
        let at = all.iter().position(|v| *v == self.view)?;
        all[..at]
            .iter()
            .rev()
            .find_map(|view| shown.iter().position(|v| v == view))
    }

    /// All inboxes when shown, every folder of every account, then Rules, Activity and Trash, in tree order.
    fn tree_entries(&self, skip_collapsed: bool) -> Vec<View> {
        let mut entries: Vec<View> = Vec::new();
        if self.shows_all_inboxes() {
            entries.push(View::AllInboxes);
        }
        entries.extend(self.accounts.iter().enumerate().flat_map(|(account, a)| {
            folders::shown(a, |path| {
                skip_collapsed && self.collapsed.contains(&(account, path.to_string()))
            })
            .into_iter()
            .map(move |folder| View::Folder { account, folder })
        }));
        entries.extend([View::Rules, View::Activity, View::Trash]);
        entries
    }

/// A stored message, its attachments and its HTML.
type Stored = (Option<Message>, Vec<Attachment>, Option<HtmlBody>);

/// The note for HTML too large to lay out, which then shows as text.
fn too_large(html: Option<&HtmlBody>) -> Option<&'static str> {
    html.filter(|h| h.html.len() > html::MAX_HTML)
        .map(|_| html::TOO_LARGE)
}

/// The line under the attachments after a save: where the file went, or why not.
fn saved_note(result: Option<std::io::Result<PathBuf>>) -> String {
    match result {
        Some(Ok(path)) => format!("Saved to {}", path.display()),
        Some(Err(e)) => format!("Could not save: {e}"),
        None => "Could not save: the message is not downloaded".into(),
    }
}

fn body_text(message: Option<&Message>) -> Option<String> {
    message?.body_text.as_deref().map(|text| clean(text, true))
}

/// A thread row's key is its latest uid, which changes with new mail, so it is matched by thread id. A member and its
/// thread row share a key, so `member` tells them apart.
fn same_row(row: &Row, cursor: &Row) -> bool {
    if row.account != cursor.account || row.member != cursor.member {
        return false;
    }
    match (&row.thread_id, &cursor.thread_id) {
        (Some(row_thread), Some(cursor_thread)) if !row.member => row_thread == cursor_thread,
        _ => row.key() == cursor.key(),
    }
}

/// Where Save puts attachments: the Downloads folder, else home.
fn downloads_dir() -> PathBuf {
    directories::UserDirs::new()
        .map(|dirs| dirs.download_dir().unwrap_or(dirs.home_dir()).to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

pub(crate) fn step(index: usize, delta: isize, len: usize) -> usize {
    index
        .saturating_add_signed(delta)
        .min(len.saturating_sub(1))
}

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}
