//! Starting where the graphics stack may be broken. Slint's winit backend makes the OpenGL context inside the event
//! loop and falls back by itself only on Windows, and a process gets one Slint platform, so the retry is a new process.
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::sync::atomic::{AtomicBool, Ordering};

use slint::winit_030::{EventResult, WinitWindowAccessor};

/// Failed starts in a row after which the next start is in safe mode.
const SAFE_MODE_AFTER: u32 = 2;

/// Whether a window of this process has shown.
static SHOWN: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Renderer {
    /// FemtoVG on OpenGL.
    Default,
    Software,
}

/// The software renderer in safe mode, or when `SLINT_BACKEND` (`software`, `sw`, `winit-software`) asks for it.
pub(crate) fn renderer(safe_mode: bool, slint_backend: Option<&str>) -> Renderer {
    if safe_mode || slint_backend.is_some_and(names_software) {
        Renderer::Software
    } else {
        Renderer::Default
    }
}

/// Slint reads `SLINT_BACKEND` as `backend-renderer` or a single name.
fn names_software(slint_backend: &str) -> bool {
    let value = slint_backend.to_lowercase();
    let renderer = value
        .split_once('-')
        .map_or(value.as_str(), |(_, renderer)| renderer);
    matches!(renderer, "software" | "sw")
}

pub(crate) fn safe_mode(asked: bool, failed_starts: u32) -> bool {
    asked || failed_starts >= SAFE_MODE_AFTER
}

/// Where a launcher hands an app its activation token: Wayland's variable, then X11's.
const ACTIVATION_TOKEN_VARS: [&str; 2] = ["XDG_ACTIVATION_TOKEN", "DESKTOP_STARTUP_ID"];

/// Takes the activation token the launcher gave this start out of the environment: `XDG_ACTIVATION_TOKEN` on Wayland,
/// `DESKTOP_STARTUP_ID` on X11. With it the first window may come to the front, and the launcher stops showing that
/// the app is starting; taken, the daemon and the browser this process starts do not inherit one that was meant for it
/// alone. Before any thread starts, since it changes the environment.
pub(crate) fn take_token() -> Option<String> {
    let token = token_from(|var| std::env::var(var).ok());
    #[cfg(target_os = "linux")]
    slint::winit_030::winit::platform::startup_notify::reset_activation_token_env();
    token
}

fn token_from(read: impl Fn(&str) -> Option<String>) -> Option<String> {
    ACTIVATION_TOKEN_VARS
        .into_iter()
        .find_map(|var| read(var).filter(|value| !value.is_empty()))
}

/// Makes this process draw with `renderer`, then names its windows `app_id`; before the first window, after which
/// Slint keeps the platform it has. Slint keeps the app id on the platform and drops it when there is none yet, so
/// the platform comes first. The first window gets `token`.
pub(crate) fn select(renderer: Renderer, app_id: &str, token: Option<String>) {
    let selector = match renderer {
        Renderer::Default => slint::BackendSelector::new(),
        Renderer::Software => slint::BackendSelector::new()
            .backend_name("winit".into())
            .renderer_name("software".into()),
    };
    #[cfg(target_os = "linux")]
    let selector = {
        use slint::winit_030::winit::platform::startup_notify::WindowAttributesExtStartupNotify;
        use slint::winit_030::winit::window::ActivationToken;
        let token = std::cell::Cell::new(token.map(ActivationToken::from_raw));
        selector.with_winit_window_attributes_hook(move |attributes| match token.take() {
            Some(token) => attributes.with_activation_token(token),
            None => attributes,
        })
    };
    #[cfg(not(target_os = "linux"))]
    let _ = token;
    if let Err(e) = selector.select() {
        log::warn!("could not select the {renderer:?} renderer: {e}");
    }
    if let Err(e) = slint::set_xdg_app_id(app_id) {
        log::warn!("could not set the app id: {e}");
    }
}

/// `gui-starting` in the state directory: the number of starts in a row that have not shown a window, this one
/// included. Written before the window opens and removed once it shows, so a start that crashes leaves it behind.
#[derive(Clone)]
pub(crate) struct Marker(PathBuf);

impl Marker {
    pub(crate) fn new(state_dir: &Path) -> Marker {
        Marker(state_dir.join("gui-starting"))
    }

    pub(crate) fn failed_starts(&self) -> u32 {
        fs::read_to_string(&self.0)
            .ok()
            .and_then(|text| text.trim().parse().ok())
            .unwrap_or(0)
    }

    pub(crate) fn set(&self, starts: u32) {
        if let Err(e) = postvak_core::paths::write_atomic(&self.0, starts.to_string().as_bytes()) {
            log::warn!("could not write {}: {e}", self.0.display());
        }
    }

    fn clear(&self) {
        match fs::remove_file(&self.0) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => {
                log::warn!("could not remove {}: {e}", self.0.display());
            }
            _ => {}
        }
    }
}

