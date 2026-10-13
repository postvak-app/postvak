# GPUI prototype against the egui build (2026-10-13)

Plan Task 6 and 7. Machine for every local number: AMD Ryzen 5 5600X (12 threads), NVIDIA GeForce RTX 3080 (driver 615.78.08), CachyOS, kernel 6.18.55, KDE Plasma on Wayland. CI numbers say so.

The prototype covers the list, the body pane (text and Blitz HTML) and search; the egui build is the whole GUI. Where that matters, the row says.

## Frame time holding `j`

Both builds open the same generated INBOX (20 000 messages, 4 446 threads), 1280×800 at scale 2, release profile. Each step: one key (egui ArrowDown, GPUI the `Next` action), one frame, drawn offscreen on the RTX 3080 and read back. Both load the new cursor row's body from the store on each step. 300 steps, three runs each.

| Build | p50 | p95 | max |
|---|---|---|---|
| egui (`src/gui/bench.rs`) | 5.09–5.15 ms | 5.56–5.97 ms | 6.73–6.89 ms |
| GPUI (`src/screenshot.rs`) | 6.74–6.78 ms | 7.90–8.01 ms | 9.02–9.56 ms |

GPUI is about 1.6 ms (30 %) slower per step here; both are far inside a 16.7 ms frame. Not measured: on screen with a compositor (no input injection on this Wayland session), where vsync and presentation dominate. The two read back through different paths (egui_kittest and GPUI's `capture_screenshot`), which this cannot separate from drawing.

Run: `cargo test --release --all-features gui::bench -- --ignored --nocapture` at the root, `cargo test --release hold_j -- --ignored --nocapture` in `proto/gpui`.

## Idle

| Build | CPU at idle | RSS |
|---|---|---|
| GPUI, 20 000-message INBOX on screen, no input, 15 s | 0.13 % of one core | 200 MiB |
| egui, same home | not measured | not measured |

The egui run needs `postvak gui` started with `XDG_*` pointing at the same home, which this session's sandbox does not allow (it guards git's config through `XDG_CONFIG_HOME`).

## HTML strips

The renderer is postvak's, unchanged; GPUI adds one step per strip on the UI thread: premultiplied RGBA to straight BGRA (`gpui::swap_rgba_pa_to_bgra`, a float divide per pixel). A full-width strip at scale 2 (2520×512): **3.6 ms p50, 4.7 ms max**. Opening an HTML message paints 3–4 strips, so up to ~14 ms of UI-thread work egui does not have. A port would convert on a background thread (or in the render thread, behind a toolkit-neutral pixel format).

## Build and size

Clean release builds, each in a fresh target dir, kache off, sources already downloaded; postvak's release profile (thin LTO, one codegen unit, stripped) for all three.

