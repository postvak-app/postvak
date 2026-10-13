# postvak-slint-proto

Throwaway Slint prototype of the Postvak mail window, the same scope as `proto/gpui` so the two can be compared: list, body pane (text and Blitz HTML), search. Branch `proto/slint`, never merged. Its own workspace and lock file; every dependency from crates.io (`slint =1.18.1`).

Slint's licence here is its royalty-free licence for desktop applications: Postvak would show the `AboutSlint` widget in an About dialog, or the "Made with Slint" badge on its download page. GPL-3.0 and a commercial licence are the alternatives.

## Run

```sh
cd proto/slint
cargo run --release
```

`POSTVAK_PROTO_COUNT=20000` opens a generated 20 000-message INBOX; `--home DIR` keeps the home in DIR; `POSTVAK_PROTO_BENCH=1` holds `j` on screen and prints timings. Tests: `cargo test --release`; benchmarks: `cargo test --release hold_j -- --ignored --nocapture`, `... strip_conversion ...`.

## Shape

- `ui/mail.slint`: the window in Slint's markup; it draws and calls back, nothing more.
- `src/app.rs`: all state and decisions, as `app.rs` in the egui GUI: the keymap (the help table is read from it), the rows as a Slint `Model` built only for the rows on screen, the shown message, the HTML strips through postvak's render thread, the read timer.
- `src/model.rs`: copied unchanged from `proto/gpui`; it has no toolkit in it.

## Results (2026-10-13; machine as in `proto/gpui/MEASUREMENTS.md`)

| Check | Result |
|---|---|
| On screen, KDE Wayland | ran on: list, search field, header and the whole newsletter; ⚑ draws (Slint falls back to system fonts); screenshot checked |
| Tests | 11, under Slint's testing backend: model, keys (`j k → ← x e ? Esc` and Up/Down from our own keymap), search (`j` types a `j`, Esc closes it), the newsletter painted and read a second after it is opened (mock time), accessible labels, a snapshot |
| Accessibility labels | the search field and each row are found by accessible label (`ElementHandle::find_by_accessible_label`), so tests can check them as `egui_kittest` does; needs Slint's debug info in test builds. A screen reader was not run |
| Snapshot without a GPU | Slint's software renderer draws the whole window in the test, at 2560×1600; two renders on different threads are byte-identical. No lavapipe needed |
| Holding `j`, 20 000 messages, on screen (femtovg, OpenGL, RTX 3080, scale 1) | key handled p50 0.06 ms; frame rendered (CPU side) p50 0.88 ms, p95 1.0 ms; three runs |
| Holding `j`, software renderer, offscreen at scale 2 | p50 17.3 ms, p95 17.7 ms: a full CPU repaint of 2560×1600 each frame, not comparable with the GPU numbers |
| Idle, 20 000 messages on screen | 0.00 % of one core over 20 s; RSS 133 MiB (GPUI: 0.13 %, 200 MiB) |
| HTML strip to an image | 1.8 ms per 2520×512 strip, one copy: Slint takes premultiplied RGBA as it is (GPUI: 3.6 ms, a conversion) |
| Libraries the binary needs | libc, libm, libgcc_s only, as egui: one binary with the CLI keeps working on a headless server |
| Clean release build | 241 s (with other builds running alongside: an upper bound); binary 37.7 MB without test debug info |
| Dependencies | 513 crates with postvak's core and Blitz (+324 over the core; egui +285, GPUI +450); none from git |

## Found on the way

- Generated code unwraps: `include_modules!` puts Slint's generated Rust in this crate, where `clippy::unwrap_used` is denied; it lives in its own module with those lints allowed.
- `viewport-*` properties of `ListView` and `Flickable` are deprecated in 1.18 in favour of `content-*`.
- The default testing backend uses fixed font metrics and cannot snapshot; `TestingBackendOptions { renderer_name: Some("software") }` gives real metrics and snapshots.
- Two languages: layout and styling in `.slint`, logic in Rust. The markup is compiled at build time and type-checked against the Rust callbacks.

## Not done

Same as the GPUI prototype: links and selection in HTML, attachments, dark theme, daemon events, a screen reader, an input method, macOS on screen.
