# GPUI prototype — report

Date: 2026-10-13. Status: gate answered below; owner decision in §6. Phase 1 and 2 of `docs/superpowers/plans/2026-10-13-gpui-migration.md`, against the questions of `2026-10-13-postvak-gpui-evaluation.md`.

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

## 6. Decision

Recommendation: **go, with conditions.** Nothing found blocks a port: the snapshot crate is shippable (herdr), headless tests work under lavapipe, and the cost in frame time is real but small. The conditions are the two manual checks in §5 as the first steps of Phase 3, a separate GUI binary from the start, and strip conversion off the UI thread.

Owner decision: _pending_.
