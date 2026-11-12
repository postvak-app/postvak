//! Opening a file or URL with the desktop's default app: on macOS through `NSWorkspace`, on Linux with `xdg-open` or the
//! Flatpak's portal. On Linux the window first asks the compositor (Wayland) or the X server for an activation token
//! and hands it to the opener: an app that is already running, such as the browser, may only come to the front with
//! one, and focus stealing prevention keeps it behind Postvak's window without.
use std::ffi::OsStr;
#[cfg(any(target_os = "linux", test))]
use std::ffi::OsString;
#[cfg(any(target_os = "linux", test))]
use std::process::Command;

use slint::ComponentHandle;
use slint::winit_030::winit::event::WindowEvent;

/// Opens `target` with the desktop's default app; `text` asks for the text editor where the platform can. On Linux
/// the app starts once the window's token arrives, so an error there is logged rather than returned.
pub(crate) fn open(target: &OsStr, text: bool) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    return mac::open(target, text);
    #[cfg(target_os = "linux")]
    {
        let _ = text;
        if linux::defer(target) {
            return Ok(());
        }
    }
    #[cfg(not(target_os = "macos"))]
    start(target, None)
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

#[cfg(not(target_os = "macos"))]
fn start(target: &OsStr, token: Option<&str>) -> std::io::Result<()> {
    // The Flatpak's xdg-open asks the portal without the token, so a link would open behind the window.
    #[cfg(target_os = "linux")]
    if postvak_core::paths::flatpak_id().is_some()
        && let Some(uri) = uri(target)
    {
        let (uri, token) = (uri.to_owned(), token.map(str::to_owned));
        crate::worker::spawn("opener-portal", move || {
            if let Err(e) = postvak_core::portal::open_uri(&uri, token.as_deref()) {
                log::warn!("could not open a link: {e:#}");
            }
        });
        return Ok(());
    }
    let mut command = command(target, token);
    let opener = command.get_program().to_string_lossy().into_owned();
    let mut child = command.spawn()?;
    // The target is not logged: a link from a message can carry a token.
    crate::worker::spawn("opener-wait", move || match child.wait() {
        Ok(status) if !status.success() => log::warn!("{opener}: {status}"),
        Ok(_) => {}
        Err(e) => log::warn!("{opener}: {e}"),
    });
    Ok(())
}

/// `target` when it is a URI rather than a local file, which the OpenURI portal refuses.
#[cfg(any(target_os = "linux", test))]
fn uri(target: &OsStr) -> Option<&str> {
    target
        .to_str()
        .filter(|t| !std::path::Path::new(t).is_absolute())
}

/// The opener for `target`, handed `token` under both names a launched app looks for it by.
#[cfg(any(target_os = "linux", test))]
fn command(target: &OsStr, token: Option<&str>) -> Command {
    let mut command = Command::new("xdg-open");
    command.arg(target);
    if let Some(token) = token {
        command.envs(postvak_core::paths::ACTIVATION_TOKEN_VARS.map(|var| (var, token)));
    }
    command
}

/// What to open once a token arrives.
#[cfg(any(target_os = "linux", test))]
struct Open {
    target: OsString,
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

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::OsStr;
    use std::io;
    use std::path::Path;

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2_app_kit::{NSRunningApplication, NSWorkspace, NSWorkspaceOpenConfiguration};
    use objc2_foundation::{NSArray, NSError, NSString, NSURL};
    use objc2_uniform_type_identifiers::UTType;

    /// Opens `target` with its default app; `text` with the default text editor, as `open -t` does, so a file type
    /// without an app of its own, such as `.toml`, still opens.
    pub(super) fn open(target: &OsStr, text: bool) -> io::Result<()> {
        let url = url(target).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "neither a URL nor an absolute path",
            )
        })?;
        let workspace = NSWorkspace::sharedWorkspace();
        let configuration = NSWorkspaceOpenConfiguration::configuration();
        // The target is not logged: a link from a message can carry a token.
        let done = RcBlock::new(|_: *mut NSRunningApplication, error: *mut NSError| {
            if !error.is_null() {
                log::warn!("macOS could not open a link or file");
            }
        });
        match text.then(|| text_editor(&workspace)).flatten() {
            Some(editor) => workspace
                .openURLs_withApplicationAtURL_configuration_completionHandler(
                    &NSArray::from_retained_slice(&[url]),
                    &editor,
                    &configuration,
                    Some(&done),
                ),
            None => {
                workspace.openURL_configuration_completionHandler(&url, &configuration, Some(&done))
            }
        }
        Ok(())
    }

    fn url(target: &OsStr) -> Option<Retained<NSURL>> {
        let target = NSString::from_str(target.to_str()?);
        if Path::new(&target.to_string()).is_absolute() {
            Some(NSURL::fileURLWithPath(&target))
        } else {
            NSURL::URLWithString(&target)
        }
    }

    fn text_editor(workspace: &NSWorkspace) -> Option<Retained<NSURL>> {
        let plain_text = UTType::typeWithIdentifier(&NSString::from_str("public.plain-text"))?;
        workspace.URLForApplicationToOpenContentType(&plain_text)
    }

    #[cfg(test)]
    mod tests {
        use std::ffi::OsStr;

        use objc2_app_kit::NSWorkspace;

        use super::{text_editor, url};

        #[test]
        fn an_absolute_path_is_a_file_and_a_link_keeps_its_scheme() {
            let file = url(OsStr::new("/tmp/rules.toml")).map(|url| url.isFileURL());
            assert_eq!(file, Some(true));
            let link = url(OsStr::new("https://example.org")).and_then(|url| url.scheme());
            assert_eq!(link.map(|scheme| scheme.to_string()), Some("https".into()));
        }

        #[test]
        fn macos_names_a_text_editor() {
            assert!(text_editor(&NSWorkspace::sharedWorkspace()).is_some());
        }
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
    pub(super) fn defer(target: &OsStr) -> bool {
        let Some(serial) = REQUEST.with_borrow(|request| request.as_ref().and_then(|ask| ask()))
        else {
            return false;
        };
        let open = Open {
            target: target.to_owned(),
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
        if let Err(e) = start(&open.target, token) {
            log::warn!("could not open a link or file: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::{Open, Pending, command, uri};

    #[test]
    fn a_link_is_a_uri_and_a_local_file_is_not() {
        assert_eq!(
            uri(OsStr::new("https://example.org")),
            Some("https://example.org")
        );
        assert_eq!(
            uri(OsStr::new("mailto:a@example.org")),
            Some("mailto:a@example.org")
        );
        assert_eq!(uri(OsStr::new("/home/me/.config/postvak/rules.toml")), None);
    }

    fn open(target: &str) -> Open {
        Open {
            target: target.into(),
        }
    }

    #[test]
    fn the_opener_gets_the_token_under_the_wayland_and_the_x11_name() {
        let with = command(OsStr::new("https://example.org"), Some("tok"));
        let envs: Vec<_> = with.get_envs().collect();
        assert!(envs.contains(&("XDG_ACTIVATION_TOKEN".as_ref(), Some("tok".as_ref()))));
        assert!(envs.contains(&("DESKTOP_STARTUP_ID".as_ref(), Some("tok".as_ref()))));
        assert_eq!(
            with.get_args().last(),
            Some(OsStr::new("https://example.org"))
        );
        let without = command(OsStr::new("https://example.org"), None);
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