| Build | Clean build | Binary |
|---|---|---|
| `postvak`, default features (CLI, MCP, egui GUI, Blitz) | 283 s | 45.0 MB |
| `postvak`, no GUI (`--no-default-features --features mcp`) | 118 s | 14.7 MB |
| GPUI prototype (GPUI, GPUI Kit, Blitz, postvak's core; no CLI or MCP) | 201 s | 52.4 MB |

The egui GUI costs about 30 MB over the CLI; the GPUI prototype alone is 52 MB, part of which is GPUI Kit's default `assets` feature (7.5 MB of icons), which a port could turn off.

## Dependencies

Unique crates in `cargo tree -e normal`:

| Build | Crates |
|---|---|
| postvak core, no features | 189 |
| postvak with egui GUI and Blitz | 474 (+285) |
| postvak core with GPUI, GPUI Kit and Blitz (the prototype's tree, eframe pruned) | 639 (+450) |

Every crate is from crates.io, none from git, in both.

## Libraries the binary needs

`readelf -d … | grep NEEDED`, release profile:

| Build | NEEDED |
|---|---|
| `postvak` (egui) | libc, libm, libgcc_s, ld-linux; X11, Wayland, Vulkan and fontconfig are loaded at runtime |
| GPUI prototype | the same plus **libxkbcommon** and **libxkbcommon-x11** |

So one binary holding both the CLI and a GPUI window would not start on a host without xkbcommon, headless servers included. Without LTO the prototype also needed libxcb, libxcb-xkb, libXau and libXdmcp. Building on Ubuntu worked with Zed's build list (`libxkbcommon-x11-dev libx11-xcb-dev libwayland-dev libfontconfig-dev`); the minimal list is not established.

## Headless screenshots (Task 6)

| Where | Result |
|---|---|
| This machine, RTX 3080 | the whole window renders offscreen at 2560×1600; two renders are identical |
| CI, ubuntu-latest, lavapipe only (`llvmpipe`, device type CPU) | renders; two renders identical; all 10 tests pass |
| RTX 3080 against lavapipe | the list pane (GPUI, bundled fonts) differs in 0.79 % of pixels, by at most 2 per channel: comparable with a tolerance, not byte for byte, as with egui. The HTML differs more, because Blitz uses the system fonts (Noto here, DejaVu on the runner), in both builds |
| CI, macos-latest | the 9 non-screenshot tests pass (list, body with Blitz, search, model); the screenshot test is Linux-only for now (it uses the Linux text system) |

Found on the way: without system fonts the flag (⚑) on a flagged row draws as an empty box; none of the bundled fonts has it, and egui falls back to its bundled Hack. A port needs an icon font or a fallback.

## Not done

- Accessibility: GPUI builds its AccessKit tree only once a real assistive technology activates it (the test platform never does), and nothing ran a screen reader; left for later on request.
- IME: needs a person at the keyboard.
- Startup time; on-screen frame time; egui idle CPU and memory; macOS on screen.

## How herdr-gpui handles the same problems

[herdr-gpui](https://github.com/penso/herdr-gpui) (Apache-2.0, started 2026-09-20, ~1 200 stars, ~347 000 lines of Rust) is a shipping GPUI client for a local daemon, a shape close to Postvak's. Read from a shallow clone of its `main` on 2026-10-13; nothing of it was built or run here.

| Problem found here | What herdr-gpui does |
|---|---|
| Shipping on a snapshot crate | `gpui = { package = "gpui-pre", version = "=0.3.6" }`, exact, three snapshots behind 0.3.9. Dependabot ignores `gpui`: upgrades are "a manual, tested change". Releases daily for macOS (universal, signed DMG), Linux (x86_64 and aarch64) and Windows. No GPUI Kit: its own components |
| Linked xkbcommon and friends | The GUI is its own binary (`herdr-gpui`), a client of the `herdr` daemon. Linux ships as `.deb`, `.rpm` and Arch packages that declare `libxkbcommon0`, `libxkbcommon-x11-0`, `libxcb1`, `libfreetype6`, `libfontconfig1`, `libwayland-client0`, `libvulkan1` (and ALSA) as dependencies, plus a tarball; a smoke test installs each package in a container and runs `ldd` and `--help` (which exits before GPUI starts, so no display). No AppImage. CI installs `libxkbcommon-dev libxkbcommon-x11-dev libxcb1-dev libfreetype6-dev libfontconfig1-dev fonts-dejavu-core` |
| Accessibility | On, not switched off: the terminal is an AccessKit node with `Role::Terminal` whose rows become text runs (`a11y_synthetic_children`), built only while `window.is_a11y_active()`, so it costs nothing without a screen reader; macOS selection tools read `AXSelectedText` |
| Tests and screenshots | 372 files use `#[gpui::test]`/`TestAppContext`. Screenshots come from `Window::render_to_image` only in a development-only `--mockup --capture` mode for design review; no pixel comparison in CI. Their notes say headless layout tests do not prove native glyph or OS input correctness; native window tests are opt-in and need a desktop |
| IME | Their own text inputs implementing `EntityInputHandler` (`input.rs`, `search_input.rs`, ~1 100 lines), with one rule: paint, hit-test and IME placement share one geometry |
| Glyphs missing from bundled fonts (our ⚑) | Icons are 97 SVGs served through their own `AssetSource`, not font glyphs |
| UI-thread work (our strip conversion, store reads) | A rule: no blocking socket or disk I/O, sleeps or expensive computation on the UI thread; worker threads or the background executor, results applied through a context update |
| Toolkit-free logic | A rule: the protocol and client crates must not depend on GPUI; the UI crate holds presentation only |

What this changes for the gate: shipping a real app on `gpui-pre`, pinned exactly with manual upgrades, works on all three platforms. The linked libraries are handled by a separate GUI binary and distro packages, which for Postvak means a `postvak-gui` binary and dropping or reworking the AppImage. Accessibility is usable in practice, but someone still has to try Postvak's list with a screen reader. Neither herdr nor this prototype compares screenshots in CI; a port would keep a tolerance-based comparison (2 per channel held between NVIDIA and lavapipe here) or move to layout tests.
