# Slint GUI rewrite — progress and handoff

Last updated: 2026-10-13. Branch `feat/slint-gui`. Read first: the plan `2026-10-13-slint-gui.md`, the design `../specs/2026-10-13-postvak-slint-gui-design.md`, the parity checklist `../specs/2026-10-13-postvak-slint-gui-parity.md`.

## Decisions so far (owner, 2026-10-13)

- GPUI dropped; the GUI is rewritten in Slint 1.18.1 under its royalty-free licence (About dialog shows `AboutSlint`). egui is deleted, not kept beside it.
- Extras in the rewrite: native menu bar + context menus, tray icon + close-to-background, software-renderer fallback, About dialog. Snapshot tests from Slint's software renderer (no lavapipe).
- Related PRs: #132 docs (toolkit report + decision; open), #131 Slint prototype (draft, never merge), #130 GPUI prototype (closed, branch deleted).

## Done

- Draft PR: postvak-app/postvak#134.

- Task 1: seams cherry-picked (`html::Pixels`, `html::Wake`, `Request::Select([f32;2])`, `gui::fixture`, `Client::in_memory` under `testing`).
- Docs commit: design, parity checklist, plan.
- Task 2 (`refactor(gui): state without egui`):
  - `src/gui/state.rs`: `app.rs` without egui. `tick(now)`, `apply(action, now)`, `request_tick`/`take_next_tick`, `wake: Wake`, `search_focused`, `theme: config::Theme`, `move_targets()` (the `m` picker's folders), `launch()` (opens a file or URL with the desktop's app). `UiAction::HtmlHover(Option<[f32;2]>)`, `HtmlSelect{from,to}`, `SetTheme(Theme)`, `SearchFocused(bool)` (the field took or lost the keyboard). All 37 old `app.rs` tests are ported to `test_support::Driver` (in `state.rs`); label assertions became assertions on state, `list::counts` or `Driver::status()`.
  - egui drawing and UI tests removed from `list.rs` (`counts` added), `folders.rs`, `body.rs`, `rules.rs`, `startup.rs` (pure tests kept), `html/mod.rs`, `html/render.rs` (the newsletter test samples the header right of the title, clear of the font's glyphs).
  - `status.rs`, `theme.rs`, `icons.rs` rewritten; `keys.rs` new; `test_support.rs` has `Fixture::app()` and `Driver` (press/apply/tick/at/ids/uids/status; typing and Enter go to the search field or move picker while they have the keyboard).
  - `ui/theme.slint` (palettes generated from `theme.rs`, a test keeps them equal; grid; fonts incl. Phosphor), `ui/window.slint` (`MailWindow`, an empty shell), `ui/startup.slint` (`StartupWindow`, the startup error window, wired in `mod.rs::explain`), `ui/app.slint` exports them.
  - `mod.rs`: `run()` sets the xdg app id, `open()` connects with a wake through `slint::invoke_from_event_loop`, keeps `Live { app, clock, timer, window }` in a thread local; `Live::present` is empty until Task 3. The macOS integrated-GPU test is gone (wgpu only). `#![allow(dead_code)]` at the top of `gui/mod.rs` until the views use the text functions; drop it in Task 13.
  - `Cargo.toml`: `unsafe_code = "deny"` (was forbid) so `gui::ui` can allow it for Slint's generated code.
  - `tests/architecture.rs`: the gui rule and the rules.toml exception name `state.rs` (Task 13 adds `bind.rs`/`rows.rs` checks).
  - Deferred to their view tasks: the harness-based view tests of `list.rs`, `folders.rs`, `body.rs`, `rules.rs`, `status.rs`, `toolbar.rs`, `html/view_tests.rs`, `startup.rs` window tests and `snapshots.rs` (parity checklist lists them; old code on `origin/main`).

- Merged `main` after #133 (newsletter HTML, Load remote content): `html/fetch.rs` wakes through `Wake`; `state.rs` has `fetcher`, `UiAction::LoadRemote`, `lay_out`, `load_remote`, `receive_remote`; `html` is a private module now. Its three view tests are listed in the parity checklist for Task 7. The `tests/snapshots/gui_html_*.png` from `main` are egui's and get replaced in Task 12.

- Task 3 (`feat(gui): Slint window shell, theme and startup window`):
  - `ui/theme.slint`: `Theme.preference` (`ThemePreference` system/light/dark; System follows std-widgets' `Palette.color-scheme`), `Theme.dark`, `Theme.pane`/`Theme.frame` palettes, grid, font family names. Icon codepoints stay in `icons.rs`; `bind.rs` passes the glyphs as text.
  - `ui/window.slint`: `MailWindow` with toolbar, folder, list and body slots (empty), `Splitter`s for `folders-width`/`list-width` (min `list::MIN_WIDTH`), `PaneFrame` with the 1 px accent outline for `focused-pane` (`Pane` enum), one pane for Rules/Activity/Backups, the config banner, a status bar slot.
  - `bind.rs`: `present(&App, &MailWindow)`.
  - `mod.rs`: `Live { app, clock, timer, window }` in the `LIVE` thread local; `with_live`, `dispatch(action)` (queued in `QUEUE` while `LIVE` is busy, drained after), `Live::install`. `startup_window(problem, open_docs)` takes the opener so tests see it.
  - Renderer fallback: nothing to write. Slint's winit backend already tries the next renderer (software included) when one cannot create the window (`allow_fallback`, `i-slint-backend-winit` `try_create_window_with_fallback_renderer`); `SLINT_BACKEND` still overrides.
  - `test_support`: `headless()` (testing backend, mock time, software renderer), `Fixture::window() -> (Open, Wires)`, `Open::{with, window, advance, find}`.
  - Tests: the four startup window tests, theme and view, config banner.
  - Not run by hand yet: the window has only been run under the testing backend.

- Task 4 (`feat(gui): folders, toolbar and status bar`):
  - Toolkit-free: `folders::{entries, pinned, Entry, FolderEntry}` (with `menu()` and `caret_name()`), `toolbar.rs` (`buttons`, `enabled`, `SEARCH_HINT`), `status::{lines, history, Line}`.
  - `ui/common.slint` (`IconButton` with a Slint `Tooltip` for the hint, `TextButton`), `ui/folders.slint` (`FolderPane`, `FolderItem`, `MenuChoice`; `ContextMenuArea` per row with head items, a `MenuSeparator`, tail items), `ui/toolbar.slint`, `ui/status.slint` (`StatusBar`, `StatusLine`, `Dot`), `ui/dialogs.slint` (`Dialog`, `KeysDialog`, `HistoryDialog`; composed, not inherited, because elements of an inherited base are not found by accessible label).
  - Keys: one root `FocusScope` in `MailWindow` calls `key(text, control, alt)`; `bind::press` turns Slint key text into `keys::Press`. Slint's `control` modifier is Cmd on macOS.
  - `bind::connect` wires every callback through `mod.rs::on(Ask)`: the closure reads `&App` and names the `UiAction`; indexes are resolved against `folders::entries`/`toolbar::buttons` at click time.
  - `UiAction::OpenUrl(String)` and `App::launch` (a `fn`, so tests record into `test_support::launched()` instead of spawning `open`/`xdg-open`; rules.toml opens through it too).
  - Labelled containers mark their inner `Text`s `accessible-role: none`, so a label matches one element.
  - `Open::{tick, get, click, span, press, png}` in `test_support`.
  - Context menus cannot show shortcuts in Slint 1.18 (only `MenuBar` can), so "Sync now · Cmd+R" is the item's title.
  - Deferred to Task 12 (geometry and snapshots): `rows_are_full_width_and_keep_the_count_in_their_name`, `a_long_name_truncates_before_its_count`, `the_selected_folder_has_a_brass_icon_and_count_badge`, `account_names_are_muted_and_semibold`, `a_row_without_a_count_keeps_its_label_on_the_left`, `an_indented_row_starts_later_and_ends_at_the_same_edge`, `a_separator_divides_special_from_custom_folders`, `action_icons_are_drawn_at_the_icon_size`, `the_wordmark_lines_up_with_the_action_buttons`. A software-rendered look at the window (`Open::png`) matched the egui snapshot's layout closely; the list title row, the status bar's top gap and the folder pane's top padding still differ by a few pixels.

- Task 5 (`feat(gui): message list, search and move picker`):
  - `rows.rs`: `ListModel`, a Slint `Model` holding the app's rows; `row_data` makes a row's texts (`list::columns`, `row_text`, the menu titles) only when the `ListView` asks. A new list resets it; a moved cursor or mark notifies only the rows it changed. `bind::present_list` finds it again with `as_any().downcast_ref`.
  - Toolkit-free: `list::{title, search_hint, results, menu}`.
  - `ui/list.slint` (`ListPane`, `ListRow`, `RowData`), `MoveDialog` in `ui/dialogs.slint`. The search field and the move filter focus themselves on `init`; the window's `FocusScope` now wraps the whole window, so Esc and Ctrl/Cmd+R bubble up from text fields, and it takes the keyboard back when search or the picker closes and after Enter in search.
  - Scrolling: the `ListView`'s `content-y` reports `ListViewport` through `list-scrolled`; while `follow_cursor` is set, `present_list` sets it from `list::offset_showing`.
  - The row menu (three groups from `list::menu`) selects its row on activation unless the row is marked, then acts; egui selected on the right click itself. No shortcut text in the menu (Slint 1.18 shows shortcuts only in a `MenuBar`).
  - Test helpers de-duplicate elements: `ElementQuery` visits each `ListView` row several times (11 in a 1280×800 window).
  - Deferred to Task 12: `in_a_narrow_row_nothing_is_drawn_over_the_date_or_past_the_edge`, `unread_senders_are_semibold_dates_monospace_and_rows_ruled`.

- Task 6 (`feat(gui): body pane`):
  - Toolkit-free `body::pane(&App) -> Pane`: empty states, header strings, downloaded, missing text, the switch, attachments, saved note, and the text's openable links (once each).
  - `ui/body.slint` (`BodyPane`, `BodyInfo`): the toolbar (Text | HTML, Save .eml, View source; disabled buttons show the missing text on hover), the subject, avatar and header lines (the short date has the full date as a `Tooltip`), attachments with Save (accessible label "Save {attachment}"), the saved note, then a read-only `TextInput` (15 px, selectable, accessible label "Message text") and the links under it. HTML shows "Rendering…" until Task 7.
  - `SourceDialog` in `ui/dialogs.slint`: rows in a `ListView` (wider content than the view scrolls sideways), and Copy, which selects a hidden read-only `TextInput` holding the cleaned text and copies it, as Slint has no clipboard API in Rust.
  - `TextButton` and `IconButton` take a palette and a `disabled-hint`.
  - The tick timer calls `Live::tick` directly: its callback already runs on the window's thread, and `invoke_from_event_loop` never runs under the headless backend.
  - `test_support::{raw, add_html}` (from the old `view_tests.rs`).
  - The read-timer and fetch tests run on `Driver` (a clock the test sets); `the_read_timer_wakes_the_window_without_input` checks the real window's timer with mock time.
  - The Copy test checks the text Copy copies (`source-text`); reading the clipboard back is not possible under the testing backend.

- Task 7 (`feat(gui): HTML view`):
  - `ui/body.slint`: `HtmlView` (remote line with Load remote content, "Rendering…", a non-interactive `Flickable` of strip `Image`s on a white page, a `TouchArea` for hover (`changed mouse-x/y`), drag selection and clicks, a `Tooltip` with the link, pointer or text cursor, and a link menu: Open link, Copy link). `PageInfo`, `Remote`, `StripData`.
  - `rows::PageModel`: strips as images (`Image::from_rgba8_premultiplied`), each converted once and kept while its `Arc<Pixels>` stays.
  - Copy: `MailWindow::copy-text(text)` puts text on the clipboard through a hidden read-only `TextInput`; Ctrl/Cmd+C copies the page selection (`page-selection`).
  - State: `HtmlState::visible` (the last reported range) is asked for again after a new layout or selection; `settle_html` lays out a width that held still on a tick; `App::html_size` with `lay_out_new_html` lays out a newly shown HTML message at the last width, as Slint reports the width only when it changes (egui sent it every frame).
  - `mod.rs`: `on(Ask)` queues callbacks that arrive while the app is borrowed (an element's `init` during `present`) in `ASKS`; `Live::tick` answers them after presenting. `dispatch`/`QUEUE` are gone.
  - `Open::tick` visits the elements and runs `mock_elapsed_time(0)`, so conditional elements are built and `changed` handlers run as in the event loop.
  - `html::view_tests` holds the ported view tests (fake renderer and fetcher), plus `a_new_layout_asks_for_the_strips_on_screen`. The drag test checks the selection the window would copy; the text cursor is not readable through the testing API.

- Fix after Task 7: Linux CI failed to build `fontique` 0.11 (Slint's), which links fontconfig, next to Blitz's 0.12, which loads it at runtime through the same `yeslogic-fontconfig-sys`. `i-slint-common` with `fontconfig-dlopen` is now a Linux dependency of the `gui` feature (in machete's ignore list).
- Task 8 (`feat(gui): rules, activity and backups views`):
  - Toolkit-free in `rules.rs`: `banner`, `proposals`, `rule_lines`, `activity_cells`, `trash_cells`, the headings and empty texts.
  - `ui/rules.slint`: `RulesPane` with `Sheet` (rules, activity, backups); the rules view (banner, Open rules.toml, Proposals with Approve/Reject, Rules with a std `CheckBox` per rule), and a striped `Table` with fixed column widths for Activity and Backups (Restore per row).
  - The window's callbacks are `approve-rule`/`reject-rule`: a callback named `reject` shadows the `reject` keyword in a `key-pressed` handler.
  - Polling tests move the window's clock with `Open::advance(5.0)`, which fires the poll timer.
  - Cosmetic gap for Task 12: the std `CheckBox` draws in std-widgets' accent blue, not the theme.

- Task 9 (`feat(gui): menu bar and About`):
  - `menu.rs`: `Item` and `item(name)`; `Item::action(&App)` is what the item does, or None, which disables it (the message items follow `toolbar::enabled`). Help ▸ Postvak documentation opens `CARGO_PKG_HOMEPAGE`.
  - `MenuBar` in `ui/window.slint` (File, Edit, Message, View with a Theme submenu of checkable items, Mailbox, Help), items calling `menu-item(name)`; `MenuState` carries what is enabled. Edit ▸ Copy copies the HTML page's selection.
  - Shortcuts only on Close window (Ctrl+W) and Quit (Ctrl+Q): a menu-bound letter such as `e` would fire while typing in search, and Ctrl+R is the keymap's. The keymap's keys are in the `?` window.
  - Quit ends the event loop; Close window hides the window (Task 10 decides between hiding and quitting).
  - `ui/about.slint`: `AboutDialog` (name and version, description and licence from `Cargo.toml`, Project and Documentation links, std-widgets' `AboutSlint`); `UiAction::About` toggles it, Esc closes it after Keys.
  - On Linux and Windows the menu bar draws inside the window, above the toolbar; snapshots will include it.

- Task 10 (`feat(gui): tray icon and close to background`):
  - `ui/tray.slint`: `Tray inherits SystemTrayIcon` with the app icon, `hint` as tooltip, a click or Open Postvak shows the window, Sync now, Quit (`quit_event_loop`).
  - `status::tray_hint`: "Postvak", or "Postvak · {n} unread" over every INBOX.
  - Close-to-background needs no code of ours: closing hides the window, and `run_event_loop` keeps running only while something keeps it alive. A tray icon keeps it alive only once its platform icon exists (`i-slint-core` `items/system_tray.rs`: `update_keepalive` after the handle is created; on Linux ksni fails without a StatusNotifierWatcher), so without a tray host closing quits as before. Slint has no public "registered" flag, so the test covers the tooltip only.
  - `Live.tray` is None in tests (creating one would put an icon in the test machine's menu bar).
  - Not checked yet: a hidden window on macOS is not shown again by a Dock click (only the tray's Open Postvak); look at it during the hand checks.

- Fix: `cargo check --no-default-features --all-targets` failed on Linux CI, because the testing backend (a dev-dependency) builds Slint's fontique without the `gui` feature, and it then linked fontconfig. `i-slint-common` with `fontconfig-dlopen` is a Linux dev-dependency too.
- Task 11 (`feat(gui): remember window and pane sizes`):
  - `persist.rs`: `Layout { width, height, position, folders_width, list_width }` as `state_dir/window.json`; `load` ignores a missing or unreadable file (logging why), `save` writes atomically.
  - `mod.rs`: `restore` before the window shows (sizes clamped to at least 400×300, the list to `list::MIN_WIDTH`), `remember` after the event loop ends, so also after Quit with the window hidden. The position is kept only when the platform reports one (Wayland reports none). The startup error window keeps nothing.
  - `docs/src/gui.md` still names `window.ron`; Task 13 updates it.

- Task 12 (`test(gui): snapshots from the software renderer`):
  - `snapshots.rs`: the 11 PNGs (inbox light/dark, all inboxes, marks and an expanded thread, keys, rules, activity, backups, HTML light/dark/narrow) from `Open::png`, compared by decoded pixels. They run in every `cargo test`; with a local offset other than zero the pixels are not compared (a line on stderr says so). `TZ=UTC UPDATE_SNAPSHOTS=1 cargo test gui::snapshots` rewrites them; a mismatch writes `{name}.new.png` beside the reference. Generated on macOS; whether Linux draws the same pixels is what the first CI run says.
  - Geometry tests through element positions: the toolbar glyph, the list title and the rows' content share an edge; the toolbar's buttons a line; the status glyph lines up with the pinned rows; folder row height and pitch; the focus outline moves nothing; the list title, the reader's actions and All inboxes share a line; the Keys window's content sits `INSET` in; with one account the first folder and the first message share a line.
  - Layout fixes the tests found: the toolbar's wordmark cell is `folders-width + PAD/2 - GLYPH` wide; the folder pane's first line (All inboxes, or an account's name) is a `CONTROL`-tall heading row on the list title's line, accounts after the first have `PAD` above them (`FolderItem.heading`, `gap`); the list scrolls to the cursor only once it has reported its height (the thread snapshot showed it scrolled two rows).
  - CI: `TZ=UTC` on both OSes, no mesa, no `POSTVAK_SNAPSHOTS`; `*.new.png` uploaded per OS on failure.
  - Not ported as separate tests, as the snapshots show them: the row-drawing and folder-row geometry tests listed under Tasks 4 and 5, the avatar centring and the search field's height (the std `LineEdit` is `CONTROL` tall, not `ROW`).
- Follow-up (owner, 2026-10-13): the alignment looks off, not on any raster. A pass over spacing and alignment against the grid comes after Task 13.

- Task 13 (`docs: the Slint GUI`):
  - `#![allow(dead_code)]` is gone from `gui/mod.rs`; leftovers removed (`UiAction::CloseSource`, `icons::NEWER`, `folders::INDENT`, the Rust copies of the icon bytes, which only the icon tests read now). `theme.rs` is the tests' reference (`cfg_attr(not(test), allow(dead_code))`, with a reason); a new test keeps `theme.slint`'s grid equal to it. The window icon picks the Dock variant on macOS through `Platform.os`.
  - `tests/architecture.rs` already holds the rule for `bind.rs` and `rows.rs` (every gui file except `mod.rs`, `state.rs` and `test_support.rs`); the rules.toml exception names `state.rs`.
  - Docs: `docs/src/gui.md` (tray and close-to-background, `window.json`, menu bar, context menus, About), the Slint attribution in `docs/src/index.md` and `README.md`, AGENTS.md's `gui` notes and snapshot step, a note on the egui GUI and HTML specs.
  - Not done here, needs the owner or CI: the AppImage binary's `NEEDED` list and the macOS DMG (CI builds them only on a tag); by hand: the window on KDE Wayland and macOS, a screen reader on the list, an input method in search, and the macOS Dock click on a hidden window. A local smoke run reached only the startup window, because the scratchpad home's socket path is too long for a Unix socket.

- `/simplify` over the branch (`refactor(gui): simplify after review`): four reviews (reuse, simplification, efficiency, altitude). Applied:
  - `bind::present` runs after every callback (pointer moves over HTML included), so the small models are updated in place (`rows::synced`/`sync`: only changed rows, Slint keeps the other elements), constants are set once in `bind::init`, `BodyInfo` is set only when it changed, and a message's links are found once in `BodyState::links`.
  - `tick` drives the HTML view: `html_width` at `html_size` every tick (no `lay_out_new_html`/`settle_html`), and `paint_visible` asks for missing strips every tick (no `repaint` flag; `html_visible` only records the range).
  - One search-focus flag (`search_focused`; `StartSearch` sets it, the field reports it), `App::typing()`, `App::apply(action)` without the unused clock, `Live::apply` as the one path for callbacks.
  - Menu items are the Slint enum `MenuAction`, matched exhaustively in `bind::menu_item`; `FolderEntry::menu()` returns two groups; `Entry::Account.gap` comes from `folders.rs`.
  - The palettes live only in `ui/theme.slint`; the contrast tests read them from the `Theme` global, `theme.rs` keeps `CONTROL` and `ROW` (later `CONTROL` and `LIST_ROW`), the snapshot geometry tests read the grid from the window.
  - Removed: `Columns`' unused fields, `sender_width` and its constants, `ListPane.query`, `RowData.flagged`. Shared test helpers: `test_support::{unread, command}`, `Fixture::break_store`.
- Not applied, as follow-ups if they show up in use:
  - `present` still rebuilds `folders::entries`, compares the list rows in full and builds the sheet's cell models on every call; dirty flags per area (folders, list, body, sheet) on `App` would skip that.
  - `follow_cursor` scrolls only after `MoveCursor`; making state own the list offset (set it whenever the cursor moved) would cover search, collapse and clicks too.
  - Search queries SQLite on the UI thread per keystroke, as egui did; debounce or a worker if large stores stutter.
  - Slint reuse: one `Heading` and a link button in `common.slint` for the four serif headings and two hand-made links; `(group, index)` for the row menu instead of a flat index.
- egui sweep (`docs: mark the egui steps in the plans not yet executed`): no egui code, crate or CI step remains; the rules editor, regex tester, tasks and surface parity plans carry a note that their GUI steps were written for egui; the superseded GPUI migration plan is removed (its results are in the toolkit report).
- Alignment against main's snapshots (`fix(gui): spacing and alignment as on main`): each PNG laid over main's egui one. Taken from main:
  - The panes meet: the splitters lie over the pane edges instead of taking a `PAD` column, so the list starts at the folder pane's edge and the list and the body are one surface; the toolbar's wordmark cell is `folders-width - PAD/2 - GLYPH` wide.
  - Text is 13 px (`Theme.text-size`, egui drew 12.5), list dates 12 px, folder icons at text size.
  - List rows are `Theme.list-row` (52: two `ROW` lines and `PAD/2`), laid out rather than placed by hand; the unread dot and the mark are fixed columns, and only a thread member's text indents. `theme.rs` has `LIST_ROW`, and the grid test compares it and `CONTROL` with the `Theme` global at run time.
  - Separators have `PAD/2` above and below; a separator above Rules, Activity and Backups.
  - Activity, Backups and the rules list are content-sized grids (`GridLayout` with repeated `Row`s) with stripes as wide as the table; fixed column widths had elided "move Receipts". The proposal card is as wide as its TOML.
  - Pane actions (Open rules.toml, Approve, Reject, Save, Load remote content, Restore) are filled `TextButton { framed: true }`.
  - The reader's sender and address have `PAD` between them, the status dot `PAD` before its text; "Remote content not loaded." elides, so the narrow reader no longer runs past the pane's edge.
  - Kept from the branch: the Keys window, links listed under a plain text body (a `TextInput` keeps the text selectable; `StyledText` would colour links inline but cannot select).
  - Found, not fixed: off macOS the in-window menu bar draws Fluent's light-scheme text (near black) on the oxblood frame, and std widgets (`LineEdit`, `CheckBox`, scroll bars) follow the OS scheme, not `Theme.preference`. `Palette.color-scheme` can be set, but setting it drops the binding `Theme.dark` reads the OS scheme from; needs a choice (a paper strip behind the menu bar, or the OS scheme from Rust).
- `/simplify` follow-ups (owner asked, 2026-10-14):
  - Done: one `Heading` and one `Link` in `common.slint` (`Open::find` now skips elements hidden from screen readers, as a component inheriting `Text` keeps its label); menu items by `(group, place)`; the list follows the cursor whenever it moves out of view, by key, click, search or collapse, and asks again while `ListView` still stops short of it (`ListState::cursor_shown`).
  - Measured and skipped (release build, 2026-10-14): `present` with 5000 threads costs about 0.2 ms including a tick, so dirty flags per area would save nothing noticeable; search per keystroke on 20000 messages costs 0.06 to 1 ms for narrow queries and about 22 ms for a word in every message (500 hits, the limit), so a worker thread waits until a store large enough to stutter shows up.
- Colour scheme (`fix(gui): std widgets and the menu bar follow the theme`): the mail window sets std-widgets' `Palette.color-scheme` from `Theme.dark`, so fields, check boxes, scroll bars, the tables and the menu bar draw in the theme's scheme; `Theme.desktop-dark` comes from a never-shown `SchemeProbe` window, whose scheme still follows the desktop, for the System theme. Behind the menu bar off macOS: the frame when dark, the pane's paper when light, since std-widgets draw the light scheme's menu text dark.
- Slint features audit (what we build by hand that Slint 1.18 has):
  - Applied: Ctrl+C copies the HTML selection through a `KeyBinding` (enabled while there is a selection) instead of matching the key in `key-pressed`; a test pastes it into search. `GridLayout` with repeated `Row`s for the tables (above).
  - Already on Slint: `MenuBar`, `ContextMenuArea`, `Tooltip` with `@markdown`, `SystemTrayIcon`, `AboutSlint`, `ListView`, `LineEdit`, `CheckBox`, `Spinner`, `ProgressIndicator`, `@children` in `Dialog`, `Platform.os`, the renderer fallback.
  - Hand-made with reason: the splitter (no Slint element), `IconButton`/`TextButton` (std `Button` draws Fluent, not the Ossenbloed palette), the theme switch (no segmented control), `rows::ListModel` (rows made on demand plus a per-row cursor diff that `MapModel` cannot express), `rows::synced` (`VecModel` has no diffing set), the hidden `TextInput` for the clipboard (Slint has no public clipboard API), state-driven overlays instead of `PopupWindow` (shown imperatively, outside `App`), the Rust `slint::Timer` driven by `tick`'s deadline.
  - Options for the owner, each with a cost:
    - Done (owner chose it, 2026-10-14; `feat(gui): sortable Activity and Backups tables`): `StandardTableView` for Activity and Backups. A heading click sorts (`UiAction::SortSheet`, `App.sheet_sort`, reset when the view changes; `rules::sheet_order` maps shown rows to the app's); Restore acts on the chosen row; the table takes the keyboard so Up and Down choose one. Cost: Fluent-styled, and its rows and headings have no accessible actions, so a screen reader can read the cells but not choose a row or sort by action.
    - `StyledText` for the plain-text body: links coloured inline and clickable (as egui drew them), but no text selection; today's `TextInput` plus a link list keeps selection.
    - `TextEdit { read-only: true }` for the plain-text body: its own scrolling and selection, replacing `ScrollView` + `TextInput` + `width: scroll.visible-width`; but a Fluent frame around the text.
    - `KeyBinding` for Ctrl+R and the other shortcuts: declarative, but `keys.rs` is shared with the `Driver` tests and the Keys help, so moving them splits one table in two.
    - `animate background` on hover and selection for polish; main had none.
    - `Palette.color-scheme` following `Theme.preference` (see the menu bar finding above).

## Next steps (in order)

1. Hand checks: the window on KDE Wayland and macOS (a smoke run needs a short home path, as the socket path is limited to about 100 bytes), a screen reader on the list, an input method in search, the macOS Dock click on a hidden window, X11/Wayland PRIMARY selection after Copy.
2. Review and merge. CI on Linux drew the snapshots as macOS did (2026-10-14), so one set of PNGs serves both.

## Notes for whoever resumes

- On the Linux sandbox `mise` is untrusted; use `env PATH=/home/pataar/.cargo/bin:/usr/bin:/bin cargo …` and commit with `git -c user.name="Pieter Willekens" -c user.email="3403851+pataar@users.noreply.github.com"`. On the Mac, plain `cargo` and git work.
- `cargo audit` warns (allowed) that `bincode` and `ttf-parser`, pulled in by Slint, are unmaintained.
- Slint facts learned in the prototype (`proto/slint` on branch `proto/slint`): `content-y`/`content-height` (not `viewport-*`); testing backend needs `TestingBackendOptions { renderer_name: Some("software") }` for real metrics and `take_snapshot`; `mock_elapsed_time` fires timers and `changed` handlers; generated code needs `clippy::unwrap_used` allowed; `SystemTrayIcon`, `MenuBar`, `ContextMenuArea`, `StyledText` (links, not selectable) exist in 1.18; `BackendSelector` for the renderer fallback.
