//! Pages of `list` and `search` over several accounts: one newest-first order across them, the cursor that
//! continues it, and the counts. The CLI and the MCP server share it, so a cursor from one works in the other.
//!
//! The order is the store's, with the account between the date and the uid: `(internaldate, account, uid, folder)`
//! descending for messages, `(latest, account, highest uid)` for threads, and a search puts every exact match before
//! every close one. Each part is in the cursor, so a page starts after the last row it ended on, even among rows that
//! share a timestamp, and rows that arrive meanwhile never shift it.
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};

use crate::config::Identity;
use crate::search::{Query, Scope};
use crate::store::{After, Filter, Hit, Matched, Message, SearchRequest, Store, StoreError};

/// A position with this uid comes before every message of its date: every uid is below it.
const ABOVE_EVERY_UID: i64 = 1 << 32;
/// A position with this uid comes after every message of its date.
const BELOW_EVERY_UID: i64 = -1;

/// Why a server search takes no cursor; both front ends refuse it with this text.
pub const SERVER_TAKES_NO_CURSOR: &str =
    "a server search returns one page and takes no cursor; leave out server to page";

#[derive(Debug, thiserror::Error)]
pub enum PageError {
    #[error("the cursor is not one this command returned; start again without it")]
    Cursor,
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// What a listing pages through; a cursor works only for the kind that made it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Kind {
    List,
    Threads,
    Search,
}

/// The last row of a page, as the cursor carries it. For a thread `date` is its latest message's and `uid` the
/// highest of its members'.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Cursor {
    #[serde(rename = "k")]
    kind: Kind,
    #[serde(rename = "t", default, skip_serializing_if = "Option::is_none")]
    tier: Option<Matched>,
    #[serde(rename = "d")]
    date: i64,
    #[serde(rename = "a")]
    account: String,
    #[serde(rename = "u")]
    uid: u32,
    #[serde(rename = "f")]
    folder: String,
}

impl Cursor {
    fn encode(&self) -> String {
        // A struct of strings and numbers always serialises.
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(self).unwrap_or_default())
    }

    fn decode(text: &str, kind: Kind) -> Result<Cursor, PageError> {
        let bytes = URL_SAFE_NO_PAD
            .decode(text.trim())
            .map_err(|_| PageError::Cursor)?;
        let cursor: Cursor = serde_json::from_slice(&bytes).map_err(|_| PageError::Cursor)?;
        let tier_fits = (kind == Kind::Search) == cursor.tier.is_some();
        if cursor.kind != kind || !tier_fits {
            return Err(PageError::Cursor);
        }
        Ok(cursor)
    }

    /// Where `account`'s rows continue: after the cursor's row in its own account; in an account that sorts below
    /// the cursor's, from the cursor's date on; in one above it, after that date.
    fn after(&self, account: &str) -> After {
        let uid = match account.cmp(&self.account) {
            std::cmp::Ordering::Equal => i64::from(self.uid),
            std::cmp::Ordering::Less => ABOVE_EVERY_UID,
            std::cmp::Ordering::Greater => BELOW_EVERY_UID,
        };
        After {
            internaldate: self.date,
            uid,
            folder: self.folder.clone(),
        }
    }
}

/// One account to page through: its name, its store, and for a search its identity, which `from:me` reads.
pub struct Source {
    pub account: String,
    pub store: Store,
    pub identity: Option<Identity>,
}

/// One page and, when more rows follow, the cursor for the next.
#[derive(Debug, Clone, PartialEq)]
pub struct Page<T> {
    pub rows: Vec<T>,
    pub next: Option<String>,
}

/// A row of `list`: a message, and in a thread view its depth.
#[derive(Debug, Clone, PartialEq)]
pub struct ListRow {
    pub account: String,
    pub message: Message,
    pub depth: Option<usize>,
}

/// A row of `search`.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchRow {
    pub account: String,
    pub hit: Hit,
}

/// What `list` shows, the same on every page.
#[derive(Debug, Clone, Copy)]
pub struct ListRequest<'a> {
    pub folder: &'a str,
    pub filter: Filter,
    /// Whole threads, the most recently active first; the limit counts threads.
    pub threads: bool,
}

/// What `search` looks for, the same on every page.
#[derive(Debug, Clone, Copy)]
pub struct SearchQuery<'a> {
    pub query: &'a Query,
    pub scope: Scope,
    pub folder: Option<&'a str>,
    pub close: bool,
}

