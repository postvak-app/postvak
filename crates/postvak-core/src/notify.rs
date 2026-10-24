//! Desktop notifications for new mail, sent by the daemon. On Linux a click on one opens the message in the window.
#[cfg(target_os = "linux")]
mod dbus;

use std::collections::BTreeMap;
use std::sync::mpsc;

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

/// What one notification says, and the message a click on it opens.
#[derive(Clone, Debug, PartialEq)]
pub struct Notice {
    /// Plain text.
    pub title: String,
    /// Plain text; escaped where the server renders markup.
    pub body: String,
    pub open: Open,
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
        open: Open {
            account: Some(mail.account.clone()),
            folder: mail.folder.clone(),
            uid: mail.uid,
        },
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
    let body = match senders.len() {
        0 => String::new(),
        1 => format!("From {}", senders[0]),
        n if n <= NAMED => format!(
            "From {} and {}",
            senders[..n - 1].join(", "),
            senders[n - 1]
        ),
        n => format!(
            "From {} and {} others",
            senders[..NAMED].join(", "),
            n - NAMED
        ),
    };
    vec![notice(
        newest,
        format!("{} new messages", batch.len()),
        body,
    )]
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
    batches: mpsc::Sender<Vec<Mail>>,
}

impl Notifier {
    pub fn new() -> Notifier {
        let (batches, received) = mpsc::channel::<Vec<Mail>>();
        let spawned = std::thread::Builder::new()
            .name("notifications".into())
            .spawn(move || {
                #[cfg(target_os = "linux")]
                let mut bus = dbus::Bus::new(std::sync::Arc::new(open_in_window));
                for batch in received {
                    for notice in notices(&batch) {
                        #[cfg(target_os = "linux")]
                        bus.notify(&notice);
                        #[cfg(target_os = "macos")]
                        {
                            let _ = notify_rust::Notification::new()
                                .summary(&notice.title)
                                .body(&notice.body)
                                .appname("Postvak")
                                .show();
                        }
                    }
                }
            });
        if let Err(e) = spawned {
            log::warn!("no desktop notifications: {e}");
        }
        Notifier { batches }
    }

    /// Notifies one account's new mail from one sync pass.
    pub fn new_mail(&self, batch: &[Mail]) {
        let _ = self.batches.send(batch.to_vec());
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

/// Opens the notice's message in the window: a running window shows it, else a new one starts. `token` is the
/// notification server's activation token, which lets the window come to the front.
#[cfg(target_os = "linux")]
fn open_in_window(notice: &Notice, token: Option<&str>) {
    let exe = match crate::paths::stable_exe() {
        Ok(exe) => exe,
        Err(e) => {
            log::warn!("could not find this program to open the message: {e}");
            return;
        }
    };
    match window_command(&exe, &notice.open, token).spawn() {
        // Waited for, so it does not linger as a zombie of a daemon that runs for days.
        Ok(child) => crate::daemon::client::reap(child),
        Err(e) => log::warn!("could not open the message: {e}"),
    }
}

/// `postvak gui --open …`, with the token where a launched app looks for it, or without an inherited one.
#[cfg(target_os = "linux")]
fn window_command(
    exe: &std::path::Path,
    open: &Open,
    token: Option<&str>,
) -> std::process::Command {
    use std::process::{Command, Stdio};
    let mut command = Command::new(exe);
    command
        .args(open.gui_args())
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
        assert_eq!(notices[1].open.uid, 2);
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
                open: Open {
                    account: Some("work".into()),
                    folder: "INBOX".into(),
                    uid: 4,
                },
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

    #[cfg(target_os = "linux")]
    #[test]
    fn a_click_opens_the_message_in_the_window_with_the_token() {
        let open = Open {
            account: Some("work".into()),
            folder: "INBOX/Team".into(),
            uid: 42,
        };
        let command = window_command(std::path::Path::new("/usr/bin/postvak"), &open, Some("tok"));
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
        let command = window_command(std::path::Path::new("/usr/bin/postvak"), &open, None);
        let envs: Vec<_> = command.get_envs().collect();
        assert!(envs.contains(&("XDG_ACTIVATION_TOKEN".as_ref(), None)));
    }
}
