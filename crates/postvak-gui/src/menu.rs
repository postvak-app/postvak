//! The menu bar: what each of the window's menu items does and when it is enabled, as the toolbar's buttons are. The
//! window lays the menus out; Quit and Close window are the window's own.
use postvak_core::config::Theme;
use postvak_core::rules::Action;

use super::state::{App, UiAction, View};
use super::toolbar;
use super::ui::MenuAction;

/// Where Help ▸ Postvak documentation goes.
pub(crate) const DOCS: &str = env!("CARGO_PKG_HOMEPAGE");

/// What `item` does now; None while it would do nothing, which disables it.
pub(crate) fn action(item: MenuAction, app: &App) -> Option<UiAction> {
    let message = toolbar::enabled(app);
    let downloaded = app
        .body
        .as_ref()
        .and_then(|b| b.message.as_ref())
        .is_some_and(|m| m.body_text.is_some());
    let act = |on: bool, action: UiAction| on.then_some(action);
    match item {
        MenuAction::About => Some(UiAction::About),
        MenuAction::AddAccount => Some(UiAction::OpenAccountForm),
        MenuAction::AddToSelection => act(message, UiAction::ToggleMark),
        MenuAction::Archive => act(message, UiAction::Act(Action::Archive)),
        MenuAction::Delete => act(message, UiAction::Act(Action::Trash)),
        MenuAction::Documentation => Some(UiAction::OpenUrl(DOCS.into())),
        MenuAction::Flag => act(message, UiAction::ToggleFlag),
        MenuAction::History => Some(UiAction::ToggleHistory),
        MenuAction::Junk => act(message, UiAction::Act(Action::Junk)),
        MenuAction::Keys => Some(UiAction::ToggleHelp),
        MenuAction::MarkFolderRead => mark_folder_read(app),
        MenuAction::Move => act(message, UiAction::OpenMovePicker),
        MenuAction::ReadOrUnread => act(message, UiAction::ToggleRead),
        MenuAction::SaveEml => act(downloaded, UiAction::SaveEml),
        MenuAction::Search => act(app.view.lists_mail(), UiAction::StartSearch),
        MenuAction::SelectAll => act(message, UiAction::SelectAll),
        MenuAction::SyncNow => Some(UiAction::SyncNow),
        MenuAction::TextOrHtml => act(
            app.body.as_ref().is_some_and(|b| b.html.is_some()),
            UiAction::ToggleHtml,
        ),
        MenuAction::ThemeDark => Some(UiAction::SetTheme(Theme::Dark)),
        MenuAction::ThemeLight => Some(UiAction::SetTheme(Theme::Light)),
        MenuAction::ThemeSystem => Some(UiAction::SetTheme(Theme::System)),
        MenuAction::ViewSource => act(downloaded, UiAction::OpenSource),
    }
}

