# Linux desktop integration — design

Status: implemented, 2026-10-19. Follows a scan of Wayland, GNOME and Plasma behaviour.

## Goal

New-mail notifications that look and sound like the desktop's own, a click on one that opens the message, and one window: a second start from the dock, the launcher or a notification brings back the window that is open instead of opening another.

## Surfaces

GUI and daemon only. This is the named exception to the surface parity rule: the CLI and MCP have no window to bring to the front or open a message in. `postvak gui` gains `--open <uid> [--account <name>] [--folder <folder>]`, which is how a notification click reaches the window. Nothing changes in `help`, `output` or the daemon wire protocol.

## Decisions

- **Notifications on Linux go straight to D-Bus** (`org.freedesktop.Notifications`) through `zbus`, on one connection the daemon keeps. A click arrives as `ActivationToken`, then `ActionInvoked`, and Plasma sends those only to the connection that sent the notification, so the connection that sends must also listen. `notify-rust` opens a connection per notification and blocks a thread on each one to hear its click, so it stays for macOS only.
- **Title:** the sender's name, or the address without one, as plain text. The specification allows markup only in the body; the old title was the escaped From and showed `&lt;` on GNOME and Plasma.
- **Body:** the subject, escaped only when the server reports `body-markup`.
- **Hints:** `desktop-entry` (the app id, so the desktop shows Postvak's icon and lists it in its per-app notification settings), `category = email.arrived`, and `sound-name = message-new-email`. The server plays the sound from the sound theme, so Do Not Disturb and the volume apply, and there is no setting or audio library in Postvak. GNOME Shell plays the hint; whether Plasma does needs checking on a real desktop. On Plasma the user can set a sound for Postvak in its notification settings.
- **Click:** the `default` action. The daemon remembers the latest 64 notifications it sent and accepts signals only from the server's unique bus name. A click runs `postvak gui --open …` with the server's token in `XDG_ACTIVATION_TOKEN` and `DESKTOP_STARTUP_ID`, and an inherited token is removed when the server gave none. On macOS a click still does nothing.
- **One window:** `gui.lock` and `gui.sock` in the state directory. The first start holds the lock and listens once its mail window is live. A later start sends `{"open": …}` on the socket, waits for `ok` and exits. If the window holding the lock does not answer within 3 s, for example because it is still showing the startup window, the later start opens its own window, as before. The software-renderer retry releases the lock first, because the retry becomes the first window.
- **Activation token:** `postvak gui` takes `XDG_ACTIVATION_TOKEN` or `DESKTOP_STARTUP_ID` out of its environment before any thread starts, so the daemon and browser it starts do not inherit it, and gives it to its first window through Slint's attributes hook. The window can then come to the front, and the launcher stops showing that the app is starting.
- **Raising an open window on Wayland is best effort.** Wayland gives an app no way to restore, focus or raise its own window. Slint applies the attributes hook only when it first creates a window, not when it recreates one after a hide, so a token handed over by a later start cannot reach the window either. A window that was hidden comes back as a new one, which compositors usually put in front. An open one asks for attention, which Plasma shows on its task bar entry and GNOME shows as "Postvak is ready". If Slint applied the hook on every recreation, the handed-over token could do better.
- **Bursts:** the daemon holds each account's `NewMail` until that pass's `Synced`. Up to three messages notify one by one; more become one "N new messages" notification naming up to three senders, newest first, which opens the newest message. On Plasma, `x-kde-origin-name` shows the account.
- **Unread count on the task bar entry:** the window sends the All inboxes unread count with the `com.canonical.Unity.LauncherEntry` `Update` signal (Plasma's task manager, Ubuntu's dock, Dash to Dock), only when it changes, and clears it when the window closes. With the window closed the count is gone, since only the window knows it. No `urgent` flag: it needs the window's focus, which winit does not report on every platform. Whether the Flatpak's D-Bus proxy lets this broadcast signal through needs checking on a real desktop.
- **Desktop entry:** `SingleMainWindow=true` now that there is one window, and `X-GNOME-UsesNotifications=true`, so GNOME lists Postvak in its notification settings before the first notification.
- **Opening the message:** its folder, with the search cleared. A filter that hides the message is reset to the default. The cursor goes to the row whose uid matches; in a thread, that is the newest message, which new mail is.

## Tests

- The notification request: title, escaping with and without `body-markup`, actions and hints.
- Clicks: a token followed by a click opens once with the token; signals from other bus names, and from closed notifications, open nothing; only the latest 64 are remembered.
- The command a click runs: arguments, and the token set or cleared.
- One window: a later start's request reaches the first window; the lock frees when the first window ends; a first window that does not answer leaves the later start alone; a stale socket is replaced.
- The token: the Wayland one is preferred, and an empty one counts as none.
- Bursts: a few messages notify one by one; more become one notification that names the senders and opens the newest; the daemon hands each account's mail over when its pass ends.
- The unread count: its properties, and on a private bus, that only a changed count is sent.
- `OpenMessage` shows the message's folder with the cursor on it, and resets a filter that hid it.

Not automated: a real notification server, compositor or desktop. Report the platform coverage as compiled and tested on Linux, not run on GNOME or Plasma.
