//! Opening a file or URL with the desktop's default app. On Linux the window first asks the compositor (Wayland) or the
//! X server for an activation token and hands it to the opener: an app that is already running, such as the browser,
//! may only come to the front with one, and focus stealing prevention keeps it behind Postvak's window without.
use std::ffi::OsStr;
#[cfg(any(target_os = "linux", test))]
use std::ffi::OsString;
use std::process::Command;

use slint::ComponentHandle;
use slint::winit_030::winit::event::WindowEvent;

use crate::worker;

/// Opens `target` with the desktop's default app; `text` asks for the text editor where the platform can. On Linux
/// the app starts once the window's token arrives, so an error there is logged rather than returned.
pub(crate) fn open(target: &OsStr, text: bool) -> std::io::Result<()> {
    #[cfg(target_os = "linux")]
    if linux::defer(target, text) {
        return Ok(());
    }
    start(target, text, None)
}

/// Makes `component`'s window the one that asks for tokens; the window a click came from, since a compositor gives one
/// only to the window in front.
pub(crate) fn attach<C: ComponentHandle + 'static>(component: &C) {
    #[cfg(target_os = "linux")]
    linux::attach(component.as_weak());
    #[cfg(not(target_os = "linux"))]
    let _ = component;
}

/// Starts the opens waiting for the token `event` delivers; for every window's event filter.
pub(crate) fn on_event(event: &WindowEvent) {
    #[cfg(target_os = "linux")]
    if let WindowEvent::ActivationTokenDone { serial, token } = event {
        linux::token_done(*serial, token.clone().into_raw());
    }
    #[cfg(not(target_os = "linux"))]
    let _ = event;
}

fn start(target: &OsStr, text: bool, token: Option<&str>) -> std::io::Result<()> {
    let mut command = command(target, text, token);
    let opener = command.get_program().to_string_lossy().into_owned();
    let mut child = command.spawn()?;
    // The target is not logged: a link from a message can carry a token.
    worker::spawn("opener-wait", move || match child.wait() {
        Ok(status) if !status.success() => log::warn!("{opener}: {status}"),
        Ok(_) => {}
        Err(e) => log::warn!("{opener}: {e}"),
    });
    Ok(())
}

/// The opener for `target`, handed `token` under both names a launched app looks for it by.
fn command(target: &OsStr, text: bool, token: Option<&str>) -> Command {
    // `open -t` uses the default text editor, so a missing `.toml` association still opens one.
    let (opener, flags): (&str, &[&str]) = match (cfg!(target_os = "macos"), text) {
        (true, true) => ("open", &["-t"]),
        (true, false) => ("open", &[]),
        (false, _) => ("xdg-open", &[]),
    };
    let mut command = Command::new(opener);
    command.args(flags).arg(target);
    if let Some(token) = token {
        command.envs(postvak_core::paths::ACTIVATION_TOKEN_VARS.map(|var| (var, token)));
    }
    command
}

/// What to open once a token arrives.
#[cfg(any(target_os = "linux", test))]
struct Open {
    target: OsString,
    text: bool,
}

/// The opens waiting for a token, by the request that asked for it.
#[cfg(any(target_os = "linux", test))]
struct Pending<K> {
    waiting: Vec<(K, Open)>,
}

#[cfg(any(target_os = "linux", test))]
impl<K: PartialEq> Pending<K> {
    const fn new() -> Self {
        Pending {
            waiting: Vec::new(),
        }
    }

    fn add(&mut self, key: K, open: Open) {
        self.waiting.push((key, open));
    }

    /// The open waiting for `key`, once: a token after the wait ran out finds nothing.
    fn take(&mut self, key: &K) -> Option<Open> {
        let index = self
            .waiting
            .iter()
            .position(|(waiting, _)| waiting == key)?;
        Some(self.waiting.remove(index).1)
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::cell::RefCell;
    use std::ffi::OsStr;
    use std::time::Duration;

    use slint::ComponentHandle;
    use slint::winit_030::WinitWindowAccessor;
    use slint::winit_030::winit::event_loop::AsyncRequestSerial;
    use slint::winit_030::winit::platform::startup_notify::WindowExtStartupNotify;

    use super::{Open, Pending, start};

    /// How long an open waits for its token before it starts without one; KWin answers within a frame.
    const TOKEN_WAIT: Duration = Duration::from_millis(500);

    type Request = Box<dyn Fn() -> Option<AsyncRequestSerial>>;

    thread_local! {
        static REQUEST: RefCell<Option<Request>> = const { RefCell::new(None) };
        static PENDING: RefCell<Pending<AsyncRequestSerial>> = const { RefCell::new(Pending::new()) };
    }

    pub(super) fn attach<C: ComponentHandle + 'static>(weak: slint::Weak<C>) {
        let request: Request = Box::new(move || {
            weak.upgrade()?
                .window()
                .with_winit_window(|native| native.request_activation_token().ok())
                .flatten()
        });
        REQUEST.set(Some(request));
    }

    /// Asks the attached window for a token and waits for it to open `target`; false when no window can ask, off the
    /// window thread or without one attached.
    pub(super) fn defer(target: &OsStr, text: bool) -> bool {
        let Some(serial) = REQUEST.with_borrow(|request| request.as_ref().and_then(|ask| ask()))
        else {
            return false;
        };
        let open = Open {
            target: target.to_owned(),
            text,
        };
        PENDING.with_borrow_mut(|pending| pending.add(serial, open));
        slint::Timer::single_shot(TOKEN_WAIT, move || {
            if let Some(open) = PENDING.with_borrow_mut(|pending| pending.take(&serial)) {
                log::info!("no activation token in time; opening without one");
                launch(&open, None);
            }
        });
        true
    }

    pub(super) fn token_done(serial: AsyncRequestSerial, token: String) {
        if let Some(open) = PENDING.with_borrow_mut(|pending| pending.take(&serial)) {
            launch(&open, Some(&token));
        }
    }

    fn launch(open: &Open, token: Option<&str>) {
        if let Err(e) = start(&open.target, open.text, token) {
            log::warn!("could not open a link or file: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::{Open, Pending, command};

    fn open(target: &str) -> Open {
        Open {
            target: target.into(),
            text: false,
        }
    }

    #[test]
    fn the_opener_gets_the_token_under_the_wayland_and_the_x11_name() {
        let with = command(OsStr::new("https://example.org"), false, Some("tok"));
        let envs: Vec<_> = with.get_envs().collect();
        assert!(envs.contains(&("XDG_ACTIVATION_TOKEN".as_ref(), Some("tok".as_ref()))));
        assert!(envs.contains(&("DESKTOP_STARTUP_ID".as_ref(), Some("tok".as_ref()))));
        assert_eq!(
            with.get_args().last(),
            Some(OsStr::new("https://example.org"))
        );
        let without = command(OsStr::new("https://example.org"), false, None);
        assert_eq!(without.get_envs().count(), 0);
    }

    #[test]
    fn a_token_starts_only_the_open_that_asked_for_it_and_only_once() {
        let mut pending = Pending::new();
        pending.add(1, open("first"));
        pending.add(2, open("second"));
        assert_eq!(
            pending.take(&2).map(|open| open.target),
            Some("second".into())
        );
        assert!(pending.take(&2).is_none());
        assert_eq!(
            pending.take(&1).map(|open| open.target),
            Some("first".into())
        );
        assert!(pending.take(&3).is_none());
    }
}
