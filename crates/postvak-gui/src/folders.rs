//! The folder pane without a toolkit: the tree of custom folders, the order of special ones, labels and unread counts.
//! The window draws All inboxes, each account's folders and the Rules, Activity and Backups entries from it.

use postvak_core::message::clean;
use postvak_core::store::Folder;

use super::state::{Account, App, FolderRow, UiAction, View};
use super::{icons, list, numbers, status};

/// A line of the folder pane's scrolling part, top to bottom.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Entry {
    /// An account's name, with a spinner while it syncs and its store error.
    Account {
        name: String,
        busy: bool,
        error: Option<String>,
        /// After the first account, with space above it.
        gap: bool,
    },
    Folder(FolderEntry),
    Separator,
}

/// A row that opens a view: All inboxes, a folder, or one of the views pinned at the bottom.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FolderEntry {
    pub icon: &'static str,
    pub label: String,
    /// The unread count shown on the right, grouped.
    pub count: Option<String>,
    /// The accessible name: the label, with the count in brackets.
    pub name: String,
    pub depth: usize,
    /// The whole path, for a folder deeper than `DEEPEST`, whose own name may not say where it is.
    pub path: Option<String>,
    pub selected: bool,
    /// What a click opens; None for a parent the server does not list.
    pub view: Option<View>,
    /// For a parent row: whether it is expanded, its account and path.
    pub branch: Option<(bool, usize, String)>,
    pub unread: u32,
}

impl FolderEntry {
    fn new(icon: &'static str, label: String, unread: u32, view: View, app: &App) -> FolderEntry {
        let (count, name) = count_and_name(&label, unread);
        FolderEntry {
            icon,
            label,
            count,
            name,
            depth: 0,
            path: None,
            selected: app.view == view,
            view: Some(view),
            branch: None,
            unread,
        }
    }

    /// Steps in from the left: one per level, up to `DEEPEST`.
    pub(crate) fn indent(&self) -> usize {
        self.depth.min(DEEPEST)
    }

    /// The chevron on a parent row: down while it is open.
    pub(crate) fn caret(&self) -> Option<&'static str> {
        let (expanded, ..) = self.branch.as_ref()?;
        Some(if *expanded {
            icons::CARET_DOWN
        } else {
            icons::CARET_RIGHT
        })
    }

    /// The caret's accessible name on a parent row.
    pub(crate) fn caret_name(&self) -> Option<String> {
        let (expanded, ..) = self.branch.as_ref()?;
        let verb = if *expanded { "Collapse" } else { "Expand" };
        Some(format!("{verb} {}", self.label))
    }

    /// What the right-click menu offers, as (title, enabled, action): Open and Collapse or Expand, then, below a
    /// separator, marking read and syncing.
    pub(crate) fn menu(&self) -> [Vec<(String, bool, UiAction)>; 2] {
        let mut items = Vec::new();
        let folder = match &self.view {
            Some(View::Folder { account, folder }) => Some((*account, folder.clone())),
            _ => None,
        };
        if let Some(view) = &self.view {
            items.push(("Open".to_string(), true, UiAction::SelectView(view.clone())));
        }
        if let Some((expanded, account, path)) = &self.branch {
            let title = if *expanded { "Collapse" } else { "Expand" };
            items.push((
                title.to_string(),
                true,
                UiAction::ToggleFolder(*account, path.clone()),
            ));
        }
        let mut below = Vec::new();
        if let Some((account, folder)) = folder {
            let mark = UiAction::MarkFolderRead(account, folder);
            below.push(("Mark all as read".to_string(), self.unread > 0, mark));
        }
        below.push((status::sync_key().to_string(), true, UiAction::SyncNow));
        [items, below]
    }
}

/// The scrolling part of the folder pane: All inboxes when there is more than one account, then per account its
/// name, its special folders, a separator and its custom folders as a tree.
pub(crate) fn entries(app: &App) -> Vec<Entry> {
    let mut entries = Vec::new();
    if app.shows_all_inboxes() {
        let unread = inbox_unread(&app.accounts);
        let all = FolderEntry::new(
            icons::ALL_INBOXES,
            "All inboxes".into(),
            unread,
            View::AllInboxes,
            app,
        );
        entries.push(Entry::Folder(all));
        entries.push(Entry::Separator);
    }
    for (index, account) in app.accounts.iter().enumerate() {
        entries.push(Entry::Account {
            name: account.name.clone(),
            busy: account.busy(),
            error: account.store.as_ref().err().map(|e| clean(e, false)),
            gap: index > 0,
        });
        entries.extend(account_entries(app, index, account));
    }
    entries
}

