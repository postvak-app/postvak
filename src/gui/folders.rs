//! The folder pane without a toolkit: the tree of custom folders, the order of special ones, labels and unread counts.
//! The window draws All inboxes, each account's folders and the Rules, Activity and Backups entries from it.

use crate::message::clean;
use crate::store::Folder;

use super::state::{Account, FolderRow};
use super::{list, numbers};

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
    rank(name, special_use) < 6
}

/// INBOX first, then the special-use folders in a fixed order, then the rest alphabetically.
pub(crate) fn sort(folders: &mut [Folder]) {
    folders.sort_by_key(|f| {
        (
            rank(&f.name, f.special_use.as_deref()),
            f.name.to_lowercase(),
        )
    });
}

fn rank(name: &str, special_use: Option<&str>) -> usize {
    if name.eq_ignore_ascii_case("INBOX") {
        return 0;
    }
    match special_use {
        Some("Archive") => 1,
        Some("Drafts") => 2,
        Some("Sent") => 3,
        Some("Junk") => 4,
        Some("Trash") => 5,
        _ => 6,
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::gui::state::{Focus, FolderRow, View};
    use crate::gui::test_support::{Fixture, message};
    use crate::store::Folder;

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

    fn stored(name: &str) -> Folder {
        Folder {
            name: name.into(),
            uidvalidity: 1,
            last_uid: 0,
            special_use: None,
            delimiter: Some("/".into()),
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
