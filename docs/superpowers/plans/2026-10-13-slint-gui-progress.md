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

## Next steps (in order)

1. Tasks 11–13 of the plan, one commit each, updating this doc after each.

## Notes for whoever resumes

- On the Linux sandbox `mise` is untrusted; use `env PATH=/home/pataar/.cargo/bin:/usr/bin:/bin cargo …` and commit with `git -c user.name="Pieter Willekens" -c user.email="3403851+pataar@users.noreply.github.com"`. On the Mac, plain `cargo` and git work.
- `cargo audit` warns (allowed) that `bincode` and `ttf-parser`, pulled in by Slint, are unmaintained.
- Slint facts learned in the prototype (`proto/slint` on branch `proto/slint`): `content-y`/`content-height` (not `viewport-*`); testing backend needs `TestingBackendOptions { renderer_name: Some("software") }` for real metrics and `take_snapshot`; `mock_elapsed_time` fires timers and `changed` handlers; generated code needs `clippy::unwrap_used` allowed; `SystemTrayIcon`, `MenuBar`, `ContextMenuArea`, `StyledText` (links, not selectable) exist in 1.18; `BackendSelector` for the renderer fallback.
