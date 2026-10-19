# Search — design

Status: draft for review, 2026-10-18. The task-by-task plan follows once the decisions below are settled.

## Goal

Search in Postvak should find the mail the user means, from every front end, as fast as they type:

- **Complete.** Every message's body is searchable locally, not only the ones a rule or the reader happened to download. The daemon builds that index in the account's idle time, on the connection it already holds.
- **Forgiving.** A typo, a prefix, a missing accent or a half-remembered name still finds the message, and says when a hit is a close match rather than an exact one.
- **Precise when asked.** `from:acme after:2026-01 has:attachment invoice` means the same thing on the CLI, over MCP and in the window.
- **Fast.** Under 10 ms for an exact query and under 30 ms with close matches at 100k messages, measured by the benchmark in `docs/src/perf.md`.

What exists: an FTS5 index over `subject`, `from_addr`, `to_addr` and `body_text` with the default tokenizer, filled by triggers (`migrations/001-initial`); `Store::search` with raw FTS5 syntax and a quoted-words retry; `search_headers` for MCP without `read:bodies`; a server search (`Command::SearchServer`, IMAP `SEARCH TEXT`) that the GUI runs on Enter and the CLI on `--server`; `--bodies` to download a folder first. Bodies are stored only on demand (`rules/apply.rs`, `FetchBody`, `FetchBodies`), so on most stores the index covers headers alone, which is the "dumb search" this design replaces.

## Decisions

| Decision | Choice | Rejected |
|---|---|---|
| Engine | SQLite FTS5, the index Postvak already has, with fuzzy matching built on `fts5vocab` | tantivy: native fuzzy queries and BM25, but a second index beside `mail.db` whose every insert, move and delete the engine must mirror by hand, a new ~1 MB dependency tree in `postvak-core`, and a rebuild path for drift. FTS5's external-content triggers keep the index consistent for free. |
| Fuzzy | Query terms are expanded with vocabulary terms within Damerau-Levenshtein distance 1 (2 for terms of 8+ characters), and the last word matches as a prefix | Trigram tokenizer: substring matching, but three times the index size and no typo tolerance; spellfix1 is not in the bundled SQLite |
| Ranking | Newest first, in two tiers: exact matches, then close matches | BM25 relevance order: mail users expect date order, as Gmail, Outlook and Apple Mail default to; relevance can come later as a sort option |
| What the indexer fetches | Text parts only, chosen from `BODYSTRUCTURE`, plus attachment names from it; never the raw message | Fetching whole messages: downloads every attachment to index a few KB of text, and either bloats `mail.db` with `raw` or throws the bytes away |
| Where the indexer runs | The account's sync thread, in slices between short IDLE waits, on its one connection | A second connection per account: faster, but doubles connections on hosts that cap them at two or three per user, and duplicates the connect, backoff and credential paths |
| Pacing | At most 5 s of fetching per slice, then an INBOX check and a 1 s IDLE; nothing while a command is queued or a pass runs | A fixed message count: a slice of 50 huge HTML newsletters and 50 one-liners differ by two orders of magnitude |
| Order | Newest first across all folders, Junk and Trash included | Skipping Junk and Trash: "the mail I deleted last week" is a common search |
| Auto-started daemon | Stays up while an account still has a backlog and indexed something in the last minute, at most two hours past the last client | Exiting 60 s after the last client: a CLI-only user would never get an index |
| Query language | A small operator set parsed in `postvak-core`, the same on every surface; raw FTS5 syntax goes | Keeping FTS5 syntax: column names leak into the UI (`from_addr:`), unbalanced quotes are errors, and `search_headers` already has to quote every word so a query cannot escape its column filter |
| Snippets | FTS5 `snippet()` for body hits, in the GUI row and the JSON row; over MCP only with `read:bodies` | Highlighting inside the open message: a reader feature, not a search one |
| Schema | One migration carries every FTS change (tokenizer, `cc_addr`, `attachment_names`, the vocab table), so the index is rebuilt once | A migration per PR: each FTS column change recreates the table and rebuilds it |

