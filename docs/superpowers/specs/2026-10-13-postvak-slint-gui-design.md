# Slint GUI — design

Date: 2026-10-13. Status: approved direction (toolkit report §7); this document fixes the design. Replaces the egui GUI of `2026-10-06-postvak-gui-design.md` and the egui parts of `2026-10-07-postvak-html-design.md`; their behaviour stays the target. The parity checklist (`2026-10-13-postvak-slint-gui-parity.md`) lists every behaviour, string and test the rewrite must keep.

## 1. Purpose and scope

Rewrite `src/gui/` in Slint 1.18 and delete egui. The window looks and behaves as today (same panes, keys, texts, palettes, fonts, grid), and gains what Slint offers: a native menu bar, context menus, a tray icon with close-to-background, a software-renderer fallback, an About dialog with the Slint attribution, and snapshot tests drawn by Slint's software renderer.

Out of scope: compose, account setup in the GUI, live reload of `config.toml`, translations, any change to the daemon, CLI or MCP. The CLI and MCP surfaces do not change, so the surface-parity rule is not involved.

## 2. Decisions log

| Decision | Choice | Rejected |
|---|---|---|
| Toolkit | Slint 1.18, royalty-free licence: an About dialog shows `AboutSlint` | GPUI (toolkit report); keep egui |
| Rollout | egui is deleted on branch `feat/slint-gui`; the branch merges once the parity checklist is done | Slint beside egui behind a feature |
| State | `gui/state.rs`: today's `App` without egui, the only code that changes state; views send `UiAction`s | State in Slint properties; logic in `.slint` |
| Views | `.slint` files draw and call back; `gui/bind.rs` turns state into Slint models and properties and callbacks into `UiAction`s | Rust building widgets in code |
| List rows | A lazy Slint `Model` over `state.list.rows`: rows are built only when shown | Rebuilding a `VecModel` per change |
| Keys | One keymap table in `gui/keys.rs` (binding, focus, action, help text); the `?` window and the menu shortcuts read it | Keys matched by hand in a function, help kept beside it |
| Icons | Phosphor as a bundled font (`assets/fonts/Phosphor.ttf`, MIT), same glyph mapping as `icons.rs` | SVG files; Slint's std icons |
| Fonts | The four bundled fonts imported in `.slint`; every glyph the GUI draws is in a bundled font, so snapshots never fall back to system fonts | System fonts for missing glyphs |
| Theme | A `Theme` global with the four palettes; Rust sets `dark` from the preference or the system scheme | Slint's std widget palette |
| Time | `state` takes `now: f64` seconds from a clock the tests replace, as egui's input time did; Slint `Timer`s wake it | Reading the clock inside state |
| Daemon events | `subscribe_waking` with a wake that calls `slint::invoke_from_event_loop`; the app drains the channel there | Polling per frame |
| HTML | The render thread and strip protocol stay; strips become `slint::Image` from premultiplied RGBA, one copy | Converting formats |
| Body text | Plain-text bodies show in a read-only `TextInput` (selectable, copyable) with the links listed under it as clickable `StyledText`; HTML keeps its own selection | `StyledText` alone (links, no selection) |
| Menu bar | Slint `MenuBar` (native on macOS, in the window elsewhere), every item a `UiAction` with its keymap shortcut | No menu bar |
| Context menus | `ContextMenuArea` on list rows, folder rows, the body and HTML links | Hand-built popups |
| Tray | `SystemTrayIcon`: unread count in the tooltip, menu Open / Sync now / Quit; closing the window hides it only while the tray icon is registered | Always quit on close; a config key |
| Renderer | Slint's default (femtovg on OpenGL); if the window cannot be created, again with the software renderer | Fail as eframe does without GPU |
| Snapshots | Slint's testing backend with its software renderer; byte-exact PNGs, run in every `cargo test` | lavapipe; GPU snapshots |
| Persistence | `state_dir/window.json`: window size, pane widths (position where the platform allows) | eframe's `window.ron` |