/// Keeps the first `limit` of `items`, sorted by `key` descending, and the cursor of the last kept when more follow.
fn cut<T, K: Ord>(
    mut items: Vec<T>,
    limit: u32,
    key: impl Fn(&T) -> K,
    cursor: impl Fn(&T) -> Cursor,
) -> (Vec<T>, Option<String>) {
    items.sort_by_key(|item| std::cmp::Reverse(key(item)));
    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    let more = items.len() > limit;
    items.truncate(limit);
    let next = if more {
        items.last().map(|last| cursor(last).encode())
    } else {
        None
    };
    (items, next)
}

/// A page of `list` over `sources`: at most `limit` messages, or with `threads` at most `limit` threads with
/// every member, after `cursor` when given.
pub fn list(
    sources: &[Source],
    request: &ListRequest,
    limit: u32,
    cursor: Option<&str>,
) -> Result<Page<ListRow>, PageError> {
    let kind = if request.threads {
        Kind::Threads
    } else {
        Kind::List
    };
    let cursor = cursor.map(|c| Cursor::decode(c, kind)).transpose()?;
    if limit == 0 {
        return Ok(Page {
            rows: Vec::new(),
            next: None,
        });
    }
    let fetch = limit.saturating_add(1);
    if request.threads {
        list_threads(sources, request, limit, fetch, cursor.as_ref())
    } else {
        let mut found = Vec::new();
        for source in sources {
            let after = cursor.as_ref().map(|c| c.after(&source.account));
            for message in source.store.messages_after(
                request.folder,
                fetch,
                request.filter,
                after.as_ref(),
            )? {
                found.push(ListRow {
                    account: source.account.clone(),
                    message,
                    depth: None,
                });
            }
        }
        let (rows, next) = cut(
            found,
            limit,
            |row| {
                (
                    row.message.internaldate,
                    row.account.clone(),
                    row.message.uid,
                    row.message.folder.clone(),
                )
            },
            |row| Cursor {
                kind: Kind::List,
                tier: None,
                date: row.message.internaldate,
                account: row.account.clone(),
                uid: row.message.uid,
                folder: row.message.folder.clone(),
            },
        );
        Ok(Page { rows, next })
    }
}

/// A thread with where it sorts: its latest date and its highest uid.
struct Thread {
    account: String,
    latest: i64,
    top_uid: u32,
    members: Vec<Message>,
}

fn list_threads(
    sources: &[Source],
    request: &ListRequest,
    limit: u32,
    fetch: u32,
    cursor: Option<&Cursor>,
) -> Result<Page<ListRow>, PageError> {
    let mut found = Vec::new();
    for source in sources {
        let after = cursor.map(|c| c.after(&source.account));
        for members in
            source
                .store
                .threads_after(request.folder, fetch, request.filter, after.as_ref())?
        {
            found.push(Thread {
                account: source.account.clone(),
                latest: members.iter().map(|m| m.internaldate).max().unwrap_or(0),
                top_uid: members.iter().map(|m| m.uid).max().unwrap_or(0),
                members,
            });
        }
    }
    let (threads, next) = cut(
        found,
        limit,
        |t| (t.latest, t.account.clone(), t.top_uid),
        |t| Cursor {
            kind: Kind::Threads,
            tier: None,
            date: t.latest,
            account: t.account.clone(),
            uid: t.top_uid,
            folder: request.folder.to_string(),
        },
    );
    let mut rows = Vec::new();
    for thread in threads {
        let depths = crate::output::depths(&thread.members);
        for (message, depth) in thread.members.into_iter().zip(depths) {
            rows.push(ListRow {
                account: thread.account.clone(),
                message,
                depth: Some(depth),
            });
        }
    }
    Ok(Page { rows, next })
}

/// How many rows `list` over `sources` has in all: messages, or with `threads` threads.
pub fn list_count(sources: &[Source], request: &ListRequest) -> Result<u64, PageError> {
    let mut count = 0;
    for source in sources {
        count += if request.threads {
            source.store.count_threads(request.folder, request.filter)?
        } else {
            source
                .store
                .count_messages(request.folder, request.filter)?
        };
    }
    Ok(count)
}

fn tier_rank(matched: Matched) -> u8 {
    match matched {
        Matched::Exact => 1,
        Matched::Close => 0,
    }
}

