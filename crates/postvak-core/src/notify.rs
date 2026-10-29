//! Desktop notifications for new mail and for rule proposals waiting for approval, sent by the daemon. A click on one
//! opens the message, or the Rules view, in the window.
#[cfg(target_os = "linux")]
mod dbus;
#[cfg(target_os = "macos")]
mod mac;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};

use serde::{Deserialize, Serialize};

use crate::message::{clean, display_name};
use crate::sync::Event;

/// The new message a notification is about.
#[derive(Clone, Debug, PartialEq)]
pub struct Mail {
    pub account: String,
    pub folder: String,
    pub uid: u32,
    pub from: String,
    pub subject: String,
}

/// A message to show in the window: the uid in an account's folder; no account means the first. What
/// `postvak gui --open` asks for and a notification click opens.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Open {
    pub account: Option<String>,
    pub folder: String,
    pub uid: u32,
}

impl Open {
    /// The `postvak` arguments that show it: `gui --open <uid> [--account <account>] --folder <folder>`.
    pub fn gui_args(&self) -> Vec<String> {
        let mut args = vec!["gui".into(), "--open".into(), self.uid.to_string()];
        if let Some(account) = &self.account {
            args.extend(["--account".into(), account.clone()]);
        }
        args.extend(["--folder".into(), self.folder.clone()]);
        args
    }
}

/// What the window shows when it starts or a later start hands it a request: what a notification click opens.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub enum Show {
    Message(Open),
    /// The Rules view, where proposals wait for approval.
    Rules,
}

impl Show {
    /// The `postvak` arguments that show it.
    pub fn gui_args(&self) -> Vec<String> {
        match self {
            Show::Message(open) => open.gui_args(),
            Show::Rules => vec!["gui".into(), "--rules".into()],
        }
    }
}

/// What one notification says, and what a click on it shows.
#[derive(Clone, Debug, PartialEq)]
pub struct Notice {
    /// Plain text.
    pub title: String,
    /// Plain text; escaped where the server renders markup.
    pub body: String,
    pub show: Show,
}

/// More new messages from one sync pass than this become one notification.
const BURST: usize = 3;
/// Senders a combined notification names.
const NAMED: usize = 3;

/// The notifications for one account's new mail from one sync pass, oldest first: one per message, or one for them all
/// when there are more than `BURST`, so a laptop that slept through a morning does not pop up dozens. That one opens
/// the newest message.
pub fn notices(batch: &[Mail]) -> Vec<Notice> {
    let notice = |mail: &Mail, title: String, body: String| Notice {
        title,
        body,
        show: Show::Message(Open {
            account: Some(mail.account.clone()),
            folder: mail.folder.clone(),
            uid: mail.uid,
        }),
    };
    let Some(newest) = batch.last().filter(|_| batch.len() > BURST) else {
        return batch
            .iter()
            .map(|mail| notice(mail, title(&mail.from), clean(&mail.subject, false)))
            .collect();
    };
    let mut senders: Vec<String> = Vec::new();
    for mail in batch.iter().rev() {
        let name = title(&mail.from);
        if !name.is_empty() && !senders.contains(&name) {
            senders.push(name);
        }
    }
    let body = if senders.is_empty() {
        String::new()
    } else {
        format!("From {}", named(&senders))
    };
    vec![notice(
        newest,
        format!("{} new messages", batch.len()),
        body,
    )]
}

/// The notification for rule proposals that started waiting for approval, by name; a click opens the Rules view.
pub fn proposals_notice(names: &[String]) -> Option<Notice> {
    let title = match names.len() {
        0 => return None,
        1 => "A rule waits for your approval".to_string(),
        n => format!("{n} rules wait for your approval"),
    };
    Some(Notice {
        title,
        body: named(names),
        show: Show::Rules,
    })
}

/// `a`, `a and b`, `a, b and c`, or the first `NAMED` and how many others.
fn named(items: &[String]) -> String {
    match items.len() {
        0 => String::new(),
        1 => items[0].clone(),
        n if n <= NAMED => format!("{} and {}", items[..n - 1].join(", "), items[n - 1]),
        n => format!("{} and {} others", items[..NAMED].join(", "), n - NAMED),
    }
}

/// Each account's new mail, held until the sync pass that found it ends, so a burst becomes one notification. A pass
/// sends its `NewMail` events right before its `Synced`.
#[derive(Default)]
pub struct Batches(BTreeMap<String, Vec<Mail>>);

