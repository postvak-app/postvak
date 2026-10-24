# Open at login — design

Status: implemented, 2026-10-24. Autostart under the Platforms rule in `AGENTS.md`.

## Goal

Postvak can start when the user logs in, out of the way, so notifications, the tray and the unread count are there without opening the window by hand.

## Surfaces

GUI (File ▸ Open at login, a checkbox) and CLI (`postvak autostart on|off|status`). MCP is the named exception: an agent should not change what runs at login.

## Decisions

- **What starts:** `postvak gui --background`. With a tray it starts hidden in the tray; without one (stock GNOME) the window shows, since a hidden window could not be reached. On macOS it starts minimized: winit 0.30's application delegate does not handle reopen, so AppKit's own handling brings a minimized window back when the Dock icon is clicked, which a hidden one would not get until winit 0.31 (see `2026-10-24-macos-notifications-design.md`). `--background` with `--open` is refused: a message to show wants the window in front.
- **A start that never shows:** the startup marker counts starts that never showed a window, for safe mode; a start that stays in the tray clears it itself, or two logins would turn on safe mode. It also keeps the saved window layout, since a window that never showed has none of its own.
- **macOS:** a launch agent, `~/Library/LaunchAgents/io.github.postvak_app.postvak.gui.plist`, next to the daemon's own agent and apart from it: `RunAtLoad`, no `KeepAlive` (a window that quits stays quit until the next login), `LimitLoadToSessionType = Aqua`. Writing the file is enough; launchd reads it at the next login. Postvak's binary path comes from `paths::stable_exe`, as the daemon's agent does. The SMAppService API was set aside: it needs objc2 bindings with unsafe calls and macOS 13.
- **Linux:** `$XDG_CONFIG_HOME/autostart/io.github.postvak_app.postvak.desktop`, named after the app's desktop entry, so desktops list it as Postvak. `Exec` is quoted as the Desktop Entry Specification says. An entry the desktop's settings turned off (`Hidden=true`, `X-GNOME-Autostart-enabled=false`) counts as off.
- **Flatpak:** the sandbox cannot write the host's autostart folder, so Postvak asks `org.freedesktop.portal.Background` (`RequestBackground` with `autostart` and the `commandline` `postvak gui --background`) over the session bus with zbus, which is already a dependency. The desktop may ask the user. The host's entry is out of the sandbox's sight, so Postvak notes in its state directory what the desktop granted; that note is the status.
- **`POSTVAK_HOME`** is carried into the agent's environment and the entry's `Exec` (through `env`), as `service install` does.
- **In the window:** read once at the first tick, and changed on a thread of its own, since the portal may wait for the user; the item is checked as soon as it is chosen and greyed out until the change is done, and goes back with an error when it fails.

## Tests

- The launch agent: label, arguments, `RunAtLoad`, no `KeepAlive`, `POSTVAK_HOME`, XML escaping.
- The autostart entry: `Exec`, its quoting (spaces, `%`, `$`, `\`), `POSTVAK_HOME`, its file name, and an entry the desktop turned off.
- The portal: the request path, and its answers (granted, not granted, cancelled, refused).
- How a start at login shows: hidden with a tray, shown without one, minimized on macOS, in front otherwise.
- The menu item: checked at once, greyed out while changing, and back with an error when the platform refuses.

Not automated: a real login, launchd, a desktop's autostart, or the portal. Compiled and tested on Linux; the macOS path is shared code plus a file write.
