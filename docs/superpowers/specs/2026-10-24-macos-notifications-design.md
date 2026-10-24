# macOS notifications and Dock badge — design

Status: implemented, 2026-10-24. The macOS side of `2026-10-19-linux-desktop-integration-design.md`, which the Platforms rule in `AGENTS.md` asks for. Reopening a hidden window from the Dock is not here: winit 0.30 has no hook for it, and it goes upstream first rather than into unsafe code in Postvak.

## Goal

New-mail notifications on macOS that show as Postvak, a click that opens the message as on Linux, and the unread count on the Dock icon.

## Surfaces

GUI and daemon only, the same exception as the Linux design: the CLI and MCP have no window.

## Decisions

- **`mac-notification-sys` directly**, replacing `notify-rust`, which Postvak used on macOS only. It posts through `NSUserNotificationCenter`, deprecated since macOS 11 but still delivered, and reports a click. The current API, `UNUserNotificationCenter`, needs a delegate class (unsafe in objc2) and a signed bundle, which the Homebrew formula's bare binary is not.
- **As Postvak:** `set_application("io.github.postvak_app.postvak")` once, so Notification Center shows Postvak's name and icon and lists it under System Settings › Notifications. It works when Launch Services knows the app, so after installing the DMG or the cask; with only the formula, macOS posts them under its default sender, as before.
- **Clicks:** each notification waits for its click on a thread of its own, and a click runs `postvak gui --open …`, which the window's one-window socket hands to a running window. The library delivers a click on the main thread's run loop, so while the daemon's main thread waits up to 200 ms for the next client, it runs that run loop in 20 ms slices and checks for a client between them (`CFRunLoopRunInMode`, safe in objc2-core-foundation); a client still connects within 20 ms. A run loop with nothing to wait on returns at once, and then the thread waits on clients alone, as on Linux.
- **Waiting threads are capped at 64.** Notification Center keeps a notification until the user clears it, and the library waits until then. Past 64, a notification still shows, and a click on it only brings Postvak to the front, which macOS does for the app it is posted as.
- **Dock badge:** `NSApplication.dockTile.badgeLabel`, set to the All inboxes unread count by the window on the main thread, cleared at 0 and when the window closes, like the Linux task bar count. All safe in objc2-app-kit 0.3.
- **No unsafe code in Postvak.** The objc2 crates are already in the tree through Slint's menus (muda).

## Tests

- Only 64 notifications wait for a click, and a slot frees when its notification is done.
- The Dock label is the count, and none at 0.
- The bursts and the `gui --open` command are shared with Linux and already tested.

Not automated: Notification Center, the Dock, or a click. Compiled and tested on Linux; the macOS code type-checked for `aarch64-apple-darwin` outside the build (the C build scripts need the macOS SDK), and CI builds and tests on macOS. Not run on a Mac.
