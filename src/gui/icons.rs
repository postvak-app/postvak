//! The Phosphor icons the window uses, by meaning rather than by glyph name: characters of the bundled
//! `assets/fonts/Phosphor.ttf`, drawn as text in the "Phosphor" family.

pub(crate) const ACTIVITY: &str = "\u{E000}"; // pulse
pub(crate) const ALL_INBOXES: &str = "\u{E466}"; // stack
pub(crate) const ARCHIVE: &str = "\u{E00C}"; // archive
pub(crate) const BACKUPS: &str = "\u{E1A0}"; // clock-counter-clockwise
pub(crate) const CARET_DOWN: &str = "\u{E136}"; // caret-down
pub(crate) const CARET_RIGHT: &str = "\u{E13A}"; // caret-right
pub(crate) const CHECK: &str = "\u{E182}"; // check
pub(crate) const CLOSE: &str = "\u{E4F6}"; // x
pub(crate) const DRAFTS: &str = "\u{E704}"; // file-dashed
pub(crate) const FLAG: &str = "\u{E244}"; // flag
pub(crate) const FOLDER: &str = "\u{E24A}"; // folder
pub(crate) const INBOX: &str = "\u{E4AA}"; // tray
pub(crate) const JUNK: &str = "\u{E4E4}"; // warning-octagon
pub(crate) const MARK_UNREAD: &str = "\u{E218}"; // envelope-simple
pub(crate) const MOVE: &str = "\u{EC2A}"; // folder-simple-dashed
pub(crate) const NEWER: &str = "\u{E08E}"; // arrow-up
pub(crate) const RULES: &str = "\u{E266}"; // funnel
pub(crate) const SAVE: &str = "\u{E20C}"; // download-simple
pub(crate) const SEARCH: &str = "\u{E30C}"; // magnifying-glass
pub(crate) const SENT: &str = "\u{E398}"; // paper-plane-tilt
pub(crate) const SOURCE: &str = "\u{E1BC}"; // code
pub(crate) const SYNC: &str = "\u{E094}"; // arrows-clockwise
pub(crate) const TRASH: &str = "\u{E4A6}"; // trash

/// The icon for a folder: its special use, INBOX by name, else a plain folder.
pub(crate) fn for_special_use(special_use: Option<&str>, name: &str) -> &'static str {
    if name.eq_ignore_ascii_case("INBOX") {
        return INBOX;
    }
    match special_use {
        Some("Archive") => ARCHIVE,
        Some("Drafts") => DRAFTS,
        Some("Junk") => JUNK,
        Some("Sent") => SENT,
        Some("Trash") => TRASH,
        _ => FOLDER,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn special_folders_get_their_icon_and_others_a_folder() {
        assert_eq!(for_special_use(None, "inbox"), INBOX);
        assert_eq!(for_special_use(Some("Archive"), "Archief"), ARCHIVE);
        assert_eq!(for_special_use(Some("Sent"), "Sent Items"), SENT);
        assert_eq!(for_special_use(None, "Receipts"), FOLDER);
    }
}
