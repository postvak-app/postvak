//! Notifications on macOS through `NSUserNotificationCenter` (`mac-notification-sys`), posted as Postvak when the app
//! is installed where Launch Services finds it. A click arrives on the main thread's run loop, which the daemon runs
//! while it waits for clients (`daemon::wait_for_client`), and wakes the thread that waits on that notification.
use std::sync::Arc;

use mac_notification_sys::{Notification, NotificationResponse, Sound};

use super::{Notice, Show, Waiting, open_in_window};
use crate::paths::APP_ID;

pub(super) struct Center {
    waiting: Arc<Waiting>,
}

impl Center {
    pub(super) fn new() -> Center {
        // Without the app installed, macOS posts them under its default sender.
        if let Err(e) = mac_notification_sys::set_application(APP_ID) {
            log::debug!("notifications not posted as Postvak: {e}");
        }
        Center {
            waiting: Arc::new(Waiting::default()),
        }
    }

    /// Posts `notice`; a click opens its message, unless too many already wait for one.
    pub(super) fn notify(&self, notice: &Notice) {
        let Some(slot) = Waiting::take(&self.waiting) else {
            post(notice, false);
            return;
        };
        let notice = notice.clone();
        let spawned = std::thread::Builder::new()
            .name("notification".into())
            .spawn(move || {
                let _slot = slot;
                if post(&notice, true) {
                    open_in_window(&notice, None);
                }
            });
        if let Err(e) = spawned {
            log::warn!("could not post a notification: {e}");
        }
    }
}

/// Posts `notice` and, with `wait`, blocks until it is clicked or dismissed; whether it was clicked.
fn post(notice: &Notice, wait: bool) -> bool {
    let mut options = Notification::new();
    options
        .wait_for_click(wait)
        .maybe_sound(sound(&notice.show));
    match mac_notification_sys::send_notification(&notice.title, None, &notice.body, Some(&options))
    {
        Ok(NotificationResponse::Click) => true,
        Ok(_) => false,
        Err(e) => {
            log::warn!("could not post a notification: {e}");
            false
        }
    }
}

/// The system's default sound for new mail, as on Linux, where the desktop plays its sound theme's; macOS leaves a
/// notification without one silent. Proposals stay quiet. System Settings › Notifications › Postvak turns it off.
fn sound(show: &Show) -> Option<Sound> {
    matches!(show, Show::Message(_)).then_some(Sound::Default)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notify::Open;

    #[test]
    fn new_mail_plays_the_default_sound_and_a_proposal_none() {
        let open = Open {
            account: Some("work".into()),
            folder: "INBOX".into(),
            uid: 4,
        };
        assert!(matches!(sound(&Show::Message(open)), Some(Sound::Default)));
        assert!(sound(&Show::Rules).is_none());
    }
}
