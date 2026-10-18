//! App state and the only code that changes it: `handle` for daemon events, `apply` for what the window asks, `tick`
//! for time. No toolkit here: `bind.rs` shows this state in the Slint window and turns its callbacks into `UiAction`s.
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, TryRecvError};
use std::time::Duration;

use crate::update;
use postvak_core::bimi::Brand;
use postvak_core::config::{self, AccountConfig, Config, Theme};
use postvak_core::daemon::Client;
use postvak_core::daemon::client::NewerDaemon;
use postvak_core::daemon::wire::AccountStatus;
use postvak_core::message::{self, Attachment, HtmlBody, clean};
use postvak_core::paths::{FileWatch, Paths};
use postvak_core::rules::{self as rule_file, Action, Rule, RuleFile, RulesError};
use postvak_core::store::{Filter, LogEntry, Message, Store, StoreError};
use postvak_core::sync::{Activity, Command, Event};
use postvak_core::trash::Trash;
use postvak_core::unsubscribe::{self, Method};

use super::account::{self, AccountForm};
use super::body;
use super::draft::{self, Draft};
use super::folders;
use super::html::{self, Fetcher, HtmlState, Renderer, Reply, Wake};
use super::list::{self, ListState, Optimistic, Row, RowKey};
use super::load::{self, ListQuery, LoadFn, Loaded, Loading};
use super::logos::Logos;
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

/// How long a frame waits for a list reload after a sync event: a quarter of a 120 Hz frame. A small mailbox's rows come
/// in time and show in the same frame; a large one's show when they come.
pub(crate) const LIST_BUDGET: Duration = Duration::from_millis(2);

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

/// The one-click unsubscribe in flight, for the message it came from.
pub(crate) struct Unsubscribing {
    account: usize,
    key: RowKey,
    answer: Receiver<Result<(), String>>,
}

pub(crate) struct BodyState {
    pub account: usize,
    /// The sender's logo record, when sender images are on and the message passed DMARC.
    pub brand: Option<Brand>,
    /// Set when the user chose this message, not when a view change or reload put it on screen; only then does it
    /// become read.
    pub armed: bool,
    pub attachments: Vec<Attachment>,
    /// The message's HTML, when it has some.
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
    /// Edits whose `ActionDone` arrived, with the first list load that reads the store after it; shown until a load
    /// that new is applied, so an older load still running does not bring the row back as it was.
    pub settling: Vec<(RowKey, Optimistic, u64)>,
    pub queued: usize,
    pub store: Result<Store, String>,
}

impl Account {
    /// The edits the list shows: the settling ones, which are older, then the pending ones.
    pub fn shown_edits(&self) -> std::borrow::Cow<'_, HashMap<RowKey, Vec<Optimistic>>> {
        if self.settling.is_empty() {
            return std::borrow::Cow::Borrowed(&self.pending);
        }
        let mut edits: HashMap<RowKey, Vec<Optimistic>> = HashMap::new();
        for (key, edit, _) in &self.settling {
            edits.entry(key.clone()).or_default().push(*edit);
        }
        for (key, pending) in &self.pending {
            edits.entry(key.clone()).or_default().extend(pending);
        }
        std::borrow::Cow::Owned(edits)
    }

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
        let was_unread = !stored.is_seen();
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
    /// Take every row out of the selection.
    ClearSelection,
    /// The rule editor's form changed.
    EditDraft(Box<Draft>),
    /// Open a rule in the editor.
    EditRule(String),
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
    /// Open an empty rule in the editor.
    NewRule,
    NextFocus,
    /// Open the add-account form.
    OpenAccountForm,
    OpenMovePicker,
    /// Open a web page in the browser.
    OpenUrl(String),
    OpenRulesFile,
    OpenSource,
    /// Count what a rule would do to the cached mail, without doing it.
    PreviewRule(String),
    RejectRule(String),
    /// Remove the open rule from rules.toml; unconfirmed, it asks first.
    RemoveRule {
        confirmed: bool,
    },
    Restore(usize, PathBuf),
    /// Save the form's account, replacing one of the same name.
    SaveAccount(account::Fields),
    SaveAttachment(usize),
    /// Write the open rule to rules.toml.
    SaveDraft,
    SaveEml,
    /// Leave the shown message's mailing list: by one click, or by opening its page or address.
    Unsubscribe,
    /// Enter in the search field: ask the servers too, and open the first hit.
    SearchAccepted,
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
    /// Count what the open rule would do to the cached mail.
    TestDraft,
    /// Remove each account's backups past its retention
    PurgeTrash,
    /// Run an enabled rule once over the stored mail of the accounts it covers
    ApplyRule(String),
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
    /// The answers to "This rule has unsaved changes."
    UnsavedCancel,
    UnsavedDiscard,
    UnsavedSave,
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
    pub(crate) config_watch: FileWatch,
    /// Set when the event stream ended; cleared once a reconnect succeeds.
    pub(crate) daemon_lost: bool,
    pub(crate) downloads: PathBuf,
    pub(crate) error: Option<String>,
    /// The outcome of the last purge or rule run, shown in the status row until the next sync or the history opens.
    pub(crate) notice: Option<String>,
    /// Each account's trash retention in days, from config.toml as the window opened or added the account.
    pub(crate) retention: BTreeMap<String, u64>,
    pub(crate) events: Receiver<Event>,
    pub(crate) fetch_release: fn() -> anyhow::Result<String>,
    /// Sends a one-click unsubscribe; tests swap it so nothing leaves the machine.
    pub(crate) post_unsubscribe: fn(&str) -> Result<(), String>,
    pub(crate) unsubscribing: Option<Unsubscribing>,
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
    /// Sender logos, with `[ui] sender_images = true`.
    pub(crate) logos: Logos,
    /// How long `tick` waits for a list reload before it lets the window draw and applies the rows when they come;
    /// tests wait as long as it takes.
    pub(crate) list_budget: Duration,
    /// Numbers list loads; the last one started, and the last one whose rows are shown.
    pub(crate) list_generation: u64,
    pub(crate) list_applied: u64,
    /// The reload running on its own thread; at most one runs.
    pub(crate) list_loading: Option<Loading>,
    /// Set when the view changed again while `list_loading` ran, so another reload follows it.
    pub(crate) list_again: bool,
    /// Set when edits went or settled since the rows were built, so a reload that runs on rebuilds them from the
    /// threads already loaded; rebuilding a large list costs milliseconds, so not on every sync event.
    pub(crate) edits_changed: bool,
    /// Reads the list off the window thread; tests swap it to hold a load back.
    pub(crate) load_list: LoadFn,
    pub(crate) move_picker: Option<String>,
    /// The add-account form while it is open.
    pub(crate) account_form: Option<AccountForm>,
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
    pub(crate) rules_watch: FileWatch,
    /// The rule open in the Rules view's editor; nothing in it is written until Save.
    pub(crate) rule_draft: Option<Draft>,
    /// What `rule_draft` was opened or last saved as, which tells an edit from a look.
    pub(crate) rule_draft_opened: Option<Draft>,
    /// Opening another rule while the open one has unsaved changes: held until the prompt is answered.
    pub(crate) leave_draft: Option<Box<UiAction>>,
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

