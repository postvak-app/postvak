# Mail window

![The mail window in the light theme](https://raw.githubusercontent.com/postvak-app/postvak/main/tests/snapshots/gui_inbox_light.png)

`postvak gui` opens a window with three columns: your accounts and folders, the threads in the chosen folder, and the selected message. Sync, rules and notifications run in the [daemon](daemon.md), which the window starts when none is running, so the window, the CLI and agents over MCP can all be open at once.

When the window cannot start, a small window says why and what to do, and the error also goes to stderr. With no account set up yet, that window holds the add-account form; saving it opens the mail window once the login passed. [Troubleshooting](#troubleshooting) covers a window that does not open at all. Closing the window does not stop sync. Where the desktop shows tray icons, Postvak puts one there with the unread count in its tooltip: closing the window then keeps Postvak running in the tray, and the tray's menu opens the window again, syncs, or quits. Without a tray, closing the window quits. GNOME shows tray icons only with the AppIndicator extension (on Ubuntu it is on by default). While the window is open, Postvak's Dock icon on macOS, or its task bar or dock entry on Linux where the desktop supports it (Plasma, Ubuntu's dock, Dash to Dock), shows the unread count of all inboxes. Postvak opens one window: starting it again, from the dock, the launcher or `postvak gui`, brings back the window that is already open. A daemon the window started stops a minute after its last client goes; one you run with `postvak run` keeps going. If the daemon goes away while the window is open, the sync line says "background sync stopped — reconnecting" and the window tries again every five seconds.

The window remembers its size, its position (where the platform reports one; Wayland does not) and the width of its columns in `window.json`, next to `daemon.log` in the state directory. Delete the file to start from the defaults.

## Menu bar

The menu bar (in the window, or at the top of the screen on macOS) has the window's actions in one place: File (Save .eml, Save .eml as…, View source, Open at login, Close window with Ctrl+W, Quit with Ctrl+Q; Cmd on macOS), Edit (Copy, Search), Message (Archive, Move…, Delete, Junk or not junk, Flag, Read or unread, Add to selection, Select all, Show original colours), View (Text or HTML, Theme, History, Keys), Mailbox (Add account…, Sync now, Mark folder read) and Help (Postvak documentation, About Postvak). An item is greyed out when it would do nothing, as the reader's action buttons are. A dialog, such as About, Add account, Keys or a rule, dims the window behind it until it closes; a click beside it closes it as Esc does, asking first when it holds unsaved changes. About Postvak shows the version, the licence and the Slint attribution. Add account… asks for the account's name, IMAP host, port, encryption (TLS or STARTTLS; typing port 143 picks STARTTLS, 993 TLS), username, password and address; the password goes to the OS keyring. It offers no unencrypted login; that is `tls = "none"` in `config.toml` (see [Accounts](accounts.md)), and an account logged in that way says "unencrypted" next to its name in the folder pane and on its sync line. Saving first has the daemon test the login, as `postvak account add` does; when it fails, nothing is saved and the form stays open with why, so you can fix a field and save again. Esc during the test closes the form and saves nothing. Once the login passed, the account is saved, joins the folder pane and its first sync starts, showing on its sync line. Saving a name that exists replaces that account.

File ▸ Open at login starts Postvak when you log in, out of the way: hidden in the tray, or minimized in the Dock on macOS, where clicking the Dock icon brings the window back. On a desktop without a tray, such as GNOME without the AppIndicator extension, the window opens instead, since a hidden one could not be reached. On macOS it is a launch agent in `~/Library/LaunchAgents`, on Linux an entry in `~/.config/autostart`, which your desktop's startup settings also list; in the Flatpak, Postvak asks the desktop through its Background portal, which may ask you first. `postvak autostart on`, `off` and `status` do the same from a terminal. The window at login is apart from `postvak service install`, which keeps the daemon syncing without any window.

Drag a message onto a folder of its account to move it there; drag a marked message to move every marked one. Right-click a message for read, flag, archive, move, delete, junk, View source, Save .eml and Save .eml as…; a folder for Open, Collapse or Expand, Mark all as read and Sync now; a link in an HTML message to open or copy it. In the tray, Open Postvak, Sync now and Quit.

## Keys

| Key | Action |
|---|---|
| `j` / `k`, Down / Up | next or previous row; in the folder pane, next or previous folder |
| Right / Left | expand or collapse a thread |
| `x` | add the row to the selection, or take it out |
| Ctrl+A (Cmd+A on macOS) | in the list, add every row to the selection; in the message, select its text |
| `e` | archive |
| `#`, Delete (Backspace on macOS) | delete: move to Trash; from Trash, or with no Trash folder, delete for good after saving an `.eml` backup |
| `!` | junk: tag `$Junk`, which trains the server's spam filter, and move to the Junk folder; in the Junk folder, not junk: tag `$NotJunk` and move to INBOX |
| `m` | move: type to filter the folders, Enter |
| `u` | mark read or unread |
| `s` | flag or unflag |
| `v` | show HTML mail as text, or as HTML again |
| `/` | search this account, or every account from All inboxes (see [Search](search.md)); while the background index is incomplete, Enter asks the server too, whose hits join the list as they arrive. When few messages match as typed, close matches follow under their own heading: a word within one typo of what you typed (two for a long word), taken from the mail itself |
| Esc | close a popup, leave search, clear the selection |
| Tab | next pane: folders, list, body |
| Ctrl+R (Cmd+R on macOS) | sync every account now |
| `?` | show these keys |

The letter keys and Right / Left act on the list and the open message; with the folder pane focused, only `j` / `k`, Down / Up, Tab, Esc, `/`, `?`, Ctrl+R and the menu shortcuts work. Ctrl+C (Cmd+C) copies text selected in an HTML message.

Each attachment of the open message has a Save button, which saves it to your Downloads folder, or to your home folder when there is none, and a Save as… button, which opens your desktop's save dialog in that folder with the attachment's name filled in. The file goes where you choose; the dialog asks before replacing one. On Linux the dialog is your desktop's own, through its file chooser portal, so it also works in the Flatpak, which can then write where you chose and nowhere else; on macOS it is the system save panel.

With the mouse, Shift-click selects every row from the current one to the one you click, and Ctrl-click (Cmd-click on macOS) adds a row to the selection or takes it out; the first one also selects the current row.

Actions apply to the selection when there is one, else to the current row; on a thread row they apply to the whole thread in that folder. A message you open, with the keys or a click, is marked read after its text has been on screen for a second; the one a folder opens on stays unread.

In All inboxes each action goes to the message's own account. `m` offers the folders of that account; marked messages from more than one account cannot be moved together.

## Folders

With two or more accounts, **All inboxes** at the top of the folder pane lists the INBOX of every account in one list, newest first; its count is the unread mail in all of them.

Each account lists its INBOX and special folders (Archive, Drafts, Sent, Junk, Trash) first, then your own folders as a tree that follows the server's hierarchy. Each level steps in a little, up to three; deeper folders line up with the third level, are cut off with "…" when long, and show their whole path on hover. Click the chevron on the right of a parent to fold or unfold its branch; folding lasts until you close the window. A parent the server does not list as a folder is shown but cannot be opened. The folders scroll, while Rules, Activity and Backups stay at the bottom of the pane.

### Managing folders

Right-click a folder, or the account's name for a top-level one, for **New folder…**, **Rename…** and **Delete…**. `postvak folder create`, `rename` and `delete` do the same. A new folder is subscribed, so other mail apps show it too.

- **New folder** on a folder makes a subfolder of it; on the account's name it makes a top-level folder, which on servers that keep every folder under INBOX goes under INBOX.

- **Rename** changes the folder and the folders under it. Rules that name it follow. A rule for every account is split into one copy per account, named `<name> (<account>)`, so only the account's own copy follows the rename.
- **Delete** of an empty folder just removes it. A folder holding mail says how many messages it holds and asks whether to move them to Trash or delete them permanently; moving them to Trash is offered only when the account has a Trash folder. A permanent delete saves each message as an `.eml` backup first; Backups, or `postvak trash restore`, brings one back and creates the folder again if it is gone.
- INBOX and the special folders (Archive, Drafts, Sent, Junk, Trash, and the server's own such as Gmail's All Mail and Starred) cannot be renamed or deleted, and a folder with one under it cannot be renamed. Nor can a folder be renamed into one of its own subfolders.
- A folder with subfolders cannot be deleted; delete or move the subfolders first.

## Message actions and search

The top of the reading pane has buttons for Archive, Move, Delete, Junk, Flag and Read or unread; hover one to see its key. They act on the selection or the current row, like the keys. In the Archive folder the archive button moves mail back to INBOX, and in the Junk folder the junk button reports it as not junk. With two or more messages selected, the reading pane lists them under "3 messages selected" instead of showing one, and the same buttons, now with their names, act on all of them; **Clear selection** (or Esc) empties it. The search field at the top of the message list starts a search, like `/`.

## Message list

Each row shows a dot when it is unread, then on its first line the sender (the recipient in Sent and Drafts), darker when unread, a flag (or a check when marked) and the date, and on its second line the subject with the thread's message count. In All inboxes the account name comes before the sender, and in search results the folder before the subject, as "[Archive] …"; a search hit that matched in its body shows the text around the match in place of the subject. Dates read "09:30" today, "Sun 18:00" within the past week, "3 Sep" earlier this year and "10 Dec 2025" before that. A long sender or subject is cut off with "…".

The funnel button beside the list's title shows All, Unread, Flagged or Attachments: a thread stays when one of its messages fits. Any choice but All shows as a chip beside the title, which clears it, and as a dot on the funnel. Attachments is a guess from the headers (mail sent as `multipart/mixed`), the same guess Thunderbird makes. The choice holds as you change folders; search results ignore it. On the CLI and over MCP, `list` takes `--unread`, `--flagged` and `--attachments`.

## Sync status

The foot of the folder pane has one line about sync: a failure when there is one (a login the server rejected also counts, with when it retries, as Postvak tries it only hourly; the sync button beside it tries it at once), else an account that is offline and when it retries, else one that is syncing (which folder, how many headers or bodies of how many, with a thin bar under the line that fills as they come in), else that the first account is up to date, with how far its search index is while it builds. An account whose server lacks `UIDPLUS` has `server lacks UIDPLUS` at the end of its line (see [Other mail clients](daemon.md#other-mail-clients)). Hover it for every account's line, click it for the last 50 lines. The button beside it syncs every account now (Ctrl+R, Cmd+R on macOS). View ▸ Theme picks the System, Light or Dark theme and saves it as `[ui] theme` in `config.toml`. Both themes put the panes in an ink-blue frame: warm paper in light mode, near-black in dark mode. Under the line, a link appears when a newer release is out; it opens that release's notes on GitHub. Postvak does not update itself, and the daily check runs only with `[ui] check_updates = true` in `config.toml` (see [Accounts](accounts.md#appearance)).

## Rules, Activity and Backups

![Rules view with an agent proposal](https://raw.githubusercontent.com/postvak-app/postvak/main/tests/snapshots/gui_rules.png)

These three views sit at the bottom of the folder pane.

**Rules** lists proposals first, each with who proposed it, what it does in one line, the TOML it adds to `rules.toml`, and Approve, Reject and Preview on cached mail, which counts what it would do to the mail already downloaded without doing it. Then a table of every rule, one row each: its number in the order rules run, a switch, its name, the account and folder it watches, its conditions and its actions. Click a column heading to sort by it, and again to reverse; # puts the rules back in the order they run. Click a rule, or New rule, to edit it in a dialog: its name, whether it is on, the account and folder it watches, its conditions and its actions. Test counts what it would do to the mail already downloaded, Save writes it to `rules.toml` keeping your comments and closes the dialog, and Remove rule takes it out after asking. Saving a change restarts the rule, so it acts only on mail that arrives after you save. Conditions the form cannot edit (`any`, `none`, lists of values) are shown as they are and kept on save. Closing the dialog, or opening another rule, while this one has unsaved changes asks to Save, Discard or Cancel. Apply to existing mail, in the dialog of a saved rule that is on, runs it once over the mail its accounts already hold, as `postvak rules apply-existing` does; when it would delete mail from the server for good, it first says how many messages per account and how long their `.eml` backups are kept, and runs only after Delete and apply; the sync line shows how many actions it took on how many messages. The funnel beside a message's subject in the reader starts a new rule for mail from its sender, watching that account and folder; it is not written to `rules.toml` until you add an action and save it. Open rules.toml opens the file in your text editor. Changes to `rules.toml`, from the window or from your editor, take effect within about two seconds for new mail. **Activity** is the log of what rules and your actions did. **Backups** lists the `.eml` backups of mail deleted for good (deleted from Trash, from an account without a Trash folder, or by a rule), with Restore, and Purge, which removes the backups older than each account's retention period as `postvak trash purge` does and says how many in the sync line. Click a column heading in Activity or Backups to sort by it. Mail moved to the server's Trash folder is in that folder in the tree.

The daemon applies changes to `config.toml` by itself. An account added from the window joins it at once; other changes show when you reopen the window, and the window says so.

## HTML mail

![An HTML message rendered in the reader](https://raw.githubusercontent.com/postvak-app/postvak/main/tests/snapshots/gui_html_light.png)

Mail with an HTML part shows as its sender laid it out; `v` switches that message to its text and back. An HTML part with no markup in it but line breaks (`<br>`), plain text sent as HTML, shows as text, line breaks kept and entities such as `&amp;` decoded. Images sent inside the message show. Images and stylesheets from the web are not loaded until you ask: when a message wants some, the line "Remote content not loaded." has a **Load remote content** button, which downloads it for that message only. Loading tells the sender's servers your IP address and that you opened the message, so leave it off for mail you don't trust. Each download is capped at 5 MB and 15 seconds, and a message loads at most 200 of them. Postvak never loads content from your own machine or local network, also not through a web address that leads there. No scripts run, forms do nothing and SVG images do not show. A page wider than 5,000 points or nested too deep shows as text. Hover over a link to see where it goes; in HTML and in text, only `http`, `https` and `mailto` links open, in your browser or mail app.

In the light theme the page is white. In the dark theme Postvak first asks the mail for its own dark design, as a browser in dark mode would, and shows mail that has one, or whose page is dark already, as its sender wrote it; only text outside the sender's own backgrounds, which would sit on the dark window, still changes where it would not read. A dark header above a light article does not make a page dark. Other mail is adapted: light backgrounds turn dark but keep their colour (a pale blue box becomes a dark blue one), and text or a link changes only where it would no longer read against what is behind it. Images, and whatever sits on a background picture that shows and spans at least half its box's width, keep their colours, and so do borders; a background picture that is not shown (remote content not loaded) leaves its box to be adapted like any other, and text beside a small background icon is adapted against what is behind it. The message itself is never changed: saving it or viewing its source gives what the sender sent. **Original colours**, beside the Text | HTML switch, and Message ▸ Show original colours show the open message on a white page in its sender's colours, until you open another; `html_colours = "original"` under `[ui]` in `config.toml` does that for all mail (see [Accounts](accounts.md#appearance)).

![An HTML message adapted to the dark theme](https://raw.githubusercontent.com/postvak-app/postvak/main/tests/snapshots/gui_html_dark.png)

HTML over 2 MB, or a page too tall or too broken to lay out, shows as text with a note saying why. Sending mail comes later.

## Reader

The subject heads the message, then the sender's initials on a colour picked from their address, their name and address, the recipients and the date. With `[ui] sender_images = true`, mail that passed DMARC shows the logo its sender's domain publishes in place of the initials (see [Accounts](accounts.md#appearance)).

On the right of the action buttons, **Save .eml** writes the message exactly as the server sent it to your Downloads folder (or your home folder when there is none), named after its subject; when that name is taken it saves beside it as "… (2).eml" and never replaces a file. File ▸ Save .eml as… and the message's right-click menu pick the place in the save dialog instead. The line under the attachments says where it went. Until the message is downloaded this button and View source are greyed out and say why on hover. The reader shows "Loading…" meanwhile, or, while the account is offline, that the message loads when it reconnects. A message larger than 256 KB shows its text, its HTML and the images inside it as soon as those have arrived, and lists its attachments with their sizes; their Save buttons stay greyed out until the rest of the message has downloaded. Above 5 MB the rest stays on the server: Save downloads just that attachment, and Save .eml or View source downloads the whole message first.

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
