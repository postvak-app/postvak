# postvak-gpui-proto

Throwaway GPUI prototype of the Postvak mail window, Phase 1 of `docs/superpowers/plans/2026-10-13-gpui-migration.md`. It lives on branch `proto/gpui` and is never merged.

Its own workspace and lock file, so `postvak`'s build and CI never see GPUI. Every dependency comes from crates.io: `gpui-pre =0.3.9` (a snapshot of zed@93d7456) and `gpui-kit =0.7.3`, which pins the same snapshot.

## Run

```sh
cd proto/gpui
cargo run --release
```

Force X11 on a Wayland session with `WAYLAND_DISPLAY= cargo run --release`. `POSTVAK_PROTO_COUNT=20000 cargo run --release` opens a generated INBOX of 20 000 messages instead of the snapshot mailbox's `work` INBOX.

Tests: `cargo test --release` (the model without a window, and keystrokes through GPUI's `TestAppContext`).

## Linux build and runtime libraries

Built on Arch-based CachyOS without installing anything extra. The binary links these at load time (`ldd`), where the egui build loads its display libraries at runtime:

- `libxcb.so.1`, `libxcb-xkb.so.1`, `libXau.so.6`, `libXdmcp.so.6`
- `libxkbcommon.so.0`, `libxkbcommon-x11.so.0`

fontconfig and freetype are not in the `ldd` list. Wayland, Vulkan and EGL are loaded at runtime.

On Debian or Ubuntu, building probably needs `libxkbcommon-x11-dev` and `libxcb1-dev` (unverified).

## Task 1 results (2026-10-13)

| Check | Result |
|---|---|
| Release build | 3 min 23 s, clean, kache off; 1036 packages in the lock (egui included, through `postvak`'s `html` feature) |
| Git sources in `Cargo.lock` | none |
| Normal dependencies (`cargo tree -e normal`, unique lines) | 948 |
| Binary size | 39.5 MB, Cargo's default release profile (not `postvak`'s thin LTO), not stripped; Task 7 compares like with like |
| KDE Plasma, Wayland | ran on: window opens and draws "Postvak" in the bundled Newsreader; screenshot checked |
| KDE Plasma, X11 (XWayland) | ran on: runs 5 s with no errors; not looked at |
| macOS | not run |

## Task 3 results: the message list (2026-10-13)

`model.rs` is `App`'s list logic without egui (rows through `list::build_rows` and `apply_pending`, `same_row`, `targets`, `act` with optimistic edits, expand and collapse). `list.rs` draws it with GPUI Kit's `List` and binds the keys as actions in the `MessageList` context.

| Check | Result |
|---|---|
| Snapshot mailbox, `work` INBOX | ran on KDE Wayland: five thread rows newest first, "(2)" on the offsite thread, unread dots, flag, Hanken Grotesk with SemiBold for unread senders, dates in Spline Sans Mono, the cursor row selected; screenshot checked |
| Generated INBOX, 20 000 messages | ran on KDE Wayland: 4 446 thread rows, opens and draws; not timed yet (Task 7) |
| `j`, `k`, Right, Left, `x`, `e`, `?`, Esc | `#[gpui::test]` with simulated keystrokes: cursor moves, a thread opens and closes, `e` sends one `Apply` for both thread members and hides the row, `?` opens the key table and Esc closes it |
| Up, Down | GPUI Kit's own `List` bindings (`SelectUp`, `SelectDown`); they win over ours because the Kit list's key context is deeper. They reach the model through the `ListDelegate` |
| Esc | Kit binds it to `Cancel`, which clears the Kit list's selection, and the switch that stops that is crate-private. Postvak binds `escape` to its own `Escape` action in Kit's `List` context, after Kit, which replaces Kit's binding |
| Key table from the keymap | works with `window.bindings_for_action_in(action, list focus handle)`; plain `bindings_for_action` reads the context stack left from the last drawn element and came back empty once the window had two panes. Shows only `j` for "Next message": Down is Kit's `SelectDown`, a different action. A port would bind Down to our own action, or list both |
| Kit in tests | `gpui_kit::init` runs inside `TestAppContext`; no window `Root` needed for the list |
| Daemon events | not wired: the prototype has no daemon, and the in-memory client's event sender is only held |

Differences from the egui list worth noting: Kit's `ListItem` draws its own hover and selected backgrounds from Kit's theme, so `main.rs` sets those theme colours to Postvak's; the unread dot is a "•" glyph and looks smaller than egui's painted circle.

## Task 4 results: the body pane (2026-10-13)

`body.rs` follows the list's cursor (it observes the Kit list's state), shows the header and the text, or the HTML page through `postvak`'s own render thread: the `Load` → `Laid` → `Paint` → `Strip` protocol of `App::html_width`, `html_visible` and `receive_html`, unchanged. The window draws the list on the left (440 px) and the body beside it.

