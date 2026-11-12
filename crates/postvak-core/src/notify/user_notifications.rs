/*! Notifications from Postvak.app through `UNUserNotificationCenter`, posted as Postvak with its icon and listed under
System Settings › Notifications › Postvak.

Each notification carries the `postvak gui …` arguments that show what it is about, so a click opens it even after the
daemon that posted it restarted. Clicks reach the [`Delegate`] on the main thread's run loop, which the daemon runs while
it waits for clients (`daemon::wait_for_client`). */
#![expect(
    unsafe_code,
    reason = "objc2 declares an Objective-C class, as the delegate must be, only in unsafe code; `setUserInfo` is unsafe too"
)]

use block2::{DynBlock, RcBlock};
use objc2::rc::Retained;
use objc2::runtime::{Bool, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{AllocAnyThread, define_class, msg_send};
use objc2_foundation::{NSDictionary, NSError, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNMutableNotificationContent, UNNotification,
    UNNotificationPresentationOptions, UNNotificationRequest, UNNotificationResponse,
    UNNotificationSound, UNUserNotificationCenter, UNUserNotificationCenterDelegate,
};

use super::{Notice, Show, run_window};
use crate::paths::APP_ID;

/// The `userInfo` key under which a notification keeps its `gui` arguments, as a JSON array.
const ARGS_KEY: &str = "postvak-gui-args";

/// Whether this process is Postvak.app: `UNUserNotificationCenter` raises an Objective-C exception, which aborts, in a
/// process that is not an app, such as the Homebrew formula's binary.
pub(super) fn available() -> bool {
    crate::paths::runs_as_app()
}

pub(super) struct Center {
    center: Retained<UNUserNotificationCenter>,
    /// The center keeps only a weak reference to it.
    _delegate: Retained<Delegate>,
}

impl Center {
    /// The app's notification center, asking the user to allow notifications the first time.
    pub(super) fn new() -> Center {
        let center = UNUserNotificationCenter::currentNotificationCenter();
        let delegate = Delegate::new();
        center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        let answered = RcBlock::new(|granted: Bool, _error: *mut NSError| {
            if !granted.as_bool() {
                log::info!("notifications are off in System Settings › Notifications › Postvak");
            }
        });
        let options = UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound;
        center.requestAuthorizationWithOptions_completionHandler(options, &answered);
        Center {
            center,
            _delegate: delegate,
        }
    }

    pub(super) fn notify(&self, notice: &Notice) {
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(&notice.title));
        content.setBody(&NSString::from_str(&notice.body));
        // New mail plays the default sound, as on Linux; proposals stay quiet.
        if matches!(notice.show, Show::Message(_)) {
            content.setSound(Some(&UNNotificationSound::defaultSound()));
        }
        if let Ok(args) = serde_json::to_string(&notice.show.gui_args()) {
            let info = NSDictionary::from_slices(
                &[&*NSString::from_str(ARGS_KEY)],
                &[&*NSString::from_str(&args)],
            );
            // SAFETY: userInfo must hold property-list objects, and this holds one string under a string key.
            unsafe { content.setUserInfo(Retained::cast_unchecked::<NSDictionary>(info).as_ref()) };
        }
        let id = NSString::from_str(&unique_id());
        let request =
            UNNotificationRequest::requestWithIdentifier_content_trigger(&id, &content, None);
        // The notice is not logged: its title and body come from mail.
        let posted = RcBlock::new(|error: *mut NSError| {
            if !error.is_null() {
                log::warn!("macOS did not post a notification");
            }
        });
        self.center
            .addNotificationRequest_withCompletionHandler(&request, Some(&posted));
    }
}

/// An identifier no other notification has, also after the daemon restarts; the same one would replace a notification.
fn unique_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(
        "{APP_ID}.{}.{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// The `gui` arguments a notification's `userInfo` carries.
fn gui_args(info: &NSDictionary) -> Option<Vec<String>> {
    let key = NSString::from_str(ARGS_KEY);
    let value = info.objectForKey(&key)?;
    let json = value.downcast::<NSString>().ok()?.to_string();
    parse_gui_args(&json)
}

/// Only `gui` arguments: the delegate runs this program with them.
fn parse_gui_args(json: &str) -> Option<Vec<String>> {
    let args: Vec<String> = serde_json::from_str(json).ok()?;
    (args.first().map(String::as_str) == Some("gui")).then_some(args)
}

define_class!(
    #[unsafe(super = NSObject)]
    #[name = "PostvakNotificationDelegate"]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl UNUserNotificationCenterDelegate for Delegate {
        /// Shows a notification also while Postvak's window is in front, where macOS would otherwise hold it back.
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn will_present(
            &self,
            _center: &UNUserNotificationCenter,
            _notification: &UNNotification,
            done: &DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            done.call((UNNotificationPresentationOptions::Banner
                | UNNotificationPresentationOptions::List
                | UNNotificationPresentationOptions::Sound,));
        }

        /// A click: shows what the notification is about in the window.
        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        fn did_receive(
            &self,
            _center: &UNUserNotificationCenter,
            response: &UNNotificationResponse,
            done: &DynBlock<dyn Fn()>,
        ) {
            let info = response.notification().request().content().userInfo();
            // A notification without arguments still brings the window to the front.
            let args = gui_args(&info).unwrap_or_else(|| vec!["gui".to_string()]);
            run_window(&args, None);
            done.call(());
        }
    }
);

impl Delegate {
    fn new() -> Retained<Self> {
        let this = Self::alloc().set_ivars(());
        // SAFETY: NSObject's `init` on a freshly allocated object.
        unsafe { msg_send![super(this), init] }
    }
}

#[cfg(test)]
mod tests {
    use super::{available, parse_gui_args};

    #[test]
    fn a_program_that_is_not_the_app_keeps_away_from_the_center() {
        assert!(!available());
    }

    #[test]
    fn a_click_runs_only_gui_arguments() {
        let open = r#"["gui","--open","4","--folder","INBOX"]"#;
        assert_eq!(
            parse_gui_args(open),
            Some(vec![
                "gui".into(),
                "--open".into(),
                "4".into(),
                "--folder".into(),
                "INBOX".into()
            ])
        );
        assert_eq!(parse_gui_args(r#"["daemon","stop"]"#), None);
        assert_eq!(parse_gui_args("[]"), None);
        assert_eq!(parse_gui_args("not json"), None);
    }
}