## 3. Modules

```
src/gui/
  mod.rs        run(): config, daemon, window, tray, renderer fallback, startup error window
  state.rs      App without egui: fields, handle, apply(UiAction), reload_view, act, body, html, timers, polling
  keys.rs       the keymap: Binding × Focus → UiAction, with help text; key text → action
  bind.rs       state → Slint (properties, models); Slint callbacks → UiAction → state.apply → present
  rows.rs       the lazy list Model, folder tree Model, other row models
  list.rs       rows, threads, merge, columns, row_text, offset_showing (toolkit-free, from today's list.rs)
  folders.rs    tree, visible, shown, label, sort, counts (toolkit-free part of today's folders.rs)
  body.rs       segments, has_target, size, initials, name_and_address, Source, dates (toolkit-free)
  status.rs     account_line, progress, failed, dot kind, sync_key, VERSION (toolkit-free)
  rules.rs      RulesState, activity_log, trash_rows (toolkit-free)
  startup.rs    Problem (unchanged)
  numbers.rs    unchanged
  icons.rs      Phosphor codepoints by meaning, for_special_use
  persist.rs    window.json
  html/         Load/Request/Reply/Pixels/Wake, render thread, net provider (toolkit-free)
  ui/           *.slint: theme, window, toolbar, folders, list, body, html, status, rules, dialogs, tray, startup
  test_support.rs  Fixture, Wires, and a window under Slint's testing backend
  snapshots.rs  PNGs from the software renderer, geometry tests
```

`bind.rs` and `rows.rs` read `&App` and return `UiAction`s; only `state.rs` changes state. `tests/architecture.rs` keeps that rule: outside `state.rs`, `mod.rs` and `test_support.rs`, no `&mut App`, no `.client`, no `crate::daemon`, no `rules::edit`. `.slint` files cannot reach Rust state at all.

Slint's generated code goes in `gui::ui` with `clippy::unwrap_used`, `expect_used` and `all` allowed: it unwraps internally.

## 4. Event loop

- `run` creates the window and the tray, then `slint::run_event_loop_until_quit()`, so a hidden window keeps the process alive.
- The daemon's wake: `slint::invoke_from_event_loop(|| APP.with(drain))`; `drain` empties the event channel into `state.handle`, then `present`.
- Timers (`slint::Timer`): `POLL` (2 s, files and the release check), `RECONNECT` (5 s while the daemon is lost), the read timer (single shot at the remaining delay), `SETTLE` (HTML width). Each fires `state.tick(now)` and `present`.
- `present` is the one place that pushes state to Slint; it compares before setting where a set is expensive (rows: `ModelNotify::reset` only when rows changed, cursor otherwise).

## 5. Window

The same layout as today (GUI spec §5–7, parity checklist §1.3): toolbar (`TOP_BAR`), optional config banner, folder pane (resizable, 220), list pane (resizable, 480, min 220) and body pane, status bar; Rules, Activity and Backups as one pane. The grid constants are Slint globals with today's values; nothing uses a literal size.

- **Focus.** One `FocusScope` per pane takes the keys; text fields take keys only while focused (search, move filter). Space and Enter never activate a focused row (parity: `tab_and_arrows_leave_no_egui_widget_focus…`). The focused pane gets a 1 px accent outline without moving content.
- **Accessible labels** carry today's widget names (`"Inbox (2)"`, `"Collapse Projects"`, row names from `row_text`, `"✔ "` for marked rows, button names), so tests find the same things.
- **Menu bar.** File: Save .eml, View source, Close window, Quit. Edit: Copy, Search. Message: Archive, Move…, Delete, Flag, Read or unread, Add to selection. View: Text or HTML, Theme ▸ System / Light / Dark, History, Keys. Mailbox: Sync now, Mark folder read. Help: Postvak documentation, About Postvak. Items are enabled when their action would do something, as the toolbar buttons are.
- **Context menus.** List row: today's `mail_menu` (three groups). Folder row: today's `folder_menu`. Body: Copy, Save .eml, View source. HTML link: Open link, Copy link.
- **Drag to a folder.** A list row dragged onto a folder of its own account moves there: the marked rows when the row is marked, else that row alone, as a row's context menu acts. GUI-only: it is a gesture for the move the CLI and MCP already have.
- **Dialogs** (Slint `PopupWindow`s): move picker, source, keys, history, About. Esc closes them in today's order.
- **About.** Name, version, licence (MIT OR Apache-2.0), project and docs links, and `AboutSlint`.

