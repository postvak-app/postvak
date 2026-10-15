//! The toolbar without a toolkit: its action buttons, their names and hints, and when they are enabled. The window
//! draws them beside the wordmark, with the search button on the right.
use crate::rules::Action;

use super::icons;
use super::state::{App, UiAction, View};

/// An icon button: its glyph, its accessible name, its hover hint and what a click does.
pub(crate) struct Button {
    pub icon: &'static str,
    pub name: &'static str,
    pub hint: &'static str,
    pub action: UiAction,
}

/// The hover hint of the search button.
pub(crate) const SEARCH_HINT: &str = "Search · /";

/// The message actions, left to right. In the Archive folder the archive button moves back to the inbox, and in the
/// Junk folder the junk button reports not junk.
pub(crate) fn buttons(app: &App) -> [Button; 6] {
    let in_archive =
        matches!(&app.view, View::Folder { account, folder } if app.is_archive(*account, folder));
    let (icon, name, hint) = if in_archive {
        (icons::INBOX, "Unarchive message", "Move to inbox · e")
    } else {
        (icons::ARCHIVE, "Archive message", "Archive · e")
    };
    let button = |icon, name, hint, action| Button {
        icon,
        name,
        hint,
        action,
    };
    [
        button(icon, name, hint, UiAction::Act(Action::Archive)),
        button(
            icons::MOVE,
            "Move message",
            "Move · m",
            UiAction::OpenMovePicker,
        ),
        button(
            icons::TRASH,
            "Delete message",
            "Delete · #",
            UiAction::Act(Action::Trash),
        ),
        junk_button(app),
        button(
            icons::FLAG,
            "Flag message",
            "Flag · s",
            UiAction::ToggleFlag,
        ),
        button(
            icons::MARK_UNREAD,
            "Mark read or unread",
            "Read or unread · u",
            UiAction::ToggleRead,
        ),
    ]
}

fn junk_button(app: &App) -> Button {
    let (icon, name, hint) = if app.shows_junk() {
        (icons::INBOX, "Not junk message", "Not junk · !")
    } else {
        (icons::JUNK, "Junk message", "Junk · !")
    };
    Button {
        icon,
        name,
        hint,
        action: UiAction::Act(Action::Junk),
    }
}

/// The actions need a row, and a closed move picker: an action would move the cursor, and the picker would then
/// move a different message.
pub(crate) fn enabled(app: &App) -> bool {
    app.view.lists_mail() && !app.list.rows.is_empty() && app.move_picker.is_none()
}

#[cfg(test)]
mod tests {
    use crate::gui::state::{UiAction, View};
    use crate::gui::test_support::{Fixture, Open, command, message};
    use crate::rules::Action;
    use crate::sync::Command;

    fn enabled(open: &Open, label: &str) -> bool {
        open.get(label).accessible_enabled().unwrap()
    }

    fn apply(folder: &str, action: Action) -> (String, Command) {
        command("work", folder, &[1], action)
    }

    #[test]
    fn the_actions_are_enabled_on_a_row_and_disabled_outside_a_folder() {
        let fx = Fixture::new(&["work"]);
        fx.add("work", message("INBOX", 1, "hello"));
        let (open, _wires) = fx.window();
        for name in [
            "Archive message",
            "Move message",
            "Delete message",
            "Flag message",
            "Mark read or unread",
            "Search",
        ] {
            assert!(enabled(&open, name), "{name}");
        }
        open.with(|live| live.app.apply(UiAction::SelectView(View::Rules)));
        for name in [
            "Archive message",
            "Move message",
            "Delete message",
            "Flag message",
            "Mark read or unread",
            "Search",
        ] {
            assert!(!enabled(&open, name), "{name}");
        }
    }

    #[test]
    fn the_actions_are_disabled_while_the_move_picker_is_open() {
        let fx = Fixture::new(&["work"]);
        fx.folder("work", "Receipts", None);
        fx.add("work", message("INBOX", 1, "hello"));
        let (open, _wires) = fx.window();
        open.press("m");
        assert!(open.with(|live| live.app.move_picker.is_some()));
        assert!(!enabled(&open, "Archive message"));
    }

    #[test]
    fn the_archive_button_archives_the_cursor_row() {
        let fx = Fixture::new(&["work"]);
        fx.add("work", message("INBOX", 1, "hello"));
        let (open, wires) = fx.window();
        open.click("Archive message");
        assert_eq!(wires.sent(), [apply("INBOX", Action::Archive)]);
        assert!(open.with(|live| live.app.list.rows.is_empty()));
    }

    #[test]
    fn in_the_archive_the_archive_button_moves_the_row_back_to_the_inbox() {
        let fx = Fixture::new(&["work"]);
        fx.folder("work", "Archive", Some("Archive"));
        fx.add("work", message("Archive", 1, "hello"));
        let (open, wires) = fx.window();
        open.click("Archive");
        assert!(open.find("Archive message").is_empty());
        open.click("Unarchive message");
        assert_eq!(
            wires.sent(),
            [apply("Archive", Action::Move("INBOX".into()))]
        );
    }

    #[test]
    fn the_junk_button_reports_junk_and_in_the_junk_folder_not_junk() {
        let fx = Fixture::new(&["work"]);
        fx.folder("work", "Spam", Some("Junk"));
        fx.add("work", message("INBOX", 1, "hello"));
        fx.add("work", message("Spam", 2, "offer"));
        let (open, wires) = fx.window();
        open.click("Junk message");
        open.click("Spam");
        assert!(open.find("Junk message").is_empty());
        open.click("Not junk message");
        assert_eq!(
            wires.sent(),
            [
                apply("INBOX", Action::Junk),
                command("work", "Spam", &[2], Action::NotJunk)
            ]
        );
    }

    #[test]
    fn the_search_button_opens_search() {
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = fx.window();
        open.click("Search");
        assert_eq!(
            open.with(|live| live.app.search.clone()),
            Some(String::new())
        );
    }
}
