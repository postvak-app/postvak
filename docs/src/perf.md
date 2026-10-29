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
| `open_message` | Reading a 5 MB message from the store and parsing it, as the reader thread does |
| `open_message_frame` | The frame that opens that message: how long it waits for the read |
| `search_close` | A misspelled word searched in All inboxes: no exact hit, so the vocabulary is consulted and the close tier runs |
| `sync_pass` | One sync of an account after 200 messages arrived and 500 changed flags |
| `rules_read_with_body` | Reading one INBOX whole, with 4 kB of indexed body text per message, as the rules pass did for every folder |
| `rules_read` | Reading the same INBOX without the body text, as the rules pass does for a folder whose rules test no body |

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

## The rules pass

Rules look at every message of each folder they watch, so the pass after each new mail, IDLE push or move reads those folders whole. It read every column, and since bodies are indexed in idle time (`body_text`), that included each message's text; the text sits behind the stored raw message in the row, so reading it also walked the raw message's overflow pages wherever one was stored. A folder none of whose rules tests the body is now read without the text and the index columns (`Store::messages_without_body`), and the check for new mail to notify about reads only INBOX rows above the last notified uid.

Release build, the same machine as the search table, 100k messages over three accounts; the INBOX read holds about 16.7k messages. Median in ms, with the range of five runs.

| Phase | 100k |
| --- | --- |
| `rules_read_with_body` | 89 (82–148) |
| `rules_read` | 35 (33–36) |

`sync_pass` did not move (37 ms against 40): `RecordingOps` scans each folder on every select, and that dominates it.

## The daemon

- **Connecting.** The accept loop slept 200 ms whenever no client was waiting, so every CLI command, MCP tool call and window start waited up to 200 ms before the daemon read its hello. A thread of its own now blocks in `accept` and hands each connection over at once; the loop still wakes every 200 ms for its stop, idle and file checks.
- **Actions on many messages.** Archiving, moving, flagging or deleting chosen messages sent one IMAP command per message: 200 archived messages were 200 round trips. They now go as one `UID MOVE`, `UID STORE` or `UID EXPUNGE` per batch of up to 500 uids, written as ranges where they run (`1:3,7`). A delete still downloads each message it has no copy of, for its `.eml` backup, before expunging them together. Junk and not-junk, which also swap keywords, still run per message.
- **The index backlog.** The indexer counts what is left before each slice, and kept doing so on every idle turn once all was indexed; the count read every row, behind each stored raw message. It now reads two partial indexes, `messages_unindexed` and `messages_retry` (migration 013), which are empty once indexing is done.
- **Reading the backlog.** Each slice then read its candidates with one condition over both halves of the backlog, which no index serves: every row, sorted, for the newest 200, once per five-second slice for as long as a large store is being indexed. The two halves are now read apart, each from its partial index, which migration 014 orders as the indexer takes them (`internaldate DESC, uid DESC`), so a slice reads its candidates and no more. On 60k messages in SQLite directly, the read fell from 150 ms to 2 ms.

## Reloads after a sync event

The window reloads the shown list whenever sync reports changes. That reload runs on a thread of its own with its own store connections; the frame waits for it at most 2 ms (`LIST_BUDGET` in `state.rs`, a quarter of a 120 Hz frame), so a small mailbox's rows still show in the same frame, and a large one's show when the thread wakes the window. While a reload runs, the next waits for it and then starts once; the rows already loaded are rebuilt meanwhile only when edits changed, since rebuilding a large list costs milliseconds. A view change reads on the window thread, as before, and drops a reload still running; a keystroke in the search field does not (below).

An archive, move or delete hides its rows at once and its `ActionDone` arrives after the daemon changed the store. A reload that started before then still holds the old rows, so the edit stays on the rows until a reload that started after it is applied.

While the Trash view is open, each sync event lists the backups again on the window thread; a backup's sender and subject are read once, and later lists open only new files.

The same sync event also reads the account's folders and unread counts again, on the window thread. Each count reads the partial index `messages_unread` (migration 010), which holds only unread mail; the covering folder index SQLite chose before read every message in the folder. The query names the index (`INDEXED BY`), since without statistics SQLite does not know it is the smaller one, and a test checks that the plan uses it.

The thread list reads the covering index `messages_folder_thread` (migration 009) alone, whichever filter is on, except that the attachments filter read every message's headers to make its guess (`has_attachment`, a generated column). Migration 015 adds the column to the index, which SQLite fills once per row written, so that filter reads the index like the others; the plan does not say so (it says `USING INDEX`, not `COVERING INDEX`, since the column is resolved while the code is generated), so the test checks the bytecode for a read of the table instead. On a 20k-message folder in SQLite directly, the filtered list fell from 136 ms to 33 ms, the unfiltered list's cost.

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

