use std::io;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};
use rusqlite_migration::{M, Migrations};
use serde::{Deserialize, Serialize};

use crate::config::Identity;
use crate::paths::Paths;
use crate::search::{self, Candidate, Plan, Query, Scope, Vocab};

/// In order; a new directory under `migrations/` needs its line here too.
fn migration_list() -> Vec<M<'static>> {
    vec![
        M::up(include_str!("../migrations/001-initial/up.sql")),
        M::up(include_str!("../migrations/002-initial-uid-next/up.sql")),
        M::up(include_str!("../migrations/003-notified-uid/up.sql")),
        M::up(include_str!(
            "../migrations/004-drop-message-id-index/up.sql"
        )),
        M::up(include_str!("../migrations/005-folder-delimiter/up.sql")),
        M::up(include_str!("../migrations/006-flag-sync/up.sql")),
        M::up(include_str!("../migrations/007-rule-definition/up.sql")),
        M::up(include_str!("../migrations/008-has-attachment/up.sql")),
        M::up(include_str!("../migrations/009-folder-thread-index/up.sql")),
        M::up(include_str!("../migrations/010-unread-index/up.sql")),
        M::up(include_str!("../migrations/011-search/up.sql")),
        M::up(include_str!("../migrations/012-rule-moves/up.sql")),
        M::up(include_str!("../migrations/013-backlog-index/up.sql")),
        M::up(include_str!("../migrations/014-newest-index/up.sql")),
        M::up(include_str!(
            "../migrations/015-attachment-in-thread-index/up.sql"
        )),
        M::up(include_str!("../migrations/016-folder-uid-next/up.sql")),
        M::up_with_hook(
            include_str!("../migrations/017-decode-folder-names/up.sql"),
            decode_folder_names,
        ),
        M::up(include_str!("../migrations/018-folder-list/up.sql")),
        M::up(include_str!("../migrations/019-noselect-folders/up.sql")),
        M::up(include_str!("../migrations/020-folder-backfill/up.sql")),
        M::up(include_str!(
            "../migrations/021-thread-by-in-reply-to/up.sql"
        )),
        M::up(include_str!("../migrations/022-list-preview/up.sql")),
    ]
}

/// Gives the folder `from` the name `to` in every table that names one. The caller defers the foreign keys.
fn rename_folder_rows(tx: &rusqlite::Connection, from: &str, to: &str) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE folders SET name = ?2 WHERE name = ?1",
        params![from, to],
    )?;
    tx.execute(
        "UPDATE messages SET folder = ?2 WHERE folder = ?1",
        params![from, to],
    )?;
    tx.execute(
        "UPDATE rule_log SET folder = ?2 WHERE folder = ?1",
        params![from, to],
    )?;
    tx.execute(
        "UPDATE OR IGNORE rule_moves SET folder = ?2 WHERE folder = ?1",
        params![from, to],
    )?;
    Ok(())
}

/// Renames the folders stored under their modified UTF-7 wire name (`Caf&AOk-`) to the UTF-8 name `ImapOps` now
/// gives them (`Café`), with their messages and rule history, so they keep their state instead of syncing again. A
/// name that is not exactly what the encoder writes stays: it was sent raw, and the server knows it so. A placeholder
/// a rule move left under the UTF-8 name (it sent the raw name, which the server refused) gives way. A synced folder
/// under the UTF-8 name, which only a server listing both spellings has, stays; the encoded row then goes with its
/// messages and move records, its rule history keeps the wire name, and the next sync fetches what the server still
/// lists.
fn decode_folder_names(tx: &rusqlite::Transaction) -> rusqlite_migration::HookResult {
    let names: Vec<String> = tx
        .prepare("SELECT name FROM folders")?
        .query_map([], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    // The messages follow their folder's new name before the commit checks their reference to it.
    tx.pragma_update(None, "defer_foreign_keys", "ON")?;
    for wire in names {
        let Some(name) = crate::mail_ops::utf7::decode(&wire).filter(|name| *name != wire) else {
            continue;
        };
        tx.execute(
            "DELETE FROM folders WHERE name = ?1 AND uidvalidity = 0",
            params![name],
        )?;
        let taken: bool = tx.query_row(
            "SELECT EXISTS (SELECT 1 FROM folders WHERE name = ?1)",
            params![name],
            |row| row.get(0),
        )?;
        if taken {
            // The history stays under the wire name, the encoded folder's own: the listing keeps that folder apart
            // from the raw one. Its move records go with its messages.
            tx.execute("DELETE FROM messages WHERE folder = ?1", params![wire])?;
            tx.execute("DELETE FROM folders WHERE name = ?1", params![wire])?;
        } else {
            rename_folder_rows(tx, &wire, &name)?;
        }
        tx.execute("DELETE FROM rule_moves WHERE folder = ?1", params![wire])?;
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("database migration: {0}")]
    Migration(#[from] rusqlite_migration::Error),
    #[error("database row: {0}")]
    Serde(#[from] serde_rusqlite::Error),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Folder {
    pub name: String,
    pub uidvalidity: u32,
    pub last_uid: u32,
    pub special_use: Option<String>,
    #[serde(default)]
    pub delimiter: Option<String>,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub folder: String,
    pub uid: u32,
    pub message_id: Option<String>,
    pub from_addr: Option<String>,
    pub to_addr: Option<String>,
    pub cc_addr: Option<String>,
    pub delivered_to: Option<String>,
    pub in_reply_to: Option<String>,
    pub refs: Option<String>,
    pub thread_id: String,
    pub subject: Option<String>,
    pub date: Option<i64>,
    pub internaldate: i64,
    pub flags: String,
    pub size: Option<u32>,
    #[serde(skip_serializing, deserialize_with = "deserialize_blob")]
    pub headers: Vec<u8>,
    pub body_text: Option<String>,
    /// Attachment file names, one per line, once the indexer has read the message's structure.
    #[serde(skip_serializing, default)]
    pub attachment_names: Option<String>,
    /// When the indexer last tried this message; None until it has.
    #[serde(skip_serializing, default)]
    pub indexed_at: Option<i64>,
}

/// One search result: the message and how it matched.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hit {
    #[serde(flatten)]
    pub message: Message,
    #[serde(rename = "match")]
    pub matched: Matched,
    /// Context around a body match, once snippets exist; never for a headers-only search.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
}

/// Whether the query matched as typed, or through a close match such as a corrected typo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Matched {
    Exact,
    Close,
}

/// Whether the space-separated `flags` contain `flag`. Flags compare
/// case-insensitively (RFC 3501), so every check and edit agrees with matching.
pub fn has_flag(flags: &str, flag: &str) -> bool {
    flags
        .split_whitespace()
        .any(|f| f.eq_ignore_ascii_case(flag))
}

impl Message {
    pub fn is_seen(&self) -> bool {
        has_flag(&self.flags, "\\Seen")
    }

    pub fn is_flagged(&self) -> bool {
        has_flag(&self.flags, "\\Flagged")
    }

    /// How deep in its thread the message sits, judged from its own reply headers.
    pub fn thread_depth(&self) -> usize {
        let refs = self
            .refs
            .as_deref()
            .map_or(0, |r| r.split_whitespace().count());
        refs.max(usize::from(self.in_reply_to.is_some()))
    }
}

/// How much of the body text a summary's `preview` holds: the generated column's length (migration 022).
pub const PREVIEW_CHARS: usize = 160;

/// What a list row shows; no headers or body beyond the body's first `PREVIEW_CHARS` characters.
#[derive(Debug, Clone, PartialEq)]
pub struct MessageSummary {
    pub uid: u32,
    pub from: String,
    pub to: String,
    pub subject: String,
    pub date: i64,
    pub flags: String,
    /// The headers' guess that it carries an attachment, as the attachments filter makes it.
    pub attachment: bool,
    /// The start of the body text, untouched; empty until the body is indexed.
    pub preview: String,
}

impl MessageSummary {
    pub fn is_seen(&self) -> bool {
        has_flag(&self.flags, "\\Seen")
    }

    pub fn is_flagged(&self) -> bool {
        has_flag(&self.flags, "\\Flagged")
    }
}

/// A collapsed thread row: its latest message plus counts over all members in the folder.
#[derive(Debug, Clone, PartialEq)]
pub struct ThreadSummary {
    pub thread_id: String,
    pub latest: MessageSummary,
    pub count: u32,
    pub unread: bool,
    pub flagged: bool,
}

fn summary_from_row(row: &rusqlite::Row, offset: usize) -> rusqlite::Result<MessageSummary> {
    Ok(MessageSummary {
        uid: row.get(offset)?,
        from: row.get(offset + 1)?,
        to: row.get(offset + 2)?,
        subject: row.get(offset + 3)?,
        date: row.get(offset + 4)?,
        flags: row.get(offset + 5)?,
        attachment: row.get(offset + 6)?,
        preview: row.get(offset + 7)?,
    })
}

/// SQL conditions on a message row's space-separated `flags`.
pub(crate) const UNSEEN: &str = r"instr(' ' || flags || ' ', ' \Seen ') = 0";
pub(crate) const FLAGGED: &str = r"instr(' ' || flags || ' ', ' \Flagged ') > 0";

/// A folder's unread count, from the index of unread messages alone (migration 010). Without statistics SQLite would
/// pick the covering folder index and read the whole folder; a condition other than the index's makes this an error.
fn unread_count_sql() -> String {
    format!(
        "SELECT COUNT(*) FROM messages INDEXED BY messages_unread WHERE folder = ?1 AND {UNSEEN}"
    )
}

/// Which messages a list keeps; each set field must hold, and the default keeps all.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Filter {
    pub unread: bool,
    pub flagged: bool,
    /// Guessed from the headers: a top-level multipart/mixed.
    pub attachments: bool,
}

impl Filter {
    /// The filter as one SQL condition on a message row; `1` when it keeps all.
    fn sql(self) -> String {
        let conditions = [
            (self.unread, UNSEEN),
            (self.flagged, FLAGGED),
            (self.attachments, "has_attachment"),
        ];
        let set: Vec<&str> = conditions
            .iter()
            .filter(|(on, _)| *on)
            .map(|(_, sql)| *sql)
            .collect();
        if set.is_empty() {
            "1".into()
        } else {
            format!("({})", set.join(" AND "))
        }
    }
}

/// Where a newest-first page starts: the rows after this position, those whose `(internaldate, uid, folder)` sorts
/// below it. `uid` is wider than a uid so a position can sit before or after every uid of one `internaldate`; a list
/// of one folder compares `internaldate` and `uid` alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct After {
    pub internaldate: i64,
    pub uid: i64,
    pub folder: String,
}

/// Where a page of one search continues in one account: after a position among the exact matches or the close ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resume<'a> {
    Exact(&'a After),
    /// `shown` when an earlier page showed this account's close matches: they keep showing even when exact matches
    /// arrived since. Otherwise they show only while the exact tier is thin.
    Close {
        after: &'a After,
        shown: bool,
    },
}

const SUMMARY_COLUMNS: &str = "uid, COALESCE(from_addr, ''), COALESCE(to_addr, ''), COALESCE(subject, ''), internaldate, flags, has_attachment, COALESCE(preview, '')";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogEntry {
    #[serde(default)]
    pub id: i64,
    pub at: i64,
    pub rule_name: String,
    pub folder: String,
    pub uid: u32,
    pub message_id: Option<String>,
    pub subject: Option<String>,
    pub action: String,
    pub trash_file: Option<String>,
}

/// serde's `Vec<u8>` only accepts sequences; rusqlite offers BLOBs as bytes.
fn deserialize_blob<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
    struct BlobVisitor;
    impl<'de> serde::de::Visitor<'de> for BlobVisitor {
        type Value = Vec<u8>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("bytes")
        }
        fn visit_bytes<E: serde::de::Error>(self, v: &[u8]) -> Result<Vec<u8>, E> {
            Ok(v.to_vec())
        }
        fn visit_byte_buf<E: serde::de::Error>(self, v: Vec<u8>) -> Result<Vec<u8>, E> {
            Ok(v)
        }
    }
    d.deserialize_byte_buf(BlobVisitor)
}

pub struct Store {
    conn: Connection,
}

/// Close matches run when the exact tier returns fewer hits than this.
const CLOSE_MATCHES_BELOW: usize = 10;

/// The messages the indexer never tried, spelled as the partial index `messages_unindexed` (migration 014) so a
/// count and the backlog read it.
const NEVER: &str = "indexed_at IS NULL";
/// The messages the indexer tried without finding text more than a day before `?1`, spelled as the partial index
/// `messages_retry` (migration 014) so a count and the backlog read it. Together with `NEVER` these are the backlog.
const RETRY: &str = "body_text IS NULL AND indexed_at IS NOT NULL AND indexed_at < ?1 - 86400";

const MESSAGE_COLUMNS: &str = "folder, uid, message_id, from_addr, to_addr, cc_addr, delivered_to, in_reply_to, refs, thread_id, subject, date, internaldate, flags, size, headers, body_text, attachment_names, indexed_at";
/// `MESSAGE_COLUMNS` without the body text and the index columns. Those sit behind `raw` in the row, so reading them walks
/// a stored message's overflow pages, and an indexed body's text is the largest column read.
const BODILESS_COLUMNS: &str = "folder, uid, message_id, from_addr, to_addr, cc_addr, delivered_to, in_reply_to, refs, thread_id, subject, date, internaldate, flags, size, headers, NULL AS body_text, NULL AS attachment_names, NULL AS indexed_at";

