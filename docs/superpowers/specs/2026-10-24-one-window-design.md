# One window — design

Status: implemented, 2026-10-24. The first part of #241, which was closed unmerged; notifications that open a message and the unread badge follow in their own PRs.

## Goal

One mail window per user. Starting Postvak again, from the launcher, the dock or a terminal, brings back the window that is already running, including one hidden in the tray, instead of opening a second window with a second tray icon. Autostart, which starts the window hidden in the tray, needs this first.

## Surfaces

GUI only. This is a named exception to the surface parity rule: the CLI and MCP have no window. `postvak gui` gains no flags.

## Decisions

- **Lock and socket:** `gui.lock` and `gui.sock` in the state directory, next to the daemon's. The first start holds the lock and listens once its mail window is live. A later start sends one JSON line (`{}` for now) and waits for `ok`, then exits. A window ignores fields it does not know, so the notification click (`gui --open`) can add one without breaking a running older window.
- **A first window that does not answer:** if the lock holder does not listen within 3 s, for example because it is still showing the startup window or the first-run form, the later start opens its own window, as before. A stuck window never blocks a new one.
- **Safe mode retry:** the software-renderer retry releases the lock first, because the retry becomes the first window.
- **Shared helpers:** `paths::try_lock_file` and `paths::bind_private` serve both the daemon and the window; the daemon's lock and socket behave as before.
- **Activation token:** `postvak gui` takes `XDG_ACTIVATION_TOKEN` or `DESKTOP_STARTUP_ID` out of its environment before any thread starts, so the daemon and browser it starts do not inherit it, and gives it to its first window through Slint's window attributes hook. The window can then come to the front, and the launcher stops showing that the app is starting. The software-renderer retry gets the same token.
- **Raising on Wayland is best effort.** Wayland gives an app no way to restore, focus or raise its own window, and Slint applies the attributes hook only when it first creates a window, so a later start's token cannot reach it. A window that was hidden comes back as a new one, which compositors usually put in front. An open one asks for attention, which Plasma shows on its task bar entry and GNOME as "Postvak is ready".
- **macOS:** a second `postvak gui` from a terminal hands over the same way. Opening Postvak.app again does not start a second process: Launch Services activates the running app and sends it a reopen event, which winit 0.30 does not pass on, so a window hidden in the menu bar stays hidden until its menu bar icon is used. Handling that reopen is part of the macOS integration PR.
- **Desktop entry:** `SingleMainWindow=true`, so GNOME and Plasma stop offering "New Window".

## Tests

- A later start's request reaches the first window; the lock frees when the first window ends; a first window that does not answer leaves the later start alone; a socket left by a window that died is replaced; a request with unknown fields is accepted.
- The token: the Wayland one is preferred, and an empty one counts as none.

Not automated: a real compositor or desktop. Report the platform coverage as compiled and tested on Linux, and compiled on macOS by CI; not run on Plasma, GNOME or macOS.
