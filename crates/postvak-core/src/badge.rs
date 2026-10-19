//! The unread count on Postvak's task bar or dock entry, through the `com.canonical.Unity.LauncherEntry` D-Bus signal
//! that Plasma's task manager, Ubuntu's dock and Dash to Dock read. Linux only; elsewhere it does nothing.
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
    #[cfg(target_os = "linux")]
    bus: Option<zbus::blocking::Connection>,
    /// Connecting failed once; without a session bus there is no entry to badge.
    #[cfg(target_os = "linux")]
    no_bus: bool,
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
    }

    #[cfg(target_os = "linux")]
    fn send(&mut self, unread: u32) {
        if self.bus.is_none() && !self.no_bus {
            match zbus::blocking::Connection::session() {
                Ok(bus) => self.bus = Some(bus),
                Err(e) => {
                    log::debug!("no session bus for the unread count: {e}");
                    self.no_bus = true;
                }
            }
        }
        let Some(bus) = &self.bus else { return };
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