fn search_key(row: &SearchRow) -> (u8, i64, String, u32, String) {
    (
        tier_rank(row.hit.matched),
        row.hit.message.internaldate,
        row.account.clone(),
        row.hit.message.uid,
        row.hit.message.folder.clone(),
    )
}

fn search_cursor(row: &SearchRow) -> Cursor {
    Cursor {
        kind: Kind::Search,
        tier: Some(row.hit.matched),
        date: row.hit.message.internaldate,
        account: row.account.clone(),
        uid: row.hit.message.uid,
        folder: row.hit.message.folder.clone(),
    }
}

fn request<'a>(query: &SearchQuery<'a>, source: &'a Source) -> SearchRequest<'a> {
    SearchRequest {
        query: query.query,
        scope: query.scope,
        folder: query.folder,
        close: query.close,
        identity: source.identity.as_ref(),
    }
}

/// A page of `search` over `sources`: the exact matches of every account newest first, then the close ones, at most
/// `limit`, after `cursor` when given. Each account decides on its own whether its close matches show.
pub fn search(
    sources: &[Source],
    query: &SearchQuery,
    limit: u32,
    cursor: Option<&str>,
) -> Result<Page<SearchRow>, PageError> {
    let cursor = cursor
        .map(|c| Cursor::decode(c, Kind::Search))
        .transpose()?;
    if limit == 0 {
        return Ok(Page {
            rows: Vec::new(),
            next: None,
        });
    }
    let mut found = Vec::new();
    for source in sources {
        let after = cursor
            .as_ref()
            .map(|c| (c.tier.unwrap_or(Matched::Exact), c.after(&source.account)));
        let hits = source.store.search_after(
            &request(query, source),
            limit.saturating_add(1),
            after.as_ref().map(|(tier, position)| (*tier, position)),
        )?;
        found.extend(hits.into_iter().map(|hit| SearchRow {
            account: source.account.clone(),
            hit,
        }));
    }
    let (rows, next) = cut(found, limit, search_key, search_cursor);
    Ok(Page { rows, next })
}

/// How many messages `search` over `sources` finds in all, close matches included where they would show.
pub fn search_count(sources: &[Source], query: &SearchQuery) -> Result<u64, PageError> {
    let mut count = 0;
    for source in sources {
        count += source.store.search_count(&request(query, source))?;
    }
    Ok(count)
}