## 1. Index

Migration `011-search` replaces `messages_fts`:

```sql
ALTER TABLE messages ADD COLUMN attachment_names TEXT;   -- one name per line, from BODYSTRUCTURE
ALTER TABLE messages ADD COLUMN indexed_at INTEGER;      -- when the indexer last tried this message; NULL never
CREATE INDEX messages_unindexed ON messages (internaldate DESC) WHERE indexed_at IS NULL;

DROP TABLE messages_fts;  -- and its three triggers
CREATE VIRTUAL TABLE messages_fts USING fts5 (
  subject, from_addr, to_addr, cc_addr, attachment_names, body_text,
  content='messages', content_rowid='id',
  tokenize="unicode61 remove_diacritics 2"
);
-- the three external-content triggers again, now over the six columns
INSERT INTO messages_fts(messages_fts) VALUES ('rebuild');
CREATE VIRTUAL TABLE messages_vocab USING fts5vocab('messages_fts', 'col');
```

- `remove_diacritics 2` makes `cafe` find `café` and `Müller` find `Muller`. The default separators stay, so `pieter@example.nl` indexes as `pieter`, `example`, `nl`, and a pasted address searches as that phrase.
- The subject stays the first column: a plain word finds it, `subject:` narrows to it, and close matches and the last-word prefix apply to it as to every column. It is the decoded subject (`mail-parser` undoes RFC 2047), so `Rechnung` finds a subject sent as `=?utf-8?q?Rechnung?=`, and a search for `factuur` finds `Re: Fwd: factuur 2026-042`.
- `cc_addr` and `attachment_names` join the index; `from_addr` already holds display names (`Pieter Willekens <pieter@…>`), so a surname finds the sender.
- The rebuild runs inside the migration, in whichever process opens the store first. Today's stores hold few bodies, so it is a header rebuild: measure it at 100k in the benchmark before merging; the budget is 5 s. This is the one rebuild, which is why every FTS column change is in this migration.
- `messages_vocab` is a live view of the index's terms with per-column document counts. It costs nothing until queried.
- `has_attachment` (migration 008) stays the header guess for now; replacing it with `attachment_names IS NOT NULL` is a later change, since the names only exist once the indexer has passed.

## 2. The `search` module

New module `crates/postvak-core/src/search/`, the only code that builds FTS expressions. `Store::search` and `search_headers` become thin wrappers that take a parsed `Query`.

```rust
pub struct Query { terms: Vec<Term>, filters: Vec<Filter> }
pub enum Term { Word(String), Phrase(String), Not(Box<Term>) }
pub enum Filter { From(String), To(String), Subject(String), Attachment(String), In(String),
                  HasAttachment, Unread, Read, Flagged, Before(i64), After(i64), Account(String) }

pub fn parse(text: &str) -> Query                       // never fails: anything unparsable is a word
pub fn plan(q: &Query, scope: Scope, vocab: &dyn Vocab) -> Plan  // Scope::All or Scope::Headers (MCP without read:bodies)
pub struct Plan { exact: String, close: Option<String>, sql: Vec<Predicate>, params: Vec<Value> }
```

### Syntax

| Written | Means |
|---|---|
| `invoice` | the word, in any indexed column; the last word of a query also matches as a prefix (`inv` finds invoice) |
| `"status update"` | the phrase, exact, no prefix |
| `-newsletter` | without the word |
| `from:acme`, `to:me`, `cc:`, `subject:`, `attachment:` | the word in that column; `to:` covers To, Cc and Delivered-To; `to:me` and `from:me` use the identity's address and aliases |
| `in:Archive`, `in:"Projects/Postvak"` | one folder; `in:anywhere` is the default |
| `has:attachment`, `is:unread`, `is:read`, `is:flagged` | the flag or guess |
| `before:2026-01-15`, `after:2026-01-01`, `after:7d`, `after:3w`, `after:1y` | by `internaldate`, dates in local time |
| `account:work` | one account, on surfaces that search several |

