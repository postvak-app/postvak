//! The list's state and behaviour, copied from `src/gui/app.rs` without egui: rows from the store through
//! `list::build_rows` and `apply_pending`, the cursor, marks, threads, and actions sent through the daemon client.
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::mpsc::{Receiver, Sender};

use postvak::daemon::Client;
use postvak::gui::list::{self, Optimistic, Row, RowKey};
use postvak::rules::Action;
use postvak::store::{MessageSummary, Store, ThreadSummary};
use postvak::sync::{Command, Event};

pub struct Account {
    pub name: String,
    pub store: Store,
    pub pending: HashMap<RowKey, Vec<Optimistic>>,
    pub archive: Option<String>,
}

pub struct Model {
    pub accounts: Vec<Account>,
    /// The accounts the list shows, by index; more than one is All inboxes.
    pub shown: Vec<usize>,
    pub folder: String,
    pub threads: Vec<(usize, ThreadSummary)>,
    pub expanded: HashMap<(usize, String), Vec<MessageSummary>>,
    pub rows: Vec<Row>,
    pub cursor: usize,
    pub marked: HashSet<(usize, RowKey)>,
    pub help: bool,
    client: Client,
    /// The daemon's ends of the in-memory client; held so sends succeed. `commands` is what the prototype sent. No
    /// daemon runs in the prototype, so only tests read them.
    #[cfg_attr(not(test), allow(dead_code))]
    pub commands: Receiver<(String, Command)>,
    #[allow(dead_code)]
    pub events: Sender<Event>,
}

impl Model {
    /// Every account in `home`, showing `shown`'s INBOX.
    pub fn open(home: &postvak::gui::fixture::Home, shown: Vec<usize>) -> anyhow::Result<Model> {
        let names: Vec<&str> = home.accounts.iter().map(String::as_str).collect();
        let (client, commands, events) = Client::in_memory(&names);
        let mut accounts = Vec::new();
        for name in &home.accounts {
            let store = home.store(name)?;
            let archive = store
                .folders()?
                .into_iter()
                .find(|f| f.special_use.as_deref() == Some("Archive"))
                .map(|f| f.name);
            accounts.push(Account {
                name: name.clone(),
                store,
                pending: HashMap::new(),
                archive,
            });
        }
        let mut model = Model {
            accounts,
            shown,
            folder: list::INBOX.into(),
            threads: Vec::new(),
            expanded: HashMap::new(),
            rows: Vec::new(),
            cursor: 0,
            marked: HashSet::new(),
            help: false,
            client,
            commands,
            events,
        };
        model.reload();
        Ok(model)
    }

    /// `App::reload_view` for a folder view: threads from each shown store, expanded threads reloaded.
    pub fn reload(&mut self) {
        let mut found = Vec::new();
        for &account in &self.shown {
            match self.accounts[account]
                .store
                .thread_summaries(&self.folder, list::THREAD_LIMIT)
            {
                Ok(threads) => found.push((account, threads)),
                Err(e) => eprintln!(
                    "[{}] could not load threads: {e}",
                    self.accounts[account].name
                ),
            }
        }
        self.threads = list::merge_threads(found, list::THREAD_LIMIT);
        let keys: Vec<(usize, String)> = self.expanded.keys().cloned().collect();
        for key in keys {
            match self.accounts[key.0]
                .store
                .thread_members(&self.folder, &key.1)
            {
                Ok(members) if members.len() > 1 => {
                    self.expanded.insert(key, members);
                }
                _ => {
                    self.expanded.remove(&key);
                }
            }
        }
        self.rebuild_rows();
    }

    /// `App::rebuild_rows`: the cursor stays on its row when that row is still there.
    pub fn rebuild_rows(&mut self) {
        let current = self.rows.get(self.cursor).cloned();
        let mut rows = list::build_rows(&self.folder, &self.threads, &self.expanded);
        let pending: Vec<_> = self.accounts.iter().map(|a| &a.pending).collect();
        list::apply_pending(&mut rows, &pending);
        let kept = current.and_then(|cursor| rows.iter().position(|row| same_row(row, &cursor)));
        self.rows = rows;
        self.cursor = kept.unwrap_or_else(|| self.cursor.min(self.rows.len().saturating_sub(1)));
    }

    pub fn step(&mut self, delta: isize) {
        self.cursor = self
            .cursor
            .saturating_add_signed(delta)
            .min(self.rows.len().saturating_sub(1));
    }

    pub fn select(&mut self, index: usize) {
        if index < self.rows.len() {
            self.cursor = index;
        }
    }

    pub fn toggle_mark(&mut self) {
        if let Some(id) = self.rows.get(self.cursor).map(Row::id)
            && !self.marked.remove(&id)
        {
            self.marked.insert(id);
        }
    }

    /// Esc: clears the help, else the marks.
    pub fn escape(&mut self) {
        if self.help {
            self.help = false;
        } else {
            self.marked.clear();
        }
    }

    pub fn expand(&mut self) {
        let Some(row) = self.rows.get(self.cursor) else {
            return;
        };
        let Some(thread) = row.thread_id.clone() else {
            return;
        };
        let key = (row.account, thread);
        if row.count < 2 || self.expanded.contains_key(&key) {
            return;
        }
        if let Ok(members) = self.accounts[key.0]
            .store
            .thread_members(&row.folder, &key.1)
        {
            self.expanded.insert(key, members);
            self.rebuild_rows();
        }
    }

