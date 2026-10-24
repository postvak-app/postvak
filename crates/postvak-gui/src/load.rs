//! Reading the list's rows from the stores. A view change reads them on the window thread, since the user waits for
//! that view anyway; a reload after the daemon reported changes reads them on a thread of its own, so a large mailbox
//! never stalls a frame on every sync event. Both run [`load`]; the app applies its result.

use std::sync::mpsc::{self, Receiver};

use postvak_core::config::Identity;
use postvak_core::paths::Paths;
use postvak_core::search::{self, Scope};
use postvak_core::store::{Filter, Matched, MessageSummary, Store, ThreadSummary};

use super::html::Wake;
use super::list::{self, Row, RowKey, THREAD_LIMIT};

/// A thread by account index and thread id: mail sent to two accounts shares its thread id.
pub(crate) type ThreadKey = (usize, String);

/// What one load reads, copied from the app when it starts.
#[derive(Clone, Debug)]
pub(crate) struct ListQuery {
    pub folder: String,
    pub filter: Filter,
    /// The typed query; a search lists hits instead of threads.
    pub search: Option<String>,
    /// What the servers found for `search`, by account.
    pub server_hits: Vec<(usize, RowKey)>,
    /// The expanded threads.
    pub expanded: Vec<ThreadKey>,
}

/// What a load found.
#[derive(Debug, Default)]
pub(crate) struct Loaded {
    /// The search hits, merged; `None` when the query listed threads.
    pub hits: Option<Vec<Row>>,
    /// With hits: how many messages of the searched stores lack an indexed body, and how many there are.
    pub unindexed: u64,
    pub covered: u64,
    /// Each account's threads, `None` where its store could not list them, so the ones shown stay.
    pub threads: Vec<(usize, Option<Vec<ThreadSummary>>)>,
    /// The members of each expanded thread, `None` where it has no more than one left.
    pub members: Vec<(ThreadKey, Option<Vec<MessageSummary>>)>,
}

/// A store a load reads: the account's index, name, store and, for `from:me`, its identity.
pub(crate) type LoadStore<'a> = (usize, &'a str, &'a Store, Option<&'a Identity>);

