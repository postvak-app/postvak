//! The keymap: which key does what in which pane, and the `?` window's rows. Printable keys match on the typed text, so
//! they follow the keyboard layout; the window turns its key events into a `Press`.
use crate::rules::Action;

use super::state::{Focus, UiAction};

/// A key that types no text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Named {
    Backspace,
    Delete,
    Down,
    Escape,
    Left,
    Right,
    Tab,
    Up,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Key {
    /// The text the key typed, such as "j" or "#".
    Text(String),
    Named(Named),
}

/// A key pressed, with whether Ctrl (Cmd on macOS) or Alt was held.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Press {
    pub key: Key,
    /// Ctrl, or Cmd on macOS.
    pub command: bool,
    pub alt: bool,
}

impl Press {
    pub(crate) fn text(text: &str) -> Press {
        Press {
            key: Key::Text(text.into()),
            command: false,
            alt: false,
        }
    }

    pub(crate) fn named(named: Named) -> Press {
        Press {
            key: Key::Named(named),
            command: false,
            alt: false,
        }
    }

    fn is(&self, text: &str) -> bool {
        !self.command && !self.alt && self.key == Key::Text(text.into())
    }

    fn is_named(&self, named: Named) -> bool {
        !self.command && !self.alt && self.key == Key::Named(named)
    }
}

/// What `press` does with `focus` on the focused pane. While a text field has the keyboard (`typing`) only Esc and
/// Ctrl/Cmd+R count, so letters stay text.
pub(crate) fn action(press: &Press, focus: Focus, typing: bool) -> Option<UiAction> {
    if press.is_named(Named::Escape) {
        return Some(UiAction::Escape);
    }
    let lower_r = matches!(&press.key, Key::Text(t) if t.eq_ignore_ascii_case("r"));
    if press.command && !press.alt && lower_r {
        return Some(UiAction::SyncNow);
    }
    if typing {
        return None;
    }
    if press.is_named(Named::Tab) {
        return Some(UiAction::NextFocus);
    }
    if press.is("?") {
        return Some(UiAction::ToggleHelp);
    }
    if press.is("/") {
        return Some(UiAction::StartSearch);
    }
    let down = press.is("j") || press.is_named(Named::Down);
    let up = press.is("k") || press.is_named(Named::Up);
    match focus {
        Focus::Folders => {
            if down {
                return Some(UiAction::StepFolder(1));
            }
            if up {
                return Some(UiAction::StepFolder(-1));
            }
            None
        }
        Focus::Body | Focus::List => {
            if down {
                return Some(UiAction::MoveCursor(1));
            }
            if up {
                return Some(UiAction::MoveCursor(-1));
            }
            // macOS keyboards label Backspace "delete".
            let delete = press.is("#")
                || press.is_named(Named::Delete)
                || (cfg!(target_os = "macos") && press.is_named(Named::Backspace));
            if delete {
                return Some(UiAction::Act(Action::Trash));
            }
            let action = match &press.key {
                Key::Named(Named::Right) => UiAction::Expand,
                Key::Named(Named::Left) => UiAction::Collapse,
                Key::Text(t) if t == "x" => UiAction::ToggleMark,
                Key::Text(t) if t == "e" => UiAction::Act(Action::Archive),
                Key::Text(t) if t == "m" => UiAction::OpenMovePicker,
                Key::Text(t) if t == "s" => UiAction::ToggleFlag,
                Key::Text(t) if t == "u" => UiAction::ToggleRead,
                Key::Text(t) if t == "v" => UiAction::ToggleHtml,
                _ => return None,
            };
            (!press.command && !press.alt).then_some(action)
        }
    }
}

/// The `?` window. Keys are spelled out, so every glyph is in the bundled fonts.
pub(crate) const HELP: [(&str, &str); 14] = [
    ("j / k, Down / Up", "next or previous row"),
    ("Right / Left", "expand or collapse a thread"),
    ("x", "add the row to the selection, or take it out"),
    ("e", "archive"),
    (
        "#, Delete (Backspace on macOS)",
        "delete (to the Trash folder; from Trash or with no Trash folder, saved as .eml and deleted for good)",
    ),
    ("m", "move to a folder"),
    ("u", "mark read or unread"),
    ("s", "flag or unflag"),
    ("v", "show HTML mail as text, or as HTML again"),
    ("/", "search this account; in All inboxes, every account"),
    ("Esc", "close a popup, leave search, clear the selection"),
    ("Tab", "next pane: folders, list, body"),
    ("Ctrl+R (Cmd+R on macOS)", "sync every account now"),
    ("?", "show these keys"),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn on_list(press: Press) -> Option<UiAction> {
        action(&press, Focus::List, false)
    }

    #[test]
    fn j_and_k_and_the_arrows_move_the_list_and_step_the_folders() {
        assert_eq!(on_list(Press::text("j")), Some(UiAction::MoveCursor(1)));
        assert_eq!(
            on_list(Press::named(Named::Up)),
            Some(UiAction::MoveCursor(-1))
        );
        let folders = |press| action(&press, Focus::Folders, false);
        assert_eq!(folders(Press::text("k")), Some(UiAction::StepFolder(-1)));
        assert_eq!(
            folders(Press::named(Named::Down)),
            Some(UiAction::StepFolder(1))
        );
        assert_eq!(folders(Press::text("e")), None, "letters act on mail only");
    }

    #[test]
    fn the_letters_act_on_the_cursor_row() {
        let cases = [
            ("x", UiAction::ToggleMark),
            ("e", UiAction::Act(Action::Archive)),
            ("#", UiAction::Act(Action::Trash)),
            ("m", UiAction::OpenMovePicker),
            ("s", UiAction::ToggleFlag),
            ("u", UiAction::ToggleRead),
            ("v", UiAction::ToggleHtml),
            ("?", UiAction::ToggleHelp),
            ("/", UiAction::StartSearch),
        ];
        for (text, want) in cases {
            assert_eq!(on_list(Press::text(text)), Some(want), "{text}");
        }
        assert_eq!(on_list(Press::named(Named::Right)), Some(UiAction::Expand));
        assert_eq!(on_list(Press::named(Named::Left)), Some(UiAction::Collapse));
        assert_eq!(on_list(Press::named(Named::Tab)), Some(UiAction::NextFocus));
    }

    #[test]
    fn while_typing_only_escape_and_command_r_count() {
        let typing = |press| action(&press, Focus::List, true);
        assert_eq!(typing(Press::text("e")), None);
        assert_eq!(typing(Press::named(Named::Down)), None);
        assert_eq!(typing(Press::named(Named::Escape)), Some(UiAction::Escape));
        let sync = Press {
            command: true,
            ..Press::text("r")
        };
        assert_eq!(typing(sync), Some(UiAction::SyncNow));
    }

    #[test]
    fn command_r_syncs_and_command_with_a_letter_is_no_shortcut() {
        let command = |text| Press {
            command: true,
            ..Press::text(text)
        };
        assert_eq!(on_list(command("r")), Some(UiAction::SyncNow));
        assert_eq!(on_list(command("e")), None);
        assert_eq!(on_list(command("j")), None);
        assert_eq!(on_list(Press::text("r")), None);
    }

    #[test]
    fn backspace_deletes_on_macos_only() {
        let delete = on_list(Press::named(Named::Backspace));
        if cfg!(target_os = "macos") {
            assert_eq!(delete, Some(UiAction::Act(Action::Trash)));
        } else {
            assert_eq!(delete, None);
        }
        assert_eq!(
            on_list(Press::named(Named::Delete)),
            Some(UiAction::Act(Action::Trash))
        );
    }
}
