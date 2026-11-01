# Install

| Channel | Command |
|---|---|
| Homebrew | `brew install postvak-app/tap/postvak` |
| macOS app (Homebrew cask) | `brew install --cask postvak-app/tap/postvak`, see [below](#macos-app) |
| crates.io | `cargo install postvak` |
| mise | `mise use github:postvak-app/postvak` |
| AppImage (Linux) | download `postvak-<arch>.AppImage` from the [latest release](https://github.com/postvak-app/postvak/releases/latest), see [below](#appimage-on-linux) |
| Flatpak (Linux) | `flatpak install --user postvak io.github.postvak_app.postvak` after adding the remote, see [below](#flatpak-on-linux) |
| Docker (headless) | `ghcr.io/postvak-app/postvak`, see [Docker](daemon.md#docker) |

Each [GitHub release](https://github.com/postvak-app/postvak/releases) also has plain binaries for macOS and Linux on x86_64 and aarch64.

To build from source:

```sh
git clone https://github.com/postvak-app/postvak
cd postvak
mise install rust         # the pinned Rust toolchain; rustup works too
cargo install --path .
```

On Linux the password keyring is the Secret Service (KDE Wallet or GNOME Keyring), reached over D-Bus. KDE Wallet offers the Secret Service from KDE Frameworks 5.97 on; with an older Plasma, use a password command instead (see [Accounts](accounts.md)). On macOS it is the login Keychain; a new, unsigned binary (every upgrade) asks again for Keychain access, so choose "Always Allow".

## macOS app

Postvak also ships as `Postvak.app` on a DMG, attached to each [GitHub release](https://github.com/postvak-app/postvak/releases). Homebrew installs it as a cask:

```sh
brew install --cask postvak-app/tap/postvak
```

The cask puts `Postvak.app` in `/Applications` and links the app's own binary as the `postvak` command, so the CLI and the MCP server come with it. Opening the app from Finder or the Dock opens the mail window; `postvak` in a terminal works as usual.

The app is not notarized by Apple yet; it is signed ad hoc. The cask clears the quarantine flag on install, so it opens normally. A DMG downloaded by hand is blocked by Gatekeeper the first time: clear the flag once with `xattr -dr com.apple.quarantine /Applications/Postvak.app`, or allow it under System Settings → Privacy & Security. Each upgrade has a new signature, so the Keychain asks again for access to the password.

Install either the cask or the formula (`brew install postvak-app/tap/postvak`), not both: the window, the CLI and the MCP server share one daemon over one socket, and an older client refuses a newer daemon, so two installs at different versions would get in each other's way. Homebrew refuses the second one. The formula stays for Linux and headless Macs.

## AppImage on Linux

Each release has an AppImage for x86_64 and aarch64. It runs without installing on Linux desktops (KDE Plasma, GNOME and others) that have the system's graphics libraries and FUSE (`fusermount`), as most do:

```sh
chmod +x postvak-x86_64.AppImage
./postvak-x86_64.AppImage                  # double-clicked or run without a terminal it opens the window
./postvak-x86_64.AppImage account add      # with arguments it is the postvak command
./postvak-x86_64.AppImage mcp install      # MCP hosts and `service install` record this file's path
```

Keep the file where you run it from: the daemon it starts, `service install` and `mcp install` all point at it, so after moving it run those two again. To put it in your app launcher, use an AppImage manager such as Gear Lever or AppImageLauncher.

## Flatpak on Linux

Postvak has its own Flatpak repository, so it installs as a desktop app and `flatpak update` (or your software centre) brings each new release. The Freedesktop runtime it needs comes from Flathub:

```sh
flatpak remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo
flatpak remote-add --user --if-not-exists postvak https://postvak.pataar.nl/flatpak/postvak.flatpakrepo
flatpak install --user postvak io.github.postvak_app.postvak
```

Then open Postvak from your app launcher, or from a terminal:

```sh
flatpak run io.github.postvak_app.postvak                # the mail window
flatpak run io.github.postvak_app.postvak account add    # with arguments it is the postvak command
```

An alias such as `alias postvak='flatpak run io.github.postvak_app.postvak'` saves typing. Each release also attaches the same app as a single file, `postvak-<arch>.flatpak`, for `flatpak install --user postvak-x86_64.flatpak`; that one does not update itself.

The sandbox may use the network, the display, the password keyring (Secret Service), desktop notifications, the tray and your Downloads folder, where saved attachments go, and nothing else. It keeps its own config and state in `~/.var/app/io.github.postvak_app.postvak/`, so a Flatpak install does not share accounts, rules or the daemon with a `postvak` installed another way. A `password = { command = "..." }` runs inside the sandbox too, where tools such as `pass` or `op` are missing, so use the keyring there. The daemon that the window, the CLI or an agent host starts runs in a sandbox of its own, so quitting the window ends the window's sandbox; the daemon stops a minute after its last client.

Two commands work differently inside the sandbox, because it cannot write files for programs outside it:

- `mcp install json` prints an entry that runs `flatpak run io.github.postvak_app.postvak mcp`; add it to your MCP host yourself. `mcp install claude-code`, `codex`, `copilot` and `gemini` print the commands to run, and `mcp install claude-desktop`, `cursor` and `opencode` print the entry to add to that host's file.
- `service install` prints a systemd user unit that runs `flatpak run io.github.postvak_app.postvak run`, and the commands to enable it; `service remove` prints the commands to undo that.

Opening at login (File ▸ Open at login, or `autostart on`) works in the sandbox too: Postvak asks the desktop through its Background portal, which writes the host's autostart entry and may ask you first. Only the portals of Plasma and GNOME offer it; elsewhere, such as on Hyprland, Postvak says so and which line to add to your compositor's startup instead (see [below](#hyprland-sway-and-other-compositors)).

## Desktop entry on Linux

Apart from the macOS app, the AppImage and the Flatpak, the channels above install only the binary. To get Postvak into your app launcher with its icon, run this from a checkout:

```sh
packaging/linux/install.sh                                # into ~/.local/share, for you
sudo packaging/linux/install.sh --prefix /usr/local/share # for everyone
packaging/linux/install.sh --uninstall                    # add the same --prefix if you used one
```

The entry runs `postvak gui`, so `postvak` must be on your `PATH`. The window's Wayland app id, `io.github.postvak_app.postvak`, matches the entry, so docks and task switchers show the right name and icon.

## Hyprland, Sway and other compositors

Postvak reaches the desktop only through freedesktop standards, so it runs on a Wayland compositor such as Hyprland (and Omarchy, which is built on it), Sway, niri, river or labwc, and on an X11 window manager such as i3, as well as on Plasma and GNOME. A compositor brings less of a desktop with it, so what works depends on what else you run:

- **Tray:** a bar with a tray, such as Waybar's `tray` module, shows Postvak's icon, and closing the window keeps Postvak there. Without one, closing the window quits.
- **Notifications:** any notification daemon, such as mako, dunst or SwayNotificationCenter, shows new mail. A click opens the message where the daemon runs a notification's default action on click: mako does, dunst only with `mouse_left_click = do_action`.
- **Keyring:** the password keyring needs a Secret Service, such as GNOME Keyring or KeePassXC with its Secret Service on. Without one, use a password command (see [Accounts](accounts.md)).
- **Theme, reduced motion and Save as…:** these come from the Settings and FileChooser portals, which `xdg-desktop-portal-gtk` provides; the compositor's own portal (`xdg-desktop-portal-hyprland`, `-wlr`) lacks them. Without the Settings portal the System theme may not follow a dark desktop, so pick Light or Dark under View ▸ Theme; without the FileChooser portal Save as… fails, and the save button still saves to Downloads.
- **Unread count:** there is no dock entry to show it on, so it shows in the tray icon's tooltip only.
- **Window rules:** the window's Wayland app id (its class in Hyprland) is `io.github.postvak_app.postvak`.

**Opening at login** (File ▸ Open at login, or `postvak autostart on`) writes an entry in `~/.config/autostart`, which these compositors do not read themselves. A session manager that has systemd start those entries, such as uwsm, does; Omarchy starts Hyprland that way, so the entry works there. Without one, Postvak keeps the entry but says it may not open and which line to add instead, as `postvak autostart status` does too. On Hyprland that is, in `~/.config/hypr/hyprland.conf`:

```
exec-once = postvak gui --background
```

Sway and i3 take `exec postvak gui --background` in their config, niri `spawn-at-startup "postvak" "gui" "--background"` in `config.kdl`. Postvak's own line names the full path of the binary or AppImage it runs from. In the Flatpak, the command is `flatpak run io.github.postvak_app.postvak gui --background`, as these compositors' portals have no Background portal for Postvak to ask.
