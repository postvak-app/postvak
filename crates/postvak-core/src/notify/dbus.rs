//! Notifications through the desktop's notification server (`org.freedesktop.Notifications` on the session bus), on one
//! connection that also hears the server's signals: a click arrives as `ActivationToken` and then `ActionInvoked`, and
//! servers send those only to the connection that sent the notification.
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use zbus::blocking::{Connection, MessageIterator};
use zbus::message::Type;
use zbus::zvariant::Value;
use zbus::{MatchRule, Message};

use super::{Notice, Show, escape_markup};
use crate::daemon::lock;
use crate::paths::APP_ID;

const SERVER: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";
/// The action a click on the notification itself invokes.
const DEFAULT_ACTION: &str = "default";
/// Notifications whose click is still remembered; a server may keep older ones in its history without telling.
const REMEMBERED: usize = 64;

/// What a click does: opens the notice's message, with the activation token the server gave for it.
pub type OnClick = Arc<dyn Fn(&Notice, Option<&str>) + Send + Sync>;

pub struct Bus {
    /// Connected on the first notification; tried again on the next one while there is no notification server.
    session: Option<Session>,
    clicks: Arc<Mutex<Clicks>>,
    on_click: OnClick,
}

struct Session {
    connection: Connection,
    /// Whether the server renders markup in the body; then `&`, `<` and `>` there must be entities.
    body_markup: bool,
}

impl Bus {
    pub fn new(on_click: OnClick) -> Bus {
        Bus {
            session: None,
            clicks: Arc::new(Mutex::new(Clicks::default())),
            on_click,
        }
    }

    pub fn notify(&mut self, notice: &Notice) {
        if self.session.is_none() {
            match self.connect() {
                Ok(session) => self.session = Some(session),
                Err(e) => {
                    log::debug!("no notification server: {e}");
                    return;
                }
            }
        }
        let Some(session) = &self.session else { return };
        match send(session, notice) {
            Ok((id, server)) => lock(&self.clicks).sent(id, server, notice.clone()),
            // The connection stays: a server that failed this once (restarting, say) is started again by the bus.
            Err(e) => log::debug!("could not send a notification: {e}"),
        }
    }

    /// Connects, and hears the server's signals from then on.
    fn connect(&self) -> zbus::Result<Session> {
        let connection = Connection::session()?;
        // Fails at once on a desktop without a notification server, before anything is started for it.
        let capabilities: Vec<String> = connection
            .call_method(Some(SERVER), PATH, Some(SERVER), "GetCapabilities", &())?
            .body()
            .deserialize()?;
        let rule = MatchRule::builder()
            .msg_type(Type::Signal)
            .interface(SERVER)?
            .path(PATH)?
            .build();
        let signals = MessageIterator::for_match_rule(rule, &connection, Some(REMEMBERED))?;
        let (clicks, on_click) = (self.clicks.clone(), self.on_click.clone());
        std::thread::Builder::new()
            .name("notification-clicks".into())
            .spawn(move || {
                for message in signals.flatten() {
                    if let Some((from, signal)) = Signal::read(&message)
                        && let Some((notice, token)) = lock(&clicks).heard(&from, signal)
                    {
                        on_click(&notice, token.as_deref());
                    }
                }
            })
            .map_err(|e| zbus::Error::Failure(e.to_string()))?;
        Ok(Session {
            connection,
            body_markup: capabilities.iter().any(|c| c == "body-markup"),
        })
    }
}

/// Sends the notification; its id, and the server's unique bus name, which the signals about it come from.
fn send(session: &Session, notice: &Notice) -> zbus::Result<(u32, Option<String>)> {
    let request = Request::of(notice, session.body_markup);
    let reply = session.connection.call_method(
        Some(SERVER),
        PATH,
        Some(SERVER),
        "Notify",
        &(
            "Postvak",
            0u32,
            APP_ID,
            request.summary.as_str(),
            request.body.as_str(),
            request.actions,
            request.hints,
            -1i32,
        ),
    )?;
    let server = reply.header().sender().map(ToString::to_string);
    Ok((reply.body().deserialize()?, server))
}

/// A `Notify` call's texts, actions and hints.
struct Request {
    summary: String,
    body: String,
    actions: Vec<&'static str>,
    hints: HashMap<&'static str, Value<'static>>,
}

impl Request {
    fn of(notice: &Notice, body_markup: bool) -> Request {
        Request {
            // The summary is plain text everywhere; only the body may hold markup.
            summary: notice.title.clone(),
            body: if body_markup {
                escape_markup(&notice.body)
            } else {
                notice.body.clone()
            },
            actions: vec![DEFAULT_ACTION, "Open"],
            hints: hints(&notice.show),
        }
    }
}