/// The SQL of one search tier: `?1` the FTS expression (NULL when empty), `?2` the folder (NULL for all), then the
/// plan's parameters, the excluded expression when `exclude`, the position to start after when `after`, and the
/// limit. The folder is its own condition only when there is one: through `?2 IS NULL OR folder = ?2` the planner
/// cannot use the folder's index, and without a match to drive the join it would read every message.
fn search_sql(
    expression: &str,
    exclude: bool,
    folder: Option<&str>,
    plan: &Plan,
    snippets: bool,
    after: bool,
) -> String {
    // The body is column 6 of the FTS table; bm25 weighted on some columns is non-zero only when they matched.
    let hit_columns = if snippets {
        "snippet(messages_fts, 6, '', '', '…', 12) AS snip,
         bm25(messages_fts, 1, 1, 1, 1, 1, 1, 0) AS header_score,
         bm25(messages_fts, 0, 0, 0, 0, 0, 0, 1) AS body_score"
    } else {
        "NULL AS snip, 0.0 AS header_score, 0.0 AS body_score"
    };
    let in_folder = if folder.is_some() {
        "folder = ?2"
    } else {
        "?2 IS NULL"
    };
    let mut sql = if expression.is_empty() {
        format!(
            "SELECT {MESSAGE_COLUMNS}, NULL AS snip, 0.0 AS header_score, 0.0 AS body_score FROM messages
             WHERE ?1 IS NULL AND {in_folder}"
        )
    } else {
        format!(
            "SELECT {MESSAGE_COLUMNS}, snip, header_score, body_score FROM messages
             JOIN (SELECT rowid AS fts_id, {hit_columns} FROM messages_fts WHERE messages_fts MATCH ?1) AS hit
               ON hit.fts_id = messages.id
             WHERE {in_folder}"
        )
    };
    for predicate in &plan.sql {
        sql.push_str(" AND (");
        sql.push_str(predicate);
        sql.push(')');
    }
    let mut next = 3 + plan.params.len();
    if exclude {
        sql.push_str(&format!(
            " AND messages.id NOT IN (SELECT rowid FROM messages_fts WHERE messages_fts MATCH ?{next})"
        ));
        next += 1;
    }
    if after {
        sql.push_str(&format!(
            " AND (messages.internaldate, messages.uid, messages.folder) < (?{next}, ?{}, ?{})",
            next + 1,
            next + 2
        ));
        next += 3;
    }
    // The folder decides between equal dates and uids in different folders, so a page ends at one place.
    sql.push_str(&format!(
        " ORDER BY internaldate DESC, uid DESC, folder DESC LIMIT ?{next}"
    ));
    sql
}

/// The parameters of `search_sql`, in its order; a negative `limit` reads every row.
fn tier_values(
    expression: &str,
    exclude: Option<&str>,
    folder: Option<&str>,
    plan: &Plan,
    after: Option<&After>,
    limit: i64,
) -> Vec<rusqlite::types::Value> {
    let mut values: Vec<rusqlite::types::Value> = vec![
        (!expression.is_empty())
            .then(|| expression.to_owned())
            .into(),
        folder.map(str::to_owned).into(),
    ];
    values.extend(plan.params.iter().cloned());
    if let Some(exclude) = exclude {
        values.push(exclude.to_owned().into());
    }
    if let Some(after) = after {
        values.push(after.internaldate.into());
        values.push(after.uid.into());
        values.push(after.folder.clone().into());
    }
    values.push(limit.into());
    values
}

/// One search, the same on every page of it: what `search_after` and `search_count` read.
#[derive(Clone, Copy)]
pub struct SearchRequest<'a> {
    pub query: &'a Query,
    pub scope: Scope,
    /// Only this folder; None for every folder.
    pub folder: Option<&'a str>,
    /// Whether close matches may follow the exact ones.
    pub close: bool,
    /// Resolves `from:me` and `to:me`.
    pub identity: Option<&'a Identity>,
}

impl SearchRequest<'_> {
    fn plan(&self, store: &Store) -> Plan {
        search::plan(
            self.query,
            self.scope,
            self.close.then_some(store as &dyn Vocab),
            self.identity,
        )
    }
}

/// The thread list of folder `?1`, at most `?2` threads, read from the covering index `messages_folder_thread`
/// alone. With exactly one MAX() in the query, SQLite fills the bare columns from the row holding it: the thread's
/// latest message. A second MAX() anywhere breaks that.
fn thread_summaries_sql(filter: Filter) -> String {
    format!(
        "SELECT thread_id, {SUMMARY_COLUMNS}, MAX(internaldate) AS latest, COUNT(*), SUM({UNSEEN}), SUM({FLAGGED})
          FROM messages WHERE folder = ?1 GROUP BY thread_id HAVING SUM({}) > 0
          ORDER BY latest DESC, uid DESC LIMIT ?2",
        filter.sql()
    )
}

/// The index backlog at `?1`, newest first, at most `?2`: each half read from its own partial index up to the
/// limit, then the newest of both. One condition over both halves would read every row.
fn backlog_sql() -> String {
    format!(
        "SELECT * FROM (SELECT {MESSAGE_COLUMNS} FROM messages WHERE {NEVER}
                        ORDER BY internaldate DESC, uid DESC LIMIT ?2)
         UNION ALL
         SELECT * FROM (SELECT {MESSAGE_COLUMNS} FROM messages WHERE {RETRY}
                        ORDER BY internaldate DESC, uid DESC LIMIT ?2)
         ORDER BY internaldate DESC, uid DESC LIMIT ?2"
    )
}