## 6. Tray and close-to-background

- `ui/tray.slint`: `Tray inherits SystemTrayIcon`, icon from `assets/icon.png`, tooltip `"Postvak"` or `"Postvak · {n} unread"` (all INBOXes, as All inboxes counts), menu: Open Postvak, Sync now, Quit. Clicking the icon shows the window.
- Closing the window hides it when the tray icon is registered with the desktop; otherwise it quits as today. Quit (menu, tray, `Ctrl+Q`/`Cmd+Q`) ends the event loop.
- The daemon keeps syncing either way; it is its own process.

## 7. Renderer fallback

`run` creates the window with Slint's default backend. If that fails, it selects the software renderer (`BackendSelector::new().renderer_name("software")`) and tries once more, logging which it used; `SLINT_BACKEND` still overrides both. Only if both fail does the startup error window path report it (to stderr, as today without a display).

## 8. HTML

The render thread, its batching, the net provider and the generation protocol are unchanged (HTML spec). `html::Renderer::start(wake)` wakes through the event loop; `Reply::Strip` carries `Pixels`; a strip becomes `Image::from_rgba8_premultiplied`. The page is a `Flickable` of positioned images; hover asks `Request::Hit` and shows the link in a tooltip and the pointer cursor; a click opens `openable` links; a drag sends `Request::Select` and Copy copies the selection. Tests lay out with the bundled Hanken Grotesk instead of egui's Ubuntu Light, so HTML snapshots are byte-exact too.

## 9. Tests

- **Pure tests** of today's modules stay as they are.
- **State tests**: today's `app.rs` tests that check state (rows, sent commands, history, errors, timers) drive `App` directly with `UiAction`s and a test clock.
- **Window tests**: `Fixture::window()` opens the real window under Slint's testing backend (mock time, software renderer); keys and clicks are dispatched as window events; elements are found by accessible label; geometry is read from `ElementHandle` positions and sizes for the grid tests.
- **Snapshots**: every test of today's `snapshots.rs`, drawn by the software renderer at scale 1, compared byte for byte with `tests/snapshots/gui_*.png`; `UPDATE_SNAPSHOTS=1` rewrites them. They run in every `cargo test` (no GPU, no `POSTVAK_SNAPSHOTS`), still with `TZ=UTC`.

## 10. Packaging, docs, licence

- The binary needs only libc (prototype, `readelf`); the AppImage keeps bundling just the binary. The window sets the app id `io.github.postvak_app.postvak` (Wayland) and WM class (X11).
- macOS: the `.app` and DMG build as today; the hardened runtime is checked with Slint's renderer on the first PR run.
- `README.md`/`docs/src/index.md` gain the Slint attribution line; `docs/src/gui.md` gains the menu bar, tray and About; `AGENTS.md`'s module map and definition of done drop egui and lavapipe for GUI snapshots.
- `Cargo.toml`: `slint`, `slint-build` (build), `i-slint-backend-testing` (dev), each with its reason; `eframe`, `egui-phosphor`, `egui_kittest` removed.

## 11. Risks

- A screen reader and an input method have not been tried on Slint (toolkit report §6); checked by hand during the rewrite, before the merge.
- Slint's tray on desktops without a StatusNotifierItem host: the icon is not registered, so close quits; covered by §6.
- The read-only `TextInput` for bodies of megabytes: limited as today by `message::clean`; a very long body is the same risk as in egui.
