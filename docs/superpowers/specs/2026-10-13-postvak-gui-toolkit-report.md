# GUI toolkit prototypes — report

Date: 2026-10-13. Status: decided 2026-10-13 (§7): GPUI dropped, Slint under its royalty-free licence. Phase 1 and 2 of `docs/superpowers/plans/2026-10-13-gpui-migration.md`, against the questions of `2026-10-13-postvak-gpui-evaluation.md`.

The prototype is the throwaway crate `proto/gpui` on branch `proto/gpui`, draft PR postvak-app/postvak#130, never merged; its last commit is `5bdfc44`, and the PR's `refs/pull/130/head` keeps it after the branch is deleted. Its `README.md` has the per-task notes and `MEASUREMENTS.md` the numbers and their method.

## 1. What was built

About 1 600 lines on `gpui-pre =0.3.9` and `gpui-kit =0.7.3`, every crate from crates.io:

- **List:** GPUI Kit's virtual `List` over rows from `postvak::gui::list` (threads, expand and collapse, marks, archive with the optimistic edit), keys as GPUI actions (`j k → ← x e ? / Esc`), the `?` table read from the keymap.
- **Body:** header and text, or HTML through postvak's own Blitz render thread, its strips drawn as GPUI images; the read timer.
- **Search:** GPUI Kit's `Input`; typing searches every folder, `j` types a `j`, Esc closes it.
- **Model:** `App`'s list logic without egui, one `Entity<Model>` shared by both panes.
- **Tests:** 10 (model, keystrokes through `TestAppContext`, the newsletter painted in a headless window with the real render thread, the read timer on the executor's clock, an offscreen screenshot), plus two benchmarks.

Seams added to `postvak` (on the branch only): the HTML renderer speaks plain premultiplied RGBA and wakes through a callback (`html::Pixels`, `html::Wake`); the GUI fixture moved to `gui::fixture` and, with `Client::in_memory`, is built under `testing`. The 11 egui snapshots are unchanged (CI, lavapipe).

Not built: folders pane, toolbar, status bar, rules and activity views, links and selection in HTML, attachments, dark theme, daemon events.

## 2. Gate

| # | Question | Answer | Evidence |
|---|---|---|---|
| 1 | Frame time p95 and idle CPU no worse than egui | **No for frame time; idle unanswered.** Holding `j` on 20 000 messages, offscreen and read back on an RTX 3080: egui p50 5.1 ms, p95 5.8 ms; GPUI p50 6.8 ms, p95 7.9 ms, about 30 % slower, both far inside 16.7 ms. GPUI idles at 0.13 % of one core and 200 MiB; egui idle not measured (the session's sandbox blocks the `XDG_*` launch) | `MEASUREMENTS.md`, three runs each |
| 2 | List and body read by Orca and VoiceOver | **Not tested** (deferred by the owner). GPUI builds its AccessKit tree only once an assistive technology activates it; the test platform never does. herdr-gpui uses it in production | — |
| 3 | Headless screenshots under lavapipe, stable enough for CI | **Yes.** Whole window offscreen on a GPU-less runner (`llvmpipe`), identical across two renders; against the RTX 3080 the GPUI-drawn pane differs by at most 2 per channel. Exact comparison only per renderer, as with egui | CI run of the `GPUI prototype` workflow |
| 4 | Crates.io only, and the owner accepts `gpui-pre` | **Tree: yes. Acceptance: the owner's call.** herdr-gpui ships daily on all three platforms with `gpui-pre` pinned exactly and upgraded by hand | `cargo tree`, herdr-gpui |
| 5 | A non-GUI `postvak` still starts without the GUI's libraries | **No, for one binary.** The GPUI binary needs libxkbcommon and libxkbcommon-x11 at load time; egui's needs only libc. A port means a separate `postvak-gui` binary, as herdr does | `readelf -d` |
| 6 | The `?` table generated from the keymap | **Yes, with one caveat.** It works through `bindings_for_action_in(action, list focus)`; GPUI Kit's own Up and Down are other actions, so "Next message" lists only `j` until Down is bound to ours | test `keys_move_the_cursor_open_a_thread_and_archive_it` |

## 3. Other findings

- **Strips cost the UI thread.** Turning a 2520×512 strip into a GPUI image (premultiplied RGBA to straight BGRA) takes 3.6 ms on the UI thread; opening a newsletter paints 3–4. A port converts off the UI thread. A plain channel swap was wrong: it drew translucent edges too dark.
- **Bundled fonts miss symbols.** ⚑ draws as an empty box without system fonts; egui falls back to Hack. Icons as SVG (herdr) or an icon font fix it.
- **Kit's defaults leak through.** Its list paints its own hover and selection (set to Postvak's colours by hand), its input draws in Kit's white style, and its `Cancel` on Esc clears the list selection (Postvak binds Esc to its own action in Kit's `List` context instead). Its `assets` feature adds 7.5 MB of icons.
- **Two GPUI pitfalls.** `window.bindings_for_action` reads the context stack left by the last drawn element and came back empty once the window had two panes; `request_animation_frame` does nothing in tests (no frame loop), so a redraw for a pane that has no size yet is deferred with `cx.defer_in` instead.
- **Size and build.** GPUI, GPUI Kit and Blitz over postvak's core: about 450 extra crates against egui's 285; the prototype binary is 52 MB against egui's GUI share of about 30 MB; a clean release build of the prototype 201 s against 283 s for the whole of postvak.
- **What worked first time:** GPUI Kit's list and input in GPUI's test harness, Blitz output as GPUI images, simulated keys and timers. GPUI's test harness is easier to drive than `egui_kittest` for keys and time.