impl Store {
    pub fn migrations() -> Migrations<'static> {
        Migrations::new(migration_list())
    }

    pub fn open(path: &Path) -> Result<Store, StoreError> {
        // The migrator reads the schema version before its transaction, so two processes opening an old store at
        // once would both run the same migration; the lock makes the second see the first one's result. Taking it
        // creates the folder, private.
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("store");
        let _lock = crate::paths::lock_beside(path, &format!(".{name}.lock"))?;
        let conn = Connection::open(path)?;
        Store::init(conn)
    }

    /// The account's store, in its private directory.
    pub fn open_account(paths: &Paths, account: &str) -> Result<Store, StoreError> {
        paths.ensure_account(account)?;
        Store::open(&paths.mail_db(account))
    }

    pub fn open_in_memory() -> Result<Store, StoreError> {
        Store::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> Result<Store, StoreError> {
        conn.pragma_update_and_check(None, "journal_mode", "WAL", |_| Ok(()))?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        // Clients migrate and write rule clocks while the daemon may hold the write lock.
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        Store::migrations().to_latest(&mut conn)?;
        Ok(Store { conn })
    }

    /// Runs `f` in one transaction; any error rolls back everything `f` wrote.
    pub fn transaction<T, E: From<StoreError>>(
        &self,
        f: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, E> {
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(StoreError::from)?;
        let out = f()?;
        tx.commit().map_err(StoreError::from)?;
        Ok(out)
    }

    /// Renames `from` and the stored folders under it (by `from`'s stored delimiter) to sit under `to`, with their
    /// messages and rule history, in one transaction.
    pub fn rename_folder(&self, from: &str, to: &str) -> Result<(), StoreError> {
        let prefix = self
            .folder(from)?
            .and_then(|folder| folder.delimiter)
            .filter(|delimiter| !delimiter.is_empty())
            .map(|delimiter| format!("{from}{delimiter}"));
        let children: Vec<String> = match &prefix {
            Some(prefix) => self
                .folders()?
                .into_iter()
                .map(|folder| folder.name)
                .filter(|name| name.starts_with(prefix.as_str()))
                .collect(),
            None => Vec::new(),
        };
        let tx = self.conn.unchecked_transaction()?;
        tx.pragma_update(None, "defer_foreign_keys", "ON")?;
        rename_folder_rows(&tx, from, to)?;
        for child in children {
            rename_folder_rows(&tx, &child, &format!("{to}{}", &child[from.len()..]))?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn folders(&self) -> Result<Vec<Folder>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT name, uidvalidity, last_uid, special_use, delimiter FROM folders ORDER BY name",
        )?;
        let rows = serde_rusqlite::from_rows::<Folder>(stmt.query([])?);
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn folder(&self, name: &str) -> Result<Option<Folder>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT name, uidvalidity, last_uid, special_use, delimiter FROM folders WHERE name = ?1",
        )?;
        let mut rows = stmt.query(params![name])?;
        match rows.next()? {
            Some(row) => Ok(Some(serde_rusqlite::from_row(row)?)),
            None => Ok(None),
        }
    }

    /// The folders the server last listed as `\\Noselect`, replacing those of the listing before.
    pub fn set_noselect_folders(&self, names: &[&str]) -> Result<(), StoreError> {
        self.transaction(|| {
            self.conn.execute("DELETE FROM noselect_folders", [])?;
            for name in names {
                self.conn.execute(
                    "INSERT OR IGNORE INTO noselect_folders (name) VALUES (?1)",
                    params![name],
                )?;
            }
            Ok(())
        })
    }

    /// Whether the server last listed `name` as `\\Noselect`, a folder that holds no messages.
    pub fn is_noselect(&self, name: &str) -> Result<bool, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT 1 FROM noselect_folders WHERE name = ?1",
                params![name],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// Records that a pass went through every folder the server listed; `synced_folders` answers from then on.
    pub fn set_folders_listed(&self, at: i64) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO folder_list (id, synced_at) VALUES (1, ?1)
             ON CONFLICT (id) DO UPDATE SET synced_at = ?1",
            params![at],
        )?;
        Ok(())
    }

    /// The folders a sync filled in, without a move's placeholders; `None` until a pass went through every folder the
    /// server listed, since before that a folder may only not be reached yet.
    pub fn synced_folders(&self) -> Result<Option<Vec<Folder>>, StoreError> {
        let listed: Option<i64> = self
            .conn
            .query_row("SELECT synced_at FROM folder_list", [], |row| row.get(0))
            .optional()?;
        if listed.is_none() {
            return Ok(None);
        }
        Ok(Some(
            self.folders()?
                .into_iter()
                .filter(|f| f.uidvalidity != 0)
                .collect(),
        ))
    }

    pub fn upsert_folder(&self, folder: &Folder) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO folders (name, uidvalidity, last_uid, special_use, delimiter) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(name) DO UPDATE SET uidvalidity = excluded.uidvalidity, last_uid = excluded.last_uid,
             special_use = excluded.special_use, delimiter = COALESCE(excluded.delimiter, folders.delimiter)",
            params![folder.name, folder.uidvalidity, folder.last_uid, folder.special_use, folder.delimiter],
        )?;
        Ok(())
    }

    /// One above the highest uid already on the server when `folder` was first tracked; 0 when unknown.
    pub fn initial_uid_next(&self, folder: &str) -> Result<u32, StoreError> {
        self.folder_uid(folder, "initial_uid_next")
    }

    pub fn set_initial_uid_next(&self, folder: &str, uid_next: u32) -> Result<(), StoreError> {
        self.set_folder_uid(folder, "initial_uid_next", uid_next)
    }

    /// The UIDNEXT the last complete sync of `folder` saw; 0 when unknown.
    pub fn uid_next(&self, folder: &str) -> Result<u32, StoreError> {
        self.folder_uid(folder, "uid_next")
    }

    pub fn set_uid_next(&self, folder: &str, uid_next: u32) -> Result<(), StoreError> {
        self.set_folder_uid(folder, "uid_next", uid_next)
    }

    /// Uids below this one in `folder` are history its first sync has not fetched yet; 0 when there is none.
    pub fn backfill_uid(&self, folder: &str) -> Result<u32, StoreError> {
        self.folder_uid(folder, "backfill_uid")
    }

    pub fn set_backfill_uid(&self, folder: &str, uid: u32) -> Result<(), StoreError> {
        self.set_folder_uid(folder, "backfill_uid", uid)
    }

    /// The highest uid the rules have evaluated in `folder`; above it mail is fresh. 0 when unknown.
    pub fn rules_uid(&self, folder: &str) -> Result<u32, StoreError> {
        self.folder_uid(folder, "rules_uid")
    }

    pub fn set_rules_uid(&self, folder: &str, uid: u32) -> Result<(), StoreError> {
        self.set_folder_uid(folder, "rules_uid", uid)
    }

    /// The highest uid new-mail notifications have covered in `folder`; 0 when unknown.
    pub fn notified_uid(&self, folder: &str) -> Result<u32, StoreError> {
        self.folder_uid(folder, "notified_uid")
    }

    pub fn set_notified_uid(&self, folder: &str, uid: u32) -> Result<(), StoreError> {
        self.set_folder_uid(folder, "notified_uid", uid)
    }

    /// The HIGHESTMODSEQ (0 when unknown) and Unix time at which `folder`'s flags were last fully up to date.
    pub fn flags_synced(&self, folder: &str) -> Result<(u64, i64), StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT highest_modseq, flags_synced_at FROM folders WHERE name = ?1",
                params![folder],
                |r| Ok((r.get::<_, i64>(0)? as u64, r.get(1)?)),
            )
            .optional()?
            .unwrap_or((0, 0)))
    }

    pub fn set_flags_synced(
        &self,
        folder: &str,
        highest_modseq: u64,
        at: i64,
    ) -> Result<(), StoreError> {
        // RFC 7162 caps mod-sequences at 2^63 - 1, so they fit SQLite's signed INTEGER.
        self.conn.execute(
            "UPDATE folders SET highest_modseq = ?2, flags_synced_at = ?3 WHERE name = ?1",
            params![folder, highest_modseq as i64, at],
        )?;
        Ok(())
    }

    /// A uid column of the `folders` row; 0 when the folder is untracked.
    fn folder_uid(&self, folder: &str, column: &'static str) -> Result<u32, StoreError> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {column} FROM folders WHERE name = ?1"),
                params![folder],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0))
    }

    fn set_folder_uid(
        &self,
        folder: &str,
        column: &'static str,
        uid: u32,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            &format!("UPDATE folders SET {column} = ?2 WHERE name = ?1"),
            params![folder, uid],
        )?;
        Ok(())
    }

    pub fn reset_folder(&self, name: &str, uidvalidity: u32) -> Result<(), StoreError> {
        self.conn
            .execute("DELETE FROM messages WHERE folder = ?1", params![name])?;
        self.conn.execute(
            "UPDATE folders SET uidvalidity = ?2, last_uid = 0, uid_next = 0, backfill_uid = 0 WHERE name = ?1",
            params![name, uidvalidity],
        )?;
        Ok(())
    }

    /// Removes the folder with its messages and its rule and notification state.
    pub fn remove_folder(&self, name: &str) -> Result<(), StoreError> {
        self.conn
            .execute("DELETE FROM folders WHERE name = ?1", params![name])?;
        Ok(())
    }

    pub fn insert_message(&self, m: &Message) -> Result<(), StoreError> {
        let sql = format!(
            "INSERT OR IGNORE INTO messages ({MESSAGE_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)"
        );
        self.conn.prepare_cached(&sql)?.execute(params![
            m.folder,
            m.uid,
            m.message_id,
            m.from_addr,
            m.to_addr,
            m.cc_addr,
            m.delivered_to,
            m.in_reply_to,
            m.refs,
            m.thread_id,
            m.subject,
            m.date,
            m.internaldate,
            m.flags,
            m.size,
            m.headers,
            m.body_text,
            m.attachment_names,
            m.indexed_at
        ])?;
        Ok(())
    }

    /// The thread of a stored message with this Message-ID, in any folder.
    pub fn thread_of(&self, message_id: &str) -> Result<Option<String>, StoreError> {
        Ok(self
            .conn
            .prepare_cached("SELECT thread_id FROM messages WHERE message_id = ?1 LIMIT 1")?
            .query_row(params![message_id], |r| r.get(0))
            .optional()?)
    }

    /// Moves the stored replies without References under this message, and theirs in turn, to its thread: a backfill
    /// fetches newest first, so a reply can be stored before its parent.
    pub fn adopt_replies(&self, message_id: &str, thread_id: &str) -> Result<(), StoreError> {
        self.conn
            .prepare_cached(
                "WITH RECURSIVE parents(id) AS (
                   SELECT ?1
                   UNION
                   SELECT m.message_id FROM messages AS m JOIN parents ON m.in_reply_to = parents.id
                    WHERE m.refs IS NULL AND m.message_id IS NOT NULL
                 )
                 UPDATE messages SET thread_id = ?2
                  WHERE refs IS NULL AND in_reply_to IN parents AND thread_id IS NOT ?2",
            )?
            .execute(params![message_id, thread_id])?;
        Ok(())
    }

    pub fn update_flags(&self, folder: &str, uid: u32, flags: &str) -> Result<(), StoreError> {
        self.conn
            .prepare_cached(
                "UPDATE messages SET flags = ?3 WHERE folder = ?1 AND uid = ?2 AND flags IS NOT ?3",
            )?
            .execute(params![folder, uid, flags])?;
        Ok(())
    }

    pub fn remove_message(&self, folder: &str, uid: u32) -> Result<(), StoreError> {
        self.conn.execute(
            "DELETE FROM messages WHERE folder = ?1 AND uid = ?2",
            params![folder, uid],
        )?;
        Ok(())
    }

    /// Removes rows with uid <= `upto_uid` whose uid is not in `present`.
    pub fn remove_missing(
        &self,
        folder: &str,
        upto_uid: u32,
        present: &[u32],
    ) -> Result<usize, StoreError> {
        let present = serde_json::to_string(present)?;
        Ok(self.conn.execute(
            "DELETE FROM messages
             WHERE folder = ?1 AND uid <= ?2 AND uid NOT IN (SELECT value FROM json_each(?3))",
            params![folder, upto_uid, present],
        )?)
    }

    pub fn messages(
        &self,
        folder: &str,
        limit: u32,
        filter: Filter,
    ) -> Result<Vec<Message>, StoreError> {
        self.messages_after(folder, limit, filter, None)
    }

    /// The messages in `folder` that `filter` keeps, newest first, at most `limit`; with `after`, only those after
    /// that position. One folder's `internaldate` and `uid` order it, so `after.folder` plays no part.
    pub fn messages_after(
        &self,
        folder: &str,
        limit: u32,
        filter: Filter,
        after: Option<&After>,
    ) -> Result<Vec<Message>, StoreError> {
        let order = "ORDER BY internaldate DESC, uid DESC LIMIT ?2";
        match after {
            None => self.query_messages(
                &format!("WHERE folder = ?1 AND {} {order}", filter.sql()),
                params![folder, limit],
            ),
            Some(after) => self.query_messages(
                &format!(
                    "WHERE folder = ?1 AND {} AND (internaldate, uid) < (?3, ?4) {order}",
                    filter.sql()
                ),
                params![folder, limit, after.internaldate, after.uid],
            ),
        }
    }

    /// How many messages in `folder` `filter` keeps.
    pub fn count_messages(&self, folder: &str, filter: Filter) -> Result<u64, StoreError> {
        let count: i64 = self.conn.query_row(
            &format!(
                "SELECT COUNT(*) FROM messages WHERE folder = ?1 AND {}",
                filter.sql()
            ),
            params![folder],
            |r| r.get(0),
        )?;
        Ok(u64::try_from(count).unwrap_or(0))
    }

    /// How many threads in `folder` have a member `filter` keeps.
    pub fn count_threads(&self, folder: &str, filter: Filter) -> Result<u64, StoreError> {
        let count: i64 = self.conn.query_row(
            &format!(
                "SELECT COUNT(*) FROM (SELECT 1 FROM messages WHERE folder = ?1 GROUP BY thread_id HAVING SUM({}) > 0)",
                filter.sql()
            ),
            params![folder],
            |r| r.get(0),
        )?;
        Ok(u64::try_from(count).unwrap_or(0))
    }

    /// `SELECT` every message column, then `tail`; the statement is cached, so a repeated `tail` is prepared once.
    fn query_messages(
        &self,
        tail: &str,
        params: impl rusqlite::Params,
    ) -> Result<Vec<Message>, StoreError> {
        let mut stmt = self
            .conn
            .prepare_cached(&format!("SELECT {MESSAGE_COLUMNS} FROM messages {tail}"))?;
        let rows = serde_rusqlite::from_rows::<Message>(stmt.query(params)?);
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Threads in `folder` with a member `filter` keeps, the most recently active first; each thread's messages oldest
    /// first.
    pub fn threads(
        &self,
        folder: &str,
        limit: u32,
        filter: Filter,
    ) -> Result<Vec<Vec<Message>>, StoreError> {
        self.threads_after(folder, limit, filter, None)
    }

    /// Like `threads`; with `after`, only the threads after that position, where a thread's `internaldate` is its
    /// latest message's and its `uid` the highest of its members'. `after.folder` plays no part.
    pub fn threads_after(
        &self,
        folder: &str,
        limit: u32,
        filter: Filter,
        after: Option<&After>,
    ) -> Result<Vec<Vec<Message>>, StoreError> {
        let ids: Vec<String> = {
            let position = if after.is_some() {
                " AND (MAX(internaldate), MAX(uid)) < (?3, ?4)"
            } else {
                " AND ?3 IS NULL AND ?4 IS NULL"
            };
            let mut stmt = self.conn.prepare(&format!(
                "SELECT thread_id FROM messages WHERE folder = ?1 GROUP BY thread_id HAVING SUM({}) > 0{position}
                 ORDER BY MAX(internaldate) DESC, MAX(uid) DESC LIMIT ?2",
                filter.sql()
            ))?;
            let rows = stmt.query_map(
                params![
                    folder,
                    limit,
                    after.map(|a| a.internaldate),
                    after.map(|a| a.uid)
                ],
                |r| r.get(0),
            )?;
            rows.collect::<Result<_, _>>()?
        };
        ids.iter()
            .map(|id| {
                self.query_messages(
                    "WHERE folder = ?1 AND thread_id = ?2 ORDER BY internaldate, uid",
                    params![folder, id],
                )
            })
            .collect()
    }

    /// One row per thread in `folder` with a member `filter` keeps, the most recently active first.
    pub fn thread_summaries(
        &self,
        folder: &str,
        limit: u32,
        filter: Filter,
    ) -> Result<Vec<ThreadSummary>, StoreError> {
        let mut stmt = self.conn.prepare(&thread_summaries_sql(filter))?;
        let rows = stmt.query_map(params![folder, limit], |r| {
            Ok(ThreadSummary {
                thread_id: r.get(0)?,
                latest: summary_from_row(r, 1)?,
                count: r.get(10)?,
                unread: r.get::<_, i64>(11)? > 0,
                flagged: r.get::<_, i64>(12)? > 0,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn thread_members(
        &self,
        folder: &str,
        thread_id: &str,
    ) -> Result<Vec<MessageSummary>, StoreError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {SUMMARY_COLUMNS} FROM messages WHERE folder = ?1 AND thread_id = ?2 ORDER BY internaldate, uid"
        ))?;
        let rows = stmt.query_map(params![folder, thread_id], |r| summary_from_row(r, 0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn messages_in_folder(&self, folder: &str) -> Result<Vec<Message>, StoreError> {
        self.query_messages("WHERE folder = ?1 ORDER BY uid", params![folder])
    }

    /// The messages in `folder` with a uid above `after`, oldest first, read without their body text: what the rules
    /// read of a folder none of whose rules tests the body.
    pub fn messages_without_body(
        &self,
        folder: &str,
        after: u32,
    ) -> Result<Vec<Message>, StoreError> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {BODILESS_COLUMNS} FROM messages WHERE folder = ?1 AND uid > ?2 ORDER BY uid"
        ))?;
        let rows = serde_rusqlite::from_rows::<Message>(stmt.query(params![folder, after])?);
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// The messages in `folder` that arrived at or after `after` and before `before`, newest first; with their body
    /// text only when `with_body`, so sender statistics and rules coverage read no more than the rules they run need.
    pub fn messages_between(
        &self,
        folder: &str,
        after: Option<i64>,
        before: Option<i64>,
        with_body: bool,
    ) -> Result<Vec<Message>, StoreError> {
        let columns = if with_body {
            MESSAGE_COLUMNS
        } else {
            BODILESS_COLUMNS
        };
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {columns} FROM messages WHERE folder = ?1 AND internaldate >= ?2 AND internaldate < ?3
             ORDER BY internaldate DESC, uid DESC"
        ))?;
        let range = params![
            folder,
            after.unwrap_or(i64::MIN),
            before.unwrap_or(i64::MAX)
        ];
        let rows = serde_rusqlite::from_rows::<Message>(stmt.query(range)?);
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// The messages `query` matches, newest first, in `folder` or any folder, at most `limit`. `close` adds the
    /// close matches when the exact ones are few; `identity` resolves `from:me` and `to:me`. The plan's predicates
    /// follow the FTS match; their `?` parameters count on from the fixed ones, so the later placeholders are
    /// numbered after them.
    pub fn search(
        &self,
        query: &Query,
        scope: Scope,
        folder: Option<&str>,
        limit: u32,
        close: bool,
        identity: Option<&Identity>,
    ) -> Result<Vec<Hit>, StoreError> {
        let request = SearchRequest {
            query,
            scope,
            folder,
            close,
            identity,
        };
        self.search_after(&request, limit, None)
    }

    /// Like `search`, as a page: the exact matches newest first, then the close ones newest first, and with `after`
    /// only what follows that position in its tier. The close tier joins when the exact one, as a whole, holds
    /// fewer than ten matches, and once a page showed it, it stays (see `Resume::Close`).
    pub fn search_after(
        &self,
        request: &SearchRequest,
        limit: u32,
        after: Option<Resume>,
    ) -> Result<Vec<Hit>, StoreError> {
        let plan = request.plan(self);
        if plan.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let snippets = request.scope == Scope::All;
        let mut hits = match after {
            Some(Resume::Close { .. }) => Vec::new(),
            Some(Resume::Exact(position)) => self.search_tier(
                &plan.exact,
                plan.exclude.as_deref(),
                request.folder,
                &plan,
                Some(position),
                i64::from(limit),
                Matched::Exact,
                snippets,
            )?,
            None => self.search_tier(
                &plan.exact,
                plan.exclude.as_deref(),
                request.folder,
                &plan,
                None,
                i64::from(limit),
                Matched::Exact,
                snippets,
            )?,
        };
        let left = i64::from(limit) - i64::try_from(hits.len()).unwrap_or(i64::MAX);
        // The close tier only when the exact one is thin, and never a message the exact expression matches.
        if let Some(widened) = &plan.close
            && left > 0
        {
            // Without a position, a page that is not full holds every exact match. Close matches an earlier page
            // showed keep showing, so mail that arrived since does not hide the rest.
            let thin = match after {
                None => hits.len() < CLOSE_MATCHES_BELOW,
                Some(Resume::Close { shown: true, .. }) => true,
                Some(_) => self.exact_is_thin(request, &plan)?,
            };
            if thin {
                let position = match after {
                    Some(Resume::Close { after, .. }) => Some(after),
                    _ => None,
                };
                hits.extend(self.search_tier(
                    widened,
                    Some(&plan.exact),
                    request.folder,
                    &plan,
                    position,
                    left,
                    Matched::Close,
                    snippets,
                )?);
            }
        }
        Ok(hits)
    }

    /// How many messages the search finds, close matches included when they would show.
    pub fn search_count(&self, request: &SearchRequest) -> Result<u64, StoreError> {
        let plan = request.plan(self);
        if plan.is_empty() {
            return Ok(0);
        }
        let exact = self.count_tier(&plan.exact, plan.exclude.as_deref(), request.folder, &plan)?;
        let close = match &plan.close {
            Some(widened) if exact < CLOSE_MATCHES_BELOW as u64 => {
                self.count_tier(widened, Some(&plan.exact), request.folder, &plan)?
            }
            _ => 0,
        };
        Ok(exact + close)
    }

    /// Whether the exact tier holds fewer than `CLOSE_MATCHES_BELOW` matches, read no further than that.
    fn exact_is_thin(&self, request: &SearchRequest, plan: &Plan) -> Result<bool, StoreError> {
        let first = self.search_tier(
            &plan.exact,
            plan.exclude.as_deref(),
            request.folder,
            plan,
            None,
            CLOSE_MATCHES_BELOW as i64,
            Matched::Exact,
            false,
        )?;
        Ok(first.len() < CLOSE_MATCHES_BELOW)
    }

    /// How many messages one tier matches.
    fn count_tier(
        &self,
        expression: &str,
        exclude: Option<&str>,
        folder: Option<&str>,
        plan: &Plan,
    ) -> Result<u64, StoreError> {
        let sql = format!(
            "SELECT COUNT(*) FROM ({})",
            search_sql(expression, exclude.is_some(), folder, plan, false, false)
        );
        let values = tier_values(expression, exclude, folder, plan, None, -1);
        let count: i64 = self
            .conn
            .prepare_cached(&sql)?
            .query_row(rusqlite::params_from_iter(values), |r| r.get(0))?;
        Ok(u64::try_from(count).unwrap_or(0))
    }

    /// The messages `expression` matches and `exclude` does not, newest first, at most `limit`; an empty
    /// expression matches every message, for a query of predicates alone. With `snippets`, a hit whose body matched
    /// carries the text around the match. The plan's predicates follow the FTS match; their `?` parameters count on
    /// from the fixed ones, so the later placeholders are numbered after them.
    #[allow(clippy::too_many_arguments)] // one per part of the query
    fn search_tier(
        &self,
        expression: &str,
        exclude: Option<&str>,
        folder: Option<&str>,
        plan: &Plan,
        after: Option<&After>,
        limit: i64,
        matched: Matched,
        snippets: bool,
    ) -> Result<Vec<Hit>, StoreError> {
        let sql = search_sql(
            expression,
            exclude.is_some(),
            folder,
            plan,
            snippets,
            after.is_some(),
        );
        let values = tier_values(expression, exclude, folder, plan, after, limit);
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let mut rows = stmt.query(rusqlite::params_from_iter(values))?;
        let mut hits = Vec::new();
        while let Some(row) = rows.next()? {
            let message: Message = serde_rusqlite::from_row(row)?;
            let header_score: f64 = row.get("header_score")?;
            let body_score: f64 = row.get("body_score")?;
            // Only a hit the headers did not make: the row shows them already. One line: a line break in the body
            // becomes a space, and control characters go.
            let snippet = if body_score < 0.0 && header_score == 0.0 {
                row.get::<_, Option<String>>("snip")?.map(|text| {
                    crate::message::clean(&text, true)
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ")
                })
            } else {
                None
            };
            hits.push(Hit {
                message,
                matched,
                snippet,
            });
        }
        Ok(hits)
    }

    pub fn message(&self, folder: &str, uid: u32) -> Result<Option<Message>, StoreError> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM messages WHERE folder = ?1 AND uid = ?2"
        ))?;
        let mut rows = stmt.query(params![folder, uid])?;
        match rows.next()? {
            Some(row) => Ok(Some(serde_rusqlite::from_row(row)?)),
            None => Ok(None),
        }
    }

    pub fn raw(&self, folder: &str, uid: u32) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT raw FROM messages WHERE folder = ?1 AND uid = ?2",
                params![folder, uid],
                |r| r.get::<_, Option<Vec<u8>>>(0),
            )
            .optional()?
            .flatten())
    }

    pub fn set_raw(
        &self,
        folder: &str,
        uid: u32,
        raw: &[u8],
        body_text: &str,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE messages SET raw = ?3, body_text = ?4 WHERE folder = ?1 AND uid = ?2",
            params![folder, uid, raw, body_text],
        )?;
        Ok(())
    }

    /// How many messages still await an index pass at `now`, and how many messages there are.
    pub fn backlog_count(&self, now: i64) -> Result<(u64, u64), StoreError> {
        let remaining = self.backlog_remaining(now)?;
        let total: i64 = self
            .conn
            .query_row("SELECT count(*) FROM messages", [], |r| r.get(0))?;
        Ok((remaining, u64::try_from(total).unwrap_or(0)))
    }

    /// How many messages wait for the indexer, from the two partial indexes rather than every row: once indexing is done
    /// both are empty, so the check each idle turn makes costs nothing.
    pub fn backlog_remaining(&self, now: i64) -> Result<u64, StoreError> {
        let never: i64 = self.conn.query_row(
            &format!("SELECT count(*) FROM messages WHERE {NEVER}"),
            [],
            |r| r.get(0),
        )?;
        let retry: i64 = self.conn.query_row(
            &format!("SELECT count(*) FROM messages WHERE {RETRY}"),
            params![now],
            |r| r.get(0),
        )?;
        Ok(u64::try_from(never + retry).unwrap_or(0))
    }

    /// The next messages to index, newest first: never tried, or tried without text more than a day before `now`.
    pub fn backlog(&self, now: i64, limit: u32) -> Result<Vec<Message>, StoreError> {
        let mut stmt = self.conn.prepare_cached(&backlog_sql())?;
        let rows = serde_rusqlite::from_rows::<Message>(stmt.query(params![now, limit])?);
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Records an index pass at `at`: `text` is None when the fetch failed, so the message is retried a day later,
    /// and Some("") when the message has no text. Never touches `raw`.
    pub fn set_indexed(
        &self,
        folder: &str,
        uid: u32,
        text: Option<&str>,
        names: &[String],
        at: i64,
    ) -> Result<(), StoreError> {
        let names = (!names.is_empty()).then(|| names.join("\n"));
        self.conn
            .prepare_cached(
                "UPDATE messages SET indexed_at = ?3,
                    body_text = CASE WHEN ?4 IS NULL THEN body_text ELSE ?4 END,
                    attachment_names = CASE WHEN ?4 IS NULL THEN attachment_names ELSE ?5 END
                 WHERE folder = ?1 AND uid = ?2",
            )?
            .execute(params![folder, uid, at, text, names])?;
        Ok(())
    }

    /// Whether the message's raw bytes are stored, which is what the reader needs; an indexed message may lack them.
    pub fn has_raw(&self, folder: &str, uid: u32) -> Result<bool, StoreError> {
        Ok(self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM messages WHERE folder = ?1 AND uid = ?2 AND raw IS NOT NULL)",
            params![folder, uid],
            |r| r.get(0),
        )?)
    }

    /// Forgets every index pass and the text it stored, so a benchmark can index the same mail again.
    #[cfg(any(test, feature = "testing"))]
    pub fn forget_indexing(&self) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE messages SET indexed_at = NULL, body_text = NULL, attachment_names = NULL",
            [],
        )?;
        Ok(())
    }

    /// FTS5's own check of the search index against the message rows; an error means it needs a rebuild.
    pub fn check_search_index(&self) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO messages_fts(messages_fts) VALUES ('integrity-check')",
            [],
        )?;
        Ok(())
    }

    /// Rebuilds the search index from the message rows, as migration 011 did once; the cure for a failed
    /// integrity check.
    pub fn rebuild_search_index(&self) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO messages_fts(messages_fts) VALUES ('rebuild')",
            [],
        )?;
        Ok(())
    }

    /// Returns when the rule was first seen with this definition, starting its clock at `now` if it is new or changed.
    pub fn rule_first_seen(
        &self,
        name: &str,
        definition: &str,
        now: i64,
    ) -> Result<i64, StoreError> {
        self.conn.execute(
            "INSERT INTO rules_seen (name, first_seen_at, definition) VALUES (?1, ?2, ?3)
             ON CONFLICT (name) DO UPDATE SET
               first_seen_at = CASE WHEN definition IS NULL OR definition = excluded.definition
                                    THEN first_seen_at ELSE excluded.first_seen_at END,
               definition = excluded.definition",
            params![name, now, definition],
        )?;
        Ok(self.conn.query_row(
            "SELECT first_seen_at FROM rules_seen WHERE name = ?1",
            params![name],
            |r| r.get(0),
        )?)
    }

    /// Starts the rule's clock at `now`, replacing any stale first-seen time; the next load adopts its definition.
    pub fn restart_rule_clock(&self, name: &str, now: i64) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT OR REPLACE INTO rules_seen (name, first_seen_at) VALUES (?1, ?2)",
            params![name, now],
        )?;
        Ok(())
    }

    /// Remembers that a rule moved `msg` out of its folder.
    pub fn record_rule_move(&self, msg: &Message) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT OR IGNORE INTO rule_moves (folder, message_id, internaldate) VALUES (?1, ?2, ?3)",
            params![msg.folder, msg.message_id.as_deref().unwrap_or(""), msg.internaldate],
        )?;
        Ok(())
    }

    /// Whether a rule moved `msg` out of its folder before, so it came back.
    pub fn rule_moved_out(&self, msg: &Message) -> Result<bool, StoreError> {
        Ok(self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM rule_moves WHERE folder = ?1 AND message_id = ?2 AND internaldate = ?3)",
            params![msg.folder, msg.message_id.as_deref().unwrap_or(""), msg.internaldate],
            |r| r.get(0),
        )?)
    }

    /// Moves `rules_uid` up to `last_uid` in every folder not in `except`, so their mail stops counting as fresh.
    pub fn mark_rules_seen_except(&self, except: &[String]) -> Result<(), StoreError> {
        let except = serde_json::to_string(except)?;
        self.conn.execute(
            "UPDATE folders SET rules_uid = last_uid
             WHERE last_uid > rules_uid AND name NOT IN (SELECT value FROM json_each(?1))",
            params![except],
        )?;
        Ok(())
    }

    /// Drops first-seen times of rules no longer in the file, so a re-added rule starts fresh instead of acting on history.
    pub fn forget_rules_except(&self, names: &[&str]) -> Result<(), StoreError> {
        let names = serde_json::to_string(names)?;
        self.conn.execute(
            "DELETE FROM rules_seen WHERE name NOT IN (SELECT value FROM json_each(?1))",
            params![names],
        )?;
        Ok(())
    }

    pub fn log_action(&self, entry: &LogEntry) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO rule_log (at, rule_name, folder, uid, message_id, subject, action, trash_file)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![entry.at, entry.rule_name, entry.folder, entry.uid, entry.message_id, entry.subject, entry.action, entry.trash_file],
        )?;
        Ok(())
    }

    pub fn log(&self, limit: u32) -> Result<Vec<LogEntry>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, at, rule_name, folder, uid, message_id, subject, action, trash_file FROM rule_log ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = serde_rusqlite::from_rows::<LogEntry>(stmt.query(params![limit])?);
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn message_count(&self, folder: &str) -> Result<u32, StoreError> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM messages WHERE folder = ?1",
            params![folder],
            |r| r.get(0),
        )?)
    }

    pub fn unread_count(&self, folder: &str) -> Result<u32, StoreError> {
        Ok(self
            .conn
            .query_row(&unread_count_sql(), params![folder], |r| r.get(0))?)
    }

    pub fn unread_uids(&self, folder: &str) -> Result<Vec<u32>, StoreError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT uid FROM messages WHERE folder = ?1 AND {UNSEEN} ORDER BY uid"
        ))?;
        let uids = stmt.query_map(params![folder], |r| r.get(0))?;
        Ok(uids.collect::<Result<_, _>>()?)
    }
}

