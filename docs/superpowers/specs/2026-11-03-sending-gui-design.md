# Sending mail, part 3: the GUI composer

Postvak can send from the command line (part 1, `2026-11-02-sending-design.md`) and from a rule (part 2), but the window still answers mail by opening another app (#372). This part adds the composer to the GUI: replies and forwards in the reader pane with the original drawn below, new mail in a window of its own, drafts in the server's Drafts folder, signatures, and the undo toast. It builds on part 1's outbox, builders and checks, and changes none of what they send.

## Goals

- Reply, reply all, reply to list, forward and write new mail from the window, from the account's address or an alias.
- Write with the original in view as the reader shows it (HTML, images, dark theme), not as `> ` lines to scroll past.
- Take a sent message back during the undo delay with Undo or `z`, as with any other action in the window.
- Drafts that live on the server, so they show on the user's other devices and survive this machine.
- Signatures per address, and the undo delay, set in Preferences.

## Non-goals

- Writing HTML or rich text. The user writes plain text, as in part 1; an HTML composer is a later part.
- Drafts or sending over MCP (part 4; MCP never sends).
- Registering Postvak as the desktop's `mailto:` handler. A follow-up, since it is a desktop integration that has to ship on every platform and package at once.
- Spell checking, address-book completion beyond the addresses in the user's own mail (below), templates, scheduled send.

## Anti-goals

- Losing what the user typed: to a crash of the window, a click elsewhere in the list, a failed send or an undo.
- A message that differs from what the CLI would send for the same input: the GUI calls part 1's builders and checks; it builds nothing of its own.
- A draft left twice in Drafts, or a sent message whose draft stays behind.
- Recipients, subjects or bodies in the log.

## The composer

One component, used in two places:

- **Reply, reply all, reply to list, forward**: in the reader pane, replacing the message view. The composer takes the top of the pane; the original is drawn below it, as the reader draws it (the same HTML view, its images, remote content as the reader had it, the dark theme), behind a `Splitter` the user can drag. The split is saved with the window (`persist`), like the other splitters. The composer and the original scroll on their own. The original is shown, never edited; the quote that goes into the sent message is still part 1's (`> ` text and the HTML blockquote), built when it is sent.
- **New mail**: a window of its own, without an original. Opened from the toolbar, the menu and `Ctrl+N` (`Cmd+N`), and from a `mailto:` link in a message, which opened the mail app until now (my call: Reply did too, and both now stay in Postvak).

Its fields, top to bottom:

- **From**: a drop-down of the account's address and its aliases with a name, as part 1 sends them (`display_name`). It starts on part 1's `our_address` for a reply or forward, else the account's. A wildcard alias can be typed as a complete address. New mail picks the account of the folder in view, else the first; the drop-down lists every account's addresses (my call: changing the account of a new message is common, of a reply rare, so a reply lists its own account's only).
- **To, Cc, Bcc**: Cc and Bcc show on request, and whenever they hold an address. Addresses complete from the user's own mail and nothing else: the To and Cc of every account's Sent folder first (people the user wrote to), then the From of stored mail, each ranked by its latest message. This is a new store query over the `from_addr`, `to_addr` and `cc_addr` columns; `senders` groups From only. Each address is checked as it is left, with part 1's check, and a bad one is marked in place.
- **Subject**, filled by part 1's prefix rule for a reply or forward.
- **The text**: plain, wrapped by the window, sent as part 1 sends it (`format=flowed`). A reply and forward start empty above where the quote will go; the signature (below) is already in it.
- **Attachments**: added with a button that opens the desktop's file dialog (the FileChooser portal's `OpenFile` on Linux, as `save_as` uses its `SaveFile`; `NSOpenPanel` on macOS), by the button only: Slint 1.18.1's winit backend takes no file dropped from the desktop, so dropping files on the composer is a follow-up. Each shows with its name, size and a remove button. A forward lists the original's attachments the same way, all kept by default, each removable; `as attachment` (a toggle beside them) sends the original untouched instead, as part 1 does.
- **The buttons**: Send (`Ctrl+Enter`), Save draft (`Ctrl+S`), Close, and Discard. Close leaves a draft where it is; Discard throws the composer away and, on an opened draft, deletes it from Drafts.

There is at most one composer in the reader pane, and any number of new-mail windows, each with its own state (my call: as in other clients). Starting a reply while one is open in the pane asks first, as leaving does.

A forward's single attachments are removable, so part 1's `AttachmentFilter` (`All` or `None`) gains a list of the parts kept; part 2's rule forward goes on passing `All` or `None` through it.

The checks run when Send is pressed, and the first one that fails is shown on the field it is about; nothing is queued. The size check counts the attachments as they are added and says so before Send (my call: finding out at Send that 40 MB is too much is late).

### Leaving the composer

What the user typed is never thrown away without asking:

- Discard asks first, unless nothing was typed or changed since it opened; on an opened draft it always asks, since it deletes the draft.
- Close (and `Escape`) closes at once when nothing changed since the last save, and otherwise asks as below.
- Opening another message, changing folder, or closing a composer's window with an unsaved composer asks: Save draft, Discard, or Cancel (my call; Gmail saves silently, but a server round trip on every click in the list is the wrong default here, and the question names what happens).
- The window saves the composer to a local file in `paths`' state directory every few seconds while it is open and unsaved (my call), so a crash of the window loses nothing: on the next start, a composer left that way opens again with a line saying so. The file is removed when the composer is sent, saved as a draft or discarded. It holds only what the composer holds and lives with the outbox, which already holds whole messages on disk. There is one file per open composer.
- Quitting (from the tray or the menu, or closing the main window without a tray) does not ask: it writes every unsaved composer's file once more, and the next start opens them again, as after a crash (my call: a question per composer while quitting is easy to dismiss wrongly, and this loses nothing). Hiding the main window to the tray keeps the composers as they are.

## Sending and undo

Send builds the message with part 1's functions (`build`, `reply`, `quote_html`), runs part 1's checks, and queues it with `Command::Send`, so it goes through the outbox and the account's sender thread like `postvak send`. The undo delay is `[sending] undo_secs` (0 sends at once).

- The composer closes, and the toast says "Sending to Linus Example in 10 s" with Undo, counting down; `z` undoes it like any toast in the window (`undo`). When the delay is 0 the toast says "Sent" without Undo.
- Undo sends `CancelSend`. When it comes back in time, the composer opens again where it was (a reply in the reader pane, asking first if another composer took the pane meanwhile; new mail in a new window), with everything as it was: the GUI keeps the composer's state until the message has left the `waiting` state, rather than parsing the returned message back. When it is too late, the toast says the message is already on its way.
- Closing the window during the delay does not stop the message: the daemon owns it.
- The original's row in the list shows a pending mark while a reply or forward to it waits, and `\Answered` or `$Forwarded` once part 1's flag path has set them (the list already shows flags; my call: the pending mark is a small clock beside the subject, matched on the original's Message-ID from the outbox).
- A message that fails shows in the status bar as part 1 already does, now as a button that opens the outbox: the waiting and failed messages of every account, with Retry, and Edit, which takes a failed message back from the outbox and saves it as a draft, opening it in the composer (my call: a failed message from the GUI must not be a dead end, and the outbox cannot be edited in place). The CLI's `postvak outbox` is the same list.

## Drafts

A draft lives in the account's Drafts folder on the server: the folder with special-use `\Drafts`, else one named `Drafts` (compared as `folder_name` does), as part 1 finds Sent. Without one, Save draft is refused with "This account has no Drafts folder" and the composer stays open; Postvak does not create one (my call).

- **Saving** goes through the daemon as a new `Command::SaveDraft { account, raw, replaces }`: the IMAP thread APPENDs the draft with `\Draft` and `\Seen`, and only once that succeeded deletes the copy it replaces (found by its Message-ID in Drafts; expunged, not moved to Trash). Both happen before the reply, so a failed APPEND leaves the old copy, and the composer stays open saying so. Every save gives the draft a new Message-ID (my call: a server is free to drop a second message with the same ID, and Gmail does).
- **What a draft holds** is the message as part 1 would send it, built by the same functions, with the quote: a draft opened in another client is the whole message. Postvak adds one header, `X-Postvak-Draft`, naming the kind (reply, reply all, reply to list, forward inline or as attachment) and where the user's text ends, so opening it again shows the text and the original separately rather than the quote in the editor. A draft without that header, such as one from another client, opens with its whole text part in the editor (HTML-only: converted to text, with a line saying so) and its attachments listed; a reply's original is found by In-Reply-To in the store when it is there.
- `X-Postvak-Draft` names the original by its Message-ID, for a forward too. Opening or sending a Postvak draft looks the original up in every folder of the account's store. When it is not there (a rule deleted it, the draft came from another device, its folder is not synced), the draft opens as one from another client: its whole text, quote included, in the editor and its attachments listed, sent as new text with In-Reply-To and References kept (my call: nothing is lost and nothing needs rebuilding from a message Postvak cannot see).
- **Opening** a message in Drafts shows an Edit draft button in the reader, and Enter on it in the list opens it too: a reply or forward in the reader pane with its original below, new mail in its own window.
- **Sending** a draft queues it as any message, and its outbox entry names the draft to delete. The draft is deleted once the message is `sent`, by the IMAP thread like the Sent copy and the flag on the original (part 1's work handed to the sync thread), so an undo or a failure leaves the draft where it was.
- **Discarding** an opened draft deletes it from Drafts, after asking, through `SaveDraft`'s delete path; Close leaves it.

Part 4's agent drafts use the same folder and command.

## Signatures

Per address, in config.toml, written only by `config` (a new `save_signature`):

```toml
[[sending.signatures]]
address = "pieter@example.com"   # or an alias pattern, as in `aliases`
text = "Pieter Willekens\nExample BV"
on_replies = false               # default true
```

- The composer puts the signature of the From address under a `-- ` line (RFC 3676's separator, `-- ` with the space) at the end of the user's text, and swaps it when From changes, as long as the user did not edit it; an edited signature stays.
- A reply or forward leaves it off when `on_replies = false`.
- `postvak send` adds it too, with the same rule, in the editor file and to text from stdin or `--body-file`, unless `--no-signature` is given (my call: one setting, one behaviour, on every surface; `--raw` never adds one). MCP gets nothing, since it does not send.

## Preferences › Sending

A new page in Preferences, written through `config`:

- Undo send: Off, 5, 10, 20 or 30 seconds (`[sending] undo_secs`, a new `save_undo_secs`).
- Signatures: one per address of every account, edited in place, with "Also on replies and forwards".
- Per account, the SMTP server: what is used (from config.toml, or the stored guess, saying which), and a Save button for a guess, doing what `postvak account smtp <name> --save` does; Guess again, as `--guess` does.

## Adding an account

The add-account form shows the SMTP server the daemon guessed once the IMAP login test passed, beside the IMAP settings, with Change to type it, as `--smtp-host`, `--smtp-port` and `--smtp-tls` do. When no guess works, the form says "Sending not set up" with a link to the docs, and the account is still added.

## Keys

- `r` reply, `Shift+R` reply all, `l` reply to list, `f` forward, in the list and the reader, as other clients bind them (none is taken today; `Ctrl+R` stays Sync now).
- In the composer: `Ctrl+Enter` send, `Ctrl+S` save draft, `Escape` closes as Close does.
- `Ctrl+N` new mail.

## Surfaces

Part 3 is the GUI's. The CLI already sends; it gains `--no-signature` and the signature itself. The exceptions to shipping on every surface at once: drafts are GUI-only until part 4 (MCP), and the CLI gets no draft commands (its editor file is its draft); MCP gets none of this. `help` holds the new CLI text; the outbox dialog's rows read the same `output` rows as `postvak outbox --json`.

## Docs

- `docs/src/gui.md`: replying and forwarding, new mail, drafts, the undo toast, the outbox dialog, the keys.
- `docs/src/sending.md`: signatures, Preferences › Sending, and that drafts live in the server's Drafts folder.
- `docs/src/compare.md` drops the composer from what is missing; HTML writing stays there.
- The CLI page regenerates (`POSTVAK_BLESS=1`).

## Tests

- Without a toolkit (the GUI's view modules): the composer's state from a reply, reply all, reply to list and forward of fixture messages; From's starting address; the signature swap on a From change and its `on_replies` rule; when Discard and leaving ask; the draft's `X-Postvak-Draft` header written and read back, and a foreign draft opened.
- That the GUI's message equals the CLI's: the same input through the composer and through `send::compose` gives the same bytes, Date and Message-ID aside, for new mail, each reply kind and both forwards (not for a draft whose original is gone, which has no CLI equivalent).
- Address completion: Sent recipients before senders, ranked by latest message, across accounts.
- Daemon: `SaveDraft` APPENDs before it deletes, keeps the old copy when APPEND fails, refuses without a Drafts folder; sending a draft deletes it once `sent`, not when cancelled or failed (`RecordingOps`).
- Window (`Fixture::window()`): Reply opens the composer above the original; Send shows the toast and Undo reopens the composer as it was; `z` does the same; leaving with an unsaved composer asks.
- Snapshots (`TZ=UTC UPDATE_SNAPSHOTS=1 cargo test -p postvak-gui snapshots`): the reply composer over an HTML original, light and dark; a forward with its attachments listed; the new-mail window; Preferences › Sending; the outbox dialog.
- Live: `SaveDraft` and replacing a draft against the Dovecot servers in `tests/imap_live.rs`, with and without UIDPLUS.

## Left to the builder

The autosave interval and its file format; the outbox dialog's layout within the grid; the wording of the toasts, questions and refusals; the pending mark's glyph; how `X-Postvak-Draft` encodes where the user's text ends; how the composer's text widget wraps and scrolls within Slint's `TextEdit`; the order the work lands in, which may be several PRs (my suggestion: the composer and sending first, then drafts, then signatures and Preferences).