fn account_entries(app: &App, index: usize, account: &Account) -> Vec<Entry> {
    let open = |name: &str| View::Folder {
        account: index,
        folder: name.to_string(),
    };
    let (special, custom): (Vec<&FolderRow>, Vec<&FolderRow>) = account
        .folders
        .iter()
        .partition(|f| is_special(&f.name, f.special_use.as_deref()));
    let mut entries: Vec<Entry> = special
        .iter()
        .map(|folder| {
            let icon = special_folder(&folder.name, folder.special_use.as_deref())
                .map_or(icons::FOLDER, |(_, icon)| icon);
            let label = clean(label(&folder.name), false);
            Entry::Folder(FolderEntry::new(
                icon,
                label,
                folder.unread,
                open(&folder.name),
                app,
            ))
        })
        .collect();
    if !special.is_empty() && !custom.is_empty() {
        entries.push(Entry::Separator);
    }
    let rows = tree(&account.folders);
    let is_collapsed = |path: &str| app.collapsed.contains(&(index, path.to_string()));
    for row in visible(&rows, is_collapsed) {
        let unread = row
            .folder
            .as_ref()
            .and_then(|name| account.folders.iter().find(|f| &f.name == name))
            .map_or(0, |f| f.unread);
        let expanded = !is_collapsed(&row.path);
        let label = clean(&row.label, false);
        let (count, name) = count_and_name(&label, unread);
        let view = row.folder.as_deref().map(open);
        entries.push(Entry::Folder(FolderEntry {
            icon: icons::FOLDER,
            label,
            count,
            name,
            depth: row.depth,
            path: (row.depth > DEEPEST).then(|| clean(&row.trail, false)),
            selected: view.as_ref() == Some(&app.view),
            view,
            branch: row
                .has_children
                .then(|| (expanded, index, row.path.clone())),
            unread,
        }));
    }
    entries
}

/// The deepest level that still steps in; folders below it line up with it, and show their path on hover.
pub(crate) const DEEPEST: usize = 3;

/// Rules, Activity and Backups, pinned at the bottom of the pane. Rules counts the proposals waiting for approval.
pub(crate) fn pinned(app: &App) -> [FolderEntry; 3] {
    let proposals = u32::try_from(app.rules.proposals.len()).unwrap_or(u32::MAX);
    [
        FolderEntry::new(icons::RULES, "Rules".into(), proposals, View::Rules, app),
        FolderEntry::new(icons::ACTIVITY, "Activity".into(), 0, View::Activity, app),
        FolderEntry::new(icons::BACKUPS, "Backups".into(), 0, View::Trash, app),
    ]
}

/// Unread mail in every account's INBOX, for the All inboxes count.
pub(crate) fn inbox_unread(accounts: &[Account]) -> u32 {
    accounts
        .iter()
        .flat_map(|account| &account.folders)
        .filter(|folder| folder.name == list::INBOX)
        .map(|folder| folder.unread)
        .sum()
}

pub(crate) fn count_and_name(name: &str, unread: u32) -> (Option<String>, String) {
    match unread {
        0 => (None, name.to_string()),
        unread => {
            let unread = numbers::count(u64::from(unread));
            (Some(unread.clone()), format!("{name} ({unread})"))
        }
    }
}

/// One row of the custom folder tree.
#[derive(Debug, PartialEq)]
pub(crate) struct TreeRow {
    /// The folder a click opens; None for a parent the server does not list (not selectable).
    pub folder: Option<String>,
    pub label: String,
    pub depth: usize,
    /// Segments joined by '/', the key for collapse state.
    pub path: String,
    /// Segments joined by " / ", as the hover text shows the path.
    pub trail: String,
    pub has_children: bool,
}

