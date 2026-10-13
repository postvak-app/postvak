# Slint GUI rewrite: parity checklist

Date: 2026-10-13. What the egui GUI does today, as found by reading `src/gui/` at `origin/main` (`fffa619`), its tests, and the GUI docs, specs, CI and packaging around it. The Slint rewrite (`2026-10-13-postvak-slint-gui-design.md`) is done when every item here has an equivalent, or the design names why not. **[pure]** marks logic that is toolkit-free and moves unchanged; **[egui]** marks what is rebuilt.

## App state, events and the window (`app.rs`, `mod.rs`)
Legend: **[pure]** means the logic can move to a toolkit-free module unchanged, or nearly so. **[egui]** means it is tied to egui or eframe and has to be rebuilt.

### 1. Constants and user-visible strings

- `MOVE_ONE_ACCOUNT` = "move works on one account at a time; mark messages from one account" (app.rs:30) [pure]
- `DAEMON_LOST` = "background sync stopped — reconnecting" (app.rs:34) [pure]
- `NEWER_DAEMON` prefix = "the daemon is version " (app.rs:37). A reconnect clears an error that starts with it. [pure]
- `HISTORY` = 50 lines (app.rs:40), `POLL` = 2.0 s (app.rs:43), `READ_DELAY` = 1.0 s (app.rs:46), `RECONNECT` = 5.0 s (app.rs:49) [pure]
- Banner: "config.toml changed — reopen the window to show it", drawn in `warn_fg_color` in a top panel under the toolbar (app.rs:462-469)
- History line: "background sync reconnected" (app.rs:633)
- History line: "{account}: restored to {clean(folder)}" (app.rs:717-722)
- Account error: "{n} of {total} messages failed: {first error}" (app.rs:706)
- Theme save error: "could not save the theme: {e}" (app.rs:879)
- Rules file error: "could not open rules.toml: {e}" (app.rs:1678)
- Save notes (app.rs:1889-1895): "Saved to {path}", "Could not save: {e}", "Could not save: the message is not downloaded"
- Source error: "Could not show the source: the message is not downloaded" (app.rs:1345)
- Folder-load warning, log only: "[{name}] could not list folders: {e}" (app.rs:133)
- Search failure, log only (app.rs:1104). Thread-load failure, log only (app.rs:1114).

### 2. Types and state

**`Connection` = (Client, Receiver<Event>, Vec<AccountStatus>)** (app.rs:52). `Waker` = boxed `Fn()`, called on each event so an idle window repaints (app.rs:55).
- `connect` calls `Client::connect_or_start` (app.rs:58). `session` subscribes *before* asking for the status, so no event is lost in between (app.rs:63-67). [pure]

**`BodyState`** (app.rs:69-93) holds the message on screen:
- `account`, `key: RowKey`
- `armed`: only the user's own choice marks a message read
- `attachments`, `html: Option<HtmlBody>`, `html_view: Option<HtmlState>`, `message`
- `read_sent`, `saved` (the note line), `shown_at`, `show_text` (`v`), `source` (the raw window), `text_note` (why HTML fell back to text), `text` (the body cleaned once)
- `shows_html()` = html is some && !show_text && no text_note (app.rs:97) [pure]

**`Account`** (app.rs:107-136) has `activity`, `error`, `folders: Vec<FolderRow>`, `name`, `pending: HashMap<RowKey, Vec<Optimistic>>` (oldest first), `queued`, and `store: Result<Store, String>`. The error text is "could not open the store: {e}" (app.rs:337).
- `busy()` is true unless the activity is None, Idle, NotRunning or Offline (app.rs:120). It drives the spinner. [pure]
- `reload_folders()` (app.rs:129) [pure]

**`folder_rows`** (app.rs:139-171) [pure]: unread counts after the pending edits. Hidden means read, `Seen(s)` means !s, `Flagged` leaves the count alone. The delta per folder is added to `store.unread_count`, clamped at 0, and the folders are sorted by `folders::sort`.

**`FolderRow`** {delimiter, name, special_use, unread} (app.rs:174) [pure].

**`View`**: Activity, AllInboxes, Folder{account, folder}, Rules, Trash. `lists_mail()` is true for AllInboxes and Folder (app.rs:182-199) [pure].

**`Focus`**: Body, Folders, List (app.rs:262).

**`UpdateCheck`**: Off, Waiting, Running(rx), Done(Option<String>) (app.rs:270).

**`App` fields** (app.rs:277-323):

| Field | Purpose |
|---|---|
| `accounts` | per-account state |
| `activity_log` | Activity view rows |
| `body` | message on screen |
| `client`, `events` | daemon client and its subscription |
| `collapsed` | folded folder branches (account, path); kept for the window's lifetime only, not persisted |
| `config_changed`, `config_mtime` | config banner |
| `daemon_lost` | event stream ended |
| `downloads` | save target |
| `error` | app-level status line |
| `fetch_release` | injectable release fetch, for tests |
| `focus` | focused pane |
| `focus_search` | focus the search field on the next frame |
| `help_open`, `history_open` | overlays |
| `logo` | toolbar texture [egui] |
| `html_generation` | layout counter; replies for older layouts are dropped |
| `history` | VecDeque of HistoryLine{at, text} |
| `last_poll`, `last_reconnect` | timers |
| `list: ListState` | rows, cursor, marked, expanded, threads, hits, viewport, follow_cursor |
| `move_picker` | Option of the filter text |
| `paths` | |
| `pending_arm` | the next loaded body is armed |
| `renderer` | HTML thread, started lazily |
| `reconnect` | injectable fn |
| `reconnecting` | at most one attempt in flight |
| `requested` | set of (account, RowKey) body fetches already sent |
| `rules`, `rules_mtime` | rules view and its file time |
| `search` | Option of the query |
| `theme` | preference |
| `theme_applied` | first-frame flag [egui] |
| `trash` | Trash view rows |
| `update` | release check |
| `view` | current view |
| `view_dirty` | reload at the start of the next frame |

**`App::new`** (app.rs:326-403): opens each account's store, reloads folders, loads rules (`RulesState::load`), records both mtimes, sets the theme from `config.ui.theme`, and sets `update` to Waiting if `config.ui.check_updates`, else Off. The initial view is `Folder{account 0, "INBOX"}` with focus on List, then `reload_view()`.

### 3. The frame loop, `show` (app.rs:406-508) [egui, but the order matters]

1. First frame only: `theme::install`, `set_theme`, then return and request a repaint, because fonts load next pass and drawing the serif family earlier panics (app.rs:408-415).
2. `receive`, then `receive_html`, `keep_daemon`, `check_release`. If `view_dirty` is set, take it and `reload_view`. Then `poll_files`. Then `keys()` → `apply` each.
3. If no text edit has focus, surrender egui widget focus so Tab, arrows, Space and Enter don't drive widgets (app.rs:429-435). Slint needs an equivalent: global keys must not activate the focused row.
4. `typing_search` = focus_search || the search field has focus. Then `sync_body` and `mark_read_after_delay`.
5. Load the logo texture from `ICON` once.
6. Layout:
   - top toolbar at exact height `TOP_BAR`, margins INSET / PAD / (TOP_BAR − CONTROL)/2
   - optional config banner
   - bottom status bar, margins INSET − GLYPH / INSET / PAD
   - left folders pane, id `FOLDERS` = "folders", resizable, default width `FOLDERS_WIDTH` = 220; the toolbar reads its width
   - central area framed with PAD on the right
7. Then the overlays: move picker, source window, help. Then apply the collected actions. Finally request a repaint after `POLL` (2 s), so polling ticks while idle.

**`show_panes`** (app.rs:1805-1850):
- Mail views: a list panel on the left (resizable, default 480, min `list::MIN_WIDTH`, inner margin PAD, left corners rounded) and the body pane (right corners rounded).
- Rules, Activity and Trash: one pane with all four corners rounded; it counts as focused whenever focus is not Folders.
- The focused pane gets a highlight via `theme::pane(frame, ui, focused)`. `body_frame` fills with `window_fill`, margins INSET / INSET / PAD (app.rs:1858). `central_panel` is the shared helper (app.rs:1854).

### 4. Daemon events, `handle` (app.rs:637-726) [pure]

Events for an unknown account name are ignored.

- **Activity**: pushes "{name}: {activity}" to history, except for FetchingBodies and FetchingHeaders ticks. Clears `account.error` unless the activity is Offline (Offline follows the error that caused it). Sets `activity`.
- **NewMail**: nothing here (`notify` handles it).
- **CommandFailed with request != 0**: ignored. That is another client's outcome, or the broadcast copy of our own.
- **ActionDone with request != 0**: `refresh(index)` only. Pending edits are not touched.
- **CommandFailed with request 0**: clears all of the account's `pending`, sets `queued` = 0, `note_error(Some)`, `refresh`.
- **Error**: `note_error(Some(index), msg)`.
- **BodiesFetched, RuleApplied, Synced**: `refresh`.
- **ActionDone with request 0**: `queued` saturating −1. For each uid in the results, removes the *oldest* pending edit for (folder, uid), and drops the key once it is empty. If any result failed, note "N of M messages failed: first". Then `refresh`.
- **BodyReady**: `reload_body(index, (folder, uid), now)`.
- **Restored**: history line, then `refresh`.

**`refresh`** (app.rs:729): reloads the folders, and sets `view_dirty` if the view is this account's folder, or AllInboxes, Activity or Trash.

**`note_error`** (app.rs:739) [pure]:
- Cleans the message with `clean(msg, false)`.
- For an account: drops that account's entries from `requested`, so failed body fetches are retried. Sets `account.error`. The history line is "{name}: {msg}".
- Otherwise sets `self.error`.
- Always `push_history`.

**`push_history`** (app.rs:756): skips a line identical to the last one, stamps it with `time::now()`, caps at 50.

### 5. Losing and reconnecting the daemon [pure logic plus a thread]

- **`receive`** (app.rs:510): drains with `try_recv` while `!daemon_lost`. A disconnect sets `daemon_lost`, sets `last_reconnect = now`, and calls `note_error(None, DAEMON_LOST)`.
- **`keep_daemon`** (app.rs:525):
  - Only while the daemon is lost.
  - With no attempt in flight and ≥5 s since the last one, it starts `start_reconnect`: a thread runs `reconnect(paths, waker)` and requests a repaint.
  - A disconnected attempt is cleared.
  - An Err is logged as a warning. If it downcasts to `NewerDaemon`, its Display text goes to the status line (only if different from the current error): "the daemon is version X, newer than this postvak (Y); restart this program". It retries every 5 s.
  - Ok → `reconnected`.
- **`reconnected`** (app.rs:610):
  - Swaps in the new client and events and clears `daemon_lost`.
  - Clears `error` if it is DAEMON_LOST or starts with NEWER_DAEMON.
  - Clears `requested`.
  - Per account: takes the activity from the new states (matched by name), clears the error if it is DAEMON_LOST, clears `pending`, sets `queued` = 0, reloads folders.
  - Pushes "background sync reconnected" and sets `view_dirty`.
- **`send`** (app.rs:1622) returns a bool. When `send_apply` fails it calls `note_error(Some(acct), DAEMON_LOST)`, which shows "work: background sync stopped — reconnecting".

### 6. Release check (app.rs:571-606)

Waiting (on the first real frame, after tests swap `fetch_release`) spawns `update::check(cache, now, CARGO_PKG_VERSION, fetch)`. Running → Done(answer); a disconnect gives Done(None). `newer_release()` returns the version only once the check is Done. [pure state machine; the thread wake is egui]

### 7. Keys, `keys()` (app.rs:947-1024) [egui input; the mapping is pure]

Printable keys are matched on the *typed text* (`typed`, app.rs:1914), so they follow the keyboard layout.

Always active, even while typing in a field:
- Esc → Escape
- Cmd/Ctrl+R → SyncNow (COMMAND modifier)

While a text field has focus, nothing else counts.

Otherwise:
- Tab → NextFocus
- `?` → ToggleHelp
- `/` → StartSearch
- `down` = `j` or ArrowDown; `up` = `k` or ArrowUp

Focus Folders:
- down → StepFolder(1), up → StepFolder(−1)

Focus List or Body:
- down/up → MoveCursor(±1)
- → Expand, ← Collapse
- `x` → ToggleMark
- `e` → Act(Archive)
- `#`, Delete, or Backspace (**macOS only**) → Act(Trash)
- `m` → OpenMovePicker
- `s` → ToggleFlag
- `u` → ToggleRead
- `v` → ToggleHtml

### 8. UiAction → `apply` (app.rs:769-944) [pure apart from SetTheme, the Html* actions and egui types]

- **Act(a)** → `act` (see §10).
- **ApproveRule(name)** → `rules::edit::approve_from_now(paths, account names, name, now)` via `edit_rules`.
- **RejectRule(name)** → `edit::reject`.
- **SetRuleEnabled(n, b)** → `edit::set_enabled`.
- `edit_rules`: Ok → `rules_changed`; Err → app error (app.rs:1647).
- **CloseSource** → `body.source = None`.
- **Collapse** / **Expand** → §9.
- **Escape**, a priority chain (app.rs:792-808):
  1. close the move picker
  2. else close the source window
  3. else close help
  4. else close history
  5. else, if searching: `search = None`, reset `list`, `reload_view`
  6. else clear `marked`
