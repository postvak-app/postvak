# GPUI evaluation — design

Date: 2026-10-13. Status: owner decision recorded in §2; plan in `docs/superpowers/plans/2026-10-13-gpui-migration.md`. Compares Zed's GPUI with the GUI as built from `2026-10-06-postvak-gui-design.md` and `2026-10-07-postvak-html-design.md`. Decides nothing in code; it records what a switch would buy, what it would cost, and when to look again.

## 1. Purpose and scope

The GUI is eframe/egui 0.36 on wgpu, tested with `egui_kittest` (AccessKit queries, PNG snapshots under lavapipe), with HTML mail rendered by Blitz on a CPU thread into egui textures. About 11k lines live in `src/gui/`.

In scope: the benefits GPUI would bring to this GUI, the costs of moving, a recommendation, and the conditions that would reopen the question.

Out of scope: other toolkits (Iced, Slint, Xilem), and any change to the daemon, the CLI or MCP, which a GUI toolkit does not touch.

## 2. Decisions log

| Decision | Choice | Rejected |
|---|---|---|
| Long-term toolkit | GPUI (owner, 2026-10-13); egui keeps shipping until the port merges | Stay on egui indefinitely |
| Next step | The §7 prototype now, on `gpui-pre` and GPUI Kit from crates.io, then a go/no-go review | Wait for the §6 triggers first; prototype against a Zed git revision (cannot be mixed with GPUI Kit, cannot ship) |
| Port | A long-lived branch that merges only when every GPUI dependency is on crates.io and the review says go | Port now on `main` |
| Text gaps meanwhile | Load system fonts as egui fallbacks at startup, as GUI §11 already plans | Wait for a toolkit change to fix CJK |

## 3. State of GPUI on 2026-10-13

