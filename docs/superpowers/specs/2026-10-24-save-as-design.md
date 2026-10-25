# Save as — design

Status: implemented, 2026-10-24. A desktop integration under the Platforms rule in `AGENTS.md`: opening files.

## Goal

Choose where an attachment or a message's `.eml` goes, in the desktop's own save dialog, next to the one-click Save into Downloads.

## Surfaces

GUI only. This is a named exception to the surface parity rule: a save dialog needs a window. The CLI already takes a folder (`attachment save --dir`), and MCP writes only into the folder it was given (`--save-dir`).

## Decisions

- **One-click Save stays.** It keeps writing into Downloads and never replaces a file. Save as… is an extra button beside each attachment's Save, and "Save .eml as…" sits in the File menu and the message's right-click menu.
- **The dialog** starts in Downloads with the name Save would use: the attachment's own name made safe, or `<subject>.eml`.
- **Linux:** the FileChooser portal (`org.freedesktop.portal.FileChooser.SaveFile`) over zbus, on a thread of its own since it waits for the user. It is the desktop's own dialog (Plasma's or GNOME's), and in a Flatpak the document portal makes only the chosen file writable, so no broader file permission is needed for it. The request plumbing is shared with the Background portal in `portal::request`.
- **macOS:** `NSSavePanel`, run modally on the main thread, where the window runs and AppKit wants it. All safe calls in objc2-app-kit 0.3, which is already in the tree.
- **Writing:** in place, replacing the chosen file. The dialog has asked before replacing one, and a document portal path has no folder to put a temporary file in.
- **One at a time:** a second Save as… while a dialog is open does nothing. The note ("Saved to …" or why not) shows only on the message it came from; a cancelled dialog says nothing.
- No new dependency: zbus and the objc2 crates are already in the tree. `rfd` was set aside, since it brings its own portal client (ashpd) and runtime choice.

## Tests

- The suggested names and the in-place write that replaces a file.
- The portal: the request path, a cancelled request, the chosen `file://` URI as a path, the start folder's bytes.
- In the window: Save as… writes where the dialog chose and replaces that file; a cancelled dialog writes nothing and shows no note.
- The menus: Save .eml as… is enabled with Save .eml.

Not automated: a real portal or save panel. Compiled and tested on Linux; the macOS panel code type-checked for `aarch64-apple-darwin` in a scratch crate. Not run on Plasma, GNOME or macOS.