    /// Collapses the thread the cursor is in and puts the cursor on its thread row.
    pub fn collapse(&mut self) {
        if self.rows.is_empty() {
            return;
        }
        let from = self.cursor.min(self.rows.len() - 1);
        let Some(parent) = (0..=from).rev().find(|&i| self.rows[i].thread_id.is_some()) else {
            return;
        };
        let Some(thread) = self.rows[parent].thread_id.clone() else {
            return;
        };
        if self
            .expanded
            .remove(&(self.rows[parent].account, thread))
            .is_some()
        {
            self.cursor = parent;
            self.rebuild_rows();
        }
    }

    fn selected_rows(&self) -> Vec<&Row> {
        if self.marked.is_empty() {
            self.rows.get(self.cursor).into_iter().collect()
        } else {
            self.rows
                .iter()
                .filter(|row| self.marked.contains(&row.id()))
                .collect()
        }
    }

    /// `App::targets`: the marked rows, or the cursor row, with a thread row standing for all its members.
    fn targets(&self) -> Vec<(usize, String, Vec<u32>)> {
        let mut by_folder: BTreeMap<(usize, String), BTreeSet<u32>> = BTreeMap::new();
        for row in self.selected_rows() {
            let uids = by_folder
                .entry((row.account, row.folder.clone()))
                .or_default();
            let members = match &row.thread_id {
                Some(thread) if row.count > 1 => self.accounts[row.account]
                    .store
                    .thread_members(&row.folder, thread)
                    .ok(),
                _ => None,
            };
            match members {
                Some(members) => uids.extend(members.iter().map(|m| m.uid)),
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

    /// `App::act`: sends `action` per account and folder, and shows its effect before the daemon confirms it.
    pub fn act(&mut self, action: Action) {
        let Some(optimistic) = Optimistic::of(&action) else {
            return;
        };
        for (account, folder, uids) in self.targets() {
            let action = match action {
                Action::Archive if self.accounts[account].archive.as_deref() == Some(&folder) => {
                    Action::Move(list::INBOX.into())
                }
                _ => action.clone(),
            };
            let keys: Vec<RowKey> = uids.iter().map(|&uid| (folder.clone(), uid)).collect();
            let command = Command::Apply {
                folder,
                uids,
                action,
                by: "gui".into(),
            };
            if self.client.send(&self.accounts[account].name, command) {
                for key in keys {
                    self.accounts[account]
                        .pending
                        .entry(key)
                        .or_default()
                        .push(optimistic);
                }
            }
        }
        self.marked.clear();
        self.rebuild_rows();
    }
}

/// `app.rs`'s `same_row`.
fn same_row(row: &Row, cursor: &Row) -> bool {
    if row.account != cursor.account || row.member != cursor.member {
        return false;
    }
    match (&row.thread_id, &cursor.thread_id) {
        (Some(row_thread), Some(cursor_thread)) if !row.member => row_thread == cursor_thread,
        _ => row.key() == cursor.key(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn work_inbox() -> (tempfile::TempDir, Model) {
        let dir = tempfile::tempdir().unwrap();
        let home = postvak::gui::fixture::mailbox(dir.path(), "light").unwrap();
        let model = Model::open(&home, vec![1]).unwrap();
        (dir, model)
    }

    #[test]
    fn the_snapshot_inbox_has_one_row_per_thread_newest_first() {
        let (_dir, model) = work_inbox();
        let subjects: Vec<&str> = model.rows.iter().map(|r| r.subject.as_str()).collect();
        assert_eq!(subjects[0], "Lunch on Friday?");
        // 93 and 94 share a thread: five rows for six messages.
        assert_eq!(model.rows.len(), 5);
        assert!(model.rows.iter().any(|r| r.count == 2));
    }

    #[test]
    fn right_expands_a_thread_and_left_returns_to_it() {
        let (_dir, mut model) = work_inbox();
        let thread = model.rows.iter().position(|r| r.count == 2).unwrap();
        model.select(thread);
        model.expand();
        assert_eq!(model.rows.len(), 7);
        assert!(model.rows[thread + 1].member && model.rows[thread + 2].member);
        model.step(2);
        model.collapse();
        assert_eq!((model.rows.len(), model.cursor), (5, thread));
    }

    #[test]
    fn archive_sends_the_whole_thread_and_hides_its_row() {
        let (_dir, mut model) = work_inbox();
        let thread = model.rows.iter().position(|r| r.count == 2).unwrap();
        model.select(thread);
        model.act(Action::Archive);
        let sent: Vec<_> = model.commands.try_iter().collect();
        assert_eq!(sent.len(), 1);
        let (
            account,
            Command::Apply {
                folder,
                uids,
                action,
                ..
            },
        ) = &sent[0]
        else {
            panic!("not an Apply: {sent:?}");
        };
        assert_eq!((account.as_str(), folder.as_str()), ("work", "INBOX"));
        assert_eq!((uids.as_slice(), action), (&[93, 94][..], &Action::Archive));
        assert_eq!(model.rows.len(), 4);
        assert!(model.rows.iter().all(|r| r.count == 1));
    }

    #[test]
    fn marks_choose_the_targets_and_escape_clears_them() {
        let (_dir, mut model) = work_inbox();
        model.toggle_mark();
        model.step(1);
        model.toggle_mark();
        assert_eq!(model.marked.len(), 2);
        model.escape();
        assert!(model.marked.is_empty());
        model.toggle_mark();
        model.step(-1);
        model.act(Action::MarkRead);
        let sent: Vec<_> = model.commands.try_iter().collect();
        let (_, Command::Apply { uids, .. }) = &sent[0] else {
            panic!("not an Apply");
        };
        // The marked row is the offsite thread; the cursor row, 95, is not sent.
        assert_eq!(
            uids.as_slice(),
            &[93, 94],
            "only the marked row, not the cursor"
        );
    }
}
