//! App state and the only code that changes it: `handle` for daemon events, `apply` for what the window asks, `tick`
//! for time. No toolkit here: `bind.rs` shows this state in the Slint window and turns its callbacks into `UiAction`s.
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::Duration;

use crate::{numbers, update};
use postvak_core::actions::Preview;
use postvak_core::bimi::Brand;
use postvak_core::config::{self, AccountConfig, Config, HtmlColours, Identity, Theme};
use postvak_core::credentials::Secret;
use postvak_core::daemon::Client;
use postvak_core::daemon::client::NewerDaemon;
use postvak_core::daemon::wire::AccountStatus;
use postvak_core::folder_name;
use postvak_core::help;
use postvak_core::message::{self, Attachment, HtmlBody, clean};
use postvak_core::paths::{FileWatch, Paths};
use postvak_core::rules::edit::FolderRename;
use postvak_core::rules::{self as rule_file, Action, Rule, RuleFile, RulesError};
use postvak_core::store::{Filter, LogEntry, Message, Store, StoreError};
use postvak_core::sync::{Activity, Command, DeleteMode, Event};
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
use super::preferences::{self, Section, Setting};
use super::reader::{self, Read, ReadFn, Reader};
use super::rules::{self, RulesState, SheetSort, TrashRow};
use super::save_as;
use super::undo::{self, Taken, Toast, UndoState};
use super::worker::{self, Stopped};

mod acting;
mod folder_dialog;
mod html_view;
mod opened;
mod rules_view;

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
/// How often an open search reloads while the indexer adds bodies, in seconds.
const INDEX_RELOAD: f64 = 1.0;

/// How long a message stays on screen before it is marked read.
pub(crate) const READ_DELAY: f64 = 1.0;

/// Seconds between attempts to reach the daemon again.
const RECONNECT: f64 = 5.0;

/// How many of this window's gestures `z` can still reach.
const MAX_TAKEN: usize = 50;

/// How long a frame waits for a list reload after a sync event: a quarter of a 120 Hz frame. A small mailbox's rows come
/// in time and show in the same frame; a large one's show when they come.
pub(crate) const LIST_BUDGET: Duration = Duration::from_millis(2);

/// How long a frame waits for the opened message to be read: half a 60 Hz frame. Most mail comes in time and opens in
/// the same frame; a large message shows "Loading…" until it comes, rather than freezing the window.
pub(crate) const BODY_BUDGET: Duration = Duration::from_millis(8);

/// Previews kept for messages still downloading: the shown one and its neighbours, with room to spare. Their HTML and
/// images can be megabytes, and one whose download failed is not stored, so the oldest go.
const MAX_PREVIEWS: usize = 4;

/// A daemon client, its event subscription, and each account's state and activity when it connected.
pub(crate) type Connection = (Client, Receiver<Event>, Vec<AccountStatus>);

/// Called after each daemon event and once the daemon hung up, so an idle window still shows them.
pub(crate) type Waker = Box<dyn Fn() + Send>;

/// Connects to the daemon, starting it when none answers.
pub(crate) fn connect(paths: &Paths, wake: Waker) -> anyhow::Result<Connection> {
    session(Client::connect_or_start(paths)?, wake)
}

/// Logs in as an account not saved yet through a connection of its own, starting the daemon if none runs, as
/// `postvak account add` does: the add-account form's test before it saves.
pub(crate) fn test_login(
    paths: &Paths,
    config: &Config,
    account: &AccountConfig,
    secret: &Secret,
) -> Result<(), String> {
    Client::connect_or_start(paths)
        .and_then(|client| {
            postvak_core::account::test_login(&client, paths, config, account, Some(secret))
        })
        .map_err(|e| clean(&format!("{e:#}"), false))
}

/// Subscribes before asking for the states, so no event between the two is lost.
pub(crate) fn session(client: Client, wake: Waker) -> anyhow::Result<Connection> {
    let events = client.subscribe_waking(wake)?;
    let states = client.status()?.accounts;
    Ok((client, events, states))
}

/// A rule run that would expunge mail, waiting for the user's yes.
pub(crate) struct ApplyConfirm {
    pub rule: String,
    pub warning: String,
}

/// The folder dialog: a new folder's name, a folder's new name, or a delete and its question.
pub(crate) struct FolderDialog {
    pub account: usize,
    pub kind: FolderKind,
    /// What the name field holds: the new folder's name, or the last level of the renamed folder's.
    pub name: String,
    /// Why the name will not do, or why the daemon refused.
    pub error: Option<String>,
    /// The folder the sent command is about, until the daemon answers: the window cannot tell its own request ids.
    pub waiting: Option<String>,
}

pub(crate) enum FolderKind {
    /// Under `parent`, joined with its delimiter; at the top level without one.
    New { parent: Option<String> },
    /// `rules` name `from` or a folder under it, as `(name, will_split)`.
    Rename {
        from: String,
        rules: Vec<(String, bool)>,
    },
    /// `not_empty` is the daemon's answer to the first try: the messages in it, and whether the account has Trash.
    Delete {
        folder: String,
        not_empty: Option<(u32, bool)>,
    },
    /// The messages the Trash folder held when asked; only these are deleted, not mail arriving meanwhile.
    Empty { folder: String, uids: Vec<u32> },
}

/// The one-click unsubscribe in flight, for the message it came from.
pub(crate) struct Unsubscribing {
    account: usize,
    key: RowKey,
    answer: Receiver<Result<(), String>>,
}

/// A Save as… waiting for the user to pick the file, for the message it came from.
pub(crate) struct SavingAs {
    account: usize,
    key: RowKey,
    /// What goes in the file.
    bytes: Vec<u8>,
    answer: Receiver<save_as::Chosen>,
}

/// An attachment asked of the daemon on its own, and what to do with it once it comes.
pub(crate) struct Fetching {
    pub account: usize,
    pub key: RowKey,
    pub index: usize,
    /// Save as… rather than Save.
    pub save_as: bool,
}

pub(crate) struct BodyState {
    pub account: usize,
    /// The sender's logo record, when sender images are on and the message passed DMARC.
    pub brand: Option<Brand>,
    /// Set when the user chose this message, not when a view change or reload put it on screen; only then does it
    /// become read.
    pub armed: bool,
    pub attachments: Vec<Attachment>,
    /// The attachments are listed from the message's structure and cannot be saved until the whole message is here.
    pub attachments_pending: bool,
    /// The message is too large to download whole unasked: each attachment is fetched when saved, and the whole
    /// message when Save .eml or View source needs it.
    pub on_demand: bool,
    /// Save .eml, Save .eml as… or View source, waiting for the whole message.
    pub after_download: Option<UiAction>,
    /// The message's HTML, when it has some.
    pub html: Option<HtmlBody>,
    /// The HTML view, once the panel asked for a layout; `None` while the text view shows.
    pub html_view: Option<HtmlState>,
    pub key: RowKey,
    pub message: Option<Message>,
    /// Set once mark-read was sent, or the user marked read or unread by hand, so the delay does not act again.
    pub read_sent: bool,
    pub saved: Option<String>,
    /// The sender accepted this message's one-click unsubscribe, so the note offers to delete it.
    pub unsubscribed: bool,
    pub shown_at: f64,
    /// Set by `v`: the text view for this message even though it has HTML.
    pub show_text: bool,
    /// Show original colours: this message's HTML in the sender's colours on a white page, also in the dark theme.
    pub original_colours: bool,
    /// The raw message, while the source window shows it.
    pub source: Option<body::Source>,
    /// Why the HTML could not be shown; the text view shows instead, under this note.
    pub text_note: Option<&'static str>,
    /// The body text cleaned once on load, since the panel draws it every frame.
    pub text: Option<String>,
    /// The read of this message running on the reader thread; the message shows as it was until it answers.
    pub reading: Option<u64>,
    /// The links in `text` that open, found once with it.
    pub links: Vec<String>,
    /// What rules did to this message, newest first; whether the strip lists them all; and, while Why? is open, each
    /// condition of the newest one's rule worded with whether the message meets it.
    pub provenance: Vec<LogEntry>,
    pub provenance_open: bool,
    pub why: Option<Vec<(String, bool)>>,
}

