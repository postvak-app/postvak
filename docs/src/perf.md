# Performance

How Postvak behaves as a mailbox grows, measured on generated mail. User stores are never used.

## The benchmark

`src/gui/bench.rs` builds three accounts with six folders each (INBOX holds half of an account's mail, Archive a quarter). The mail comes in threads: 60% single messages, then short threads, then a few with up to 60 replies, all with real `In-Reply-To` and `References` headers. About 10% is unread and 3% flagged. A fixed seed makes every run produce the same mail. The mail goes into SQLite through the real sync code, against `RecordingOps`, and then each phase is timed:

| Phase | What it times |
| --- | --- |
| `first_sync` | Syncing every account into empty stores, once |
| `open_store` | Opening one account's store |
| `open_window` | Building the mail window's state over all accounts, with its first list |
| `thread_grouping` | `thread_summaries` on one INBOX: the query that groups the list into threads |
| `folder_list` | Reloading one account's INBOX list: the query plus the rows |
| `all_inboxes` | Reloading All inboxes: every account's INBOX, merged |
| `search` | One word searched in All inboxes |
| `sync_pass` | One sync of an account after 200 messages arrived and 500 changed flags |

```sh
POSTVAK_BENCH_SCALE=10000 cargo test --release --lib gui::bench::bench -- --ignored --nocapture
```

`POSTVAK_BENCH_SCALE` sets the total number of messages over all accounts (10,000 by default). Each phase prints one JSON line with the median, minimum and maximum of five runs (`first_sync` runs once). `cargo test` runs the same code at 600 messages as a plain test, so the benchmark keeps compiling and working.

## Method

- Release build (`--release`), on a machine with nothing else running.
- Before and after an optimisation, run both builds several times in turn and compare medians and ranges, not one run.
- State the machine and the scale with each table.
- An optimisation shows that the output did not change: the same rows, the same hits.

## Baseline

Release build of `main` at 78938b8, Apple M1 Pro, 32 GB, macOS 27.0.1, Rust 1.99.0. Median in ms, with the range of five runs.

| Phase | 1k | 10k | 100k |
| --- | --- | --- | --- |
| `first_sync` | 31 | 390 | 3,410 |
| `open_store` | 0.14 (0.14–0.30) | 0.15 (0.14–0.34) | 0.38 (0.18–0.62) |
| `open_window` | 1.7 (1.5–3.4) | 8.5 (8.1–14.7) | 76 (68–90) |
| `thread_grouping` | 0.16 (0.15–0.20) | 1.5 (1.5–2.2) | 27 (26–34) |
| `folder_list` | 0.16 (0.16–0.18) | 1.6 (1.5–1.8) | 29 (27–31) |
| `all_inboxes` | 0.57 (0.52–0.61) | 5.3 (5.2–5.6) | 87 (82–103) |
| `search` | 0.65 (0.64–0.69) | 5.9 (5.6–6.2) | 40 (35–46) |
| `sync_pass` | 6.5 (5.7–7.6) | 11 (7.4–21) | 21 (20–31) |

What this shows (not yet issues; nothing was optimised here):

- No phase grows faster than the mailbox; all of them scale about linearly.
- A list reload runs `thread_summaries`, which groups every message in the folder, however few rows fit on screen. At 100k messages (about 17k per INBOX) All inboxes takes 87 ms. The window reloads the list when sync reports changes, so with big inboxes a frame can stall on each sync event.
- `open_window` grows with the mail too, so the window opens more slowly on big mailboxes. It includes the first list load and per-folder counts.
- `sync_pass` includes the fake server's own work: `RecordingOps` scans each folder on every select. A real server's time is not in these numbers, and the fake's is.

Not measured yet: drawing a frame of the list through the real window (`Fixture::window()`), and opening one message.