| Check | Result |
|---|---|
| Newsletter (`tests/fixtures/newsletter.html`) | ran on KDE Wayland: laid out by Blitz, painted in strips, drawn as GPUI images; header bar, link colours, table, quote, code block, footer and the `cid:` image all show and are sharp; screenshot checked |
| Strips as GPUI images | `RenderImage` is BGRA, Blitz paints RGBA: one channel swap per strip on the UI thread. Blitz's pixels are premultiplied; whether GPUI expects straight alpha is unchecked (mail is mostly opaque) |
| Waking the window | the render thread's wake callback sends on a `futures` channel; a `cx.spawn` task drains it and calls `receive`. No polling per frame |
| Read after 1 s | `#[gpui::test]`: a message shown at start stays unread; after `j k` it is read one (simulated) second later, not at half a second, and `MarkRead` is sent once. The timers use the executor's clock (`executor.now()`), so the test advances it by hand |
| HTML in a test | the newsletter paints in a headless test window with the real render thread (`allow_parking`) |
| Width changes | laid out again once a new width held for `html::SETTLE`, as egui does; not tried by hand |
| Not done | links (hover, click), text selection, the HTML/text toggle, attachments, remote-content notice, the dark theme; scrolling a long page only checked in code |

## /simplify pass (2026-10-13)

- The model is one `Entity<Model>` shared by the list's delegate and the body, instead of living inside GPUI Kit's delegate. The body observes the model, not the Kit list.
- `armed_for` (the row the user moved to) replaces a one-shot flag the body took; the read timer is a `Task` in the shown message, so moving on cancels it.
- Strips: converted in place (`Pixels::into_rgba`) with GPUI's `swap_rgba_pa_to_bgra`, which also un-premultiplies; a plain channel swap drew semi-transparent edges too dark.
- The body is laid out at its scroll area's real width, not the window width minus the list's; the header text is computed once per message.
- One `send_apply` for archive and mark-read, as `App` has; the settle timer is replaced instead of piling up; the window is drawn only when a render reply changed something.
- The temp home and the command receiver are owned by `main`, no longer leaked; `init` is shared by `main` and the tests.

Skipped: sharing `same_row`, `targets`, the parent-row search, the pending-Seen fold and the theme constants from `src/gui` (changes to the egui code beyond this branch's seams); a GPUI Kit theme file instead of four colours set by hand; trimming `pub` in a binary crate.

## Task 5 results: search (2026-10-13)

GPUI Kit's `Input` above the list. Typing searches every folder of the shown accounts through `store.search`, as `App::reload_view` does, and the hits replace the threads (`list::merge_hits`).

| Check | Result |
|---|---|
| `/` focuses the field; typing searches | `#[gpui::test]`: `/ o f f s i t e` finds the two offsite messages, unthreaded |
| `j` while the field has focus | types a `j`. The list keys are bound in Kit's `List` context, active only while the list has focus; bound one level up (the list pane) they would fire inside the field too |
| Esc in the field | Kit's input handles Esc first (IME composition, extra cursors) and passes its own `input::Escape` on; the list pane takes that one, empties and closes the search and focuses the list. Test: `j` moves the cursor again afterwards |
| Look | ran on KDE Wayland: the field draws in GPUI Kit's default input style (white, rounded), not Postvak's; screenshot checked |
| IME (日本語 through fcitx5 or IBus on Wayland, the macOS Japanese input method) | not run: needs a person at the keyboard. Kit's input implements GPUI's `EntityInputHandler`, which is where IME composition comes in |
