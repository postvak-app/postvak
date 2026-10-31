//! The folder dialog: creating, renaming and deleting a folder, and following a rename.
use super::*;

impl App {
    pub(super) fn new_folder(&mut self, account: usize, parent: Option<String>) {
        let folders = &self.accounts[account].folders;
        let parent = parent
            .or_else(|| folders::under_inbox(folders).then(|| list::INBOX.to_string()))
            .filter(|parent| self.delimiter(account, parent).is_some());
        self.open_folder_dialog(account, FolderKind::New { parent }, String::new());
    }

    pub(super) fn rename_folder(&mut self, account: usize, from: String) {
        let delimiter = self.delimiter(account, &from);
        let level = delimiter
            .as_deref()
            .and_then(|d| from.rsplit_once(d))
            .map_or(from.as_str(), |(_, level)| level)
            .to_string();
        let accounts: Vec<String> = self.accounts.iter().map(|a| a.name.clone()).collect();
        let rename = FolderRename {
            account: &self.accounts[account].name,
            accounts: &accounts,
            from: &from,
            to: &from,
            delimiter: delimiter.as_deref(),
        };
        let rules = rule_file::edit::rules_naming(&self.rules.file, &rename);
        self.open_folder_dialog(account, FolderKind::Rename { from, rules }, level);
    }

    pub(super) fn open_folder_dialog(&mut self, account: usize, kind: FolderKind, name: String) {
        self.folder_dialog = Some(FolderDialog {
            account,
            kind,
            name,
            error: None,
            waiting: None,
        });
    }

    /// The delimiter of the account's `folder`, else the one its other folders use; None on a server without levels.
    pub(super) fn delimiter(&self, account: usize, folder: &str) -> Option<String> {
        let folders = &self.accounts[account].folders;
        let own = folders.iter().find(|f| f.name == folder);
        own.into_iter()
            .chain(folders)
            .find_map(|f| f.delimiter.clone())
            .filter(|d| !d.is_empty())
    }

    /// The whole name the folder dialog would create or rename to: its parent, the delimiter and the field.
    pub(crate) fn folder_target(&self, dialog: &FolderDialog) -> Option<String> {
        let parent = match &dialog.kind {
            FolderKind::New { parent } => parent.clone(),
            FolderKind::Rename { from, .. } => self.delimiter(dialog.account, from).and_then(|d| {
                from.rsplit_once(d.as_str())
                    .map(|(parent, _)| parent.to_string())
            }),
            FolderKind::Delete { .. } => return None,
        };
        let prefix =
            parent.and_then(|p| Some(format!("{p}{}", self.delimiter(dialog.account, &p)?)));
        Some(format!("{}{}", prefix.unwrap_or_default(), dialog.name))
    }

    /// Whether the folder dialog's button would send: a name that changed and passed, and no answer awaited.
    pub(crate) fn folder_ready(&self) -> bool {
        let Some(dialog) = &self.folder_dialog else {
            return false;
        };
        if dialog.waiting.is_some() || dialog.error.is_some() {
            return false;
        }
        match &dialog.kind {
            FolderKind::Rename { from, .. } => {
                !dialog.name.is_empty() && self.folder_target(dialog).as_ref() != Some(from)
            }
            FolderKind::New { .. } => !dialog.name.is_empty(),
            FolderKind::Delete { .. } => true,
        }
    }

    /// The name the folder dialog would send in NFC, or why it will not do; None while there is nothing to check.
    pub(super) fn checked_folder_name(&self) -> Option<Result<String, String>> {
        let dialog = self.folder_dialog.as_ref()?;
        let target = self.folder_target(dialog)?;
        let unchanged = matches!(&dialog.kind, FolderKind::Rename { from, .. } if *from == target);
        if dialog.name.is_empty() || unchanged {
            return None;
        }
        Some(match &self.accounts[dialog.account].store {
            Ok(store) => folder_name::check_new(store, &target),
            Err(e) => Err(e.clone()),
        })
    }

