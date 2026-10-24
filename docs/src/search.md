# Search

Search finds mail in the local store, so it answers as you type and works offline. The same query means the same thing on the CLI (`postvak search`), over MCP (`search`) and in the mail window (`/`).

## What is searched

Every message's subject, sender, recipients (To, Cc and Delivered-To), attachment file names and body text. Words match whole words with accents folded, so `cafe` finds `café`; the last word of a query also matches as a prefix, so `inv` finds invoices while you type. Results come newest first.

| Written | Means |
|---|---|
| `invoice acme` | both words, anywhere in the message |
| `"status update"` | the phrase, in that order |
| `-newsletter` | without the word |
| `from:acme`, `to:alice`, `cc:lists`, `subject:invoice`, `attachment:pdf` | the word in that column; `to:` covers To, Cc and Delivered-To; `subject:"status update"` is a phrase; `-from:acme` excludes |
| `from:me`, `to:me`, `cc:me` | your own address or one of your aliases (see [Accounts](accounts.md#aliases)) |
| `in:Archive`, `in:"Projects/Postvak"` | one folder |
| `has:attachment` | with an attachment, as guessed from the headers |
| `is:unread`, `is:read`, `is:flagged` | by flag |
| `before:2026-01-15`, `after:2026-01-01` | by date, in local time; `before:` is exclusive of that day, `after:` inclusive |
| `after:7d`, `before:3w`, `after:6m`, `after:1y` | days, weeks, months or years back from now |
| `account:work` | one account, where several are searched: All inboxes in the window, and the CLI and MCP without `--account` |

Operators combine with words in any order: `from:acme after:30d invoice` is the invoices Acme sent in the last month. A column operator's value is matched whole, never as a prefix and never widened to a close match. Nothing you type is read as query syntax: a bare address, an unbalanced quote, an unknown key such as `size:10` or a date that does not parse all search as words. A `-` before `in:`, `is:`, `has:`, `before:`, `after:` or `account:` is ignored.

Keys are case-insensitive. A search of operators alone, such as `is:unread from:me`, lists every message they describe; `-newsletter` alone lists nothing, as there is nothing to exclude it from.

## The index

Bodies are searchable once the background sync has read them. After an account is added, the daemon indexes its messages in idle time, newest first: for each message it fetches the structure and only the text parts, and stores the text and the attachment names. It never downloads attachments for the index, and it keeps the raw message only for mail you open. A slice of indexing lasts at most five seconds; between slices the daemon checks INBOX and waits a second for new mail, so notifications arrive as before, at most a few seconds later while the first index builds. `postvak status` and the window's status line show how far it is: "up to date · indexing 12,345 of 48,210".

Indexing happens while the daemon runs: while the window or another client is open, or always with the service installed (see [Background sync](daemon.md#always-on)). A mailbox of a few thousand messages is done within minutes; a large archive takes longer, and search already covers everything indexed so far. Until the index is complete, Enter in the window and `--server` on the CLI also ask the mail server, which searches the bodies not indexed yet.

`index_bodies = false` in an account's `[[accounts]]` entry turns this off (see [Accounts](accounts.md)). Then only the subject, addresses and the bodies of mail you opened are searched, and the server search covers the rest.

With bodies indexed, `mail.db` under the state directory holds the text of every message of the account, in the same private directory as the headers it already held.
