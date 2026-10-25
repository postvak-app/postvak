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

/// Turns opening at login on or off; what it did, to print. In a Flatpak the desktop may ask the user first.
pub fn set(paths: &Paths, on: bool) -> Result<String> {
    let target = Target::current()?;
    let file = target.file(paths)?;
    match (target, on) {
        (Target::Portal, _) => {
            #[cfg(target_os = "linux")]
            {
                let granted = portal::request(on)?;
                if on && !granted {
                    bail!("the desktop did not allow Postvak to open at login");
                }
                if on {
                    paths::write_atomic(&file, b"on\n")?;
                } else {
                    remove(&file)?;
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
            paths::write_atomic_keeping_dir_mode(&file, text.as_bytes())
                .with_context(|| format!("writing {}", file.display()))?;
            Ok(format!("wrote {}", file.display()))
        }
        (_, false) => {
            remove(&file)?;
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

fn desktop_entry(exe: &Path, postvak_home: Option<&Path>) -> String {
    let mut command: Vec<String> = Vec::new();
    if let Some(home) = postvak_home {
        command.push("env".into());
        command.push(format!("POSTVAK_HOME={}", home.to_string_lossy()));
    }
    command.push(exe.to_string_lossy().into_owned());
    command.extend(GUI_ARGS.map(String::from));
    let exec: Vec<String> = command.iter().map(|arg| exec_arg(arg)).collect();
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

    #[test]
    fn the_autostart_entry_is_named_after_the_app() {
        assert_eq!(
            xdg_file(Path::new("/home/me/.config")),
            Path::new("/home/me/.config/autostart/io.github.postvak_app.postvak.desktop")
        );
    }
}
