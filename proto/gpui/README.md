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
| Up, Down, Esc | GPUI Kit's own `List` bindings (`SelectUp`, `SelectDown`, `Cancel`); they win over ours because the Kit list's key context is deeper. They reach the model through the `ListDelegate` |
| Key table from the keymap | works (`window.bindings_for_action`), but shows only `j` for "Next message": Down is Kit's `SelectDown`, a different action. A port would bind Down to our own action, or list both |
| Kit in tests | `gpui_kit::init` runs inside `TestAppContext`; no window `Root` needed for the list |
| Daemon events | not wired: the prototype has no daemon, and the in-memory client's event sender is only held |

Differences from the egui list worth noting: Kit's `ListItem` draws its own hover and selected backgrounds from Kit's theme, so `main.rs` sets those theme colours to Postvak's; the unread dot is a "•" glyph and looks smaller than egui's painted circle.
