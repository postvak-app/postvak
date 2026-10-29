# Folder management: create, rename, delete

Postvak syncs folders but gives the user no way to make, rename or remove one; the only CREATE it sends is the one a move to an unknown folder needs. This adds the three, chiefly for the window.

## Surfaces

- **GUI**: the folder tree's right-click menu gains New folder…, Rename… and Delete…, each opening a modal dialog (the dialogs of #422).
- **CLI**: `postvak folder create|rename|delete`, for scripting and the live IMAP tests. `postvak folders` (the listing) stays as it is.
- **MCP: not offered.** This is the exception the surface rule allows. An agent never needs to create a folder (a rule's `move` already creates its target) and renaming or deleting someone's folders is too destructive to hand it, even behind `dry_run`.

All three go through the daemon as new `sync::Command`s (`CreateFolder`, `RenameFolder`, `DeleteFolder`), run on the account's sync thread like `Apply`, and the folder list is refreshed when they succeed so the tree shows the change at once. Logic lives in `postvak-core`; the GUI and CLI only build the command and word the result.

## Protected folders

INBOX and the folders with a special-use role (`\Sent`, `\Trash`, `\Junk`, `\Drafts`, `\Archive`, `\All`, `\Flagged`, from `Folder::special_use`) cannot be renamed or deleted. Nor can a folder with a protected folder under it be renamed, since RENAME would carry that one along (builder's call: refusing is simpler than moving a role). The GUI greys those menu items out; the CLI and the daemon refuse them with a message naming the role. A protected folder may still get subfolders.

The three commands need the account online; while it is offline they are refused rather than queued (builder's call), so a confirmed delete never runs later against different contents.

## Create

- The user types a name. In the GUI it goes under the folder right-clicked, joined with the account's delimiter (`folder_name`); right-clicking the account creates it at the top level (under `INBOX` + delimiter when `folder_name::under_inbox` says the account keeps everything there). The CLI takes the full name.
- The name is stored and sent in NFC. A name that is empty, has a level that starts or ends in whitespace, an empty level, or contains `*` or `%` is refused before anything is sent: the same checks `rules::compile` applies to a `move` target, shared rather than copied.
- A name the store already has (`folder_name::same`) is refused.
- After CREATE the folder is subscribed, as the move path does.

## Rename

- IMAP RENAME (RFC 3501 §6.3.5) also renames the folder's subfolders, so the store does the same: the folder and each folder under it (by the delimiter) keep their messages, rule log and rule moves under the new name, as `decode_folder_names` does in migration 017. That code is lifted into a `Store::rename_folder` both use. Nothing syncs again.
- Rules whose `folder` or `move` names the folder, or a folder under it, are listed in the rename dialog ("2 rules move mail here and will follow the new name") and rewritten in rules.toml on confirm, through `rules::edit` (the only writer of rules.toml). A rule scoped to another `account` is left alone. A rule with no `account` is rewritten in place when only one account is configured; with several, it is split: the original gets `account` set to each other account (one copy each, keeping the old name, each right where the original stood) and one copy scoped to the renamed account takes the new name, so no account's rule stops working silently. Copies are named `<name> (<account>)`, and the dialog says which rules will be split. The CLI rewrites them too and prints which.
- The new name gets the same validation as create; renaming onto an existing folder is refused.
- After RENAME the new name is subscribed and the old one unsubscribed.
- If RENAME succeeds and the rules.toml write fails, the error says the folder was renamed and which rules still name the old folder; the rules warnings then show them too.

## Delete

- A folder with subfolders is refused: "Delete or move its subfolders first." Some servers refuse such a DELETE anyway, and IMAP leaves the children behind on the rest.
- An empty folder (the server's own count from STATUS, not the store's) is deleted after a plain confirm.
- A folder with mail asks what to do with it:
  - **Move to Trash**: every message is moved to the account's Trash folder on the server, then the folder is deleted. Without a Trash folder this choice is not offered.
  - **Delete permanently**: the dialog says how many messages go. Each is first saved as an `.eml` in Postvak's trash (`trash`), as a rule delete is, so `trash restore` can bring it back; then the folder is deleted. If a backup fails, nothing is deleted.
- `DeleteFolder` carries an optional mode (`trash` or `permanent`) and the count the user confirmed. Without a mode, a folder that is not empty is refused with its count; the GUI asks its question from that answer, the CLI prints it. With a mode, the count is checked again first, and a different count is refused, so a delete never takes more mail than the user was shown (builder's call: no separate query command).
- The CLI needs `--trash` or `--permanent` for a folder that is not empty, and refuses without one, naming the count.
- `trash restore` creates and subscribes a backup's folder when the server lacks it before APPEND, as a rule move does, so mail from a deleted folder comes back into it (builder's call; this changes restore on every surface for the better).
- If the moves or backups succeed and DELETE then fails, the folder is left empty and the error says so.
- The store drops the folder and its messages once DELETE succeeds. Rules that still name it are not touched: the existing warnings (`rules/warnings.rs`) already flag a move target the account lacks, and a move recreates it.

## Tests

- `RecordingOps` gains rename and delete, and sync tests cover each command, including the refusals (protected, subfolders, name taken, invalid name) and the store rename of children.
- `Store::rename_folder` keeps messages, rule log and rule moves, and migration 017 still passes its tests through it.
- `rules::edit` rewrites `folder` and `move` for a rename, children included, and leaves another account's rules alone.
- `tests/imap_live.rs` creates, renames (with a subfolder) and deletes (empty, to Trash, permanent) against Dovecot.
- GUI: the menu items and their enabled state, the three dialogs, and snapshots of them.

## Docs

`docs/src/` gets a short section on managing folders, saying why the MCP server lacks it; `help` holds the CLI texts.
