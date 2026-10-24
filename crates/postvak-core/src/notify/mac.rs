//! Notifications on macOS through `NSUserNotificationCenter` (`mac-notification-sys`), posted as Postvak when the app
//! is installed where Launch Services finds it. A click arrives on the main thread's run loop, which the daemon runs
//! between accepts (`daemon::wait_for_clients`), and wakes the thread that waits on that notification.
use std::sync::Arc;

use mac_notification_sys::{Notification, NotificationResponse};

use super::{Notice, Waiting, open_in_window};
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
    options.wait_for_click(wait);
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