/// Mailbox ▸ Mark folder read: the shown folder, while it has unread mail.
fn mark_folder_read(app: &App) -> Option<UiAction> {
    let View::Folder { account, folder } = &app.view else {
        return None;
    };
    let unread = app.accounts[*account]
        .folders
        .iter()
        .find(|f| &f.name == folder)?
        .unread;
    (unread > 0).then(|| UiAction::MarkFolderRead(*account, folder.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Fixture, launched, message, unread};
    use postvak_core::sync::Command;

    #[test]
    fn every_menu_item_has_its_action_on_a_downloaded_unread_message() {
        let fx = Fixture::new(&["work"]);
        let unread = unread("INBOX", 1, "hello");
        fx.add("work", unread);
        crate::test_support::add_html(&fx, 2, "<p>rich</p>", "rich");
        let (mut driver, _wires) = fx.driver();
        driver.press("j");
        let inbox = || UiAction::MarkFolderRead(0, "INBOX".into());
        let expected = [
            Some(UiAction::About),
            Some(UiAction::OpenAccountForm),
            Some(UiAction::ToggleMark),
            Some(UiAction::Act(Action::Archive)),
            Some(UiAction::Act(Action::Trash)),
            Some(UiAction::OpenUrl(DOCS.into())),
            Some(UiAction::ToggleFlag),
            Some(UiAction::ToggleHistory),
            Some(UiAction::Act(Action::Junk)),
            Some(UiAction::ToggleHelp),
            Some(inbox()),
            Some(UiAction::OpenMovePicker),
            Some(UiAction::ToggleRead),
            Some(UiAction::SaveEml),
            Some(UiAction::StartSearch),
            Some(UiAction::SelectAll),
            Some(UiAction::SyncNow),
            None,
            Some(UiAction::SetTheme(Theme::Dark)),
            Some(UiAction::SetTheme(Theme::Light)),
            Some(UiAction::SetTheme(Theme::System)),
            Some(UiAction::OpenSource),
        ];
        let items = [
            MenuAction::About,
            MenuAction::AddAccount,
            MenuAction::AddToSelection,
            MenuAction::Archive,
            MenuAction::Delete,
            MenuAction::Documentation,
            MenuAction::Flag,
            MenuAction::History,
            MenuAction::Junk,
            MenuAction::Keys,
            MenuAction::MarkFolderRead,
            MenuAction::Move,
            MenuAction::ReadOrUnread,
            MenuAction::SaveEml,
            MenuAction::Search,
            MenuAction::SelectAll,
            MenuAction::SyncNow,
            MenuAction::TextOrHtml,
            MenuAction::ThemeDark,
            MenuAction::ThemeLight,
            MenuAction::ThemeSystem,
            MenuAction::ViewSource,
        ];
        for (item, expected) in items.into_iter().zip(expected) {
            assert_eq!(action(item, &driver.app), expected, "{item:?}");
        }
        driver.press("k");
        let html = action(MenuAction::TextOrHtml, &driver.app);
        assert_eq!(
            html,
            Some(UiAction::ToggleHtml),
            "the newer message has HTML"
        );
    }

    #[test]
    fn message_items_are_disabled_outside_a_folder() {
        let fx = Fixture::new(&["work"]);
        fx.add("work", message("INBOX", 1, "hello"));
        let (mut driver, _wires) = fx.driver();
        driver.apply(UiAction::SelectView(View::Rules));
        for item in [
            MenuAction::Archive,
            MenuAction::Delete,
            MenuAction::Flag,
            MenuAction::Junk,
            MenuAction::Move,
            MenuAction::ReadOrUnread,
            MenuAction::AddToSelection,
            MenuAction::SelectAll,
            MenuAction::Search,
            MenuAction::MarkFolderRead,
        ] {
            assert_eq!(action(item, &driver.app), None, "{item:?}");
        }
        assert_eq!(
            action(MenuAction::SyncNow, &driver.app),
            Some(UiAction::SyncNow)
        );
    }

    #[test]
    fn a_menu_item_acts_in_the_window() {
        let fx = Fixture::new(&["work"]);
        fx.add("work", message("INBOX", 1, "hello"));
        let (open, wires) = fx.window();
        open.window().invoke_menu_item(MenuAction::Archive);
        let archive = Command::Apply {
            folder: "INBOX".into(),
            uids: vec![1],
            action: Action::Archive,
            by: "gui".into(),
        };
        assert_eq!(wires.sent(), [("work".to_string(), archive)]);
        open.window().invoke_menu_item(MenuAction::Documentation);
        assert_eq!(launched(), [DOCS]);
    }

    #[test]
    fn about_shows_the_version_and_the_slint_attribution() {
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = fx.window();
        open.window().invoke_menu_item(MenuAction::About);
        assert_eq!(open.find("About Postvak").len(), 1);
        assert_eq!(
            open.find(&format!("Postvak v{}", crate::status::VERSION))
                .len(),
            1
        );
        assert_eq!(open.find("#MadeWithSlint").len(), 1);
        open.click("Project");
        assert_eq!(launched(), [env!("CARGO_PKG_REPOSITORY")]);
        open.press("escape");
        assert!(open.find("About Postvak").is_empty());
    }
}