impl Vocab for Store {
    /// Runs the word through the index's own tokenizer: a one-row FTS5 table in `temp`, read back through its
    /// vocabulary, so the folding is exactly the index's. Anything failing here only disables close matches.
    fn fold(&self, word: &str) -> Option<String> {
        let prepared = self.conn.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS temp.query_tokens
               USING fts5(w, tokenize=\"unicode61 remove_diacritics 2\");
             CREATE VIRTUAL TABLE IF NOT EXISTS temp.query_vocab
               USING fts5vocab('temp', 'query_tokens', 'row');
             DELETE FROM temp.query_tokens;",
        );
        let folded = prepared
            .and_then(|()| {
                self.conn.execute(
                    "INSERT INTO temp.query_tokens (w) VALUES (?1)",
                    params![word],
                )
            })
            .and_then(|_| {
                let mut stmt = self
                    .conn
                    .prepare_cached("SELECT term FROM temp.query_vocab")?;
                let terms = stmt.query_map([], |r| r.get::<_, String>(0))?;
                terms.collect::<Result<Vec<_>, _>>()
            });
        match folded {
            Ok(mut terms) if terms.len() == 1 => terms.pop(),
            Ok(_) => None,
            Err(e) => {
                log::debug!("close matches off: {e}");
                None
            }
        }
    }

    fn candidates(&self, first: char, min_len: usize, max_len: usize) -> Vec<Candidate> {
        let found = self
            .conn
            .prepare_cached(
                "SELECT term, sum(doc), max(col != 'body_text') FROM messages_vocab
                 WHERE term >= ?1 AND term < ?2 AND length(term) BETWEEN ?3 AND ?4
                 GROUP BY term",
            )
            .and_then(|mut stmt| {
                let rows = stmt.query_map(
                    params![
                        first.to_string(),
                        format!("{first}\u{10FFFF}"),
                        min_len as i64,
                        max_len as i64
                    ],
                    |r| {
                        Ok(Candidate {
                            term: r.get(0)?,
                            docs: u64::try_from(r.get::<_, i64>(1)?).unwrap_or(0),
                            in_headers: r.get::<_, i64>(2)? != 0,
                        })
                    },
                )?;
                rows.collect::<Result<Vec<_>, _>>()
            });
        match found {
            Ok(candidates) => candidates,
            Err(e) => {
                log::debug!("close matches off: {e}");
                Vec::new()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_keeps_its_delimiter_through_an_upsert_without_one() {
        let store = Store::open_in_memory().unwrap();
        let mut folder = Folder {
            name: "Projects/Postvak".into(),
            uidvalidity: 1,
            last_uid: 0,
            special_use: None,
            delimiter: Some("/".into()),
        };
        store.upsert_folder(&folder).unwrap();
        folder.delimiter = None;
        folder.last_uid = 5;
        store.upsert_folder(&folder).unwrap();
        let stored = store.folder("Projects/Postvak").unwrap().unwrap();
        assert_eq!(stored.delimiter.as_deref(), Some("/"));
        assert_eq!(stored.last_uid, 5);
        assert_eq!(store.folders().unwrap()[0].delimiter.as_deref(), Some("/"));
    }

    #[test]
    fn open_account_makes_the_account_directory_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        Store::open_account(&paths, "work").unwrap();
        let meta = std::fs::metadata(paths.account_dir("work")).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o700);
    }

    #[test]
    fn rename_folder_keeps_messages_and_rule_history_of_children() {
        let s = Store::open_in_memory().unwrap();
        for name in ["Work", "Work/Clients", "Workshop"] {
            s.upsert_folder(&Folder {
                name: name.into(),
                uidvalidity: 1,
                last_uid: 1,
                special_use: None,
                delimiter: Some("/".into()),
            })
            .unwrap();
            s.insert_message(&msg(name, 1, 10)).unwrap();
            s.log_action(&LogEntry {
                id: 0,
                at: 1,
                rule_name: "r".into(),
                folder: name.into(),
                uid: 1,
                message_id: None,
                subject: None,
                action: "flag".into(),
                trash_file: None,
            })
            .unwrap();
        }
        s.rename_folder("Work", "Archive/Work").unwrap();
        let mut names: Vec<String> = s.folders().unwrap().into_iter().map(|f| f.name).collect();
        names.sort();
        assert_eq!(names, ["Archive/Work", "Archive/Work/Clients", "Workshop"]);
        for name in ["Archive/Work", "Archive/Work/Clients", "Workshop"] {
            assert_eq!(s.messages_in_folder(name).unwrap().len(), 1, "{name}");
        }
        assert!(s.messages_in_folder("Work").unwrap().is_empty());
        let mut logged: Vec<String> = s.log(10).unwrap().into_iter().map(|e| e.folder).collect();
        logged.sort();
        assert_eq!(logged, ["Archive/Work", "Archive/Work/Clients", "Workshop"]);
    }

    fn msg(folder: &str, uid: u32, internaldate: i64) -> Message {
        Message {
            folder: folder.into(),
            uid,
            message_id: Some(format!("m{uid}@x")),
            from_addr: Some("Alice <alice@x>".into()),
            to_addr: Some("bob@x".into()),
            thread_id: format!("m{uid}@x"),
            subject: Some(format!("subject {uid}")),
            date: Some(internaldate),
            internaldate,
            size: Some(100),
            headers: b"From: alice@x\r\n\r\n".to_vec(),
            ..Default::default()
        }
    }

    fn store_with_inbox() -> Store {
        let s = Store::open_in_memory().unwrap();
        s.upsert_folder(&Folder {
            name: "INBOX".into(),
            uidvalidity: 1,
            last_uid: 0,
            special_use: None,
            delimiter: None,
        })
        .unwrap();
        s
    }

    #[test]
    fn migrations_are_valid() {
        Store::migrations().validate().unwrap();
    }

    #[test]
    fn every_migration_directory_is_listed() {
        let dirs = std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/migrations")).unwrap();
        assert_eq!(dirs.count(), migration_list().len());
    }

    #[test]
    fn open_creates_file_and_applies_schema() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("acc").join("mail.db");
        let s = Store::open(&path).unwrap();
        assert!(path.exists());
        assert!(s.folders().unwrap().is_empty());
    }