Everything else is a word. There are no operators to balance, so `parse` cannot fail and no query is ever retried in another shape. FTS5 syntax never reaches SQLite from user text: every word and phrase is emitted as a quoted FTS5 string, which closes the column-filter escape `search_headers` guards against today. `in:`, `is:`, `has:`, `before:`, `after:` and `account:` become SQL predicates on `messages`, not FTS filters.

### Close matches

For each `Word` of four or more characters, the planner asks the vocabulary for candidates: terms of length n−1 to n+1 sharing the first character, counted either in a header column or in at least three documents (the body vocabulary is full of tracking tokens). Candidates within the distance are kept, at most five per word, by document count. The `close` expression is the exact one with each such word replaced by `(word OR cand1 OR …)`; it is `None` when no word gained a candidate. A quoted phrase, a filter value and a negated word are never expanded.

Search runs `exact` first, newest first, up to `limit`. If that returns fewer than `limit`, `close` runs for the remainder, excluding the ids already found. Each returned message carries `match: exact | close`. The vocabulary lookup is the one cost that is new per keystroke; at 100k messages the benchmark must show the whole query under 30 ms, else the length and first-character prefilter tightens, or expansion waits until the user pauses typing.

`Vocab` is a trait so the planner's tests use a fixed term list; the store's implementation queries `messages_vocab`.

### Scopes

`Scope::Headers` wraps the whole expression in the column filter `{subject from_addr to_addr cc_addr attachment_names}`, so MCP without `read:bodies` keeps the guarantee it has today: no body text decides a result. Snippets are never built in that scope.

### Snippets

With `Scope::All`, a hit whose match is in `body_text` gets `snippet(messages_fts, 5, '', '', '…', 12)`; a hit matched by its headers gets none, since the row already shows them. The snippet is computed in the same query, cleaned like every output string.

## 3. Indexing in idle time

### What is fetched

`MailOps` gains:

```rust
/// The message's structure: its text parts with their encodings and charsets, and its attachment names.
fn fetch_structure(&mut self, uid: u32) -> MailResult<Structure>;
/// The decoded-as-sent bytes of one part, e.g. "1.2"; the caller decodes the transfer encoding.
fn fetch_part(&mut self, uid: u32, section: &str) -> MailResult<Vec<u8>>;
```

`message::text_from_parts` wraps each fetched part in a one-part message (`Content-Type` with its charset, `Content-Transfer-Encoding`) and hands it to `mail-parser`, so the decoding path is the one `body_text` already uses; `text/plain` wins over `text/html` inside a `multipart/alternative`, as it does there. A message whose structure has no text part, or whose structure the server returns unparsably, falls back to `fetch_raw` when its size is at most 2 MB, and is otherwise indexed from headers only. The indexer writes `body_text`, `attachment_names` and `indexed_at`, and never `raw`: `raw` stays what the reader, `show`, attachments and the trash backup fetch on demand through `Client::raw_message`, which already decides on `raw` alone.

`RecordingOps` gets a fake structure per message so the slice logic is tested without a server; the live Dovecot tests cover real `BODYSTRUCTURE` on both ports: `multipart/alternative`, `multipart/mixed` with named attachments, a base64 `text/html` part in ISO-8859-1, a message with no text part.

### The loop

`run_session` gains a backlog state. After a pass and after the command queue is drained, while the account has a backlog and `index_bodies` is on:

1. **Slice.** Take messages `WHERE indexed_at IS NULL ORDER BY internaldate DESC`, folder by folder as they come, and index them for at most 5 s of wall time, in one transaction per folder visited. A failure marks `indexed_at` and leaves `body_text` NULL, so the message is retried a day later, and at once if the user opens it. A lost connection ends the session as it does elsewhere.
2. **INBOX check.** `SELECT INBOX`; when `EXISTS` differs from the stored row count, sync INBOX and run rules, as a push would. Fetching in another folder is blind to pushes, so this bounds the delay of new mail to one slice.
3. **IDLE for 1 s.** A push or a queued command interrupts it at once and is served before the next slice. On its timeout the next slice runs, unless the full-sync interval has passed, in which case the full pass runs first.

