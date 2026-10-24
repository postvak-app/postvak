//! The unread count on Postvak's task bar or dock entry. On Linux through the `com.canonical.Unity.LauncherEntry`
//! D-Bus signal that Plasma's task manager, Ubuntu's dock and Dash to Dock read; on macOS on the Dock icon.
#[cfg(target_os = "linux")]
use std::collections::HashMap;

#[cfg(target_os = "linux")]
use zbus::zvariant::Value;

#[cfg(target_os = "linux")]
const INTERFACE: &str = "com.canonical.Unity.LauncherEntry";
#[cfg(target_os = "linux")]
const PATH: &str = "/io/github/postvak_app/postvak/LauncherEntry";

#[derive(Default)]
pub struct Badge {
    /// The count the entry shows; None before the first.
    shown: Option<u32>,
    /// Connected on the first count; None when that failed, since without a session bus there is no entry to badge.
    #[cfg(target_os = "linux")]
    bus: std::cell::OnceCell<Option<zbus::blocking::Connection>>,
}

impl Badge {
    pub fn new() -> Badge {
        Badge::default()
    }

    /// Shows `unread` on the entry, or no count at 0; only a changed count is sent.
    pub fn show(&mut self, unread: u32) {
        if self.shown == Some(unread) {
            return;
        }
        self.shown = Some(unread);
        #[cfg(target_os = "linux")]
        self.send(unread);
        #[cfg(target_os = "macos")]
        dock(unread);
    }

    #[cfg(target_os = "linux")]
    fn send(&self, unread: u32) {
        let bus = self.bus.get_or_init(|| {
            zbus::blocking::Connection::session()
                .inspect_err(|e| log::debug!("no session bus for the unread count: {e}"))
                .ok()
        });
        let Some(bus) = bus else { return };
        let sent = bus.emit_signal(
            None::<&str>,
            PATH,
            INTERFACE,
            "Update",
            &(app_uri(), properties(unread)),
        );
        if let Err(e) = sent {
            log::debug!("could not show the unread count: {e}");
        }
    }
}

/// Shows `unread` on the Dock icon, or no badge at 0. AppKit takes it only on the main thread, where the window runs.
#[cfg(target_os = "macos")]
fn dock(unread: u32) {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;
    use objc2_foundation::NSString;
    let Some(main) = MainThreadMarker::new() else {
        log::debug!("the Dock badge is set from the main thread only");
        return;
    };
    let label = dock_label(unread).map(|label| NSString::from_str(&label));
    NSApplication::sharedApplication(main)
        .dockTile()
        .setBadgeLabel(label.as_deref());
}

/// The Dock badge's text: the count, or none at 0.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn dock_label(unread: u32) -> Option<String> {
    (unread > 0).then(|| unread.to_string())
}

/// The desktop entry the count belongs to.
#[cfg(target_os = "linux")]
fn app_uri() -> String {
    format!("application://{}.desktop", crate::paths::APP_ID)
}

#[cfg(target_os = "linux")]
fn properties(unread: u32) -> HashMap<&'static str, Value<'static>> {
    HashMap::from([
        ("count", Value::from(i64::from(unread))),
        ("count-visible", Value::from(unread > 0)),
    ])
}

#[cfg(test)]
mod dock_tests {
    use super::dock_label;

    #[test]
    fn the_dock_shows_the_count_and_no_badge_at_zero() {
        assert_eq!(dock_label(7).as_deref(), Some("7"));
        assert_eq!(dock_label(0), None);
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn the_count_shows_only_when_there_is_unread_mail() {
        assert_eq!(properties(12)["count"], Value::from(12i64));
        assert_eq!(properties(12)["count-visible"], Value::from(true));
        assert_eq!(properties(0)["count-visible"], Value::from(false));
        assert_eq!(
            app_uri(),
            "application://io.github.postvak_app.postvak.desktop"
        );
    }

    /// On a private bus, so no dock of yours sees it:
    /// `dbus-run-session -- env POSTVAK_TEST_DBUS=1 cargo test -p postvak-core badge`.
    #[test]
    fn a_changed_count_is_sent_once() {
        use std::time::Duration;
        use zbus::blocking::MessageIterator;
        use zbus::zvariant::OwnedValue;

        if std::env::var_os("POSTVAK_TEST_DBUS").is_none() {
            return;
        }
        let dock = zbus::blocking::Connection::session().unwrap();
        let rule = zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .interface(INTERFACE)
            .unwrap()
            .build();
        let mut updates = MessageIterator::for_match_rule(rule, &dock, None).unwrap();
        let (sent, received) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            while let Some(Ok(update)) = updates.next() {
                let (uri, properties): (String, HashMap<String, OwnedValue>) =
                    update.body().deserialize().unwrap();
                let count = i64::try_from(&properties["count"]).unwrap();
                sent.send((uri, count)).unwrap();
            }
        });
        let mut badge = Badge::new();
        badge.show(3);
        badge.show(3);
        badge.show(0);
        let wait = Duration::from_secs(5);
        assert_eq!(received.recv_timeout(wait).unwrap(), (app_uri(), 3));
        assert_eq!(received.recv_timeout(wait).unwrap(), (app_uri(), 0));
        assert!(received.recv_timeout(Duration::from_millis(200)).is_err());
    }
}