fn hints(show: &Show) -> HashMap<&'static str, Value<'static>> {
    // Ties the notification to the desktop entry: its icon, and the desktop's per-app settings.
    let mut hints = HashMap::from([("desktop-entry", Value::from(APP_ID))]);
    if let Show::Message(open) = show {
        hints.extend([
            ("category", Value::from("email.arrived")),
            // From the sound theme; the server plays it, so Do Not Disturb and the volume apply.
            ("sound-name", Value::from("message-new-email")),
            // Plasma shows it beside the app's name, so you see which account the mail is for.
            (
                "x-kde-origin-name",
                Value::from(open.account.clone().unwrap_or_default()),
            ),
        ]);
    }
    hints
}

/// The server's signals about notifications.
#[derive(Debug, PartialEq)]
enum Signal {
    ActionInvoked { id: u32, action: String },
    ActivationToken { id: u32, token: String },
    Closed { id: u32 },
}

impl Signal {
    /// The signal, and the unique bus name it came from.
    fn read(message: &Message) -> Option<(String, Signal)> {
        let header = message.header();
        let from = header.sender()?.to_string();
        let body = message.body();
        let signal = match header.member()?.as_str() {
            "ActionInvoked" => {
                let (id, action) = body.deserialize().ok()?;
                Signal::ActionInvoked { id, action }
            }
            "ActivationToken" => {
                let (id, token) = body.deserialize().ok()?;
                Signal::ActivationToken { id, token }
            }
            "NotificationClosed" => {
                let (id, _reason): (u32, u32) = body.deserialize().ok()?;
                Signal::Closed { id }
            }
            _ => return None,
        };
        Some((from, signal))
    }
}

/// The notifications that can still be clicked, by id.
#[derive(Default)]
struct Clicks {
    /// The notification server's unique name; signals from anyone else on the bus are not its.
    server: Option<String>,
    sent: BTreeMap<u32, (Notice, Option<String>)>,
}

impl Clicks {
    fn sent(&mut self, id: u32, server: Option<String>, notice: Notice) {
        if server.is_some() {
            self.server = server;
        }
        self.sent.insert(id, (notice, None));
        while self.sent.len() > REMEMBERED {
            self.sent.pop_first();
        }
    }