With no backlog the loop is today's loop, unchanged. `Activity::Indexing { done, total }` is sent at the start of each slice and `Activity::Idle` when the backlog empties; `Event::Indexed { account, count }` after each slice lets a window with an open search reload it, at most once a second. The status line reads "up to date · indexing 12,345 of 48,210".

Rules that need bodies keep their own fetch (`MAX_RULE_BODY`), which now finds `body_text` already present for everything the indexer passed.

### Config

`[[accounts]] index_bodies = true` (default). Off, the account behaves as today: headers are indexed, bodies on demand, `--server` and Enter reach the rest. No size or age bound in this version; `index_after = "2y"` is the obvious later addition and the config name is reserved for it.

### Daemon lifetime

`daemon::Options::idle_exit` gains a hold: the auto-started daemon does not exit while any account reports `Indexing` and advanced its count within the last minute, for at most two hours after the last client left. `postvak daemon status` shows the backlog per account, so a user who wants it done sooner installs the service.

## 4. Surfaces

### CLI

```
postvak search QUERY [--account NAME] [--folder NAME] [--exact] [--server] [--limit 50] [--json]
```

- `QUERY` is the syntax above; `--folder` is `in:`, `--account` is `account:`. `--exact` skips close matches. `--bodies` goes: the indexer makes it redundant, and `--server` stays for mail the index has not reached yet. Each plain-text line of a close match starts with `~`; `--json` rows gain `"match"` and, for body hits, `"snippet"`.
- `postvak status` and `daemon status --json` show `indexed` and `total` per account.
- `help::SEARCH` describes the syntax in one line and points at the docs.

### MCP

- `search` keeps its arguments, gains `exact: bool`, and its rows gain `match`. With `read:bodies` rows may carry `snippet`; without it the planner runs in `Scope::Headers` and no row has one. `server` keeps needing `read:bodies`. The tool description carries the operator table, from `help`.
- `status` rows gain `indexed` and `total`.
- `backend.rs` stays the only MCP code that touches the store; it calls `search::parse` and `Store::search(&query, scope, limit)`.

### GUI

- The field accepts the same syntax. Rows are the exact tier first, then a divider row "Close matches" and the close tier, in `list.rs`; a close row's second line shows the snippet when there is one, else the subject as today.
- The line under the field says "Enter also searches the server…" only while the covered accounts still have a backlog (`Store::backlog() > 0`); afterwards it says how many messages the search covered.
- `Event::Indexed` while a search is open reloads it on the load thread, throttled to once a second, keeping the cursor on its row.
- The status line and its hover show the indexing count; the Activity view logs when an account's backlog started and emptied.
- Snapshots: a search with close matches and a snippet, light and dark; the status line while indexing.

### Docs

`docs/src/cli.md` blessed; a new `docs/src/search.md` with the syntax table, how close matches work, what the indexer fetches and stores, `index_bodies`, and why the first index takes a while; `gui.md` keys row for `/` and the status line; `mcp.md` scope table and `search`; `accounts.md` option table; `agent-guide.md` replaces the FTS5 example with `postvak search 'from:acme invoice' --json`; `perf.md` gains the new benchmark rows.

## 5. Privacy

- Body text already sits in `mail.db` for every fetched message; the indexer extends that to every message of an account with `index_bodies` on. The file keeps its 0700 directory. `search.md` says so.
- Queries are never logged. A failed search logs the error without the query; FTS5 errors cannot echo it anyway, since user text only ever reaches SQLite as bound parameters and quoted strings.
- Snippets are body text and follow its rules: `<untrusted_mail_content>` wrapping is not applied to a one-line snippet, so over MCP the row's description marks `snippet` as stranger-written data, as `subject` is already; the tag rewrite and invisible-character cleaning apply as to every string.
- Attachment names are sender-written; they already reach every surface through `attachments`.

## 6. Errors