/// The custom folders as a tree. Sorted by path segments, not by full name, so "Work Archive" cannot land between
/// "Work" and "Work/Clients". An `INBOX` + delimiter prefix (Dovecot and Courier namespaces) is dropped when every
/// custom folder has it; otherwise INBOX's children stay under an INBOX branch, apart from top-level namesakes.
pub(crate) fn tree(folders: &[FolderRow]) -> Vec<TreeRow> {
    let custom: Vec<&FolderRow> = folders
        .iter()
        .filter(|f| !is_special(&f.name, f.special_use.as_deref()))
        .collect();
    let strip = !custom.is_empty() && custom.iter().all(|f| inbox_prefix(f).is_some());
    let mut paths: Vec<(Vec<String>, &FolderRow)> = custom
        .into_iter()
        .map(|f| (segments(f, strip), f))
        .collect();
    paths.sort_by_key(|(segments, _)| {
        segments
            .iter()
            .map(|s| s.to_lowercase())
            .collect::<Vec<_>>()
    });
    let mut rows: Vec<TreeRow> = Vec::new();
    for (segments, folder) in paths {
        for depth in 0..segments.len() {
            let path = segments[..=depth].join("/");
            let leaf = depth + 1 == segments.len();
            // ponytail: linear parent lookup; a HashMap from path to index if accounts with thousands of folders show up
            match rows.iter_mut().find(|row| row.path == path) {
                Some(row) if leaf => row.folder = Some(folder.name.clone()),
                Some(_) => {}
                None => rows.push(TreeRow {
                    folder: leaf.then(|| folder.name.clone()),
                    label: segments[depth].clone(),
                    depth,
                    path,
                    trail: segments[..=depth].join(" / "),
                    has_children: false,
                }),
            }
        }
    }
    for row in rows.iter_mut().filter(|row| row.folder.is_none()) {
        row.folder = folders
            .iter()
            .find(|f| segments(f, strip).join("/") == row.path)
            .map(|f| f.name.clone());
    }
    for index in 1..rows.len() {
        if rows[index].depth > rows[index - 1].depth {
            rows[index - 1].has_children = true;
        }
    }
    rows
}

/// The name after a leading `INBOX` + delimiter, when it has one.
fn inbox_prefix(folder: &FolderRow) -> Option<&str> {
    let delimiter = folder.delimiter.as_deref().filter(|d| !d.is_empty())?;
    let prefix = format!("INBOX{delimiter}");
    let head = folder.name.get(..prefix.len())?;
    head.eq_ignore_ascii_case(&prefix)
        .then(|| &folder.name[prefix.len()..])
}

