/*! Makes the macOS menu bar glyph a template image, so the menu bar tints it for its appearance and highlight.

Slint has no property for this (https://github.com/slint-ui/slint/issues/12330), so the image is marked on the status
item's button once Slint has made it on the event loop. */

/// Marks the tray's glyph as a template once Slint has put it in the menu bar.
#[cfg(target_os = "macos")]
pub(crate) fn mark_template() {
    use std::time::Duration;

    // Slint makes the status item on one of the event loop's first turns; give up after five seconds.
    let mut tries = 0;
    RETRY.with(|timer| {
        timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(50),
            move || {
                tries += 1;
                let found = marked();
                if !found && tries >= 100 {
                    log::debug!("no menu bar button found; its glyph stays untinted");
                }
                if found || tries >= 100 {
                    RETRY.with(slint::Timer::stop);
                }
            },
        );
    });
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn mark_template() {}

#[cfg(target_os = "macos")]
thread_local! {
    static RETRY: slint::Timer = slint::Timer::default();
}

/// Whether a status bar button with an image was found, and so marked; Slint's is the app's only one.
#[cfg(target_os = "macos")]
fn marked() -> bool {
    let Some(mtm) = objc2::MainThreadMarker::new() else {
        return false;
    };
    let mut found = false;
    for window in objc2_app_kit::NSApplication::sharedApplication(mtm).windows() {
        if let Some(view) = window.contentView() {
            found |= mark(&view);
        }
    }
    found
}

#[cfg(target_os = "macos")]
fn mark(view: &objc2_app_kit::NSView) -> bool {
    if let Some(button) = view.downcast_ref::<objc2_app_kit::NSStatusBarButton>()
        && let Some(image) = button.image()
    {
        image.setTemplate(true);
        objc2_app_kit::NSView::setNeedsDisplay(button, true);
        return true;
    }
    let mut found = false;
    for subview in view.subviews() {
        found |= mark(&subview);
    }
    found
}