- The daemon runs `integrity-check` on each store once per session, before the first slice; a failure runs `rebuild` once and logs it. A second failure shows in the status line and `postvak status` as "search index broken", and the account's indexing stops until the next session. No new command: a rebuild that fails twice is a bug report.
- A server that rejects `BODYSTRUCTURE` or part fetches for a folder marks that folder's messages indexed with headers only and logs once per folder per session.
- The indexer never fails a pass: an error in a slice is logged, the slice ends, the loop continues with IDLE.

## 7. Testing

- `search::parse`: table-driven over every operator, quoting, negation, dates and relative dates, unknown operators as words, empty and whitespace queries; property test in `tests/fuzz.rs` that any string parses and plans into an expression FTS5 accepts.
- `search::plan`: tiers, prefix on the last word only, no expansion of phrases, filter values and negations, candidate limits, the `Headers` scope column filter, and that `"x) OR body_text:secret"` stays a phrase.
- Store: the migration rebuilds an index that then passes `integrity-check` and finds a body indexed before it; diacritics; `cc_addr` and `attachment_names` hits; `match` values and the exclusion of exact ids from the close tier; snippets only for body hits; `backlog()`.
- Sync, with `RecordingOps`: a slice stops at its time budget (clock injected) and at a queued command; newest first across folders; a failure sets `indexed_at` without `body_text` and is retried after a day; headers-only above 2 MB without a text part; `Indexing` then `Idle` activities; the INBOX check syncs new mail between slices; `index_bodies = false` indexes nothing. `tests/sync_model.rs` gains: after enough idle slices every message is indexed and `integrity-check` passes.
- Daemon: idle exit held while an account indexes, released when it stops advancing, hard stop at two hours.
- Live: the four `BODYSTRUCTURE` cases above on both Dovecot ports.
- GUI: divider and snippet rows, hint text with and without a backlog, reload on `Event::Indexed` keeps the cursor; snapshots.
- CLI black box: `search 'from:alice'` on a fixture store, `~` prefix on a close match, `--exact`, `--json` fields.
- MCP: `search` without `read:bodies` never returns `snippet` and never matches on a body word; the operator table is in the description; `tools_list_per_scope_set_is_exact_and_alphabetical` unchanged.
- Benchmark: `search` at 10k and 100k with an exact word, a misspelled word and a `from:` filter; a new `index_slice` phase (messages indexed per second against `RecordingOps`); the migration's rebuild time at 100k.

## 8. Sequencing

Five PRs, each leaving `main` releasable. 2 and 3 are independent and can run as parallel branches; the rest stack.

1. `feat(store): rebuild the search index with diacritics folding, cc and attachment columns` — migration 011, the `search` module with `parse` and `plan` for words and phrases only (no operators, no close matches yet), prefix on the last word, `Store::search(&Query, Scope, limit)` replacing the string API, the quoted-words retry removed. The surfaces change only in that a query can no longer fail to parse.
2. `feat(sync): index message bodies in idle time` — `fetch_structure`, `fetch_part`, `text_from_parts`, the backlog loop, `Indexing`, `Event::Indexed`, `index_bodies`, the daemon hold, status on all three surfaces, `--bodies` removed, `search.md`.
3. `feat(search): close matches` — the vocabulary, expansion, tiers, `match` on every surface, `--exact`/`exact`, the GUI divider.
4. `feat(search): operators` — the filter set, `from:me`, dates, `account:`, the agent guide, the MCP description table.
5. `feat(search): snippets` — the GUI second line and the JSON field, with the MCP scope rule.

## Out of scope

- Relevance ordering (`--sort relevance` over BM25 with column weights); the column weights are easy to add once date order proves insufficient.
- Indexing attachment contents (PDF, Office text): needs the attachment bytes the indexer deliberately does not fetch, and a set of extractors; the strongest candidate for the next search spec.
- Semantic or embedding search.
- Replacing `has_attachment` with the indexed names.
- A bound on how far back the indexer goes (`index_after`), metered-connection or battery awareness.
- Saved searches and search folders.
- Trigram substring matching inside tokens (`studios` in `interactivestudios`); revisit if close matches and prefixes leave real gaps.
