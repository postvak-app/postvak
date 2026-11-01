//! Opening the window at login (`postvak gui --background`): a launchd agent on macOS, an XDG autostart entry on Linux,
//! and in a Flatpak the desktop's Background portal, since the sandbox cannot write the host's autostart folder. The
//! only code that turns it on or off.
#[cfg(target_os = "linux")]
mod portal;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::paths::{self, APP_ID, Paths};

/// The `postvak` arguments a login start runs.
pub const GUI_ARGS: [&str; 2] = ["gui", "--background"];

/// Apart from the daemon's own agent (`daemon::service`), which syncs without a window.
const LAUNCHD_LABEL: &str = "io.github.postvak_app.postvak.gui";

#[derive(Clone, Copy, Debug, PartialEq)]
enum Target {
    Launchd,
    Xdg,
    Portal,
}

impl Target {
    fn current() -> Result<Target> {
        if paths::flatpak_id().is_some() {
            Ok(Target::Portal)
        } else if cfg!(target_os = "macos") {
            Ok(Target::Launchd)
        } else if cfg!(target_os = "linux") {
            Ok(Target::Xdg)
        } else {
            bail!("opening at login works on macOS and Linux")
        }
    }

    /// The file that says it is on. For the portal, which keeps the host's file out of the sandbox's sight, it is
    /// Postvak's own note of what the desktop granted.
    fn file(self, paths: &Paths) -> Result<PathBuf> {
        let dirs = directories::BaseDirs::new().context("no home directory found")?;
        Ok(match self {
            Target::Launchd => dirs
                .home_dir()
                .join("Library/LaunchAgents")
                .join(format!("{LAUNCHD_LABEL}.plist")),
            Target::Xdg => xdg_file(dirs.config_dir()),
            Target::Portal => paths.state_dir.join("open-at-login"),
        })
    }
}

fn xdg_file(config_dir: &Path) -> PathBuf {
    config_dir
        .join("autostart")
        .join(format!("{APP_ID}.desktop"))
}

/// Whether the window opens at login.
pub fn is_on(paths: &Paths) -> Result<bool> {
    let target = Target::current()?;
    let file = target.file(paths)?;
    let text =
        paths::read_or_empty(&file).with_context(|| format!("reading {}", file.display()))?;
    Ok(match target {
        Target::Xdg => xdg_enabled(&text),
        Target::Launchd | Target::Portal => !text.is_empty(),
    })
}

/// What [`set`] did, and what keeps it from working in this session, if anything.
#[derive(Debug, Default)]
pub struct Changed {
    /// What it did, to print.
    pub done: String,
    /// Why the login start it turned on may not happen here, and what to do instead.
    pub warning: Option<String>,
}

/// Turns opening at login on or off. In a Flatpak the desktop may ask the user first.
pub fn set(paths: &Paths, on: bool) -> Result<Changed> {
    let target = Target::current()?;
    let file = target.file(paths)?;
    let done = set_in(target, &file, paths, on)?;
    let warning = match (target, on) {
        (Target::Xdg, true) => xdg_warning(&file, paths),
        _ => None,
    };
    Ok(Changed { done, warning })
}

/// Why an autostart entry that is on may not open the window at login in this session: a compositor or window manager
/// that starts nothing from the autostart folder itself, where no session manager does it either.
pub fn warning(paths: &Paths) -> Result<Option<String>> {
    let target = Target::current()?;
    Ok(match target {
        Target::Xdg if is_on(paths)? => xdg_warning(&target.file(paths)?, paths),
        _ => None,
    })
}

fn xdg_warning(file: &Path, paths: &Paths) -> Option<String> {
    let compositor = compositor(|name| std::env::var(name).ok())?;
    if autostart_target_active() {
        return None;
    }
    let exe = paths::stable_exe().ok()?;
    let folder = file.parent().unwrap_or(file);
    Some(unread_entry(
        compositor,
        folder,
        &command(&exe, paths.home.as_deref()),
    ))
}

/// Compositors and window managers that start nothing from the autostart folder themselves, as `XDG_CURRENT_DESKTOP`
/// names them. Desktops such as Plasma, GNOME and Xfce do; a session manager such as uwsm can do it for these.
const NO_XDG_AUTOSTART: [&str; 6] = ["Hyprland", "sway", "niri", "river", "labwc", "i3"];