/// Adds the messages the mail server found in `account` to a first page, in order, at most `limit`; the page then
/// has no next, since a later page could not tell which server hits it already showed.
pub fn with_server_hits(
    page: Page<SearchRow>,
    account: &str,
    store: &Store,
    server: &[(String, u32)],
    limit: u32,
) -> Result<Page<SearchRow>, PageError> {
    let (own, mut rows): (Vec<SearchRow>, Vec<SearchRow>) = page
        .rows
        .into_iter()
        .partition(|row| row.account == account);
    let own = crate::actions::with_server_hits(
        store,
        own.into_iter().map(|row| row.hit).collect(),
        server,
        limit,
    )?;
    rows.extend(own.into_iter().map(|hit| SearchRow {
        account: account.to_string(),
        hit,
    }));
    let (rows, _) = cut(rows, limit, search_key, search_cursor);
    Ok(Page { rows, next: None })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Folder;

    fn source(account: &str, messages: &[(&str, u32, i64, &str)]) -> Source {
        let store = Store::open_in_memory().unwrap();
        for folder in ["INBOX", "Archive"] {
            store
                .upsert_folder(&Folder {
                    name: folder.into(),
                    uidvalidity: 1,
                    last_uid: 0,
                    special_use: None,
                    delimiter: None,
                })
                .unwrap();
        }
        for (folder, uid, date, subject) in messages {
            store
                .insert_message(&Message {
                    folder: (*folder).into(),
                    uid: *uid,
                    thread_id: format!("{folder}{uid}"),
                    subject: Some((*subject).into()),
                    internaldate: *date,
                    flags: String::new(),
                    ..Default::default()
                })
                .unwrap();
        }
        Source {
            account: account.into(),
            store,
            identity: None,
        }
    }

    const INBOX: ListRequest = ListRequest {
        folder: "INBOX",
        filter: Filter {
            unread: false,
            flagged: false,
            attachments: false,
        },
        threads: false,
    };

    /// Every row of `list`, a page of `limit` at a time, as `account/uid`.
    fn every_page(sources: &[Source], request: &ListRequest, limit: u32) -> Vec<Vec<String>> {
        let mut pages = Vec::new();
        let mut cursor = None;
        loop {
            let page = list(sources, request, limit, cursor.as_deref()).unwrap();
            pages.push(
                page.rows
                    .iter()
                    .map(|r| format!("{}/{}", r.account, r.message.uid))
                    .collect(),
            );
            match page.next {
                Some(next) => cursor = Some(next),
                None => return pages,
            }
            assert!(pages.len() < 50, "paging must end");
        }
    }

    #[test]
    fn pages_of_equal_timestamps_neither_skip_nor_repeat() {
        let same: Vec<(&str, u32, i64, &str)> =
            (1..=7).map(|uid| ("INBOX", uid, 100, "x")).collect();
        let sources = [source("work", &same), source("home", &same)];
        let pages = every_page(&sources, &INBOX, 3);
        let all: Vec<String> = pages.concat();
        assert_eq!(all.len(), 14);
        let mut unique = all.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), 14, "no row twice: {all:?}");
        assert_eq!(&all[..3], ["work/7", "work/6", "work/5"]);
        assert_eq!(pages.len(), 5);
        assert!(pages.last().is_some_and(|p| p.len() == 2));
    }

    #[test]
    fn accounts_merge_newest_first_and_the_last_page_has_no_next() {
        let sources = [
            source("work", &[("INBOX", 1, 10, "a"), ("INBOX", 2, 30, "b")]),
            source("home", &[("INBOX", 1, 20, "c")]),
        ];
        assert_eq!(
            every_page(&sources, &INBOX, 2),
            [vec!["work/2", "home/1"], vec!["work/1"]]
        );
        assert_eq!(
            every_page(&sources, &INBOX, 3),
            [vec!["work/2", "home/1", "work/1"]],
            "a page holding the rest has no next"
        );
        assert_eq!(list_count(&sources, &INBOX).unwrap(), 3);
    }

    #[test]
    fn a_message_arriving_between_pages_does_not_shift_the_next() {
        let sources = [source(
            "work",
            &[
                ("INBOX", 1, 10, "a"),
                ("INBOX", 2, 20, "b"),
                ("INBOX", 3, 30, "c"),
            ],
        )];
        let first = list(&sources, &INBOX, 2, None).unwrap();
        sources[0]
            .store
            .insert_message(&Message {
                folder: "INBOX".into(),
                uid: 4,
                thread_id: "n".into(),
                internaldate: 40,
                ..Default::default()
            })
            .unwrap();
        let second = list(&sources, &INBOX, 2, first.next.as_deref()).unwrap();
        let uids: Vec<u32> = second.rows.iter().map(|r| r.message.uid).collect();
        assert_eq!(uids, [1]);
        assert_eq!(second.next, None);
    }

    #[test]
    fn threads_page_by_thread_with_every_member() {
        let sources = [source(
            "work",
            &[
                ("INBOX", 1, 10, "a"),
                ("INBOX", 2, 20, "b"),
                ("INBOX", 3, 20, "c"),
            ],
        )];
        sources[0]
            .store
            .insert_message(&Message {
                folder: "INBOX".into(),
                uid: 4,
                thread_id: "INBOX1".into(),
                internaldate: 5,
                ..Default::default()
            })
            .unwrap();
        let threads = ListRequest {
            threads: true,
            ..INBOX
        };
        assert_eq!(
            every_page(&sources, &threads, 1),
            [vec!["work/3"], vec!["work/2"], vec!["work/4", "work/1"],]
        );
        assert_eq!(list_count(&sources, &threads).unwrap(), 3);
    }

    #[test]
    fn a_cursor_works_only_where_it_was_made() {
        let sources = [source(
            "work",
            &[("INBOX", 1, 10, "a"), ("INBOX", 2, 20, "b")],
        )];
        let next = list(&sources, &INBOX, 1, None).unwrap().next.unwrap();
        let threads = ListRequest {
            threads: true,
            ..INBOX
        };
        assert!(matches!(
            list(&sources, &threads, 1, Some(&next)),
            Err(PageError::Cursor)
        ));
        let query = crate::search::parse("a", 0);
        let search_query = SearchQuery {
            query: &query,
            scope: Scope::All,
            folder: None,
            close: false,
        };
        assert!(matches!(
            search(&sources, &search_query, 1, Some(&next)),
            Err(PageError::Cursor)
        ));
        for garbage in ["", "not a cursor", "e30"] {
            assert!(matches!(
                list(&sources, &INBOX, 1, Some(garbage)),
                Err(PageError::Cursor)
            ));
        }
    }

    /// Every hit of `text`, `limit` at a time, as `account/folder/uid` with `~` before a close match.
    fn search_pages(sources: &[Source], text: &str, limit: u32) -> Vec<String> {
        let query = crate::search::parse(text, 0);
        let query = SearchQuery {
            query: &query,
            scope: Scope::All,
            folder: None,
            close: true,
        };
        let mut all = Vec::new();
        let mut cursor = None;
        loop {
            let page = search(sources, &query, limit, cursor.as_deref()).unwrap();
            assert!(page.rows.len() <= limit as usize);
            all.extend(page.rows.iter().map(|r| {
                let close = if r.hit.matched == Matched::Close {
                    "~"
                } else {
                    ""
                };
                format!(
                    "{close}{}/{}/{}",
                    r.account, r.hit.message.folder, r.hit.message.uid
                )
            }));
            match page.next {
                Some(next) => cursor = Some(next),
                None => return all,
            }
            assert!(all.len() < 100, "paging must end");
        }
    }

    #[test]
    fn search_pages_through_both_tiers_and_folders_with_one_uid() {
        let work = source(
            "work",
            &[
                ("INBOX", 1, 50, "invioce one"),
                ("Archive", 1, 50, "invioce two"),
                ("INBOX", 2, 40, "invoice three"),
                ("INBOX", 3, 60, "unrelated"),
            ],
        );
        let home = source(
            "home",
            &[
                ("INBOX", 1, 50, "invioce four"),
                ("INBOX", 2, 70, "invoice five"),
            ],
        );
        let sources = [work, home];
        let whole = search_pages(&sources, "invioce", 100);
        assert_eq!(
            whole,
            [
                "work/INBOX/1",
                "work/Archive/1",
                "home/INBOX/1",
                "~home/INBOX/2",
                "~work/INBOX/2"
            ]
        );
        for limit in 1..=4 {
            assert_eq!(
                search_pages(&sources, "invioce", limit),
                whole,
                "limit {limit}"
            );
        }
        let query = crate::search::parse("invioce", 0);
        let query = SearchQuery {
            query: &query,
            scope: Scope::All,
            folder: None,
            close: true,
        };
        assert_eq!(search_count(&sources, &query).unwrap(), 5);
        let exact = SearchQuery {
            close: false,
            ..query
        };
        assert_eq!(search_count(&sources, &exact).unwrap(), 3);
    }

    #[test]
    fn close_matches_stay_away_once_the_exact_tier_holds_ten() {
        let mut messages: Vec<(&str, u32, i64, &str)> = (1..=10)
            .map(|uid| ("INBOX", uid, i64::from(uid), "invioce"))
            .collect();
        messages.push(("INBOX", 11, 100, "invoice"));
        let sources = [source("work", &messages)];
        let found = search_pages(&sources, "invioce", 3);
        assert_eq!(found.len(), 10);
        assert!(found.iter().all(|row| !row.starts_with('~')));
        let query = crate::search::parse("invioce", 0);
        let query = SearchQuery {
            query: &query,
            scope: Scope::All,
            folder: None,
            close: true,
        };
        assert_eq!(search_count(&sources, &query).unwrap(), 10);
    }

    #[test]
    fn server_hits_join_the_first_page_and_end_paging() {
        let sources = [source(
            "work",
            &[
                ("INBOX", 1, 10, "invoice"),
                ("INBOX", 2, 20, "other"),
                ("INBOX", 3, 5, "invoice"),
            ],
        )];
        let query = crate::search::parse("invoice", 0);
        let query = SearchQuery {
            query: &query,
            scope: Scope::All,
            folder: None,
            close: false,
        };
        let page = search(&sources, &query, 1, None).unwrap();
        assert!(page.next.is_some());
        let merged = with_server_hits(
            page,
            "work",
            &sources[0].store,
            &[("INBOX".to_string(), 2)],
            2,
        )
        .unwrap();
        let uids: Vec<u32> = merged.rows.iter().map(|r| r.hit.message.uid).collect();
        assert_eq!(uids, [2, 1]);
        assert_eq!(merged.next, None);
    }
}
