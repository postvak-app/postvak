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

    pub fn new_mail(&mut self, mail: &Mail) {
        #[cfg(target_os = "linux")]
        self.bus.notify(mail);
        #[cfg(target_os = "macos")]
        {
            let _ = notify_rust::Notification::new()
                .summary(&title(&mail.from))
                .body(&clean(&mail.subject, false))
                .appname("Postvak")
                .show();
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

/// Opens `mail` in the window: a running window shows it, else a new one starts. `token` is the notification server's
/// activation token, which lets the window come to the front.
#[cfg(target_os = "linux")]
fn open_in_window(mail: &Mail, token: Option<&str>) {
    let exe = match crate::paths::stable_exe() {
        Ok(exe) => exe,
        Err(e) => {
            log::warn!("could not find this program to open the message: {e}");
            return;
        }
    };
    match window_command(&exe, mail, token).spawn() {
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
    mail: &Mail,
    token: Option<&str>,
) -> std::process::Command {
    use std::process::{Command, Stdio};
    let mut command = Command::new(exe);
    command
        .arg("gui")
        .arg("--open")
        .arg(mail.uid.to_string())
        .arg("--account")
        .arg(&mail.account)
        .arg("--folder")
        .arg(&mail.folder)
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

    #[cfg(target_os = "linux")]
    #[test]
    fn a_click_opens_the_message_in_the_window_with_the_token() {
        let mail = Mail {
            account: "work".into(),
            folder: "INBOX/Team".into(),
            uid: 42,
            from: String::new(),
            subject: String::new(),
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