/// An account's window state over its store; `open_account` creates the store of an account just added.
fn account_state(paths: &Paths, name: String, activity: Option<Activity>) -> Account {
    let store =
        Store::open_account(paths, &name).map_err(|e| format!("could not open the store: {e}"));
    let mut account = Account {
        activity,
        error: None,
        folders: Vec::new(),
        name,
        pending: HashMap::new(),
        settling: Vec::new(),
        queued: 0,
        store,
    };
    account.reload_folders();
    account
}

impl App {
    pub fn new(
        config: &Config,
        paths: Paths,
        client: Client,
        events: Receiver<Event>,
        states: Vec<AccountStatus>,
    ) -> App {
        let retention = config
            .accounts
            .iter()
            .map(|a| (a.name.clone(), a.trash_retention_days))
            .collect();
        let accounts = states
            .into_iter()
            .map(|AccountStatus { activity, name }| account_state(&paths, name, activity))
            .collect();
        let rules_path = paths.rules_file();
        let rules = RulesState::load(&rules_path, Vec::new());
        let mut app = App {
            accounts,
            activity_log: Vec::new(),
            body: None,
            client,
            collapsed: HashSet::new(),
            config_changed: false,
            config_watch: FileWatch::new(paths.config_file()),
            daemon_lost: false,
            downloads: downloads_dir(),
            error: None,
            events,
            fetch_release: update::fetch,
            post_unsubscribe: |url| unsubscribe::one_click(url).map_err(|e| e.to_string()),
            unsubscribing: None,
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
            logos: Logos::new(config.ui.sender_images, paths.cache_dir.clone()),
            list_budget: LIST_BUDGET,
            list_generation: 0,
            list_applied: 0,
            list_loading: None,
            list_again: false,
            edits_changed: false,
            load_list: load::load_from_paths,
            move_picker: None,
            account_form: None,
            paths,
            pending_arm: false,
            reconnect: connect,
            renderer: None,
            reconnecting: None,
            requested: HashSet::new(),
            rules,
            rules_watch: FileWatch::new(rules_path),
            rule_draft: None,
            rule_draft_opened: None,
            leave_draft: None,
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
            retention,
            notice: None,
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
        self.logos.receive();
        self.keep_daemon(now);
        self.check_release();
        self.receive_unsubscribe();
        self.receive_saved_account();
        self.receive_list();
        if std::mem::take(&mut self.view_dirty) {
            self.refresh_view();
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
                        postvak_core::time::now(),
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
            account.settling.clear();
            account.queued = 0;
            account.reload_folders();
        }
        self.edits_changed = true;
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
            | Event::ServerSearchDone { account, .. }
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
            Event::ServerSearchDone { query, hits, .. } => {
                if self.search.as_deref() == Some(query.as_str()) {
                    self.list.server_searching = self.list.server_searching.saturating_sub(1);
                    self.list
                        .server_hits
                        .extend(hits.into_iter().map(|hit| (index, hit)));
                    self.view_dirty = true;
                }
            }
            Event::CommandFailed { message, .. } => {
                // The refusal does not say of what; a search that failed must not say it is still running.
                self.list.server_searching = self.list.server_searching.saturating_sub(1);
                // A refusal carries no uids, so every edit of the account goes and the store shows what is true.
                let account = &mut self.accounts[index];
                account.pending.clear();
                account.settling.clear();
                account.queued = 0;
                self.edits_changed = true;
                self.note_error(Some(index), message);
                self.refresh(index);
            }
            Event::Error { message, .. } => self.note_error(Some(index), message),
            Event::RuleApplied {
                evaluated,
                actions,
                errors,
                ..
            } => {
                // Accounts report one by one; the status row shows the latest and the history keeps them all.
                let action_noun = if actions == 1 { "action" } else { "actions" };
                let message_noun = if evaluated == 1 {
                    "message"
                } else {
                    "messages"
                };
                let line = format!(
                    "{}: {actions} {action_noun} on {evaluated} {message_noun}",
                    self.accounts[index].name
                );
                self.notice(line);
                for message in errors {
                    self.note_error(Some(index), message);
                }
                self.refresh(index);
            }
            Event::BodiesFetched { .. } => self.refresh(index),
            Event::Synced { .. } => {
                // A fresh sync result replaces the last notice, which would otherwise outrank every account line.
                self.notice = None;
                self.refresh(index);
            }
            Event::ActionDone {
                folder, results, ..
            } => {
                let account = &mut self.accounts[index];
                account.queued = account.queued.saturating_sub(1);
                // The next load reads the store after this change; until one does, a done edit stays shown. A failed
                // one changed nothing, so it goes at once.
                let settles = self.list_generation + 1;
                for (uid, result) in &results {
                    let key = (folder.clone(), *uid);
                    if let Some(edits) = account.pending.get_mut(&key) {
                        let edit = edits.remove(0);
                        if result.is_ok() {
                            account.settling.push((key.clone(), edit, settles));
                        }
                        if edits.is_empty() {
                            account.pending.remove(&key);
                        }
                    }
                }
                self.edits_changed = true;
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
            at: postvak_core::time::now(),
            text,
        });
        while self.history.len() > HISTORY {
            self.history.pop_front();
        }
    }

