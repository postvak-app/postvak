# Search

Search finds mail in the local store, so it answers as you type and works offline. The same query means the same thing on the CLI (`postvak search`), over MCP (`search`) and in the mail window (`/`).

## What is searched

Every message's subject, sender, recipients (To, Cc and Delivered-To), attachment file names and body text. Words match whole words with accents folded, so `cafe` finds `café`; the last word of a query also matches as a prefix, so `inv` finds invoices while you type. Results come newest first.

| Written | Means |
|---|---|
| `invoice acme` | both words, anywhere in the message |
| `"status update"` | the phrase, in that order |
| `-newsletter` | without the word |

Nothing you type is read as query syntax: a bare address, a colon or an unbalanced quote all search as words. (Operators such as `from:` and `after:` are planned; see the design in the repository.)

## The index

Bodies are searchable once the background sync has read them. After an account is added, the daemon indexes its messages in idle time, newest first: for each message it fetches the structure and only the text parts, and stores the text and the attachment names. It never downloads attachments for the index, and it keeps the raw message only for mail you open. A slice of indexing lasts at most five seconds; between slices the daemon checks INBOX and waits a second for new mail, so notifications arrive as before, at most a few seconds later while the first index builds. `postvak status` and the window's status line show how far it is: "up to date · indexing 12,345 of 48,210".

Indexing happens while the daemon runs: while the window or another client is open, or always with the service installed (see [Background sync](daemon.md#always-on)). A mailbox of a few thousand messages is done within minutes; a large archive takes longer, and search already covers everything indexed so far. Until the index is complete, Enter in the window and `--server` on the CLI also ask the mail server, which searches the bodies not indexed yet.

`index_bodies = false` in an account's `[[accounts]]` entry turns this off (see [Accounts](accounts.md)). Then only the subject, addresses and the bodies of mail you opened are searched, and the server search covers the rest.

With bodies indexed, `mail.db` under the state directory holds the text of every message of the account, in the same private directory as the headers it already held.
