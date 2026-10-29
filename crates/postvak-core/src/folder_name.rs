//! Folder names above the wire, where they are UTF-8: comparing them across Unicode normalization forms, finding the
//! store's own spelling of one, and the `INBOX` + delimiter namespace some servers keep every folder under.
use std::borrow::Cow;

use unicode_normalization::{UnicodeNormalization, is_nfc};

use crate::store::{Folder, Store, StoreError};

/// `name` in NFC, as RFC 9051 §5.1 and RFC 6855 §3 want a new mailbox name: `Één` typed or pasted as letters plus
/// combining accents (macOS input, file names) becomes the precomposed letters.
pub fn nfc(name: &str) -> Cow<'_, str> {
    if is_nfc(name) {
        Cow::Borrowed(name)
    } else {
        Cow::Owned(name.nfc().collect())
    }
}

/// Whether `a` and `b` name the same folder apart from Unicode normalization. Other differences, case among them,
/// count: some servers keep `Lists` and `lists` apart.
pub fn same(a: &str, b: &str) -> bool {
    a == b || (!(a.is_ascii() && b.is_ascii()) && nfc(a) == nfc(b))
}

/// Whether a rule's folder `rule` names `folder`: [`same`], or `rule` is how IMAP spells `folder` on the wire, as
/// rules written before Postvak decoded folder names spell it ([`wire_spelling`]).
pub fn rule_names(rule: &str, folder: &str) -> bool {
    same(rule, folder) || wire_spelling(rule).is_some_and(|plain| same(&plain, folder))
}

/// The plain name `name` encodes when it is modified UTF-7 for another name, such as `&AMkA6Q-n` for `Één`: before
/// Postvak decoded folder names it showed them so, and rules were written so.
pub fn wire_spelling(name: &str) -> Option<String> {
    name.contains('&')
        .then(|| crate::mail_ops::utf7::decode(name))
        .flatten()
        .filter(|plain| plain != name)
}

/// The folder the store knows by `name`, in the store's spelling: the exact name first, else one equal in NFC. A
/// folder another client created through IMAP4rev1 may be stored in NFD, and only its own spelling reaches it. A
/// name no folder has that is the wire spelling of one the store knows, such as `Archief/&AMkA6Q-n` for
/// `Archief/Één`, finds that folder, so a rule from before Postvak decoded names never creates a literal twin.
pub fn known(store: &Store, name: &str) -> Result<Option<Folder>, StoreError> {
    if let Some(folder) = store.folder(name)? {
        return Ok(Some(folder));
    }
    let plain = wire_spelling(name);
    if name.is_ascii() && plain.is_none() {
        return Ok(None);
    }
    let folders = store.folders()?;
    if let Some(folder) = folders.iter().find(|f| same(&f.name, name)) {
        return Ok(Some(folder.clone()));
    }
    Ok(plain.and_then(|plain| folders.into_iter().find(|f| same(&f.name, &plain))))
}

/// The rest of `name` after a leading `INBOX` + `delimiter` (Dovecot and Courier namespaces), in any case.
pub fn inbox_prefix<'a>(name: &'a str, delimiter: Option<&str>) -> Option<&'a str> {
    let delimiter = delimiter.filter(|d| !d.is_empty())?;
    let prefix = format!("INBOX{delimiter}");
    let head = name.get(..prefix.len())?;
    head.eq_ignore_ascii_case(&prefix)
        .then(|| &name[prefix.len()..])
}

/// Whether the server keeps every folder under `INBOX` + delimiter: there is at least one, as (name, delimiter), and
/// each has that prefix. The caller leaves out INBOX itself and the folders it does not count.
pub fn under_inbox<'a>(folders: impl IntoIterator<Item = (&'a str, Option<&'a str>)>) -> bool {
    let mut any = false;
    for (name, delimiter) in folders {
        if inbox_prefix(name, delimiter).is_none() {
            return false;
        }
        any = true;
    }
    any
}