- **ListViewport{offset, height}** → stores the viewport and sets `follow_cursor = false`.
- **MarkFolderRead(acct, folder)** → `store.unread_uids`. Empty → nothing. Otherwise `send_apply(MarkRead, Seen(true))` and `rebuild_rows`. Err → account error.
- **MoveCursor(d)** → `step` with clamping (app.rs:1928, no wrap), `follow_cursor = true`, `pending_arm = true`.
- **MoveFilter(s)** → `move_picker = Some(s)`.
- **MoveTo(f)** → closes the picker. If the selection spans more than one account → MOVE_ONE_ACCOUNT (this rechecks, in case the selection grew under the open picker). Else `act(Move(f))`.
- **OpenMovePicker** → selected accounts 0: nothing; 1: `Some("")`; more: MOVE_ONE_ACCOUNT error, and the marks stay.
- **NextFocus** → Body → Folders → List → Body.
- **OpenSource** → `body::Source::new(raw)`, or the "not downloaded" note.
- **OpenRulesFile** (app.rs:1654):
  - Creates an empty rules.toml atomically if it is missing.
  - macOS runs `open -t`, otherwise `xdg-open`.
  - A thread waits on the child and logs a non-zero exit.
- **Restore(acct, file)** → removes the trash row optimistically and sends `Command::Restore`. If the send fails: DAEMON_LOST error, then `refresh` (which lists the backup again).
- **SaveAttachment(i)** → `message::save_attachment(raw, i, downloads)` → `body.saved`.
- **SaveEml** → `message::save_eml(raw, subject, downloads)`.
- `downloads_dir`: Downloads, else home, else "." (app.rs:1922).
- **SearchFocused** → `focus_search = false`.
- **SearchFor(q)** → sets `search`, cursor 0, clears rows, `reload_view`.
- **SelectRow(i)** → cursor = i, focus List, `pending_arm`, `arm_shown_body(i)`: clicking the row already shown arms it, and `shown_at` is kept, so a message long on screen is marked read at once (app.rs:1150).
- **SelectView(v)** → `select_view` (app.rs:1026): sets the view, closes the move picker, clears search, resets `ListState`, `reload_view`.
- **SetTheme(p)** → `ctx.set_theme`, then `config::save_theme(config_file, theme_of(p))`. Ok → updates `config_mtime`, so its own write does not raise the banner. Err → error note.
- **StartSearch** → only in mail views: `search = Some("")`, `focus_search = true`, reset list, reload.
- **StepFolder(d)** (app.rs:890) → steps through `tree_entries(skip_collapsed=true)`.
  - If the current view is hidden in a folded branch, it starts from `nearest_shown_above` (app.rs:1778).
  - Selects the new view only if it differs.
  - `tree_entries` (app.rs:1788) [pure]: All inboxes (only when there is more than one account), then each account's `folders::shown` minus collapsed branches, then Rules, Activity, Trash.
- **SyncNow** → `sync_all`: `Command::SyncNow` to every account.
- **ToggleFolder(acct, path)** → toggles membership in `collapsed`.
- **ToggleFlag** / **ToggleRead** → based on the *first* selected row: Flag or Unflag, MarkRead or MarkUnread.
- **ToggleHelp**. **ToggleHistory** → toggles and also clears `self.error`.
- **ToggleHtml** → flips `show_text`, only when the body has HTML.
- **ToggleMark** → toggles the cursor row's id in `marked`.
- **HtmlWidth / HtmlVisible / HtmlHover / HtmlSelect** → §12.

Other view helpers:
- `shows_recipient` (Sent and Drafts by special_use) (app.rs:1042)
- `shows_all_inboxes` (app.rs:1052)
- `is_archive` (app.rs:1066)
- `list_folder`: AllInboxes → `list::INBOX` (app.rs:1057)
[pure]

### 9. List loading and rows [pure]

**`reload_view`** (app.rs:1075):
- Activity → `rules::activity_log`; Trash → `rules::trash_rows`.
- Mail views cover one account or all of them. Stores that failed to open are skipped; their error shows in the folder pane.
- Searching: `store.search(q, None, SEARCH_LIMIT)` per account, then `merge_hits`.
- Otherwise: `thread_summaries(folder, THREAD_LIMIT)`. **On failure it keeps that account's previously listed threads.** Then `merge_threads`.
- Expanded threads are refreshed from `thread_members`; one with ≤1 member, or a failed query, is collapsed.
- Then `rebuild_rows`.

**`rebuild_rows`** (app.rs:1522):
- Builds rows from the hits or from `build_rows(folder, threads, expanded)`, then `apply_pending` for every account.
- The cursor stays on the same row via `same_row`. If that row is gone it keeps its index, clamped, so the next row is selected.

**`same_row`** (app.rs:1903): the account and the member flag must match. Thread rows match on thread id, because the key is the latest uid and changes with new mail. Other rows match on key.

**`expand`** (app.rs:1734): only for a thread row with count ≥2 that is not yet expanded.

**`collapse`** (app.rs:1754): finds the nearest row at or above the cursor that has a thread_id. If that thread was expanded, it is removed and the cursor moves to the thread row.

### 10. Actions and optimistic edits [pure]

**`act`** (app.rs:1542):
- `Optimistic::of(action)`. None means no-op.
- Targets come from `targets()` (app.rs:1709): marked rows, else the cursor row, grouped by (account, folder) into sorted uid sets. A thread row with count >1 expands to every `thread_members` uid.
- Empty targets → nothing is sent.
- MarkRead or MarkUnread on the shown message sets `body.read_sent`.
- A Hidden optimistic edit sets `pending_arm`, so the next message shown is marked read.
- **Archive inside the Archive folder becomes Move("INBOX")**.
- Then `send_apply` per group, clear `marked`, `rebuild_rows`.

**`send_apply`** (app.rs:1593):
- Sends `Command::Apply{folder, uids, action, by: "gui"}`.
- On success: `queued += 1`, pushes the optimistic edit per key, `reload_folders` so unread counts move at once.
- On failure: DAEMON_LOST account error.

`selected_rows` (app.rs:1695) and `selected_account_count` (app.rs:1688).

### 11. Body loading, prefetch and the read timer [pure, with times as f64 seconds]

**`sync_body`** (app.rs:1163):
- While a search is being typed, `body = None`.
- Otherwise takes `pending_arm`. If the cursor row's id differs from the body's, `load_body(…, armed)` and `prefetch_neighbours`.

**`read_stored`**: the raw message → attachments, `html::html_of`, `store.message` (app.rs:1178).

**`load_body`** (app.rs:1249):
- Requests the body if `body_text` is None.
- `text_note = too_large(html)`: HTML longer than `html::MAX_HTML` → `html::TOO_LARGE`.
- `shown_at = now`.

**`request_body`** (app.rs:1219): sends FetchBody once per (account, key), tracked in `requested`.

**`prefetch_neighbours`** (app.rs:1228): the rows at cursor ±1 with a missing body_text, each fetched from its own account.

**`reload_body`** on BodyReady (app.rs:1193):
- Only for the shown message.
- `shown_at` is reset when text first appears.
- If the HTML changed: recomputes `text_note`, swaps the HTML, resets `html_view`.

**`mark_read_after_delay`** (app.rs:1274):
- Requires `armed`.
- Waits while body_text is None, so the delay counts from the text appearing, not from "Loading…".
- `seen` = the latest pending `Seen` edit, else `message.is_seen`. Skips if seen or `read_sent`.
- Before 1 s: schedules a repaint at the remainder.
- Then: `read_sent = true`, `send_apply(MarkRead, Seen(true))`, `rebuild_rows`.

### 12. HTML view plumbing (app.rs:1351-1518) [egui textures; generation and settle logic pure]

**`receive_html`**: only replies whose generation matches the body's `html_view` are taken.
- Failed → `text_note = html::FAILED`, the view is dropped.
- Laid with height > `MAX_HEIGHT` → TOO_LARGE. Otherwise sets the page size and remote, and clears strips, requested, hover, hovered_at, selected_at and selection.
- Link → `hover`.
- Selected → sets `selection` and clears `requested` to repaint the strips with the highlight; the old strips stay until then.
- Strip → texture "html-{gen}-{strip}".

**`html_width`**:
- Only when `shows_html`.
- First layout: immediate.
- Same (scale, width) as sent: clears `wanted`.
- A changed size must hold still for `html::SETTLE` before a relayout. Until then the old layout stays and repaints are scheduled.
- Increments `html_generation` and sends `Request::Load`. The `Renderer` starts lazily.

**`html_visible`**: requests the `wanted_strips` not yet requested, and drops the `far_strips`.

**`html_hover`**: sends `Request::Hit`; None clears the hover.

**`html_select`**: sends `Request::Select`.

### 13. File polling and the config banner [pure apart from timing]

`poll_files` runs every 2 s (app.rs:1627):
- The rules.toml mtime changed → `rules_changed`: re-load `RulesState`, carrying over the previous rules (app.rs:1641). The daemon notices on its own.
- The config.toml mtime changed → `config_changed = true`, which shows the banner permanently. There is no live reload.

### 14. Theme [egui]

- `preference` and `theme_of` map `config::Theme` ↔ `egui::ThemePreference` (app.rs:1944-1958). Slint needs its own Dark/Light/System mapping.
- The theme starts from config and is saved on change (§8).

### 15. Startup, `mod.rs`

- **`ICON`** = assets/icon.png (mod.rs:31). The toolbar logo uses it.
- **`WINDOW_ICON`**: on macOS, `assets/macos.iconset/icon_512x512@2x.png` (Apple grid, tile 824/1024); elsewhere `ICON` (mod.rs:34-37).
- **`APP_ID`** = "io.github.postvak_app.postvak". It is the Wayland app id and X11 WM class, and the desktop entry and icons are named after it (mod.rs:41).
- **`run(paths)`** (mod.rs:45):
  - A `OnceLock<Context>` lets daemon events that arrive before the window exists wait in the channel.
  - `start` Ok → `open`. Otherwise `explain(e, Some(daemon_log))`, or `explain(e, None)` if Paths failed. Always returns `Err` after explaining.
- **`start`** (mod.rs:60):
  - `Config::load`.
  - No accounts → `bail!(startup::NoAccounts)`.
  - The waker repaints the context once it is set.
  - `app::connect`.
- **`native_options(size)`** → `viewport`: icon, app id, inner size, title "Postvak". On macOS, `prefer_integrated_gpu` sets wgpu `PowerPreference::LowPower` unless `WGPU_POWER_PREF` is set (mod.rs:74-106). This keeps the discrete GPU of a dual-GPU MacBook off; Slint's renderer needs the same check.
- **`explain`** (mod.rs:110):
  - Shows `startup::Problem::of(error, log)` in a 520×260 window and returns the error for stderr.
  - A failure to show the window (no display) is only debug-logged.
  - The window closes when `startup::show` returns true.
- **`explain_options`**: `persist_window = false` and an empty `persistence_path`, so it never shares or creates the mail window's state file. `Explain` also does not persist egui memory (mod.rs:128-155).
- **`open`** (mod.rs:157):
  - 1280×800 default.
  - Persists window size, position *and pane widths* to `state_dir/window.ron`; typed text is never saved.
  - Sets the OnceLock context and builds `App::new`.
  - Error text: "the window failed: {e}".

### 16. Tests

### mod.rs

- `macos_windows_prefer_the_integrated_gpu`: power preference is LowPower (macOS, skipped when the env var is set).
- `the_icon_is_a_square_png_with_transparent_corners`: 512×512, top-left alpha 0.
- `the_window_icon_is_a_valid_png`: square; on macOS the left-middle pixel is transparent (Apple margin).
- `the_error_window_saves_nothing`: no persist_window, empty path, no memory persistence.
- `the_desktop_entry_names_the_app_id`: the .desktop file has `Icon=` and `StartupWMClass=` equal to APP_ID.

### app.rs

Fixture plus kittest. "sent" means the commands the test daemon received.

**All inboxes and search**
- `all_inboxes_lists_every_accounts_inbox_newest_first` (1985): rows [(0,3),(1,2),(0,1)]; the Archive message is excluded; label "3 threads, 0 unread".
- `all_inboxes_lists_the_other_accounts_when_one_store_is_broken` (2000): only the work row shows; "All inboxes" label appears twice.
- `a_failed_thread_load_keeps_the_rows_already_listed` (2018).
- `slash_in_all_inboxes_searches_every_account_newest_first` (2056): placeholder "Search every account"; hits across folders; label "home · [Archive] Sender 3"; Esc restores the inbox rows and the view.

**Theme**
- `theme_starts_from_config` (2033): System by default, Dark from config.

**Actions and the list**
- `archive_sends_the_cursor_row_hides_it_and_selects_the_next` (2120): sends Archive [3]; rows [2,1]; cursor on 2; "· 1 queued".
- `archive_on_a_thread_row_sends_every_member` (2135): [4,5].
- `the_same_thread_in_another_account_is_another_row` (2148): `same_row` unit test.
- `marked_rows_are_deleted_together` (2158): x, j, x, # → Trash [2,3]; marks cleared.
- `a_failed_action_puts_the_row_back_and_says_why` (2171): "work: 1 of 1 messages failed: NO [SERVERBUG] try later"; queued 0.
- `a_sync_before_the_action_finishes_keeps_the_row_hidden` (2197).
- `acting_on_an_empty_folder_sends_nothing` (2230): cursor stays at 0.
- `a_second_edit_on_a_row_stacks_and_the_first_done_keeps_it` (2244): the flag survives the first ActionDone; queued 1.
- `the_unread_count_follows_sent_actions_before_the_daemon_answers` (2281): 2 → 1 → 0.
- `u_marks_read_and_s_flags` (2322).