## 4. Evidence from herdr-gpui

herdr-gpui (Apache-2.0, a GPUI client of a local daemon, about 347 000 lines) ships daily for macOS, Linux and Windows on `gpui-pre =0.3.6`, with Dependabot told to leave GPUI alone. Its GUI is a separate binary; Linux ships as `.deb`, `.rpm` and Arch packages that declare xkbcommon and the rest, with an `ldd` smoke test, and no AppImage. It keeps GPUI's AccessKit on and feeds it only while a screen reader is active, draws icons as SVG, writes its own text inputs with IME (about 1 100 lines), and forbids blocking work on the UI thread. It does not compare screenshots in CI. Details in `MEASUREMENTS.md`.

## 5. What a port takes

From the egui GUI's 11 200 lines, about 2 000 carry over unchanged (the Blitz renderer, the list and body helpers, fixtures); the rest is new GPUI code, roughly 7 000–9 000 lines: folders, the rest of the body (links, selection, attachments, source), status bar and history, toolbar, rules, activity and trash views, the startup window, dark theme, daemon events and reconnect. About 250 test functions and 11 snapshots move to `#[gpui::test]` and headless captures with a tolerance. Packaging changes: a `postvak-gui` binary, Linux packages that declare the GUI libraries (the AppImage reworked or dropped), the macOS app checked on its oldest supported version. Phase 3 of the plan lists the order.

Before any of that, two checks the prototype could not do: a screen reader on the list (macOS and Linux) and an input method in the search field.

## 6. Slint, the same prototype

Before deciding, the owner asked for Slint to be tried the same way. `proto/slint` on branch `proto/slint` (draft PR postvak-app/postvak#131, never merged) builds the same list, body and search on `slint =1.18.1`, over the same seams, fixture and model (`model.rs` copied unchanged: it has no toolkit in it). About 1 300 lines, of which 280 are Slint markup. Its `README.md` has the details.

| | egui (today) | GPUI prototype | Slint prototype |
|---|---|---|---|
| Release line | crates.io, stable | `gpui-pre`, a weekly snapshot, one maintainer | crates.io 1.x, a release about every two months |
| Licence | MIT OR Apache-2.0 | Apache-2.0 | GPL-3.0, or royalty-free with attribution (`AboutSlint` in an About dialog, or the badge on the download page), or commercial |
| Libraries needed to start | libc only | + libxkbcommon, libxkbcommon-x11: a separate GUI binary | libc only: one binary still works |
| Snapshots without a GPU | lavapipe | lavapipe; within 2 per channel across GPUs | Slint's software renderer in the test itself; byte-identical, no GPU or Vulkan at all |
| Accessibility in tests | AccessKit queries in `egui_kittest` | tree built only with a live screen reader; none in tests | elements found by accessible label in tests |
| Idle, 20 000 messages | not measured | 0.13 % of a core, 200 MiB | 0.00 %, 133 MiB |
| Holding `j`, 20 000 messages | 5.1 ms p50 (offscreen, GPU, read back) | 6.8 ms p50 (same method) | 0.9 ms p50 frame on screen (CPU side; a different method); 17.3 ms with the software renderer at 2560×1600 |
| HTML strip to an image | — | 3.6 ms (format conversion) | 1.8 ms (one copy) |
| Binary, clean build | 45 MB for the whole app | 52 MB, 201 s | 38 MB, 241 s (upper bound) |
| Crates over postvak's core | +285 | +450 | +324 |
| Keymap | by hand in `app.rs` | GPUI actions and key contexts; GPUI Kit's list binds its own keys | by hand: a Rust table, the help read from it |
| Code | Rust | Rust | Slint markup for layout, Rust for state |

The frame-time rows are not one method: no offscreen GPU path was used for Slint, and no on-screen measurement for egui and GPUI. All three are far inside a 16.7 ms frame.

Against the gate (§2), Slint answers questions 3 and 5 better (snapshots without any GPU; one binary), question 4 better (a stable release line, no snapshot crate), question 1 at least as well where it was measured (idle CPU and memory), and question 6 the same way egui does. Question 2 (a screen reader) is as untested as for GPUI, but its accessibility labels are testable today. Its costs: the licence attribution or GPL, a second language for the UI, and a keymap written by hand (as egui's is).

## 7. Decision

Recommendation: **Slint over GPUI**, if the owner accepts its royalty-free licence (an `AboutSlint` widget in an About dialog, or the badge on the download page). It keeps one binary, needs no GPU for snapshot tests, and does not depend on a one-person snapshot crate. If the licence is not acceptable: GPUI, with the conditions of §5 (screen reader and input method first, a separate GUI binary, strip conversion off the UI thread).

Owner decision, 2026-10-13: **GPUI is dropped. Slint is the long-term toolkit, under its royalty-free licence** (Postvak shows the `AboutSlint` widget in its About dialog, or the badge on its download page). egui keeps shipping until a Slint port merges. Next: a detailed Slint port plan, whose first steps are a screen reader on the list and an input method in the search field. The GPUI prototype's draft PR postvak-app/postvak#130 is closed and its branch deleted; its commits stay reachable through `refs/pull/130/head`.