A search of filters alone, such as `after:2026-01-01` or `is:unread` without a word, has no match to drive the join, so it read every message of the store into a sort for its newest 100. It now walks `messages_newest` (migration 014), every message newest first, and stops at the limit; a search in one folder walks that folder's own index, since the folder is now its own condition in the SQL rather than `?2 IS NULL OR folder = ?2`, which the planner cannot serve from an index. A test checks both plans.

## Opening a message

Opening a message reads its row and raw bytes from the store and parses its MIME structure for the attachments and the HTML. That ran on the window thread, so the frame that chose a message with a large attachment waited for all of it. Now a thread of its own reads it (`reader.rs`), with its own connection, and the frame waits at most `BODY_BUDGET` (8 ms, half a 60 Hz frame): ordinary mail still opens in the frame that chose it, and a larger message shows "Loading…" until it comes. The thread reads only the newest request, so holding `j` through the list reads only where the cursor stops.

`open_message` is the read on its own, what the frame waited before; `open_message_frame` is what the frame waits now. The message is a 200 kB HTML newsletter with a base64 attachment. Release build, 10k messages, the same machine as the search table above. Median in ms, with the range of five runs.

| Message | `open_message` | `open_message_frame` |
| --- | --- | --- |
| 5 MB | 8.9 (8.9–11) | 8.2 (8.1–8.2) |
| 25 MB | 65 (64–78) | 8.2 (8.2–8.2) |

The bench keeps the 5 MB message; the 25 MB row is one run with a larger attachment. Below the budget nothing changes; above it the frame no longer grows with the message.

Downloading a message the store has no copy of was one `fetch_raw`: the reader showed nothing until every attachment had arrived, so a mail with a 20 MB scan took as long to show its two lines as it took to download 20 MB. A message larger than 256 KB now comes in two steps: its `BODYSTRUCTURE` and the parts the reader shows (the text, the HTML and the images it names as `cid:`), one round trip each, then the whole message. Below 256 KB one round trip still beats several. Not measured over a slow link yet.

## Startup and memory

What a user notices first: how long until the window shows a list, and how much memory the GUI and the daemon use. With `POSTVAK_STARTUP_METRICS=1` the GUI prints one JSON line per startup phase on stderr, `{"startup":"<phase>","ms":<ms>}`, in milliseconds since it started measuring; nothing otherwise.

| Phase | When |
| --- | --- |
| `start` | The GUI's code starts running, 0 ms |
| `backend_selected` | Slint's platform is up: connected to the display, its libraries loaded |
| `config_loaded` | config.toml is read |
| `daemon_connected` | The daemon answered, after starting it when none ran |
| `stores_opened` | Every account's store is open, with its folders |
| `first_list` | The first list's rows are loaded |
| `first_frame` | The window has drawn its first frame |

`scripts/startup.py` (Python 3, standard library only) runs the GUI against generated mail: the ignored test `bench::home` writes the benchmark's mail into a mail home, with an HTML newsletter (30 sections in tables and a `cid:` banner) and a plain message. Its accounts reach no server, so nothing but the stores is read. The script starts the GUI on a fresh copy of that home `--runs` times, stopping it and its daemon after each run, and records:

- each phase, in the GUI's own milliseconds and in milliseconds since the script started the process (`median_wall_ms`, which adds loading the binary);
- RSS, PSS and USS (private memory) of the GUI and of the daemon, which are separate processes: from `/proc/<pid>/smaps_rollup` on Linux, RSS from `ps` and the physical footprint from `footprint` on macOS. Once the window is idle, with the newsletter open (`postvak gui --open`), and after the plain message replaced it, which catches a renderer that never frees. Each reading waits `--settle` seconds (3 by default).

```sh
cargo build --release
SLINT_BACKEND=winit-software scripts/startup.py run --runs 5 --out startup.json   # needs a display
scripts/startup.py show startup.json                  # as Markdown tables
scripts/startup.py compare main.json branch.json      # two reports, metric by metric
```