    #[test]
    fn opening_a_new_store_from_threads_at_once_migrates_it_once() {
        let dir = tempfile::tempdir().unwrap();
        for round in 0..20 {
            let path = dir.path().join(format!("{round}.db"));
            std::thread::scope(|scope| {
                for _ in 0..4 {
                    let path = &path;
                    scope.spawn(move || Store::open(path).unwrap());
                }
            });
        }
    }

    #[test]
    fn folder_upsert_and_reset() {
        let s = store_with_inbox();
        s.insert_message(&msg("INBOX", 1, 10)).unwrap();
        s.upsert_folder(&Folder {
            name: "INBOX".into(),
            uidvalidity: 1,
            last_uid: 1,
            special_use: None,
            delimiter: None,
        })
        .unwrap();
        assert_eq!(s.folder("INBOX").unwrap().unwrap().last_uid, 1);
        s.set_uid_next("INBOX", 2).unwrap();
        s.reset_folder("INBOX", 2).unwrap();
        let f = s.folder("INBOX").unwrap().unwrap();
        assert_eq!((f.uidvalidity, f.last_uid), (2, 0));
        assert_eq!(s.uid_next("INBOX").unwrap(), 0, "a reset forgets UIDNEXT");
        assert!(s.messages_in_folder("INBOX").unwrap().is_empty());
    }

    #[test]
    fn threads_group_by_thread_most_recent_activity_first() {
        let s = store_with_inbox();
        let mut root = msg("INBOX", 1, 10);
        root.thread_id = "root@x".into();
        let mut reply = msg("INBOX", 3, 30);
        reply.thread_id = "root@x".into();
        let other = msg("INBOX", 2, 20);
        for m in [&root, &reply, &other] {
            s.insert_message(m).unwrap();
        }
        let uids: Vec<Vec<u32>> = s
            .threads("INBOX", 10, Filter::default())
            .unwrap()
            .iter()
            .map(|thread| thread.iter().map(|m| m.uid).collect())
            .collect();
        assert_eq!(uids, [vec![1, 3], vec![2]]);
        assert_eq!(s.threads("INBOX", 1, Filter::default()).unwrap().len(), 1);
    }

    #[test]
    fn thread_depth_follows_reply_headers() {
        let mut m = msg("INBOX", 1, 10);
        assert_eq!(m.thread_depth(), 0);
        m.in_reply_to = Some("a@x".into());
        assert_eq!(m.thread_depth(), 1);
        m.refs = Some("a@x b@x".into());
        assert_eq!(m.thread_depth(), 2);
    }

    #[test]
    fn messages_round_trip_and_order_newest_first() {
        let s = store_with_inbox();
        s.insert_message(&msg("INBOX", 1, 10)).unwrap();
        s.insert_message(&msg("INBOX", 2, 20)).unwrap();
        let list = s.messages("INBOX", 10, Filter::default()).unwrap();
        assert_eq!(list.iter().map(|m| m.uid).collect::<Vec<_>>(), vec![2, 1]);
        assert_eq!(list[1], msg("INBOX", 1, 10));
        assert_eq!(s.message("INBOX", 3).unwrap(), None);
    }

    #[test]
    fn flags_raw_and_removal() {
        let s = store_with_inbox();
        s.insert_message(&msg("INBOX", 1, 10)).unwrap();
        s.update_flags("INBOX", 1, "\\Seen \\Flagged").unwrap();
        assert!(s.message("INBOX", 1).unwrap().unwrap().is_seen());
        assert_eq!(s.raw("INBOX", 1).unwrap(), None);
        s.set_raw("INBOX", 1, b"raw bytes", "body words").unwrap();
        assert_eq!(
            s.raw("INBOX", 1).unwrap().as_deref(),
            Some(&b"raw bytes"[..])
        );
        assert_eq!(
            s.message("INBOX", 1).unwrap().unwrap().body_text.as_deref(),
            Some("body words")
        );
        s.remove_message("INBOX", 1).unwrap();
        assert_eq!(s.message("INBOX", 1).unwrap(), None);
    }

    fn fts_hits(s: &Store, query: &str) -> i64 {
        s.conn
            .query_row(
                "SELECT count(*) FROM messages_fts WHERE messages_fts MATCH ?1",
                params![query],
                |r| r.get(0),
            )
            .unwrap()
    }

    fn fts_integrity_check(s: &Store) {
        s.conn
            .execute(
                "INSERT INTO messages_fts(messages_fts) VALUES ('integrity-check')",
                [],
            )
            .unwrap();
    }

    #[test]
    fn fts_follows_rows_and_ignores_flag_updates() {
        let s = store_with_inbox();
        s.insert_message(&msg("INBOX", 1, 10)).unwrap();
        assert_eq!(fts_hits(&s, "alice"), 1);
        s.set_raw("INBOX", 1, b"raw", "pineapple").unwrap();
        assert_eq!(fts_hits(&s, "pineapple"), 1);
        let before = s.conn.total_changes();
        s.update_flags("INBOX", 1, "").unwrap();
        assert_eq!(
            s.conn.total_changes(),
            before,
            "unchanged flags write nothing"
        );
        s.update_flags("INBOX", 1, "\\Seen").unwrap();
        assert_eq!(
            s.conn.total_changes(),
            before + 1,
            "a flag change does not touch the FTS index"
        );
        s.remove_message("INBOX", 1).unwrap();
        assert_eq!(fts_hits(&s, "pineapple"), 0);
        fts_integrity_check(&s);
    }

    #[test]
    fn the_backlog_is_newest_first_across_folders_and_skips_indexed_rows() {
        let s = store_with_inbox();
        s.upsert_folder(&Folder {
            name: "Archive".into(),
            uidvalidity: 1,
            last_uid: 0,
            special_use: None,
            delimiter: None,
        })
        .unwrap();
        s.insert_message(&msg("INBOX", 1, 10)).unwrap();
        s.insert_message(&msg("Archive", 2, 30)).unwrap();
        s.insert_message(&msg("INBOX", 3, 20)).unwrap();
        let uids = |rows: Vec<Message>| rows.iter().map(|m| m.uid).collect::<Vec<_>>();
        assert_eq!(uids(s.backlog(1_000, 10).unwrap()), [2, 3, 1]);
        assert_eq!(uids(s.backlog(1_000, 2).unwrap()), [2, 3]);
        assert_eq!(s.backlog_count(1_000).unwrap(), (3, 3));
        s.set_indexed("Archive", 2, Some("pineapple"), &[], 1_000)
            .unwrap();
        assert_eq!(uids(s.backlog(1_000, 10).unwrap()), [3, 1]);
        assert_eq!(s.backlog_count(1_000).unwrap(), (2, 3));
    }

    #[test]
    fn set_indexed_writes_text_and_names_the_index_follows_and_never_raw() {
        let s = store_with_inbox();
        s.insert_message(&msg("INBOX", 1, 10)).unwrap();
        s.set_indexed(
            "INBOX",
            1,
            Some("pineapple pie"),
            &["Q3 report.pdf".into(), "notes.txt".into()],
            50,
        )
        .unwrap();
        let m = s.message("INBOX", 1).unwrap().unwrap();
        assert_eq!(m.body_text.as_deref(), Some("pineapple pie"));
        assert_eq!(
            m.attachment_names.as_deref(),
            Some("Q3 report.pdf\nnotes.txt")
        );
        assert_eq!(m.indexed_at, Some(50));
        assert_eq!(fts_hits(&s, "pineapple"), 1);
        assert_eq!(fts_hits(&s, "notes"), 1);
        assert!(
            !s.has_raw("INBOX", 1).unwrap(),
            "the indexer never stores raw"
        );
        s.set_raw("INBOX", 1, b"raw", "pineapple pie").unwrap();
        assert!(s.has_raw("INBOX", 1).unwrap());
        fts_integrity_check(&s);
    }

    #[test]
    fn a_failed_index_keeps_the_text_unknown_and_is_retried_after_a_day() {
        let s = store_with_inbox();
        s.insert_message(&msg("INBOX", 1, 10)).unwrap();
        s.set_indexed("INBOX", 1, None, &[], 1_000).unwrap();
        let m = s.message("INBOX", 1).unwrap().unwrap();
        assert_eq!((m.body_text, m.indexed_at), (None, Some(1_000)));
        assert!(s.backlog(1_000, 10).unwrap().is_empty());
        assert_eq!(s.backlog_count(1_000 + 86_399).unwrap(), (0, 1));
        assert_eq!(s.backlog(1_000 + 86_401, 10).unwrap().len(), 1);
        // A message indexed without text is done, not failed.
        s.set_indexed("INBOX", 1, Some(""), &["a.pdf".into()], 2_000)
            .unwrap();
        assert!(s.backlog(2_000 + 1_000_000, 10).unwrap().is_empty());
        assert_eq!(
            s.message("INBOX", 1).unwrap().unwrap().body_text.as_deref(),
            Some("")
        );
    }

    #[test]
    fn the_search_index_check_passes_and_a_rebuild_keeps_the_hits() {
        let s = store_with_inbox();
        s.insert_message(&msg("INBOX", 1, 10)).unwrap();
        s.set_raw("INBOX", 1, b"raw", "pineapple").unwrap();
        s.check_search_index().unwrap();
        s.rebuild_search_index().unwrap();
        s.check_search_index().unwrap();
        assert_eq!(fts_hits(&s, "pineapple"), 1);
    }