fn segments(folder: &FolderRow, strip_inbox: bool) -> Vec<String> {
    let Some(delimiter) = folder.delimiter.as_deref().filter(|d| !d.is_empty()) else {
        return vec![folder.name.clone()];
    };
    let name = inbox_prefix(folder)
        .filter(|_| strip_inbox)
        .unwrap_or(&folder.name);
    name.split(delimiter)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// The rows not under a collapsed ancestor.
pub(crate) fn visible(rows: &[TreeRow], collapsed: impl Fn(&str) -> bool) -> Vec<&TreeRow> {
    let mut hidden_below: Option<usize> = None;
    let mut shown = Vec::new();
    for row in rows {
        if hidden_below.is_some_and(|depth| row.depth > depth) {
            continue;
        }
        hidden_below = collapsed(&row.path).then_some(row.depth);
        shown.push(row);
    }
    shown
}

/// The folders an account shows, in the order the pane draws them: special folders, then the visible tree.
pub(crate) fn shown(account: &Account, collapsed: impl Fn(&str) -> bool) -> Vec<String> {
    let special = account
        .folders
        .iter()
        .filter(|f| is_special(&f.name, f.special_use.as_deref()))
        .map(|f| f.name.clone());
    let rows = tree(&account.folders);
    let custom = visible(&rows, collapsed)
        .into_iter()
        .filter_map(|row| row.folder.clone());
    special.chain(custom).collect()
}

/// A folder's name as the window shows it: "Inbox" for the INBOX, which IMAP spells in capitals; commands keep the
/// server's name.
pub(crate) fn label(name: &str) -> &str {
    if name.eq_ignore_ascii_case("INBOX") {
        "Inbox"
    } else {
        name
    }
}

/// INBOX and the special-use folders, which sort above the rest.
pub(crate) fn is_special(name: &str, special_use: Option<&str>) -> bool {
    special_folder(name, special_use).is_some()
}

/// INBOX first, then the special-use folders in a fixed order, then the rest alphabetically.
pub(crate) fn sort(folders: &mut [Folder]) {
    folders.sort_by_key(|f| {
        (
            special_folder(&f.name, f.special_use.as_deref())
                .map_or(SPECIAL.len(), |(rank, _)| rank),
            f.name.to_lowercase(),
        )
    });
}

/// INBOX by name, then the special uses, in the order they sort, with their icons.
const SPECIAL: [(&str, &str); 6] = [
    ("INBOX", icons::INBOX),
    ("Archive", icons::ARCHIVE),
    ("Drafts", icons::DRAFTS),
    ("Sent", icons::SENT),
    ("Junk", icons::JUNK),
    ("Trash", icons::TRASH),
];

/// A special folder's rank and icon; `None` for the rest.
fn special_folder(name: &str, special_use: Option<&str>) -> Option<(usize, &'static str)> {
    let key = if name.eq_ignore_ascii_case("INBOX") {
        "INBOX"
    } else {
        special_use?
    };
    SPECIAL
        .iter()
        .enumerate()
        .find(|(_, (special, _))| *special == key)
        .map(|(rank, (_, icon))| (rank, *icon))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Focus, FolderRow};
    use crate::test_support::{Fixture, message, unread};
    use postvak_core::store::Folder;

    fn stored(name: &str) -> Folder {
        Folder {
            name: name.into(),
            uidvalidity: 1,
            last_uid: 0,
            special_use: None,
            delimiter: Some("/".into()),
        }
    }

    fn work(name: &str) -> View {
        View::Folder {
            account: 0,
            folder: name.into(),
        }
    }

    /// Projects/Postvak, Projects/Website and Zeta in account `account`.
    fn projects(fx: &Fixture, account: &str) {
        let store = fx.store(account);
        for name in ["Projects/Postvak", "Projects/Website", "Zeta"] {
            store.upsert_folder(&stored(name)).unwrap();
        }
    }

    #[test]
    fn special_folders_get_their_icon_and_others_a_folder() {
        assert_eq!(special_folder("inbox", None), Some((0, icons::INBOX)));
        assert_eq!(
            special_folder("Archief", Some("Archive")),
            Some((1, icons::ARCHIVE))
        );
        assert_eq!(
            special_folder("Sent Items", Some("Sent")),
            Some((3, icons::SENT))
        );
        assert_eq!(special_folder("Receipts", None), None);
    }

    #[test]
    fn all_inboxes_sits_on_top_with_every_inbox_unread_count_and_opens_on_click() {
        let fx = Fixture::new(&["home", "work"]);
        fx.folder("work", "Archive", Some("Archive"));
        fx.add("home", unread("INBOX", 1, "hello"));
        fx.add("work", unread("INBOX", 2, "hello"));
        fx.add("work", unread("INBOX", 3, "hello"));
        fx.add("work", unread("Archive", 4, "hello"));
        let (open, _wires) = fx.window();
        assert!(open.span("All inboxes (3)").1 < open.span("home").0);
        open.click("All inboxes (3)");
        assert_eq!(open.with(|live| live.app.view.clone()), View::AllInboxes);
    }

    #[test]
    fn one_account_has_no_all_inboxes_entry() {
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = fx.window();
        let labels = open.with(|live| entries(&live.app));
        assert!(
            !labels
                .iter()
                .any(|e| matches!(e, Entry::Folder(f) if f.view == Some(View::AllInboxes)))
        );
        assert!(open.find("All inboxes").is_empty());
    }

    #[test]
    fn k_in_the_folder_pane_steps_up_from_the_first_inbox_to_all_inboxes() {
        let fx = Fixture::new(&["home", "work"]);
        let (open, _wires) = fx.window();
        open.press("tab tab k");
        assert_eq!(open.with(|live| live.app.view.clone()), View::AllInboxes);
    }

    #[test]
    fn right_clicking_a_folder_opens_its_menu() {
        let fx = Fixture::new(&["work"]);
        fx.add("work", unread("INBOX", 1, "hello"));
        let (open, _wires) = fx.window();
        assert!(open.find("Mark all as read").is_empty());
        open.right_tap("Inbox (1)");
        assert!(!open.find("Mark all as read").is_empty());
    }

    #[test]
    fn a_folder_menu_marks_all_its_unread_mail_read() {
        let fx = Fixture::new(&["work"]);
        for uid in 1..=3 {
            fx.add(
                "work",
                if uid == 2 {
                    message("INBOX", 2, "hi")
                } else {
                    unread("INBOX", uid, "hello")
                },
            );
        }
        let (open, wires) = fx.window();
        let (row, item) = open.with(|live| {
            let entries = entries(&live.app);
            let row = entries
                .iter()
                .position(|e| matches!(e, Entry::Folder(f) if f.name == "Inbox (2)"))
                .unwrap();
            let Entry::Folder(inbox) = &entries[row] else {
                panic!("the Inbox row is a folder")
            };
            let item = inbox.menu()[1]
                .iter()
                .position(|(title, ..)| title == "Mark all as read")
                .unwrap();
            (row, item)
        });
        open.window().invoke_folder_menu(row as i32, 1, item as i32);
        let read = postvak_core::sync::Command::Apply {
            folder: "INBOX".into(),
            uids: vec![1, 3],
            action: postvak_core::rules::Action::MarkRead,
            by: "gui".into(),
        };
        assert_eq!(wires.sent(), [("work".to_string(), read)]);
    }

    #[test]
    fn the_folder_menu_offers_open_collapse_mark_read_and_sync() {
        let fx = Fixture::new(&["work"]);
        projects(&fx, "work");
        let (app, _wires) = fx.app();
        let entries = entries(&app);
        let titles = |label: &str| -> Vec<(String, bool)> {
            let folder = entries
                .iter()
                .find_map(|e| match e {
                    Entry::Folder(f) if f.label == label => Some(f),
                    _ => None,
                })
                .unwrap();
            folder
                .menu()
                .into_iter()
                .flatten()
                .map(|(title, enabled, _)| (title, enabled))
                .collect()
        };
        let sync = (status::sync_key().to_string(), true);
        assert_eq!(
            titles("Projects"),
            [("Collapse".to_string(), true), sync.clone()]
        );
        assert_eq!(
            titles("Inbox"),
            [
                ("Open".to_string(), true),
                ("Mark all as read".to_string(), false),
                sync
            ]
        );
    }

    #[test]
    fn stepping_from_a_folder_in_a_collapsed_branch_stays_in_its_account() {
        let fx = Fixture::new(&["home", "work"]);
        projects(&fx, "work");
        let (open, _wires) = fx.window();
        let work = |name: &str| View::Folder {
            account: 1,
            folder: name.into(),
        };
        open.with(|live| live.app.select_view(work("Projects/Postvak")));
        open.click("Collapse Projects");
        open.with(|live| live.app.focus = Focus::Folders);
        open.press("down");
        assert_eq!(open.with(|live| live.app.view.clone()), work("Zeta"));
    }

    #[test]
    fn folders_step_in_three_levels_and_deeper_ones_show_their_path() {
        let fx = Fixture::new(&["work"]);
        fx.store("work")
            .upsert_folder(&stored("A/B/C/D/Old explorations"))
            .unwrap();
        let (app, _wires) = fx.app();
        let rows: Vec<(String, usize, Option<String>, Option<&str>)> = entries(&app)
            .into_iter()
            .filter_map(|e| match e {
                Entry::Folder(f) => Some((f.label.clone(), f.indent(), f.path.clone(), f.caret())),
                _ => None,
            })
            .collect();
        let open = Some(icons::CARET_DOWN);
        let row = |label: &str, indent, path: Option<&str>, caret| {
            (label.to_string(), indent, path.map(str::to_string), caret)
        };
        assert_eq!(
            rows,
            [
                row("Inbox", 0, None, None),
                row("A", 0, None, open),
                row("B", 1, None, open),
                row("C", 2, None, open),
                row("D", 3, None, open),
                row(
                    "Old explorations",
                    3,
                    Some("A / B / C / D / Old explorations"),
                    None
                ),
            ]
        );
    }

    #[test]
    fn a_collapsed_parent_points_its_chevron_right() {
        let fx = Fixture::new(&["work"]);
        projects(&fx, "work");
        let (open, _wires) = fx.window();
        open.click("Collapse Projects");
        let caret = open.with(|live| {
            entries(&live.app).into_iter().find_map(|e| match e {
                Entry::Folder(f) if f.label == "Projects" => f.caret(),
                _ => None,
            })
        });
        assert_eq!(caret, Some(icons::CARET_RIGHT));
    }

    #[test]
    fn clicking_a_caret_collapses_its_folder() {
        let fx = Fixture::new(&["work"]);
        projects(&fx, "work");
        let (open, _wires) = fx.window();
        assert_eq!(open.find("Website").len(), 1);
        open.click("Collapse Projects");
        assert!(open.find("Website").is_empty());
        assert_eq!(open.find("Expand Projects").len(), 1);
        let collapsed = open.with(|live| live.app.collapsed.contains(&(0, "Projects".to_string())));
        assert!(collapsed);
    }

    #[test]
    fn arrow_keys_follow_the_tree_and_skip_collapsed_folders() {
        let fx = Fixture::new(&["work"]);
        projects(&fx, "work");
        let (open, _wires) = fx.window();
        open.with(|live| {
            live.app.select_view(work("Projects/Postvak"));
            live.app.focus = Focus::Folders;
        });
        open.press("down");
        assert_eq!(
            open.with(|live| live.app.view.clone()),
            work("Projects/Website")
        );
        open.click("Collapse Projects");
        open.with(|live| {
            live.app.select_view(work("INBOX"));
            live.app.focus = Focus::Folders;
        });
        open.press("down");
        assert_eq!(open.with(|live| live.app.view.clone()), work("Zeta"));
    }

    #[test]
    fn tree_shows_folders_with_unread_counts_and_selects_on_click() {
        let fx = Fixture::new(&["work"]);
        fx.folder("work", "Archive", Some("Archive"));
        fx.add("work", unread("INBOX", 1, "hello"));
        fx.add("work", message("INBOX", 2, "read already"));
        let (open, _wires) = fx.window();
        assert_eq!(open.find("Inbox (1)").len(), 1);
        open.click("Archive");
        assert_eq!(open.with(|live| live.app.view.clone()), work("Archive"));
        open.click("Rules");
        assert_eq!(open.with(|live| live.app.view.clone()), View::Rules);
    }

    #[test]
    fn rules_counts_the_proposals_waiting_for_approval() {
        let fx = Fixture::new(&["work"]);
        std::fs::write(
            fx.paths.rules_file(),
            "[[rules]]\nname = \"codes\"\nenabled = false\nproposed_by = \"agent\"\nmatch.subject = { contains = \"code\" }\nactions = [\"delete\"]\n",
        )
        .unwrap();
        let (open, _wires) = fx.window();
        assert_eq!(open.find("Rules (1)").len(), 1);
    }

    #[test]
    fn the_backup_view_is_labelled_backups_and_sits_at_the_bottom() {
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = fx.window();
        let (_, bottom) = open.span("Backups");
        assert!(
            bottom > 700.0,
            "pinned to the bottom of an 800 px window, at {bottom}"
        );
        open.click("Backups");
        assert_eq!(open.with(|live| live.app.view.clone()), View::Trash);
    }

    #[test]
    fn a_store_that_fails_to_open_shows_its_error_and_other_accounts_still_work() {
        let fx = Fixture::new(&["broken", "work"]);
        fx.break_store("broken");
        let (app, _wires) = fx.app();
        let errors: Vec<String> = entries(&app)
            .into_iter()
            .filter_map(|e| match e {
                Entry::Account { error, .. } => error,
                _ => None,
            })
            .collect();
        assert_eq!(errors.len(), 1);
        assert!(
            errors[0].starts_with("could not open the store"),
            "{errors:?}"
        );
        drop(app);
        let (open, _wires) = fx.window();
        assert_eq!(open.find(&errors[0]).len(), 1);
        // The other account's Inbox row, and the list's title above that Inbox.
        assert_eq!(open.find("Inbox").len(), 2);
    }

    fn folder(name: &str, special_use: Option<&str>) -> Folder {
        Folder {
            name: name.into(),
            uidvalidity: 1,
            last_uid: 0,
            special_use: special_use.map(str::to_string),
            delimiter: None,
        }
    }

    fn custom(name: &str, delimiter: &str) -> FolderRow {
        FolderRow {
            name: name.into(),
            special_use: None,
            unread: 0,
            delimiter: Some(delimiter.into()),
        }
    }

    fn shape(rows: &[TreeRow]) -> Vec<(usize, &str, bool)> {
        rows.iter()
            .map(|r| (r.depth, r.label.as_str(), r.folder.is_some()))
            .collect()
    }

    #[test]
    fn the_inbox_shows_as_inbox_and_other_names_stay() {
        assert_eq!(label("INBOX"), "Inbox");
        assert_eq!(label("inbox"), "Inbox");
        assert_eq!(label("INBOX/Receipts"), "INBOX/Receipts");
        assert_eq!(label("Archive"), "Archive");
    }

    #[test]
    fn tree_nests_by_delimiter_and_adds_unlisted_parents() {
        let rows = tree(&[
            custom("Projects/Postvak/GitHub", "/"),
            custom("Projects/Website", "/"),
        ]);
        assert_eq!(
            shape(&rows),
            [
                (0, "Projects", false),
                (1, "Postvak", false),
                (2, "GitHub", true),
                (1, "Website", true)
            ]
        );
        assert!(rows[0].has_children && rows[1].has_children && !rows[2].has_children);
    }

    #[test]
    fn a_sibling_sorting_between_parent_and_child_does_not_steal_the_child() {
        let rows = tree(&[
            custom("Work", "/"),
            custom("Work Archive", "/"),
            custom("Work/Clients", "/"),
        ]);
        assert_eq!(
            shape(&rows),
            [
                (0, "Work", true),
                (1, "Clients", true),
                (0, "Work Archive", true)
            ]
        );
    }

    #[test]
    fn an_inbox_namespace_prefix_is_not_a_parent() {
        let rows = tree(&[
            custom("INBOX.Clients", "."),
            custom("INBOX.Clients.Acme", "."),
        ]);
        assert_eq!(shape(&rows), [(0, "Clients", true), (1, "Acme", true)]);
    }

    #[test]
    fn the_inbox_prefix_is_dropped_only_when_every_custom_folder_has_it() {
        let rows = tree(&[custom("Clients", "."), custom("INBOX.Clients", ".")]);
        let mut opened: Vec<&str> = rows.iter().filter_map(|r| r.folder.as_deref()).collect();
        opened.sort();
        assert_eq!(opened, ["Clients", "INBOX.Clients"]);
    }

    #[test]
    fn a_special_folder_with_children_is_an_openable_parent() {
        let mut archive = custom("Archive", "/");
        archive.special_use = Some("Archive".into());
        let rows = tree(&[archive, custom("Archive/2024", "/")]);
        assert_eq!(rows[0].folder.as_deref(), Some("Archive"));
        assert_eq!(shape(&rows), [(0, "Archive", true), (1, "2024", true)]);
    }

    #[test]
    fn a_folder_without_a_delimiter_stays_flat() {
        let mut flat = custom("a/b", "/");
        flat.delimiter = None;
        assert_eq!(shape(&tree(&[flat])), [(0, "a/b", true)]);
    }

    #[test]
    fn collapsing_hides_descendants_only() {
        let rows = tree(&[custom("A/B/C", "/"), custom("A/D", "/"), custom("E", "/")]);
        let shown: Vec<&str> = visible(&rows, |path| path == "A/B")
            .iter()
            .map(|r| r.label.as_str())
            .collect();
        assert_eq!(shown, ["A", "B", "D", "E"]);
    }

    #[test]
    fn sort_puts_inbox_then_special_use_then_the_rest_alphabetically() {
        let mut folders = vec![
            folder("zeta", None),
            folder("Trash", Some("Trash")),
            folder("alpha", None),
            folder("Junk", Some("Junk")),
            folder("Sent Items", Some("Sent")),
            folder("Drafts", Some("Drafts")),
            folder("All Mail", Some("Archive")),
            folder("INBOX", None),
        ];
        sort(&mut folders);
        let names: Vec<&str> = folders.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "INBOX",
                "All Mail",
                "Drafts",
                "Sent Items",
                "Junk",
                "Trash",
                "alpha",
                "zeta"
            ]
        );
    }
}
