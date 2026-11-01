# Rules on attachments and MIME parts

A rule could not see whether a message has attachments or of what kind, so "file invoices that come as a PDF", "keep invitations in INBOX" or "flag archives from unvouched senders" could not be written; `larger_than` cannot tell a PDF from an inline image (#497).

## Conditions

- `match.has_attachment`: bool. An attachment is what the search index's `attachment_names` holds: every named part that is not body text, inline images and the parts of an enclosed message included, so rules, search's `attachment:` and the reader's chips agree.
- `match.attachment`: a text condition (`contains`, `equals`, `regex` or a text `list`) on the file names; any name may match.
- `match.has_part`: `type/subtype` or `type/*`, compared lower-cased against the type of every part but a multipart, at any depth, the `message/rfc822` part itself included. `*/*`, `*/x` and a `*` inside a subtype are refused.

## Structure

The store keeps `part_types` (migration 026), one type per line, beside `attachment_names`. `part_types IS NULL` means the structure is unknown; then a rule that reads it does not match, as an unknown body does, so `none` cannot turn it into a hit. Both columns come from the same `BODYSTRUCTURE`, or from the whole message when the server gives none and the message is small enough.

- The indexer stores both on every pass.
- For new mail in a folder whose rules read the structure, the rules pass fetches `BODYSTRUCTURE` alone, or reads it from the stored message when one is there.
- Mail indexed before migration 026 has text but no part types. The indexer's backlog takes it (`RESTRUCTURE`, partial index `messages_restructure`) once its last pass is a day old, and fetches its structure alone, not its text. A message whose structure cannot be had is tried again a day later.
- A folder's messages are read with the indexed columns only when a rule reads the body or the structure (`CompiledRule::reads_index`), so other folders keep the cheap read.

## Surfaces

rules.toml is shared, so the CLI and MCP take the keys through the schema. Attachment names are already searchable without the MCP `read:bodies` scope, so these keys need none. The window's rule editor offers them as "has attachment", "attachment name" and "has part".
