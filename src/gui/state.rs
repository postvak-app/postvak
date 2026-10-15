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
use crate::store::{Filter, LogEntry, Message, Store, StoreError, has_flag};
use crate::sync::{Activity, Command, Event};
use crate::update;

use super::body;
use super::folders;
use super::html::{self, Fetcher, HtmlState, Renderer, Reply, Wake};
use super::list::{self, ListState, Optimistic, Row, RowKey, THREAD_LIMIT};
use super::rules::{self, RulesState, SheetSort, TrashRow};

/// The status line when `m` is pressed with marked rows from more than one account.
pub(crate) const MOVE_ONE_ACCOUNT: &str =
    "move works on one account at a time; mark messages from one account";

/// The status line when messages are dropped on a folder of another account.
pub(crate) const MOVE_OWN_ACCOUNT: &str = "messages move only to folders of their own account";

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
    /// The links in `text` that open, found once with it.
    pub links: Vec<String>,
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
    /// Open or close the About window.
    About,
    Act(Action),
    ApproveRule(String),
    Collapse,
    /// Move the targeted rows into a folder dropped on.
    DropOn(View),
    Escape,
    Expand,
    /// Shift-click: mark the rows from the cursor to this one, and move the cursor there.
    ExtendSelection(usize),
    SetFilter(Filter),
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
    /// Download the remote content the shown HTML page asked for, and lay it out again with it.
    LoadRemote,
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
    /// Open a web page in the browser.
    OpenUrl(String),
    OpenRulesFile,
    OpenSource,
    RejectRule(String),
    Restore(usize, PathBuf),
    SaveAttachment(usize),
    SaveEml,
    /// The search field took (true) or lost (false) the keyboard.
    SearchFocused(bool),
    SearchFor(String),
    /// Mark every row in the list.
    SelectAll,
    SelectRow(usize),
    SelectView(View),
    SetRuleEnabled(String, bool),
    SortSheet(SheetSort),
    SetTheme(Theme),
    StartSearch,
    StepFolder(isize),
    SyncNow,
    /// Aim the next action at the marked rows when `row` is one of them, else at `row` alone.
    TargetRow(usize),
    ToggleFlag,
    ToggleFolder(usize, String),
    ToggleHelp,
    ToggleHistory,
    ToggleHtml,
    ToggleMark,
    ToggleRead,
    /// Cmd-click (Ctrl-click off macOS): add this row to the selection, which starts from the cursor row, or take it out.
    ToggleRowMark(usize),
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
    /// Downloads remote content for HTML mail, started the first time the user asks.
    pub(crate) fetcher: Option<Fetcher>,
    pub(crate) focus: Focus,
    pub(crate) about_open: bool,
    pub(crate) help_open: bool,
    /// Numbers HTML layouts, so a reply for an older one is dropped.
    pub(crate) html_generation: u64,
    /// The scale and width the window last gave the HTML view, so the next HTML message lays out at once.
    pub(crate) html_size: Option<(f32, f32)>,
    pub(crate) history: VecDeque<HistoryLine>,
    pub(crate) history_open: bool,
    /// Opens a file or URL with the desktop's app; tests record instead.
    pub(crate) launch: fn(&std::ffi::OsStr, bool) -> std::io::Result<()>,
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
    /// Which rows a folder's list keeps; search results ignore it.
    pub(crate) filter: Filter,
    /// Set by the window while the search field has the keyboard, so the list is not opened while a query is typed.
    pub(crate) search_focused: bool,
    /// The earliest time something here wants `tick` again (the read delay, a settling width, polling); the window
    /// sets a timer for it.
    pub(crate) next_tick: Option<f64>,
    /// Wakes the window from another thread (a reconnect, the release check, the HTML renderer).
    pub(crate) wake: Wake,
    pub(crate) theme: Theme,
    pub(crate) trash: Vec<TrashRow>,
    /// How Activity or Backups is sorted; their own order, newest first, until a heading is clicked.
    pub(crate) sheet_sort: Option<SheetSort>,
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
            fetcher: None,
            focus: Focus::List,
            about_open: false,
            help_open: false,
            html_generation: 0,
            html_size: None,
            history: VecDeque::new(),
            history_open: false,
            launch,
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
            filter: Filter::default(),
            search_focused: false,
            next_tick: None,
            wake: Arc::new(|| {}),
            theme: config.ui.theme,
            trash: Vec::new(),
            sheet_sort: None,
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
        self.receive_remote();
        self.keep_daemon(now);
        self.check_release();
        if std::mem::take(&mut self.view_dirty) {
            self.reload_view();
        }
        self.poll_files(now);
        let typing_search = self.search_focused;
        self.sync_body(now, typing_search);
        // The window reports the HTML view's width only when it changes; laying out at the last one starts a newly
        // shown message, and lays out again once a new width has held still.
        if let Some((scale, width)) = self.html_size {
            self.html_width(scale, width, now);
        }
        self.paint_visible();
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
                let (cache, fetch, wake) = (
                    self.paths.update_check(),
                    self.fetch_release,
                    self.wake.clone(),
                );
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

    /// Applies `action`; when it moved the cursor, by key, click, search or collapse, the list scrolls to it.
    pub fn apply(&mut self, action: UiAction) {
        let cursor = self.list.cursor;
        self.apply_action(action);
        if self.list.cursor != cursor && !self.list.cursor_shown() {
            self.list.follow_cursor = true;
        }
    }

    fn apply_action(&mut self, action: UiAction) {
        match action {
            UiAction::Act(action) => self.act(action),
            UiAction::ApproveRule(name) => {
                let accounts: Vec<String> = self.accounts.iter().map(|a| a.name.clone()).collect();
                let paths = self.paths.clone();
                self.edit_rules(|_| {
                    rule_file::edit::approve_from_now(&paths, &accounts, &name, crate::time::now())
                });
            }
            UiAction::Collapse => self.collapse(),
            UiAction::DropOn(view) => self.drop_on(view),
            UiAction::HtmlHover(at) => self.html_hover(at),
            UiAction::HtmlSelect { from, to } => self.html_select(from, to),
            UiAction::HtmlVisible { top, bottom } => self.html_visible(top, bottom),
            UiAction::HtmlWidth { scale, width } => self.html_size = Some((scale, width)),
            UiAction::LoadRemote => self.load_remote(),
            UiAction::Escape => {
                if self.move_picker.is_some() {
                    self.move_picker = None;
                } else if let Some(body) = self.body.as_mut().filter(|b| b.source.is_some()) {
                    body.source = None;
                } else if self.help_open {
                    self.help_open = false;
                } else if self.about_open {
                    self.about_open = false;
                } else if self.history_open {
                    self.history_open = false;
                } else if self.search.is_some() {
                    self.search = None;
                    self.search_focused = false;
                    self.list = ListState::default();
                    self.reload_view();
                } else {
                    self.list.marked.clear();
                }
            }
            UiAction::Expand => self.expand(),
            UiAction::ListViewport { offset, height } => {
                self.list.viewport = (offset, height);
                // The list can stop short of the cursor while it still estimates its rows; then it is asked again.
                if self.list.cursor_shown() {
                    self.list.follow_cursor = false;
                }
            }
            UiAction::MarkFolderRead(account, folder) => self.mark_folder_read(account, folder),
            UiAction::MoveCursor(delta) => {
                self.list.cursor = step(self.list.cursor, delta, self.list.rows.len());
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
            UiAction::OpenUrl(url) => {
                if let Err(e) = (self.launch)(url.as_ref(), false) {
                    self.note_error(None, format!("could not open the link: {e}"));
                }
            }
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
            UiAction::SearchFocused(focused) => self.search_focused = focused,
            UiAction::SetFilter(filter) => {
                self.filter = filter;
                self.reload_view();
            }
            UiAction::SearchFor(query) => {
                self.search = Some(query);
                self.list.cursor = 0;
                self.list.rows.clear();
                self.reload_view();
            }
            UiAction::ExtendSelection(index) if index < self.list.rows.len() => {
                let (low, high) = (self.list.cursor.min(index), self.list.cursor.max(index));
                let ids: Vec<_> = self.list.rows[low..=high]
                    .iter()
                    .map(list::Row::id)
                    .collect();
                self.list.marked.extend(ids);
                self.apply_action(UiAction::SelectRow(index));
            }
            UiAction::ExtendSelection(_) => {}
            UiAction::SelectAll => {
                let ids: Vec<_> = self.list.rows.iter().map(list::Row::id).collect();
                self.list.marked.extend(ids);
            }
            UiAction::ToggleRowMark(index) => {
                if self.list.marked.is_empty()
                    && let Some(cursor) = self.list.rows.get(self.list.cursor).map(list::Row::id)
                {
                    self.list.marked.insert(cursor);
                }
                if let Some(id) = self.list.rows.get(index).map(list::Row::id)
                    && !self.list.marked.remove(&id)
                {
                    self.list.marked.insert(id);
                }
            }
            UiAction::SelectRow(index) => {
                self.list.cursor = index;
                self.search_focused = false;
                self.focus = Focus::List;
                self.pending_arm = true;
                self.arm_shown_body(index);
            }
            UiAction::SelectView(view) => self.select_view(view),
            UiAction::SortSheet(sort) => self.sheet_sort = Some(sort),
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
                    // The field takes the keyboard as it appears.
                    self.search_focused = true;
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
            UiAction::TargetRow(row) => {
                if let Some(id) = self.list.rows.get(row).map(list::Row::id)
                    && !self.list.marked.contains(&id)
                {
                    self.list.marked.clear();
                    self.apply_action(UiAction::SelectRow(row));
                }
            }
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
            UiAction::About => self.about_open = !self.about_open,
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

    /// Whether a text field has the keyboard, so letters are text rather than keys.
    pub(crate) fn typing(&self) -> bool {
        self.search_focused || self.move_picker.is_some()
    }

    /// The `m` picker's folders: those of the selected rows' account that match the filter, without the folder the
    /// cursor message is in.
    pub(crate) fn move_targets(&self) -> Vec<&str> {
        let (Some(filter), Some(row)) = (&self.move_picker, self.selected_rows().first().copied())
        else {
            return Vec::new();
        };
        let folder = self
            .list
            .rows
            .get(self.list.cursor)
            .map_or(&row.folder, |cursor| &cursor.folder);
        let needle = filter.to_lowercase();
        self.accounts[row.account]
            .folders
            .iter()
            .map(|f| f.name.as_str())
            .filter(|name| name != folder && name.to_lowercase().contains(&needle))
            .collect()
    }

    /// Moves the selected rows into a folder of their own account; rows dropped on the folder they are in stay.
    fn drop_on(&mut self, view: View) {
        let View::Folder { account, folder } = view else {
            return;
        };
        let rows = self.selected_rows();
        if rows
            .iter()
            .all(|row| row.account == account && row.folder == folder)
        {
            return;
        }
        if rows.iter().any(|row| row.account != account) {
            self.note_error(None, MOVE_OWN_ACCOUNT.into());
            return;
        }
        self.apply_action(UiAction::MoveTo(folder));
    }

    pub(crate) fn select_view(&mut self, view: View) {
        self.view = view;
        self.sheet_sort = None;
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
        self.has_role(account, folder, "Archive")
    }

    pub(crate) fn is_junk(&self, account: usize, folder: &str) -> bool {
        self.has_role(account, folder, "Junk")
    }

    /// Whether the shown folder is a Junk folder, where the junk action means not junk.
    pub(crate) fn shows_junk(&self) -> bool {
        matches!(&self.view, View::Folder { account, folder } if self.is_junk(*account, folder))
    }

    fn has_role(&self, account: usize, folder: &str, role: &str) -> bool {
        self.accounts[account]
            .folders
            .iter()
            .any(|f| f.name == folder && f.special_use.as_deref() == Some(role))
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
                match store.thread_summaries(&folder, THREAD_LIMIT, self.filter) {
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
            body.links = body::links(text.as_deref());
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
            links: body::links(text.as_deref()),
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
                    // Once the user loaded remote content, what it asks for in turn (images in a stylesheet) follows.
                    let follow = !view.fetched.is_empty() && view.fetched.len() < html::MAX_REMOTE;
                    view.remote = remote;
                    if follow && !view.remote.is_empty() && view.fetching.is_none() {
                        self.load_remote();
                        continue;
                    }
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
        if fresh {
            self.lay_out(scale, width);
        } else if let Some(at) = settle_at {
            self.request_tick(at);
        }
    }

    /// Sends the shown message's HTML to the render thread, at `width` points and `scale`.
    fn lay_out(&mut self, scale: f32, width: f32) {
        let Some(body) = self.body.as_mut() else {
            return;
        };
        let Some(source) = body.html.as_ref() else {
            return;
        };
        self.html_generation += 1;
        let generation = self.html_generation;
        let fetched = body
            .html_view
            .as_ref()
            .map(|view| view.fetched.clone())
            .unwrap_or_default();
        let load = html::Load {
            fetched,
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

    /// Starts downloading what the shown page asked for; the tag is a fresh layout number, unique to this request.
    fn load_remote(&mut self) {
        let Some(view) = self.body.as_mut().and_then(|b| b.html_view.as_mut()) else {
            return;
        };
        if view.remote.is_empty() || view.fetching.is_some() {
            return;
        }
        self.html_generation += 1;
        let tag = self.html_generation;
        view.fetching = Some(tag);
        let room = html::MAX_REMOTE.saturating_sub(view.fetched.len());
        let urls = std::mem::take(&mut view.remote)
            .into_iter()
            .take(room)
            .collect();
        self.fetcher
            .get_or_insert_with(|| Fetcher::start(self.wake.clone()))
            .send(html::FetchJob { tag, urls });
    }

    /// Takes finished downloads for the shown message and lays it out again with them.
    fn receive_remote(&mut self) {
        let Some(fetcher) = &self.fetcher else {
            return;
        };
        for done in fetcher.done() {
            let Some(view) = self.body.as_mut().and_then(|b| b.html_view.as_mut()) else {
                continue;
            };
            if view.fetching != Some(done.tag) {
                continue;
            }
            view.fetching = None;
            std::sync::Arc::make_mut(&mut view.fetched).extend(done.fetched);
            let (scale, width) = view.sent;
            self.lay_out(scale, width);
        }
    }

    /// Records the part of the page on screen, and asks for its strips.
    fn html_visible(&mut self, top: f32, bottom: f32) {
        if let Some(view) = self.body.as_mut().and_then(|b| b.html_view.as_mut()) {
            view.visible = (top, bottom);
        }
        self.paint_visible();
    }

    /// Asks for the strips around what is on screen that are neither painted nor asked for, and lets go of those far
    /// from it. A new layout or selection empties `requested`, so they are asked for again.
    fn paint_visible(&mut self) {
        let (Some(view), Some(renderer)) = (
            self.body.as_mut().and_then(|b| b.html_view.as_mut()),
            self.renderer.as_mut(),
        ) else {
            return;
        };
        let (top, bottom) = view.visible;
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
            let action = match action {
                Action::Archive if self.is_archive(account, &folder) => {
                    Action::Move("INBOX".into())
                }
                Action::Junk if self.is_junk(account, &folder) => Action::NotJunk,
                _ => action.clone(),
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
            (self.launch)(path.as_os_str(), true)
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

/// Opens a file or URL with the desktop's default app; `text` asks for the text editor where the platform can.
pub(crate) fn launch(target: &std::ffi::OsStr, text: bool) -> std::io::Result<()> {
    // `open -t` uses the default text editor, so a missing `.toml` association still opens one.
    let (opener, flags): (&str, &[&str]) = match (cfg!(target_os = "macos"), text) {
        (true, true) => ("open", &["-t"]),
        (true, false) => ("open", &[]),
        (false, _) => ("xdg-open", &[]),
    };
    let mut child = std::process::Command::new(opener)
        .args(flags)
        .arg(target)
        .spawn()?;
    // The target is not logged: a link from a message can carry a token.
    std::thread::spawn(move || match child.wait() {
        Ok(status) if !status.success() => log::warn!("{opener}: {status}"),
        Ok(_) => {}
        Err(e) => log::warn!("{opener}: {e}"),
    });
    Ok(())
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

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::daemon::Client;
    use crate::daemon::wire::AccountStatus;
    use crate::gui::test_support::{Driver, Fixture, Wires, command, message, unread};
    use crate::rules::Action;
    use crate::sync::{Command, Event};

    fn inbox(fx: &Fixture, uids: &[u32]) {
        for &uid in uids {
            fx.add("work", message("INBOX", uid, &format!("subject {uid}")));
        }
    }

    fn apply(uids: &[u32], action: Action) -> (String, Command) {
        command("work", "INBOX", uids, action)
    }

    fn done(account: &str, results: Vec<(u32, Result<usize, String>)>, request: u64) -> Event {
        Event::ActionDone {
            account: account.into(),
            folder: "INBOX".into(),
            results,
            request,
        }
    }

    fn refused(account: &str, request: u64, message: &str) -> Event {
        Event::CommandFailed {
            account: account.into(),
            request,
            message: message.into(),
        }
    }

    /// Sends the daemon's events and ticks so the app takes them.
    fn receive(driver: &mut Driver, wires: &Wires, events: impl IntoIterator<Item = Event>) {
        for event in events {
            wires.events.send(event).unwrap();
        }
        driver.tick();
    }

    /// Ticks, with the clock at `at`, until `done` holds or 5 s of real time passed; for answers from other threads.
    fn tick_until(driver: &mut Driver, at: f64, done: impl Fn(&App) -> bool) {
        driver.at(at);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done(&driver.app) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
            driver.tick();
        }
        assert!(done(&driver.app), "timed out");
    }

    fn inbox_unread(driver: &Driver) -> Option<u32> {
        let folders = &driver.app.accounts[0].folders;
        folders.iter().find(|f| f.name == "INBOX").map(|f| f.unread)
    }

    #[test]
    fn all_inboxes_lists_every_accounts_inbox_newest_first() {
        let fx = Fixture::new(&["home", "work"]);
        fx.add("home", message("INBOX", 1, "oldest"));
        fx.add("work", message("INBOX", 2, "middle"));
        fx.add("home", message("INBOX", 3, "newest"));
        fx.folder("work", "Archive", Some("Archive"));
        fx.add("work", message("Archive", 4, "not an inbox"));
        let (mut driver, _wires) = fx.driver();
        driver.apply(UiAction::SelectView(View::AllInboxes));
        assert_eq!(driver.ids(), [(0, 3), (1, 2), (0, 1)]);
        assert_eq!(list::counts(&driver.app.list.rows), "3 threads, 0 unread");
    }

    #[test]
    fn all_inboxes_lists_the_other_accounts_when_one_store_is_broken() {
        let fx = Fixture::new(&["broken", "work"]);
        fx.add("work", message("INBOX", 1, "still here"));
        fx.break_store("broken");
        let (mut driver, _wires) = fx.driver();
        assert!(driver.app.shows_all_inboxes());
        driver.apply(UiAction::SelectView(View::AllInboxes));
        assert_eq!(driver.ids(), [(1, 1)]);
    }

    #[test]
    fn a_failed_thread_load_keeps_the_rows_already_listed() {
        let fx = Fixture::new(&["work"]);
        fx.add("work", message("INBOX", 1, "still here"));
        let (mut driver, _wires) = fx.driver();
        assert_eq!(driver.ids(), [(0, 1)]);
        let db = rusqlite::Connection::open(fx.paths.mail_db("work")).unwrap();
        db.execute_batch("ALTER TABLE messages RENAME TO messages_gone")
            .unwrap();
        driver.app.reload_view();
        driver.tick();
        assert_eq!(driver.ids(), [(0, 1)]);
    }

    #[test]
    fn theme_starts_from_config() {
        let fx = Fixture::new(&["work"]);
        assert_eq!(fx.app().0.theme, Theme::System);
        fx.append_config("[ui]\ntheme = \"dark\"\n");
        assert_eq!(fx.app().0.theme, Theme::Dark);
    }

    #[test]
    fn slash_in_all_inboxes_searches_every_account_newest_first() {
        let fx = Fixture::new(&["home", "work"]);
        fx.folder("home", "Archive", Some("Archive"));
        fx.add("home", message("INBOX", 1, "invoice march"));
        fx.add("work", message("INBOX", 2, "lunch"));
        fx.add("home", message("Archive", 3, "invoice april"));
        fx.add("work", message("INBOX", 4, "invoice may"));
        let (mut driver, _wires) = fx.driver();
        driver.apply(UiAction::SelectView(View::AllInboxes));
        driver.press("/");
        assert_eq!(driver.app.search.as_deref(), Some(""));
        assert!(driver.app.search_focused);
        driver.press("invoice");
        let found: Vec<(usize, &str, u32)> = driver
            .app
            .list
            .rows
            .iter()
            .map(|r| (r.account, r.folder.as_str(), r.uid))
            .collect();
        assert_eq!(found, [(1, "INBOX", 4), (0, "Archive", 3), (0, "INBOX", 1)]);
        driver.press("escape");
        assert_eq!(driver.app.view, View::AllInboxes);
        assert_eq!(driver.ids(), [(1, 4), (1, 2), (0, 1)]);
    }

    #[test]
    fn archive_sends_the_cursor_row_hides_it_and_selects_the_next() {
        let fx = Fixture::new(&["work"]);
        inbox(&fx, &[1, 2, 3]);
        let (mut driver, wires) = fx.driver();
        driver.press("e");
        assert_eq!(wires.sent(), [apply(&[3], Action::Archive)]);
        assert_eq!(driver.uids(), [2, 1]);
        assert_eq!(driver.app.list.rows[driver.app.list.cursor].uid, 2);
        assert!(driver.status()[0].ends_with(" · 1 queued"));
    }

    #[test]
    fn bang_reports_junk_and_in_the_junk_folder_not_junk() {
        let fx = Fixture::new(&["work"]);
        fx.folder("work", "Junk", Some("Junk"));
        inbox(&fx, &[1]);
        fx.add("work", message("Junk", 2, "offer"));
        let (mut driver, wires) = fx.driver();
        driver.press("!");
        driver.apply(UiAction::SelectView(View::Folder {
            account: 0,
            folder: "Junk".into(),
        }));
        driver.press("!");
        assert_eq!(
            wires.sent(),
            [
                apply(&[1], Action::Junk),
                command("work", "Junk", &[2], Action::NotJunk)
            ]
        );
    }

    #[test]
    fn archive_on_a_thread_row_sends_every_member() {
        let fx = Fixture::new(&["work"]);
        for uid in [4, 5] {
            let mut m = message("INBOX", uid, "thread");
            m.thread_id = "<t@example.com>".into();
            fx.add("work", m);
        }
        let (mut driver, wires) = fx.driver();
        driver.press("e");
        assert_eq!(wires.sent(), [apply(&[4, 5], Action::Archive)]);
    }

    #[test]
    fn the_same_thread_in_another_account_is_another_row() {
        let mut home = Row::from_message(0, &message("INBOX", 1, "hi"));
        home.thread_id = Some("<t@example.com>".into());
        let mut work = home.clone();
        work.account = 1;
        assert!(same_row(&home, &home.clone()));
        assert!(!same_row(&work, &home));
    }

    #[test]
    fn marked_rows_are_deleted_together() {
        let fx = Fixture::new(&["work"]);
        inbox(&fx, &[1, 2, 3]);
        let (mut driver, wires) = fx.driver();
        driver.press("x j x #");
        assert_eq!(wires.sent(), [apply(&[2, 3], Action::Trash)]);
        assert_eq!(driver.uids(), [1]);
        assert!(driver.app.list.marked.is_empty());
    }

    #[test]
    fn shift_click_marks_the_rows_from_the_cursor_to_the_clicked_one() {
        let fx = Fixture::new(&["work"]);
        inbox(&fx, &[1, 2, 3, 4, 5]);
        let (mut driver, wires) = fx.driver();
        driver.press("j");
        driver.apply(UiAction::ExtendSelection(3));
        assert_eq!(driver.app.list.cursor, 3);
        driver.press("#");
        assert_eq!(wires.sent(), [apply(&[2, 3, 4], Action::Trash)]);
    }

    #[test]
    fn command_click_adds_the_cursor_row_and_the_clicked_one_or_takes_it_out() {
        let fx = Fixture::new(&["work"]);
        inbox(&fx, &[1, 2, 3]);
        let (mut driver, wires) = fx.driver();
        driver.apply(UiAction::ToggleRowMark(2));
        assert_eq!(driver.app.list.marked.len(), 2);
        assert_eq!(driver.app.list.cursor, 0, "the cursor stays");
        driver.apply(UiAction::ToggleRowMark(1));
        driver.apply(UiAction::ToggleRowMark(1));
        driver.press("e");
        assert_eq!(wires.sent(), [apply(&[1, 3], Action::Archive)]);
    }

    #[test]
    fn command_a_marks_every_row() {
        let fx = Fixture::new(&["work"]);
        inbox(&fx, &[1, 2, 3]);
        let (mut driver, wires) = fx.driver();
        driver.press("ctrl+a e");
        assert_eq!(wires.sent(), [apply(&[1, 2, 3], Action::Archive)]);
    }

    #[test]
    fn a_failed_action_puts_the_row_back_and_says_why() {
        let fx = Fixture::new(&["work"]);
        inbox(&fx, &[1, 2, 3]);
        let (mut driver, wires) = fx.driver();
        driver.press("e");
        let failed = done("work", vec![(3, Err("NO [SERVERBUG] try later".into()))], 0);
        receive(&mut driver, &wires, [failed]);
        assert_eq!(driver.uids(), [3, 2, 1]);
        assert_eq!(
            driver.status()[0],
            "work: 1 of 1 messages failed: NO [SERVERBUG] try later"
        );
        assert_eq!(driver.app.accounts[0].queued, 0);
    }

    #[test]
    fn a_sync_before_the_action_finishes_keeps_the_row_hidden() {
        let fx = Fixture::new(&["work"]);
        inbox(&fx, &[1, 2, 3]);
        let (mut driver, wires) = fx.driver();
        driver.press("e");
        let synced = Event::Synced {
            account: "work".into(),
            new_messages: 0,
            actions: 0,
            requests: vec![],
            errors: vec![],
        };
        receive(&mut driver, &wires, [synced]);
        assert_eq!(driver.uids(), [2, 1]);
        fx.store("work").remove_message("INBOX", 3).unwrap();
        receive(&mut driver, &wires, [done("work", vec![(3, Ok(1))], 0)]);
        assert_eq!(driver.uids(), [2, 1]);
    }

    #[test]
    fn acting_on_an_empty_folder_sends_nothing() {
        let fx = Fixture::new(&["work"]);
        inbox(&fx, &[1]);
        let (mut driver, wires) = fx.driver();
        driver.press("e");
        assert_eq!(wires.sent().len(), 1);
        driver.press("e # u s x j");
        assert!(wires.sent().is_empty());
        assert_eq!(driver.app.list.cursor, 0);
    }

    #[test]
    fn a_second_edit_on_a_row_stacks_and_the_first_done_keeps_it() {
        let fx = Fixture::new(&["work"]);
        fx.add("work", unread("INBOX", 1, "hello"));
        let (mut driver, wires) = fx.driver();
        driver.press("u s");
        let row = &driver.app.list.rows[0];
        assert!(!row.unread && row.flagged);
        receive(&mut driver, &wires, [done("work", vec![(1, Ok(1))], 0)]);
        assert!(driver.app.list.rows[0].flagged);
        assert_eq!(driver.app.accounts[0].queued, 1);
    }

    #[test]
    fn switching_views_closes_the_move_picker() {
        let fx = Fixture::new(&["work"]);
        fx.folder("work", "Archive", Some("Archive"));
        inbox(&fx, &[1]);
        let (mut driver, _wires) = fx.driver();
        driver.press("m");
        assert!(driver.app.move_picker.is_some());
        driver.apply(UiAction::SelectView(View::Rules));
        assert!(driver.app.move_picker.is_none());
    }

    #[test]
    fn the_unread_count_follows_sent_actions_before_the_daemon_answers() {
        let fx = Fixture::new(&["work"]);
        for uid in [1, 2] {
            fx.add("work", unread("INBOX", uid, "hello"));
        }
        let (mut driver, _wires) = fx.driver();
        assert_eq!(inbox_unread(&driver), Some(2));
        driver.press("u");
        assert_eq!(inbox_unread(&driver), Some(1));
        driver.press("j e");
        assert_eq!(inbox_unread(&driver), Some(0));
    }

    #[test]
    fn the_next_rows_body_is_fetched_before_the_cursor_reaches_it() {
        let fx = Fixture::new(&["work"]);
        inbox(&fx, &[1, 3]);
        let mut later = message("INBOX", 2, "later");
        later.body_text = None;
        fx.add("work", later);
        let (mut driver, wires) = fx.driver();
        let fetch = Command::FetchBody {
            folder: "INBOX".into(),
            uid: 2,
        };
        assert_eq!(wires.sent(), [("work".to_string(), fetch)]);
        driver.press("j");
        assert!(wires.sent().is_empty());
    }

    #[test]
    fn u_marks_read_and_s_flags() {
        let fx = Fixture::new(&["work"]);
        fx.add("work", unread("INBOX", 1, "hello"));
        let (mut driver, wires) = fx.driver();
        driver.press("u");
        assert!(!driver.app.list.rows[0].unread);
        driver.press("s");
        assert_eq!(
            wires.sent(),
            [apply(&[1], Action::MarkRead), apply(&[1], Action::Flag)]
        );
        assert!(driver.app.list.rows[0].flagged);
    }

    #[test]
    fn m_filters_the_folders_and_enter_moves() {
        let fx = Fixture::new(&["work"]);
        fx.folder("work", "Archive", Some("Archive"));
        fx.folder("work", "Receipts", None);
        inbox(&fx, &[1]);
        let (mut driver, wires) = fx.driver();
        driver.press("m");
        assert_eq!(driver.app.move_targets(), ["Archive", "Receipts"]);
        driver.press("rec");
        assert_eq!(driver.app.move_targets(), ["Receipts"]);
        driver.press("enter");
        assert_eq!(wires.sent(), [apply(&[1], Action::Move("Receipts".into()))]);
        assert!(driver.app.move_picker.is_none());
    }

    #[test]
    fn command_r_syncs_every_running_account() {
        let fx = Fixture::new(&["home", "work"]);
        let (mut driver, wires) = fx.driver();
        driver.press("ctrl+r");
        assert_eq!(
            wires.sent(),
            [
                ("home".to_string(), Command::SyncNow),
                ("work".to_string(), Command::SyncNow)
            ]
        );
    }

    #[test]
    fn backspace_deletes_on_macos_only() {
        let fx = Fixture::new(&["work"]);
        inbox(&fx, &[1]);
        let (mut driver, wires) = fx.driver();
        driver.press("backspace");
        let expected = if cfg!(target_os = "macos") {
            vec![apply(&[1], Action::Trash)]
        } else {
            Vec::new()
        };
        assert_eq!(wires.sent(), expected);
    }

    #[test]
    fn closing_the_event_stream_shows_the_reconnect_line() {
        let fx = Fixture::new(&["work"]);
        let (mut driver, wires) = fx.driver();
        drop(wires.events);
        driver.tick();
        assert!(driver.app.daemon_lost);
        assert_eq!(driver.status().last().unwrap(), DAEMON_LOST);
    }

    /// The daemon end of the client a reconnect hands over; one per test binary, as `reconnect` is a plain `fn`.
    static RECONNECTED: Mutex<Option<Receiver<(String, Command)>>> = Mutex::new(None);

    fn idle(name: &str) -> AccountStatus {
        AccountStatus {
            name: name.into(),
            activity: Some(Activity::Idle {
                since: 1_790_000_000,
            }),
        }
    }

    #[test]
    fn a_reconnect_replaces_the_client_and_refreshes_the_accounts() {
        let fx = Fixture::new(&["work"]);
        inbox(&fx, &[1]);
        let (mut driver, wires) = fx.driver();
        driver.app.reconnect = |_, _| {
            let (client, commands, events) = Client::in_memory(&["work"]);
            *RECONNECTED.lock().unwrap() = Some(commands);
            // A dropped sender would read as another loss.
            std::mem::forget(events);
            let (client, events, _) = session(client, Box::new(|| {}))?;
            Ok((client, events, vec![idle("work")]))
        };
        drop(wires);
        driver.tick();
        driver.press("e");
        assert_eq!(driver.status()[0], format!("work: {DAEMON_LOST}"));
        fx.add("work", unread("INBOX", 2, "while away"));
        tick_until(&mut driver, 10.0, |app| !app.daemon_lost);
        driver.tick();
        assert!(
            driver
                .status()
                .iter()
                .all(|line| !line.contains(DAEMON_LOST))
        );
        let line = format!("work: up to date · {}", crate::time::clock(1_790_000_000));
        assert_eq!(driver.status(), [line]);
        assert_eq!(inbox_unread(&driver), Some(1));
        driver.press("e");
        let sent: Vec<_> = RECONNECTED
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .try_iter()
            .collect();
        assert_eq!(sent, [apply(&[1], Action::Archive)]);
    }

    #[test]
    fn a_newer_daemon_is_named_on_the_status_line_and_retried() {
        let fx = Fixture::new(&["work"]);
        let (mut driver, wires) = fx.driver();
        driver.app.reconnect = |_, _| {
            Err(crate::daemon::client::NewerDaemon {
                theirs: "9.0.0".into(),
                ours: "0.1.0".into(),
            }
            .into())
        };
        drop(wires);
        driver.tick();
        let newer =
            "the daemon is version 9.0.0, newer than this postvak (0.1.0); restart this program";
        tick_until(&mut driver, 10.0, |app| app.error.as_deref() == Some(newer));
        assert!(driver.app.daemon_lost);
        driver.at(20.0);
        assert!(
            driver.app.reconnecting.is_some(),
            "it tries again 5 s later"
        );
    }

    #[test]
    fn the_window_opens_with_the_daemons_activity() {
        let fx = Fixture::new(&["work"]);
        let config = Config::load(&fx.paths.config_file()).unwrap();
        let (client, _commands, _events) = Client::in_memory(&["work"]);
        let events = client.subscribe().unwrap();
        let app = App::new(
            &config,
            fx.paths.clone(),
            client,
            events,
            vec![idle("work")],
        );
        assert_eq!(
            app.accounts[0].activity,
            Some(Activity::Idle {
                since: 1_790_000_000
            })
        );
    }

    #[test]
    fn a_refused_command_shows_its_reason() {
        let fx = Fixture::new(&["work"]);
        let (mut driver, wires) = fx.driver();
        let reason = "work is offline (x); retrying at 10:00";
        receive(&mut driver, &wires, [refused("work", 0, reason)]);
        assert_eq!(driver.status()[0], format!("work: {reason}"));
    }

    #[test]
    fn another_clients_outcomes_leave_this_windows_edits_alone() {
        let fx = Fixture::new(&["work"]);
        inbox(&fx, &[1, 2, 3]);
        let (mut driver, wires) = fx.driver();
        driver.press("e");
        let foreign = [
            done("work", vec![(3, Ok(1))], 7),
            refused("work", 8, "no rule named 'x'"),
        ];
        receive(&mut driver, &wires, foreign);
        assert_eq!(driver.uids(), [2, 1]);
        assert_eq!(driver.app.accounts[0].queued, 1);
        assert!(
            driver
                .status()
                .iter()
                .all(|line| !line.contains("no rule named"))
        );
    }

    #[test]
    fn a_refused_action_puts_the_row_back_and_clears_the_queue() {
        let fx = Fixture::new(&["work"]);
        inbox(&fx, &[1, 2, 3]);
        let (mut driver, wires) = fx.driver();
        driver.press("e");
        assert_eq!(driver.uids(), [2, 1]);
        let reason = "work is offline (x); retrying at 10:00";
        receive(&mut driver, &wires, [refused("work", 0, reason)]);
        assert_eq!(driver.uids(), [3, 2, 1]);
        assert_eq!(driver.app.accounts[0].queued, 0);
        assert!(driver.status().iter().all(|line| !line.contains("queued")));
    }

    fn update_done(app: &App) -> bool {
        matches!(app.update, UpdateCheck::Done(_))
    }

    #[test]
    fn a_cached_newer_release_reaches_the_window() {
        let fx = Fixture::new(&["work"]);
        fx.append_config("[ui]\ncheck_updates = true\n");
        let cached = update::Cached {
            checked_at: crate::time::now(),
            latest: Some("999.0.0".into()),
        };
        update::save(&fx.paths.update_check(), &cached).unwrap();
        let (mut driver, _wires) = fx.driver();
        tick_until(&mut driver, 0.0, update_done);
        assert_eq!(driver.app.newer_release(), Some("999.0.0"));
    }

    #[test]
    fn a_failed_check_shows_nothing_and_is_remembered() {
        let fx = Fixture::new(&["work"]);
        fx.append_config("[ui]\ncheck_updates = true\n");
        let (mut driver, _wires) = fx.driver();
        tick_until(&mut driver, 0.0, update_done);
        assert_eq!(driver.app.newer_release(), None);
        assert!(update::load(&fx.paths.update_check()).is_some());
    }

    #[test]
    fn by_default_no_check_starts() {
        let fx = Fixture::new(&["work"]);
        let (mut driver, _wires) = fx.driver();
        driver.tick();
        assert!(matches!(driver.app.update, UpdateCheck::Off));
        assert!(!fx.paths.update_check().exists());
    }

    /// home INBOX uid 1, work INBOX uids 1 and 2: rows (work 2), (home 1), (work 1), equal dates in account order.
    fn all_inboxes(fx: &Fixture) -> (Driver, Wires) {
        fx.add("home", message("INBOX", 1, "home mail"));
        fx.add("work", message("INBOX", 1, "work mail"));
        fx.add("work", message("INBOX", 2, "newer work mail"));
        let (mut driver, wires) = fx.driver();
        driver.apply(UiAction::SelectView(View::AllInboxes));
        assert_eq!(driver.ids(), [(1, 2), (0, 1), (1, 1)]);
        wires.sent();
        (driver, wires)
    }

    #[test]
    fn all_inboxes_sends_each_action_to_the_rows_own_account() {
        let fx = Fixture::new(&["home", "work"]);
        let (mut driver, wires) = all_inboxes(&fx);
        driver.press("j e");
        assert_eq!(
            wires.sent(),
            [command("home", "INBOX", &[1], Action::Archive)]
        );
        assert_eq!(
            driver.ids(),
            [(1, 2), (1, 1)],
            "work's uid 1 is another message and stays"
        );
        driver.press("s u");
        assert_eq!(
            wires.sent(),
            [
                command("work", "INBOX", &[1], Action::Flag),
                command("work", "INBOX", &[1], Action::MarkUnread)
            ]
        );
    }

    #[test]
    fn marking_in_all_inboxes_marks_only_that_accounts_message() {
        let fx = Fixture::new(&["home", "work"]);
        let (mut driver, wires) = all_inboxes(&fx);
        driver.press("j x");
        assert_eq!(
            driver.app.list.marked,
            HashSet::from([(0, ("INBOX".to_string(), 1))])
        );
        driver.press("j x #");
        assert_eq!(
            wires.sent(),
            [
                command("home", "INBOX", &[1], Action::Trash),
                command("work", "INBOX", &[1], Action::Trash)
            ]
        );
        assert_eq!(driver.ids(), [(1, 2)]);
    }

    #[test]
    fn the_reader_opens_and_marks_read_in_the_rows_own_account() {
        let fx = Fixture::new(&["home", "work"]);
        fx.add("home", unread("INBOX", 1, "home mail"));
        fx.add("work", message("INBOX", 2, "work mail"));
        let (mut driver, wires) = fx.driver();
        driver.apply(UiAction::SelectView(View::AllInboxes));
        driver.at(10.0);
        driver.press("j");
        let body = driver.app.body.as_ref().unwrap();
        assert_eq!(body.account, 0);
        assert_eq!(body.text.as_deref(), Some("Body of 1"));
        driver.at(11.1);
        assert_eq!(
            wires.sent(),
            [command("home", "INBOX", &[1], Action::MarkRead)]
        );
    }

    #[test]
    fn the_move_picker_offers_the_rows_own_account_folders() {
        let fx = Fixture::new(&["home", "work"]);
        fx.folder("home", "Receipts", None);
        fx.folder("work", "Archive", Some("Archive"));
        let (mut driver, wires) = all_inboxes(&fx);
        driver.press("j m enter");
        assert_eq!(
            wires.sent(),
            [command(
                "home",
                "INBOX",
                &[1],
                Action::Move("Receipts".into())
            )]
        );
    }

    #[test]
    fn moving_marked_rows_from_two_accounts_is_refused() {
        let fx = Fixture::new(&["home", "work"]);
        let (mut driver, wires) = all_inboxes(&fx);
        driver.press("x j x m");
        assert!(driver.app.move_picker.is_none());
        assert_eq!(driver.app.error.as_deref(), Some(MOVE_ONE_ACCOUNT));
        assert!(wires.sent().is_empty());
        assert_eq!(
            driver.app.list.marked.len(),
            2,
            "the marks stay for the user to narrow"
        );
    }

    #[test]
    fn a_refusal_for_one_account_puts_back_only_its_rows() {
        let fx = Fixture::new(&["home", "work"]);
        let (mut driver, wires) = all_inboxes(&fx);
        driver.press("x j x e");
        assert_eq!(wires.sent().len(), 2);
        assert_eq!(driver.ids(), [(1, 1)]);
        let reason = "home is offline (x); retrying at 10:00";
        receive(&mut driver, &wires, [refused("home", 0, reason)]);
        assert_eq!(driver.ids(), [(0, 1), (1, 1)]);
        assert_eq!(driver.app.accounts[1].queued, 1);
    }

    #[test]
    fn a_move_is_refused_when_the_selection_grew_to_two_accounts_under_the_open_picker() {
        let fx = Fixture::new(&["home", "work"]);
        fx.folder("home", "Receipts", None);
        let (mut driver, wires) = all_inboxes(&fx);
        driver.press("j m");
        assert!(driver.app.move_picker.is_some());
        driver
            .app
            .list
            .marked
            .extend([(0, ("INBOX".to_string(), 1)), (1, ("INBOX".to_string(), 1))]);
        driver.apply(UiAction::MoveTo("Receipts".into()));
        assert!(wires.sent().is_empty());
        assert!(driver.app.move_picker.is_none());
        assert_eq!(driver.app.error.as_deref(), Some(MOVE_ONE_ACCOUNT));
    }
}
