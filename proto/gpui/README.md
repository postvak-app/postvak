# postvak-gpui-proto

Throwaway GPUI prototype of the Postvak mail window, Phase 1 of `docs/superpowers/plans/2026-10-13-gpui-migration.md`. It lives on branch `proto/gpui` and is never merged.

Its own workspace and lock file, so `postvak`'s build and CI never see GPUI. Every dependency comes from crates.io: `gpui-pre =0.3.9` (a snapshot of zed@93d7456) and `gpui-kit =0.7.3`, which pins the same snapshot.

## Run

```sh
cd proto/gpui
cargo run --release
```

Force X11 on a Wayland session with `WAYLAND_DISPLAY= cargo run --release`.

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