/// The compositor of this session when it is one of [`NO_XDG_AUTOSTART`]: from `XDG_CURRENT_DESKTOP`, which may list
/// several names, or from the socket variable a compositor sets for its clients when a display manager set no name.
fn compositor(env: impl Fn(&str) -> Option<String>) -> Option<&'static str> {
    let named = env("XDG_CURRENT_DESKTOP").and_then(|desktops| {
        desktops.split(':').find_map(|desktop| {
            NO_XDG_AUTOSTART
                .into_iter()
                .find(|known| known.eq_ignore_ascii_case(desktop.trim()))
        })
    });
    named.or_else(|| {
        [
            ("HYPRLAND_INSTANCE_SIGNATURE", "Hyprland"),
            ("SWAYSOCK", "sway"),
            ("NIRI_SOCKET", "niri"),
        ]
        .into_iter()
        .find(|(variable, _)| env(variable).is_some_and(|value| !value.is_empty()))
        .map(|(_, compositor)| compositor)
    })
}

/// Whether systemd started the autostart folder's entries in this session (`systemd-xdg-autostart-generator`), as
/// uwsm and some other session managers have it do.
fn autostart_target_active() -> bool {
    std::process::Command::new("systemctl")
        .args([
            "--user",
            "is-active",
            "--quiet",
            "xdg-desktop-autostart.target",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Says that `compositor` does not read the autostart `folder`, and how to start `command` at login there instead.
fn unread_entry(compositor: &str, folder: &Path, command: &[String]) -> String {
    format!(
        "{compositor} does not start the entries in {} unless a session manager such as uwsm does; to open Postvak at \
         login there, {}",
        folder.display(),
        startup_hint(Some(compositor), command)
    )
}

/// Where to put `command` so that `compositor` runs it at login, in that compositor's own syntax.
fn startup_hint(compositor: Option<&str>, command: &[String]) -> String {
    let line = shell_line(command);
    match compositor {
        Some("Hyprland") => {
            format!("add `exec-once = {line}` to ~/.config/hypr/hyprland.conf")
        }
        Some("sway") => format!("add `exec {line}` to ~/.config/sway/config"),
        Some("i3") => format!("add `exec {line}` to ~/.config/i3/config"),
        Some("niri") => {
            let args: Vec<String> = command.iter().map(|arg| kdl_string(arg)).collect();
            format!(
                "add `spawn-at-startup {}` to ~/.config/niri/config.kdl",
                args.join(" ")
            )
        }
        Some("labwc") => format!("add `{line} &` to ~/.config/labwc/autostart"),
        Some("river") => format!(
            "add `riverctl spawn {}` to ~/.config/river/init",
            shell_arg(&line)
        ),
        _ => format!("add `{line}` to your desktop's startup commands"),
    }
}

/// `command` as one line for `sh`, each argument quoted when it needs to be.
fn shell_line(command: &[String]) -> String {
    let args: Vec<String> = command.iter().map(|arg| shell_arg(arg)).collect();
    args.join(" ")
}

fn shell_arg(arg: &str) -> String {
    let safe = |c: char| c.is_ascii_alphanumeric() || "_-./:=@%+,".contains(c);
    if !arg.is_empty() && arg.chars().all(safe) {
        arg.to_string()
    } else {
        format!("'{}'", arg.replace('\'', "'\\''"))
    }
}

fn kdl_string(arg: &str) -> String {
    format!("\"{}\"", arg.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The command a Flatpak's login start runs, for a desktop without the Background portal.
#[cfg(any(target_os = "linux", test))]
fn flatpak_command() -> Vec<String> {
    ["flatpak", "run", APP_ID]
        .into_iter()
        .chain(GUI_ARGS)
        .map(String::from)
        .collect()
}

/// Says that the desktop could not be asked to start a Flatpak at login, and how to do it without the portal.
#[cfg(any(target_os = "linux", test))]
fn no_background_portal(compositor: Option<&str>) -> String {
    let hint = startup_hint(compositor, &flatpak_command());
    format!(
        "the desktop has no Background portal to open Postvak at login (the portals of Hyprland and other compositors \
         lack it); {hint}"
    )
}

fn set_in(target: Target, file: &Path, paths: &Paths, on: bool) -> Result<String> {
    match (target, on) {
        (Target::Portal, _) => {
            #[cfg(target_os = "linux")]
            {
                let granted = portal::request(on).map_err(|e| match on {
                    true => {
                        let compositor = compositor(|name| std::env::var(name).ok());
                        e.context(no_background_portal(compositor))
                    }
                    false => e,
                })?;
                if on && !granted {
                    bail!("the desktop did not allow Postvak to open at login");
                }
                if on {
                    paths::write_atomic(file, b"on\n")?;
                } else {
                    remove(file)?;
                }
                Ok(if on {
                    "the desktop opens Postvak at login".to_string()
                } else {
                    "the desktop no longer opens Postvak at login".to_string()
                })
            }
            #[cfg(not(target_os = "linux"))]
            bail!("a Flatpak runs on Linux only")
        }
        (_, true) => {
            let exe = paths::stable_exe().context("finding this postvak")?;
            let home = paths.home.as_deref();
            let text = match target {
                Target::Launchd => plist(&exe, home),
                _ => desktop_entry(&exe, home),
            };
            paths::write_atomic_keeping_dir_mode(file, text.as_bytes())
                .with_context(|| format!("writing {}", file.display()))?;
            Ok(format!("wrote {}", file.display()))
        }
        (_, false) => {
            remove(file)?;
            Ok(format!("deleted {}", file.display()))
        }
    }
}

fn remove(file: &Path) -> Result<()> {
    match std::fs::remove_file(file) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(e).with_context(|| format!("deleting {}", file.display()))
        }
        _ => Ok(()),
    }
}

/// An autostart entry is on unless the desktop's settings turned it off in the file (`Hidden`, or GNOME's own key).
fn xdg_enabled(text: &str) -> bool {
    !text.is_empty()
        && !text.lines().any(|line| {
            matches!(
                line.trim(),
                "Hidden=true" | "X-GNOME-Autostart-enabled=false"
            )
        })
}

fn plist(exe: &Path, postvak_home: Option<&Path>) -> String {
    let escape = crate::notify::escape_markup;
    let arguments: String = std::iter::once(exe.to_string_lossy().into_owned())
        .chain(GUI_ARGS.map(String::from))
        .map(|arg| format!("        <string>{}</string>\n", escape(&arg)))
        .collect();
    let environment = postvak_home.map_or_else(String::new, |home| {
        format!(
            "    <key>EnvironmentVariables</key>\n    <dict>\n        <key>POSTVAK_HOME</key>\n        <string>{}</string>\n    </dict>\n",
            escape(&home.to_string_lossy())
        )
    });
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LAUNCHD_LABEL}</string>
    <key>ProgramArguments</key>
    <array>
{arguments}    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>LimitLoadToSessionType</key>
    <string>Aqua</string>
    <key>ProcessType</key>
    <string>Interactive</string>
{environment}</dict>
</plist>
"#
    )
}

/// The command a login start runs on Linux: this postvak in the background, keeping a `POSTVAK_HOME`.
fn command(exe: &Path, postvak_home: Option<&Path>) -> Vec<String> {
    let mut command: Vec<String> = Vec::new();
    if let Some(home) = postvak_home {
        command.push("env".into());
        command.push(format!("POSTVAK_HOME={}", home.to_string_lossy()));
    }
    command.push(exe.to_string_lossy().into_owned());
    command.extend(GUI_ARGS.map(String::from));
    command
}

fn desktop_entry(exe: &Path, postvak_home: Option<&Path>) -> String {
    let exec: Vec<String> = command(exe, postvak_home)
        .iter()
        .map(|arg| exec_arg(arg))
        .collect();
    format!(
        "[Desktop Entry]\nType=Application\nName=Postvak\nComment=Opens Postvak in the tray at login\nExec={}\nIcon={APP_ID}\nTerminal=false\nX-GNOME-Autostart-enabled=true\n",
        exec.join(" ")
    )
}

/// One argument of a desktop entry's `Exec`, quoted as the Desktop Entry Specification says: in double quotes when it
/// holds a reserved character, with `"`, `` ` ``, `$` and `\` escaped there, and `\` escaped once more because the key's
/// value is itself an escaped string; `%` doubled, since a single one starts a field code.
fn exec_arg(arg: &str) -> String {
    const RESERVED: &str = " \t\n\"'\\><~|&;$*?#()`";
    let arg = arg.replace('%', "%%");
    if !arg.is_empty() && !arg.chars().any(|c| RESERVED.contains(c)) {
        return arg;
    }
    let mut quoted = String::from('"');
    for c in arg.chars() {
        match c {
            '"' | '`' | '$' => {
                quoted.push_str("\\\\");
                quoted.push(c);
            }
            '\\' => quoted.push_str("\\\\\\\\"),
            _ => quoted.push(c),
        }
    }
    quoted.push('"');
    quoted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_launchd_agent_opens_the_window_in_the_background_at_login() {
        let text = plist(
            Path::new("/Applications/Postvak.app/Contents/MacOS/postvak"),
            None,
        );
        assert!(text.contains("<string>io.github.postvak_app.postvak.gui</string>"));
        assert!(text.contains(
            "<string>/Applications/Postvak.app/Contents/MacOS/postvak</string>\n        <string>gui</string>\n        <string>--background</string>"
        ));
        assert!(text.contains("<key>RunAtLoad</key>\n    <true/>"));
        // A window that quits stays quit until the next login.
        assert!(!text.contains("KeepAlive"));
        assert!(!text.contains("EnvironmentVariables"));
    }

    #[test]
    fn the_launchd_agent_keeps_a_postvak_home_and_escapes_xml() {
        let text = plist(
            Path::new("/opt/a&b/postvak"),
            Some(Path::new("/tmp/<home>")),
        );
        assert!(text.contains("<string>/opt/a&amp;b/postvak</string>"));
        assert!(
            text.contains("<key>POSTVAK_HOME</key>\n        <string>/tmp/&lt;home&gt;</string>")
        );
    }

    #[test]
    fn the_autostart_entry_runs_this_postvak_in_the_background() {
        let text = desktop_entry(Path::new("/usr/bin/postvak"), None);
        assert!(text.starts_with("[Desktop Entry]\n"));
        assert!(text.contains("\nExec=/usr/bin/postvak gui --background\n"));
        assert!(text.contains("\nIcon=io.github.postvak_app.postvak\n"));
        assert!(xdg_enabled(&text));
    }

    #[test]
    fn the_autostart_entry_keeps_a_postvak_home() {
        let text = desktop_entry(Path::new("/usr/bin/postvak"), Some(Path::new("/srv/mail")));
        assert!(
            text.contains("\nExec=env POSTVAK_HOME=/srv/mail /usr/bin/postvak gui --background\n")
        );
    }

    #[test]
    fn exec_arguments_are_quoted_as_the_desktop_entry_specification_says() {
        assert_eq!(exec_arg("/usr/bin/postvak"), "/usr/bin/postvak");
        assert_eq!(
            exec_arg("/home/me/My Apps/Postvak.AppImage"),
            "\"/home/me/My Apps/Postvak.AppImage\""
        );
        assert_eq!(exec_arg("100%"), "100%%");
        assert_eq!(exec_arg("a$b"), "\"a\\\\$b\"");
        assert_eq!(exec_arg("a\\b"), "\"a\\\\\\\\b\"");
        assert_eq!(exec_arg(""), "\"\"");
    }

    #[test]
    fn an_entry_the_desktop_turned_off_counts_as_off() {
        assert!(!xdg_enabled(""));
        assert!(!xdg_enabled("[Desktop Entry]\nHidden=true\n"));
        assert!(!xdg_enabled(
            "[Desktop Entry]\nX-GNOME-Autostart-enabled=false\n"
        ));
        assert!(xdg_enabled(
            "[Desktop Entry]\nExec=postvak gui --background\n"
        ));
    }

    fn env<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            vars.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.to_string())
        }
    }

    #[test]
    fn compositors_that_skip_the_autostart_folder_are_known_by_name_or_socket() {
        assert_eq!(
            compositor(env(&[("XDG_CURRENT_DESKTOP", "Hyprland")])),
            Some("Hyprland")
        );
        assert_eq!(
            compositor(env(&[("XDG_CURRENT_DESKTOP", "sway:wlroots")])),
            Some("sway")
        );
        assert_eq!(
            compositor(env(&[("XDG_CURRENT_DESKTOP", "NIRI")])),
            Some("niri")
        );
        assert_eq!(
            compositor(env(&[("HYPRLAND_INSTANCE_SIGNATURE", "abc")])),
            Some("Hyprland")
        );
        assert_eq!(
            compositor(env(&[("SWAYSOCK", "/run/user/1000/sway.sock")])),
            Some("sway")
        );
        // Desktops that start the folder's entries themselves.
        for desktop in ["KDE", "GNOME", "ubuntu:GNOME", "XFCE", "X-Cinnamon"] {
            assert_eq!(
                compositor(env(&[("XDG_CURRENT_DESKTOP", desktop)])),
                None,
                "{desktop}"
            );
        }
        assert_eq!(compositor(env(&[("SWAYSOCK", "")])), None);
        assert_eq!(compositor(env(&[])), None);
    }

    #[test]
    fn the_warning_says_where_hyprland_starts_postvak_instead() {
        let command = command(Path::new("/usr/bin/postvak"), None);
        let text = unread_entry(
            "Hyprland",
            Path::new("/home/me/.config/autostart"),
            &command,
        );
        assert_eq!(
            text,
            "Hyprland does not start the entries in /home/me/.config/autostart unless a session manager such as uwsm \
             does; to open Postvak at login there, add `exec-once = /usr/bin/postvak gui --background` to \
             ~/.config/hypr/hyprland.conf"
        );
    }

    #[test]
    fn each_compositor_gets_the_startup_line_in_its_own_syntax() {
        let command = command(
            Path::new("/home/me/My Apps/Postvak.AppImage"),
            Some(Path::new("/srv/mail")),
        );
        assert_eq!(
            startup_hint(Some("sway"), &command),
            "add `exec env POSTVAK_HOME=/srv/mail '/home/me/My Apps/Postvak.AppImage' gui --background` to \
             ~/.config/sway/config"
        );
        assert_eq!(
            startup_hint(Some("niri"), &command),
            "add `spawn-at-startup \"env\" \"POSTVAK_HOME=/srv/mail\" \"/home/me/My Apps/Postvak.AppImage\" \"gui\" \
             \"--background\"` to ~/.config/niri/config.kdl"
        );
        assert_eq!(
            startup_hint(Some("river"), &command[2..]),
            "add `riverctl spawn ''\\''/home/me/My Apps/Postvak.AppImage'\\'' gui --background'` to \
             ~/.config/river/init"
        );
        assert_eq!(
            startup_hint(None, &flatpak_command()),
            "add `flatpak run io.github.postvak_app.postvak gui --background` to your desktop's startup commands"
        );
    }

    #[test]
    fn shell_and_kdl_arguments_are_quoted() {
        assert_eq!(shell_arg("/usr/bin/postvak"), "/usr/bin/postvak");
        assert_eq!(shell_arg("it's"), "'it'\\''s'");
        assert_eq!(shell_arg(""), "''");
        assert_eq!(kdl_string("a\"b\\c"), "\"a\\\"b\\\\c\"");
    }

    #[test]
    fn a_flatpak_without_the_background_portal_is_told_what_to_add() {
        let text = no_background_portal(Some("Hyprland"));
        assert!(
            text.starts_with("the desktop has no Background portal"),
            "{text}"
        );
        assert!(
            text.ends_with(
                "add `exec-once = flatpak run io.github.postvak_app.postvak gui --background` to \
                 ~/.config/hypr/hyprland.conf"
            ),
            "{text}"
        );
    }

    #[test]
    fn the_autostart_entry_is_named_after_the_app() {
        assert_eq!(
            xdg_file(Path::new("/home/me/.config")),
            Path::new("/home/me/.config/autostart/io.github.postvak_app.postvak.desktop")
        );
    }
}