/// Refuses a folder name servers refuse or misread, by its levels between `/`, the delimiter the docs teach: a level
/// starting or ending in whitespace, which looks like a folder that is not there. A move target, which may be created,
/// also may not have an empty level (`/Lists`, `Lists//GitHub`, or `Lists/`, which RFC 3501 §6.3.3 makes only a
/// parent for later folders) or IMAP's `LIST` wildcards `*` and `%`.
pub fn check_shape(name: &str, target: bool) -> Result<(), String> {
    if target && name.contains(['*', '%']) {
        return Err("must not contain * or %, which IMAP reads as wildcards".into());
    }
    for level in name.split('/') {
        if target && level.is_empty() {
            return Err("must not have an empty level: a leading, trailing or doubled /".into());
        }
        if level.trim() != level {
            return Err(format!(
                "level '{level}' must not start or end with whitespace"
            ));
        }
    }
    Ok(())
}

/// `name` in NFC for a folder to create, or why not: a bad shape ([`check_shape`]) or a folder the store already
/// knows by that name, in any normalization.
pub fn check_new(store: &Store, name: &str) -> Result<String, String> {
    check_shape(name, true)?;
    if let Some(existing) = known(store, name).map_err(|e| e.to_string())? {
        return Err(format!("a folder named {} already exists", existing.name));
    }
    Ok(nfc(name).into_owned())
}

/// Why a folder must not be renamed or deleted: `INBOX` in any case, or the special-use role the store has for it.
pub fn protected(store: &Store, name: &str) -> Result<Option<String>, StoreError> {
    if name.eq_ignore_ascii_case("INBOX") {
        return Ok(Some("INBOX".into()));
    }
    Ok(known(store, name)?.and_then(|folder| folder.special_use))
}

/// The first folder under `name` that [`protected`] names, as `child (role)`.
pub fn protected_below(store: &Store, name: &str) -> Result<Option<String>, StoreError> {
    for child in children(store, name)? {
        if let Some(role) = protected(store, &child)? {
            return Ok(Some(format!("{child} ({role})")));
        }
    }
    Ok(None)
}

