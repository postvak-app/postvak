# GPUI Migration Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move the GUI from egui to GPUI for the long term, without shipping anything that is not ready: first a throwaway prototype that measures GPUI against the egui build on the same mailbox, then a go/no-go review, then the port on a long-lived branch that merges only when every GPUI dependency comes from crates.io.

**Architecture:** Three phases. Phase 1 (this plan, Tasks 1–7) is a separate binary crate in `proto/gpui/` on branch `proto/gpui`, never merged; it depends on `postvak` by path and on `gpui-pre` and `gpui-kit` from crates.io, and draws the message list and the body pane from the same temp stores and in-memory daemon client the egui tests use. Phase 2 (Task 8) is a written report and the owner's decision. Phase 3 (Tasks 9–14) is an outline; it gets its own detailed plan only after a "go".

**Tech Stack:** Rust 2024 (toolchain 1.99), `gpui-pre =0.3.9` and `gpui-pre-platform =0.3.9` (snapshot of zed@93d7456, published 2026-10-12), `gpui-kit =0.7.3`, Blitz `=0.3.0-beta.2` through `postvak::gui::html` unchanged, egui 0.36 for the comparison build.

**Spec:** `docs/superpowers/specs/2026-10-13-postvak-gpui-evaluation.md` (§7 prototype, §6 triggers). The owner chose GPUI as the long-term toolkit on 2026-10-13; this plan replaces the spec's "stay on egui, revisit later" with "prototype now, then gate".

## Decisions (the owner can veto any of these before Task 1)