impl Batches {
    /// The batch `event` completes: an account's new mail, once the pass that found it is `Synced`.
    pub fn hear(&mut self, event: &Event) -> Option<Vec<Mail>> {
        match event {
            Event::NewMail {
                account,
                folder,
                uid,
                from,
                subject,
            } => {
                self.0.entry(account.clone()).or_default().push(Mail {
                    account: account.clone(),
                    folder: folder.clone(),
                    uid: *uid,
                    from: from.clone(),
                    subject: subject.clone(),
                });
                None
            }
            Event::Synced { account, .. } => self.0.remove(account),
            _ => None,
        }
    }

    /// The mail no pass's end claimed.
    pub fn rest(self) -> impl Iterator<Item = Vec<Mail>> {
        self.0.into_values()
    }
}

/// Sends the notifications from a thread of its own, so a slow notification server never holds up the daemon's
/// events. On Linux that thread keeps one session bus connection, which also hears which ones were clicked.
pub struct Notifier {
    notices: mpsc::Sender<Vec<Notice>>,
}

impl Notifier {
    pub fn new() -> Notifier {
        let (notices, received) = mpsc::channel::<Vec<Notice>>();
        let spawned = std::thread::Builder::new()
            .name("notifications".into())
            .spawn(move || {
                #[cfg(target_os = "linux")]
                let mut bus = dbus::Bus::new(std::sync::Arc::new(open_in_window));
                #[cfg(target_os = "macos")]
                let center = mac::Center::new();
                for batch in received {
                    for notice in batch {
                        #[cfg(target_os = "linux")]
                        bus.notify(&notice);
                        #[cfg(target_os = "macos")]
                        center.notify(&notice);
                    }
                }
            });
        if let Err(e) = spawned {
            log::warn!("no desktop notifications: {e}");
        }
        Notifier { notices }
    }

    /// Notifies one account's new mail from one sync pass.
    pub fn new_mail(&self, batch: &[Mail]) {
        let _ = self.notices.send(notices(batch));
    }

    /// Notifies the rule proposals, by name, that started waiting for approval.
    pub fn proposals(&self, names: &[String]) {
        let _ = self
            .notices
            .send(proposals_notice(names).into_iter().collect());
    }
}

impl Default for Notifier {
    fn default() -> Notifier {
        Notifier::new()
    }
}

/// The sender's name, or the address when there is none: the title is plain text and short.
fn title(from: &str) -> String {
    display_name(&clean(from, false))
}

/// `&`, `<` and `>` as entities: Linux notification servers render a subset of HTML in the body, and launchd plists
/// are XML.
pub fn escape_markup(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Notifications that wait for a click, each on a thread of its own until it is clicked or dismissed. macOS keeps them
/// in Notification Center until the user clears it, so only `MAX_WAITING` wait; later ones still show, and a click on
/// one brings Postvak to the front without opening its message.
#[derive(Default)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
struct Waiting(AtomicUsize);

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const MAX_WAITING: usize = 64;

/// One waiting notification, counted until it drops.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
struct Slot(Arc<Waiting>);

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
impl Waiting {
    /// A slot, or None when `MAX_WAITING` are taken.
    fn take(waiting: &Arc<Waiting>) -> Option<Slot> {
        waiting
            .0
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < MAX_WAITING).then_some(n + 1)
            })
            .ok()
            .map(|_| Slot(Arc::clone(waiting)))
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Shows what the notice is about in the window: a running window shows it, else a new one starts. `token` is the
/// notification server's activation token, which lets the window come to the front.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn open_in_window(notice: &Notice, token: Option<&str>) {
    let exe = match crate::paths::stable_exe() {
        Ok(exe) => exe,
        Err(e) => {
            log::warn!("could not find this program to open the message: {e}");
            return;
        }
    };
    match window_command(&exe, &notice.show, token).spawn() {
        // Waited for, so it does not linger as a zombie of a daemon that runs for days.
        Ok(child) => crate::daemon::client::reap(child),
        Err(e) => log::warn!("could not open the message: {e}"),
    }
}