`--scale` sets the number of messages (10,000 by default) and `--home` uses a home written before. The script runs on the display and renderer it is given; leave `SLINT_BACKEND` out for femtovg. CI runs it on every pull request and every merge to `main` (the `startup` job in `ci.yml`), on Linux under Xvfb with the software renderer so runs compare, writes the tables to the job's summary and uploads the report as `startup-report`. It sets no limits yet; once a few runs show the noise, it should fail on a regression beyond a threshold. The script was written for macOS too, but only Linux has run it.

Release build of `main` at d9b804b with this measurement added, Linux container under Xvfb (1920 × 1080), 4 vCPUs of an Intel Xeon at 2.80 GHz, software renderer, 10,000 messages. Median of five runs; in one of them the platform took 151 ms instead of about 28, and every later phase moved with it.

| Phase | ms | Since the process started |
| --- | --- | --- |
| `backend_selected` | 28 | 33 |
| `config_loaded` | 29 | 33 |
| `daemon_connected` | 133 | 137 |
| `stores_opened` | 141 | 146 |
| `first_list` | 144 | 149 |
| `first_frame` | 191 | 197 |

| | GUI RSS | GUI PSS | GUI USS | Daemon RSS | Daemon PSS |
| --- | --- | --- | --- | --- | --- |
| Idle | 47 MB | 37 MB | 30 MB | 16 MB | 8 MB |
| Newsletter open | 64 MB | 54 MB | 46 MB | 16 MB | 8 MB |
| After the plain message | 64 MB | 54 MB | 46 MB | 16 MB | 8 MB |

What this shows:

- Starting the daemon and waiting for its answer is more than half of the time to the first frame: about 105 ms. With a daemon already running it falls away.
- Slint's platform takes about 28 ms, and is the noisiest phase; reading config.toml takes under a millisecond.
- The stores and the first list take about 10 ms together at this scale, the first frame about 45 ms more.
- The newsletter adds 17 MB to the GUI, and replacing it with a plain message gives none of that back. Whether that is the HTML renderer's cache, the strips or the allocator keeping freed memory is not known yet.
- A longer newsletter, taller than 32,767 physical pixels, panics the software renderer (#321), which is why the generated one has 30 sections.

Not measured yet: drawing a frame of the list through the real window (`Fixture::window()`).

## Choosing the renderer

The GUI draws with Slint's default renderer, femtovg on OpenGL, and with the software renderer only in safe mode, after a failed start or when `SLINT_BACKEND` asks for it. An OpenGL context brings the driver's memory into the process, while a mail client mostly draws text and flat rectangles, and the HTML strips are painted on the CPU either way. Which default costs less is being measured (#315); femtovg stays the default until more than one machine has been measured.

`scripts/startup.py`, run once per renderer and scale on the same mail home: release build of `main` at 35bcb6b, KDE Plasma 6 on Wayland, NVIDIA GeForce RTX 3080 with the proprietary driver 615.78, a 3440 × 1440 screen at scale 1, AMD Ryzen 9 5900XT (16 cores), 32 GB, Rust 1.99.1, 10,000 messages. Scale 2 is `SLINT_SCALE_FACTOR=2` on the same screen, not a HiDPI display. Median of five runs; GUI memory only, the daemon's did not change.

| | femtovg, 1x | software, 1x | femtovg, 2x | software, 2x |
| --- | --- | --- | --- | --- |
| `first_frame` | 402 ms (315–741) | 179 ms (166–202) | 348 ms (305–411) | 178 ms (173–287) |
| Idle, RSS / PSS / USS | 161 / 137 / 128 MB | 60 / 47 / 37 MB | 160 / 136 / 127 MB | 84 / 59 / 36 MB |
| Newsletter open | 186 / 161 / 152 MB | 80 / 66 / 55 MB | 208 / 184 / 175 MB | 118 / 92 / 69 MB |
| After the plain message | 181 / 157 / 147 MB | 75 / 61 / 50 MB | 189 / 164 / 155 MB | 99 / 72 / 49 MB |

What this shows, on this machine:

- The OpenGL context costs about 100 MB of private memory before anything is drawn; the software renderer's GUI uses less than a third of femtovg's.
- femtovg's first frame comes about 220 ms later at 1x and 170 ms later at 2x, and varies more: creating the context and compiling its shaders. Every phase before it is the same for both.
- At scale 2 the software renderer's RSS grows by the size of its buffers, which the compositor shares (USS stays at 36 MB); still about half of femtovg's.

Not measured yet: Mesa on Intel or AMD, which the issue expects to cost less than NVIDIA's driver; a real HiDPI or 4K screen; X11; macOS; and CPU and frame times while scrolling a long list or HTML message, where the software renderer pays for drawing on the CPU.
