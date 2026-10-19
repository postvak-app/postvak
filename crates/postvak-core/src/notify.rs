//! Desktop notifications for new mail, sent by the daemon. On Linux a click on one opens the message in the window.
#[cfg(target_os = "linux")]
mod dbus;

use crate::message::{clean, display_name};

/// The new message a notification is about.
#[derive(Clone, Debug, PartialEq)]
pub struct Mail {
    pub account: String,
    pub folder: String,
    pub uid: u32,
    pub from: String,
    pub subject: String,
}

/// What one notification says, and the message a click on it opens.
#[derive(Clone, Debug, PartialEq)]
pub struct Notice {
    /// Plain text.
    pub title: String,
    /// Plain text; escaped where the server renders markup.
    pub body: String,
    pub account: String,
    pub folder: String,
    pub uid: u32,
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
        account: mail.account.clone(),
        folder: mail.folder.clone(),
        uid: mail.uid,
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

/// Sends the notifications. On Linux it keeps one session bus connection, which also hears which ones were clicked.
pub struct Notifier {
    #[cfg(target_os = "linux")]
    bus: dbus::Bus,
}

impl Notifier {
    pub fn new() -> Notifier {
        Notifier {
            #[cfg(target_os = "linux")]
            bus: dbus::Bus::new(Box::new(open_in_window)),
        }
    }

    /// Notifies one account's new mail from one sync pass.
    pub fn new_mail(&mut self, batch: &[Mail]) {
        for notice in notices(batch) {
            #[cfg(target_os = "linux")]
            self.bus.notify(&notice);
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
    match window_command(&exe, notice, token).spawn() {
        // Waited for, so it does not linger as a zombie of a daemon that runs for days.
        Ok(mut child) => {
            let _ = std::thread::Builder::new()
                .name("open-window".into())
                .spawn(move || child.wait());
        }
        Err(e) => log::warn!("could not open the message: {e}"),
    }
}

/// `postvak gui --open <uid> --account <account> --folder <folder>`, with the token where Wayland (`XDG_ACTIVATION_TOKEN`)
/// and X11 (`DESKTOP_STARTUP_ID`) look for it.
#[cfg(target_os = "linux")]
fn window_command(
    exe: &std::path::Path,
    notice: &Notice,
    token: Option<&str>,
) -> std::process::Command {
    use std::process::{Command, Stdio};
    let mut command = Command::new(exe);
    command
        .arg("gui")
        .arg("--open")
        .arg(notice.uid.to_string())
        .arg("--account")
        .arg(&notice.account)
        .arg("--folder")
        .arg(&notice.folder)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for var in ["XDG_ACTIVATION_TOKEN", "DESKTOP_STARTUP_ID"] {
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
        assert_eq!(notices[1].uid, 2);
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
                account: "work".into(),
                folder: "INBOX".into(),
                uid: 4,
            }]
        );
        let many: Vec<_> = (1..=6)
            .map(|uid| mail(uid, &format!("S{uid} <s{uid}@x>")))
            .collect();
        assert_eq!(notices(&many)[0].body, "From S6, S5, S4 and 3 others");
        let one_sender: Vec<_> = (1..=5).map(|uid| mail(uid, "Alice <a@x>")).collect();
        assert_eq!(notices(&one_sender)[0].body, "From Alice");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_click_opens_the_message_in_the_window_with_the_token() {
        let mail = Notice {
            title: String::new(),
            body: String::new(),
            account: "work".into(),
            folder: "INBOX/Team".into(),
            uid: 42,
        };
        let command = window_command(std::path::Path::new("/usr/bin/postvak"), &mail, Some("tok"));
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
        let command = window_command(std::path::Path::new("/usr/bin/postvak"), &mail, None);
        let envs: Vec<_> = command.get_envs().collect();
        assert!(envs.contains(&("XDG_ACTIVATION_TOKEN".as_ref(), None)));
    }
}
