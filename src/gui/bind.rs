//! State into the window: `present` sets the window's properties and models from `&App`. The window's callbacks
//! become `UiAction`s in `mod.rs`; nothing here changes state.
use slint::ComponentHandle;

use crate::config;

use super::list;
use super::state::{App, Focus};
use super::ui::{MailWindow, Pane, Theme, ThemePreference};

/// Shows `app` in `window`.
pub(crate) fn present(app: &App, window: &MailWindow) {
    window
        .global::<Theme>()
        .set_preference(preference(app.theme));
    window.set_config_changed(app.config_changed);
    window.set_focused_pane(pane(app.focus));
    window.set_mail_view(app.view.lists_mail());
    window.set_list_min_width(list::MIN_WIDTH);
}

fn preference(theme: config::Theme) -> ThemePreference {
    match theme {
        config::Theme::Dark => ThemePreference::Dark,
        config::Theme::Light => ThemePreference::Light,
        config::Theme::System => ThemePreference::System,
    }
}

fn pane(focus: Focus) -> Pane {
    match focus {
        Focus::Body => Pane::Body,
        Focus::Folders => Pane::Folders,
        Focus::List => Pane::List,
    }
}