impl BodyState {
    /// The message chosen, empty until `read_body` reads it.
    fn chosen(account: usize, key: RowKey, now: f64, armed: bool) -> BodyState {
        BodyState {
            account,
            brand: None,
            armed,
            attachments: Vec::new(),
            attachments_pending: false,
            on_demand: false,
            after_download: None,
            text_note: None,
            html: None,
            html_view: None,
            key,
            message: None,
            read_sent: false,
            reading: None,
            saved: None,
            unsubscribed: false,
            show_text: false,
            original_colours: false,
            shown_at: now,
            source: None,
            links: Vec::new(),
            provenance: Vec::new(),
            provenance_open: false,
            why: None,
            text: None,
        }
    }

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
    /// One entry per `Apply` sent, oldest first, naming the gesture it belongs to; each `ActionDone` takes the oldest.
    pub apply_queue: VecDeque<Option<u64>>,
    pub store: Result<Store, String>,
    /// Whether the server offered UIDPLUS at the sync thread's last login; None before it logged in.
    pub uidplus: Option<bool>,
    /// Whether the sync thread's last login ran over TLS; None before it logged in.
    pub encrypted: Option<bool>,
    /// Who the account is, for `from:me` in a search; None when its config could not say.
    pub identity: Option<Identity>,
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
                Activity::Idle { .. }
                    | Activity::Indexing { .. }
                    | Activity::NotRunning { .. }
                    | Activity::Offline { .. }
                    | Activity::LoginRejected { .. }
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
    let names: Vec<String> = folders.iter().map(|f| f.name.clone()).collect();
    folders
        .into_iter()
        .map(|f| {
            let unread =
                i64::from(store.unread_count(&f.name)?) + delta.get(f.name.as_str()).unwrap_or(&0);
            let can_delete = folder_name::protected(store, &f.name)?.is_none();
            // Asks the store only for a folder with children, so a reload does not scan the folders once per folder.
            let has_children = f
                .delimiter
                .as_deref()
                .filter(|d| !d.is_empty())
                .is_some_and(|d| {
                    let prefix = format!("{}{d}", f.name);
                    names.iter().any(|name| name.starts_with(&prefix))
                });
            let can_rename = can_delete
                && !(has_children && folder_name::protected_below(store, &f.name)?.is_some());
            Ok(FolderRow {
                can_delete,
                can_rename,
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
    /// Neither INBOX nor a special-use folder (`folder_name::protected`).
    pub can_delete: bool,
    /// Deletable, and no folder under it is protected.
    pub can_rename: bool,
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

/// A rule asked to apply, with what it would expunge in each account, or why that could not be read.
pub(crate) type ExpungeCheck = (String, Result<Vec<String>, String>);

/// The proposals' dry runs, by rule name, and the pending list changes', by list name.
type DryRuns = (
    BTreeMap<String, rules::DryRun>,
    BTreeMap<String, rules::ListRun>,
);

/// What the window asks for; `App::apply` carries it out.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum UiAction {
    /// Open or close the About window.
    About,
    Act(Action),
    ApproveRule(String),
    /// Approve the pending change of this list.
    ApproveList(String),
    /// Add this authserv-id to this account's trusted_authserv_ids, as the Rules view offered.
    TrustAuthserv {
        account: String,
        id: String,
    },
    /// Take every row out of the selection.
    ClearSelection,
    /// Dismiss this dialog without answering it; its close button, Escape and the toggles that open it all come here.
    Close(Dialog),
    /// The rule editor's form changed.
    EditDraft(Box<Draft>),
    /// Open a rule in the editor.
    EditRule(String),
    /// The reader's strip: move the shown message back to the folder the rule that last acted took it from.
    MoveBack,
    /// The reader's strip: list every rule action on the message, or only the newest again.
    ToggleProvenance,
    /// The reader's strip: show each condition of the rule that last acted, or hide them again.
    ToggleWhy,
    Collapse,
    /// Ask before deleting this folder of this account.
    DeleteFolder(usize, String),
    /// Ask before deleting every message in this Trash folder of this account.
    EmptyTrash(usize, String),
    /// Move the targeted rows into a folder dropped on.
    DropOn(View),
    /// Close the topmost open dialog; with none open, leave the search, else clear the selection.
    Escape,
    Expand,
    /// Shift-click: mark the rows from the cursor to this one, and move the cursor there.
    ExtendSelection(usize),
    SetFilter(Filter),
    /// The folder dialog's button: create, rename or delete, a delete with what to do with its mail.
    FolderConfirm(Option<DeleteMode>),
    /// The folder dialog's name field changed.
    FolderName(String),
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
    /// Stop loading remote content from this sender domain without asking.
    ForgetRemote(String),
    ListViewport {
        offset: f32,
        height: f32,
    },
    MarkFolderRead(usize, String),
    MoveCursor(isize),
    MoveFilter(String),
    MoveTo(String),
    /// Ask for the name of a folder to create in this account, under this folder or at the top level.
    NewFolder(usize, Option<String>),
    /// Open an empty rule in the editor.
    NewRule,
    NextFocus,
    /// Open the add-account form.
    OpenAccountForm,
    /// Show a message a notification was about, or a later start asked for, in its folder.
    OpenMessage(postvak_core::notify::Open),
    OpenMovePicker,
    /// Open a web page in the browser.
    OpenUrl(String),
    OpenRulesFile,
    OpenSource,
    RejectRule(String),
    /// Remove the pending change of this list.
    RejectList(String),
    /// Ask for a new name for this folder of this account.
    RenameFolder(usize, String),
    /// Start a new rule for mail from the sender of the message shown, given its From.
    RuleFrom(String),
    /// Remove the open rule from rules.toml; unconfirmed, it asks first.
    RemoveRule {
        confirmed: bool,
    },
    Restore(usize, PathBuf),
    /// Save the form's account, replacing one of the same name.
    SaveAccount(account::Fields),
    SaveAttachment(usize),
    /// Save as… for attachment `index`: the save dialog picks where.
    SaveAttachmentAs(usize),
    /// Write the open rule to rules.toml.
    SaveDraft,
    SaveEml,
    SaveEmlAs,
    /// Leave the shown message's mailing list: by one click, or by opening its page or address.
    Unsubscribe,
    /// Move the message whose one-click unsubscribe the sender accepted to Trash.
    TrashUnsubscribed,
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
    /// Move the rule `name` right before the rule `anchor`, or right after it when `below`.
    MoveRule {
        name: String,
        anchor: String,
        below: bool,
    },
    SortSheet(SheetSort),
    /// The text typed in the Rules view's filter field.
    FilterRules(String),
    SetTheme(Theme),
    /// File ▸ Open at login, turned on or off.
    SetOpenAtLogin(bool),
    /// File ▸ Preferences…, opened or closed.
    TogglePreferences,
    PreferencesSection(Section),
    /// The account Preferences shows, by its index in config.toml.
    PreferencesAccount(usize),
    SetPreference(Setting),
    StartSearch,
    StepFolder(isize),
    /// Count what the open rule would do to the cached mail.
    TestDraft,
    /// Remove each account's backups past its retention
    PurgeTrash,
    /// Run an enabled rule once over the stored mail of the accounts it covers; asks first when it would expunge mail
    ApplyRule(String),
    /// Run the rule the expunge question is about
    ConfirmApplyRule,
    SyncNow,
    /// Put back the newest gesture of this window not undone yet.
    Undo,
    /// Put back logged action `id` of an account, as the Activity view offers it.
    UndoAction(usize, i64),
    /// The pointer is over the toast, which keeps it.
    ToastHover(bool),
    /// Aim the next action at the marked rows when `row` is one of them, else at `row` alone.
    TargetRow(usize),
    ToggleFlag,
    ToggleFolder(usize, String),
    ToggleHelp,
    ToggleHistory,
    ToggleHtml,
    /// The open message's HTML in its sender's colours, or adapted to the dark theme again.
    ToggleOriginalColours,
    /// Whether the window shows the dark theme, which decides how HTML mail is styled.
    HtmlDark(bool),
    ToggleMark,
    ToggleRead,
    /// Cmd-click (Ctrl-click off macOS): add this row to the selection, which starts from the cursor row, or take it out.
    ToggleRowMark(usize),
    /// The answers to "This rule has unsaved changes."
    UnsavedCancel,
    UnsavedDiscard,
    UnsavedSave,
}

impl UiAction {
    /// Where the list or the HTML page scrolled to, as the window reports it.
    pub(crate) fn is_scroll(&self) -> bool {
        matches!(
            self,
            UiAction::HtmlVisible { .. } | UiAction::ListViewport { .. }
        )
    }
}

/// The window's dialogs, topmost first: the order `ui/window.slint` draws them in reverse.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Dialog {
    Help,
    Unsaved,
    ApplyConfirm,
    Rule,
    Folder,
    Account,
    About,
    Preferences,
    Source,
    Move,
    History,
}

impl Dialog {
    const TOP_FIRST: [Dialog; 11] = [
        Dialog::Help,
        Dialog::Unsaved,
        Dialog::ApplyConfirm,
        Dialog::Rule,
        Dialog::Folder,
        Dialog::Account,
        Dialog::About,
        Dialog::Preferences,
        Dialog::Source,
        Dialog::Move,
        Dialog::History,
    ];
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Focus {
    Body,
    Folders,
    List,
}

/// The release check: waiting for the first frame, running on its thread, or done with the version to announce; a
/// check that config turns off starts done.
pub(crate) enum UpdateCheck {
    Waiting,
    Running(Receiver<Option<String>>),
    Done(Option<String>),
}

/// Whether the window opens at login: read at the first tick, after the test fixture has swapped the hooks, and
/// changed on a thread of its own, since in a Flatpak the desktop may ask the user first.
pub(crate) enum OpenAtLogin {
    Unknown,
    Known(bool),
    /// Turning it on (true) or off; the answer is what changed, or why not.
    Changing(
        bool,
        Receiver<anyhow::Result<postvak_core::autostart::Changed>>,
    ),
}

impl OpenAtLogin {
    /// What File ▸ Open at login shows: the setting, or the one it is changing to.
    pub(crate) fn checked(&self) -> bool {
        match self {
            OpenAtLogin::Known(on) | OpenAtLogin::Changing(on, _) => *on,
            OpenAtLogin::Unknown => false,
        }
    }
}

/// The Preferences dialog while it is open, over config.toml as it read at the last open or save.
pub(crate) struct PreferencesState {
    pub(crate) section: Section,
    /// Index into `config.accounts`.
    pub(crate) account: usize,
    pub(crate) config: Config,
    /// Why the last change was not saved.
    pub(crate) error: Option<String>,
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
    /// The gestures taken in this window, oldest first, which `z` undoes newest first.
    pub(crate) taken: Vec<Taken>,
    pub(crate) next_taken: u64,
    /// At the foot of the list pane: what the last action or undo did.
    pub(crate) toast: Option<Toast>,
    /// Each account's trash retention in days, from config.toml as the window opened, added the account or last purged.
    pub(crate) retention: BTreeMap<String, u64>,
    pub(crate) events: Receiver<Event>,
    pub(crate) fetch_release: fn() -> anyhow::Result<String>,
    pub(crate) open_at_login: OpenAtLogin,
    /// Read and change opening at login (`autostart`); tests swap them so nothing outside the test changes.
    pub(crate) read_open_at_login: fn(&Paths) -> anyhow::Result<bool>,
    pub(crate) set_open_at_login:
        fn(&Paths, bool) -> anyhow::Result<postvak_core::autostart::Changed>,
    /// Tests an account's login before the form saves it; tests swap it so no daemon starts.
    pub(crate) check_login: account::Check,
    /// Sends a one-click unsubscribe; tests swap it so nothing leaves the machine.
    pub(crate) post_unsubscribe: fn(&str) -> Result<(), String>,
    pub(crate) unsubscribing: Option<Unsubscribing>,
    /// Each rule's hits as last counted, and the count under way.
    pub(crate) rule_hits: HashMap<String, rule_file::hits::Hits>,
    counting_hits: Option<Receiver<HashMap<String, rule_file::hits::Hits>>>,
    /// The proposals or the cached mail changed since their dry runs were counted; and the run under way.
    dry_runs_stale: bool,
    dry_running: Option<Receiver<DryRuns>>,
    pub(crate) saving_as: Option<SavingAs>,
    /// Attachments of messages too large to download whole, asked of the daemon one by one, until they arrive.
    pub(crate) fetching: Vec<Fetching>,
    /// Opens the save dialog; tests swap it so no dialog opens.
    pub(crate) ask_save_path: fn(&str, &Path, Wake) -> Receiver<save_as::Chosen>,
    /// Downloads remote content for HTML mail, started the first time the user asks.
    pub(crate) fetcher: Option<Fetcher>,
    pub(crate) focus: Focus,
    pub(crate) about_open: bool,
    pub(crate) preferences: Option<PreferencesState>,
    /// The rule waiting for the user to agree to the mail it would expunge, with the warning per account.
    pub(crate) apply_confirm: Option<ApplyConfirm>,
    /// The rule asked to apply, with what it would expunge, read off the window thread as it reads every folder.
    pub(crate) expunge_check: Option<Receiver<ExpungeCheck>>,
    pub(crate) help_open: bool,
    pub(crate) folder_dialog: Option<FolderDialog>,
    /// Numbers HTML layouts, so a reply for an older one is dropped.
    pub(crate) html_generation: u64,
    /// The scale and width the window last gave the HTML view, so the next HTML message lays out at once.
    pub(crate) html_size: Option<(f32, f32)>,
    /// The part of the HTML view on screen as the window last reported it, in points, for the next message's page.
    pub(crate) html_shown: (f32, f32),
    /// `[ui] html_colours`: whether HTML mail is adapted to the dark theme.
    pub(crate) html_colours: HtmlColours,
    /// Whether the window shows the dark theme; System follows the desktop, which only the window knows.
    pub(crate) html_dark: bool,
    pub(crate) history: VecDeque<HistoryLine>,
    pub(crate) history_open: bool,
    /// Opens a file or URL with the desktop's app; tests record instead.
    pub(crate) launch: fn(&std::ffi::OsStr, bool) -> std::io::Result<()>,
    /// When an open search last reloaded for `Event::Indexed`.
    pub(crate) last_index_reload: f64,
    pub(crate) last_poll: f64,
    pub(crate) last_reconnect: f64,
    pub(crate) list: ListState,
    /// Sender logos, with `[ui] sender_images = true`.
    pub(crate) logos: Logos,
    /// `[ui] remote_content`: the sender domains whose remote content loads without asking.
    pub(crate) remote_content: BTreeSet<String>,
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
    /// The first load that reads the query typed last; its hits show from the top, not at the row the cursor was on.
    pub(crate) search_reset: Option<u64>,
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
    /// View source or Save .eml asked for while the body still shows another message than the cursor's, as after a
    /// row menu's `TargetRow`; it runs once `sync_body` has caught up.
    pub(crate) after_body: Option<UiAction>,
    /// The HTML render thread, started for the first HTML message.
    pub(crate) renderer: Option<Renderer>,
    /// The thread that reads the opened message from its store, started for the first one.
    pub(crate) reader: Option<Reader>,
    /// How the reader reads a message; tests swap it to hold a read back.
    pub(crate) read_body: ReadFn,
    /// How long `tick` waits for the opened message to be read; tests wait as long as it takes.
    pub(crate) body_budget: Duration,
    /// Numbers the reads of the opened message, so an answer that a later read overtook is dropped.
    pub(crate) body_generation: u64,
    pub(crate) reconnect: fn(&Paths, Waker) -> anyhow::Result<Connection>,
    /// The result of the reconnect attempt in flight; at most one runs.
    pub(crate) reconnecting: Option<Receiver<anyhow::Result<Connection>>>,
    pub(crate) requested: HashSet<(usize, RowKey)>,
    /// The one preview fetch each account runs at a time, so stepping fast does not queue a download per row passed.
    pub(crate) fetching_body: HashMap<usize, RowKey>,
    /// Bodies wanted while their account's fetch runs: the shown message and its neighbours, emptied when the cursor
    /// moves on.
    pub(crate) waiting_bodies: Vec<(usize, RowKey)>,
    /// What large messages show while the rest of them downloads, newest last, until each is stored.
    pub(crate) previews: Vec<((usize, RowKey), Preview)>,
    pub(crate) rules: RulesState,
    pub(crate) rules_watch: FileWatch,
    /// The folder holding rules.toml, whose entries change when a file sync leaves a conflicting copy there.
    pub(crate) config_dir_watch: FileWatch,
    /// The rule open in the Rules view's editor; nothing in it is written until Save.
    pub(crate) rule_draft: Option<Draft>,
    /// What `rule_draft` was opened or last saved as, which tells an edit from a look.
    pub(crate) rule_draft_opened: Option<Draft>,
    /// The open rule changed in rules.toml while the editor had unsaved changes; the next Save says so instead.
    pub(crate) rule_draft_stale: bool,
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
    /// What the Rules view's filter field holds; leaving the view clears it, as it does `sheet_sort`.
    pub(crate) rules_filter: String,
    pub(crate) update: UpdateCheck,
    pub(crate) view: View,
    pub(crate) view_dirty: bool,
}

/// An account's window state over its store; `open_account` creates the store of an account just added.
fn account_state(
    paths: &Paths,
    name: String,
    activity: Option<Activity>,
    identity: Option<Identity>,
) -> Account {
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
        apply_queue: VecDeque::new(),
        store,
        uidplus: None,
        encrypted: None,
        identity,
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
            .map(
                |AccountStatus {
                     activity,
                     name,
                     uidplus,
                     encrypted,
                 }| {
                    let identity = config.account(&name).and_then(|a| a.identity().ok());
                    Account {
                        uidplus,
                        encrypted,
                        ..account_state(&paths, name, activity, identity)
                    }
                },
            )
            .collect::<Vec<Account>>();
        super::phases::mark("stores_opened");
        let rules_path = paths.rules_file();
        let rules_dir = paths.config_dir.clone();
        let rules = RulesState::load(&rules_path, RuleFile::default(), &accounts);
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
            open_at_login: OpenAtLogin::Unknown,
            read_open_at_login: postvak_core::autostart::is_on,
            set_open_at_login: postvak_core::autostart::set,
            check_login: test_login,
            post_unsubscribe: |url| unsubscribe::one_click(url).map_err(|e| e.to_string()),
            unsubscribing: None,
            rule_hits: HashMap::new(),
            counting_hits: None,
            dry_runs_stale: true,
            dry_running: None,
            saving_as: None,
            fetching: Vec::new(),
            ask_save_path: save_as::ask,
            fetcher: None,
            focus: Focus::List,
            about_open: false,
            preferences: None,
            apply_confirm: None,
            expunge_check: None,
            help_open: false,
            html_generation: 0,
            html_size: None,
            html_shown: (0.0, 0.0),
            history: VecDeque::new(),
            history_open: false,
            launch: crate::opener::open,
            last_index_reload: 0.0,
            last_poll: 0.0,
            last_reconnect: 0.0,
            list: ListState::default(),
            logos: Logos::new(config.ui.sender_images, paths.cache_dir.clone()),
            remote_content: config.ui.remote_content.clone(),
            list_budget: LIST_BUDGET,
            list_generation: 0,
            list_applied: 0,
            list_loading: None,
            list_again: false,
            search_reset: None,
            edits_changed: false,
            load_list: load::load_from_paths,
            move_picker: None,
            folder_dialog: None,
            account_form: None,
            paths,
            pending_arm: false,
            after_body: None,
            reconnect: connect,
            renderer: None,
            reader: None,
            read_body: reader::read,
            body_budget: BODY_BUDGET,
            body_generation: 0,
            reconnecting: None,
            requested: HashSet::new(),
            fetching_body: HashMap::new(),
            waiting_bodies: Vec::new(),
            previews: Vec::new(),
            rules,
            rules_watch: FileWatch::new(rules_path),
            config_dir_watch: FileWatch::new(rules_dir),
            rule_draft: None,
            rule_draft_opened: None,
            rule_draft_stale: false,
            leave_draft: None,
            search: None,
            filter: Filter::default(),
            search_focused: false,
            next_tick: None,
            wake: Arc::new(|| {}),
            theme: config.ui.theme,
            html_colours: config.ui.html_colours,
            // The window says whether the System theme is dark once it shows (`UiAction::HtmlDark`).
            html_dark: config.ui.theme == Theme::Dark,
            trash: Vec::new(),
            sheet_sort: None,
            rules_filter: String::new(),
            update: if config.ui.check_updates {
                UpdateCheck::Waiting
            } else {
                UpdateCheck::Done(None)
            },
            view: View::Folder {
                account: 0,
                folder: "INBOX".into(),
            },
            view_dirty: false,
            retention,
            notice: None,
            taken: Vec::new(),
            next_taken: 0,
            toast: None,
        };
        app.reload_view();
        super::phases::mark("first_list");
        app
    }

    /// Everything that happens with time: daemon events and render replies in, reconnects, the release check, reloads,
    /// file polling, the shown body and its read delay. The window calls it when woken, on its timer and after input.
    pub fn tick(&mut self, now: f64) {
        // Before the events, so a new account is listed when its first sync reports.
        self.receive_saved_account();
        self.receive(now);
        self.receive_html();
        self.receive_read(now);
        self.receive_remote();
        self.logos.receive();
        self.keep_daemon(now);
        self.check_release();
        self.receive_open_at_login();
        self.receive_unsubscribe();
        self.receive_hits();
        self.receive_dry_runs();
        self.run_dry_runs();
        self.receive_expunge_check();
        self.receive_save_as();
        self.receive_list();
        if std::mem::take(&mut self.view_dirty) {
            self.refresh_view();
        }
        self.show_row_logos();
        self.list.logos_changed = false;
        self.poll_files(now);
        let typing_search = self.search_focused;
        self.sync_body(now, typing_search);
        if let Some(action) = self.after_body.take()
            && self.body_follows_cursor()
        {
            self.apply_action(action);
        }
        // The window reports the HTML view's width only when it changes; laying out at the last one starts a newly
        // shown message, and lays out again once a new width has held still.
        if let Some((scale, width)) = self.html_size {
            self.html_width(scale, width, now);
        }
        self.paint_visible();
        self.html_progress(now);
        self.mark_read_after_delay(now);
        self.expire_toast(now);
        self.request_tick(self.last_poll + POLL);
        if self.daemon_lost {
            self.request_tick(self.last_reconnect + RECONNECT);
        }
    }

    /// Asks for `tick` at `at` (seconds on the window's clock) at the latest.
    /// Puts the logos that are in on the rows in view, reading the brand of a row new to the view and asking for its
    /// logo; whether a row changed. Sent and Drafts show the recipient's initials, so their rows get none.
    fn show_row_logos(&mut self) -> bool {
        let mut changed = false;
        if !self.logos.enabled() || self.shows_recipient() {
            for row in &mut self.list.rows {
                changed |= row.logo.take().is_some();
            }
            return changed;
        }
        let now = chrono::Local::now();
        let shown = list::visible(
            &self.list.rows,
            self.search.is_some(),
            self.list.viewport,
            &now,
        );
        for index in shown {
            let row = &self.list.rows[index];
            let brand = match self.list.brands.get(&row.id()) {
                Some(brand) => brand.clone(),
                None => {
                    let brand = self.accounts[row.account]
                        .store
                        .as_ref()
                        .ok()
                        .and_then(|store| store.headers(&row.folder, row.uid).ok().flatten())
                        .and_then(|headers| self.logos.brand(&headers));
                    self.list.brands.insert(row.id(), brand.clone());
                    brand
                }
            };
            // Asked each time, as a logo setting turned off and on again starts with none known.
            let logo = brand.and_then(|brand| {
                self.logos.want(&brand, &self.wake);
                Some((brand.record_name(), self.logos.svg(&brand)?.clone()))
            });
            if self.list.rows[index].logo != logo {
                self.list.rows[index].logo = logo;
                changed = true;
            }
        }
        changed
    }

    pub(crate) fn request_tick(&mut self, at: f64) {
        self.next_tick = Some(self.next_tick.map_or(at, |next| next.min(at)));
    }

    fn receive(&mut self, now: f64) {
        while !self.daemon_lost {
            match worker::poll(&self.events) {
                Some(Ok(event)) => self.handle(event, now),
                None => break,
                Some(Err(Stopped)) => {
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
        match worker::poll(attempt) {
            None => {}
            Some(Err(Stopped)) => self.reconnecting = None,
            Some(Ok(Err(e))) => {
                log::warn!("could not reach the daemon: {e:#}");
                self.reconnecting = None;
                if let Some(newer) = e.downcast_ref::<NewerDaemon>() {
                    let text = newer.to_string();
                    if self.error.as_deref() != Some(text.as_str()) {
                        self.note_error(None, text);
                    }
                }
            }
            Some(Ok(Ok(connection))) => {
                self.reconnecting = None;
                self.reconnected(connection);
            }
        }
    }

    /// Connects on a background thread so the window never waits; the window is woken with the result.
    fn start_reconnect(&self) -> Receiver<anyhow::Result<Connection>> {
        let (reconnect, paths, wake) = (self.reconnect, self.paths.clone(), self.wake.clone());
        worker::oneshot("reconnect", wake.clone(), move || {
            reconnect(&paths, Box::new(move || wake()))
        })
    }

    /// Starts the release check at the first tick, after the test fixture has swapped `fetch_release`, then takes its
    /// answer.
    fn check_release(&mut self) {
        let next = match &self.update {
            UpdateCheck::Waiting => {
                let (cache, fetch) = (self.paths.update_check(), self.fetch_release);
                let answer = worker::oneshot("update-check", self.wake.clone(), move || {
                    update::check(
                        &cache,
                        postvak_core::time::now(),
                        env!("CARGO_PKG_VERSION"),
                        fetch,
                    )
                });
                Some(UpdateCheck::Running(answer))
            }
            UpdateCheck::Running(answer) => {
                worker::poll(answer).map(|newer| UpdateCheck::Done(newer.ok().flatten()))
            }
            UpdateCheck::Done(_) => None,
        };
        if let Some(next) = next {
            self.update = next;
        }
    }

    /// Reads whether the window opens at login, once, then takes the answer of a change.
    fn receive_open_at_login(&mut self) {
        let next = match &self.open_at_login {
            OpenAtLogin::Unknown => match (self.read_open_at_login)(&self.paths) {
                Ok(on) => OpenAtLogin::Known(on),
                Err(e) => {
                    log::debug!("cannot tell whether Postvak opens at login: {e:#}");
                    OpenAtLogin::Known(false)
                }
            },
            OpenAtLogin::Changing(on, answer) => match worker::poll(answer) {
                Some(Ok(Ok(changed))) => {
                    let on = *on;
                    // On, but the session may never start it: the user should know, as the setting looks done.
                    if let Some(warning) = changed.warning {
                        self.note_error(None, warning);
                    }
                    OpenAtLogin::Known(on)
                }
                Some(Ok(Err(e))) => {
                    let (on, text) = (*on, format!("{e:#}"));
                    self.note_error(None, format!("could not change opening at login: {text}"));
                    OpenAtLogin::Known(!on)
                }
                Some(Err(Stopped)) => OpenAtLogin::Known(!*on),
                None => return,
            },
            OpenAtLogin::Known(_) => return,
        };
        self.open_at_login = next;
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
        self.fetching_body.clear();
        self.waiting_bodies.clear();
        for account in &mut self.accounts {
            if let Some(status) = states.iter().find(|status| status.name == account.name) {
                account.activity = status.activity.clone();
                account.uidplus = status.uidplus;
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
            | Event::AttachmentFetched { account, .. }
            | Event::BodyPreview { account, .. }
            | Event::BodyReady { account, .. }
            | Event::CommandFailed { account, .. }
            | Event::Connected { account, .. }
            | Event::Error { account, .. }
            | Event::FolderCreated { account, .. }
            | Event::FolderDeleted { account, .. }
            | Event::FolderNotEmpty { account, .. }
            | Event::FolderRenamed { account, .. }
            | Event::Indexed { account, .. }
            | Event::NewMail { account, .. }
            | Event::Restored { account, .. }
            | Event::Undone { account, .. }
            | Event::RuleApplied { account, .. }
            | Event::ServerSearchDone { account, .. }
            | Event::ServerSearchFailed { account, .. }
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
                    Activity::FetchingBodies { .. }
                        | Activity::FetchingHeaders { .. }
                        | Activity::Indexing { .. }
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
            Event::Connected {
                uidplus, encrypted, ..
            } => {
                if !encrypted && self.accounts[index].encrypted != Some(false) {
                    let line = format!("{}: {}", self.accounts[index].name, help::UNENCRYPTED);
                    self.push_history(line);
                }
                if !uidplus && self.accounts[index].uidplus != Some(false) {
                    let line = format!("{}: {}", self.accounts[index].name, help::NO_UIDPLUS);
                    self.push_history(line);
                }
                self.accounts[index].uidplus = Some(uidplus);
                self.accounts[index].encrypted = Some(encrypted);
            }
            Event::NewMail { .. } => {}
            // More bodies are searchable: an open search reloads, at most once a second, keeping its cursor as a
            // sync event's reload does.
            Event::Indexed { .. } => {
                if self.search.is_some() && now - self.last_index_reload >= INDEX_RELOAD {
                    self.last_index_reload = now;
                    self.view_dirty = true;
                }
            }
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
            Event::ServerSearchFailed { query, message, .. } => {
                let account = self.accounts[index].name.clone();
                let message = clean(&message, false);
                self.push_history(format!("{account}: server search failed: {message}"));
                // Another client's search for the same text is broadcast too; only a round this window runs counts.
                if self.list.server_searching > 0
                    && self.search.as_deref() == Some(query.as_str())
                    && !self.list.server_failed.contains(&account)
                {
                    self.list.server_searching -= 1;
                    self.list.server_failed.push(account);
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
                account.apply_queue.clear();
                self.edits_changed = true;
                self.undo_answered(index, None);
                // The refusal may be the running body fetch's; the next one asked for may go.
                self.fetching_body.remove(&index);
                let waiting = self
                    .folder_dialog
                    .as_mut()
                    .filter(|dialog| dialog.account == index && dialog.waiting.is_some());
                match waiting {
                    Some(dialog) => {
                        dialog.error = Some(clean(&message, false));
                        dialog.waiting = None;
                    }
                    None => self.note_error(Some(index), message),
                }
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
                let line = format!(
                    "{}: {} on {}",
                    self.accounts[index].name,
                    numbers::counted(actions as u64, "action", "actions"),
                    numbers::counted(evaluated as u64, "message", "messages")
                );
                self.notice(line);
                for message in errors {
                    self.note_error(Some(index), message);
                }
                self.refresh(index);
            }
            Event::Synced { .. } => {
                // A fresh sync result replaces the last notice, which would otherwise outrank every account line.
                self.notice = None;
                self.refresh(index);
                // The folder warnings read what the stores hold, which a sync changes.
                self.rules.check(&self.paths.rules_file(), &self.accounts);
                if self.view == View::Rules {
                    self.count_hits();
                }
                self.dry_runs_stale = true;
            }
            Event::ActionDone {
                folder,
                results,
                action,
                ..
            } => {
                self.action_done(index, action);
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
                        "{} of {} failed: {first}",
                        numbers::count(failed.len() as u64),
                        numbers::counted(results.len() as u64, "message", "messages")
                    );
                    self.note_error(Some(index), line);
                }
                self.refresh(index);
            }
            Event::BodyPreview {
                folder,
                uid,
                preview,
                ..
            } => {
                let key = (index, (folder, uid));
                self.previews.retain(|(held, _)| *held != key);
                if self.previews.len() >= MAX_PREVIEWS {
                    let (gone, _) = self.previews.remove(0);
                    // Shown again without its preview, it is asked for again.
                    self.requested.remove(&gone);
                }
                self.previews.push((key.clone(), *preview));
                self.reload_body(key.0, key.1, now);
            }
            Event::BodyReady { folder, uid, .. } => {
                let key = (index, (folder, uid));
                // A message too large to download unasked keeps its preview until it is stored.
                let stored = match &self.accounts[index].store {
                    Ok(store) => matches!(store.has_raw(&key.1.0, key.1.1), Ok(true)),
                    Err(_) => false,
                };
                if stored {
                    self.previews.retain(|(held, _)| *held != key);
                }
                if self.fetching_body.get(&index) == Some(&key.1) {
                    self.body_fetch_ended(index);
                }
                self.reload_body(key.0, key.1, now);
            }
            Event::AttachmentFetched {
                folder,
                uid,
                index: attachment,
                name,
                contents,
                ..
            } => self.attachment_fetched(index, (folder, uid), attachment, name, contents),
            Event::FolderCreated { name, .. } => {
                self.folder_answered(index, &name);
                self.refresh(index);
            }
            Event::FolderDeleted { name, .. } => {
                self.folder_answered(index, &name);
                self.refresh(index);
                if let View::Folder { account, folder } = &self.view
                    && *account == index
                    && folder_name::same(folder, &name)
                {
                    self.select_view(View::Folder {
                        account: index,
                        folder: list::INBOX.into(),
                    });
                }
            }
            Event::FolderRenamed { from, to, .. } => self.folder_renamed(index, &from, &to),
            Event::FolderNotEmpty {
                name,
                count,
                has_trash,
                ..
            } => {
                if let Some(dialog) = self.awaited(index, &name)
                    && let FolderKind::Delete { not_empty, .. } = &mut dialog.kind
                {
                    *not_empty = Some((count, has_trash));
                    dialog.waiting = None;
                }
            }
            // Another client's undo, or the broadcast copy of our own: the stores changed.
            Event::Undone { request, .. } if request != 0 => self.refresh(index),
            Event::Undone { report, .. } => {
                self.undo_answered(index, Some(report));
                self.refresh(index);
            }
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
                self.fetching_body.remove(&index);
                self.waiting_bodies.retain(|(account, _)| *account != index);
                self.fetching.retain(|fetch| fetch.account != index);
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
        if self.list.cursor != cursor && !self.list.cursor_shown(self.search.is_some()) {
            self.list.follow_cursor = true;
        }
    }

    fn apply_action(&mut self, action: UiAction) {
        match action {
            leave @ (UiAction::Close(Dialog::Rule)
            | UiAction::EditRule(_)
            | UiAction::NewRule
            | UiAction::RuleFrom(_))
                if self.draft_edited() =>
            {
                self.leave_draft = Some(Box::new(leave));
            }
            UiAction::Act(action) => self.act(action),
            UiAction::ApproveRule(name) => {
                let paths = self.paths.clone();
                self.edit_rules(|_| {
                    rule_file::edit::approve_from_now(&paths, &name, postvak_core::time::now())
                        .map(drop)
                });
            }
            UiAction::ApproveList(name) => {
                let paths = self.paths.clone();
                self.edit_rules(|_| {
                    rule_file::edit::approve_list_from_now(&paths, &name, postvak_core::time::now())
                        .map(drop)
                });
            }
            UiAction::TrustAuthserv { account, id } => self.trust_authserv(&account, &id),
            UiAction::Collapse => self.collapse(),
            UiAction::DropOn(view) => self.drop_on(view),
            UiAction::HtmlHover(at) => self.html_hover(at),
            UiAction::HtmlSelect { from, to } => self.html_select(from, to),
            UiAction::HtmlVisible { top, bottom } => self.html_visible(top, bottom),
            UiAction::HtmlWidth { scale, width } => self.html_size = Some((scale, width)),
            UiAction::LoadRemote => {
                self.load_remote();
                if let Some(domain) = self.vouched_sender()
                    && self.remote_content.insert(domain)
                {
                    self.save_remote_content();
                }
            }
            UiAction::ForgetRemote(domain) => {
                if self.remote_content.remove(&domain) {
                    self.save_remote_content();
                }
            }
            UiAction::ClearSelection => self.list.marked.clear(),
            UiAction::Close(dialog) => self.close(dialog),
            UiAction::Escape => {
                if let Some(dialog) = self.top_dialog() {
                    self.apply_action(UiAction::Close(dialog));
                } else if self.search.is_some() {
                    self.reset_list(None, false);
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
                let rule = self.rules.file.rules.iter().find(|rule| rule.name == name);
                if let Some(draft) = rule.map(Draft::from_rule) {
                    self.open_draft(Some(draft));
                }
            }
            UiAction::Expand => self.expand(),
            UiAction::ToggleProvenance => {
                if let Some(body) = &mut self.body {
                    body.provenance_open = !body.provenance_open;
                }
            }
            UiAction::ToggleWhy => self.toggle_why(),
            UiAction::MoveBack => self.move_back(),
            UiAction::ListViewport { offset, height } => {
                self.list.viewport = (offset, height);
                self.list.logos_changed = self.show_row_logos();
                // The list can stop short of the cursor while it still estimates its rows; then it is asked again.
                if self.list.cursor_shown(self.search.is_some()) {
                    self.list.follow_cursor = false;
                }
            }
            UiAction::MarkFolderRead(account, folder) => self.mark_folder_read(account, folder),
            UiAction::MoveCursor(delta) => {
                self.list.cursor = step(self.list.cursor, delta, self.list.rows.len());
                self.pending_arm = true;
            }
            UiAction::MoveFilter(filter) => self.move_picker = Some(filter),
            UiAction::NewFolder(account, parent) => self.new_folder(account, parent),
            UiAction::RenameFolder(account, from) => self.rename_folder(account, from),
            UiAction::DeleteFolder(account, folder) => {
                let kind = FolderKind::Delete {
                    folder,
                    not_empty: None,
                };
                self.open_folder_dialog(account, kind, String::new());
            }
            UiAction::EmptyTrash(account, folder) => self.empty_trash(account, folder),
            UiAction::FolderName(name) => {
                if let Some(dialog) = self.folder_dialog.as_mut() {
                    dialog.name = name;
                }
                self.check_folder_name();
            }
            UiAction::FolderConfirm(mode) => self.confirm_folder(mode),
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
            action @ (UiAction::OpenSource | UiAction::SaveEml | UiAction::SaveEmlAs)
                if self.body_follows_cursor() && self.shown_on_demand() =>
            {
                self.download_whole(action);
            }
            action @ (UiAction::OpenSource | UiAction::SaveEml | UiAction::SaveEmlAs)
                if !self.body_follows_cursor() =>
            {
                self.after_body = Some(action);
            }
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
            UiAction::RejectList(name) => {
                self.edit_rules(|path| rule_file::edit::reject_list(path, &name));
            }
            UiAction::RemoveRule { confirmed } => self.remove_draft(confirmed),
            UiAction::RuleFrom(from) => {
                if let Some(body) = &self.body {
                    let draft =
                        Draft::from_sender(&from, &self.accounts[body.account].name, &body.key.0);
                    self.select_view(View::Rules);
                    self.open_draft(Some(draft));
                }
            }
            UiAction::SaveDraft => {
                if self.save_draft() {
                    self.open_draft(None);
                }
            }
            UiAction::TestDraft => {
                let tested = self.rule_draft.as_ref().map(|draft| match draft.to_rule() {
                    Ok(rule) => self.preview(rule),
                    Err(e) => e,
                });
                if let Some(draft) = &mut self.rule_draft {
                    draft.tested = tested;
                }
            }
            UiAction::UnsavedCancel => self.close(Dialog::Unsaved),
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
            UiAction::SaveAttachmentAs(index) if self.shown_on_demand() => {
                self.fetch_attachment(index, true);
            }
            UiAction::SaveAttachmentAs(index) => {
                let raw = self.shown_raw();
                let file = raw.map(|raw| message::attachment(&raw, index));
                self.save_as(file);
            }
            UiAction::SaveEml => self.save_eml(),
            UiAction::SaveEmlAs => {
                let raw = self.shown_raw();
                let subject = self
                    .body
                    .as_ref()
                    .and_then(|b| b.message.as_ref()?.subject.clone());
                let file = raw.map(|raw| Ok((message::eml_name(subject.as_deref()), raw)));
                self.save_as(file);
            }
            UiAction::Unsubscribe => self.unsubscribe(),
            UiAction::TrashUnsubscribed => self.trash_unsubscribed(),
            UiAction::SearchFocused(focused) => self.search_focused = focused,
            UiAction::SearchAccepted => {
                self.search_server();
                self.apply_action(UiAction::SelectRow(0));
            }
            UiAction::SetFilter(filter) => {
                self.filter = filter;
                self.reload_view();
            }
            // The hits load on a thread of their own, as after a sync event, so a keystroke never waits for the store
            // and fast typing runs one search at a time, for the latest text. The last query's rows stay until then.
            UiAction::SearchFor(query) => {
                let rows = std::mem::take(&mut self.list.rows);
                self.reset_list(Some(query), self.search_focused);
                self.list.rows = rows;
                self.search_reset = Some(self.list_generation + 1);
                self.view_dirty = true;
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
            UiAction::FilterRules(filter) => self.rules_filter = filter,
            UiAction::SetRuleEnabled(name, enabled) => {
                self.edit_rules(|path| rule_file::edit::set_enabled(path, &name, enabled));
            }
            UiAction::MoveRule {
                name,
                anchor,
                below,
            } => {
                let place = if below {
                    rule_file::edit::Place::After(anchor)
                } else {
                    rule_file::edit::Place::Before(anchor)
                };
                self.edit_rules(|path| rule_file::edit::move_rule(path, &name, &place));
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
            UiAction::SetOpenAtLogin(on) => {
                let (paths, set) = (self.paths.clone(), self.set_open_at_login);
                let answer =
                    worker::oneshot("open-at-login", self.wake.clone(), move || set(&paths, on));
                self.open_at_login = OpenAtLogin::Changing(on, answer);
            }
            UiAction::TogglePreferences if self.preferences.is_some() => {
                self.close(Dialog::Preferences)
            }
            UiAction::TogglePreferences => match Config::load(&self.paths.config_file()) {
                Ok(config) => {
                    self.preferences = Some(PreferencesState {
                        section: Section::default(),
                        account: 0,
                        config,
                        error: None,
                    })
                }
                Err(e) => self.note_error(None, format!("could not read config.toml: {e}")),
            },
            UiAction::PreferencesSection(section) => {
                if let Some(prefs) = self.preferences.as_mut() {
                    prefs.section = section;
                }
            }
            UiAction::PreferencesAccount(index) => {
                if let Some(prefs) = self.preferences.as_mut()
                    && index < prefs.config.accounts.len()
                {
                    prefs.account = index;
                }
            }
            UiAction::SetPreference(setting) => self.set_preference(&setting),
            UiAction::StartSearch => {
                if self.view.lists_mail() {
                    // The field takes the keyboard as it appears.
                    self.reset_list(Some(String::new()), true);
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
            UiAction::Undo => self.undo_newest(),
            UiAction::UndoAction(account, action) => {
                let id = self.take(String::new(), 0);
                if let Some(taken) = self.taken.iter_mut().find(|taken| taken.id == id) {
                    taken.actions.push((account, action));
                }
                self.send_undo(id);
            }
            UiAction::ToastHover(hovered) => {
                if let Some(toast) = &mut self.toast {
                    toast.hovered = hovered;
                    toast.until = None;
                }
            }
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
            UiAction::ToggleHelp if self.help_open => self.close(Dialog::Help),
            UiAction::ToggleHelp => self.help_open = true,
            UiAction::About if self.about_open => self.close(Dialog::About),
            UiAction::About => self.about_open = true,
            UiAction::OpenAccountForm => {
                self.account_form.get_or_insert_with(AccountForm::default);
            }
            UiAction::OpenMessage(open) => self.open_message(open),
            UiAction::SaveAccount(fields) => self.save_account(&fields),
            UiAction::ToggleHtml => {
                if let Some(body) = self.body.as_mut().filter(|b| b.html.is_some()) {
                    body.show_text = !body.show_text;
                }
            }
            UiAction::ToggleOriginalColours => {
                if self.offers_original_colours()
                    && let Some(body) = self.body.as_mut()
                {
                    body.original_colours = !body.original_colours;
                    self.restyle_html();
                }
            }
            UiAction::HtmlDark(dark) if dark != self.html_dark => {
                self.html_dark = dark;
                self.restyle_html();
            }
            UiAction::HtmlDark(_) => {}
            UiAction::PurgeTrash => self.purge_trash(),
            UiAction::ApplyRule(name) => self.apply_rule(&name),
            UiAction::ConfirmApplyRule => {
                if let Some(confirm) = self.apply_confirm.take() {
                    self.send_apply_rule(&confirm.rule);
                }
            }
            UiAction::ToggleHistory if self.history_open => self.close(Dialog::History),
            UiAction::ToggleHistory => {
                self.history_open = true;
                // The history shows them; the status line need not any more.
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
        self.search_focused
            || self.move_picker.is_some()
            || self.account_form.is_some()
            || self.folder_dialog.is_some()
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

    /// Shows the message in its folder with the cursor on it. A filter that hides it gives way; a thread shows it when
    /// it is the newest message there.
    fn open_message(&mut self, open: postvak_core::notify::Open) {
        let account = match &open.account {
            Some(name) => self.accounts.iter().position(|a| &a.name == name),
            None => (!self.accounts.is_empty()).then_some(0),
        };
        let Some(account) = account else {
            let name = open.account.unwrap_or_default();
            self.note_error(
                None,
                format!("no account named {name} to open a message in"),
            );
            return;
        };
        let view = View::Folder {
            account,
            folder: open.folder.clone(),
        };
        if self.view == view && self.search.is_none() {
            // The rows may not have the new message yet.
            self.reload_view();
        } else {
            self.select_view(view);
        }
        let find = |rows: &[Row]| {
            rows.iter().position(|row| {
                row.account == account && row.folder == open.folder && row.uid == open.uid
            })
        };
        if find(&self.list.rows).is_none() && self.filter != Filter::default() {
            self.filter = Filter::default();
            self.reload_view();
        }
        if let Some(index) = find(&self.list.rows) {
            self.apply_action(UiAction::SelectRow(index));
        }
    }

    pub(crate) fn select_view(&mut self, view: View) {
        if view == View::Rules {
            self.count_hits();
        }
        self.view = view;
        self.sheet_sort = None;
        self.rules_filter.clear();
        self.reset_list(None, false);
        self.reload_view();
    }

    /// Starts the list over for `search` (None leaves search), with the search field focused or not: the one place
    /// the list, the search and what depends on them are reset. A search field that goes away is not unfocused by
    /// Slint, so leaving search must clear `search_focused` here.
    fn reset_list(&mut self, search: Option<String>, focused: bool) {
        self.move_picker = None;
        self.search = search;
        self.search_focused = focused;
        // The window keeps its scroll offset across lists, so the new list scrolls to its cursor from there.
        self.list = ListState {
            viewport: self.list.viewport,
            ..ListState::default()
        };
        self.list.follow_cursor = !self.list.cursor_shown(self.search.is_some());
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
                View::Trash => {
                    self.trash = rules::trash_rows(&self.accounts, &self.paths, &self.trash)
                }
                _ => {}
            }
            return;
        };
        // A store that failed to open shows its error in the folder pane and adds no rows.
        let stores: Vec<load::LoadStore<'_>> = self
            .covered_accounts()
            .into_iter()
            .filter_map(|index| {
                let account = &self.accounts[index];
                account.store.as_ref().ok().map(|store| {
                    (
                        index,
                        account.name.as_str(),
                        store,
                        account.identity.as_ref(),
                    )
                })
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
            .map(|index| {
                let account = &self.accounts[index];
                (index, account.name.clone(), account.identity.clone())
            })
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
        let loaded = match worker::poll(&loading.answer) {
            None => return,
            Some(Ok(loaded)) => Some(loaded),
            Some(Err(Stopped)) => {
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
        if self.search_reset.is_some_and(|first| generation >= first) {
            self.search_reset = None;
            self.list.rows.clear();
            self.list.cursor = 0;
        }
        for account in &mut self.accounts {
            account
                .settling
                .retain(|(_, _, settles)| *settles > generation);
        }
        if let Some(hits) = loaded.hits {
            self.list.hits = hits;
            self.list.unindexed = loaded.unindexed;
            self.list.covered = loaded.covered;
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
        // Rebuilt rows come without logos; drawn before the next tick, they would blink to the initials.
        self.show_row_logos();
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
        self.list.server_failed.clear();
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
        } else if self.config_dir_watch.changed() {
            // A sync tool's conflicting copy appears beside rules.toml without changing it.
            self.rules.check(&self.paths.rules_file(), &self.accounts);
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
        let (wake, check) = (self.wake.clone(), self.check_login);
        let paths = self.paths.clone();
        let Some(form) = self
            .account_form
            .as_mut()
            .filter(|form| form.saving.is_none())
        else {
            return;
        };
        match account::save(paths, fields, wake, check) {
            Ok(answer) => {
                form.error = None;
                form.saving = Some(answer);
                form.testing = true;
            }
            Err(message) => form.error = Some(message),
        }
    }

    /// Takes the save's steps: a saved account joins the window and closes the form, and a refusal, of the login or of
    /// the save, stays in the form with why.
    fn receive_saved_account(&mut self) {
        loop {
            let Some(form) = &mut self.account_form else {
                return;
            };
            let Some(answer) = &form.saving else { return };
            let Some(step) = worker::poll(answer) else {
                return;
            };
            let step =
                step.unwrap_or_else(|Stopped| account::Step::Refused(account::STOPPED.into()));
            match step {
                account::Step::Tested => form.testing = false,
                account::Step::Saved(account) => return self.account_added(account),
                account::Step::Refused(why) => {
                    form.saving = None;
                    form.testing = false;
                    form.error = Some(why);
                }
            }
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
                replaced.uidplus = None;
                index
            }
            None => {
                let state = account_state(
                    &self.paths,
                    account.name.clone(),
                    None,
                    account.identity().ok(),
                );
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

    /// Removes each account's backups past its retention in config.toml now, as `postvak trash purge` does; the view
    /// lists the rest.
    fn purge_trash(&mut self) {
        match Config::load(&self.paths.config_file()) {
            Ok(config) => {
                for account in config.accounts {
                    self.retention
                        .insert(account.name, account.trash_retention_days);
                }
            }
            Err(e) => return self.note_error(None, format!("could not purge: {e}")),
        }
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
        let mut notice = format!(
            "removed {} older than the retention period",
            numbers::counted(removed as u64, "backup", "backups")
        );
        if !failures.is_empty() {
            let accounts = numbers::counted(failures.len() as u64, "account", "accounts");
            notice.push_str(&format!("; {accounts} failed"));
            self.note_error(None, format!("could not purge: {}", failures.join("; ")));
        }
        self.notice(notice);
        self.view_dirty = true;
    }

    fn dialog_open(&self, dialog: Dialog) -> bool {
        match dialog {
            Dialog::Help => self.help_open,
            Dialog::Unsaved => self.leave_draft.is_some(),
            Dialog::ApplyConfirm => self.apply_confirm.is_some(),
            Dialog::Rule => self.rule_draft.is_some() && self.view == View::Rules,
            Dialog::Folder => self.folder_dialog.is_some(),
            Dialog::Account => self.account_form.is_some(),
            Dialog::About => self.about_open,
            Dialog::Preferences => self.preferences.is_some(),
            Dialog::Source => self.body.as_ref().is_some_and(|b| b.source.is_some()),
            Dialog::Move => self.move_picker.is_some(),
            Dialog::History => self.history_open,
        }
    }

    fn top_dialog(&self) -> Option<Dialog> {
        Dialog::TOP_FIRST.into_iter().find(|&d| self.dialog_open(d))
    }

    /// Dismisses a dialog, however the user asked; an answer (Move, Save, Apply) closes its own. A closed one stays closed.
    fn close(&mut self, dialog: Dialog) {
        match dialog {
            Dialog::Help => self.help_open = false,
            Dialog::Unsaved => self.leave_draft = None,
            Dialog::ApplyConfirm => self.apply_confirm = None,
            Dialog::Rule => self.open_draft(None),
            Dialog::Folder => self.folder_dialog = None,
            Dialog::Account => {
                // During the login test nothing is saved yet and the save gives up; once it writes, it finishes
                // first, so its account joins the window.
                if self
                    .account_form
                    .as_ref()
                    .is_some_and(|form| form.saving.is_none() || form.testing)
                {
                    self.account_form = None;
                }
            }
            Dialog::About => self.about_open = false,
            Dialog::Preferences => self.preferences = None,
            Dialog::Source => {
                if let Some(body) = self.body.as_mut() {
                    body.source = None;
                }
            }
            Dialog::Move => self.move_picker = None,
            Dialog::History => {
                if self.history_open {
                    self.history_open = false;
                    // Whatever arrived while it was open was shown there.
                    self.error = None;
                    self.notice = None;
                }
            }
        }
    }

    /// A result for the status row and the history.
    fn notice(&mut self, text: String) {
        self.push_history(text.clone());
        self.notice = Some(text);
    }

    /// Saves a change made in Preferences, which the daemon notices itself, and shows it in the window at once.
    fn set_preference(&mut self, setting: &Setting) {
        if self.preferences.is_none() {
            return;
        }
        let saved = preferences::save(&self.paths.config_file(), setting);
        if let Ok(config) = &saved {
            // Absorbs this write, so the poll does not take it for an outside edit.
            self.config_watch.changed();
            self.follow_config(config);
        }
        if let Some(prefs) = self.preferences.as_mut() {
            match saved {
                Ok(config) => {
                    prefs.config = config;
                    prefs.error = None;
                }
                Err(e) => prefs.error = Some(format!("Not saved: {e}")),
            }
        }
    }

    /// What the window shows of config.toml, made to match `config` as this window saved it.
    fn follow_config(&mut self, config: &Config) {
        if self.html_colours != config.ui.html_colours {
            self.html_colours = config.ui.html_colours;
            self.restyle_html();
        }
        if self.logos.enabled() != config.ui.sender_images {
            self.logos = Logos::new(config.ui.sender_images, self.paths.cache_dir.clone());
        }
        match (&self.update, config.ui.check_updates) {
            (UpdateCheck::Done(None), true) => self.update = UpdateCheck::Waiting,
            (_, false) => self.update = UpdateCheck::Done(None),
            _ => {}
        }
        for account_config in &config.accounts {
            self.retention.insert(
                account_config.name.clone(),
                account_config.trash_retention_days,
            );
            if let Some(account) = self
                .accounts
                .iter_mut()
                .find(|a| a.name == account_config.name)
            {
                account.identity = account_config.identity().ok();
            }
        }
        self.rules.check(&self.paths.rules_file(), &self.accounts);
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

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;
    use crate::test_support::{Driver, Fixture, Wires, command, message, synced, unread, work};
    use postvak_core::daemon::Client;
    use postvak_core::daemon::wire::AccountStatus;
    use postvak_core::rules::Action;
    use postvak_core::sync::{Command, Event};

    fn inbox(fx: &Fixture, uids: &[u32]) {
        for &uid in uids {
            fx.add("work", message("INBOX", uid, &format!("subject {uid}")));
        }
    }

    fn done(account: &str, results: Vec<(u32, Result<usize, String>)>, request: u64) -> Event {
        Event::ActionDone {
            account: account.into(),
            folder: "INBOX".into(),
            results,
            action: None,
            request,
        }
    }

    fn toast(driver: &Driver) -> Option<(&str, bool)> {
        driver
            .app
            .toast
            .as_ref()
            .map(|toast| (toast.text.as_str(), toast.undo))
    }

    fn undos(wires: &Wires) -> Vec<(String, Command)> {
        wires
            .sent()
            .into_iter()
            .filter(|(_, command)| matches!(command, Command::Undo { .. }))
            .collect()
    }

    #[test]
    fn z_undoes_the_last_gesture_once_the_daemon_numbered_it() {
        use postvak_core::actions::{UndoOutcome, UndoReport, UndoRow};
        let fx = Fixture::new(&["work"]);
        inbox(&fx, &[1, 2]);
        let (mut driver, wires) = fx.driver();
        driver.press("e");
        assert_eq!(toast(&driver), Some(("Archived 1 message", true)));
        // Pressed before the daemon answered: the undo waits for the action's number.
        driver.press("z");
        assert_eq!(toast(&driver), Some(("Undoing…", false)));
        assert_eq!(undos(&wires), []);
        let archived = Event::ActionDone {
            account: "work".into(),
            folder: "INBOX".into(),
            results: vec![(2, Ok(1))],
            action: Some(7),
            request: 0,
        };
        driver.receive(&wires, [archived]);
        let undo = Command::Undo {
            action: 7,
            by: "gui".into(),
        };
        assert_eq!(undos(&wires), [("work".to_string(), undo)]);
        let report = UndoReport {
            action: 7,
            rows: vec![UndoRow {
                folder: "INBOX".into(),
                uid: 2,
                subject: None,
                action: "archive".into(),
                outcome: UndoOutcome::PutBack,
                error: None,
            }],
        };
        let undone = Event::Undone {
            account: "work".into(),
            report,
            request: 0,
        };
        driver.receive(&wires, [undone]);
        assert_eq!(
            toast(&driver),
            Some(("Moved 1 message back to Inbox", false))
        );
        driver.press("z");
        assert_eq!(toast(&driver), Some(("Nothing to undo", false)));
        assert_eq!(undos(&wires), []);
    }

    #[test]
    fn the_toast_goes_after_a_while_unless_the_pointer_is_on_it() {
        let fx = Fixture::new(&["work"]);
        inbox(&fx, &[1]);
        let (mut driver, _wires) = fx.driver();
        driver.press("u");
        assert!(driver.app.toast.is_some());
        driver.apply(UiAction::ToastHover(true));
        driver.now = 20.0;
        driver.tick();
        assert!(driver.app.toast.is_some(), "the pointer keeps it");
        driver.apply(UiAction::ToastHover(false));
        driver.now = 27.0;
        driver.tick();
        assert!(driver.app.toast.is_some(), "it gets its full time again");
        driver.now = 28.5;
        driver.tick();
        assert_eq!(driver.app.toast, None);
    }

    fn refused(account: &str, request: u64, message: &str) -> Event {
        Event::CommandFailed {
            account: account.into(),
            request,
            message: message.into(),
        }
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
    fn search_operators_narrow_folders_accounts_and_me_from_all_inboxes() {
        let fx = Fixture::new(&["home", "work"]);
        fx.folder("home", "Archive", Some("Archive"));
        fx.add("home", message("INBOX", 1, "invoice march"));
        fx.add("home", message("Archive", 3, "invoice april"));
        fx.add("work", message("INBOX", 4, "invoice may"));
        let mut mine = message("INBOX", 5, "notes to self");
        mine.from_addr = Some("Me <home@example.com>".into());
        fx.add("home", mine);
        let (mut driver, _wires) = fx.driver();
        driver.apply(UiAction::SelectView(View::AllInboxes));
        let mut found = |query: &str| -> Vec<(usize, String, u32)> {
            driver.apply(UiAction::SearchFor(query.into()));
            driver
                .app
                .list
                .rows
                .iter()
                .map(|r| (r.account, r.folder.clone(), r.uid))
                .collect()
        };
        assert_eq!(found("invoice in:Archive"), [(0, "Archive".to_string(), 3)]);
        assert_eq!(found("invoice account:work"), [(1, "INBOX".to_string(), 4)]);
        assert_eq!(
            found("from:me"),
            [(0, "INBOX".to_string(), 5)],
            "each account's own identity"
        );
        assert!(found("invoice from:me").is_empty());
    }

    #[test]
    fn archive_sends_the_cursor_row_hides_it_and_selects_the_next() {
        let fx = Fixture::new(&["work"]);
        inbox(&fx, &[1, 2, 3]);
        let (mut driver, wires) = fx.driver();
        driver.press("e");
        assert_eq!(wires.sent(), [work("INBOX", &[3], Action::Archive)]);
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
                work("INBOX", &[1], Action::Junk),
                command("work", "Junk", &[2], Action::NotJunk)
            ]
        );
    }

    #[test]
    fn opening_a_message_shows_its_folder_with_the_cursor_on_it() {
        let fx = Fixture::new(&["home", "work"]);
        fx.folder("work", "Receipts", None);
        inbox(&fx, &[1, 2]);
        for uid in [3, 4, 5] {
            fx.add("work", message("Receipts", uid, "receipt"));
        }
        let (mut driver, _) = fx.driver();
        // The fixture's mail is read, so this filter hides it all.
        driver.apply(UiAction::SetFilter(Filter {
            unread: true,
            ..Filter::default()
        }));
        driver.apply(UiAction::OpenMessage(postvak_core::notify::Open {
            account: Some("work".into()),
            folder: "Receipts".into(),
            uid: 4,
        }));
        let work = driver.app.accounts.iter().position(|a| a.name == "work");
        assert_eq!(
            driver.app.view,
            View::Folder {
                account: work.unwrap(),
                folder: "Receipts".into()
            }
        );
        assert_eq!(driver.app.filter, Filter::default());
        assert_eq!(driver.app.list.rows[driver.app.list.cursor].uid, 4);
        assert_eq!(driver.app.focus, Focus::List);
    }

    #[test]
    fn opening_a_message_of_an_unknown_account_says_so_and_stays() {
        let fx = Fixture::new(&["work"]);
        inbox(&fx, &[1]);
        let (mut driver, _) = fx.driver();
        let view = driver.app.view.clone();
        driver.apply(UiAction::OpenMessage(postvak_core::notify::Open {
            account: Some("gone".into()),
            folder: "INBOX".into(),
            uid: 1,
        }));
        assert_eq!(driver.app.view, view);
        assert!(
            driver
                .app
                .history
                .iter()
                .any(|line| line.text.contains("no account named gone"))
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
        assert_eq!(wires.sent(), [work("INBOX", &[4, 5], Action::Archive)]);
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
        assert_eq!(wires.sent(), [work("INBOX", &[2, 3], Action::Trash)]);
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
        assert_eq!(wires.sent(), [work("INBOX", &[2, 3, 4], Action::Trash)]);
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
        assert_eq!(wires.sent(), [work("INBOX", &[1, 3], Action::Archive)]);
    }

    #[test]
    fn command_a_marks_every_row() {
        let fx = Fixture::new(&["work"]);
        inbox(&fx, &[1, 2, 3]);
        let (mut driver, wires) = fx.driver();
        driver.press("ctrl+a e");
        assert_eq!(wires.sent(), [work("INBOX", &[1, 2, 3], Action::Archive)]);
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
        driver.receive(&wires, [found("old query", 1), found("zebra", 2)]);
        assert_eq!(driver.uids(), [2]);
        assert!(!list::results(&driver.app).contains("searching the server"));
        driver.press("escape escape");
        assert_eq!(driver.uids(), [3, 2, 1]);
    }

    /// #275: a failed server search arrived as a bare failure, so the line said "with what the server found" and the
    /// account's pending edits were dropped.
    #[test]
    fn a_failed_server_search_keeps_the_local_results_and_says_so() {
        let fx = Fixture::new(&["work"]);
        inbox(&fx, &[1, 2, 3]);
        let (mut driver, wires) = fx.driver();
        driver.press("e");
        driver.press("/ subject");
        let local = driver.uids();
        assert!(!local.is_empty());
        driver.press("enter");
        let failed = |query: &str| Event::ServerSearchFailed {
            account: "work".into(),
            request: 9,
            query: query.into(),
            message: "NO [UNAVAILABLE] try later".into(),
        };
        driver.receive(&wires, [failed("old query"), failed("subject")]);
        assert_eq!(driver.uids(), local);
        assert!(
            list::results(&driver.app).ends_with("· the server search failed for work"),
            "{}",
            list::results(&driver.app)
        );
        assert_eq!(driver.app.accounts[0].queued, 1);
    }

    /// #268: the line under the search field said "1 results · in 1 messages".
    #[test]
    fn one_result_in_one_message_reads_in_the_singular() {
        let fx = Fixture::new(&["work"]);
        inbox(&fx, &[1]);
        let (mut driver, _wires) = fx.driver();
        driver.app.list.covered = 1;
        driver.app.list.unindexed = 0;
        assert_eq!(list::results(&driver.app), "1 result · in 1 message");
        driver.app.list.covered = 2;
        assert_eq!(list::results(&driver.app), "1 result · in 2 messages");
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
        driver.receive(&wires, [failed]);
        assert_eq!(driver.uids(), [3, 2, 1]);
        assert_eq!(
            driver.status()[0],
            "work: 1 of 1 message failed: NO [SERVERBUG] try later"
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
        driver.receive(&wires, [synced]);
        assert_eq!(driver.uids(), [2, 1]);
        fx.store("work").remove_message("INBOX", 3).unwrap();
        driver.receive(&wires, [done("work", vec![(3, Ok(1))], 0)]);
        assert_eq!(driver.uids(), [2, 1]);
    }

    /// A held list load's gate: it says when it has read the store, then waits to be let go.
    type Gate = (mpsc::Sender<()>, Receiver<()>);

    /// Gates for the list loads of the test that set them, one per load in order: each load reads the store, says so,
    /// then waits for its gate. One per test, as `load_list` is a plain `fn`.
    macro_rules! held_loads {
        ($gates:ident, $load:ident) => {
            static $gates: Mutex<VecDeque<Gate>> = Mutex::new(VecDeque::new());
            fn $load(paths: &Paths, accounts: &[load::LoadAccount], query: &ListQuery) -> Loaded {
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
        driver.receive(&wires, [synced("work", 0)]);
        assert!(driver.app.list_loading.is_some());
        assert_eq!(driver.uids(), [3, 2, 1], "the frame does not wait");
        release.release();
        driver.at(0.0);
        driver.tick_until(|app| app.list_loading.is_none());
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
        driver.receive(&wires, [synced("work", 0)]);
        let started = driver.app.list_generation;
        first.wait_read();
        fx.add("work", message("INBOX", 3, "subject 3"));
        driver.receive(&wires, [synced("work", 0)]);
        assert!(driver.app.list_again, "the second waits for the first");
        first.release();
        driver.at(0.0);
        driver.tick_until(|app| app.list_applied == started);
        assert_eq!(
            driver.uids(),
            [2, 1],
            "the first read before message 3 came"
        );
        second.release();
        driver.at(0.0);
        driver.tick_until(|app| app.list_loading.is_none());
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
        driver.receive(&wires, [synced("work", 0)]);
        let started = driver.app.list_generation;
        stale.wait_read();
        fx.store("work").remove_message("INBOX", 3).unwrap();
        driver.receive(&wires, [done("work", vec![(3, Ok(1))], 0)]);
        assert_eq!(driver.uids(), [2, 1]);
        stale.release();
        driver.at(0.0);
        driver.tick_until(|app| app.list_applied == started);
        assert_eq!(
            driver.uids(),
            [2, 1],
            "the archive settles until a newer load"
        );
        fresh.release();
        driver.at(0.0);
        driver.tick_until(|app| app.accounts[0].settling.is_empty());
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
        driver.receive(&wires, [synced("work", 0)]);
        assert_eq!(driver.uids(), [2, 1]);
        let failed = done("work", vec![(3, Err("NO [SERVERBUG] try later".into()))], 0);
        driver.receive(&wires, [failed]);
        assert_eq!(
            driver.uids(),
            [3, 2, 1],
            "the row is back before the reload ends"
        );
        assert!(driver.app.accounts[0].settling.is_empty());
        release.release();
        driver.at(0.0);
        driver.tick_until(|app| app.list_loading.is_none());
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
        driver.receive(&wires, [synced("work", 0)]);
        // Rows the store does not hold, so a rebuild from the loaded threads would show.
        driver.app.list.rows.truncate(1);
        driver.receive(&wires, [synced("work", 0)]);
        assert_eq!(driver.uids(), [3], "nothing changed the edits");
        driver.receive(&wires, [refused("work", 0, "NO")]);
        assert_eq!(driver.uids(), [3, 2, 1], "a refusal drops every edit");
        release.release();
        driver.at(0.0);
        driver.tick_until(|app| app.list_loading.is_none());
    }

    /// #263: clicking a folder while typing a search removed the field without unfocusing it, so letter keys stayed
    /// typing and the body stayed hidden.
    #[test]
    fn choosing_a_folder_while_typing_a_search_ends_the_typing() {
        let fx = Fixture::new(&["work"]);
        fx.folder("work", "Archive", Some("Archive"));
        inbox(&fx, &[1, 2, 3]);
        let (mut driver, wires) = fx.driver();
        driver.press("/ he");
        assert!(driver.app.search_focused);
        driver.apply(UiAction::SelectView(View::Folder {
            account: 0,
            folder: "Archive".into(),
        }));
        assert_eq!(driver.app.search, None);
        assert!(!driver.app.search_focused);
        assert!(!driver.app.typing());
        driver.apply(UiAction::SelectView(View::Folder {
            account: 0,
            folder: "INBOX".into(),
        }));
        driver.press("e");
        assert_eq!(wires.sent(), [work("INBOX", &[3], Action::Archive)]);
    }

    /// #263: the menu's Search closes the move picker, as choosing a folder does.
    #[test]
    fn starting_a_search_closes_the_move_picker() {
        let fx = Fixture::new(&["work"]);
        inbox(&fx, &[1]);
        let (mut driver, _wires) = fx.driver();
        driver.press("m");
        assert!(driver.app.move_picker.is_some());
        driver.apply(UiAction::StartSearch);
        assert!(driver.app.move_picker.is_none());
        assert!(driver.app.search_focused);
    }

    #[test]
    fn typing_a_search_does_not_wait_for_the_store() {
        held_loads!(GATES, held);
        let fx = Fixture::new(&["work"]);
        inbox(&fx, &[1, 2, 3]);
        let (mut driver, _wires) = fx.driver();
        driver.press("/");
        driver.app.list_budget = Duration::ZERO;
        driver.app.load_list = held;
        let first = hold(&GATES);
        let second = hold(&GATES);
        let shown = driver.uids();
        driver.apply(UiAction::SearchFor("subject 1".into()));
        assert!(driver.app.list_loading.is_some());
        assert_eq!(
            driver.uids(),
            shown,
            "the keystroke does not wait for the hits"
        );
        first.wait_read();
        driver.apply(UiAction::SearchFor("subject 2".into()));
        assert!(
            driver.app.list_again,
            "the next keystroke waits for the search running"
        );
        first.release();
        second.release();
        driver.at(0.0);
        driver.tick_until(|app| app.list_loading.is_none() && !app.view_dirty && !app.list_again);
        assert_eq!(driver.uids(), [2]);
    }

    #[test]
    fn the_hits_of_a_new_query_start_at_the_top() {
        let fx = Fixture::new(&["work"]);
        inbox(&fx, &[1, 2, 3]);
        let (mut driver, _wires) = fx.driver();
        driver.press("/");
        driver.apply(UiAction::SearchFor("subject".into()));
        assert_eq!(driver.uids(), [3, 2, 1]);
        driver.app.list.cursor = 2;
        driver.apply(UiAction::SearchFor("subjec".into()));
        assert_eq!(driver.uids(), [3, 2, 1]);
        assert_eq!(
            driver.app.list.cursor, 0,
            "the cursor does not follow message 1"
        );
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
        driver.receive(&wires, [synced("work", 0)]);
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
        driver.receive(&wires, [done("work", vec![(1, Ok(1))], 0)]);
        assert!(driver.app.list.rows[0].flagged);
        assert_eq!(driver.app.accounts[0].queued, 1);
    }

    #[test]
    fn escape_closes_the_topmost_dialog_first() {
        let fx = Fixture::new(&["work"]);
        fx.folder("work", "Archive", Some("Archive"));
        inbox(&fx, &[1]);
        let (mut driver, _wires) = fx.driver();
        driver.apply(UiAction::ToggleHistory);
        driver.press("m");
        driver.apply(UiAction::About);
        driver.app.apply_confirm = Some(ApplyConfirm {
            rule: "r".into(),
            warning: "w".into(),
        });
        driver.apply(UiAction::ToggleHelp);
        let open = |app: &App| {
            Dialog::TOP_FIRST
                .into_iter()
                .filter(|&d| app.dialog_open(d))
                .collect::<Vec<_>>()
        };
        use Dialog::{About, ApplyConfirm as Confirm, Help, History, Move};
        assert_eq!(open(&driver.app), [Help, Confirm, About, Move, History]);
        for left in [
            &[Confirm, About, Move, History][..],
            &[About, Move, History],
            &[Move, History],
            &[History],
            &[],
        ] {
            driver.press("escape");
            assert_eq!(open(&driver.app), left);
        }
    }

    #[test]
    fn closing_a_closed_dialog_leaves_it_closed() {
        let fx = Fixture::new(&["work"]);
        let (mut driver, _wires) = fx.driver();
        driver.apply(UiAction::About);
        driver.apply(UiAction::ToggleHelp);
        driver.apply(UiAction::ToggleHistory);
        // A double click on a close button.
        for dialog in [Dialog::About, Dialog::Help, Dialog::History] {
            driver.apply(UiAction::Close(dialog));
            driver.apply(UiAction::Close(dialog));
            assert!(!driver.app.dialog_open(dialog), "{dialog:?}");
        }
    }

    #[test]
    fn escape_and_the_close_button_both_clear_the_status_line_with_the_history() {
        let fx = Fixture::new(&["work"]);
        let (mut driver, _wires) = fx.driver();
        for close in [UiAction::Escape, UiAction::Close(Dialog::History)] {
            driver.apply(UiAction::ToggleHistory);
            driver.app.error = Some("work: boom".into());
            driver.app.notice = Some("removed 1 backup".into());
            driver.apply(close.clone());
            assert!(!driver.app.history_open, "{close:?}");
            assert_eq!((&driver.app.error, &driver.app.notice), (&None, &None));
        }
    }

    #[test]
    fn escape_cancels_the_unsaved_changes_question() {
        let fx = Fixture::new(&["work"]);
        let (mut driver, _wires) = fx.driver();
        driver.app.leave_draft = Some(Box::new(UiAction::SelectView(View::Rules)));
        driver.press("escape");
        assert!(driver.app.leave_draft.is_none());
        assert_ne!(driver.app.view, View::Rules);
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
            preview: true,
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
            [
                work("INBOX", &[1], Action::MarkRead),
                work("INBOX", &[1], Action::Flag)
            ]
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
        assert_eq!(
            wires.sent(),
            [work("INBOX", &[1], Action::Move("Receipts".into()))]
        );
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
            uidplus: None,
            encrypted: None,
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
        driver.at(10.0);
        driver.tick_until(|app| !app.daemon_lost);
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
        assert_eq!(sent, [work("INBOX", &[1], Action::Archive)]);
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
        driver.at(10.0);
        driver.tick_until(|app| app.error.as_deref() == Some(newer));
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
        driver.receive(&wires, [refused("work", 0, reason)]);
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
        driver.receive(&wires, foreign);
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
        driver.receive(&wires, [refused("work", 0, reason)]);
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
        driver.at(0.0);
        driver.tick_until(update_done);
        assert_eq!(driver.app.newer_release(), Some("999.0.0"));
    }

    #[test]
    fn a_failed_check_shows_nothing_and_is_remembered() {
        let fx = Fixture::new(&["work"]);
        fx.append_config("[ui]\ncheck_updates = true\n");
        let (mut driver, _wires) = fx.driver();
        driver.at(0.0);
        driver.tick_until(update_done);
        assert_eq!(driver.app.newer_release(), None);
        assert!(update::load(&fx.paths.update_check()).is_some());
    }

    #[test]
    fn by_default_no_check_starts() {
        let fx = Fixture::new(&["work"]);
        let (mut driver, _wires) = fx.driver();
        driver.tick();
        assert!(matches!(driver.app.update, UpdateCheck::Done(None)));
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
        driver.receive(&wires, [refused("home", 0, reason)]);
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

    fn slashed(name: &str) -> postvak_core::store::Folder {
        postvak_core::store::Folder {
            name: name.into(),
            uidvalidity: 1,
            last_uid: 0,
            special_use: None,
            delimiter: Some("/".into()),
        }
    }

    /// The folder dialog's buttons, as (title, enabled).
    fn buttons(driver: &Driver) -> Vec<(String, bool)> {
        folders::question(&driver.app)
            .unwrap()
            .buttons
            .into_iter()
            .map(|(title, enabled, _)| (title, enabled))
            .collect()
    }

    fn created(name: &str) -> Event {
        Event::FolderCreated {
            account: "work".into(),
            name: name.into(),
            request: 7,
        }
    }

    fn deleted(name: &str) -> Event {
        Event::FolderDeleted {
            account: "work".into(),
            name: name.into(),
            request: 7,
        }
    }

    fn renamed(from: &str, to: &str) -> Event {
        Event::FolderRenamed {
            account: "work".into(),
            from: from.into(),
            to: to.into(),
            request: 7,
        }
    }

    fn not_empty(name: &str, count: u32, has_trash: bool) -> Event {
        Event::FolderNotEmpty {
            account: "work".into(),
            name: name.into(),
            count,
            has_trash,
            request: 7,
        }
    }

    fn delete(name: &str, mode: Option<DeleteMode>, count: u32) -> (String, Command) {
        let command = Command::DeleteFolder {
            name: name.into(),
            mode,
            count,
        };
        ("work".into(), command)
    }

    #[test]
    fn new_folder_under_selected_joins_with_delimiter() {
        let fx = Fixture::new(&["work"]);
        fx.store("work")
            .upsert_folder(&slashed("Projects/Website"))
            .unwrap();
        let (mut driver, wires) = fx.driver();
        driver.apply(UiAction::NewFolder(0, Some("Projects/Website".into())));
        driver.press("Q3 enter");
        let create = Command::CreateFolder {
            name: "Projects/Website/Q3".into(),
        };
        assert_eq!(wires.sent(), [("work".to_string(), create)]);
        assert!(
            driver.app.folder_dialog.is_some(),
            "open until the daemon answers"
        );
        driver.receive(&wires, [created("Projects/Website/Q3")]);
        assert!(driver.app.folder_dialog.is_none());
    }

    #[test]
    fn new_folder_on_an_account_that_keeps_folders_under_inbox_goes_there() {
        let fx = Fixture::new(&["work"]);
        let clients = postvak_core::store::Folder {
            delimiter: Some(".".into()),
            ..slashed("INBOX.Clients")
        };
        fx.store("work").upsert_folder(&clients).unwrap();
        let (mut driver, wires) = fx.driver();
        driver.apply(UiAction::NewFolder(0, None));
        driver.press("Q3 enter");
        let create = Command::CreateFolder {
            name: "INBOX.Q3".into(),
        };
        assert_eq!(wires.sent(), [("work".to_string(), create)]);
    }

    #[test]
    fn new_folder_name_error_disables_confirm() {
        let fx = Fixture::new(&["work"]);
        let (mut driver, wires) = fx.driver();
        driver.apply(UiAction::NewFolder(0, None));
        assert_eq!(buttons(&driver), [("Create".to_string(), false)], "empty");
        driver.press("INBOX");
        let dialog = driver.app.folder_dialog.as_ref().unwrap();
        assert_eq!(
            dialog.error.as_deref(),
            Some("a folder named INBOX already exists")
        );
        assert_eq!(buttons(&driver), [("Create".to_string(), false)]);
        driver.press("enter");
        assert!(wires.sent().is_empty());
    }

    #[test]
    fn a_refusal_shows_in_the_folder_dialog() {
        let fx = Fixture::new(&["work"]);
        let (mut driver, wires) = fx.driver();
        driver.apply(UiAction::NewFolder(0, None));
        driver.press("Lists enter");
        assert_eq!(wires.sent().len(), 1);
        driver.receive(&wires, [refused("work", 0, "work is offline")]);
        let dialog = driver.app.folder_dialog.as_ref().unwrap();
        assert_eq!(dialog.error.as_deref(), Some("work is offline"));
        assert_eq!(driver.app.accounts[0].error, None);
        assert_eq!(buttons(&driver), [("Create".to_string(), false)]);
        driver.press("2");
        assert_eq!(
            buttons(&driver),
            [("Create".to_string(), true)],
            "an edit tries again"
        );
    }

    #[test]
    fn rename_lists_rules_that_follow() {
        let fx = Fixture::new(&["home", "work"]);
        fx.store("work")
            .upsert_folder(&slashed("Projects/Website"))
            .unwrap();
        let rules = "[[rules]]\nname = \"sites\"\nmatch.subject = { contains = \"site\" }\nactions = { move = \"Projects/Website\" }\n\n[[rules]]\nname = \"old\"\naccount = \"work\"\nfolder = \"Projects/Website/Old\"\nmatch.subject = { contains = \"old\" }\nactions = { flagged = true }\n\n[[rules]]\nname = \"home\"\naccount = \"home\"\nmatch.subject = { contains = \"site\" }\nactions = { move = \"Projects/Website\" }\n";
        std::fs::write(fx.paths.rules_file(), rules).unwrap();
        let (mut driver, wires) = fx.driver();
        driver.apply(UiAction::RenameFolder(1, "Projects/Website".into()));
        let dialog = driver.app.folder_dialog.as_ref().unwrap();
        assert_eq!(dialog.name, "Website");
        assert_eq!(
            buttons(&driver),
            [("Rename".to_string(), false)],
            "unchanged"
        );
        driver.apply(UiAction::FolderName("Site".into()));
        let question = folders::question(&driver.app).unwrap();
        assert_eq!(
            question.text,
            [
                help::folder_rule_split("sites"),
                help::folder_rule_will_follow("old")
            ]
            .join("\n")
        );
        driver.press("enter");
        let rename = Command::RenameFolder {
            from: "Projects/Website".into(),
            to: "Projects/Site".into(),
        };
        assert_eq!(wires.sent(), [("work".to_string(), rename)]);
        driver.receive(&wires, [renamed("Projects/Website", "Projects/Site")]);
        assert!(driver.app.folder_dialog.is_none());
        let file = std::fs::read_to_string(fx.paths.rules_file()).unwrap();
        assert!(file.contains("folder = \"Projects/Site/Old\""), "{file}");
        assert!(file.contains("name = \"sites (work)\""), "{file}");
    }

    #[test]
    fn rename_on_an_account_without_a_delimiter_sends_the_name() {
        let fx = Fixture::new(&["work"]);
        fx.folder("work", "Old", None);
        let (mut driver, wires) = fx.driver();
        driver.apply(UiAction::RenameFolder(0, "Old".into()));
        driver.apply(UiAction::FolderName("New".into()));
        driver.press("enter");
        let rename = Command::RenameFolder {
            from: "Old".into(),
            to: "New".into(),
        };
        assert_eq!(wires.sent(), [("work".to_string(), rename)]);
    }

    #[test]
    fn delete_not_empty_offers_trash_and_permanent() {
        let fx = Fixture::new(&["work"]);
        fx.folder("work", "Old", None);
        let (mut driver, wires) = fx.driver();
        driver.apply(UiAction::DeleteFolder(0, "Old".into()));
        assert!(wires.sent().is_empty(), "asks first");
        driver.apply(UiAction::FolderConfirm(None));
        assert_eq!(wires.sent(), [delete("Old", None, 0)]);
        assert_eq!(folders::question(&driver.app).unwrap().text, "Deleting…");
        driver.receive(&wires, [not_empty("Old", 3, true)]);
        assert_eq!(
            buttons(&driver),
            [
                ("Move 3 messages to Trash".to_string(), true),
                ("Delete 3 messages permanently".to_string(), true)
            ]
        );
        driver.apply(UiAction::FolderConfirm(Some(DeleteMode::Trash)));
        assert_eq!(wires.sent(), [delete("Old", Some(DeleteMode::Trash), 3)]);
        driver.receive(&wires, [deleted("Old")]);
        assert!(driver.app.folder_dialog.is_none());
    }

    #[test]
    fn empty_trash_asks_then_deletes_every_message_in_it() {
        let fx = Fixture::new(&["work"]);
        fx.folder("work", "Trash", Some("Trash"));
        for uid in [4, 7] {
            fx.add("work", crate::test_support::message("Trash", uid, "old"));
        }
        let (mut driver, wires) = fx.driver();
        driver.apply(UiAction::EmptyTrash(0, "Trash".into()));
        assert!(wires.sent().is_empty(), "asks first");
        let question = folders::question(&driver.app).unwrap();
        assert_eq!(question.text, "Permanently delete 2 messages in Trash?");
        assert_eq!(buttons(&driver), [("Empty Trash".to_string(), true)]);
        assert_eq!(question.danger, [true]);
        driver.apply(UiAction::FolderConfirm(None));
        let apply = Command::Apply {
            folder: "Trash".into(),
            uids: vec![4, 7],
            action: Action::Delete,
            by: "gui".into(),
        };
        assert_eq!(wires.sent(), [("work".to_string(), apply)]);
        assert!(driver.app.folder_dialog.is_none());
    }

    #[test]
    fn an_empty_trash_has_nothing_to_ask() {
        let fx = Fixture::new(&["work"]);
        fx.folder("work", "Trash", Some("Trash"));
        let (mut driver, wires) = fx.driver();
        driver.apply(UiAction::EmptyTrash(0, "Trash".into()));
        assert!(driver.app.folder_dialog.is_none());
        assert!(wires.sent().is_empty());
    }

    #[test]
    fn only_a_choice_that_destroys_mail_or_a_folder_is_drawn_as_danger() {
        let fx = Fixture::new(&["work"]);
        fx.folder("work", "Old", None);
        let (mut driver, wires) = fx.driver();
        driver.apply(UiAction::DeleteFolder(0, "Old".into()));
        assert_eq!(
            folders::question(&driver.app).unwrap().danger,
            [true],
            "Delete"
        );
        driver.apply(UiAction::FolderConfirm(None));
        driver.receive(&wires, [not_empty("Old", 3, true)]);
        let danger = folders::question(&driver.app).unwrap().danger;
        assert_eq!(danger, [false, true], "Trash leads, permanently is danger");
        driver.apply(UiAction::Close(Dialog::Folder));
        driver.apply(UiAction::NewFolder(0, None));
        assert_eq!(
            folders::question(&driver.app).unwrap().danger,
            [false],
            "Create"
        );
    }

    #[test]
    fn delete_without_trash_offers_permanent_only() {
        let fx = Fixture::new(&["work"]);
        fx.folder("work", "Old", None);
        let (mut driver, wires) = fx.driver();
        driver.apply(UiAction::DeleteFolder(0, "Old".into()));
        driver.apply(UiAction::FolderConfirm(None));
        driver.receive(&wires, [not_empty("Old", 1, false)]);
        assert_eq!(
            buttons(&driver),
            [("Delete 1 message permanently".to_string(), true)]
        );
        driver.apply(UiAction::FolderConfirm(Some(DeleteMode::Trash)));
        assert_eq!(
            wires.sent(),
            [delete("Old", None, 0)],
            "no Trash to move to"
        );
    }

    #[test]
    fn renamed_selected_folder_keeps_the_view() {
        let fx = Fixture::new(&["work"]);
        fx.store("work")
            .upsert_folder(&slashed("Projects/Website"))
            .unwrap();
        let (mut driver, wires) = fx.driver();
        let open = |folder: &str| View::Folder {
            account: 0,
            folder: folder.into(),
        };
        driver.apply(UiAction::SelectView(open("Projects/Website")));
        driver.receive(&wires, [renamed("Projects/Website", "Projects/Site")]);
        assert_eq!(driver.app.view, open("Projects/Site"));
    }

    #[test]
    fn deleted_selected_folder_returns_to_inbox() {
        let fx = Fixture::new(&["work"]);
        fx.folder("work", "Old", None);
        let (mut driver, wires) = fx.driver();
        let open = |folder: &str| View::Folder {
            account: 0,
            folder: folder.into(),
        };
        driver.apply(UiAction::SelectView(open("Old")));
        driver.receive(&wires, [deleted("Old")]);
        assert_eq!(driver.app.view, open("INBOX"));
    }

    #[test]
    fn renamed_parent_of_the_selected_folder_keeps_the_view() {
        let fx = Fixture::new(&["work"]);
        for name in ["Projects/Website", "Projects/Website/Q3"] {
            fx.store("work").upsert_folder(&slashed(name)).unwrap();
        }
        let (mut driver, wires) = fx.driver();
        let open = |folder: &str| View::Folder {
            account: 0,
            folder: folder.into(),
        };
        driver.apply(UiAction::SelectView(open("Projects/Website/Q3")));
        driver.receive(&wires, [renamed("Projects/Website", "Projects/Site")]);
        assert_eq!(driver.app.view, open("Projects/Site/Q3"));
    }

    #[test]
    fn deleted_selected_folder_named_in_another_normalization_returns_to_inbox() {
        let fx = Fixture::new(&["work"]);
        fx.folder("work", "\u{c9}\u{e9}n", None);
        let (mut driver, wires) = fx.driver();
        let open = |folder: &str| View::Folder {
            account: 0,
            folder: folder.into(),
        };
        driver.apply(UiAction::SelectView(open("\u{c9}\u{e9}n")));
        driver.receive(&wires, [deleted("E\u{301}e\u{301}n")]);
        assert_eq!(driver.app.view, open("INBOX"));
    }

    #[test]
    fn escape_closes_the_folder_dialog() {
        let fx = Fixture::new(&["work"]);
        let (mut driver, _wires) = fx.driver();
        driver.apply(UiAction::NewFolder(0, None));
        assert_eq!(driver.app.top_dialog(), Some(Dialog::Folder));
        driver.press("escape");
        assert!(driver.app.folder_dialog.is_none());
    }
}