/// Counts this start as one that showed a window once `window`'s native window has its first event.
pub(crate) fn watch(window: &slint::Window, marker: Option<Marker>) {
    // ponytail: the first event comes before the first frame, so a crash while drawing that frame is not counted.
    window.on_winit_window_event(move |_, _| {
        if !SHOWN.swap(true, Ordering::Relaxed)
            && let Some(marker) = &marker
        {
            marker.clear();
        }
        EventResult::Propagate
    });
}

/// True when `error` is Slint failing to make or show a window and none has shown yet.
pub(crate) fn no_window(error: &anyhow::Error) -> bool {
    !SHOWN.load(Ordering::Relaxed)
        && error
            .chain()
            .any(|cause| cause.is::<slint::PlatformError>())
}

/// Runs this program again with the same arguments, the software renderer and this start's token, and waits for it.
pub(crate) fn retry_in_software(token: Option<&str>) -> io::Result<ExitStatus> {
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args(std::env::args_os().skip(1))
        .env("SLINT_BACKEND", "winit-software");
    if let Some(token) = token {
        command.envs(ACTIVATION_TOKEN_VARS.map(|var| (var, token)));
    }
    command.status()
}

/// Says that no window could open: on stderr for a terminal, and in the daemon log for a launch from a desktop entry or
/// Finder, which has no terminal.
pub(crate) fn report(window_error: &anyhow::Error, daemon_log: Option<&Path>) {
    let message =
        format!("no window could open, not even with the software renderer: {window_error:#}");
    log::warn!("{message}");
    let Some(daemon_log) = daemon_log else { return };
    let line = format!(
        "[{} WARN postvak::gui] {message}\n",
        chrono::Utc::now().to_rfc3339()
    );
    let written = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(daemon_log)
        .and_then(|mut file| file.write_all(line.as_bytes()));
    if let Err(e) = written {
        log::warn!("could not write {}: {e}", daemon_log.display());
    }
}

#[cfg(test)]
mod tests {
    use super::{Marker, Renderer, renderer, safe_mode, token_from};

    #[test]
    fn the_wayland_token_comes_before_the_x11_one_and_an_empty_one_is_none() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |var: &str| {
                pairs
                    .iter()
                    .find(|(name, _)| *name == var)
                    .map(|(_, value)| value.to_string())
            }
        };
        let both = env(&[
            ("DESKTOP_STARTUP_ID", "x11"),
            ("XDG_ACTIVATION_TOKEN", "wl"),
        ]);
        assert_eq!(token_from(both).as_deref(), Some("wl"));
        let x11 = env(&[("XDG_ACTIVATION_TOKEN", ""), ("DESKTOP_STARTUP_ID", "x11")]);
        assert_eq!(token_from(x11).as_deref(), Some("x11"));
        assert_eq!(token_from(env(&[])), None);
    }

    #[test]
    fn slint_backend_names_the_software_renderer_in_any_of_its_spellings() {
        for value in ["software", "sw", "winit-software", "Winit-SW"] {
            assert_eq!(renderer(false, Some(value)), Renderer::Software, "{value}");
        }
        for value in ["", "winit", "winit-femtovg", "qt"] {
            assert_eq!(renderer(false, Some(value)), Renderer::Default, "{value}");
        }
        assert_eq!(renderer(false, None), Renderer::Default);
    }

    #[test]
    fn safe_mode_draws_in_software_whatever_slint_backend_says() {
        assert_eq!(renderer(true, None), Renderer::Software);
        assert_eq!(renderer(true, Some("winit-femtovg")), Renderer::Software);
    }

    #[test]
    fn two_failed_starts_in_a_row_bring_safe_mode() {
        assert!(!safe_mode(false, 0));
        assert!(!safe_mode(false, 1));
        assert!(safe_mode(false, 2));
        assert!(safe_mode(true, 0));
    }

    #[test]
    fn the_marker_counts_failed_starts_until_cleared() {
        let dir = tempfile::tempdir().unwrap();
        let marker = Marker::new(&dir.path().join("state"));
        assert_eq!(marker.failed_starts(), 0);
        marker.set(marker.failed_starts() + 1);
        marker.set(marker.failed_starts() + 1);
        assert_eq!(marker.failed_starts(), 2);
        marker.clear();
        assert_eq!(marker.failed_starts(), 0);
        marker.clear();
    }

    /// Regression: the app id was set before the platform existed, so Slint dropped it and Wayland showed a generic
    /// icon instead of the desktop entry's.
    #[test]
    fn slint_keeps_the_app_id_only_once_a_platform_exists() {
        assert!(slint::set_xdg_app_id(crate::APP_ID).is_err());
        crate::test_support::headless();
        assert!(slint::set_xdg_app_id(crate::APP_ID).is_ok());
    }
}