    /// The notice to open and its token, when `signal`, from bus name `from`, is a click on one of ours.
    fn heard(&mut self, from: &str, signal: Signal) -> Option<(Notice, Option<String>)> {
        if self.server.as_deref() != Some(from) {
            return None;
        }
        match signal {
            Signal::ActivationToken { id, token, .. } => {
                if let Some((_, slot)) = self.sent.get_mut(&id) {
                    *slot = Some(token);
                }
                None
            }
            Signal::ActionInvoked { id, action, .. }
                if action == DEFAULT_ACTION || action == "Open" =>
            {
                self.sent.remove(&id)
            }
            Signal::ActionInvoked { .. } => None,
            Signal::Closed { id, .. } => {
                self.sent.remove(&id);
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mail(uid: u32) -> Notice {
        Notice {
            title: "Tom & Jerry".into(),
            body: "Cheese <today> & tomorrow".into(),
            show: Show::Message(super::super::Open {
                account: Some("work".into()),
                folder: "INBOX".into(),
                uid,
            }),
        }
    }

    #[test]
    fn the_notification_names_the_desktop_entry_and_asks_for_the_mail_sound() {
        let request = Request::of(&mail(1), true);
        assert_eq!(request.summary, "Tom & Jerry");
        assert_eq!(request.body, "Cheese &lt;today&gt; &amp; tomorrow");
        assert_eq!(request.actions, ["default", "Open"]);
        assert_eq!(request.hints["desktop-entry"], Value::from(APP_ID));
        assert_eq!(request.hints["category"], Value::from("email.arrived"));
        assert_eq!(request.hints["x-kde-origin-name"], Value::from("work"));
        assert_eq!(
            request.hints["sound-name"],
            Value::from("message-new-email")
        );
    }

    #[test]
    fn a_proposal_notification_neither_plays_the_mail_sound_nor_names_an_account() {
        let notice = Notice {
            title: "A rule waits for your approval".into(),
            body: "newsletters".into(),
            show: Show::Rules,
        };
        let hints = Request::of(&notice, true).hints;
        assert_eq!(hints.keys().collect::<Vec<_>>(), [&"desktop-entry"]);
    }

    #[test]
    fn the_body_is_escaped_only_for_a_server_that_renders_markup() {
        assert_eq!(
            Request::of(&mail(1), false).body,
            "Cheese <today> & tomorrow"
        );
    }

    fn server() -> String {
        ":1.7".into()
    }

    #[test]
    fn a_click_opens_its_mail_once_with_the_token_sent_before_it() {
        let mut clicks = Clicks::default();
        clicks.sent(5, Some(server()), mail(10));
        clicks.sent(6, Some(server()), mail(11));
        let token = Signal::ActivationToken {
            id: 6,
            token: "tok".into(),
        };
        assert_eq!(clicks.heard(&server(), token), None);
        let click = || Signal::ActionInvoked {
            id: 6,
            action: "default".into(),
        };
        assert_eq!(
            clicks.heard(&server(), click()),
            Some((mail(11), Some("tok".into())))
        );
        assert_eq!(clicks.heard(&server(), click()), None);
        // Opened without a token from a server that sends none.
        let click = Signal::ActionInvoked {
            id: 5,
            action: "Open".into(),
        };
        assert_eq!(clicks.heard(&server(), click), Some((mail(10), None)));
    }

    #[test]
    fn signals_from_others_on_the_bus_and_closed_notifications_open_nothing() {
        let mut clicks = Clicks::default();
        clicks.sent(5, Some(server()), mail(10));
        let click = || Signal::ActionInvoked {
            id: 5,
            action: "default".into(),
        };
        assert_eq!(clicks.heard(":1.99", click()), None);
        assert_eq!(clicks.heard(&server(), Signal::Closed { id: 5 }), None);
        assert_eq!(clicks.heard(&server(), click()), None);
    }

    /// Against a stand-in notification server on a private bus, so nothing reaches your desktop:
    /// `dbus-run-session -- env POSTVAK_TEST_DBUS=1 cargo test -p postvak-core notify::dbus`.
    #[test]
    fn a_click_on_the_bus_opens_the_mail_with_its_token() {
        use std::sync::mpsc::channel;
        use std::time::Duration;
        use zbus::zvariant::OwnedValue;
        type Notify = (
            String,
            u32,
            String,
            String,
            String,
            Vec<String>,
            HashMap<String, OwnedValue>,
            i32,
        );

        if std::env::var_os("POSTVAK_TEST_DBUS").is_none() {
            return;
        }
        let server = Connection::session().unwrap();
        server.request_name(SERVER).unwrap();
        let rule = MatchRule::builder()
            .msg_type(Type::MethodCall)
            .interface(SERVER)
            .unwrap()
            .build();
        let calls = MessageIterator::for_match_rule(rule, &server, None).unwrap();
        let (notified, notifies) = channel();
        std::thread::spawn({
            let server = server.clone();
            move || {
                for call in calls.flatten() {
                    let header = call.header();
                    let reply = Message::method_return(&header).unwrap();
                    let reply = match header.member().unwrap().as_str() {
                        // No body-markup, so the body stays as it is.
                        "GetCapabilities" => reply.build(&vec!["body", "actions"]),
                        "Notify" => {
                            let notify: Notify = call.body().deserialize().unwrap();
                            notified
                                .send((notify, header.sender().unwrap().to_string()))
                                .unwrap();
                            reply.build(&7u32)
                        }
                        _ => continue,
                    };
                    server.send(&reply.unwrap()).unwrap();
                }
            }
        });
        let (opened, opens) = channel();
        let mut bus = Bus::new(Arc::new(move |mail: &Notice, token: Option<&str>| {
            opened
                .send((mail.clone(), token.map(str::to_string)))
                .unwrap();
        }));
        bus.notify(&mail(3));

        let wait = Duration::from_secs(5);
        let (notify, client) = notifies.recv_timeout(wait).unwrap();
        assert_eq!(notify.0, "Postvak");
        assert_eq!(notify.2, APP_ID);
        assert_eq!(notify.3, "Tom & Jerry");
        assert_eq!(notify.4, "Cheese <today> & tomorrow");
        assert_eq!(notify.5, ["default", "Open"]);
        assert_eq!(
            notify.6["desktop-entry"],
            OwnedValue::try_from(Value::from(APP_ID)).unwrap()
        );
        // Sent to the client alone, as Plasma does.
        for (signal, body) in [
            ("ActivationToken", (7u32, "tok")),
            ("ActionInvoked", (7u32, "default")),
        ] {
            server
                .emit_signal(Some(client.as_str()), PATH, SERVER, signal, &body)
                .unwrap();
        }
        assert_eq!(
            opens.recv_timeout(wait).unwrap(),
            (mail(3), Some("tok".to_string()))
        );
    }

    #[test]
    fn only_the_latest_notifications_are_remembered() {
        let mut clicks = Clicks::default();
        for id in 0..(REMEMBERED as u32 + 10) {
            clicks.sent(id, Some(server()), mail(id));
        }
        assert_eq!(clicks.sent.len(), REMEMBERED);
        assert_eq!(clicks.sent.keys().next(), Some(&10));
    }
}