/// Reads `query` from `stores`. A search with `account:` reads only that account's store.
pub(crate) fn load(stores: &[LoadStore<'_>], query: &ListQuery) -> Loaded {
    let folder = query.folder.as_str();
    let mut loaded = Loaded::default();
    if let Some(text) = &query.search {
        let now = postvak_core::time::now();
        let parsed = search::parse(text, now);
        let searched: Vec<&LoadStore<'_>> = stores
            .iter()
            .filter(|(_, name, ..)| parsed.account().is_none_or(|named| named == *name))
            .collect();
        let mut hits = Vec::new();
        for &&(account, name, store, identity) in &searched {
            match store.search(
                &parsed,
                Scope::All,
                None,
                list::SEARCH_LIMIT,
                true,
                identity,
            ) {
                Ok(found) => hits.extend(found.iter().map(|hit| {
                    let mut row = Row::from_message(account, &hit.message);
                    row.close = hit.matched == Matched::Close;
                    row.snippet = hit.snippet.clone();
                    row
                })),
                Err(e) => log::warn!("[{name}] search failed: {e}"),
            }
        }
        for (account, (hit_folder, uid)) in &query.server_hits {
            let known = hits
                .iter()
                .any(|row| row.account == *account && row.folder == *hit_folder && row.uid == *uid);
            let store = stores
                .iter()
                .find(|(index, ..)| index == account)
                .map(|(_, _, store, _)| store);
            if let (false, Some(store)) = (known, store)
                && let Ok(Some(message)) = store.message(hit_folder, *uid)
            {
                hits.push(Row::from_message(*account, &message));
            }
        }
        loaded.hits = Some(list::merge_hits(hits, list::SEARCH_LIMIT));
        for &&(_, name, store, _) in &searched {
            match store.backlog_count(now) {
                Ok((unindexed, total)) => {
                    loaded.unindexed += unindexed;
                    loaded.covered += total;
                }
                Err(e) => log::warn!("[{name}] backlog count failed: {e}"),
            }
        }
        return loaded;
    }
    for &(account, name, store, _) in stores {
        match store.thread_summaries(folder, THREAD_LIMIT, query.filter) {
            Ok(threads) => loaded.threads.push((account, Some(threads))),
            Err(e) => {
                log::warn!("[{name}] {folder}: could not load threads: {e}");
                loaded.threads.push((account, None));
            }
        }
    }
    for key in &query.expanded {
        let members = stores
            .iter()
            .find(|(account, ..)| *account == key.0)
            .and_then(|(_, _, store, _)| store.thread_members(folder, &key.1).ok())
            .filter(|members| members.len() > 1);
        loaded.members.push((key.clone(), members));
    }
    loaded
}

/// [`load`] over connections of its own, opened by account name; an account whose store does not open keeps its rows.
pub(crate) fn load_from_paths(
    paths: &Paths,
    accounts: &[LoadAccount],
    query: &ListQuery,
) -> Loaded {
    let mut opened = Vec::new();
    let mut failed = Vec::new();
    for (index, name, identity) in accounts {
        match Store::open(&paths.mail_db(name)) {
            Ok(store) => opened.push((*index, name.as_str(), store, identity.as_ref())),
            Err(e) => {
                log::warn!("[{name}] could not open the store to reload the list: {e}");
                failed.push(*index);
            }
        }
    }
    let stores: Vec<LoadStore<'_>> = opened
        .iter()
        .map(|(index, name, store, identity)| (*index, *name, store, *identity))
        .collect();
    let mut loaded = load(&stores, query);
    if loaded.hits.is_none() {
        loaded
            .threads
            .extend(failed.into_iter().map(|index| (index, None)));
    }
    loaded
}

/// An account a load opens: its index, name and, for `from:me`, its identity.
pub(crate) type LoadAccount = (usize, String, Option<Identity>);

/// How a load opens its own connections and reads; tests swap it to hold a load back.
pub(crate) type LoadFn = fn(&Paths, &[LoadAccount], &ListQuery) -> Loaded;

/// A load running on its own thread, numbered so a result that a later load overtook is dropped.
pub(crate) struct Loading {
    pub generation: u64,
    pub answer: Receiver<Loaded>,
}

/// Starts `load_fn` on a thread of its own; SQLite's write-ahead log lets it read while the daemon writes. The window is
/// woken with the result.
pub(crate) fn spawn(
    load_fn: LoadFn,
    paths: Paths,
    accounts: Vec<LoadAccount>,
    query: ListQuery,
    wake: Wake,
) -> Receiver<Loaded> {
    let (done, answer) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = done.send(load_fn(&paths, &accounts, &query));
        wake();
    });
    answer
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Fixture, message};

    fn inbox() -> ListQuery {
        ListQuery {
            folder: "INBOX".into(),
            filter: Filter::default(),
            search: None,
            server_hits: Vec::new(),
            expanded: Vec::new(),
        }
    }

    #[test]
    fn an_account_whose_store_does_not_open_lists_nothing_so_its_rows_stay() {
        let fx = Fixture::new(&["work"]);
        fx.add("work", message("INBOX", 1, "hello"));
        // A directory where the database should be: SQLite cannot open it.
        std::fs::create_dir_all(fx.paths.mail_db("gone")).unwrap();
        let accounts = [(0, "work".to_string(), None), (1, "gone".to_string(), None)];
        let loaded = load_from_paths(&fx.paths, &accounts, &inbox());
        assert!(loaded.hits.is_none());
        assert_eq!(loaded.threads.len(), 2);
        assert_eq!(loaded.threads[0].1.as_ref().map(Vec::len), Some(1));
        assert_eq!(loaded.threads[1], (1, None));
    }

    #[test]
    fn a_search_lists_hits_instead_of_threads() {
        let fx = Fixture::new(&["work"]);
        fx.add("work", message("INBOX", 1, "invoice march"));
        fx.add("work", message("INBOX", 2, "lunch"));
        let query = ListQuery {
            search: Some("invoice".into()),
            ..inbox()
        };
        let loaded = load_from_paths(&fx.paths, &[(0, "work".to_string(), None)], &query);
        let uids: Vec<u32> = loaded.hits.unwrap().iter().map(|row| row.uid).collect();
        assert_eq!(uids, [1]);
        assert!(loaded.threads.is_empty());
    }
}