1. **The prototype uses crates.io snapshots, not a Zed git rev.** Research on 2026-10-13 found `gpui-pre` (owner `huacnlee`, Longbridge), a weekly crates.io snapshot of Zed main (0.3.2 → 0.3.9 since 2026-09-03), and GPUI Kit pins it exactly (`gpui = { package = "gpui-pre", version = "=0.3.9" }`). A Zed git rev cannot be mixed with GPUI Kit (different `gpui` crate, different types), and a port could never publish with it. Measuring `gpui-pre` measures what we would ship. Rejected: `gpui` + `gpui_platform` at zed `1de16662` (compiles standalone, but Zed's `[patch.crates-io]` for `async-task`, `calloop` and `khronos-egl` does not reach us, and it pulls git-only `zed-font-kit` and `zed-xim`).
2. **GPUI Kit for list, input and scroll; our own code for the rest.** Its list and input carry AccessKit (README claim, checked in Task 6). Zed's `ui` and `ui_input` are GPL and stay out.
3. **The prototype is a separate crate, not a feature of `postvak`.** `proto/gpui/Cargo.toml` with `postvak = { path = "../..", default-features = false, features = ["html", "testing"] }`. Nothing in `postvak`'s own `Cargo.toml` or `Cargo.lock` changes, so `main`'s CI, `cargo audit` and release-plz never see GPUI. Its binary size is measured with the egui GUI linked in too (feature `html` implies `gui`); Task 7 subtracts it by also measuring `postvak --no-default-features`.
4. **Edits to `postvak` on the branch are only the seams the prototype needs** (Task 2): the fixture builders and `Client::in_memory` behind `testing`, and the HTML renderer's egui types swapped for plain ones. They are written so they could be cherry-picked to `main` after a "go", but they are not proposed for `main` in Phase 1.
5. **The branch is never merged.** It is pushed so the numbers are reproducible, and deleted after the decision; the report (Task 8) lands on `main` as a doc.

### Accepted risks

- **`gpui-pre` is one person's snapshot.** If it stops, the port waits for Zed or another snapshot. Phase 2 weighs this; Phase 1 only needs it to exist today.
- **Linux libraries.** `gpui-pre-platform` links `libxkbcommon`, `libxkbcommon-x11`, `libxcb`, `libfontconfig` and `libfreetype` at build time (eframe loads X11, Wayland and fontconfig at runtime). A binary that links them does not start on a host without them, even for `postvak run`. Task 7 measures this; the port must answer it (Task 9).

## Scope

Phase 1: one branch `proto/gpui` off `origin/main`, never merged. Phase 2: one PR to `main` with the report and the spec update. Phase 3: outline only.

Not in this plan: compose, folders pane, rules views, toolbar and status bar in the prototype; Windows; the web target; any change to the daemon, CLI or MCP.

## Global Constraints

- Phase 1 code is throwaway, but it still builds with `cargo clippy -- -D warnings` and no `unwrap`/`expect` outside tests; it is evidence, and sloppy code makes slow numbers.
- `postvak` on the branch keeps passing `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings` and `cargo test` after Task 2; the egui GUI must not change behaviour (the 11 snapshots stay identical).
- Never read message bodies from a real store; the prototype only opens the fixture stores it builds in a tempdir.
- Report platform coverage honestly: every number in the report says "ran on <OS, GPU or lavapipe>"; anything only built says "compiled on".
- Conventional commits; branch `git switch -c proto/gpui --no-track origin/main`, first push `git push -u origin HEAD`.

## Review Focus

1. **Same input, same work.** Both builds open the same fixture (`snapshots::mailbox` plus a 20 000-message generated INBOX), at the same window size (1280×800) and scale, in a release build. A number without both sides is not reported.
2. **The prototype must not cheat.** Rows go through `list::build_rows` and `apply_pending`, keys through the same `UiAction` meanings, the body through `html::Request`/`Reply` and the same strip protocol. A faster prototype that skips work proves nothing.
3. **Accessibility is checked with a real screen reader**, not by reading our own code: Orca on Linux, VoiceOver on macOS, reading three list rows and the body's subject.
4. **The dependency tree is crates.io only**: `cargo tree -p postvak-gpui-proto --prefix none | grep -F '(https://'` prints nothing.

---

## Phase 1 — prototype

### Task 1: The probe crate builds and opens a window

**Files:**
- Create: `proto/gpui/Cargo.toml`, `proto/gpui/src/main.rs`, `proto/gpui/README.md` (how to run, and "throwaway, never merged")
- Modify: `.gitignore` (`/proto/gpui/target`)

- [ ] **Step 1:** `proto/gpui/Cargo.toml`, its own `[workspace]` so it never joins `postvak`'s build:

```toml
[package]
name = "postvak-gpui-proto"
version = "0.0.0"
edition = "2024"
publish = false

[workspace]

[dependencies]
postvak = { path = "../..", default-features = false, features = ["html", "testing"] }  # the store, rules, list and HTML code under test
gpui = { package = "gpui-pre", version = "=0.3.9" }  # GPUI, crates.io snapshot of zed@93d7456; the version GPUI Kit pins
gpui_platform = { package = "gpui-pre-platform", version = "=0.3.9", features = ["font-kit", "x11", "wayland", "runtime_shaders"] }  # platform backend; runtime_shaders for Xcode 26 without the Metal toolchain
gpui-kit = "=0.7.3"  # list, input and scroll components with AccessKit
async-channel = "2"  # daemon events and render replies into cx.spawn tasks
tempfile = "3"  # the fixture stores live in a tempdir
anyhow = "1"

[dev-dependencies]
gpui = { package = "gpui-pre", version = "=0.3.9", features = ["test-support"] }  # TestAppContext, VisualTestContext, headless capture
```

- [ ] **Step 2:** `main.rs` opens one 1280×800 window with a `Root` view that renders "Postvak" in the bundled Hanken Grotesk (load `assets/fonts/*.ttf` through `cx.text_system().add_fonts`):

```rust
fn main() -> anyhow::Result<()> {
    gpui_platform::application().run(|cx: &mut gpui::App| {
        let bounds = gpui::Bounds::centered(None, gpui::size(gpui::px(1280.), gpui::px(800.)), cx);
        let options = gpui::WindowOptions {
            window_bounds: Some(gpui::WindowBounds::Windowed(bounds)),
            ..Default::default()
        };
        if let Err(e) = cx.open_window(options, |_, cx| cx.new(|_| Root)) {
            eprintln!("could not open the window: {e}");
            cx.quit();
        }
    });
    Ok(())
}
```

- [ ] **Step 3:** `cargo build --release` in `proto/gpui`, install whatever `-dev` packages it asks for, and write them in `README.md`. Run it on Linux (Wayland and X11) and macOS.
- [ ] **Step 4:** Check Review Focus 4 (no git sources). Commit `chore(proto): GPUI probe window`.

### Task 2: Seams in `postvak` the prototype needs

**Files:**
- Modify: `src/gui/test_support.rs`, `src/gui/snapshots.rs` (move `mail` and `mailbox` out), `src/daemon/client.rs` (`Client::in_memory` from `#[cfg(test)]` to `#[cfg(any(test, feature = "testing"))]`, and `pub`), `src/gui/mod.rs` (`pub mod` for what the prototype reads), `src/gui/html/render.rs`, `src/gui/html/mod.rs`, `src/gui/app.rs` (callers of the changed renderer)

**Interfaces produced:**
- `postvak::gui::fixture::{Home, AT, message, mail, mailbox(root, theme), generated(root, count)}` under `testing`, all fallible and under a directory the caller owns (`tempfile` stays a dev-dependency). The tests' `test_support::Fixture` wraps a `Home` in a `TempDir`, derefs to it and keeps its infallible methods and egui `harness()`. `generated(root, 20_000)` adds threads with 1–8 members, deterministic from the uid, in one transaction.
- `postvak::daemon::Client::in_memory(&[&str]) -> (Client, Receiver<(String, Command)>, Sender<Event>)` under `testing`; `test_support::Wires` moves with the fixture.
- `postvak::gui::list::{Row, RowKey, build_rows, apply_pending, Optimistic, row_text, list_date, display_name}` made `pub` (no code change).
- `html::Renderer::start(wake: html::Wake)` (`Arc<dyn Fn() + Send + Sync>`) instead of `start(ctx: egui::Context)`; `Reply::Strip { generation, strip, image }` carries `image: Pixels` (premultiplied RGBA; `Pixels::new` checks the length, `width()`, `height()`, `rgba()`, `to_egui()`) instead of `egui::ColorImage`; the non-`html` stub `Renderer::start` changes the same way; `Request::Select` takes `[f32; 2]` (`Hit` already took `x`, `y`). `App::receive_html` calls `to_egui()`.
- `gui::{body, html, list}` are `pub mod`, with the items above `pub`.

- [ ] **Step 1:** A failing test in `render.rs`: `a_strip_is_plain_premultiplied_rgba`, which paints the existing `HELLO WORLD` fixture and checks `rgba.len() == width * height * 4` and a non-zero alpha somewhere.
- [ ] **Step 2:** Make the renderer changes; the egui app calls `Renderer::start(Box::new(move || ctx.request_repaint()))`.
- [ ] **Step 3:** Move the fixture builders; add `generated`.
- [ ] **Step 4:** `cargo test --all-features`, and `TZ=UTC POSTVAK_SNAPSHOTS=1 cargo test gui::snapshots` under lavapipe: all 11 snapshots identical.
- [ ] **Step 5:** Commit `refactor(gui): toolkit-free HTML strips and a testing fixture`.

### Task 3: The message list

**Files:**
- Create: `proto/gpui/src/list.rs`, `proto/gpui/src/model.rs`, `proto/gpui/src/theme.rs`

**What it does:**
- `Model` (an `Entity`) holds the accounts, `ListState` and `BodyState` copied from `app.rs` without egui; it loads rows with the same store calls as `App::reload_view` and builds them with `list::build_rows` + `apply_pending`.
- `theme.rs` restates `ROW`, `CONTROL`, `INSET`, `PAD`, `LIST_ROW` and the `PAPER`/`NIGHT` palettes as GPUI `rgb()` values, copied from `src/gui/theme.rs`.
- The list is GPUI Kit's virtual list (falling back to `gpui::uniform_list` at `LIST_ROW` if the Kit list cannot do fixed rows), drawing `row_text`'s columns: sender (semibold when unread), subject, count, date in monospace, the flag.
- Keys as actions in the `MessageList` key context, matching `App::keys`: `j`/Down `Next`, `k`/Up `Prev`, Right `Expand`, Left `Collapse`, `x` `ToggleMark`, `e` `Archive`, `?` `ToggleHelp`, Esc `Escape`, Tab `NextFocus`. `Archive` and `ToggleMark` send the same `Command` through `Client::in_memory` and apply the same `Optimistic` edit.
- `?` draws the key table generated from the bindings (`cx.bindings_for_action`), to test spec §4.3's claim.
- Daemon events: a thread forwards the `Receiver<Event>` into an `async_channel`; a `cx.spawn` task updates `Model` and calls `cx.notify()`.

- [ ] **Step 1:** A `#[gpui::test]` in `list.rs`: open the fixture `mailbox`, `simulate_keystrokes("j j")`, assert the cursor is on row 2; `simulate_keystrokes("right")` on a thread row expands it; `e` sends `Command::Apply { action: Action::Archive, .. }` for that message on the in-memory client and hides the row.
- [ ] **Step 2:** Implement until it passes; run it on the 20 000-message fixture and hold `j`.
- [ ] **Step 3:** Commit `feat(proto): message list with keys as actions`.

### Task 4: The body pane

**Files:**
- Create: `proto/gpui/src/body.rs`, `proto/gpui/src/html.rs`

**What it does:**
- Text bodies: header (from, to, date, subject in Newsreader), then the text split by `body::segments` with links as clickable runs.
- HTML bodies: the same flow as `App::html_width`/`html_visible`/`receive_html` — settle the width for 0.1 s, `Request::Load`, then `Paint` for `wanted_strips`, free `far_strips` with `window.drop_image`. Each `Strip` becomes `Arc<RenderImage>` after swapping RGBA to BGRA (GPUI's `RenderImage` is BGRA) and is drawn with `img(ImageSource::Render(..))` at `strip * STRIP / scale`.
- The read timer: after 1 s on an armed body, `MarkRead` with `Optimistic::Seen(true)`, as `mark_read_after_delay`, driven by `cx.background_executor().timer`.

- [ ] **Step 1:** `#[gpui::test]`: select the newsletter fixture (`tests/fixtures/newsletter.html`), `run_until_parked`, assert at least one strip image exists and its pixel size matches `physical(width, scale)`; after advancing the clock 1 s, `MarkRead` was sent once.
- [ ] **Step 2:** Implement; compare the newsletter by eye against `tests/snapshots/gui_html_light.png`.
- [ ] **Step 3:** Commit `feat(proto): body pane with Blitz strips`.

### Task 5: Search input and IME

**Files:**
- Create: `proto/gpui/src/search.rs`

- [ ] **Step 1:** GPUI Kit's input above the list; `/` focuses it, typing filters through `store.search` as `App::reload_view` does, Esc returns focus to the list. While it has focus, `j` types a `j`.
- [ ] **Step 2:** `#[gpui::test]`: `simulate_keystrokes("/ j")` leaves the cursor where it was and the query `j`.
- [ ] **Step 3:** By hand: type 日本語 through fcitx5 or IBus on Wayland and the macOS Japanese input method; note whether the candidate window sits at the caret. Commit `feat(proto): search field`.

### Task 6: Tests and screenshots without a GPU

**Files:**
- Create: `proto/gpui/tests/screenshot.rs`

- [ ] **Step 1:** Render the inbox fixture offscreen at 1280×800 with `HeadlessAppContext` and `current_headless_renderer()` (`capture_screenshot` → `RgbaImage`), write `inbox.png`.
- [ ] **Step 2:** Run it under lavapipe (`WGPU_ADAPTER_NAME=llvmpipe`, `mesa-vulkan-drivers`, no GPU) in a `ubuntu-latest` container; record whether a software adapter is accepted (spec §3 said refused; current main only refuses it for on-screen windows, unverified).
- [ ] **Step 3:** Run it twice and on a second machine; record whether the PNGs are byte-identical (the egui snapshots are, under lavapipe).
- [ ] **Step 4:** AccessKit: with Orca (Linux) and VoiceOver (macOS) running, read three list rows and the body subject. Record what each says. Also `window.debug_a11y_tree_json()` in a test, to see whether tests can query by label as `Fixture` does with kittest. Commit `test(proto): headless screenshot and accessibility notes`.

### Task 7: Measurements

**Files:**
- Create: `proto/gpui/MEASUREMENTS.md`, `proto/gpui/bench.sh`

Both builds, release profile, same machine, fixture `generated(20_000)`, window 1280×800 at scale 1 and 2:

| Measure | How |
|---|---|
| Frame time holding `j` | 10 s of key repeat at 30 Hz; egui: `ctx.input(|i| i.unstable_dt)` logged; GPUI: time between `render` calls; report p50/p95/max |
| Idle CPU | window open, no input, 60 s, `pidstat -p <pid> 1` (Linux) / Activity Monitor energy (macOS) |
| HTML strip latency | from `Request::Load` to the first strip drawn, newsletter fixture |
| Memory | RSS after loading the newsletter, scrolled to the end |
| Binary size | stripped release; GPUI proto minus `postvak --no-default-features`, egui `postvak` minus the same |
| Clean build time | `cargo clean && cargo build --release`, cold kache |
| Dependency count | `cargo tree --prefix none -e normal | sort -u | wc -l` for each |
| Linked libraries | `ldd` on the GPUI binary; run `postvak-gpui-proto --version` in a container without X, Wayland, xkbcommon and fontconfig |
| Startup | process start to first frame |

- [ ] **Step 1:** Write `bench.sh` to produce the build, size, dependency and `ldd` rows automatically.
- [ ] **Step 2:** Run everything on Linux (name GPU and compositor) and macOS (name the chip). Mark any row not run.
- [ ] **Step 3:** Commit `docs(proto): measurements`; push the branch.

## Phase 2 — the gate

### Task 8: Report and decision

**Files:**
- Create: `docs/superpowers/specs/2026-10-xx-postvak-gpui-prototype-report.md`
- Modify: `docs/superpowers/specs/2026-10-13-postvak-gpui-evaluation.md` (§2 decisions log, §3 corrections from the research and Task 6)

- [ ] **Step 1:** The report: the Task 7 table, the Task 5 IME notes, the Task 6 accessibility and headless results, lines of prototype code per pane against the egui equivalents, and every surprise.
- [ ] **Step 2:** Answer the gate questions, each yes/no with evidence:
  1. Frame time p95 and idle CPU no worse than egui.
  2. The list and the body subject are read by Orca and VoiceOver.
  3. Headless screenshots run under lavapipe and are stable enough to compare in CI.
  4. The tree is crates.io only, and the owner accepts `gpui-pre` (one maintainer, weekly) as the pinned source, or a Zed release exists.
  5. A non-GUI `postvak` still starts without the linked GUI libraries (Task 9 has the options if not).
  6. The `?` table generated from the keymap works.
- [ ] **Step 3:** The owner decides: go (Phase 3 gets its own detailed plan), wait (record the new triggers in spec §6), or stop (record why).
- [ ] **Step 4:** PR to `main` with the report and the spec change only; delete `proto/gpui` after merge.

## Phase 3 — the port (outline; detailed plan after "go")

Branch `feat/gpui`, long-lived, rebased on `main` weekly. It merges only when every gate answer is yes, the dependency tree is crates.io only, and the parity list below is done. egui ships from `main` until then.

### Task 9: Where the GUI lives
Decide between keeping `gui` as a feature of the one binary (then the linked X11/xkbcommon/fontconfig libraries are needed by every Linux user, including headless ones) and a second binary `postvak-gui` that `postvak gui` execs. The AppImage, the macOS `.app` and the Homebrew formula follow from that.

### Task 10: Toolkit-free model on `main` first
Extract from `app.rs` what Task 2's report shows is toolkit-free (`gui::model`: `apply`, `handle`, `reload_view`, `rebuild_rows`, `expand`, `collapse`, `act`, `sync_body`, `load_body`, the read timer) behind a small `Ui` trait for repaint and time, and a `KeyMap` table per `Focus`. This lands on `main` under egui, with its tests, so the port only replaces views.

### Task 11: Views, pane by pane
Folders, list, body (text and HTML), toolbar and search, status bar and history, rules/activity/trash, help, the startup window. Each view keeps the rule that it returns actions and only the model changes state; `tests/architecture.rs` rule 3 is restated for GPUI entities.

### Task 12: Tests and snapshots
Port `test_support::Fixture` to `TestAppContext`, every `egui_kittest` test to `#[gpui::test]`, and the 11 snapshots to headless captures under lavapipe; regenerate `tests/snapshots/` and the README images.

### Task 13: Packaging and CI
`ci.yml` installs the `-dev` packages; the AppImage bundles or requires the linked libraries; macOS universal build with `runtime_shaders`, checked on macOS 11; `window.ron` persistence replaced (GPUI Kit dock state or our own file).

### Task 14: Remove egui
Drop `eframe`, `egui-phosphor` (or keep Phosphor as a font), `egui_kittest`; update `AGENTS.md` (module map, snapshot command), `docs/src/gui.md`, `docs/src/install.md`, the `Cargo.toml` reasons that mention egui, and spec GUI §11.