- **Releases.** The last crates.io release is `gpui` 0.2.2, 2025-10-22. Main still carries 0.2.2 as its version and has changed a great deal since. The README warns of frequent breaking changes; there is no changelog.
- **Governance.** Developed inside the Zed monorepo; Zed reportedly paused community-facing GPUI work in February 2026. Snapshot and fork crates fill the gap: `gpui-pre`, `fc-gpui`, `gpui-ce`. `gpui-pre` (Longbridge) publishes a crates.io snapshot of Zed main about weekly, 0.3.9 on 2026-10-12 (zed@93d7456), with the wgpu renderer, headless rendering and AccessKit; GPUI Kit 0.7.3 pins it exactly.
- **Licence.** `gpui` and the `gpui_*` platform crates are Apache-2.0. Zed's own components (`ui`, `ui_input`) are GPL-3.0-or-later and not usable here; GPUI Kit (formerly `gpui-component`, Longbridge) is Apache-2.0.
- **Linux renderer.** Blade was replaced by wgpu on main, 2026-02-13 (zed#46758), fixing freezes on NVIDIA and Wayland. 0.2.2 still uses blade.
- **Headless rendering.** `gpui_wgpu` renders offscreen on Linux with RGBA readback since 2026-09-25 (zed#64718). On main as of 2026-10-13, `WgpuContext::create_headless` accepts CPU adapters (only on-screen windows can refuse them), so lavapipe should work; not yet run, and no CI job is known to run screenshot tests with it.
- **Accessibility.** AccessKit plumbing landed on main 2026-05-27 (zed#56065), deliberately minimal; Zed opts out with `Application::inaccessible()` after nightly panics. Not in any release. GPUI Kit states AccessKit roles, names, states and actions on its components.

## 4. Benefits

Each item names the part of Postvak it would change.

### 4.1 Text

- Platform shaping: CoreText on macOS, DirectWrite on Windows, cosmic-text 0.19 with swash on Linux (main).
- Closes the gaps GUI §11 records: system font fallback, so CJK no longer renders as boxes; real font weights, so `theme.rs` no longer needs the static Medium and SemiBold instances bundled because egui cannot pick a weight; native-looking text on macOS.
- Right-to-left is only partly better: on CoreText bidi lines render correctly, but caret and selection geometry break inside reordered runs (zed#31102 open). egui has no RTL shaping at all.

### 4.2 IME

- Native input methods on macOS (NSTextInputClient), Wayland (text-input) and Windows. Matters for the search field now and compose later.
- Only widgets implementing `EntityInputHandler` get it; Zed's example input is 746 lines. A library input (GPUI Kit) rather than our own.

### 4.3 Actions, keymaps and focus

- `actions!` and `KeyBinding` scoped to focus contexts are what drives Zed's vim mode. They match GUI §6: single keys (`j`, `k`, `e`, `x`, `v`, `?`, `Esc`) that go to the focused pane while text fields take keys when focused. Today `app.rs` routes this by hand.
- The `?` key table could be generated from the keymap instead of kept beside it.
- Focus handles with `tab_index()` and `tab_stop()` give Tab navigation between panes.

### 4.4 Async on the UI thread

- `cx.spawn` runs futures on the window's event loop; a `gpui_tokio` bridge exists.
- `daemon::Client` events and the 2 s modification-time poll of `rules.toml` and `config.toml` (GUI §8) become tasks that update an entity and call `cx.notify()`, instead of being checked inside frames.

### 4.5 Rendering model

- Retained views: a view is rebuilt and laid out only when it calls `cx.notify()`. egui rebuilds the whole UI on each repaint. No power measurements found either way; the gain is expected to grow with the UI, not to show today.

### 4.6 Layout

- Taffy flexbox with Tailwind-like styling, the same layout engine Blitz uses. The grid in `theme.rs` (`ROW`, `CONTROL`, `INSET`, `PAD`) becomes a handful of style tokens on flex containers rather than sizes applied by hand.

### 4.7 Components (GPUI Kit)

- Data tables and lists with virtual scrolling over hundreds of thousands of rows, variable row heights included: the message list and thread rows.
- Dock layout with resizable panels, draggable tabs and nested splits, serialisable: the three panes and the window state eframe's persistence keeps today.
- Markdown and simple HTML views: not a replacement for Blitz on real mail, but usable for text bodies with links.
- Semantic themes from a schema file; an editor component on a rope.

### 4.8 Testing

- `#[gpui::test]` gives deterministic async tests with simulated input, a fit for the daemon-event and read-timer tests that `egui_kittest` drives today.
- With headless wgpu rendering (§3) the snapshot PNGs in `tests/snapshots/` could carry over, once a software adapter is allowed in CI.
- Finding widgets by label, as `test_support::Fixture` does, needs AccessKit on the components.

### 4.9 Platforms

- Windows first-class since Zed's Windows release, 2025-10-15. Web/WASM on main, unreleased.

## 5. Costs

- **Nothing shippable from Zed itself.** Every gain in §3 except the platform text systems is on main only, and `cargo publish` rejects git dependencies. A port would pin a snapshot crate (`gpui-pre`) exactly, with its reason, and depend on its maintainer.
- **Linked GUI libraries.** `gpui-pre-platform` links libxkbcommon, libxkbcommon-x11, libxcb, fontconfig and freetype at build time, where eframe loads X11, Wayland and fontconfig at runtime; a single binary would then need them even for `postvak run`.
- **Accessibility.** egui exposes AccessKit today; released GPUI does not, and main's support is switched off in Zed itself.
- **Tests.** `test_support::Fixture`, every `egui_kittest` test and the lavapipe snapshot workflow are rewritten. The `gui` view rules in `tests/architecture.rs` are restated for entities.
- **Rewrite.** About 11k lines. The `UiAction` pattern (views return actions, `app` alone changes state) becomes entities updated through `cx`; the rule survives but its enforcement changes.
- **HTML view: no gain.** No Blitz backend for GPUI exists. Strips are still painted on the CPU and shown as images. `gpui-wry` embeds a system webview but is macOS and Windows only and draws over whatever lies behind it, the flaw HTML §2 rejected `wry` for.
- **Gaps in 0.2.2.** `register_url_scheme` unimplemented on Linux and Windows; no tray or native notifications (we have `notify`); the default macOS build fails on Xcode 26 without the Metal toolchain unless `runtime_shaders` is on.
- **Packaging.** eframe loads X11, Wayland and fontconfig at runtime, which the AppImage relies on; GPUI's Linux dependencies are unverified against `packaging/linux/appimage.sh`.

## 6. Triggers to revisit

Look again when all of these hold:
1. A crates.io release (from Zed or a maintained snapshot crate) carries the wgpu renderer, headless rendering and AccessKit.
2. GPUI Kit, or another Apache-2.0 library, targets that release with AccessKit on its list, table, input and dock components.
3. Headless rendering accepts a software adapter for snapshot tests on a GPU-less CI runner.

Or sooner if users report CJK, RTL or IME problems that system-font fallback in egui does not fix.

## 7. Prototype, when triggered

A throwaway branch, not merged:
- The message list (virtual scrolling, thread rows, the GUI §6 keys through actions) and the body pane (text view, Blitz strips as images) against `RecordingOps` fixtures.
- Measured against the egui build on the same fixture: frame time while holding `j`, idle CPU, binary size, clean build time, and a screen reader reading the list on macOS and Linux.
- Ran on Linux and macOS, reported as compiled on vs ran on.

## 8. Sources

- zed#46758, blade replaced by wgpu: https://github.com/zed-industries/zed/pull/46758
- zed#64718, Linux headless rendering: https://github.com/zed-industries/zed/pull/64718
- zed#56065, AccessKit; zed#41138, Windows screen readers; zed#31102, RTL: https://github.com/zed-industries/zed
- Rust GUI ecosystem report, GPUI chapter: https://github.com/GoldStrikeArch/rust-gui-desktop-ecosystem-state/blob/main/report/03-gpui.md
- GPUI Kit: https://github.com/longbridge/gpui-kit
- gpui-pre: https://github.com/ducktape-industries/gpui-pre
- gpui-wry: https://lib.rs/crates/gpui-wry
