# Reader: text first — design

Status: implemented, 2026-10-25. Issue #318, in two PRs: text first, then attachments on demand.

## Goal

Opening a message the store has no copy of shows its text and HTML as soon as those parts have arrived, instead of after the whole message, every attachment included. A mail with a 20 MB scan shows its two lines at once.

## Surfaces

GUI only. This is a named exception to the surface parity rule: the CLI's `show` and the MCP read tools answer one request with the whole message (`--raw`, attachments), so they keep fetching it at once.

## Decisions

- **When.** A message larger than 256 KB (its `RFC822.SIZE`) that the store has no raw copy of. Below that, or with no known size, one `fetch_raw` as before: one round trip beats several.
- **The preview.** The daemon fetches `BODYSTRUCTURE`, then the text parts the indexer would fetch (the plain part of each alternative), the first HTML part outside an enclosed message, and the parts with a `Content-ID` that HTML names as `cid:`. Each is one `fetch_part`.
- **Where it lives.** The text goes into the store as `body_text` and the attachment names with it, through `set_indexed`, exactly as the indexer would store them; the store still only ever holds whole raw messages. The HTML and its `cid:` parts travel to the window in a `BodyPreview` event and are kept in its memory until the whole message is stored; their bytes are base64 on the wire.
- **Then the rest.** In the same command the daemon fetches the whole message and stores it, then sends `BodyReady` as before. The window reads it from the store, and drops the preview.
- **Attachments meanwhile.** Listed from `BODYSTRUCTURE`, with their size estimated from the transferred size; Save and Save as… are greyed out until the whole message is stored.
- **Failures.** A preview that fails for any reason but a lost connection falls back to the whole message, as before; a lost connection ends the command like any other.
- **Only on request.** `FetchBody` carries `preview: true` from the window; the CLI and MCP send `false` and get one `fetch_raw`.
- **Rules and backups unchanged.** Anything that needs the whole message still calls `ensure_raw`.

### Attachments on demand

- **When.** A previewed message larger than 5 MB is not fetched whole after its preview: `BodyPreview` says so (`whole: false`), and `BodyReady` follows at once. The window keeps the preview until the message is stored.
- **Attachments.** Save and Save as… stay enabled. Each sends `FetchAttachment` with the attachment's number as the reader lists it; the daemon fetches the structure and that one part, undoes its transfer encoding, and answers `AttachmentFetched` with its name and contents (base64 on the wire). Nothing of it is stored. A stored message answers from the store without the server.
- **The whole message.** Save .eml, Save .eml as… and View source send `FetchBody` without preview, say "Downloading the message…", and do what was asked once it is stored.
- **Names.** The window makes the sender's file name safe, as for a stored message.
- **Evicted previews.** A preview pushed out of the window's four is asked for again when its message shows again.
- The CLI's `attachment save` and the MCP tools keep reading the whole message.

## Tests

- `RecordingOps`: a large multipart message sends `BodyPreview` with its HTML and `cid:` image after `BODYSTRUCTURE` and part fetches, before any `fetch_raw`; then `BodyReady`.
- Below the threshold, or without `preview`: a single `fetch_raw`.
- A part fetch that fails halfway falls back to the whole message; a lost connection ends the command.
- The structure: `cid:` parts, nested text parts and the attachment list from real `BODYSTRUCTURE` replies.
- The window: a preview shows the HTML and lists the attachments with Save greyed out; `BodyReady` shows the stored message and enables it.
- `imap_live.rs`: a message with a large attachment opens with only its text transferred first.
- Above 5 MB: no `fetch_raw` after the preview; `FetchAttachment` fetches only that part, or reads a stored message; an unknown number fails. In the window, Save fetches the attachment and writes it to Downloads, and Save .eml downloads the whole message first. Live: a 10 MB message's attachment fetched on its own equals the one in the whole message.
