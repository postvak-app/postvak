# Paging, field selection and counts for `list` and `search` — design

Issue #353.

## Goal

An agent that reads a large mailbox through `list` or `search` gets a few hundred rows and no way to read on, every row with every key, and no cheap way to ask "how many?". This adds three things to both commands, on the CLI and over MCP together:

- a **cursor** that continues where a page ended;
- **fields** that keep only the keys asked for in each row;
- **count_only**, which returns how many rows there are without them.

## Decisions

| Question | Chosen | Not chosen |
|---|---|---|
| Cursor | Keyset: the last row's sort key, opaque to the caller | An offset: rows that arrive between pages shift it, so a page repeats or skips rows |
| Order across accounts | One order over every selected account, newest first; `limit` counts rows of all accounts together | Per account, `limit` each: a cursor would have to carry a position per account and a page would not be newest first |
| Ties | `(internaldate, account, uid, folder)` descending; every part is in the cursor | Date alone: equal timestamps (a bulk import, a copied message) would repeat or vanish at a page edge |
| JSON lines | A last `{"next": "…"}` line only when more rows follow; the rows stay one message per line | An envelope object: it breaks the one-object-per-line contract scripts already parse |
| Fields | A list of row keys, plus `compact`; `account` always stays; an unknown name is an error that lists the known ones | Silently dropping unknown names: a typo would look like an empty column |
| Count | `{"count": N}` alone, the total matching; takes no cursor or fields | A count beside every page: it costs a second query per page |
| `search --server` | One page, no cursor and no count: the server's hits cannot be positioned in the local order on a later page | Paging server hits: needs state the cursor does not carry |
| GUI | Exception: the window already loads its list lazily (`gui/rows`) and shows the folder's counts, so nothing changes there | — |

## 1. The order and the cursor

`postvak-core::page` holds the paging for both front ends, so a cursor the CLI printed works over MCP and back.

- A message row sorts by `(internaldate, account, uid, folder)` descending. The store reads each account's rows with `ORDER BY internaldate DESC, uid DESC, folder DESC` from the position the cursor gives it, at most `limit + 1`; `page` merges the accounts, keeps `limit`, and returns a `next` only when a row is left over.
- An account other than the cursor's starts at the cursor's date: one that sorts below the cursor's account takes that date's rows too, one above it only older ones. So a page boundary inside a run of equal timestamps across accounts neither skips nor repeats.
- In a thread view a thread sorts by its latest message's date, then account, then its highest uid, which is unique in a folder; `limit` counts threads, and each comes with all its messages. A thread that gets new mail between pages moves to the top, so a later page does not show it again. One that loses its newest message between pages (moved, deleted, or no longer unread or flagged under a filter) now sorts by an older date, which can put it behind the cursor, so a later page shows it again: the cursor holds one position, not every thread shown, and remembering them all would grow it with every page.
- In a search, every exact match of every account comes before every close one, and the tier is in the cursor. Whether an account's close tier shows is decided as before, on the whole exact tier (fewer than ten matches). Once a page showed an account's close matches, the cursor names that account, and its close matches keep showing on later pages even when exact matches arrive meanwhile; every other account decides again on its exact tier. A page with server hits orders by tier before it cuts to `limit`, so no exact match gives way to a newer close one. A count includes the close matches exactly when they would show.
- The cursor is URL-safe base64 of a small JSON object: what made it (`list`, threads or `search`), the tier, date, account, uid and folder, and in a search the accounts whose close matches showed. It is opaque to callers; one from a different command or kind is refused with "the cursor is not one this command returned; start again without it". The arguments other than `cursor` must be the same on every page; the cursor does not check them.
- A message that arrives after the first page sorts above the cursor, so it is not shown on a later page; a new first page shows it.

## 2. Fields

`fields` names the keys of a message row to keep: `account`, `folder`, `uid`, `message_id`, `from_addr`, `to_addr`, `cc_addr`, `delivered_to`, `in_reply_to`, `refs`, `thread_id`, `subject`, `date`, `internaldate`, `flags`, `size`, plus `depth` in a thread view and `match` and `snippet` in a search. `compact` stands for `account, folder, uid, date, from_addr, subject, flags`, plus `depth` or `match`. `account` is kept whatever is asked, as the agent guide promises every row has it. A name outside the list fails with `unknown field '…'; fields are compact, account, …`. `output::Fields` does this for both front ends.

## 3. Surfaces

### CLI

```
postvak list   [...] [--cursor NEXT] [--fields a,b,…] [--count-only]
postvak search QUERY [...] [--cursor NEXT] [--fields a,b,…] [--count-only]
```

- `--fields` requires `--json`. `--count-only` conflicts with `--cursor` and `--fields`, and in `search` with `--server`, which also conflicts with `--cursor`.
- With `--json` the rows are one object per line as before; when more follow, a last line `{"next": "…"}`. Without `--json`, the rows go to stdout and `more: --cursor …` to stderr.
- `--count-only` prints the number, or `{"count": N}` with `--json`.
- `--limit` now counts rows of every account together, where it counted per account before.

### MCP

- `list` and `search` take `cursor`, `fields` and `count_only`. A page returns `{"rows": [...], "next": "…"}`, with `next` only when more follow; `count_only` returns `{"count": N}`. `limit` stays at most 500.
- `count_only` with `cursor` or `fields`, or a `search` count with `server`, is an error.
- `backend.rs` stays the only MCP code that touches the store; the tools name `page::ListRequest` through it, as they do `Filter`.

### GUI

None; see Decisions.

### Docs

`docs/src/agent-guide.md` (the JSON-lines rule and a paging bullet), `docs/src/mcp.md` (`list` and `search`), `docs/src/search.md` (paging and close matches), `docs/src/cli.md` blessed.

## 4. Testing

- `page`: equal timestamps across two accounts page without skip or repeat at every page size; accounts merge newest first; a message arriving between pages does not shift the next; threads page by thread with every member; a cursor works only for the kind that made it, and garbage is refused; a search pages through both tiers and folders that share a uid, at every page size, with the same rows as one big page; the close tier stays away on every page once the exact tier holds ten; an account whose close matches a page showed keeps them when exact matches arrive between pages, and a close cursor shows none of an account with ten exact matches; server hits end paging.
- `output`: the field list names every key of a message row; fields keep what was asked and always `account`; `compact` per row kind; unknown names fail.
- MCP and CLI: two pages over two accounts, `next` absent on the last, `count_only`, `fields`, an unknown field and a bad cursor.