**Move picker**
- `switching_views_closes_the_move_picker` (2269).
- `m_filters_the_folders_and_enter_moves` (2339): "rec" + Enter → Move("Receipts").

**Body**
- `the_next_rows_body_is_fetched_before_the_cursor_reaches_it` (2302): prefetch at startup; nothing again on `j`.

**Keys**
- `command_r_syncs_every_running_account` (2355).
- `backspace_deletes_on_macos_only` (2371).

**Daemon**
- `closing_the_event_stream_shows_the_reconnect_line` (2386).
- `a_reconnect_replaces_the_client_and_refreshes_the_accounts` (2424): "work: {DAEMON_LOST}" while lost; after the reconnect, "work: up to date · {clock}" and "Inbox (1)"; commands go to the new client.
- `a_newer_daemon_is_named_on_the_status_line_and_retried` (2464): the exact NewerDaemon text; another attempt 5 s later.
- `the_window_opens_with_the_daemons_activity` (2495).
- `a_refused_command_shows_its_reason` (2516).
- `another_clients_outcomes_leave_this_windows_edits_alone` (2537): request ids 7 and 8 are ignored.
- `a_refused_action_puts_the_row_back_and_clears_the_queue` (2565): no "queued" label.

**Release check**
- `a_cached_newer_release_reaches_the_window` (2596).
- `a_failed_check_shows_nothing_and_is_remembered` (2610).
- `by_default_no_check_starts` (2620): no cache file is written.

**All inboxes across accounts**
- `all_inboxes_sends_each_action_to_the_rows_own_account` (2656).
- `marking_in_all_inboxes_marks_only_that_accounts_message` (2682): marks are keyed (account, (folder, uid)).
- `the_reader_opens_and_marks_read_in_the_rows_own_account` (2705): "Body of 1"; MarkRead after 1.1 s.
- `the_move_picker_offers_the_rows_own_account_folders` (2731).
- `moving_marked_rows_from_two_accounts_is_refused` (2752): the marks stay.
- `a_refusal_for_one_account_puts_back_only_its_rows` (2769).
- `a_move_is_refused_when_the_selection_grew_to_two_accounts_under_the_open_picker` (2791).

**Porting note:** most of these tests drive pure App state. Splitting App into a toolkit-free `state` (handle, apply, reload_view, act, timers taking `now: f64`) with a thin Slint adapter keeps all of them except the label queries. Those label assertions (the status line, "N threads, M unread", the placeholder, the folder labels) need Slint's testing backend or assertions on view-model strings instead.

## List, folders, toolbar and status bar
Legend: **[pure]** means toolkit-free and reusable unchanged (move it out of `gui/` or keep it as a plain module). **[egui]** means drawing or interaction code that has to be rebuilt in Slint. Paths are relative to `src/gui/`.

### 1. Message list (`list.rs`)

### Data model and logic, all [pure] apart from `search_id`
- `THREAD_LIMIT = 10_000` threads per folder (list.rs:16). `SEARCH_LIMIT = 500` search results (list.rs:18).
- `RowKey = (folder, uid)` (list.rs:20). `Row` holds account index, count, date, flagged, folder, from, member, subject, `thread_id` (set only on thread rows), to, uid and unread (list.rs:27-42).
  - `key()` and `id()` (list.rs:45-52). A uid is unique only within one account's folder, so `id()` is `(account, key)`.
  - `from_message` and `from_summary` (list.rs:54-86): `unread = !is_seen()`.
- `ListState` (list.rs:90-103) holds the cursor, `expanded` keyed by `(account, thread_id)`, `follow_cursor`, `hits`, `marked: HashSet<(usize, RowKey)>`, rows, threads and `viewport (offset, height)`. `search_id()` is an egui `Id` (list.rs:22) and is the only egui-specific part here.
- `build_rows` (list.rs:106-128) builds one row per thread from its latest message, with the thread's count, flagged and unread. Members of an expanded thread follow it with `member = true` and `thread_id = None`.
- `Optimistic::of` (list.rs:138-151):
  - Archive, Delete, Move and Trash give `Hidden`.
  - Flag and Unflag give `Flagged(bool)`.
  - MarkRead and MarkUnread give `Seen(bool)`.
  - Notify, Silent and Tag give `None`.
- `apply_pending` (list.rs:155-176) drops rows that have any `Hidden` edit, then applies the edits in order, so the last one wins. `pending[i]` is the edits for account i.
- `display_name` (list.rs:179-199) uses the quoted name first, then `Name <addr>`, then the bare address from `<addr>`, then the first comma-separated entry.
- `list_date` (list.rs:202-220): `%H:%M` for today, `%a %H:%M` for 1-6 days ago, `%-d %b` for this year, `%-d %b %Y` otherwise. It returns an empty string for an invalid timestamp.
- `Columns` and `columns()` (list.rs:223-285):
  - `who` is the recipient (`to`) when `recipient` is set, otherwise the sender.
  - The subject is prefixed `"[{label(folder)}] "` in search and suffixed `" ({count})"` when count > 1.
  - Every field goes through `clean(.., false)`, which strips control characters.
- `row_text` (list.rs:288-318) is the accessible name, and tests query it. Format: `"• "` or `"   "`, then `"⚑ "` if flagged, six spaces if a member, `"{account} · "`, `"[Folder] "` in search, `"{who} — {subject}"`, `" (n)"`, `"  ·  {date}"`, all cleaned. A marked row gets the prefix `"✔ "` (list.rs:527-529).
- `sender_width(total, date) = max(0, total - 32 - date - 12)` (list.rs:251).
- `offset_showing` (list.rs:428-438) scrolls only as far as needed to show the cursor.
- `INBOX = "INBOX"` (list.rs:441) is the folder All inboxes reads, by exact name.
- `merge_hits` (list.rs:444) and `merge_threads` (list.rs:452) are stable sorts, newest first, truncated to the limit. On equal dates, account order is kept.

### Row drawing rules (`draw_row`, list.rs:322-425) [egui]
- Constants:
  - `DOT = 12`, `MARKER = 20`, `GAP = 12`, `MIN_TEXT = 16`.
  - `MIN_WIDTH = 220` is the list pane's minimum.
  - `MEMBER_INDENT = INSET (16)`.
  - `TAG = 80` is the widest the account tag gets.
  - `LIST_ROW = 2 * ROW = 48` (list.rs:234-248).
- Background:
  - selected: `palette.selected` fill with `RADIUS`;
  - hovered: `palette.hover`;
  - otherwise a 1 px `palette.border` hairline at the bottom.
- A row counts as selected when it has the cursor or is marked (list.rs:517).
- Two lines, at 1/3 and 2/3 of the row height. Content starts at `PAD`.
- Unread dot: a filled circle of radius 3 at `x + 3`, in `highlight`, on line 1.
- Marker cell:
  - `icons::CHECK` if marked, else `icons::FLAG` if flagged.
  - Drawn in `highlight`, centred at `x + MARKER/2 - 2`.
  - Marked takes precedence over flagged.
- Date: right-aligned at `right - PAD`, monospace at font size - 1, in `muted`.
- Sender and subject colours: both `text` when unread; otherwise `secondary` for the sender and `muted` for the subject. An unread sender uses the `theme::BOLD` (semibold) family.
- Member rows indent both the sender and the subject by 16.
- Account tag (All inboxes only):
  - Font size - 2, `muted`, truncated to `min(TAG, sender_width - indent)`.
  - Left out when that width is under 16.
  - The sender starts after the tag plus `PAD`.
- The sender is truncated with "…" and left out if the room is under 16. The subject on line 2 gets the full width, with the same 16 rule.
- Selected rows draw everything in `on_accent`.

### Panel behaviour (`show`, list.rs:465-545) [egui]
- Title row (`title`, list.rs:594-612), shown only when not searching:
  - Heading `"All inboxes"`, or `label(folder)`.
  - Weak text `"{n} thread(s), {u} unread"` with the singular `"thread"`. Counts exclude member rows and use `numbers::count`.
  - Indented by PAD, inside `theme::heading_row`.
- Search field, shown when `app.search` is `Some`:
  - Hint text `"Search every account"` in All inboxes, otherwise `"Search this account"`. Full width.
  - When `focus_search` is set, it takes focus and emits `SearchFocused`.
  - Each change emits `SearchFor(text)`.
  - Enter while focused selects row 0 (`SelectRow(0)`).
  - Weak line below: `"{n} results · mail whose body is not downloaded matches on sender, recipients and subject only"`.
- The list is virtualised with `show_rows` at `LIST_ROW`.
  - When `follow_cursor` is set, it scrolls via `offset_showing` using row height + item_spacing.y.
  - It reports `ListViewport{offset,height}` every frame.
- Mouse:
  - A left click emits `SelectRow(i)`.
  - A right click selects the row only if it is **not** marked, so the menu acts on the marked set. Then it opens the context menu.
- The account tag shows only when `view == AllInboxes`. The recipient replaces the sender in folders whose special_use is Sent or Drafts (`app.shows_recipient`, app.rs:1042).

