//! Drafts on the server: which folder holds them.

use crate::store::Folder;

/// The folder drafts are kept in: the one marked `\Drafts`, else one named `Drafts`.
pub fn drafts_folder(folders: &[Folder]) -> Option<String> {
    folders
        .iter()
        .find(|folder| folder.special_use.as_deref() == Some("Drafts"))
        .or_else(|| {
            folders
                .iter()
                .find(|folder| crate::folder_name::same(&folder.name, "Drafts"))
        })
        .map(|folder| folder.name.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(name: &str, special_use: Option<&str>) -> Folder {
        Folder {
            name: name.into(),
            uidvalidity: 1,
            last_uid: 0,
            special_use: special_use.map(str::to_string),
            delimiter: None,
        }
    }

    #[test]
    fn drafts_folder_prefers_special_use_then_the_name() {
        let folders = [folder("Drafts", None), folder("Concepten", Some("Drafts"))];
        assert_eq!(drafts_folder(&folders).as_deref(), Some("Concepten"));
        assert_eq!(drafts_folder(&folders[..1]).as_deref(), Some("Drafts"));
        assert_eq!(drafts_folder(&[folder("drafts", None)]), None);
        assert_eq!(drafts_folder(&[]), None);
    }
}
