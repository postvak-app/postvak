# Folder names on the wire and in rules

Issue #347. How Postvak spells a folder name to the server, which move targets it refuses, and what it warns about. How rules *use* folders (subscribing, `\Noselect`, case, special-use names) is #348.

## Modified UTF-7

- `mail_ops/utf7.rs` encodes and decodes RFC 3501 §5.1.3 modified UTF-7. `ImapOps` encodes every name it sends (SELECT, STATUS, CREATE, SUBSCRIBE, MOVE/COPY, APPEND, DELETE) and decodes every name it lists. Everything above `MailOps` (store, rules, GUI, CLI, MCP) sees UTF-8.
- `decode` accepts only what `encode` writes, so `encode(decode(wire)) == wire` and the server always gets back the name it listed. A listed name that is not canonical modified UTF-7 (raw UTF-8 from a lax server, two adjacent sections, non-zero padding bits) is kept as it came, and `ImapOps` remembers it for the connection and sends it back unchanged.
- `UTF8=ACCEPT` (RFC 6855) is not used: modified UTF-7 works on every server, and one spelling is simpler.
- Existing stores hold encoded names. Migration 017 runs a Rust hook (`store::decode_folder_names`) once, inside the migration's transaction: each folder whose name decodes is renamed, with its messages, rule log and rule moves, so it keeps its state instead of syncing again. A placeholder (`uidvalidity = 0`) left under the decoded name by a move that sent raw UTF-8 gives way. When a synced folder already holds the decoded name, the encoded row and its messages go, and the next sync refetches what the server lists.

## Move targets and watched folders

Validation (`rules::compile`), by levels between `/`, the delimiter the docs teach:

- Any level of a `folder` or `move` target that starts or ends in whitespace is refused.
- A `move` target may not contain `*` or `%` (LIST wildcards, refused by many servers in CREATE), nor an empty level: a leading, trailing or doubled `/`. A trailing delimiter makes CREATE declare only a parent (RFC 3501 §6.3.3).
- The 255-character cap stays as a guard against runaway input. IMAP sets no limit on length or depth and servers do not advertise theirs, so Postvak invents none; the server's refusal is reported (below).

Direct actions (`postvak move`, MCP `move`, the GUI's Move to) are not validated this way; the server's answer, with the create error, is what the user sees there.

## Warnings

`rules::folder_warnings` reads each configured account's store (only one that exists; a check creates none) and warns, per move target and account the rule runs on, when the account does not have the folder yet and:

- the target contains `/` while the account's delimiter (INBOX's, else the first folder's) is something else: the server would create one folder with `/` in its name, or refuse it. Postvak does not rewrite `/`, since it is a legal character on a `.` server and the same rules.toml can serve accounts on other servers;
- every synced folder of the account other than INBOX lives under `INBOX` + delimiter (`folder_name::under_inbox`, which the GUI's folder tree also uses to drop that prefix) and the target does not.

Placeholders a move left (`uidvalidity = 0`) do not count. These are warnings, not errors, shown wherever the other rules warnings are: `rules check`, MCP `rules_check` (only visible accounts), the Rules view and the daemon log.

## Create errors

A move to a folder the store does not know creates it first. When CREATE fails the move is still tried, since the folder may exist before its first sync. When the move fails too, its error carries the create error: `…; creating the folder failed first: …`, in rule results and direct-action results alike.

## Unicode normalization

RFC 9051 §5.1 and RFC 6855 §3 want mailbox names in NFC; RFC 3501 says nothing.

- A new folder is created in NFC.
- A rule's watched folder or move target matches a known folder in NFC (`folder_name::same`), and the server's own spelling is sent: a folder another client created in NFD is still reached.
- Case still counts, apart from what #348 does for INBOX.