### Right-click mail menu (`mail_menu`, list.rs:548-591)
Three groups, separated by separators. Shortcut text is shown on the right.
1. `"Mark as read"` / `"Mark as unread"` (u), depending on `row.unread`. `"Flag"` / `"Unflag"` (s).
2. `"Archive"` (e, `Act(Archive)`), `"Move to…"` (m, `OpenMovePicker`), `"Delete"` (#, `Act(Trash)`).
3. `"View source"` (`OpenSource`), `"Save .eml"` (`SaveEml`). Neither has a shortcut.

The menu closes after a click: a test checks that the `"Archive e"` label disappears.

### Move picker (`show_move_picker`, list.rs:616-658)
- A window titled `"Move to"`, not collapsible and not resizable. It shows only when `move_picker` is `Some` and a row is selected.
- The candidate list is the folders of the selected row's account, filtered by a case-insensitive substring of the filter text. It excludes the **cursor** row's folder, falling back to the first selected row's folder. That rule is what makes an archived search hit offer INBOX.
- The text field has hint `"Folder"` and always takes focus. Each change emits `MoveFilter`.
- Enter moves to the first match (`MoveTo`). Clicking a name moves to that folder. Names are cleaned for display, but the raw name is sent.
- The filter and exclusion logic is [pure] in substance but inlined, so extract it.

### Tests in `list.rs`
- `merge_hits_is_newest_first_and_capped`: shape is `[(1,4),(0,3),(1,2)]`.
- `merge_threads_is_newest_first_across_accounts_and_capped`: equal dates keep account order, and the cap applies.
- `build_rows_puts_members_under_an_expanded_thread`: covers member flags, count, `thread_id` set only on thread rows, and flagged.
- `list_date_is_time_today_…`: expects `"09:30"`, `"Sun 18:00"`, `"3 Sep"` and `"10 Dec 2025"`.
- `columns_split_markers_sender_subject_and_date`: in search, the subject is `"[Inbox] Lunch? (3)"`, `who` is `"Linus Example"` and the date is `"09:13"`.
- `the_account_tag_comes_before_the_sender`: `"   work · Sender 1 — hello"`.
- `rows_name_their_account_in_all_inboxes_only` (GUI).
- `the_sender_keeps_clear_of_the_date_and_never_goes_negative`.
- `display_name_prefers_the_name_then_the_address`: five cases.
- `offset_showing_scrolls_only_as_far_as_needed`.
- `row_text_strips_control_characters_from_headers`: no control characters remain, and the text starts with `"• ⚑ "`.
- `j_and_k_move_through_rows_newest_first`: rows are uids 3, 2, 1; j and ArrowDown move; the cursor stops at the last row; k moves up.
- `right_expands_a_thread_and_left_collapses_it_from_a_member`: 2 rows, then 4 rows, then Left from a member gives 2 rows with the cursor on the thread row. Also checks `"thread (2)"`.
- `a_marked_row_says_so_in_its_name`: the `✔ ` prefix.
- `in_a_narrow_row_nothing_is_drawn_over_the_date_or_past_the_edge`: widths 150 and 130. The subject is below the date, nothing beside the date overlaps it, and nothing goes past the right edge.
- `unread_senders_are_semibold_dates_monospace_and_rows_ruled`.
- `the_list_is_titled_with_the_folder_and_its_counts`: `"Inbox"` and `"2 threads, 1 unread"`.
- `right_clicking_a_row_selects_it_and_its_menu_archives_it`: sends `Apply{INBOX,[1],Archive,by:"gui"}`.
- `x_marks_rows_and_escape_clears_them`.
- `tab_cycles_focus_and_j_in_the_folder_pane_opens_the_next_folder`: focus goes List, Body, Folders; j opens Archive.
- `keys_on_an_account_without_a_store_do_nothing`.
- `tab_and_arrows_leave_no_egui_widget_focus_for_space_or_enter_to_click`: Space and Enter must never activate a focused widget. In Slint, make sure the FocusScope consumes them.
- `apply_pending_hides_and_overrides_rows` and `apply_pending_reads_each_rows_own_account`.
- `expanding_a_thread_in_one_account_leaves_the_same_thread_in_another_alone`.
- `slash_searches_the_account_and_escape_returns_to_the_folder`:
  - Hits come back newest first across folders, and the `"[Archive] Sender 3"` label appears.
  - `"2 results"` is shown.
  - Enter then `e` in Archive sends `Move("INBOX")` (unarchive).
  - Escape restores the rows, with no focus left on the field, and j works again.
- `shortcut_letters_typed_into_search_stay_text`: typing `"e#us"` sends nothing.
- `the_move_picker_in_search_offers_the_inbox_for_an_archived_hit`.

### 2. Folder pane (`folders.rs`)

### Structure (`show`, folders.rs:10-72) [egui]
- A bottom-pinned panel holds `Rules` (`icons::RULES`, `View::Rules`), `Activity` (`ACTIVITY`) and `Backups` (`BACKUPS`, `View::Trash`). Each is a `folder_row` at indent 0 with no count.
- The rest scrolls.
- `"All inboxes"`:
  - Shown only when there is more than one account (`shows_all_inboxes`, app.rs:1052).
  - Sits in a `heading_row`, on the same line as the list title.
  - Uses the `ALL_INBOXES` icon. Its count is the sum of unread in every account's `INBOX` (`inbox_unread`, folders.rs:217).
  - Followed by a separator.
- For each account:
  - A heading row indented by button_padding.x shows the account name in BOLD, `muted`.
  - A small spinner in the weak colour shows while `account.busy()`, which means activity is not None, Idle, NotRunning or Offline (app.rs:120).
  - If the store failed to open, an error label in `error_fg_color` follows, with the text cleaned.
  - Then the account's folders, then `PAD` of space.

### Folders per account (`account_folders`, folders.rs:75-149)
- Special folders (`is_special`) come first, flat, labelled with `label()` and using `icons::for_special_use`. A right click gives `folder_menu` with no branch.
- A separator follows, only when both special and custom folders exist.
- Custom folders come next, as a `tree` filtered by `visible` with the collapsed set `app.collapsed: (account, path)`.
  - Icons: `FOLDER` for a leaf, `CARET_DOWN` when expanded, `CARET_RIGHT` when collapsed.
  - Indent is `16 * depth` (`INDENT`, folders.rs:195).
- Caret hit area (folders.rs:198-214): from the row's left edge, `2*button_padding + 16` wide.
  - Clicking it emits `ToggleFolder(account, path)` and does not open the folder.
  - Accessible name: `"Collapse {label}"` or `"Expand {label}"`.
- Clicking the rest of the row opens the folder, unless the row is an unlisted parent (`folder = None`).

### Folder menu (`folder_menu`, folders.rs:152-192)
- `"Open"`, only when the row is a folder.
- `"Collapse"` / `"Expand"`, only for a parent row.
- A separator.
- `"Mark all as read"`, disabled when unread is 0. It emits `MarkFolderRead`.
- `"Sync now"`, with shortcut text formatted as Cmd/Ctrl+R. It emits `SyncNow`.

### `folder_row` (folders.rs:352-404) [egui]
- A full-width selectable button: indent, then the icon, then `"  {label}"`, which shrinks and truncates before the count.
- The count sits on the right in `muted`. The right slot is always present, even when empty, so the label stays on the left.
- When selected and the count is non-empty:
  - the icon turns `highlight` (brass);
  - the count becomes a pill: a `highlight` fill with radius `h/2`, width `text + 1.5*PAD`, and the text in `palette.background`.
- Accessible name is `count_and_name` (folders.rs:226): `"Name"`, or `"Name (n)"` with a grouped number.

### Tree logic, all [pure]
- `tree` (folders.rs:251-298):
  - Sorts by lowercase path segments, so `"Work Archive"` cannot land between `Work` and `Work/Clients`.
  - Creates rows for unlisted parents (`folder: None`), then fills `folder` when a stored folder matches the path. This makes a special folder with children an openable parent.
  - `has_children` is true when the next row is deeper.
  - The `INBOX{delim}` prefix (case-insensitive) is stripped only when **every** custom folder has it.
  - A folder with no delimiter or an empty one stays flat.
- `visible` hides descendants of collapsed paths (folders.rs:323). `shown` returns the drawing order (special, then visible custom) and drives j/k and arrow navigation in the folder pane (folders.rs:337).
- `label`: "INBOX" in any case becomes `"Inbox"`, display only (folders.rs:408).
- `rank`, `is_special` and `sort` (folders.rs:417-443): INBOX = 0, Archive 1, Drafts 2, Sent 3, Junk 4, Trash 5, everything else 6, and then alphabetical, case-insensitive.

### Tests in `folders.rs`
- `all_inboxes_sits_on_top_with_every_inbox_unread_count_and_opens_on_click`: `"All inboxes (3)"` sits above `"home"`, and the Archive unread is excluded.
- `one_account_has_no_all_inboxes_entry`.
- `k_in_the_folder_pane_steps_up_from_the_first_inbox_to_all_inboxes`.
- `a_folder_menu_marks_all_its_unread_mail_read`: sends `Apply{INBOX,[1,3],MarkRead}`.
- `the_inbox_shows_as_inbox_and_other_names_stay`.
- `tree_nests_by_delimiter_and_adds_unlisted_parents`.
- `a_sibling_sorting_between_parent_and_child_does_not_steal_the_child`.
- `an_inbox_namespace_prefix_is_not_a_parent`.
- `the_inbox_prefix_is_dropped_only_when_every_custom_folder_has_it`.
- `a_special_folder_with_children_is_an_openable_parent`.
- `stepping_from_a_folder_in_a_collapsed_branch_stays_in_its_account`.
- `a_folder_without_a_delimiter_stays_flat`.
- `collapsing_hides_descendants_only`.
- `clicking_a_caret_collapses_its_folder`: uses the `"Collapse Projects"` label; Website is then hidden and the path is in the collapsed set.
- `arrow_keys_follow_the_tree_and_skip_collapsed_folders`.
- `sort_puts_inbox_then_special_use_then_the_rest_alphabetically`.
- `tree_shows_folders_with_unread_counts_and_selects_on_click`: also clicking Rules gives `View::Rules`.
- `rows_are_full_width_and_keep_the_count_in_their_name`.
- `a_long_name_truncates_before_its_count`.
- `the_selected_folder_has_a_brass_icon_and_count_badge`: icon colour `FRAME.highlight`, count colour `FRAME.background`, and a pill rect.
- `account_names_are_muted_and_semibold`.
- `a_row_without_a_count_keeps_its_label_on_the_left`: label.left < 30.
- `an_indented_row_starts_later_and_ends_at_the_same_edge`.
- `a_separator_divides_special_from_custom_folders`.
- `the_backup_view_is_labelled_backups_and_sits_at_the_bottom`: bottom > 700 in an 800 px window; clicking it opens `View::Trash`.
- `a_store_that_fails_to_open_shows_its_error_and_other_accounts_still_work`: text contains `"could not open the store"`, and two `"Inbox"` labels exist (the folder row and the list title).

### 3. Toolbar (`toolbar.rs`) [egui]

- Logo cell width is the folder pane's current width, minus item_spacing.x, minus `GLYPH` (6). It follows a live resize, so the first action's glyph lines up with the list content. Height is `CONTROL` (28). See toolbar.rs:18-24.
- Logo: a 20×20 image if `app.logo` is set, then `"Postvak"` in the `SERIF` family, size 19, strong, vertically centred with the buttons (toolbar.rs:26-35).
- Action buttons have item spacing `PAD/2` (toolbar.rs:48). Each entry is name, then hover hint:
  - Archive, which depends on the view:
    - in a folder whose special_use is Archive: `INBOX` icon, `"Unarchive message"`, hint `"Move to inbox · e"`;
    - otherwise: `ARCHIVE` icon, `"Archive message"`, hint `"Archive · e"`.
    - Both emit `Act(Archive)`; the app turns it into Move INBOX in the Archive folder.
  - `"Move message"` / `"Move · m"`, `OpenMovePicker`.
  - `"Delete message"` / `"Delete · #"`, `Act(Trash)`.
  - `"Flag message"` / `"Flag · s"`, `ToggleFlag`.
  - `"Mark read or unread"` / `"Read or unread · u"`, `ToggleRead`.
- The buttons are enabled only when the view lists mail, the list has rows, **and** the move picker is closed (toolbar.rs:46).
- `icon_button` (toolbar.rs:91-109): frameless, `CONTROL×CONTROL`, glyph size `ICON` (16), hover hint. Its accessible name is `name`. It is reused by the status bar.
- Search field (toolbar.rs:112-149), right-aligned:
  - 260×28, PAD radius.
  - Fill is `faint`, or `pressed` when hovered and enabled. It is drawn at disabled alpha when the view does not list mail.
  - Content: the `SEARCH` icon, then `"Search mail"` in `muted`. Hover text `"Search · /"`.
  - It is only a button: a click emits `StartSearch`, and typing happens in the list pane's field.
  - Accessible name `"Search"`.

### Tests in `toolbar.rs`
Note that this module also holds two status-bar tests.
- `action_icons_are_drawn_at_the_icon_size`.
- `the_wordmark_lines_up_with_the_action_buttons`: centre y within 1 px.
- `the_actions_are_enabled_on_a_row_and_disabled_outside_a_folder`: in Rules, all actions are disabled and so is Search.
- `the_actions_are_disabled_while_the_move_picker_is_open`.
- `the_archive_button_archives_the_cursor_row`: sends `Apply{INBOX,[1],Archive}`, and the row is hidden optimistically.
- `in_the_archive_the_archive_button_moves_the_row_back_to_the_inbox`.
- `the_search_button_opens_search`.
- `the_status_bar_sync_button_syncs_every_account`: `SyncNow` goes to home and to work.
- `the_status_bar_shows_the_version`: label `"v0.0.0"`.

### 4. Status bar (`status.rs`)

- `VERSION` is `"0.0.0"` under `cfg(test)`, otherwise `CARGO_PKG_VERSION`, so snapshots stay stable (status.rs:12). [pure]
- `account_line` [pure] (status.rs:19-30):
  - `"{name}: {state}"`, where state is the error string if there is one, otherwise the Activity's `Display` (from `crate::sync`, [pure]), otherwise `"starting…"`.
  - With queued commands it appends `" · {n} queued"`.
- `progress` [pure]: for FetchingHeaders and FetchingBodies with total > 0, `done/total`. It draws a progress bar 160 wide after the line (status.rs:32, :146).
- `failed`: an error is set, or the activity is NotRunning (status.rs:44).
- `line_color`: the error colour when failed.
- `dot` (status.rs:62-81):
  - failed: a ring in the error colour;
  - Idle: a filled brass (`highlight`) dot;
  - Offline: a brass ring;
  - otherwise none.
  - The logic is [pure] apart from the `Color32` type, so parametrise the colour.
- `paint_dot`: a PAD×PAD cell that is always reserved, so the text lines up. Radius `PAD/2 - 1`, ring stroke 1.5.
- `sync_key()` returns `"Sync now · Cmd+R"` on macOS, otherwise `"Sync now · Ctrl+R"` (status.rs:103). [pure]
- Layout (`show`, status.rs:111-195):
  - Left: the `SYNC` icon_button, named `"Sync now"`, emitting `SyncNow`.
  - Then a column of lines at a 20 px pitch (`LINE`) with item_spacing.y = 0.
  - An optional global `app.error` line, in the error colour.
  - Each account line: dot, then text, then the progress bar.
  - **Every line is clickable** and emits `ToggleHistory`.
  - Right-to-left:
    - the update link, if `newer_release()`;
    - `"v{VERSION}"` in weak monospace;
    - the theme buttons, which read `System Light Dark` left to right. Each is a selectable label, selected when it matches `app.theme`, and emits `SetTheme`.
- Update link (status.rs:198-207): `"↑ v{version} available"`, monospace because the font needs the arrow, in the `highlight` hyperlink colour. It opens `update::release_url(version)`.
- History window `"History"` (status.rs:175-193):
  - Default width 520, scrollable, with a close X that emits `ToggleHistory`. Escape also closes it, which is handled in app.
  - When empty: weak text `"Nothing has happened yet."`.
  - Lines are newest first: `"{clock(at)}  {text}"`.
- Help window `"Keys"` (`show_help`, status.rs:230-252):
  - Not collapsible, with a close X that emits `ToggleHelp`. A two-column grid `"keys"`: the key in strong text, then the description.
  - `KEYS` [pure], verbatim (status.rs:210-228):
    - `"j / k, Down / Up"` — `"next or previous row"`
    - `"Right / Left"` — `"expand or collapse a thread"`
    - `"x"` — `"add the row to the selection, or take it out"`
    - `"e"` — `"archive"`
    - `"#, Delete (Backspace on macOS)"` — `"delete (to the Trash folder; from Trash or with no Trash folder, saved as .eml and deleted for good)"`
    - `"m"` — `"move to a folder"`
    - `"u"` — `"mark read or unread"`
    - `"s"` — `"flag or unflag"`
    - `"v"` — `"show HTML mail as text, or as HTML again"`
    - `"/"` — `"search this account; in All inboxes, every account"`
    - `"Esc"` — `"close a popup, leave search, clear the selection"`
    - `"Tab"` — `"next pane: folders, list, body"`
    - `"Ctrl+R (Cmd+R on macOS)"` — `"sync every account now"`
    - `"?"` — `"show these keys"`

### Tests in `status.rs`
Several of these test app event handling, not drawing.
- `an_up_to_date_account_gets_a_brass_dot_and_an_error_wins`.
- `fetching_headers_shows_the_counts`: `"work: INBOX headers 12,500 / 48,213"`. Note that the Activity `Display` groups with commas regardless of locale.
- `offline_shows_the_retry_time`: `"work: offline (timeout) · retry {clock}"`.
- `an_account_that_is_not_running_says_why_in_red_without_a_spinner`: `"work: not running (could not start its sync thread)"`; not busy; red ring.
- `an_error_shows_on_the_account_line_and_in_the_history`: control characters are cleaned to `"login failed[2J"`. Clicking the line opens history, which contains `"  work: login failed[2J"`. Escape closes it.
- `synced_reloads_the_shown_folder_and_its_unread_count`.
- `a_rule_applied_or_bodies_fetched_reloads_the_shown_folder`.
- `choosing_dark_applies_it_and_writes_it_keeping_comments`: the config keeps its `# test accounts` comment and gains `theme = "dark"`.
- `progress_ticks_stay_out_of_the_history`: history holds only `"work: up to date · {clock}"` (deduplicated) and `"work: boom"`.
- `offline_keeps_the_error_on_the_line`: the error wins over the Offline activity.
- `theme_buttons_read_system_light_dark_from_left_to_right`.
- `a_newer_release_links_to_its_notes`: opens `https://github.com/postvak-app/postvak/releases/tag/v9.9.9`.
- `no_newer_release_shows_only_the_version`.

### 5. Numbers (`numbers.rs`), all [pure]

- `separator_for(locale)` reads the language subtag, split on `- _ .`:
  - `'.'` for da, de, el, es, id, it, nl, pt, tr;
  - `','` for en, he, ja, ko, th, zh;
  - U+202F (narrow no-break space) for cs, fi, fr, nb, pl, ru, sk, sv, uk;
  - otherwise `None`, which gives plain digits.
- `group(n, sep)` inserts the separator every three digits.
- `count(n)` reads `sys_locale` once (OnceLock).
- Tests: `locales_pick_their_separator_and_unknown_ones_none` and `group_inserts_the_separator_every_three_digits`.

### 6. Icons (`icons.rs`)

- Meaning-to-glyph constants from Phosphor regular (icons.rs:4-24):
  - ACTIVITY=PULSE, ALL_INBOXES=STACK, ARCHIVE, BACKUPS=CLOCK_COUNTER_CLOCKWISE
  - CARET_DOWN, CARET_RIGHT, CHECK, DRAFTS=FILE_DASHED, FLAG, FOLDER, INBOX=TRAY
  - JUNK=WARNING_OCTAGON, MARK_UNREAD=ENVELOPE_SIMPLE, MOVE=FOLDER_SIMPLE_DASHED, RULES=FUNNEL
  - SAVE=DOWNLOAD_SIMPLE, SEARCH=MAGNIFYING_GLASS, SENT=PAPER_PLANE_TILT, SOURCE=CODE, SYNC=ARROWS_CLOCKWISE, TRASH
- They come from the `egui_phosphor` crate. In Slint, use the Phosphor font or SVGs with the same mapping.
- `for_special_use` [pure mapping]: INBOX by name (case-insensitive), then Archive, Drafts, Junk, Sent and Trash by special_use, otherwise FOLDER.
- Test: `special_folders_get_their_icon_and_others_a_folder`.

### Porting notes
- Many tests find widgets by their accessible names, for example `"Collapse Projects"`, `"Inbox (2)"`, `"Archive e"` and the `✔ ` prefix. Keep those names as `accessible-label` in Slint so the tests can be ported.
- Reuse unchanged:
  - `Row` and `ListState` (drop `search_id`), `build_rows`, `Optimistic`, `apply_pending`;
  - `display_name`, `list_date`, `columns`, `row_text`, `sender_width`, `offset_showing`, `merge_*`;
  - `tree`, `visible`, `shown`, `label`, `rank`, `is_special`, `sort`, `inbox_unread`, `count_and_name`;
  - `account_line`, `progress`, `failed`, `KEYS`, `sync_key`, `VERSION`;
  - all of `numbers.rs` and `icons::for_special_use`.
- Extract before reusing:
  - the move-picker filter;
  - `dot()`, made free of `Color32`.

## Body pane and HTML
Scope: `src/gui/body.rs`, `src/gui/html/{mod,render,net,view_tests}.rs`, plus the `app.rs` handlers they depend on (`BodyState`, `receive_html`, `html_width`, `html_visible`, `html_hover`, `html_select`, `save_*`, `open_source`).

**The `Pixels` and `Wake` types the task calls "already shared" do not exist yet.** I searched `src/` and `proto/` (excluding `target/`) and found neither. The only wake type is `gui::app::Waker = Box<dyn Fn() + Send>` (`app.rs:55`). The render thread is currently tied to egui in two places:
- It wakes the UI with `egui::Context::request_repaint` (`render.rs:31,114`).
- It returns frames as `egui::ColorImage`, and `Request::Select` carries `egui::Pos2` (`mod.rs:96,138`).

Those three things are what need extracting into toolkit-free types.

Legend: **[free]** means toolkit-free and reusable as is. **[free\*]** means toolkit-free apart from an egui type that is easy to replace. **[egui]** means it must be rewritten in Slint.

---

### 1. Header

- **[free]** `HEADER_DATE = "%A %-d %B %Y, %H:%M:%S (UTC%:z)"` (`body.rs:20`). The UTC offset stands in for a zone abbreviation because chrono has no portable one.
- **[free]** `SHORT_DATE = "%a %-d %b %Y, %H:%M"` (`body.rs:22`). It is shown beside the sender; `HEADER_DATE` appears as a hover tooltip (`body.rs:187`).
- Which date is used: `message.date`, falling back to `internaldate`, through `crate::time::local_time` (`body.rs:113-114`).
- **[free]** `name_and_address(from)` (`body.rs:233`):
  - The name comes from `list::display_name`.
  - The address is whatever sits inside `<…>`, trimmed.
  - The address is dropped when it is empty or equal to the name.
- **[free]** `initials(from)` (`body.rs:244`):
  - First alphanumeric of the first and last word of the display name, uppercased.
  - A single word gives one letter.
  - An empty result gives `"?"`.
- **[egui]** Layout (`body.rs:156-215`):
  - **Subject** is a wrapping heading in the `theme::SERIF` family, size `SUBJECT = 26.0`, colour `palette.heading`. It is omitted when the message has no subject. There is no "Subject" label.
  - **Avatar** is a circle of diameter `AVATAR = 2*INSET` (32), filled `palette.hover`. The initials are drawn centred at `AVATAR*0.4` in `palette.heading`. The avatar is vertically centred on the 1–3 header lines: `lines_height = lines*(ROW+spacing) - spacing`, and whichever side is shorter is pushed down by half the difference.
  - **Line 1:**
    - Sender name in `theme::BOLD`, `palette.strong`, truncating.
    - Address in weak text, truncating.
    - Short date right-aligned, monospace, `palette.muted`.
  - **Lines 2–3:** a weak `to` / `cc` label followed by the value, truncating. Each line appears only when the field is `Some`.
  - **Not a grid, on purpose:** grid cells do not truncate, which is regression `long_headers_truncate_in_a_narrow_reader`.
  - Every header string goes through `message::clean(s, false)`.

### 2. Empty and missing states (exact strings)

- `"No message selected."` when `app.body` is None (`body.rs:105`).
- `"This message is no longer in the local store."` when `body.message` is None (`body.rs:109`).
- `missing_text(account)` (`body.rs:308`):
  - `"Not downloaded; loads when {account.name} reconnects."` when `account.activity` is `Activity::Offline`.
  - `"Loading…"` otherwise.
  - Shown in place of the text, and as the disabled-hover text on the toolbar buttons.
- `"Rendering…"` while there is no `HtmlState` or no `page` yet (`mod.rs:233`).

### 3. Toolbar (`body.rs:261-306`)

- The row is right-aligned. Reading left to right it shows `[Text|HTML] [Save .eml] [View source]`, with View source rightmost.
- `icon_button(icons::SAVE, "Save .eml")` sends `SaveEml`; `icon_button(icons::SOURCE, "View source")` sends `OpenSource`.
- Both buttons are enabled only when `message.body_text.is_some()` ("downloaded"). While disabled they show `missing_text` on hover and never fetch anything (test `reader_actions_wait_for_the_download_and_never_fetch_on_their_own`).
- **Text | HTML switch**, shown only when `body.html.is_some()`:
  - Two selectable buttons with no gap between them.
  - Clicking the inactive side sends `ToggleHtml`, the same action as the `v` key (`app.rs:1018`).
  - The HTML side is disabled while `text_note` is `Some`, and shows the note on disabled hover.
- **[free]** `BodyState::shows_html() = html.is_some() && !show_text && text_note.is_none()` (`app.rs:97`).
- `ToggleHtml` flips `show_text`, but only when `html.is_some()` (`app.rs:918`).
- `show_text` belongs to the message and resets to false on the next message (`load_body`, `app.rs:1259`).

### 4. Attachments and saving

- Each attachment gets a row:
  - The label is `"{name} ({size})"`.
  - The name is `clean(name, false)`, or `"attachment {index}"` when the attachment has none.
  - A `"Save"` button sends `SaveAttachment(index)` (`body.rs:116-128`).
- **[free]** `size(bytes)` (`body.rs:87`):
  - `"{b} B"` below 1024.
  - `"{b/1024} KB"` (integer division) below 1 MiB.
  - `"{:.1} MB"` above that.
- The result note is `body.saved`, shown as a label under the attachments after `clean` (`body.rs:129`). **[free]** `saved_note` (`app.rs:1889`) produces:
  - `"Saved to {path}"`
  - `"Could not save: {e}"`
  - `"Could not save: the message is not downloaded"`
- Library functions: `message::save_attachment(raw, index, downloads)` and `message::save_eml(raw, subject, downloads)`, which writes `<downloads>/<sanitised subject>.eml` (the test expects `"Re- lunch.eml"`).
- A separator follows the attachments (`body.rs:132`).

### 5. Source window (`body.rs:349-419`)

- **[free]** `SOURCE_ROW = 1000`.
- **[free]** `Source::new(raw)` computes `clean(from_utf8_lossy(raw), true)` once, then the rows.
- **[free]** `source_rows(text)` returns one byte range per `\n`-separated line. Lines longer than `SOURCE_ROW` chars are split on char boundaries. An empty text gives `[0..0]`.
- `open_source` (`app.rs:1340`) sets `body.source`, or sets `saved` to `"Could not show the source: the message is not downloaded"`.
- **[egui]** A window titled `"Message source"`:
  - Closable, not collapsible, default size 720×480.
  - A `"Copy"` button copies `source.text`. That text is already cleaned, so a paste cannot replay escape sequences.
  - A virtualised (`show_rows`) monospace list scrolling both ways. Rows do not wrap (`extend`).
  - Closing it sends `CloseSource`.
- Escape closes it, after the move picker and before help (`app.rs:792`).
- Moving to another message drops `source`.
- It works in both the text and the HTML view.

### 6. Text view (`body.rs:316-347`)

- **[free]** `Segment { Link(&str), Text(&str) }`, `SCHEMES = ["http://","https://","mailto:"]`, `segments(line)`, `has_target(link)`, `next_link`. The linkifier rules:
  - A link starts only at a word start (the previous char is not alphanumeric).
  - The scheme must be followed by a non-whitespace char.
  - It ends at whitespace or `< > " '`.
  - Trailing `. , ; : ! ? ) ]` are trimmed.
  - A bare scheme left after trimming stays text.
  - `has_target` means one of the schemes plus a non-empty remainder. `html::openable` reuses it.
- The body text is `clean(body_text, true)` computed once on load (`app.rs:1898`).
- **[egui]** Rendering:
  - Font size `BODY_TEXT = 15.0`, proportional (test `the_message_text_is_larger_than_the_interface`).
  - Line spacing `PAD/2`, zero interact height, so a link does not make its line taller.
  - A blank or whitespace-only line renders as `" "`.
  - Each line wraps horizontally with no gap between segments.
  - A link is a hyperlink widget; clicking it opens the URL in a new tab.
- `text_note`, when present, is shown weak above the text (`body.rs:140`).
- The view scrolls vertically and does not auto-shrink.
- **Selection and copy of plain text:** egui labels are selectable by default; there is no custom code for it. Slint needs explicit selectable text (for example a read-only `TextInput`) to keep this.
- A perf note in the code: every line is laid out every frame.

### 7. Read timer and fetch (behaviour exercised from `body.rs` tests; logic lives in `app.rs`)

- A missing body is fetched once per message with `FetchBody`, and is shown when `BodyReady` arrives.
- A failed fetch is retried when the message is shown again.
- `shown_at` resets when the text first appears, and the read delay of one second starts from there.
- Mark-read fires only when the message is "armed": chosen by the user, not put on screen by startup, a folder switch, a sync or a search being typed.
- Marking read or unread by hand sets `read_sent`, so the delay does not act again.
- The row after an archive is armed.
- Clicking the message already shown arms it.

### 8. HTML view: constants and pure logic (`html/mod.rs`)

- **[free]** Constants:

  | Constant | Value | Meaning |
  |---|---|---|
  | `STRIP` | 512 | strip height in physical px |
  | `MIN_WIDTH` | 320.0 | minimum layout width in points; narrower panes scroll sideways |
  | `MAX_HTML` | 2 MiB | larger HTML shows as text |
  | `MAX_HEIGHT` | 200_000.0 | a taller laid-out page shows as text |
  | `SETTLE` | 0.1 s | how long a new width must hold still before relayout |

- **[free]** Notes:
  - `TOO_LARGE = "Too large to render; showing text."`
  - `FAILED = "Could not render HTML; showing text."`
  - `"Remote content not loaded."` is shown weak above the page when `state.remote` is set (`mod.rs:237`).
- **[free]** `html_of(raw)` returns `message::html_body` only when the `html` feature is on.
- Without the feature, the `Renderer` stub never renders (`mod.rs:43-57`).
- **[free]** `too_large(html)` sets `text_note` on load, and again whenever `body.html` changes (`app.rs:1211`, `:1883`).
- **[free]** `wanted_strips(top,bottom,height,scale)` returns the visible strips plus one screen on either side, clamped to the page. A strip spans `STRIP/max(scale,0.5)` points.
- **[free]** `far_strips(...)` returns strips more than three screens away, whose textures are dropped.
- **[free]** `openable(href)` trims the href and allows it only when `has_target` holds.
- **[free\*]** `Load`, `Request` (Load / Paint / Hit / Select), `Reply` (Failed / Laid / Link / Selected / Strip) and the `generation()` accessors. To make them toolkit-free:
  - Swap the `egui::Pos2` in `Select` for `(f32,f32)`.
  - Swap the `egui::ColorImage` in `Strip` for raw RGBA bytes plus a size: a `Pixels`-like type that still has to be written.
- **[free\*]** `HtmlState` holds `generation`, `hover`, `hovered_at`, `page`, `remote`, `requested`, `selected_at`, `selection`, `sent:(scale,width)`, `strips:BTreeMap<u32,TextureHandle>`, `wanted`. Only `TextureHandle`, `Pos2` and `Vec2` are egui.

### 9. HTML view: drawing and interaction (`mod.rs:227-316`) **[egui]**

- The width is `max(available, MIN_WIDTH)`.
- Every frame emits `HtmlWidth{scale: pixels_per_point, width}`.
- The page rect is `(max(page.x,width), page.y)`:
  - Filled white.
  - Each strip is drawn at `y = strip*STRIP/scale` with size `texture/scale`.
  - A 1 px inside stroke uses `widgets.noninteractive.bg_stroke`.
- The scroll area scrolls both ways and does not auto-shrink (`body.rs:135`).
- The visible slice emits `HtmlVisible{top,bottom}` in page points.
- **Hover:**
  - The pointer position in page coordinates emits `HtmlHover(pos)`, only when it differs from `hovered_at`. Leaving the page sends `None`, which clears `hover`.
  - Over a link: a tooltip at the pointer showing `clean(href)` and the PointingHand cursor.
  - Elsewhere on the page: the Text cursor (asserted in a test).
- **Click:** opens the URL only if `openable(href)` (https, http or mailto). `javascript:` and relative links show their tooltip but do nothing.
- **Selection** (`select`):
  - A drag sends `HtmlSelect{from: press_origin, to: pointer}`.
  - A click while a selection exists sends `from == to`, which clears it.
  - Nothing is sent when the pair equals `selected_at`.
  - The Copy event copies `state.selection`, but only when no widget has keyboard focus.

### 10. App-side HTML state machine (`app.rs:1352-1520`)

- **Replies:** any reply whose generation differs from `html_view.generation` is dropped (test `a_reply_for_an_older_layout_is_dropped`).
- **`Failed`:** sets `text_note = FAILED` and `html_view = None`.
- **`Laid`:**
  - When `height > MAX_HEIGHT`: `text_note = TOO_LARGE` and the view is dropped.
  - Otherwise: sets `page` and `remote`, and clears strips, requested, hover, hovered_at, selected_at and selection.
- **`Link`:** sets `hover`.
- **`Selected`:** sets `selection` and clears `requested`, so the strips are repainted with the highlight. The old strips stay on screen until then.
- **`Strip`:** uploads a texture named `html-{gen}-{strip}` with LINEAR filtering.
- **`html_width`:**
  - Runs only when `shows_html()`.
  - The first call lays out.
  - A different `(scale,width)` is recorded in `wanted` together with a timestamp, and a repaint is scheduled after `SETTLE`. The new layout happens once that value has held for at least `SETTLE`.
  - Returning to the `sent` value cancels `wanted`.
  - Each layout increments `html_generation`.
  - The renderer is started lazily: no renderer exists for plain-text mail or HTML that is too large (tests check `renderer.is_none()`).
- **`html_visible`:** sends `Paint` for each wanted strip not yet in `requested`, and evicts far strips from both `strips` and `requested`.
- **`html_hover` / `html_select`:** record `hovered_at` / `selected_at`, then send `Hit` / `Select`.

### 11. Render thread (`render.rs`). Logic is free apart from the waker and image type.

- The thread is named `"html-render"`.
- A one-time panic hook replaces Blitz panic messages from that thread with a log line, because the messages can quote the mail: `log::warn!("the HTML renderer failed; showing text instead")`. Other threads keep the previous hook.
- `Renderer::start` logs `"could not start the HTML renderer: {e}"` if spawning fails.
- `send`: if the thread has died and the request is a `Load`, a new thread is started and the request resent.
- `replies()` drains with `try_iter`.
- **Batching in `run`:**
  - Everything before the last `Load` in a batch is discarded.
  - Then only the newest request per `kind` is kept: Load; Paint per strip; Hit; Select.
  - Each job runs inside `catch_unwind`. A panic drops the document and replies `Failed{generation}`.
  - After each reply the UI is woken (`ctx.request_repaint`). This is the hook to replace with a `Waker`.
- **`layout`:**
  - `scale = max(0.5)`, and the viewport is `physical(width)` × `physical(800)`, light scheme.
  - `base_url = "postvak://mail/"`, so relative URLs resolve to a URL the net provider refuses (Blitz panics on unresolvable ones).
  - User-agent CSS is `DEFAULT_CSS` plus `MAIL_CSS = "img { max-width: 100%; height: auto; } body { overflow-wrap: anywhere; }"`.
  - `font_colours`, then `handle_messages`, then `resolve`.
  - The laid width is `max(overflow.right, width)` and the height is `max(size.height, overflow.bottom)`.
  - `remote` is read from the provider's flag.
- **`font_colours`:**
  - Copies `<font color>` into `style` as `color: X; <existing style>`.
  - Applies only when the value is non-empty and contains just `#` and ASCII alphanumerics, which prevents injection.
- **`paint`:**
  - Checked strip arithmetic; a strip past the end returns `None`.
  - Sets the viewport scroll to `top/scale`.
  - Renders with vello_cpu.
  - Returns premultiplied RGBA, after checking the byte length.
- **`select`:**
  - `from == to` clears the selection.
  - A `to` that is off text keeps the previous selection.
  - Returns `get_selected_text`.
- **`link_at`:** the `href` of the innermost `<a>` ancestor of the hit node.
- **`physical`:** `ceil(points*scale) as u32`, which saturates; NaN becomes 0.
- **`fonts`:** system fonts, except in tests, where egui's bundled `Ubuntu-Light` is used. A Slint port needs another deterministic test font.

### 12. Net provider (`net.rs`), fully [free]

- `Inline::new(parts)` returns the provider and an `Arc<AtomicBool>` "refused remote" flag.
- **`cid:`** percent-decodes the path and matches it exactly against `InlinePart.cid`.
- **`data:`** is decoded with the `data_url` crate.
- **`http` / `https`:** sets the flag and returns nothing.
- **Anything else** (`file:`, `postvak:`) returns nothing.
- Every request is answered synchronously, with empty bytes when refused, so a `<head>` stylesheet does not block painting.
- `percent_decode` keeps a `%` that is not followed by two hex digits.

### 13. Tests to port or keep

**Pure tests; keep as is:**
- `body.rs`:
  - `the_header_date_is_full_with_seconds_and_offset` expects `"Monday 21 September 2026, 14:13:20 (UTC+00:00)"`.
  - `the_short_date_beside_the_sender_keeps_day_and_minute` expects `"Mon 21 Sep 2026, 14:13"`.
  - `segments_link_only_http_https_and_mailto_at_word_starts`: punctuation trimming, parentheses, and `javascript:`, `file:`, `xhttps:`, `https:// spaced`, `ftp:`, `http://!`, `mailto:)`, `mailto:.` all stay text.
  - `a_bare_scheme_after_trimming_is_not_a_link`
  - `size_is_human_readable`: 512 B, 2 KB, 3.3 MB.
  - `initials_come_from_the_name_else_the_address`: LE, LA, AK, B, É, ?.
  - `the_sender_splits_into_name_and_address`
  - `source_rows_split_lines_and_long_lines_on_char_boundaries`
- `html/mod.rs`:
  - `wanted_strips_cover_the_view_and_a_screen_either_side`: 1..4, 0..4, 0..1.
  - `far_strips_are_three_screens_away`: [0,1,2,3,4,11].
  - `only_absolute_http_https_and_mailto_links_open`
- `net.rs`:
  - `cid_and_data_resolve_from_memory`
  - `everything_else_is_answered_empty_and_remote_is_noted`
  - `percent_decode_keeps_stray_percent_signs`
- `render.rs`; these need an image type with no egui in it:
  - `a_newsletter_lays_out_paints_and_finds_its_links`: width 700–800, height over 500, remote set, strip of 700×512, pixel (10,10) is `#f2f4f6`, blue header, more than 20 colours in strip 1, a stale generation is ignored, the CTA hit is `https://example.com/cta`.
  - `font_colour_attributes_colour_the_text`
  - `a_drag_selects_text_and_paints_the_highlight`
  - `a_batch_keeps_only_the_newest_selection_paint_and_hit` expects `["load","select 2","paint 0"]`.
  - `a_strip_past_the_end_is_not_painted`
  - `relative_urls_and_a_bare_page_do_not_panic`
  - `queued_loads_lay_out_only_the_newest`
  - `a_panic_drops_the_document_and_replies_failed`
  - `physical_pixels_round_up_and_saturate`
  - proptest `hostile_html_never_panics`, with `PIECES` as the corpus.
  - Ignored known bugs: `a_table_cell_shown_as_a_block_is_painted` (Blitz td display:block), `a_huge_font_in_a_tiny_width_lays_out` (parley), `huge_geometry_paints` (vello_common).

**UI tests to re-express for Slint:**
- `body.rs`:
  - `the_message_text_is_larger_than_the_interface`
  - `the_subject_is_a_heading_above_the_sender`
  - `the_cursor_message_shows_headers_and_text`
  - `long_headers_truncate_in_a_narrow_reader` (900 px wide)
  - `an_https_link_opens_in_the_browser`
  - `a_missing_body_is_fetched_once_and_shown_when_ready`
  - `a_failed_body_fetch_is_retried_when_the_message_is_shown_again`
  - `a_fetched_body_keeps_the_read_timer_and_the_hand_marks`
  - `marking_another_row_does_not_stop_the_shown_message_being_read`
  - `offline_says_when_the_body_will_load`
  - `a_message_is_marked_read_after_one_second_on_screen`: nothing at 0.9 s, read at 1.1 s, sent once.
  - `marking_unread_by_hand_is_not_undone_by_the_delay`
  - `save_writes_the_attachment_to_downloads`
  - `a_sync_that_adds_newer_mail_keeps_the_cursor_on_its_message`
  - `the_message_shown_at_startup_is_not_marked_read`
  - `the_message_shown_after_switching_folders_is_not_marked_read`
  - `an_opened_message_is_marked_read_a_second_after_its_body_arrives`
  - `the_row_after_an_archive_is_marked_read_after_a_second`
  - `a_search_hit_is_not_opened_while_the_query_is_typed`
  - `clicking_the_message_already_shown_opens_it`
  - `a_reply_in_the_cursor_thread_keeps_the_cursor_on_that_thread`
  - `the_read_delay_starts_when_the_body_text_appears`
  - `save_eml_writes_the_raw_message_to_downloads`
  - `reader_actions_wait_for_the_download_and_never_fetch_on_their_own`
  - `the_source_window_shows_the_raw_message_cleaned_and_copies_it`: ESC/BEL stripped, copied as `"From: a@example.com\nSubject: ]0;pwnedreport\n\nline one\n"`, Escape closes.
  - `a_megabyte_line_opens_as_short_rows`
  - `moving_to_another_message_closes_the_source`
  - `the_text_html_switch_toggles_like_v_and_hides_without_html`
  - `html_too_large_to_render_disables_the_html_side`
  - `the_source_window_shows_the_raw_message_in_either_view`
- `view_tests.rs`, using the helpers `raw`/`add_html` (multipart/related with a `cid:dot@example.com` PNG), `wait_for` (30 s deadline), `painted`, `click_page`, `page_link`, and `Faked` (built on `Renderer::fake`):
  - `html_mail_shows_as_html_and_v_switches_that_message_to_text`: remote note present, `v` toggles, the toggle resets on `j`, no note for the plain `<p>`.
  - `v_on_plain_text_mail_changes_nothing`: `show_text` stays false and no renderer exists.
  - `an_https_link_opens_and_shows_its_target`
  - `a_javascript_link_does_nothing`
  - `html_over_the_limit_shows_text_with_a_note`
  - `a_render_failure_shows_text_with_a_note`
  - `a_reply_for_an_older_layout_is_dropped`
  - `a_page_over_the_height_limit_shows_text_with_a_note`
  - `a_new_width_lays_out_again_once_it_held_still`: no Load straight after a resize, `wanted` is set, exactly one Load (`first+1`) after the settle.
  - `a_drag_selects_page_text_and_copy_copies_it`: Text cursor, a drag gives a selection containing "word", Copy copies exactly that selection.

## Rules views, startup window, theme, snapshots, docs, CI and packaging
**Tags.** **[pure]** means the code has no toolkit in it and can be reused as it is. **[egui]** means it must be rebuilt in Slint. **[spec]** means the rule comes from the docs or specs.

**Note:** `src/gui/fixture.rs` does not exist. The fixture is `Fixture` in `src/gui/test_support.rs`.

### 1. Rules view (`src/gui/rules.rs:56-139`)

**Pure, reuse as is:**
- `RulesState::load(path, previous)` (`:31-42`). If the file parses, its rules are listed. If it does not compile, the error is shown and the rules are still listed, so a broken rule can be switched off. If the file does not parse, the error is shown and the **previous** rules stay.
- `is_pending` (`:45`) is `!enabled && proposed_by.is_some()`.
- `toml_source` (`:49`) writes a one-rule `RuleFile` back to TOML.

**Behaviours [egui, rebuild]:**
- **Error banner** in `error_fg_color`, exact text `"{error} — sync keeps the previous rules until this is fixed"`, with the error passed through `clean(_, false)` (`:58-66`).
- **Button `"Open rules.toml"`** returns `UiAction::OpenRulesFile` (`:67`).
  - `app.rs:1654-1680` creates an empty file if there is none. That is the only allowed write exception in `tests/architecture.rs:17-21`.
  - It then runs `open -t` on macOS or `xdg-open` on Linux and waits on a separate thread, logging a warning on a non-zero exit.
  - If it fails, the error line is `"could not open rules.toml: {e}"`.
- **Vertical scroll area** (auto_shrink false).
- **Heading `"Proposals"`** appears only when something is pending. Each proposal sits in a group frame with inner margin `PAD`:
  - a bold line `"{name} · proposed by {by}"`
  - the TOML in monospace, `clean(_, true)` so newlines are kept
  - buttons `"Approve"` (`ApproveRule`) and `"Reject"` (`RejectRule`)
- **Heading `"Rules"`.** When there are no rules, the weak text `"rules.toml has no rules yet."` shows.
- **Rules grid**, striped, 3 columns, column gap `INSET`:
  - a checkbox labelled with the rule name, which returns `SetRuleEnabled(name, bool)`
  - `"{account} · {folder}"`, defaulting to `"every account"` and `"INBOX"`
  - `"proposed by {by}"` or empty

**What `app.rs` does with these actions** (in the new app layer):
- **Approve** calls `rules::edit::approve_from_now(paths, accounts, name, now)`. It restarts the rule clock so the rule ignores older mail.
- **Reject** calls `rules::edit::reject`.
- **The switch** calls `rules::edit::set_enabled`.
- After any of the three, `rules_changed()` reloads the view (`app.rs:1640-1652`). An error goes to the status history.
- The GUI sends **no** SyncNow; the daemon notices the change itself. Spec §8 still says SyncNow, so the spec is stale.

**File polling** (`app.rs:43,1627-1638`):
- Every `POLL = 2.0` s, using input time, the app compares the modification times of rules.toml and config.toml.
- A request for a repaint after 2 s keeps the polling alive (`app.rs:507`).
- When config.toml changes, a top banner shows in `warn_fg_color` with the exact text `"config.toml changed — reopen the window to show it"` (`app.rs:462-468`). Spec §8 says "restart Postvak to apply", which is stale.
- The theme switch saves the new modification time, so it raises no banner (`app.rs:870-880`).

### 2. Activity view (`rules.rs:149-163, 199-231`)

**Pure:** `activity_log(accounts)` reads `store.log(500)` for each account and skips accounts whose store failed to open. A failed read logs `"[{account}] could not read the log: {e}"`. The result is sorted newest first and cut to `LOG_LIMIT = 500`. It returns `Vec<(account, LogEntry)>`.

- **Empty state**, weak text: `"No rule or action has run yet."`
- **Grid**, striped, 6 columns, scrolls both ways. Headings in bold: `Time, Account, Rule, Action, Folder, Subject`.
  - Time uses `local_time(at, "%Y-%m-%d %H:%M")`.
  - Every other cell is passed through `clean`.
  - Spec §8 lists a "sender" column; the code has Folder instead.
- **Load moment:** loaded in `reload_view` only when the view is selected or marked dirty (`app.rs:1078`), never on every frame.

### 3. Trash view, labelled "Backups" in the folder pane (`rules.rs:141-197, 233-264`)

**Pure:**
- `trash_rows(accounts, paths)` runs `Trash::list` for each account and sorts by `saved_at`, newest first.
- `eml_summary` reads only the first `HEADER_BYTES = 64 KiB` of each file and parses From and Subject. These are the user's own backup files; only the headers are read.
- A listing error logs `"[{account}] could not list the trash: {e}"`.

**Behaviours [egui, rebuild]:**
- **Empty state:** `"Trash is empty. Deleted mail is kept here as .eml for the account's retention period."`
- **Grid**, striped, 6 columns. Headings: `Deleted, Account, Folder, From, Subject, ""`. Each row ends with a `"Restore"` button.
- **Restore** (`app.rs:846-853`):
  - The row is removed straight away, before the daemon answers.
  - The app sends `Command::Restore{file}`.
  - If the send fails, it records `DAEMON_LOST` and reloads the view.
  - On `Event::Restored` the history gets `"{account}: restored to {folder}"` and the view refreshes (`app.rs:716-723`).
- **Folder pane entries** (`folders.rs:16-18`): `"Rules"`, `"Activity"`, `"Backups"` with icons `RULES`, `ACTIVITY`, `BACKUPS`.

### 4. Startup error window (`src/gui/startup.rs`, `src/gui/mod.rs:44-160`)

**Pure:**
- `ACCOUNTS_DOCS = "https://postvak.pataar.nl/accounts.html"`
- `NoAccounts` displays `"no accounts configured; run `postvak account add`"`.
- `Problem{summary, fix, detail, link}` and `Problem::of(error, log)`. `of` finds `NoAccounts` even when it is wrapped in context.

**The three kinds of problem:**

| Case | summary | fix | detail | link |
|---|---|---|---|---|
| No accounts | `"Postvak has no mail account set up yet."` | ``"Run `postvak account add` in a terminal, then open Postvak again."`` | none | `("Setting up an account", ACCOUNTS_DOCS)` |
| Other error, log path known | `"Postvak could not start."` | `"The background sync logs to {log}"` | `clean("{error:#}")`, the context chain joined with ": " | none |
| Other error, no paths | `"Postvak could not start."` | ``"Run `postvak gui` in a terminal to see more."`` | the error | none |

**Window contents [egui]**, top to bottom:
1. The heading, which is the summary
2. 8 px space
3. The fix
4. A hyperlink, when there is one
5. 8 px space
6. The detail in the palette's error colour, inside a vertical scroll area capped at the available height minus 40
7. 12 px space
8. A `"Close"` button, which sends a viewport Close

**Window settings:**
- 520×260, title "Postvak", app id `io.github.postvak_app.postvak`, the window icon.
- The window **persists nothing**: `persist_window: false`, an empty `persistence_path`, and no egui memory.
- `theme::install` runs before it opens.
- After the window closes, the error is returned so it reaches stderr and the exit code.
- If no window can be shown (no display), stderr is all there is.

**Mail window settings** (`mod.rs:156-175`):
- 1280×800.
- Window size, position and pane widths are saved in `state_dir/window.ron`; `docs/src/gui.md:9` documents this. Slint has no equivalent, so it needs its own persistence.
- On macOS the integrated GPU is preferred, with `WGPU_POWER_PREF` overriding it (`mod.rs:79-95`).
- Events can arrive before the window exists; a `OnceLock<Context>` repaint hook covers that.

**Tests** in `startup.rs`:
- `no_accounts_points_to_account_add_and_the_docs`
- `no_accounts_is_found_under_context`
- `other_errors_show_their_text_and_the_daemon_log`: detail is `"could not start the daemon: socket refused"`
- `without_paths_the_fix_is_to_run_it_in_a_terminal`
- `the_window_states_the_missing_account`
- `the_docs_link_opens_the_account_setup_page`: an OpenUrl for ACCOUNTS_DOCS is emitted
- `the_window_shows_the_error_and_the_daemon_log`
- `close_asks_the_window_to_close`

**Tests** in `mod.rs`:
- `macos_windows_prefer_the_integrated_gpu`
- `the_icon_is_a_square_png_with_transparent_corners`: 512×512, top-left alpha 0
- `the_window_icon_is_a_valid_png`: on macOS the Dock icon needs Apple's transparent margin
- `the_error_window_saves_nothing`
- `the_desktop_entry_names_the_app_id`: the `.desktop` file's `Icon=` and `StartupWMClass=` equal `APP_ID`. **A Slint window must set the same Wayland app id and X11 WM_CLASS.**

### 5. Theme: Ossenbloed (`src/gui/theme.rs`)

### Grid constants (pure; AGENTS.md requires these instead of literal sizes)

| Constant | Value | Meaning |
|---|---|---|
| `CONTROL` | 28 | Height of buttons and fields; width of icon buttons |
| `TOP_BAR` | 40 | Toolbar height |
| `ROW` | 24 | Rows, text buttons, fields |
| `PAD` | 8 | Space inside a row |
| `INSET` | 16 | Space from a pane's edge to content |
| `ICON` | 16 | Icon size |
| `GLYPH` | 6 | (CONTROL−ICON)/2 |
| `RADIUS` | 10 | Pane corner radius where panes meet the frame |

Spacing set in `install` (`:249-266`):
- button padding (PAD, 3), so a text button is exactly ROW tall
- `interact_size.y = ROW`
- vertical item spacing PAD/2, which makes the folder row pitch ROW + 4
- window margin INSET − 1, to allow for the 1 px outline

### Palettes (pure data)

The `hex()` helper is egui-specific. The values below are what to carry over.

| field | PAPER (light pane) | NIGHT (dark pane) | FRAME | FRAME_NIGHT |
|---|---|---|---|---|
| accent | 9a4a1c | e9c9b0 | d9b36a | d9b36a |
| background | fbf8f3 | 231919 | 3b1418 | 2a0e11 |
| border | e3d8cb | 3a2b2c | 5a2a2f | 4a2226 |
| error | a8202e | f08a8a | ffb4a8 | ffb4a8 |
| faint | fffdfa | 140e0f | 4e2629 | 3b1d20 |
| heading | 3b1418 | e9c9b0 | f1e4d8 | f1e4d8 |
| highlight | 8c6720 | c99a3e | d9b36a | d9b36a |
| hover | eadfd0 | 2e2223 | 4a1d22 | 3a171b |
| muted | 6e5958 | b09a93 | c29a92 | c29a92 |
| on_accent | f1e4d8 | f1e4d8 | ffffff | ffffff |
| panel | f3ece2 | 1a1213 | 3b1418 | 2a0e11 |
| pressed | e3d8cb | 3a2b2c | 5e2a30 | 4c2227 |
| secondary | 5e4a4b | cdb8af | e0cbbe | e0cbbe |
| selected | 3b1418 | 5a1f25 | 552a2b | 44262a |
| strong | 140b0c | ffffff | ffffff | ffffff |
| text | 2b1a1c | efe3d6 | f1e4d8 | f1e4d8 |
| warning | 8a5a00 | e6b85c | f2cc7a | f2cc7a |

**Rules that matter:**
- Each palette's `panel` value is unique, because `palette(ui)` finds the palette a ui is drawn in by its panel fill. That is an egui-era trick; in Slint, pass the palette explicitly or use a global.
- `chrome_of` maps PAPER to FRAME and NIGHT to FRAME_NIGHT. The frame always uses the dark-mode visuals.
- `chrome_frame` is filled with the frame's panel colour and has no outline.

**Visuals mapping** (`visuals()` at `:141-181`, egui but the intent carries over):
- `hyperlink` = accent
- `selection.bg` = selected; `selection.stroke` = on_accent, so selected text stays readable
- `panel_fill` = panel; `window_fill` = background; `window_stroke` = border
- `extreme_bg` and `faint_bg` = faint. This is the text-field fill; the snapshot test looks for `PAPER.faint`.
- Weak text = muted
- No shadows anywhere: popup and window shadows are NONE
- Widgets:
  - inactive and hovered fill = hover
  - hovered stroke = accent
  - active (pressed) fill = pressed, with foreground = strong. Bold text uses this colour, so `strong` must read on `pressed`; regression #41.

**Helpers to reproduce:**
- `heading_row`: a row CONTROL tall, so a pane heading shares a line with the next pane's top actions.
- `grid`: column gap INSET.
- `pane(focused)`: a 1 px accent outline when the pane has keyboard focus, with the margin reduced by 1 so content does not move.
- `text_field`: ROW tall, PAD horizontal margin, text vertically centred, and a 1 px accent focus ring drawn over egui's own.

### Fonts (`:184-246`)

| Role | Font | File |
|---|---|---|
| Text | Hanken Grotesk Medium | `assets/fonts/HankenGrotesk-Medium.ttf` |
| Bold family | Hanken Grotesk SemiBold | `HankenGrotesk-SemiBold.ttf` |
| Monospace | Spline Sans Mono Medium, then egui's **Hack** behind it for arrows | `SplineSansMono-Medium.ttf` |
| Serif family (headings, wordmark) | Newsreader Medium | `Newsreader-Medium.ttf` |
| Icons | Phosphor Regular (`egui-phosphor`) | — |

- Phosphor is the **first fallback** after the text font, so icons mix into ordinary labels.
- Bold and serif fall back to the text font.
- egui's Ubuntu-Light is removed. The text style Heading uses the serif family.
- The OFL licence files sit beside the fonts.
- **Slint gaps:**
  - It needs Phosphor as a font file, or as SVG icons.
  - It needs a replacement for Hack's arrow glyphs. The update link `"↑ v{version} available"` is monospace for that reason (`status.rs:197-206`).
  - The `?` key table spells keys out because the old fonts lack some glyphs.
  - Slint can choose font weights, so the static instances could become variable fonts. That is optional.

### Theme tests (`theme.rs:352-586`)

Fonts:
- `the_icon_font_falls_back_behind_the_text_font`: Phosphor is in second place
- `text_is_hanken_grotesk_and_monospace_spline_sans_mono_over_hack`
- `headings_and_bold_text_fall_back_to_the_text_font`: the Heading style resolves to the serif family

Palette lookup:
- `palette_finds_the_pane_or_chrome_palette_a_ui_is_drawn_in`: also checks that calling `chrome` twice is idempotent
- `panel_colours_tell_the_palettes_apart`
- `the_app_installs_both_pane_palettes_flat`: no shadows; dark and light visuals take the NIGHT and PAPER values

**Contrast tests are pure WCAG maths over the hex values; keep them:**
- `strong_text_stands_out_from_normal_text_in_every_palette`: strong is at least 7:1 on panel and window, stronger than normal text, and at least 4.5:1 on pressed (#41)
- `text_on_the_selection_links_and_status_text_pass_aa_in_every_palette`:
  - on_accent on selected, heading on background, accent on background: each at least 4.5:1
  - muted and error on both panel and hover: at least 4.5:1
  - highlight on panel: at least 3:1
- `links_stand_apart_from_text_in_every_palette`: accent against text at least 1.2:1
- `light_strong_text_and_pressed_widgets_keep_their_colours`

Text fields:
- `selected_text_in_text_fields_is_readable_in_every_palette`
- `focused_text_fields_get_an_accent_focus_ring`: the ring's rect equals the field frame's rect

### 6. Snapshots (`src/gui/snapshots.rs`, `tests/snapshots/*.png`)

**How they run:**
- Comparison runs only with `UPDATE_SNAPSHOTS` or `POSTVAK_SNAPSHOTS=1`. Otherwise the views are built and the assertions run, but nothing is rendered.
- The local UTC offset must be 0, both now and at the fixture time; otherwise the test panics with "run them with TZ=UTC".
- References are drawn by lavapipe.
- `AT = 1_790_000_000`, which is 2026-09-21 13:46 UTC.

**Fixtures:**
- `mailbox(theme)`: accounts home and work; `[ui] theme`.
- Folders in work: Archive, Sent, Trash, Receipts, and nested Projects/Postvak and Projects/Website with "/" as delimiter. Home has Archive.
- Messages in work INBOX:

| uid | From | Subject | Notes |
|---|---|---|---|
| 90 | Ada Lovelace | "Notes on the analytical engine" | read |
| 91 | Example Shop | "Your receipt for order 1042" | read |
| 92 | Weekly News | "This week: ten things about tea" | unread |
| 93 | Grace Hopper | "Planning the offsite" | thread `<offsite@example.com>` |
| 94 | Alan Turing | "Re: Planning the offsite" | flagged, same thread |
| 95 | Linus Example | "Lunch on Friday?" | unread, body has a URL |

- Home has uid 96, from Mum, "Photos from the weekend".
- `open()` sends home Offline ("no network", retry at AT+300) and work Idle, so the status bar holds still.

| Test | PNG | Shows / asserts |
|---|---|---|
| inbox_light / inbox_dark | gui_inbox_light/dark | work INBOX, newest message open; the text "Shall we try" is present |
| all_inboxes | gui_all_inboxes | dark; labels "home · Mum" and "work · Linus Example" |
| inbox_with_marks_and_an_expanded_thread | gui_inbox_thread | keys x, j, x, Right; 95 and 94 marked; members 94 and 93 shown |
| keys_help | gui_keys_help | `?` opens help |
| rules | gui_rules | the three-rule RULES fixture; "Approve" present |
| activity | gui_activity | 3 log entries (archive, "move Receipts", gui "flag") |
| trash | gui_trash | "Your verification code" backup; "Restore" present |
| html_light / html_dark / html_narrow (feature html) | gui_html_* | newsletter uid 99 at 1280 wide, and at 1060 wide (narrow stacks the columns) |

**Geometry tests that run without a GPU** (Slint needs an equivalent way to read geometry):
- `the_inbox_lines_up_on_the_grid`:
  - the toolbar's archive glyph lines up with the list content edge (list left + INSET)
  - the list title "Inbox" starts there too
  - the archive button is vertically centred in the toolbar
  - the sync glyph lines up with the Rules row + PAD
  - the folder row is ROW tall and the pitch is ROW + PAD/2
  - the focus outline moves no content
- `headers_fields_and_popups_line_up_on_the_grid`:
  - the list title, "View source" and "All inboxes (3)" are vertically centred on one line
  - the avatar circle has radius INSET and is centred between the sender and "to" lines
  - the content of the `?` window sits INSET in
  - the search field is ROW tall, filled `PAPER.faint`, with the hint "Search this account" PAD in
- `with_one_account_the_first_folder_and_first_message_share_a_line`

### 7. Test support (`src/gui/test_support.rs`)

The fixture logic itself is pure and reusable; only `harness()` is egui-specific.

- **`Fixture::new(accounts)`:**
  - a temp home under `Paths::under`
  - config entries with host `imap.example.com`, username `{name}@example.com`, `password = { keyring = true }`, preceded by the comment `# test accounts`
  - an empty INBOX per account
- **Other helpers:** `append_config`, `store`, `folder`, `add`.
- **`harness()` [egui]:**
  - wraps `Client::in_memory` in `session`
  - replaces `reconnect` and `fetch_release` with functions that always fail
  - points the downloads dir at `cache_dir/downloads`
  - 1280×800, 10 ms per step
- **`Wires{commands, events}` and `sent()`.** Dropping `events` looks like the daemon going away.
- **`message(folder, uid, subject)`:** read, alone in its thread, time = AT + uid·60, body `"Body of {uid}"`.

**rules.rs tests** (behaviour; each needs a Slint equivalent):
- `a_proposal_shows_its_toml_and_approve_enables_it`:
  - the rule clock moves past 1
  - the `# keep me` comment is kept
  - no command is sent
  - the Approve button goes away
- `the_switch_turns_a_rule_off_and_keeps_comments`
- `an_edit_from_outside_reloads_the_view`: modification time +5 s, input time 5.0
- `a_parse_error_shows_a_banner_and_keeps_the_rules`
- `a_config_change_asks_to_reopen_the_window`
- `the_theme_switch_raises_no_banner`
- `activity_merges_every_account_newest_first`
- `trash_lists_backups_and_restore_sends_the_command`: the row is gone before the daemon answers; the history gets "work: restored to INBOX"
- `question_mark_shows_the_keys_and_escape_closes_them`

### 8. Update notice (`src/update.rs`, GUI only)

**Pure; keep it as is.** `lib.rs:21` gates it behind `#[cfg(feature="gui")]`.
- `parse_version` accepts only plain X.Y.Z, with or without a leading v.
- `latest_from_json` skips drafts and prereleases.
- `newer`, `release_url`.
- `due`: no earlier check, a day has passed, or the clock went backwards.
- `load`, `save`.
- `check`: a failed fetch still counts as the day's check and keeps the version seen before.
- `fetch`: rustls over HTTP/1.0 to `api.github.com`, 10 s timeout, user agent `postvak/<ver>`.
- `body_of`.
- It has 10 unit tests (`:174-361`).

**In the app layer:**
- `UpdateCheck` moves Off → Waiting → Running → Done on a background thread, then repaints (`app.rs:569-606`).
- It only runs with `[ui] check_updates = true`.
- When a newer release is out, the status bar shows the link `"↑ v{ver} available"` in the highlight colour.
- Tests: `a_cached_newer_release_reaches_the_window`, `a_failed_check_shows_nothing_and_is_remembered`, `by_default_no_check_starts`.

### 9. What must change when egui goes

**Cargo.toml**
- Drop `eframe` (`:29`), `egui-phosphor` (`:30`) and `egui_kittest` (`:69`).
- The `gui` feature (`:98`) becomes `dep:slint` (plus a build-dependency on `slint-build`), plus `sys-locale`. Each new dependency needs a one-line reason.
- The HTML deps (`:18-27`, `:64`) say "egui texture"; reword them.
- The `fontique` machete ignore (`:86`) stays.

**HTML spec** (`2026-10-07-postvak-html-design.md`)
- Says "egui body panel", "egui texture" and `ctx.request_repaint`.
- §5 says tests use **egui's bundled Ubuntu Light**. `html/render.rs:292-296` uses `egui::FontDefinitions::default()`, so the HTML snapshots need a bundled font instead.
- §9 lists the kittest tests.

**GUI spec** (`2026-10-06-postvak-gui-design.md`)
- §2 rows: GUI tests and Theme.
- §3 says eframe 0.36.
- §5 Appearance mentions `ctx.set_theme`; Visual identity says "egui cannot pick a weight".
- §10.
- §11 says "egui has no right-to-left shaping".
- Fix the stale texts in §8: the banner wording, SyncNow, and Activity's "sender".

**AGENTS.md**
- The `gui` module map entry (egui, `egui_kittest`, `test_support::Fixture`).
- The definition-of-done command `TZ=UTC UPDATE_SNAPSHOTS=1 cargo test gui::snapshots` with lavapipe.

**tests/architecture.rs** (`:407-460`)
- The view rule exempts `app.rs`, `mod.rs` and `test_support.rs`, and flags `&mut App`, `.client`, `daemon` and `rules::edit` in other `src/gui/*.rs`.
- `.slint` files are not scanned. Decide whether callbacks into Rust keep the "views return UiActions" rule, and update the test strings, which contain `egui::Ui` (`:589`).
- The `RULES_WRITE_EXCEPTIONS` entry for `src/gui/app.rs` `open_rules_file` must follow the code if it moves.

**CI** (`.github/workflows/ci.yml:58-73`)
- On Linux it installs `mesa-vulkan-drivers` and sets `POSTVAK_SNAPSHOTS=1` and `TZ=UTC`, then uploads `*.new.png` and `*.diff.png` on failure. That is kittest naming.
- Slint's software renderer would need no Vulkan driver.
- `cargo check --no-default-features` must still build.
- **All 11 PNGs must be regenerated.**

**Docs**
- `docs/src/gui.md`:
  - :3, :56 and :66 embed the snapshot PNGs.
  - :9 documents `window.ron`.
  - :7 documents the reconnect line "background sync stopped — reconnecting", retried every 5 s.
- `docs/src/install.md:61` promises the Wayland app id.
- `docs/src/index.md:48` has the licence note. Slint is GPLv3, royalty-free or commercial, so this note and README need a licence decision.
- Old plan and spec docs mention egui; they are history and should be left alone.

**Packaging** (no egui-specific code, but check these)
- `packaging/linux/AppRun`: no arguments and no terminal means `gui`.
- The desktop entry's `StartupWMClass` and `Icon` must equal the Slint window's app id.
- `appimage.sh` bundles only the binary. eframe loads X11 and Wayland at runtime; Slint's winit backend must also avoid link-time libraries (fontconfig dlopen, no hard libxkbcommon), or the AppImage needs them.
- `packaging/macos/*` builds default features; `Info.plist` sets `NSHighResolutionCapable`; the entitlements are empty. Re-test the hardened runtime with Slint's renderer.
- `paths::launched_as_app` is unchanged.

