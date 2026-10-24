# Performance

How Postvak behaves as a mailbox grows, measured on generated mail. User stores are never used.

## The benchmark

`crates/postvak-gui/src/bench.rs` builds three accounts with six folders each (INBOX holds half of an account's mail, Archive a quarter). The mail comes in threads: 60% single messages, then short threads, then a few with up to 60 replies, all with real `In-Reply-To` and `References` headers. About 10% is unread and 3% flagged. A fixed seed makes every run produce the same mail. The mail goes into SQLite through the real sync code, against `RecordingOps`, and then each phase is timed:

| Phase | What it times |
| --- | --- |
| `first_sync` | Syncing every account into empty stores, once |
| `open_store` | Opening one account's store |
| `rebuild_index` | Rebuilding every account's search index, what migration 011 does once per store when it opens |
| `index_all` | Indexing every message of one account from the fake server, as the daemon does in idle time: structure, text parts and the store's writes, without a network |
| `open_window` | Building the mail window's state over all accounts, with its first list |
| `thread_grouping` | `thread_summaries` on one INBOX: the query that groups the list into threads |
| `folder_list` | Reloading one account's INBOX list: the query plus the rows |
| `all_inboxes` | Reloading All inboxes: every account's INBOX, merged |
| `sync_event` | The frame that takes a sync event in All inboxes: how long it waits for the reload |
| `folder_counts` | One account's folders with their unread counts, read again on each of its sync events |
| `search` | One word searched in All inboxes |
| `search_keystroke` | The frame that takes a keystroke in the search field: how long it waits for the hits |
| `search_close` | A misspelled word searched in All inboxes: no exact hit, so the vocabulary is consulted and the close tier runs |
| `sync_pass` | One sync of an account after 200 messages arrived and 500 changed flags |

```sh
POSTVAK_BENCH_SCALE=10000 cargo test --release -p postvak-gui --lib bench::bench -- --ignored --nocapture
```

`POSTVAK_BENCH_SCALE` sets the total number of messages over all accounts (10,000 by default). Each phase prints one JSON line with the median, minimum and maximum of five runs (`first_sync` runs once). `cargo test` runs the same code at 600 messages as a plain test, so the benchmark keeps compiling and working.

## Method

- Release build (`--release`), on a machine with nothing else running.
- Before and after an optimisation, run both builds several times in turn and compare medians and ranges, not one run.
- State the machine and the scale with each table.
- An optimisation shows that the output did not change: the same rows, the same hits.

## Baseline

Release build of `main` at 783a0f2. Apple M1 Pro, 32 GB, macOS 27.0.1, Rust 1.99.0. Median in ms, with the range of five runs.

| Phase | 1k | 10k | 100k |
| --- | --- | --- | --- |
| `first_sync` | 32 | 298 | 3,316 |
| `open_store` | 0.15 (0.14–0.33) | 0.14 (0.13–0.34) | 0.15 (0.14–0.37) |
| `open_window` | 1.1 (1.0–2.5) | 2.9 (2.8–4.5) | 24 (23–25) |
| `thread_grouping` | 0.12 (0.12–0.15) | 1.1 (1.1–1.2) | 12 (11–13) |
| `folder_list` | 0.12 (0.12–0.13) | 1.1 (1.1–1.1) | 12 (11–12) |
| `all_inboxes` | 0.37 (0.37–0.39) | 3.5 (3.4–3.8) | 33 (33–34) |
| `search` | 0.64 (0.62–0.73) | 5.1 (5.0–5.2) | 35 (34–35) |
| `sync_pass` | 6.5 (5.8–6.6) | 8.9 (7.8–16) | 22 (19–39) |

What this shows:

- No phase grows faster than the mailbox; all of them scale about linearly.
- A list reload runs `thread_summaries`, which groups every message in the folder, however few rows fit on screen. At 100k messages (about 17k per INBOX) All inboxes takes 33 ms, two frames at 60 Hz. A view change still waits for it; a reload after a sync event no longer does (below).
- `open_window` grows with the mail too, so the window opens more slowly on big mailboxes. It includes the first list load and per-folder counts.
- `sync_pass` includes the fake server's own work: `RecordingOps` scans each folder on every select. A real server's time is not in these numbers, and the fake's is.

## The search index

Migration 011 rebuilds the FTS index once per store, the first time the store opens after the upgrade; `rebuild_index` times that rebuild for all three stores together. Release build: Linux container, 4 vCPUs of an Intel Xeon at 2.80 GHz, Rust 1.99.1; another build was running during the 10k run. Median in ms, with the range of five runs.

| Phase | 10k | 100k |
| --- | --- | --- |
| `rebuild_index` | 30 (26–31) | 235 (219–251) |
| `search` | 10 (9.9–23) | 62 (60–70) |
| `search_close` | 12 (12–14) | |
| `index_all` | 977 (918–1,117) | |

`index_all` covers the first account's third of the mail, so about 3,400 messages a second go through the structure fetch, the text decode and the store; against a real server each message costs a round trip or two on top, which is what the five-second slices and the newest-first order are for. Rebuilding three stores that hold 100k header-only messages takes a quarter of a second, far under the five seconds the search design allows; stored bodies add to it in proportion to their text. `search` runs its last word as a prefix now (`invoice*`), which costs more than the exact word did, and this machine is not the baseline's M1, so the two `search` rows are not comparable. `search_close` searches `invioce`: the exact tier finds nothing, the vocabulary lookup and the close tier run, and the whole query still costs what an exact one does (the `search` row of the same run was 12 ms).

## Reloads after a sync event

The window reloads the shown list whenever sync reports changes. That reload runs on a thread of its own with its own store connections; the frame waits for it at most 2 ms (`LIST_BUDGET` in `state.rs`, a quarter of a 120 Hz frame), so a small mailbox's rows still show in the same frame, and a large one's show when the thread wakes the window. While a reload runs, the next waits for it and then starts once; the rows already loaded are rebuilt meanwhile only when edits changed, since rebuilding a large list costs milliseconds. A view change reads on the window thread, as before, and drops a reload still running; a keystroke in the search field does not (below).

An archive, move or delete hides its rows at once and its `ActionDone` arrives after the daemon changed the store. A reload that started before then still holds the old rows, so the edit stays on the rows until a reload that started after it is applied.

The same sync event also reads the account's folders and unread counts again, on the window thread. Each count reads the partial index `messages_unread` (migration 010), which holds only unread mail; the covering folder index SQLite chose before read every message in the folder. The query names the index (`INDEXED BY`), since without statistics SQLite does not know it is the smaller one, and a test checks that the plan uses it.

`all_inboxes` is what the frame that took a sync event waited for the list before; `sync_event` is what it waits now. Release build, the same run of each: Linux container, 4 vCPUs of an Intel Xeon at 2.10 GHz, 15 GB, Rust 1.99.1, shared (load average about 3), so the ranges are wider than on a quiet machine. Median in ms, with the range of five runs.

| Phase | 10k | 100k |
| --- | --- | --- |
| `all_inboxes` | 6.6 (6.0–7.1) | 68 (60–81) |
| `sync_event` | 2.2 (2.2–2.3) | 2.3 (2.2–2.8) |
| `folder_counts` before migration 010 | 0.42 (0.41–0.77) | 5.8 (4.0–9.9) |
| `folder_counts` | 0.06 (0.06–0.22) | 0.17 (0.15–0.39) |

The frame now waits the budget at most for the list, however large the mailbox, and the counts no longer grow with the read mail. `open_window`, which reads the counts too, went from 48 to 28 ms at 100k.

## Typing a search

Each keystroke in the search field used to run the search on the window thread, for every account, so the frame that took the key waited for the whole query. Now a keystroke starts the same reload a sync event does: on a thread of its own, with the frame waiting at most `LIST_BUDGET`. While a search runs, further keystrokes only mark the list stale, and one search follows for the latest text, so fast typing never queues a search per key. The rows of the last query stay until the new hits come, and those show from the top.

`search` is what the frame that took a keystroke waited before; `search_keystroke` is what it waits now. Release build, the same run of each: Linux container, 4 vCPUs of an Intel Xeon at 2.30 GHz, 15 GB, Rust 1.99.1. Both scales searched every account's bodies, as `index_all` ran first. Median in ms, with the range of five runs.

| Phase | 10k | 100k |
| --- | --- | --- |
| `search` | 11 (11–13) | 88 (75–110) |
| `search_keystroke` | 2.2 (2.1–2.2) | 2.1 (2.1–2.2) |

The hits still take what `search` takes to arrive; only the window no longer waits for them.

Not measured yet: drawing a frame of the list through the real window (`Fixture::window()`), and opening one message.
