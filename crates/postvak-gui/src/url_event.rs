/*! The `mailto:` links macOS hands Postvak.app once it is the mail app.

They come as a Get URL Apple Event, never as an argument, also to an app already running; winit does not pass them on. */
// The handler is an Objective-C class, which objc2 declares only in unsafe code, and the Apple Event calls are untyped.
#![allow(unsafe_code)]

/// Opens new mail for each `mailto:` link macOS sends from now on; call on the main thread before the event loop runs.
#[cfg(target_os = "macos")]
pub(crate) fn listen() {
    mac::listen();
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn listen() {}

#[cfg(target_os = "macos")]
mod mac {
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject, NSObject};
    use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
    use objc2_foundation::NSString;

    use postvak_core::notify::Show;

    /// `kInternetEventClass` and `kAEGetURL` are both 'GURL'.
    const GET_URL: u32 = u32::from_be_bytes(*b"GURL");
    /// `keyDirectObject`, where the event carries the link.
    const DIRECT_OBJECT: u32 = u32::from_be_bytes(*b"----");

    define_class!(
        #[unsafe(super = NSObject)]
        #[thread_kind = MainThreadOnly]
        #[name = "PostvakUrlHandler"]
        struct Handler;

        impl Handler {
            #[unsafe(method(handleGetURL:withReplyEvent:))]
            fn handle_get_url(&self, event: &AnyObject, _reply: &AnyObject) {
                // SAFETY: `event` is the NSAppleEventDescriptor the manager hands its handler.
                let link: Option<Retained<AnyObject>> =
                    unsafe { msg_send![event, paramDescriptorForKeyword: DIRECT_OBJECT] };
                // SAFETY: `stringValue` on an NSAppleEventDescriptor.
                let url: Option<Retained<NSString>> =
                    link.and_then(|link| unsafe { msg_send![&link, stringValue] });
                match url.map(|url| url.to_string()) {
                    Some(url) if postvak_core::send::mailto::parse(&url).is_some() => {
                        log::info!("macOS handed over a mailto: link");
                        crate::hand_over(crate::instance::Request {
                            show: Some(Show::NewMail(url)),
                        });
                    }
                    _ => log::debug!("macOS opened a link that is no mailto: link; ignored"),
                }
            }
        }
    );

    thread_local! {
        // The manager does not retain its handler.
        static HANDLER: std::cell::OnceCell<Retained<Handler>> = const { std::cell::OnceCell::new() };
    }

    pub(super) fn listen() {
        let Some(mtm) = MainThreadMarker::new() else {
            log::warn!("mailto: links are listened for only on the main thread");
            return;
        };
        let Some(class) = AnyClass::get(c"NSAppleEventManager") else {
            log::warn!("no NSAppleEventManager; mailto: links will not open new mail");
            return;
        };
        let handler = HANDLER.with(|cell| {
            cell.get_or_init(|| {
                let this = Handler::alloc(mtm).set_ivars(());
                // SAFETY: NSObject's `init` on a freshly allocated object.
                unsafe { msg_send![super(this), init] }
            })
            .clone()
        });
        // SAFETY: the shared manager, and a handler that answers the selector with the signature it calls.
        unsafe {
            let manager: Retained<AnyObject> = msg_send![class, sharedAppleEventManager];
            let () = msg_send![
                &manager,
                setEventHandler: &*handler,
                andSelector: sel!(handleGetURL:withReplyEvent:),
                forEventClass: GET_URL,
                andEventID: GET_URL
            ];
        }
    }
}