    pub(super) fn check_folder_name(&mut self) {
        let error = self.checked_folder_name().and_then(Result::err);
        if let Some(dialog) = self.folder_dialog.as_mut() {
            dialog.error = error;
        }
    }

    pub(super) fn confirm_folder(&mut self, mode: Option<DeleteMode>) {
        if !self.folder_ready() {
            return;
        }
        let checked = self.checked_folder_name();
        let Some(dialog) = &self.folder_dialog else {
            return;
        };
        let (command, about) = match (&dialog.kind, checked) {
            (FolderKind::New { .. }, Some(Ok(name))) => {
                (Command::CreateFolder { name: name.clone() }, name)
            }
            (FolderKind::Rename { from, .. }, Some(Ok(to))) => (
                Command::RenameFolder {
                    from: from.clone(),
                    to,
                },
                from.clone(),
            ),
            (FolderKind::Delete { folder, not_empty }, _) => {
                let offered = match (mode, not_empty) {
                    (None, None) => true,
                    (Some(DeleteMode::Trash), Some((_, has_trash))) => *has_trash,
                    (Some(DeleteMode::Permanent), Some(_)) => true,
                    _ => false,
                };
                if !offered {
                    return;
                }
                let command = Command::DeleteFolder {
                    name: folder.clone(),
                    mode,
                    count: not_empty.map_or(0, |(count, _)| count),
                };
                (command, folder.clone())
            }
            _ => return,
        };
        let sent = self.send(dialog.account, command);
        if let Some(dialog) = self.folder_dialog.as_mut() {
            if sent {
                dialog.waiting = Some(about);
            } else {
                dialog.error = Some(DAEMON_LOST.into());
            }
        }
    }

    /// The folder dialog when it waits for the daemon's answer about the account's `folder`.
    pub(super) fn awaited(&mut self, account: usize, folder: &str) -> Option<&mut FolderDialog> {
        self.folder_dialog.as_mut().filter(|dialog| {
            dialog.account == account
                && dialog
                    .waiting
                    .as_deref()
                    .is_some_and(|waiting| folder_name::same(waiting, folder))
        })
    }

    /// The daemon did what the folder dialog asked: it closes.
    pub(super) fn folder_answered(&mut self, account: usize, folder: &str) {
        if self.awaited(account, folder).is_some() {
            self.folder_dialog = None;
        }
    }

    /// Rules follow a rename by any front end, the view follows a shown folder, and the dialog that asked closes.
    pub(super) fn folder_renamed(&mut self, index: usize, from: &str, to: &str) {
        self.folder_answered(index, from);
        let delimiter = self.delimiter(index, from);
        self.follow_rename(index, from, to, delimiter.as_deref());
        self.refresh(index);
        let View::Folder { account, folder } = &self.view else {
            return;
        };
        let rest = match folder.strip_prefix(from) {
            Some("") => Some(""),
            Some(rest) => delimiter
                .as_deref()
                .and_then(|d| rest.strip_prefix(d).map(|_| rest)),
            None => None,
        };
        if *account == index
            && let Some(rest) = rest
        {
            let folder = format!("{to}{rest}");
            self.select_view(View::Folder {
                account: index,
                folder,
            });
        }
    }

    pub(super) fn follow_rename(
        &mut self,
        index: usize,
        from: &str,
        to: &str,
        delimiter: Option<&str>,
    ) {
        let accounts: Vec<String> = self.accounts.iter().map(|a| a.name.clone()).collect();
        let rename = FolderRename {
            account: &self.accounts[index].name,
            accounts: &accounts,
            from,
            to,
            delimiter,
        };
        match rule_file::edit::rules_after_rename(&self.paths.rules_file(), &rename) {
            Ok(naming) if naming.is_empty() => {}
            Ok(_) => self.rules_changed(),
            Err(message) => self.note_error(Some(index), message),
        }
    }
}