/// The stored folders under `name`: those starting with `name` and their own delimiter, sorted. A folder without a
/// delimiter has none.
pub fn children(store: &Store, name: &str) -> Result<Vec<String>, StoreError> {
    let Some(parent) = known(store, name)? else {
        return Ok(Vec::new());
    };
    let Some(delimiter) = parent.delimiter.filter(|d| !d.is_empty()) else {
        return Ok(Vec::new());
    };
    let prefix = format!("{}{delimiter}", parent.name);
    let mut found: Vec<String> = store
        .folders()?
        .into_iter()
        .filter(|folder| folder.name.starts_with(&prefix))
        .map(|folder| folder.name)
        .collect();
    found.sort();
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NFC: &str = "\u{c9}\u{e9}n";
    const NFD: &str = "E\u{301}e\u{301}n";

    fn folder(name: &str) -> Folder {
        Folder {
            name: name.into(),
            uidvalidity: 1,
            last_uid: 0,
            special_use: None,
            delimiter: Some("/".into()),
        }
    }

    fn stored(store: &Store, name: &str, special_use: Option<&str>) {
        let mut f = folder(name);
        f.special_use = special_use.map(Into::into);
        store.upsert_folder(&f).unwrap();
    }

    #[test]
    fn check_new_refuses_a_known_name_in_nfd() {
        let store = Store::open_in_memory().unwrap();
        stored(&store, "Caf\u{e9}", None);
        assert!(check_new(&store, "Cafe\u{301}").is_err());
    }

    #[test]
    fn check_new_returns_nfc() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(check_new(&store, NFD).unwrap(), NFC);
    }

    #[test]
    fn check_new_refuses_wildcards_and_empty_levels() {
        let store = Store::open_in_memory().unwrap();
        for bad in ["A/*", "A//B", "A/"] {
            assert!(check_new(&store, bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn inbox_and_special_use_are_protected() {
        let store = Store::open_in_memory().unwrap();
        stored(&store, "Bin", Some("Trash"));
        stored(&store, "Plain", None);
        assert_eq!(protected(&store, "inbox").unwrap(), Some("INBOX".into()));
        assert_eq!(protected(&store, "Bin").unwrap(), Some("Trash".into()));
        assert_eq!(protected(&store, "Plain").unwrap(), None);
    }

    #[test]
    fn children_match_by_delimiter_not_prefix() {
        let store = Store::open_in_memory().unwrap();
        for name in ["Work", "Work/Clients", "Workshop"] {
            stored(&store, name, None);
        }
        assert_eq!(children(&store, "Work").unwrap(), ["Work/Clients"]);
    }

    #[test]
    fn protected_below_names_the_child() {
        let store = Store::open_in_memory().unwrap();
        stored(&store, "Mail", None);
        stored(&store, "Mail/Sent", Some("Sent"));
        assert_eq!(
            protected_below(&store, "Mail").unwrap(),
            Some("Mail/Sent (Sent)".into())
        );
    }

    #[test]
    fn nfd_becomes_nfc_and_nfc_is_borrowed() {
        assert_eq!(nfc(NFD), NFC);
        assert!(matches!(nfc(NFC), Cow::Borrowed(_)));
    }

    #[test]
    fn names_differing_only_in_normalization_are_the_same_but_case_counts() {
        assert!(same(NFD, NFC));
        assert!(same("Lists", "Lists"));
        assert!(!same("Lists", "lists"));
        assert!(!same(NFC, "Een"));
    }

    #[test]
    fn a_known_folder_is_found_in_its_own_spelling() {
        let store = Store::open_in_memory().unwrap();
        store
            .upsert_folder(&folder(&format!("Archief/{NFD}")))
            .unwrap();
        let found = known(&store, &format!("Archief/{NFC}")).unwrap().unwrap();
        assert_eq!(found.name, format!("Archief/{NFD}"));
        assert_eq!(known(&store, "Archief").unwrap(), None);
    }

    /// Before Postvak decoded folder names it showed `Archief/&AMkA6Q-n`, and a rule copied from that moved mail to
    /// `Archief/Één`; it must still, not into a new folder literally called `Archief/&AMkA6Q-n`.
    #[test]
    fn a_wire_spelling_finds_the_folder_it_encodes() {
        let store = Store::open_in_memory().unwrap();
        store
            .upsert_folder(&folder(&format!("Archief/{NFC}")))
            .unwrap();
        let found = known(&store, "Archief/&AMkA6Q-n").unwrap().unwrap();
        assert_eq!(found.name, format!("Archief/{NFC}"));
        assert_eq!(known(&store, "Other/&AMkA6Q-n").unwrap(), None);
        // A folder that really has that name wins.
        store.upsert_folder(&folder("Archief/&AMkA6Q-n")).unwrap();
        let found = known(&store, "Archief/&AMkA6Q-n").unwrap().unwrap();
        assert_eq!(found.name, "Archief/&AMkA6Q-n");
    }

    #[test]
    fn a_rule_names_a_folder_in_its_wire_spelling_too() {
        assert!(rule_names("&AMkA6Q-n", NFC));
        assert!(rule_names("&AMkA6Q-n", NFD));
        assert!(rule_names(NFC, NFD));
        assert!(rule_names("Q&-A", "Q&A"));
        assert!(!rule_names("R&D", "R&D-"));
        assert!(!rule_names("&AMkA6Q-n", "Een"));
    }

    #[test]
    fn the_inbox_namespace_needs_every_folder_under_it() {
        assert_eq!(inbox_prefix("INBOX.Lists", Some(".")), Some("Lists"));
        assert_eq!(inbox_prefix("inbox.Lists", Some(".")), Some("Lists"));
        assert_eq!(inbox_prefix("INBOX.Lists", Some("/")), None);
        assert_eq!(inbox_prefix("INBOX.Lists", None), None);
        assert!(under_inbox([
            ("INBOX.Lists", Some(".")),
            ("INBOX.Work", Some("."))
        ]));
        assert!(!under_inbox([
            ("INBOX.Lists", Some(".")),
            ("Work", Some("."))
        ]));
        assert!(!under_inbox([]));
    }
}
