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

/// The folder the store knows by `name`, in the store's spelling: the exact name first, else one equal in NFC. A
/// folder another client created through IMAP4rev1 may be stored in NFD, and only its own spelling reaches it.
pub fn known(store: &Store, name: &str) -> Result<Option<Folder>, StoreError> {
    if let Some(folder) = store.folder(name)? {
        return Ok(Some(folder));
    }
    if name.is_ascii() {
        return Ok(None);
    }
    Ok(store.folders()?.into_iter().find(|f| same(&f.name, name)))
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