    #[test]
    fn migration_011_rebuilds_the_index_of_an_older_store() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mail.db");
        {
            let mut conn = Connection::open(&path).unwrap();
            Store::migrations().to_version(&mut conn, 10).unwrap();
            conn.execute(
                "INSERT INTO folders (name, uidvalidity) VALUES ('INBOX', 1)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO messages (folder, uid, thread_id, subject, from_addr, internaldate, flags, headers, body_text)
                 VALUES ('INBOX', 1, 't', 'Café invoice', 'alice@x', 10, '', X'', 'pineapple pie')",
                [],
            )
            .unwrap();
        }
        let s = Store::open(&path).unwrap();
        assert_eq!(
            fts_hits(&s, "pineapple"),
            1,
            "a body indexed before the rebuild"
        );
        assert_eq!(
            fts_hits(&s, "cafe"),
            1,
            "the rebuilt index folds diacritics"
        );
        fts_integrity_check(&s);
    }

    #[test]
    fn migration_021_rethreads_stored_replies_without_references() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mail.db");
        {
            let mut conn = Connection::open(&path).unwrap();
            Store::migrations().to_version(&mut conn, 20).unwrap();
            conn.execute_batch(
                "INSERT INTO folders (name, uidvalidity) VALUES ('INBOX', 1);
                 INSERT INTO messages (folder, uid, message_id, in_reply_to, refs, thread_id, internaldate, flags, headers) VALUES
                   ('INBOX', 1, 'mid@x', 'root@x', 'root@x', 'root@x', 10, '', X''),
                   ('INBOX', 2, 'reply@x', 'mid@x', NULL, 'mid@x', 11, '', X''),
                   ('INBOX', 3, 'orphan@x', 'gone@x', NULL, 'gone@x', 12, '', X'');",
            )
            .unwrap();
        }
        let s = Store::open(&path).unwrap();
        let thread = |uid| s.message("INBOX", uid).unwrap().unwrap().thread_id;
        assert_eq!(thread(2), "root@x");
        assert_eq!(thread(3), "gone@x", "a parent not stored leaves the thread");
    }

    #[test]
    fn migration_017_decodes_folder_names_stored_in_modified_utf7() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mail.db");
        {
            let mut conn = Connection::open(&path).unwrap();
            Store::migrations().to_version(&mut conn, 16).unwrap();
            conn.execute_batch(
                "INSERT INTO folders (name, uidvalidity, last_uid, rules_uid, delimiter) VALUES
                     ('INBOX', 1, 1, 1, '/'), ('Archief/&AMkA6Q-n', 7, 3, 2, '/'), ('R&-D', 8, 1, 1, '/'),
                     ('Caf&AOk-', 9, 1, 1, '/'), ('Caf\u{e9}', 0, 0, 0, NULL),
                     ('Re&AOc-us', 4, 1, 1, '/'), ('Re\u{e7}us', 5, 1, 1, '/'),
                     ('Sloppy&AOk-&AOk-', 6, 1, 1, '/');
                 INSERT INTO messages (folder, uid, thread_id, subject, internaldate, flags, headers) VALUES
                     ('Archief/&AMkA6Q-n', 3, 't', 'kept', 10, '', X''),
                     ('Caf&AOk-', 1, 't2', 'cafe', 10, '', X''),
                     ('Re&AOc-us', 1, 't3', 'dropped', 10, '', X''),
                     ('Re\u{e7}us', 1, 't4', 'synced', 10, '', X'');
                 INSERT INTO rule_log (at, rule_name, folder, uid, action) VALUES
                     (1, 'r', 'Archief/&AMkA6Q-n', 3, 'flag'), (2, 'r', 'Re&AOc-us', 1, 'flag');
                 INSERT INTO rule_moves (folder, message_id, internaldate) VALUES
                     ('R&-D', '<a@x>', 10), ('Re&AOc-us', '<b@x>', 10);",
            )
            .unwrap();
        }
        let s = Store::open(&path).unwrap();
        let mut names: Vec<String> = s.folders().unwrap().into_iter().map(|f| f.name).collect();
        names.sort();
        assert_eq!(
            names,
            [
                "Archief/\u{c9}\u{e9}n",
                "Caf\u{e9}",
                "INBOX",
                "R&D",
                "Re\u{e7}us",
                "Sloppy&AOk-&AOk-"
            ]
        );
        let archief = s.folder("Archief/\u{c9}\u{e9}n").unwrap().unwrap();
        assert_eq!((archief.uidvalidity, archief.last_uid), (7, 3));
        assert_eq!(s.rules_uid("Archief/\u{c9}\u{e9}n").unwrap(), 2);
        assert_eq!(
            s.messages_in_folder("Archief/\u{c9}\u{e9}n").unwrap().len(),
            1
        );
        let cafe = s.folder("Caf\u{e9}").unwrap().unwrap();
        assert_eq!(
            cafe.uidvalidity, 9,
            "the synced folder, not the placeholder"
        );
        assert_eq!(s.messages_in_folder("Caf\u{e9}").unwrap().len(), 1);
        let recus = s.messages_in_folder("Re\u{e7}us").unwrap();
        assert_eq!(recus.len(), 1);
        assert_eq!(recus[0].subject.as_deref(), Some("synced"));
        let log = s.log(10).unwrap();
        let mut logged: Vec<&str> = log.iter().map(|e| e.folder.as_str()).collect();
        logged.sort();
        assert_eq!(
            logged,
            ["Archief/\u{c9}\u{e9}n", "Re&AOc-us"],
            "the history of a folder that gave way is not the raw twin's"
        );
        let moved: Vec<String> = s
            .conn
            .prepare("SELECT folder FROM rule_moves")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(moved, ["R&D"]);
        fts_integrity_check(&s);
        let orphan = s
            .conn
            .query_row("PRAGMA foreign_key_check", [], |_| Ok(()))
            .optional()
            .unwrap();
        assert_eq!(orphan, None, "a message lost its folder");
    }

    #[test]
    fn diacritics_fold_in_both_directions() {
        let s = store_with_inbox();
        let mut m = msg("INBOX", 1, 10);
        m.subject = Some("Café Müller".into());
        s.insert_message(&m).unwrap();
        for query in ["cafe", "café", "muller", "MÜLLER"] {
            assert_eq!(fts_hits(&s, query), 1, "{query}");
        }
    }

    #[test]
    fn cc_delivered_to_and_attachment_names_are_indexed() {
        let s = store_with_inbox();
        let mut m = msg("INBOX", 1, 10);
        m.cc_addr = Some("bob@example.org".into());
        m.delivered_to = Some("list@example.org".into());
        s.insert_message(&m).unwrap();
        s.conn
            .execute(
                "UPDATE messages SET attachment_names = 'Q3 report.pdf' WHERE uid = 1",
                [],
            )
            .unwrap();
        assert_eq!(fts_hits(&s, "bob"), 1);
        assert_eq!(fts_hits(&s, "list"), 1);
        assert_eq!(fts_hits(&s, "report"), 1);
        assert_eq!(fts_hits(&s, "attachment_names:report"), 1);
        fts_integrity_check(&s);
    }

    #[test]
    fn the_vocab_table_lists_terms_per_column() {
        let s = store_with_inbox();
        s.insert_message(&msg("INBOX", 1, 10)).unwrap();
        s.set_raw("INBOX", 1, b"raw", "pineapple").unwrap();
        let (col, doc): (String, i64) = s
            .conn
            .query_row(
                "SELECT col, doc FROM messages_vocab WHERE term = 'pineapple'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((col.as_str(), doc), ("body_text", 1));
    }

    #[test]
    fn insert_ignores_duplicate_folder_uid() {
        let s = store_with_inbox();
        s.insert_message(&msg("INBOX", 1, 10)).unwrap();
        let mut again = msg("INBOX", 1, 99);
        again.subject = Some("other".into());
        s.insert_message(&again).unwrap();
        assert_eq!(
            s.messages_in_folder("INBOX").unwrap(),
            vec![msg("INBOX", 1, 10)]
        );
    }

    #[test]
    fn remove_missing_drops_uids_not_present() {
        let s = store_with_inbox();
        for uid in 1..=4 {
            s.insert_message(&msg("INBOX", uid, uid as i64)).unwrap();
        }
        let removed = s.remove_missing("INBOX", 4, &[1, 3]).unwrap();
        assert_eq!(removed, 2);
        let left: Vec<u32> = s
            .messages_in_folder("INBOX")
            .unwrap()
            .iter()
            .map(|m| m.uid)
            .collect();
        assert_eq!(left, vec![1, 3]);
    }

    #[test]
    fn a_rule_move_is_remembered_per_folder_message_id_and_internaldate() {
        let s = Store::open_in_memory().unwrap();
        let msg = |folder: &str, id: Option<&str>, internaldate| Message {
            folder: folder.into(),
            message_id: id.map(Into::into),
            internaldate,
            ..Default::default()
        };
        s.record_rule_move(&msg("INBOX", Some("a@x"), 10)).unwrap();
        s.record_rule_move(&msg("INBOX", None, 20)).unwrap();
        s.record_rule_move(&msg("INBOX", Some("a@x"), 10)).unwrap();
        assert!(s.rule_moved_out(&msg("INBOX", Some("a@x"), 10)).unwrap());
        assert!(s.rule_moved_out(&msg("INBOX", None, 20)).unwrap());
        assert!(!s.rule_moved_out(&msg("Lists", Some("a@x"), 10)).unwrap());
        assert!(
            !s.rule_moved_out(&msg("INBOX", Some("a@x"), 11)).unwrap(),
            "another copy"
        );
        assert!(!s.rule_moved_out(&msg("INBOX", Some("b@x"), 10)).unwrap());
    }

    #[test]
    fn rule_first_seen_is_sticky_while_the_definition_is_unchanged() {
        let s = store_with_inbox();
        assert_eq!(s.rule_first_seen("purge", "a", 100).unwrap(), 100);
        assert_eq!(s.rule_first_seen("purge", "a", 200).unwrap(), 100);
        assert_eq!(s.rule_first_seen("other", "a", 200).unwrap(), 200);
    }

    #[test]
    fn rule_first_seen_restarts_when_the_definition_changes() {
        let s = store_with_inbox();
        assert_eq!(s.rule_first_seen("purge", "a", 100).unwrap(), 100);
        assert_eq!(s.rule_first_seen("purge", "b", 200).unwrap(), 200);
        assert_eq!(s.rule_first_seen("purge", "b", 300).unwrap(), 200);
    }

    #[test]
    fn restart_rule_clock_overwrites_the_first_seen_time() {
        let s = store_with_inbox();
        assert_eq!(s.rule_first_seen("purge", "a", 100).unwrap(), 100);
        s.restart_rule_clock("purge", 300).unwrap();
        assert_eq!(s.rule_first_seen("purge", "a", 400).unwrap(), 300);
        s.restart_rule_clock("fresh", 500).unwrap();
        assert_eq!(s.rule_first_seen("fresh", "a", 600).unwrap(), 500);
    }

    #[test]
    fn migration_006_keeps_rule_clocks_and_adopts_the_next_definition() {
        let mut conn = Connection::open_in_memory().unwrap();
        Store::migrations().to_version(&mut conn, 5).unwrap();
        conn.execute(
            "INSERT INTO rules_seen (name, first_seen_at) VALUES ('purge', 100)",
            [],
        )
        .unwrap();
        let s = Store::init(conn).unwrap();
        assert_eq!(s.rule_first_seen("purge", "a", 200).unwrap(), 100);
        assert_eq!(s.rule_first_seen("purge", "b", 300).unwrap(), 300);
    }

    #[test]
    fn log_round_trip_newest_first() {
        let s = store_with_inbox();
        for i in 1..=3 {
            s.log_action(&LogEntry {
                id: 0,
                at: i,
                rule_name: "r".into(),
                folder: "INBOX".into(),
                uid: i as u32,
                message_id: None,
                subject: None,
                action: "delete".into(),
                trash_file: Some(format!("{i}.eml")),
            })
            .unwrap();
        }
        let log = s.log(2).unwrap();
        assert_eq!(log.iter().map(|e| e.at).collect::<Vec<_>>(), vec![3, 2]);
    }

    #[test]
    fn unread_and_message_counts() {
        let s = store_with_inbox();
        s.insert_message(&msg("INBOX", 1, 10)).unwrap();
        s.insert_message(&msg("INBOX", 2, 20)).unwrap();
        s.update_flags("INBOX", 2, "\\Seen").unwrap();
        assert_eq!(s.unread_count("INBOX").unwrap(), 1);
        assert_eq!(s.unread_uids("INBOX").unwrap(), [1]);
        assert_eq!(s.message_count("INBOX").unwrap(), 2);
        assert_eq!(s.message_count("Other").unwrap(), 0);
    }

    /// A fixed `now` for the queries: 2026-10-24 12:00 UTC.
    const NOW: i64 = 1_792_843_200;

    /// The uids `text` finds, as a user would type it.
    fn found(s: &Store, text: &str, scope: Scope, folder: Option<&str>, limit: u32) -> Vec<u32> {
        s.search(&search::parse(text, NOW), scope, folder, limit, false, None)
            .unwrap()
            .iter()
            .map(|hit| hit.message.uid)
            .collect()
    }

    /// Uids with how each matched, close matches on.
    fn tiers(s: &Store, text: &str, folder: Option<&str>, limit: u32) -> Vec<(u32, Matched)> {
        s.search(
            &search::parse(text, NOW),
            Scope::All,
            folder,
            limit,
            true,
            None,
        )
        .unwrap()
        .iter()
        .map(|hit| (hit.message.uid, hit.matched))
        .collect()
    }

    #[test]
    fn the_store_folds_words_as_its_tokenizer_does_and_lists_its_vocabulary() {
        let s = store_with_inbox();
        let mut m = msg("INBOX", 1, 10);
        m.subject = Some("Café invoice".into());
        s.insert_message(&m).unwrap();
        s.set_raw("INBOX", 1, b"raw", "pineapple").unwrap();
        assert_eq!(s.fold("CAFÉ").as_deref(), Some("cafe"));
        assert_eq!(s.fold("pieter@x.nl"), None, "several tokens");
        assert_eq!(s.fold("..."), None, "no token");
        let mut found = s.candidates('i', 6, 8);
        found.sort_by(|a, b| a.term.cmp(&b.term));
        assert_eq!(
            found,
            [Candidate {
                term: "invoice".into(),
                docs: 1,
                in_headers: true
            }]
        );
        assert_eq!(
            s.candidates('p', 9, 9),
            [Candidate {
                term: "pineapple".into(),
                docs: 1,
                in_headers: false
            }]
        );
        assert!(s.candidates('p', 2, 3).is_empty());
    }

    #[test]
    fn close_matches_follow_exact_ones_when_exact_finds_fewer_than_ten() {
        let s = store_with_inbox();
        let mut exact = msg("INBOX", 1, 10);
        exact.subject = Some("invioce for March".into());
        s.insert_message(&exact).unwrap();
        let mut close = msg("INBOX", 2, 20);
        close.subject = Some("Invoice for April".into());
        s.insert_message(&close).unwrap();
        s.insert_message(&msg("INBOX", 3, 30)).unwrap();
        let close = Matched::Close;
        assert_eq!(
            tiers(&s, "invioce", None, 10),
            [(1, Matched::Exact), (2, close)]
        );
        assert_eq!(
            tiers(&s, "invioce", None, 1),
            [(1, Matched::Exact)],
            "the limit covers both tiers"
        );
        assert_eq!(tiers(&s, "invioce", Some("INBOX"), 10).len(), 2);
        assert_eq!(
            tiers(&s, "invioce", Some("Archive"), 10),
            [],
            "the folder filter applies to both tiers"
        );
        assert_eq!(
            tiers(&s, "invoice", None, 10),
            [(2, Matched::Exact), (1, close)],
            "the other way round"
        );
        assert_eq!(
            found(&s, "invioce", Scope::All, None, 10),
            [1],
            "close matches off"
        );
    }

    #[test]
    fn close_matches_are_skipped_at_ten_exact_hits_and_never_repeat_an_exact_hit() {
        let s = store_with_inbox();
        for uid in 1..=10 {
            let mut m = msg("INBOX", uid, i64::from(uid) * 10);
            m.subject = Some("invoice".into());
            s.insert_message(&m).unwrap();
        }
        let mut typo = msg("INBOX", 11, 200);
        typo.subject = Some("invoce".into());
        s.insert_message(&typo).unwrap();
        let hits = tiers(&s, "invoice", None, 50);
        assert_eq!(hits.len(), 10, "ten exact hits, no close tier: {hits:?}");
        assert!(hits.iter().all(|(_, m)| *m == Matched::Exact));
        s.remove_message("INBOX", 10).unwrap();
        let hits = tiers(&s, "invoice", None, 50);
        assert_eq!(hits.len(), 10);
        assert_eq!(hits[9], (11, Matched::Close));
        assert_eq!(
            hits.iter().filter(|(uid, _)| *uid == 1).count(),
            1,
            "an exact hit is not also a close one"
        );
    }

    #[test]
    fn search_matches_words_newest_first_and_filters_by_folder() {
        let s = store_with_inbox();
        s.upsert_folder(&Folder {
            name: "Archive".into(),
            uidvalidity: 1,
            last_uid: 0,
            special_use: None,
            delimiter: None,
        })
        .unwrap();
        s.insert_message(&msg("INBOX", 1, 10)).unwrap();
        s.insert_message(&msg("INBOX", 2, 30)).unwrap();
        s.insert_message(&msg("Archive", 3, 20)).unwrap();
        let all = Scope::All;
        assert_eq!(found(&s, "subject", all, None, 10), [2, 3, 1]);
        assert_eq!(found(&s, "subject", all, Some("INBOX"), 10), [2, 1]);
        assert_eq!(found(&s, "subject", all, None, 1), [2]);
        assert_eq!(
            found(&s, "alice", all, None, 10),
            [2, 3, 1],
            "a sender's name"
        );
        assert_eq!(
            found(&s, "subj", all, None, 10),
            [2, 3, 1],
            "the last word is a prefix"
        );
        assert!(found(&s, "pineapple", all, None, 10).is_empty());
        assert!(found(&s, "   ", all, None, 10).is_empty());
        assert!(
            found(&s, "-subject", all, None, 10).is_empty(),
            "nothing positive to match"
        );
        let hit = &s
            .search(&search::parse("subject", NOW), all, None, 1, false, None)
            .unwrap()[0];
        assert_eq!(
            (hit.matched, hit.snippet.as_deref()),
            (Matched::Exact, None)
        );
    }

    #[test]
    fn a_bare_address_and_an_unbalanced_quote_search_as_words() {
        let s = store_with_inbox();
        s.insert_message(&msg("INBOX", 1, 10)).unwrap();
        assert_eq!(found(&s, "alice@x", Scope::All, None, 10), [1]);
        assert_eq!(found(&s, "\"alice@x", Scope::All, None, 10), [1]);
        assert!(found(&s, "re: lunch", Scope::All, None, 10).is_empty());
        assert!(found(&s, "\"unbalanced", Scope::All, None, 10).is_empty());
        assert!(found(&s, "NEAR(subject, 1)", Scope::All, None, 10).is_empty());
    }

    #[test]
    fn operators_narrow_by_folder_flags_attachment_date_column_and_me() {
        let s = store_with_inbox();
        s.upsert_folder(&Folder {
            name: "Archive".into(),
            uidvalidity: 1,
            last_uid: 0,
            special_use: None,
            delimiter: None,
        })
        .unwrap();
        let mut read = msg("INBOX", 1, NOW - 10 * 86_400);
        read.flags = "\\Seen \\Flagged".into();
        read.headers =
            b"From: alice@x\r\nContent-Type: multipart/mixed; boundary=b\r\n\r\n".to_vec();
        s.insert_message(&read).unwrap();
        let mut from_me = msg("INBOX", 2, NOW - 2 * 86_400);
        from_me.from_addr = Some("Me <me@example.com>".into());
        s.insert_message(&from_me).unwrap();
        let mut archived = msg("Archive", 3, NOW - 100 * 86_400);
        archived.to_addr = Some("orders@shop.example.com".into());
        s.insert_message(&archived).unwrap();
        let all = Scope::All;
        assert_eq!(found(&s, "subject in:Archive", all, None, 10), [3]);
        assert_eq!(
            found(&s, "in:Archive", all, None, 10),
            [3],
            "operators alone list what they describe"
        );
        assert_eq!(found(&s, "is:read", all, None, 10), [1]);
        assert_eq!(found(&s, "is:unread", all, None, 10), [2, 3]);
        assert_eq!(found(&s, "is:flagged", all, None, 10), [1]);
        assert_eq!(found(&s, "has:attachment", all, None, 10), [1]);
        assert_eq!(found(&s, "after:7d", all, None, 10), [2]);
        assert_eq!(found(&s, "before:7d", all, None, 10), [1, 3]);
        assert_eq!(found(&s, "subject from:alice", all, None, 10), [1, 3]);
        assert_eq!(found(&s, "subject -from:alice", all, None, 10), [2]);
        assert_eq!(
            found(&s, "-from:alice is:unread", all, None, 10),
            [2],
            "excluded from what the predicates list"
        );
        assert_eq!(
            found(&s, "subject", all, Some("INBOX"), 10),
            [2, 1],
            "the folder argument still applies"
        );
        assert_eq!(
            found(&s, "from:me", all, None, 10),
            [2],
            "without an identity, me is the word, which this sender's name happens to hold"
        );
        let identity = {
            let mut account = crate::config::AccountConfig::new(
                "work",
                "imap.example.com",
                "Me@example.com",
                crate::config::PasswordSource::Command {
                    command: "x".into(),
                },
            );
            account.aliases = vec!["*@shop.example.com".into()];
            account.identity().unwrap()
        };
        let with = |text: &str| -> Vec<u32> {
            s.search(
                &search::parse(text, NOW),
                all,
                None,
                10,
                false,
                Some(&identity),
            )
            .unwrap()
            .iter()
            .map(|hit| hit.message.uid)
            .collect()
        };
        assert_eq!(with("from:me"), [2], "the address, case-insensitively");
        assert_eq!(with("to:me"), [3], "an alias with a wildcard");
        assert_eq!(with("-from:me subject"), [1, 3]);
        assert!(with("cc:me").is_empty());
    }

    /// The hits of `text` as (uid, snippet).
    fn snippets(s: &Store, text: &str, scope: Scope) -> Vec<(u32, Option<String>)> {
        s.search(&search::parse(text, NOW), scope, None, 10, true, None)
            .unwrap()
            .into_iter()
            .map(|hit| (hit.message.uid, hit.snippet))
            .collect()
    }

    #[test]
    fn a_body_hit_carries_a_snippet_and_a_header_hit_does_not() {
        let s = store_with_inbox();
        let mut m = msg("INBOX", 1, 10);
        m.body_text = Some(
            "Dear Bob, the quick brown fox jumps over the lazy dog, as it does every single morning without fail."
                .into(),
        );
        s.insert_message(&m).unwrap();
        let [(uid, snippet)] = snippets(&s, "lazy dog", Scope::All).try_into().unwrap();
        assert_eq!(uid, 1);
        let snippet = snippet.unwrap();
        assert!(snippet.contains("lazy dog"), "{snippet}");
        assert!(
            snippet.contains('…') && snippet.len() < m.body_text.unwrap().len(),
            "a window of the body, not all of it: {snippet}"
        );
        assert!(snippet.split_whitespace().count() <= 13, "{snippet}");
        assert_eq!(
            snippets(&s, "subject", Scope::All),
            [(1, None)],
            "a hit in the headers shows them already"
        );
        assert_eq!(
            snippets(&s, "subject fox", Scope::All),
            [(1, None)],
            "the headers matched too, and the row shows them"
        );
        assert_eq!(snippets(&s, "is:unread", Scope::All), [(1, None)]);
    }

    #[test]
    fn snippets_are_never_built_in_the_headers_scope() {
        let s = store_with_inbox();
        let mut m = msg("INBOX", 1, 10);
        m.subject = Some("fox".into());
        m.body_text = Some("the fox again".into());
        s.insert_message(&m).unwrap();
        let mut in_body = msg("INBOX", 2, 20);
        in_body.body_text = Some("a fox in the body only".into());
        s.insert_message(&in_body).unwrap();
        assert_eq!(
            snippets(&s, "fox", Scope::Headers),
            [(1, None)],
            "the body hit is not even found"
        );
        let all = snippets(&s, "fox", Scope::All);
        assert_eq!(all.len(), 2);
        assert!(
            all[0].1.as_deref().is_some_and(|t| t.contains("fox")),
            "{all:?}"
        );
        assert_eq!(all[1], (1, None), "the subject shows the match");
    }

    #[test]
    fn a_snippet_is_cleaned_of_control_characters() {
        let s = store_with_inbox();
        let mut m = msg("INBOX", 1, 10);
        m.body_text = Some("fox\u{1b}[2J jumps\u{7} now,\nagain\ttomorrow".into());
        s.insert_message(&m).unwrap();
        let snippet = snippets(&s, "jumps", Scope::All)[0].1.clone().unwrap();
        assert!(!snippet.chars().any(char::is_control), "{snippet:?}");
        assert_eq!(snippet, "fox[2J jumps now, again tomorrow");
    }

    #[test]
    fn a_headers_search_ignores_bodies_even_through_fts_syntax() {
        let s = store_with_inbox();
        let mut m = msg("INBOX", 1, 100);
        m.subject = Some("hello".into());
        m.body_text = Some("secret word".into());
        s.insert_message(&m).unwrap();
        let headers = Scope::Headers;
        assert_eq!(found(&s, "hello", headers, None, 10), [1]);
        assert!(found(&s, "secret", headers, None, 10).is_empty());
        assert!(found(&s, "x) OR (body_text:secret", headers, None, 10).is_empty());
        assert!(found(&s, "x) OR body_text:secret", headers, None, 10).is_empty());
        assert!(found(&s, "body_text:secret", headers, None, 10).is_empty());
        assert!(
            found(&s, "hello -secret", headers, None, 10) == [1],
            "a negated body word is not seen either"
        );
        assert_eq!(found(&s, "secret", Scope::All, None, 10), [1]);
    }

    #[test]
    fn search_covers_fetched_bodies() {
        let s = store_with_inbox();
        s.insert_message(&msg("INBOX", 1, 10)).unwrap();
        assert!(found(&s, "pineapple", Scope::All, None, 10).is_empty());
        s.set_raw("INBOX", 1, b"raw", "pineapple pie").unwrap();
        assert_eq!(found(&s, "pineapple", Scope::All, None, 10), [1]);
        assert_eq!(found(&s, "\"pineapple pie\"", Scope::All, None, 10), [1]);
        assert!(found(&s, "\"pie pineapple\"", Scope::All, None, 10).is_empty());
    }

    #[test]
    fn migration_002_keeps_rows_and_marks_existing_mail_as_processed() {
        let mut conn = Connection::open_in_memory().unwrap();
        Store::migrations().to_version(&mut conn, 1).unwrap();
        conn.execute(
            "INSERT INTO folders (name, uidvalidity, last_uid) VALUES ('INBOX', 7, 42)",
            [],
        )
        .unwrap();
        let s = Store::init(conn).unwrap();
        let inbox = s.folder("INBOX").unwrap().unwrap();
        assert_eq!((inbox.uidvalidity, inbox.last_uid), (7, 42));
        assert_eq!(s.initial_uid_next("INBOX").unwrap(), 0);
        assert_eq!(s.rules_uid("INBOX").unwrap(), 42);
    }

    #[test]
    fn migration_003_counts_mail_the_rules_saw_as_notified() {
        let mut conn = Connection::open_in_memory().unwrap();
        Store::migrations().to_version(&mut conn, 2).unwrap();
        conn.execute(
            "INSERT INTO folders (name, uidvalidity, last_uid, rules_uid) VALUES ('INBOX', 7, 42, 40)",
            [],
        )
        .unwrap();
        let s = Store::init(conn).unwrap();
        assert_eq!(s.rules_uid("INBOX").unwrap(), 40);
        assert_eq!(s.notified_uid("INBOX").unwrap(), 40);
        s.set_notified_uid("INBOX", 42).unwrap();
        assert_eq!(s.notified_uid("INBOX").unwrap(), 42);
    }

    #[test]
    fn threading_a_reply_reads_the_message_id_and_reply_indexes() {
        let s = store_with_inbox();
        let plan = |sql: &str| -> String {
            let mut stmt = s
                .conn
                .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
                .unwrap();
            let rows = stmt.query_map([], |r| r.get::<_, String>(3)).unwrap();
            rows.collect::<Result<Vec<_>, _>>().unwrap().join("\n")
        };
        let lookup = plan("SELECT thread_id FROM messages WHERE message_id = 'm@x' LIMIT 1");
        assert!(lookup.contains("messages_message_id"), "{lookup}");
        let adopt = plan(
            "WITH RECURSIVE parents(id) AS (
               SELECT 'm@x'
               UNION
               SELECT m.message_id FROM messages AS m JOIN parents ON m.in_reply_to = parents.id
                WHERE m.refs IS NULL AND m.message_id IS NOT NULL
             )
             UPDATE messages SET thread_id = 't' WHERE refs IS NULL AND in_reply_to IN parents AND thread_id IS NOT 't'",
        );
        // The update and each recursive step: no pass over the whole table.
        assert_eq!(
            adopt.matches("USING INDEX messages_reply_to").count(),
            2,
            "{adopt}"
        );
    }

    #[test]
    fn the_thread_list_unread_count_and_index_backlog_read_their_indexes() {
        let s = store_with_inbox();
        // Without statistics an empty table plans differently; a few hundred rows plan like a real folder.
        for uid in 1..=500 {
            s.insert_message(&msg("INBOX", uid, i64::from(uid)))
                .unwrap();
        }
        let plan = |sql: &str| -> String {
            let mut stmt = s
                .conn
                .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
                .unwrap();
            let rows = stmt.query_map([], |r| r.get::<_, String>(3)).unwrap();
            rows.collect::<Result<Vec<_>, _>>().unwrap().join("\n")
        };
        // Whether the statement reads a column of the table itself, rather than of an index or a sorter. The plan
        // says COVERING INDEX only when it can tell before generating code; with `has_attachment`, a generated
        // column, it decides while generating it and opens the table with a seek it then never follows.
        let reads_table = |sql: &str| -> bool {
            let root: i64 = s
                .conn
                .query_row(
                    "SELECT rootpage FROM sqlite_master WHERE type = 'table' AND name = 'messages'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            let mut stmt = s.conn.prepare(&format!("EXPLAIN {sql}")).unwrap();
            let ops: Vec<(String, i64, i64)> = stmt
                .query_map([], |r| Ok((r.get(1)?, r.get(2)?, r.get(3)?)))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            let table_cursors: Vec<i64> = ops
                .iter()
                .filter(|(op, _, p2)| op == "OpenRead" && *p2 == root)
                .map(|(_, p1, _)| *p1)
                .collect();
            ops.iter()
                .any(|(op, p1, _)| op == "Column" && table_cursors.contains(p1))
        };
        for filter in [
            Filter::default(),
            Filter {
                unread: true,
                ..Filter::default()
            },
            Filter {
                flagged: true,
                ..Filter::default()
            },
            Filter {
                attachments: true,
                ..Filter::default()
            },
        ] {
            let sql = thread_summaries_sql(filter)
                .replace("?1", "'INBOX'")
                .replace("?2", "200");
            let thread_list = plan(&sql);
            assert!(
                thread_list.contains("INDEX messages_folder_thread") && !reads_table(&sql),
                "{filter:?}: {thread_list}"
            );
        }
        let unread = plan(&unread_count_sql().replace("?1", "'INBOX'"));
        assert!(unread.contains("INDEX messages_unread"), "{unread}");
        let never = plan("SELECT count(*) FROM messages WHERE indexed_at IS NULL");
        assert!(never.contains("INDEX messages_unindexed"), "{never}");
        let retry = plan(&format!(
            "SELECT count(*) FROM messages WHERE {}",
            RETRY.replace("?1", "1000")
        ));
        assert!(retry.contains("INDEX messages_retry"), "{retry}");
        let backlog = plan(&backlog_sql().replace("?1", "1000").replace("?2", "200"));
        assert!(
            backlog.contains("INDEX messages_unindexed")
                && backlog.contains("INDEX messages_retry")
                && !backlog.lines().any(|line| line.trim() == "SCAN messages"),
            "{backlog}"
        );
    }

    /// A search of predicates alone has no match to drive the join, so it walks an index newest first and stops at
    /// the limit: `messages_newest` across folders, and the folder's own index in one folder.
    #[test]
    fn a_search_without_words_reads_an_index_newest_first() {
        let s = store_with_inbox();
        for uid in 1..=500 {
            s.insert_message(&msg("INBOX", uid, i64::from(uid)))
                .unwrap();
        }
        let plan_of = |folder: Option<&str>| -> String {
            let query = search::parse("after:2024-01-01", 1_700_000_000);
            let planned = search::plan(&query, Scope::All, None, None);
            let sql = search_sql(&planned.exact, false, folder, &planned, true, false);
            let mut stmt = s
                .conn
                .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
                .unwrap();
            let values: Vec<rusqlite::types::Value> = vec![
                rusqlite::types::Null.into(),
                folder.map(str::to_owned).into(),
                1_000.into(),
                100.into(),
            ];
            let rows = stmt
                .query_map(rusqlite::params_from_iter(values), |r| {
                    r.get::<_, String>(3)
                })
                .unwrap();
            rows.collect::<Result<Vec<_>, _>>().unwrap().join("\n")
        };
        let all = plan_of(None);
        // The folder, last in the order, only parts rows of one date and uid: SQLite sorts each such group as it
        // walks the index and still stops at the limit. A sort of every match would read `USE TEMP B-TREE FOR
        // ORDER BY`.
        assert!(
            all.contains("INDEX messages_newest") && !all.contains("TEMP B-TREE FOR ORDER BY"),
            "{all}"
        );
        let one = plan_of(Some("INBOX"));
        assert!(
            one.contains("INDEX messages_internaldate (folder=? AND internaldate>?)"),
            "{one}"
        );
    }

    #[test]
    fn messages_without_body_leave_out_the_text_and_start_after_a_uid() {
        let s = store_with_inbox();
        for uid in 1..=3 {
            s.insert_message(&msg("INBOX", uid, i64::from(uid)))
                .unwrap();
            s.set_raw("INBOX", uid, b"Subject: x\r\n\r\nbody", "body")
                .unwrap();
        }
        let read = s.messages_without_body("INBOX", 1).unwrap();
        assert_eq!(read.iter().map(|m| m.uid).collect::<Vec<_>>(), [2, 3]);
        assert!(read.iter().all(|m| m.body_text.is_none()));
        let full = s.messages_in_folder("INBOX").unwrap();
        assert_eq!(full[1].body_text.as_deref(), Some("body"));
        assert_eq!(
            Message {
                body_text: None,
                ..full[1].clone()
            },
            read[0],
            "every other column is read"
        );
    }

    #[test]
    fn messages_between_read_a_date_range_newest_first_and_the_body_only_when_asked() {
        let s = store_with_inbox();
        for uid in 1..=4 {
            s.insert_message(&msg("INBOX", uid, i64::from(uid) * 10))
                .unwrap();
            s.set_raw("INBOX", uid, b"Subject: x\r\n\r\nbody", "body")
                .unwrap();
        }
        let uids = |rows: Vec<Message>| rows.iter().map(|m| m.uid).collect::<Vec<_>>();
        let all = s.messages_between("INBOX", None, None, false).unwrap();
        assert!(all.iter().all(|m| m.body_text.is_none()));
        assert_eq!(uids(all), [4, 3, 2, 1]);
        // after is inclusive, before exclusive, as in search.
        let range = s
            .messages_between("INBOX", Some(20), Some(40), true)
            .unwrap();
        assert!(range.iter().all(|m| m.body_text.as_deref() == Some("body")));
        assert_eq!(uids(range), [3, 2]);
        assert!(
            s.messages_between("Other", None, None, false)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn the_unread_count_follows_the_seen_flag() {
        let s = store_with_inbox();
        for uid in 1..=4 {
            s.insert_message(&msg("INBOX", uid, i64::from(uid)))
                .unwrap();
        }
        let unread = s.unread_count("INBOX").unwrap();
        s.update_flags("INBOX", 2, r"\Seen \Flagged").unwrap();
        assert_eq!(s.unread_count("INBOX").unwrap(), unread - 1);
        s.update_flags("INBOX", 2, r"\Flagged").unwrap();
        assert_eq!(s.unread_count("INBOX").unwrap(), unread);
        assert_eq!(s.unread_count("Archive").unwrap(), 0);
    }

    #[test]
    fn rules_uid_round_trips_and_survives_upsert() {
        let s = store_with_inbox();
        assert_eq!(s.rules_uid("INBOX").unwrap(), 0);
        assert_eq!(s.rules_uid("Nope").unwrap(), 0);
        s.set_rules_uid("INBOX", 77).unwrap();
        s.upsert_folder(&Folder {
            name: "INBOX".into(),
            uidvalidity: 1,
            last_uid: 500,
            special_use: None,
            delimiter: None,
        })
        .unwrap();
        assert_eq!(s.rules_uid("INBOX").unwrap(), 77);
    }

    #[test]
    fn mark_rules_seen_except_skips_listed_folders_and_never_lowers() {
        let s = store_with_inbox();
        for (name, last_uid) in [("INBOX", 50), ("Lists", 30), ("Old", 10)] {
            s.upsert_folder(&Folder {
                name: name.into(),
                uidvalidity: 1,
                last_uid,
                special_use: None,
                delimiter: None,
            })
            .unwrap();
        }
        s.set_rules_uid("Old", 20).unwrap();
        s.mark_rules_seen_except(&["INBOX".to_string()]).unwrap();
        assert_eq!(s.rules_uid("INBOX").unwrap(), 0);
        assert_eq!(s.rules_uid("Lists").unwrap(), 30);
        assert_eq!(s.rules_uid("Old").unwrap(), 20);
    }

    #[test]
    fn initial_uid_next_round_trips_and_survives_upsert() {
        let s = store_with_inbox();
        assert_eq!(s.initial_uid_next("INBOX").unwrap(), 0);
        assert_eq!(s.initial_uid_next("Nope").unwrap(), 0);
        s.set_initial_uid_next("INBOX", 1201).unwrap();
        s.upsert_folder(&Folder {
            name: "INBOX".into(),
            uidvalidity: 1,
            last_uid: 500,
            special_use: None,
            delimiter: None,
        })
        .unwrap();
        assert_eq!(s.initial_uid_next("INBOX").unwrap(), 1201);
    }

    #[test]
    fn uid_next_round_trips_and_survives_upsert() {
        let s = store_with_inbox();
        assert_eq!(s.uid_next("INBOX").unwrap(), 0);
        assert_eq!(s.uid_next("Nope").unwrap(), 0);
        s.set_uid_next("INBOX", 42).unwrap();
        s.upsert_folder(&Folder {
            name: "INBOX".into(),
            uidvalidity: 1,
            last_uid: 41,
            special_use: None,
            delimiter: None,
        })
        .unwrap();
        assert_eq!(s.uid_next("INBOX").unwrap(), 42);
    }

    /// uid 1 read, 2 unread, 3 flagged and read, 4 read with a multipart/mixed Content-Type folded onto a second line.
    fn filter_store() -> Store {
        let s = store_with_inbox();
        for (uid, flags) in [
            (1, "\\Seen"),
            (2, ""),
            (3, "\\Seen \\Flagged"),
            (4, "\\Seen"),
        ] {
            let mut m = msg("INBOX", uid, i64::from(uid));
            m.thread_id = format!("t{uid}");
            m.flags = flags.into();
            if uid == 4 {
                m.headers =
                    b"Subject: x\r\nContent-Type:\r\n Multipart/Mixed; boundary=b\r\n\r\n".to_vec();
            }
            s.insert_message(&m).unwrap();
        }
        s
    }

    #[test]
    fn messages_keep_only_what_the_filter_asks_for() {
        let s = filter_store();
        let uids = |filter: Filter| -> Vec<u32> {
            s.messages("INBOX", 10, filter)
                .unwrap()
                .iter()
                .map(|m| m.uid)
                .collect()
        };
        assert_eq!(uids(Filter::default()), [4, 3, 2, 1]);
        assert_eq!(
            uids(Filter {
                unread: true,
                ..Filter::default()
            }),
            [2]
        );
        assert_eq!(
            uids(Filter {
                flagged: true,
                ..Filter::default()
            }),
            [3]
        );
        assert_eq!(
            uids(Filter {
                attachments: true,
                ..Filter::default()
            }),
            [4]
        );
        assert!(
            uids(Filter {
                unread: true,
                flagged: true,
                ..Filter::default()
            })
            .is_empty()
        );
    }

    #[test]
    fn a_thread_matches_the_filter_when_any_member_does() {
        let s = filter_store();
        let mut reply = msg("INBOX", 5, 5);
        reply.thread_id = "t1".into();
        s.insert_message(&reply).unwrap();
        let unread = Filter {
            unread: true,
            ..Filter::default()
        };
        let summaries: Vec<String> = s
            .thread_summaries("INBOX", 10, unread)
            .unwrap()
            .into_iter()
            .map(|t| t.thread_id)
            .collect();
        assert_eq!(summaries, ["t1", "t2"]);
        let threads: Vec<usize> = s
            .threads("INBOX", 10, unread)
            .unwrap()
            .iter()
            .map(Vec::len)
            .collect();
        assert_eq!(threads, [2, 1], "a matching thread keeps all its members");
    }

    #[test]
    fn a_summary_previews_the_start_of_the_body_and_marks_an_attachment() {
        let s = store_with_inbox();
        let mut plain = msg("INBOX", 1, 10);
        plain.body_text = Some(format!("Hi Robin,\n\n{}", "word ".repeat(100)));
        let mut attached = msg("INBOX", 2, 20);
        attached.headers = b"Content-Type: multipart/mixed; boundary=b\r\n\r\n".to_vec();
        for m in [&plain, &attached] {
            s.insert_message(m).unwrap();
        }
        let threads = s.thread_summaries("INBOX", 10, Filter::default()).unwrap();
        let (attached, plain) = (&threads[0].latest, &threads[1].latest);
        assert!(
            plain.preview.starts_with("Hi Robin,\n\nword word"),
            "{:?}",
            plain.preview
        );
        assert_eq!(plain.preview.chars().count(), PREVIEW_CHARS);
        assert!(!plain.attachment);
        assert_eq!(attached.preview, "", "no body indexed yet");
        assert!(attached.attachment);
        let members = s.thread_members("INBOX", "m1@x").unwrap();
        assert_eq!(members[0].preview, plain.preview);
    }

    #[test]
    fn thread_summaries_describe_the_latest_message() {
        let s = store_with_inbox();
        let mut first = msg("INBOX", 1, 10);
        first.thread_id = "t1".into();
        first.flags = "\\Seen".into();
        let mut reply = msg("INBOX", 2, 30);
        reply.thread_id = "t1".into();
        reply.flags = "\\Flagged".into();
        let mut other = msg("INBOX", 3, 20);
        other.thread_id = "t2".into();
        other.flags = "\\Seen".into();
        for m in [&first, &reply, &other] {
            s.insert_message(m).unwrap();
        }
        let threads = s.thread_summaries("INBOX", 10, Filter::default()).unwrap();
        assert_eq!(
            threads,
            vec![
                ThreadSummary {
                    thread_id: "t1".into(),
                    latest: MessageSummary {
                        uid: 2,
                        from: "Alice <alice@x>".into(),
                        to: "bob@x".into(),
                        subject: "subject 2".into(),
                        date: 30,
                        flags: "\\Flagged".into(),
                        attachment: false,
                        preview: String::new(),
                    },
                    count: 2,
                    unread: true,
                    flagged: true,
                },
                ThreadSummary {
                    thread_id: "t2".into(),
                    latest: MessageSummary {
                        uid: 3,
                        from: "Alice <alice@x>".into(),
                        to: "bob@x".into(),
                        subject: "subject 3".into(),
                        date: 20,
                        flags: "\\Seen".into(),
                        attachment: false,
                        preview: String::new(),
                    },
                    count: 1,
                    unread: false,
                    flagged: false,
                },
            ]
        );
        assert_eq!(
            s.thread_summaries("INBOX", 1, Filter::default())
                .unwrap()
                .len(),
            1
        );
        let members: Vec<u32> = s
            .thread_members("INBOX", "t1")
            .unwrap()
            .iter()
            .map(|m| m.uid)
            .collect();
        assert_eq!(members, [1, 2]);
    }

    #[test]
    fn summaries_tolerate_missing_headers() {
        let s = store_with_inbox();
        let mut bare = msg("INBOX", 1, 10);
        bare.from_addr = None;
        bare.to_addr = None;
        bare.subject = None;
        s.insert_message(&bare).unwrap();
        let latest = &s.thread_summaries("INBOX", 10, Filter::default()).unwrap()[0].latest;
        assert_eq!((latest.from.as_str(), latest.subject.as_str()), ("", ""));
        assert!(!latest.is_seen() && !latest.is_flagged());
    }
}
