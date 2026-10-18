# Performance

How Postvak behaves as a mailbox grows, measured on generated mail. User stores are never used.

## The benchmark

`crates/postvak-gui/src/bench.rs` builds three accounts with six folders each (INBOX holds half of an account's mail, Archive a quarter). The mail comes in threads: 60% single messages, then short threads, then a few with up to 60 replies, all with real `In-Reply-To` and `References` headers. About 10% is unread and 3% flagged. A fixed seed makes every run produce the same mail. The mail goes into SQLite through the real sync code, against `RecordingOps`, and then each phase is timed:

| Phase | What it times |
| --- | --- |
| `first_sync` | Syncing every account into empty stores, once |
| `open_store` | Opening one account's store |
| `open_window` | Building the mail window's state over all accounts, with its first list |
| `thread_grouping` | `thread_summaries` on one INBOX: the query that groups the list into threads |
| `folder_list` | Reloading one account's INBOX list: the query plus the rows |
| `all_inboxes` | Reloading All inboxes: every account's INBOX, merged |
| `sync_event` | The frame that takes a sync event in All inboxes: how long it waits for the reload |
| `folder_counts` | One account's folders with their unread counts, read again on each of its sync events |
| `search` | One word searched in All inboxes |
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

## Reloads after a sync event

The window reloads the shown list whenever sync reports changes. That reload runs on a thread of its own with its own store connections; the frame waits for it at most 2 ms (`LIST_BUDGET` in `state.rs`, a quarter of a 120 Hz frame), so a small mailbox's rows still show in the same frame, and a large one's show when the thread wakes the window. While a reload runs, the next waits for it and then starts once; the rows already loaded are rebuilt meanwhile only when edits changed, since rebuilding a large list costs milliseconds. A view change reads on the window thread, as before, and drops a reload still running.

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

Not measured yet: drawing a frame of the list through the real window (`Fixture::window()`), and opening one message.