/// `postvak gui --open …` or `postvak gui --rules`, with the token where a launched app looks for it, or without an
/// inherited one.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn window_command(
    exe: &std::path::Path,
    show: &Show,
    token: Option<&str>,
) -> std::process::Command {
    use std::process::{Command, Stdio};
    let mut command = Command::new(exe);
    command
        .args(show.gui_args())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for var in crate::paths::ACTIVATION_TOKEN_VARS {
        match token {
            Some(token) => command.env(var, token),
            None => command.env_remove(var),
        };
    }
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_markup_escapes_tags_and_entities() {
        assert_eq!(
            escape_markup("<b>Tom & Jerry</b>"),
            "&lt;b&gt;Tom &amp; Jerry&lt;/b&gt;"
        );
    }

    /// Regression: the title was the whole From, escaped, and notification servers show the title as plain text, so it
    /// read `Alice &lt;alice@example.com&gt;`.
    #[test]
    fn the_title_is_the_senders_name_unescaped() {
        assert_eq!(title("Alice Smith <alice@example.com>"), "Alice Smith");
        assert_eq!(title("AT&T <news@att.example>"), "AT&T");
        assert_eq!(title("bob@example.com"), "bob@example.com");
    }

    fn mail(uid: u32, from: &str) -> Mail {
        Mail {
            account: "work".into(),
            folder: "INBOX".into(),
            uid,
            from: from.into(),
            subject: format!("subject {uid}"),
        }
    }

    #[test]
    fn a_few_new_messages_notify_one_by_one() {
        let batch = [
            mail(1, "Alice <a@x>"),
            mail(2, "bob@x"),
            mail(3, "Carol <c@x>"),
        ];
        let notices = notices(&batch);
        assert_eq!(notices.len(), 3);
        assert_eq!(notices[1].title, "bob@x");
        assert_eq!(notices[1].body, "subject 2");
        assert!(matches!(&notices[1].show, Show::Message(open) if open.uid == 2));
    }

    #[test]
    fn a_burst_is_one_notification_that_opens_the_newest() {
        let batch = [
            mail(1, "Alice <a@x>"),
            mail(2, "Bob <b@x>"),
            mail(3, "Alice <a@x>"),
            mail(4, "Carol <c@x>"),
        ];
        assert_eq!(
            notices(&batch),
            [Notice {
                title: "4 new messages".into(),
                body: "From Carol, Alice and Bob".into(),
                show: Show::Message(Open {
                    account: Some("work".into()),
                    folder: "INBOX".into(),
                    uid: 4,
                }),
            }]
        );
        let many: Vec<_> = (1..=6)
            .map(|uid| mail(uid, &format!("S{uid} <s{uid}@x>")))
            .collect();
        assert_eq!(notices(&many)[0].body, "From S6, S5, S4 and 3 others");
        let one_sender: Vec<_> = (1..=5).map(|uid| mail(uid, "Alice <a@x>")).collect();
        assert_eq!(notices(&one_sender)[0].body, "From Alice");
    }

    #[test]
    fn gui_args_name_the_account_only_when_there_is_one() {
        let open = Open {
            account: None,
            folder: "INBOX".into(),
            uid: 3,
        };
        assert_eq!(open.gui_args(), ["gui", "--open", "3", "--folder", "INBOX"]);
    }

    #[test]
    fn new_proposals_are_one_notification_that_opens_the_rules_view() {
        let names = |n: usize| -> Vec<String> { (1..=n).map(|i| format!("r{i}")).collect() };
        assert_eq!(proposals_notice(&[]), None);
        assert_eq!(
            proposals_notice(&names(1)),
            Some(Notice {
                title: "A rule waits for your approval".into(),
                body: "r1".into(),
                show: Show::Rules,
            })
        );
        let five = proposals_notice(&names(5)).unwrap();
        assert_eq!(five.title, "5 rules wait for your approval");
        assert_eq!(five.body, "r1, r2, r3 and 2 others");
        assert_eq!(Show::Rules.gui_args(), ["gui", "--rules"]);
    }

    #[test]
    fn only_max_waiting_notifications_wait_and_a_slot_frees_when_it_drops() {
        let waiting = Arc::new(Waiting::default());
        let slots: Vec<Slot> = (0..MAX_WAITING)
            .map(|_| Waiting::take(&waiting).unwrap())
            .collect();
        assert!(Waiting::take(&waiting).is_none());
        drop(slots);
        assert!(Waiting::take(&waiting).is_some());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn a_click_opens_the_message_in_the_window_with_the_token() {
        let open = Open {
            account: Some("work".into()),
            folder: "INBOX/Team".into(),
            uid: 42,
        };
        let show = Show::Message(open);
        let command = window_command(std::path::Path::new("/usr/bin/postvak"), &show, Some("tok"));
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(
            args,
            [
                "gui",
                "--open",
                "42",
                "--account",
                "work",
                "--folder",
                "INBOX/Team"
            ]
        );
        let envs: Vec<_> = command.get_envs().collect();
        assert!(envs.contains(&("XDG_ACTIVATION_TOKEN".as_ref(), Some("tok".as_ref()))));
        assert!(envs.contains(&("DESKTOP_STARTUP_ID".as_ref(), Some("tok".as_ref()))));

        // Without one, a stale token the daemon inherited is not passed on.
        let command = window_command(std::path::Path::new("/usr/bin/postvak"), &show, None);
        let envs: Vec<_> = command.get_envs().collect();
        assert!(envs.contains(&("XDG_ACTIVATION_TOKEN".as_ref(), None)));
    }
}
