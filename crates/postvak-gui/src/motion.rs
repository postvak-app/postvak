//! Keeps `Theme.reduce-motion` equal to the desktop's wish for less motion (`postvak_core::motion`), which stops the
//! window's fades and pulses. On Linux a thread follows the Settings portal; on macOS the setting is read again each
//! time the window gains focus.
use slint::ComponentHandle;

use crate::ui;

/// Follows the desktop's setting in `window`'s theme for as long as the window lasts.
#[cfg(target_os = "linux")]
pub(crate) fn follow(window: &ui::MailWindow) {
    let weak = window.as_weak();
    crate::worker::spawn("motion", move || {
        let result = postvak_core::motion::follow(|reduced| {
            let _ = weak.upgrade_in_event_loop(move |window| set(&window, reduced));
        });
        // Without the portal the window keeps its motion, as on a desktop that never asks for less.
        if let Err(e) = result {
            log::debug!("not following the desktop's reduced motion: {e:#}");
        }
    });
}

#[cfg(target_os = "macos")]
thread_local! {
    /// The window whose theme follows the setting; only the window's own thread reads it.
    static WINDOW: std::cell::RefCell<Option<slint::Weak<ui::MailWindow>>> = const { std::cell::RefCell::new(None) };
}

/// Follows the desktop's setting in `window`'s theme for as long as the window lasts.
#[cfg(target_os = "macos")]
pub(crate) fn follow(window: &ui::MailWindow) {
    set(window, postvak_core::motion::reduced());
    WINDOW.with(|cell| *cell.borrow_mut() = Some(window.as_weak()));
}

/// Reads the setting again as the window gains focus.
#[cfg(target_os = "macos")]
pub(crate) fn on_event(event: &slint::winit_030::winit::event::WindowEvent) {
    if !matches!(
        event,
        slint::winit_030::winit::event::WindowEvent::Focused(true)
    ) {
        return;
    }
    let window = WINDOW.with(|cell| cell.borrow().as_ref().and_then(slint::Weak::upgrade));
    if let Some(window) = window {
        set(&window, postvak_core::motion::reduced());
    }
}

/// Nothing to follow where Postvak does not run.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn follow(_window: &ui::MailWindow) {}

fn set(window: &ui::MailWindow, reduced: bool) {
    window.global::<ui::Theme>().set_reduce_motion(reduced);
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use slint::ComponentHandle;

    use crate::ui::{MailWindow, Theme};

    #[test]
    fn less_motion_makes_every_fade_instant_and_stills_the_pulse() {
        crate::test_support::headless();
        let window = MailWindow::new().unwrap();
        let theme = window.global::<Theme>();
        let fades = |theme: &Theme| {
            [
                theme.get_hover_fade(),
                theme.get_state_fade(),
                theme.get_dialog_fade(),
                theme.get_arrive_fade(),
                theme.get_progress_glide(),
            ]
        };
        assert!(fades(&theme).iter().all(|fade| *fade > 0));
        super::set(&window, true);
        assert!(fades(&theme).iter().all(|fade| *fade == 0));
        let still = theme.get_pulse();
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(500));
        assert_eq!(theme.get_pulse(), still);
        super::set(&window, false);
        assert!(fades(&theme).iter().all(|fade| *fade > 0));
    }
}