    /// Applies `action`; when it moved the cursor, by key, click, search or collapse, the list scrolls to it.
    pub fn apply(&mut self, action: UiAction) {
        postvak_core::crash::note(&action);
        let cursor = self.list.cursor;
        self.apply_action(action);
        if self.list.cursor != cursor && !self.list.cursor_shown() {
            self.list.follow_cursor = true;
        }
    }

    fn apply_action(&mut self, action: UiAction) {
        match action {
            leave @ (UiAction::EditRule(_) | UiAction::NewRule) if self.draft_edited() => {
                self.leave_draft = Some(Box::new(leave));
            }
            UiAction::Act(action) => self.act(action),
            UiAction::ApproveRule(name) => {
                let accounts: Vec<String> = self.accounts.iter().map(|a| a.name.clone()).collect();
                let paths = self.paths.clone();
                self.edit_rules(|_| {
                    rule_file::edit::approve_from_now(
                        &paths,
                        &accounts,
                        &name,
                        postvak_core::time::now(),
                    )
                });
            }
            UiAction::Collapse => self.collapse(),
            UiAction::DropOn(view) => self.drop_on(view),
            UiAction::HtmlHover(at) => self.html_hover(at),
            UiAction::HtmlSelect { from, to } => self.html_select(from, to),
            UiAction::HtmlVisible { top, bottom } => self.html_visible(top, bottom),
            UiAction::HtmlWidth { scale, width } => self.html_size = Some((scale, width)),
            UiAction::LoadRemote => self.load_remote(),
            UiAction::ClearSelection => self.list.marked.clear(),
            UiAction::Escape => {
                if let Some(form) = &self.account_form {
                    // A save in flight finishes first, so its account joins the window.
                    if form.saving.is_none() {
                        self.account_form = None;
                    }
                } else if self.move_picker.is_some() {
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
            UiAction::EditDraft(draft) => {
                self.rule_draft = Some(Draft {
                    error: None,
                    tested: None,
                    ..*draft
                });
            }
            UiAction::EditRule(name) => {
                let rule = self.rules.rules.iter().find(|rule| rule.name == name);
                if let Some(draft) = rule.map(Draft::from_rule) {
                    self.open_draft(Some(draft));
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
            UiAction::NewRule => self.open_draft(Some(Draft::new())),
            UiAction::OpenSource => self.open_source(),
            UiAction::OpenUrl(url) => {
                if let Err(e) = (self.launch)(url.as_ref(), false) {
                    self.note_error(None, format!("could not open the link: {e}"));
                }
            }
            UiAction::OpenRulesFile => self.open_rules_file(),
            UiAction::PreviewRule(name) => {
                if let Some(rule) = self.rules.rules.iter().find(|rule| rule.name == name) {
                    let text = self.preview(rule.clone());
                    self.rules.previews.insert(name, text);
                }
            }
            UiAction::RejectRule(name) => {
                self.edit_rules(|path| rule_file::edit::reject(path, &name));
            }
            UiAction::RemoveRule { confirmed } => self.remove_draft(confirmed),
            UiAction::SaveDraft => {
                self.save_draft();
            }
            UiAction::TestDraft => {
                if let Some(draft) = &self.rule_draft {
                    let tested = match draft.to_rule() {
                        Ok(rule) => self.preview(rule),
                        Err(e) => e,
                    };
                    if let Some(draft) = &mut self.rule_draft {
                        draft.tested = Some(tested);
                    }
                }
            }
            UiAction::UnsavedCancel => self.leave_draft = None,
            UiAction::UnsavedDiscard => {
                if let Some(leave) = self.leave_draft.take() {
                    self.open_draft(None);
                    self.apply_action(*leave);
                }
            }
            UiAction::UnsavedSave => {
                if let Some(leave) = self.leave_draft.take()
                    && self.save_draft()
                {
                    self.apply_action(*leave);
                }
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
            UiAction::Unsubscribe => self.unsubscribe(),
            UiAction::SearchFocused(focused) => self.search_focused = focused,
            UiAction::SearchAccepted => {
                self.search_server();
                self.apply_action(UiAction::SelectRow(0));
            }
            UiAction::SetFilter(filter) => {
                self.filter = filter;
                self.reload_view();
            }
            UiAction::SearchFor(query) => {
                self.search = Some(query);
                self.list.cursor = 0;
                self.list.rows.clear();
                self.list.server_hits.clear();
                self.list.server_searching = 0;
                self.list.server_asked = false;
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
                // The open rule follows the switch, so a later Save keeps it; it does not count as an edit.
                for draft in [&mut self.rule_draft, &mut self.rule_draft_opened]
                    .into_iter()
                    .flatten()
                {
                    if draft.original.as_deref() == Some(name.as_str()) {
                        draft.enabled = enabled;
                    }
                }
            }
            UiAction::SetTheme(theme) => {
                self.theme = theme;
                let path = self.paths.config_file();
                match config::save_theme(&path, theme) {
                    // Absorbs this write, so the poll does not take it for an outside edit.
                    Ok(()) => {
                        self.config_watch.changed();
                    }
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
            UiAction::OpenAccountForm => {
                self.account_form.get_or_insert_with(AccountForm::default);
            }
            UiAction::SaveAccount(fields) => self.save_account(&fields),
            UiAction::ToggleHtml => {
                if let Some(body) = self.body.as_mut().filter(|b| b.html.is_some()) {
                    body.show_text = !body.show_text;
                }
            }
            UiAction::PurgeTrash => self.purge_trash(),
            UiAction::ApplyRule(name) => self.apply_rule(&name),
            UiAction::ToggleHistory => {
                self.history_open = !self.history_open;
                self.error = None;
                self.notice = None;
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
        self.search_focused || self.move_picker.is_some() || self.account_form.is_some()
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
        self.has_role(*account, folder, "Drafts") || self.has_role(*account, folder, "Sent")
    }

    /// The accounts whose mail the view lists: the shown folder's, or all of them.
    fn covered_accounts(&self) -> Vec<usize> {
        match self.view {
            View::Folder { account, .. } => vec![account],
            _ => (0..self.accounts.len()).collect(),
        }
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

    /// Loads the shown view's rows from the store, on this thread: for view changes, where the user waits for the new
    /// rows anyway. A reload still running is dropped, since this one reads everything it would.
    pub(crate) fn reload_view(&mut self) {
        self.list_generation += 1;
        let generation = self.list_generation;
        self.list_loading = None;
        self.list_again = false;
        let Some(query) = self.list_query() else {
            match self.view {
                View::Activity => self.activity_log = rules::activity_log(&self.accounts),
                View::Trash => self.trash = rules::trash_rows(&self.accounts, &self.paths),
                _ => {}
            }
            return;
        };
        // A store that failed to open shows its error in the folder pane and adds no rows.
        let stores: Vec<(usize, &str, &Store)> = self
            .covered_accounts()
            .into_iter()
            .filter_map(|index| {
                let account = &self.accounts[index];
                account
                    .store
                    .as_ref()
                    .ok()
                    .map(|store| (index, account.name.as_str(), store))
            })
            .collect();
        let loaded = load::load(&stores, &query);
        self.apply_loaded(generation, loaded);
    }

    /// The daemon reported changes to the shown view: reloads it on a thread of its own, so a large mailbox does not
    /// stall the frame. Rows that come within `list_budget` are applied at once; later ones when the thread wakes the
    /// window. While one reload runs, the next waits for it.
    fn refresh_view(&mut self) {
        let Some(query) = self.list_query() else {
            self.reload_view();
            return;
        };
        if self.list_loading.is_some() {
            self.list_again = true;
            self.show_changed_edits();
            return;
        }
        self.list_generation += 1;
        let generation = self.list_generation;
        let accounts = self
            .covered_accounts()
            .into_iter()
            .filter(|&index| self.accounts[index].store.is_ok())
            .map(|index| (index, self.accounts[index].name.clone()))
            .collect();
        let answer = load::spawn(
            self.load_list,
            self.paths.clone(),
            accounts,
            query,
            self.wake.clone(),
        );
        match answer.recv_timeout(self.list_budget) {
            Ok(loaded) => self.apply_loaded(generation, loaded),
            Err(RecvTimeoutError::Timeout) => {
                self.list_loading = Some(Loading { generation, answer });
                self.show_changed_edits();
            }
            Err(RecvTimeoutError::Disconnected) => {
                log::warn!("the list reload stopped; the rows stay as they were");
            }
        }
    }

    /// Edits that went or settled while a reload runs show on the rows already loaded.
    fn show_changed_edits(&mut self) {
        if self.edits_changed {
            self.rebuild_rows();
        }
    }

    /// Applies a reload that outlasted `list_budget`, then starts the one the view asked for meanwhile.
    fn receive_list(&mut self) {
        let Some(loading) = &self.list_loading else {
            return;
        };
        let loaded = match loading.answer.try_recv() {
            Err(TryRecvError::Empty) => return,
            Ok(loaded) => Some(loaded),
            Err(TryRecvError::Disconnected) => {
                log::warn!("the list reload stopped; the rows stay as they were");
                None
            }
        };
        let generation = loading.generation;
        self.list_loading = None;
        if let Some(loaded) = loaded {
            self.apply_loaded(generation, loaded);
        }
        if std::mem::take(&mut self.list_again) {
            self.view_dirty = true;
        }
    }

    /// What the shown view's list reads, or `None` for a view without one.
    fn list_query(&self) -> Option<ListQuery> {
        let folder = self.list_folder()?.to_string();
        Some(ListQuery {
            folder,
            filter: self.filter,
            search: self.search.clone(),
            server_hits: self.list.server_hits.clone(),
            expanded: self.list.expanded.keys().cloned().collect(),
        })
    }

    /// Shows a load's rows, unless a later load is already shown; edits that it reads as stored stop settling.
    fn apply_loaded(&mut self, generation: u64, loaded: Loaded) {
        if generation < self.list_applied {
            return;
        }
        self.list_applied = generation;
        for account in &mut self.accounts {
            account
                .settling
                .retain(|(_, _, settles)| *settles > generation);
        }
        if let Some(hits) = loaded.hits {
            self.list.hits = hits;
        } else {
            let found = loaded
                .threads
                .into_iter()
                .map(|(account, threads)| {
                    // A store that could not list its threads keeps the ones shown.
                    let threads = threads.unwrap_or_else(|| {
                        self.list
                            .threads
                            .iter()
                            .filter(|(owner, _)| *owner == account)
                            .map(|(_, thread)| thread.clone())
                            .collect()
                    });
                    (account, threads)
                })
                .collect();
            self.list.threads = list::merge_threads(found, list::THREAD_LIMIT);
            for (key, members) in loaded.members {
                // A thread collapsed while the load ran stays collapsed.
                if !self.list.expanded.contains_key(&key) {
                    continue;
                }
                match members {
                    Some(members) => {
                        self.list.expanded.insert(key, members);
                    }
                    None => {
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
        let (attachments, html) = raw
            .as_deref()
            .map(message::attachments_and_html)
            .unwrap_or_default();
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
        let brand = stored.as_ref().and_then(|m| self.logos.brand(&m.headers));
        if let Some(brand) = &brand {
            self.logos.want(brand, &self.wake);
        }
        BodyState {
            account,
            brand,
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
        // The selection hides the message.
        if self.selected_rows().len() > 1 {
            return;
        }
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

    fn unsubscribe(&mut self) {
        let Some(body) = &mut self.body else { return };
        let method = body
            .message
            .as_ref()
            .and_then(|m| unsubscribe::method(&m.headers));
        match method {
            Some(Method::OneClick(url)) => {
                let (done, answer) = mpsc::channel();
                let (post, wake) = (self.post_unsubscribe, self.wake.clone());
                std::thread::spawn(move || {
                    let _ = done.send(post(&url));
                    wake();
                });
                body.saved = Some("Unsubscribing…".into());
                self.unsubscribing = Some(Unsubscribing {
                    account: body.account,
                    key: body.key.clone(),
                    answer,
                });
            }
            Some(Method::Link(url) | Method::Mail(url)) => {
                self.apply_action(UiAction::OpenUrl(url))
            }
            None => {}
        }
    }

    /// Takes the answer to a one-click unsubscribe; the note goes on its message while that still shows.
    fn receive_unsubscribe(&mut self) {
        let Some(Unsubscribing {
            account,
            key,
            answer,
        }) = &self.unsubscribing
        else {
            return;
        };
        let note = match answer.try_recv() {
            Err(TryRecvError::Empty) => return,
            Ok(Ok(())) => "Unsubscribed: the sender's server accepted it.".to_string(),
            Ok(Err(e)) => format!("Could not unsubscribe: {e}"),
            Err(TryRecvError::Disconnected) => {
                "Could not unsubscribe: the request stopped".to_string()
            }
        };
        if let Some(body) = &mut self.body
            && body.account == *account
            && body.key == *key
        {
            body.saved = Some(note.clone());
        }
        self.push_history(note);
        self.unsubscribing = None;
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
        self.edits_changed = false;
        let Some(folder) = self.list_folder().map(str::to_string) else {
            return;
        };
        let current = self.list.rows.get(self.list.cursor).cloned();
        let mut rows = if self.search.is_some() {
            self.list.hits.clone()
        } else {
            list::build_rows(&folder, &self.list.threads, &self.list.expanded)
        };
        let edits: Vec<_> = self.accounts.iter().map(Account::shown_edits).collect();
        let pending: Vec<_> = edits.iter().map(|edits| edits.as_ref()).collect();
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

    /// Asks the server of each account the search covers for the current query; the hits arrive as `ServerSearchDone`.
    fn search_server(&mut self) {
        // A new query resets the count, so a running round is for this query; a second one would only add duplicates.
        if self.list.server_searching > 0 {
            return;
        }
        let Some(query) = self.search.clone().filter(|query| !query.trim().is_empty()) else {
            return;
        };
        let covered = self.covered_accounts();
        self.list.server_hits.clear();
        self.list.server_searching = 0;
        self.list.server_asked = true;
        for account in covered {
            let command = Command::SearchServer {
                folder: None,
                query: query.clone(),
            };
            if self.send(account, command) {
                self.list.server_searching += 1;
            }
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
        if self.rules_watch.changed() {
            self.rules_changed();
        }
        let saving = self
            .account_form
            .as_ref()
            .is_some_and(|form| form.saving.is_some());
        if !saving && self.config_watch.changed() {
            self.config_changed = true;
        }
    }

    /// Starts the save; a form that cannot make an account says why at once.
    fn save_account(&mut self, fields: &account::Fields) {
        let wake = self.wake.clone();
        let paths = self.paths.clone();
        let Some(form) = self
            .account_form
            .as_mut()
            .filter(|form| form.saving.is_none())
        else {
            return;
        };
        match account::save(paths, fields, wake) {
            Ok(answer) => {
                form.error = None;
                form.saving = Some(answer);
            }
            Err(message) => form.error = Some(message),
        }
    }

    /// Takes the save's answer: the account joins the window, or the form says why not.
    fn receive_saved_account(&mut self) {
        let Some(form) = &mut self.account_form else {
            return;
        };
        let Some(answer) = &form.saving else { return };
        let saved = match answer.try_recv() {
            Err(TryRecvError::Empty) => return,
            Ok(saved) => saved,
            Err(TryRecvError::Disconnected) => Err(account::STOPPED.into()),
        };
        form.saving = None;
        match saved {
            Ok(account) => self.account_added(account),
            Err(message) => form.error = Some(message),
        }
    }

    /// A new account joins the list and a replaced one keeps its place; the daemon starts it from config.toml and its
    /// events arrive by name, so its first sync shows on its line.
    fn account_added(&mut self, account: AccountConfig) {
        self.account_form = None;
        // Absorbs this window's own write, so the poll does not take it for an outside edit.
        self.config_watch.changed();
        self.retention
            .insert(account.name.clone(), account.trash_retention_days);
        let index = match self.accounts.iter().position(|a| a.name == account.name) {
            Some(index) => {
                // The restarted thread reports afresh; the replaced account's state no longer holds.
                let replaced = &mut self.accounts[index];
                replaced.error = None;
                replaced.activity = None;
                index
            }
            None => {
                let state = account_state(&self.paths, account.name.clone(), None);
                self.accounts.push(state);
                self.accounts.len() - 1
            }
        };
        self.push_history(format!(
            "{}: account saved; its first sync starts",
            account.name
        ));
        self.select_view(View::Folder {
            account: index,
            folder: list::INBOX.into(),
        });
    }

    /// Removes each account's backups past its retention, as `postvak trash purge` does; the view lists the rest.
    fn purge_trash(&mut self) {
        let now = postvak_core::time::now();
        let mut failures = Vec::new();
        let mut removed = 0;
        for (name, days) in self.retention.clone() {
            let trash = Trash::new(self.paths.trash_dir(&name));
            match trash.purge(days, now) {
                Ok(count) => removed += count,
                Err(e) => failures.push(format!("{name}: {e}")),
            }
        }
        let noun = if removed == 1 { "backup" } else { "backups" };
        let mut notice = format!("removed {removed} {noun} older than the retention period");
        if !failures.is_empty() {
            let count = failures.len();
            let accounts = if count == 1 { "account" } else { "accounts" };
            notice.push_str(&format!("; {count} {accounts} failed"));
            self.note_error(None, format!("could not purge: {}", failures.join("; ")));
        }
        self.notice(notice);
        self.view_dirty = true;
    }

    /// Asks each account the rule covers to run it over the mail it holds; `RuleApplied` reports back per account.
    fn apply_rule(&mut self, name: &str) {
        let Some(scope) = self
            .rules
            .rules
            .iter()
            .find(|rule| rule.name == name && rule.enabled)
            .map(|rule| rule.account.clone())
        else {
            return;
        };
        if let Some(account) = scope
            .as_deref()
            .filter(|account| !self.accounts.iter().any(|a| &a.name == account))
        {
            let message =
                format!("rule '{name}' names account '{account}', which this window does not list");
            self.note_error(None, message);
            return;
        }
        for index in 0..self.accounts.len() {
            let covered = scope
                .as_deref()
                .is_none_or(|account| account == self.accounts[index].name);
            let command = Command::ApplyRule { name: name.into() };
            if covered && !self.send(index, command) {
                self.note_error(Some(index), DAEMON_LOST.into());
            }
        }
    }

    /// A result for the status row and the history.
    fn notice(&mut self, text: String) {
        self.push_history(text.clone());
        self.notice = Some(text);
    }

    /// Reloads the rules view; the daemon notices the change itself and syncs every account.
    fn rules_changed(&mut self) {
        self.rules_watch.changed();
        self.rules = RulesState::load(
            &self.paths.rules_file(),
            std::mem::take(&mut self.rules.rules),
        );
    }

    fn open_draft(&mut self, draft: Option<Draft>) {
        self.rule_draft_opened = draft.clone();
        self.rule_draft = draft;
    }

    /// Whether the open rule has changes that Save has not written.
    fn draft_edited(&self) -> bool {
        match (&self.rule_draft, &self.rule_draft_opened) {
            (Some(draft), Some(opened)) => draft.edited(opened),
            _ => false,
        }
    }

    /// Writes the open rule; on failure it stays open with the reason.
    fn save_draft(&mut self) -> bool {
        let Some(draft) = &self.rule_draft else {
            return false;
        };
        let path = self.paths.rules_file();
        let saved = draft.to_rule().and_then(|rule| {
            rule_file::edit::save_rule(&path, draft.original.as_deref(), &rule)
                .map(|()| rule.name)
                .map_err(|e| e.to_string())
        });
        let Some(draft) = &mut self.rule_draft else {
            return false;
        };
        match saved {
            Ok(name) => {
                draft.original = Some(name);
                draft.error = None;
                self.rule_draft_opened = Some(draft.clone());
                self.rules_changed();
                true
            }
            Err(e) => {
                draft.error = Some(e);
                false
            }
        }
    }

    /// Closes an unsaved rule; a saved one asks first, then leaves rules.toml.
    fn remove_draft(&mut self, confirmed: bool) {
        let Some(draft) = &mut self.rule_draft else {
            return;
        };
        let Some(name) = draft.original.clone() else {
            self.open_draft(None);
            return;
        };
        if !confirmed {
            draft.confirm_remove = true;
            return;
        }
        match rule_file::edit::remove_rule(&self.paths.rules_file(), &name) {
            Ok(()) => {
                self.open_draft(None);
                self.rules_changed();
            }
            Err(e) => {
                draft.confirm_remove = false;
                draft.error = Some(e.to_string());
            }
        }
    }

    fn edit_rules(&mut self, edit: impl FnOnce(&Path) -> Result<(), RulesError>) {
        match edit(&self.paths.rules_file()) {
            Ok(()) => self.rules_changed(),
            Err(e) => self.note_error(None, e.to_string()),
        }
    }

    /// What `rule` would do to every account's cached mail if it were enabled, as `postvak rules test` shows it. Reads
    /// config.toml and the stores only. ponytail: scans the cache on the window thread; move it to a thread if large
    /// stores stall the window.
    fn preview(&self, mut rule: Rule) -> String {
        rule.enabled = true;
        let compiled = match rule_file::compile(&RuleFile {
            rules: vec![rule.clone()],
        }) {
            Ok(compiled) => compiled,
            Err(e) => return e.to_string(),
        };
        let config = match Config::load(&self.paths.config_file()) {
            Ok(config) => config,
            Err(e) => return e.to_string(),
        };
        let mut matched = HashSet::new();
        for account in &self.accounts {
            let Ok(store) = &account.store else { continue };
            let Some(settings) = config.accounts.iter().find(|a| a.name == account.name) else {
                continue;
            };
            let planned = settings
                .identity()
                .map_err(|e| e.to_string())
                .and_then(|identity| {
                    postvak_core::actions::planned(
                        &compiled,
                        store,
                        settings,
                        &identity,
                        postvak_core::time::now(),
                    )
                    .map_err(|e| e.to_string())
                });
            match planned {
                Ok(planned) => matched.extend(
                    planned
                        .into_iter()
                        .map(|p| (account.name.clone(), p.message.folder, p.message.uid)),
                ),
                Err(e) => return format!("{}: {e}", account.name),
            }
        }
        let needs_body = compiled.iter().any(|compiled| compiled.needs_body());
        draft::preview_text(&rule, matched.len(), needs_body)
    }

    fn open_rules_file(&mut self) {
        let path = self.paths.rules_file();
        let opened = (|| -> std::io::Result<()> {
            if !path.exists() {
                postvak_core::paths::write_atomic(&path, b"")?;
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

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::test_support::{Driver, Fixture, Wires, command, message, unread};
    use postvak_core::daemon::Client;
    use postvak_core::daemon::wire::AccountStatus;
    use postvak_core::rules::Action;
    use postvak_core::sync::{Command, Event};

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
    fn enter_in_search_asks_the_server_and_adds_its_hits_for_that_query_only() {
        let fx = Fixture::new(&["work"]);
        inbox(&fx, &[1, 2, 3]);
        let (mut driver, wires) = fx.driver();
        driver.press("/ zebra");
        assert!(driver.uids().is_empty());
        driver.press("enter");
        let search = Command::SearchServer {
            folder: None,
            query: "zebra".into(),
        };
        assert_eq!(wires.sent(), [("work".to_string(), search)]);
        assert!(list::results(&driver.app).contains("searching the server"));
        let found = |query: &str, uid| Event::ServerSearchDone {
            account: "work".into(),
            request: 9,
            query: query.into(),
            hits: vec![("INBOX".into(), uid)],
        };
        receive(
            &mut driver,
            &wires,
            [found("old query", 1), found("zebra", 2)],
        );
        assert_eq!(driver.uids(), [2]);
        assert!(!list::results(&driver.app).contains("searching the server"));
        driver.press("escape escape");
        assert_eq!(driver.uids(), [3, 2, 1]);
    }

    #[test]
    fn enter_again_while_the_server_search_runs_asks_nothing_more() {
        let fx = Fixture::new(&["work"]);
        inbox(&fx, &[1]);
        let (mut driver, wires) = fx.driver();
        driver.press("/ zebra");
        driver.press("enter");
        driver.press("enter");
        assert_eq!(wires.sent().len(), 1);
        assert!(list::results(&driver.app).contains("searching the server"));
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

    fn synced() -> Event {
        Event::Synced {
            account: "work".into(),
            new_messages: 0,
            actions: 0,
            requests: vec![],
            errors: vec![],
        }
    }

    /// A held list load's gate: it says when it has read the store, then waits to be let go.
    type Gate = (mpsc::Sender<()>, Receiver<()>);

    /// Gates for the list loads of the test that set them, one per load in order: each load reads the store, says so,
    /// then waits for its gate. One per test, as `load_list` is a plain `fn`.
    macro_rules! held_loads {
        ($gates:ident, $load:ident) => {
            static $gates: Mutex<VecDeque<Gate>> = Mutex::new(VecDeque::new());
            fn $load(paths: &Paths, accounts: &[(usize, String)], query: &ListQuery) -> Loaded {
                let gate = $gates.lock().unwrap().pop_front();
                let loaded = load::load_from_paths(paths, accounts, query);
                if let Some((read, release)) = gate {
                    let _ = read.send(());
                    let _ = release.recv();
                }
                loaded
            }
        };
    }

    /// The next held load, seen from the test.
    struct Held {
        read: Receiver<()>,
        release: mpsc::Sender<()>,
    }

    impl Held {
        /// Waits until the load has read the store, so what the test changes next is after its read.
        fn wait_read(&self) {
            self.read.recv_timeout(Duration::from_secs(5)).unwrap();
        }

        fn release(&self) {
            self.release.send(()).unwrap();
        }
    }

    fn hold(gates: &Mutex<VecDeque<Gate>>) -> Held {
        let (read, read_seen) = mpsc::channel();
        let (release, released) = mpsc::channel();
        gates.lock().unwrap().push_back((read, released));
        Held {
            read: read_seen,
            release,
        }
    }

    #[test]
    fn a_reload_that_outlasts_the_budget_shows_its_rows_when_they_come() {
        held_loads!(GATES, held);
        let fx = Fixture::new(&["work"]);
        inbox(&fx, &[1, 2, 3]);
        let (mut driver, wires) = fx.driver();
        driver.app.list_budget = Duration::ZERO;
        driver.app.load_list = held;
        let release = hold(&GATES);
        fx.add("work", message("INBOX", 4, "subject 4"));
        receive(&mut driver, &wires, [synced()]);
        assert!(driver.app.list_loading.is_some());
        assert_eq!(driver.uids(), [3, 2, 1], "the frame does not wait");
        release.release();
        tick_until(&mut driver, 0.0, |app| app.list_loading.is_none());
        assert_eq!(driver.uids(), [4, 3, 2, 1]);
    }

    #[test]
    fn a_change_during_a_reload_reloads_again_after_it() {
        held_loads!(GATES, held);
        let fx = Fixture::new(&["work"]);
        inbox(&fx, &[1, 2]);
        let (mut driver, wires) = fx.driver();
        driver.app.list_budget = Duration::ZERO;
        driver.app.load_list = held;
        let first = hold(&GATES);
        let second = hold(&GATES);
        receive(&mut driver, &wires, [synced()]);
        let started = driver.app.list_generation;
        first.wait_read();
        fx.add("work", message("INBOX", 3, "subject 3"));
        receive(&mut driver, &wires, [synced()]);
        assert!(driver.app.list_again, "the second waits for the first");
        first.release();
        tick_until(&mut driver, 0.0, |app| app.list_applied == started);
        assert_eq!(
            driver.uids(),
            [2, 1],
            "the first read before message 3 came"
        );
        second.release();
        tick_until(&mut driver, 0.0, |app| app.list_loading.is_none());
        assert_eq!(driver.uids(), [3, 2, 1]);
    }

    #[test]
    fn an_older_reload_does_not_bring_back_an_archived_row() {
        held_loads!(GATES, held);
        let fx = Fixture::new(&["work"]);
        inbox(&fx, &[1, 2, 3]);
        let (mut driver, wires) = fx.driver();
        driver.app.list_budget = Duration::ZERO;
        driver.app.load_list = held;
        let stale = hold(&GATES);
        let fresh = hold(&GATES);
        driver.press("e");
        assert_eq!(driver.uids(), [2, 1]);
        // This reload reads the store while message 3 is still in it.
        receive(&mut driver, &wires, [synced()]);
        let started = driver.app.list_generation;
        stale.wait_read();
        fx.store("work").remove_message("INBOX", 3).unwrap();
        receive(&mut driver, &wires, [done("work", vec![(3, Ok(1))], 0)]);
        assert_eq!(driver.uids(), [2, 1]);
        stale.release();
        tick_until(&mut driver, 0.0, |app| app.list_applied == started);
        assert_eq!(
            driver.uids(),
            [2, 1],
            "the archive settles until a newer load"
        );
        fresh.release();
        tick_until(&mut driver, 0.0, |app| app.accounts[0].settling.is_empty());
        assert_eq!(driver.uids(), [2, 1]);
        assert!(driver.app.list_loading.is_none());
    }

    #[test]
    fn a_failed_action_puts_the_row_back_while_a_reload_runs() {
        held_loads!(GATES, held);
        let fx = Fixture::new(&["work"]);
        inbox(&fx, &[1, 2, 3]);
        let (mut driver, wires) = fx.driver();
        driver.app.list_budget = Duration::ZERO;
        driver.app.load_list = held;
        let release = hold(&GATES);
        driver.press("e");
        receive(&mut driver, &wires, [synced()]);
        assert_eq!(driver.uids(), [2, 1]);
        let failed = done("work", vec![(3, Err("NO [SERVERBUG] try later".into()))], 0);
        receive(&mut driver, &wires, [failed]);
        assert_eq!(
            driver.uids(),
            [3, 2, 1],
            "the row is back before the reload ends"
        );
        assert!(driver.app.accounts[0].settling.is_empty());
        release.release();
        tick_until(&mut driver, 0.0, |app| app.list_loading.is_none());
        assert_eq!(driver.uids(), [3, 2, 1]);
    }

    #[test]
    fn a_sync_event_during_a_reload_rebuilds_the_rows_only_when_edits_changed() {
        held_loads!(GATES, held);
        let fx = Fixture::new(&["work"]);
        inbox(&fx, &[1, 2, 3]);
        let (mut driver, wires) = fx.driver();
        driver.app.list_budget = Duration::ZERO;
        driver.app.load_list = held;
        let release = hold(&GATES);
        receive(&mut driver, &wires, [synced()]);
        // Rows the store does not hold, so a rebuild from the loaded threads would show.
        driver.app.list.rows.truncate(1);
        receive(&mut driver, &wires, [synced()]);
        assert_eq!(driver.uids(), [3], "nothing changed the edits");
        receive(&mut driver, &wires, [refused("work", 0, "NO")]);
        assert_eq!(driver.uids(), [3, 2, 1], "a refusal drops every edit");
        release.release();
        tick_until(&mut driver, 0.0, |app| app.list_loading.is_none());
    }

    #[test]
    fn a_view_change_drops_a_reload_still_running() {
        held_loads!(GATES, held);
        let fx = Fixture::new(&["work"]);
        inbox(&fx, &[1, 2, 3]);
        let (mut driver, wires) = fx.driver();
        driver.app.list_budget = Duration::ZERO;
        driver.app.load_list = held;
        let release = hold(&GATES);
        receive(&mut driver, &wires, [synced()]);
        release.wait_read();
        fx.store("work").remove_message("INBOX", 3).unwrap();
        driver.app.reload_view();
        assert_eq!(driver.uids(), [2, 1]);
        assert!(driver.app.list_loading.is_none());
        release.release();
        driver.tick();
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
        let line = format!(
            "work: up to date · {}",
            postvak_core::time::clock(1_790_000_000)
        );
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
            Err(postvak_core::daemon::client::NewerDaemon {
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
            checked_at: postvak_core::time::now(),
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
