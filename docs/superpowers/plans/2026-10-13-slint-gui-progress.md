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

## Next steps (in order)

1. Tasks 5–13 of the plan, one commit each, updating this doc after each.

## Notes for whoever resumes

- On the Linux sandbox `mise` is untrusted; use `env PATH=/home/pataar/.cargo/bin:/usr/bin:/bin cargo …` and commit with `git -c user.name="Pieter Willekens" -c user.email="3403851+pataar@users.noreply.github.com"`. On the Mac, plain `cargo` and git work.
- `cargo audit` warns (allowed) that `bincode` and `ttf-parser`, pulled in by Slint, are unmaintained.
- Slint facts learned in the prototype (`proto/slint` on branch `proto/slint`): `content-y`/`content-height` (not `viewport-*`); testing backend needs `TestingBackendOptions { renderer_name: Some("software") }` for real metrics and `take_snapshot`; `mock_elapsed_time` fires timers and `changed` handlers; generated code needs `clippy::unwrap_used` allowed; `SystemTrayIcon`, `MenuBar`, `ContextMenuArea`, `StyledText` (links, not selectable) exist in 1.18; `BackendSelector` for the renderer fallback.
