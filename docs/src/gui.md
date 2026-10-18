# Mail window

![The mail window in the light theme](https://raw.githubusercontent.com/postvak-app/postvak/main/tests/snapshots/gui_inbox_light.png)

`postvak gui` opens a window with three columns: your accounts and folders, the threads in the chosen folder, and the selected message. Sync, rules and notifications run in the [daemon](daemon.md), which the window starts when none is running, so the window, the CLI and agents over MCP can all be open at once.

When the window cannot start, a small window says why and what to do, and the error also goes to stderr. With no account set up yet, that window holds the add-account form; saving it opens the mail window. [Troubleshooting](#troubleshooting) covers a window that does not open at all. Closing the window does not stop sync. Where the desktop shows tray icons, Postvak puts one there with the unread count in its tooltip: closing the window then keeps Postvak running in the tray, and the tray's menu opens the window again, syncs, or quits. Without a tray, closing the window quits. A daemon the window started stops a minute after its last client goes; one you run with `postvak run` keeps going. If the daemon goes away while the window is open, the sync line says "background sync stopped — reconnecting" and the window tries again every five seconds.

The window remembers its size, its position (where the platform reports one; Wayland does not) and the width of its columns in `window.json`, next to `daemon.log` in the state directory. Delete the file to start from the defaults.

## Menu bar

The menu bar (in the window, or at the top of the screen on macOS) has the window's actions in one place: File (Save .eml, View source, Close window with Ctrl+W, Quit with Ctrl+Q; Cmd on macOS), Edit (Copy, Search), Message (Archive, Move…, Delete, Junk or not junk, Flag, Read or unread, Add to selection, Select all), View (Text or HTML, Theme, History, Keys), Mailbox (Add account…, Sync now, Mark folder read) and Help (Postvak documentation, About Postvak). Add account… asks for the account's name, IMAP host, port, username, password and address; the password goes to the OS keyring. The account joins the folder pane at once and its first sync starts, so a wrong login shows on its sync line. Saving a name that exists replaces that account. An item is greyed out when it would do nothing, as the reader's action buttons are. About Postvak shows the version, the licence and the Slint attribution.

Drag a message onto a folder of its account to move it there; drag a marked message to move every marked one. Right-click a message for read, flag, archive, move, delete, junk, View source and Save .eml; a folder for Open, Collapse or Expand, Mark all as read and Sync now; a link in an HTML message to open or copy it.

## Keys

| Key | Action |
|---|---|
| `j` / `k`, Down / Up | next or previous row |
| Right / Left | expand or collapse a thread |
| `x` | add the row to the selection, or take it out |
| Ctrl+A (Cmd+A on macOS) | add every row in the list to the selection |
| `e` | archive |
| `#`, Delete (Backspace on macOS) | delete: move to Trash; from Trash, or with no Trash folder, delete for good after saving an `.eml` backup |
| `!` | junk: tag `$Junk`, which trains the server's spam filter, and move to the Junk folder; in the Junk folder, not junk: tag `$NotJunk` and move to INBOX |
| `m` | move: type to filter the folders, Enter |
| `u` | mark read or unread |
| `s` | flag or unflag |
| `v` | show HTML mail as text, or as HTML again |
| `/` | search this account, or every account from All inboxes; mail whose body is not downloaded yet matches on sender, recipients and subject only, until Enter asks the server too, whose hits join the list as they arrive |
| Esc | close a popup, leave search, clear the selection |
| Tab | next pane: folders, list, body |
| Ctrl+R (Cmd+R on macOS) | sync every account now |
| `?` | show these keys |

Each attachment of the open message has a Save button, which saves it to your Downloads folder, or to your home folder when there is none.

With the mouse, Shift-click selects every row from the current one to the one you click, and Ctrl-click (Cmd-click on macOS) adds a row to the selection or takes it out; the first one also selects the current row.

Actions apply to the selection when there is one, else to the current row; on a thread row they apply to the whole thread in that folder. A message you open, with the keys or a click, is marked read after its text has been on screen for a second; the one a folder opens on stays unread.

In All inboxes each action goes to the message's own account. `m` offers the folders of that account; marked messages from more than one account cannot be moved together.

## Folders

With two or more accounts, **All inboxes** at the top of the folder pane lists the INBOX of every account in one list, newest first; its count is the unread mail in all of them.

Each account lists its INBOX and special folders (Archive, Drafts, Sent, Junk, Trash) first, then your own folders as a tree that follows the server's hierarchy. Each level steps in a little, up to three; deeper folders line up with the third level, are cut off with "…" when long, and show their whole path on hover. Click the chevron on the right of a parent to fold or unfold its branch; folding lasts until you close the window. A parent the server does not list as a folder is shown but cannot be opened. The folders scroll, while Rules, Activity and Backups stay at the bottom of the pane.

## Message actions and search

The top of the reading pane has buttons for Archive, Move, Delete, Junk, Flag and Read or unread; hover one to see its key. They act on the selection or the current row, like the keys. With two or more messages selected, the reading pane lists them under "3 messages selected" instead of showing one, and the same buttons, now with their names, act on all of them; **Clear selection** (or Esc) empties it. The search field at the top of the message list starts a search, like `/`.

## Message list

Each row shows a dot when it is unread, then on its first line the sender (the recipient in Sent and Drafts), darker when unread, a flag (or a check when marked) and the date, and on its second line the subject with the thread's message count. Dates read "09:30" today, "Sun 18:00" within the past week, "3 Sep" earlier this year and "10 Dec 2025" before that. A long subject is cut off with "…" so the date always stays in view.

The funnel button beside the list's title shows All, Unread, Flagged or Attachments: a thread stays when one of its messages fits. Any choice but All shows as a chip beside the title, which clears it, and as a dot on the funnel. Attachments is a guess from the headers (mail sent as `multipart/mixed`), the same guess Thunderbird makes. The choice holds as you change folders; search results ignore it. On the CLI and over MCP, `list` takes `--unread`, `--flagged` and `--attachments`.

## Sync status

The foot of the folder pane has one line about sync: a failure when there is one, else an account that is offline and when it retries, else one that is syncing (which folder, how many headers or bodies of how many), else that the first account is up to date. Hover it for every account's line, click it for the last 50 lines. The button beside it syncs every account now (Ctrl+R, Cmd+R on macOS). View ▸ Theme picks the System, Light or Dark theme and saves it as `[ui] theme` in `config.toml`. Both themes put the panes in an oxblood frame: warm paper in light mode, dark brown in dark mode. Under the line, a link appears when a newer release is out; it opens that release's notes on GitHub. Postvak does not update itself, and the daily check runs only with `[ui] check_updates = true` in `config.toml` (see [Accounts](accounts.md#appearance)).

## Rules, Activity and Backups

![Rules view with an agent proposal](https://raw.githubusercontent.com/postvak-app/postvak/main/tests/snapshots/gui_rules.png)

These three views sit at the bottom of the folder pane.

**Rules** lists proposals with Approve and Reject, then every rule with a switch. Changes to `rules.toml`, from the window or from your editor, take effect within about two seconds for new mail. **Activity** is the log of what rules and your actions did. **Backups** lists the `.eml` backups of mail deleted for good (deleted from Trash, from an account without a Trash folder, or by a rule), with Restore, and Purge, which removes the backups older than each account's retention period as `postvak trash purge` does and says how many in the sync line. Mail moved to the server's Trash folder is in that folder in the tree.

The daemon applies changes to `config.toml` by itself. An account added from the window joins it at once; other changes show when you reopen the window, and the window says so.

## HTML mail

![An HTML message rendered in the reader](https://raw.githubusercontent.com/postvak-app/postvak/main/tests/snapshots/gui_html_light.png)

Mail with an HTML part shows as its sender laid it out, on a white page in both themes; `v` switches that message to its text and back. Images sent inside the message show. Images and stylesheets from the web are not loaded until you ask: when a message wants some, the line "Remote content not loaded." has a **Load remote content** button, which downloads it for that message only. Loading tells the sender's servers your IP address and that you opened the message, so leave it off for mail you don't trust. Postvak never loads content from your own machine or local network. No scripts run and forms do nothing. Hover over a link to see where it goes; in HTML and in text, only `http`, `https` and `mailto` links open, in your browser or mail app.

HTML over 2 MB, or a page too tall or too broken to lay out, shows as text with a note saying why. Sending mail comes later.

## Reader

The subject heads the message, then the sender's initials on a colour picked from their address, their name and address, the recipients and the date.

On the right of the action buttons, **Save .eml** writes the message exactly as the server sent it to your Downloads folder, named after its subject; when that name is taken it saves beside it as "… (2).eml" and never replaces a file. The line under the attachments says where it went. Until the message is downloaded the button is greyed out and says why on hover.

**View source** opens the message as plain text in a window, with control characters left out; Copy puts that text on the clipboard. Esc or moving to another message closes it.

Mail from a mailing list that says how to leave it (a `List-Unsubscribe` header) has an **Unsubscribe** button there. When the sender supports one-click unsubscribe (RFC 8058), Postvak sends that request itself, only to a public web host, and the line under the attachments says whether the sender accepted it. Otherwise the button opens the sender's unsubscribe page in your browser, or a mail to their unsubscribe address in your mail app. Either way the sender learns that your address reads its mail.

Mail with an HTML part also has a **Text | HTML** switch there, which does the same as `v`. View source always shows the whole message, whichever is active.

## Troubleshooting

The window draws with OpenGL. Where OpenGL cannot start (a virtual machine, a remote desktop, some Flatpak sandboxes, a broken or missing driver), Postvak starts again with Slint's software renderer, which needs no graphics driver and is slower on large windows. If no window opens even then, the error goes to stderr and to `daemon.log` in the state directory.

To use the software renderer from the start, for a bug report or a driver you know is broken, set Slint's own variable:

```sh
SLINT_BACKEND=winit-software postvak gui
```

**Safe mode** draws with the software renderer and ignores `window.json`, opening the window at its default size. `postvak gui --safe-mode` asks for it. Postvak also enters it by itself when the last two starts in a row died before the window showed; it counts them in `gui-starting`, next to `window.json`, which it removes as soon as the window shows. The window saves its layout as usual when it closes in safe mode, which replaces a `window.json` that kept it from opening.
