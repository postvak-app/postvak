# Sender logos (BIMI) — design

Status: implemented (#201), 2026-10-18. Issue: postvak-app/postvak#162.

## Goal

Mail from a bank, a shop or GitHub should be easy to spot, and a fake one easier to notice. When a sender's domain publishes a logo through BIMI (Brand Indicators for Message Identification) and the message passed DMARC for that domain, the reader shows the logo in place of the initials.

## Surfaces

GUI only. This is the named exception to the surface parity rule: a logo is a picture in the reader's header and the message list's avatars, and the CLI and MCP have nowhere to show one. Nothing changes in `help`, `output` or the daemon wire protocol.

## Decisions

- **Off by default.** `[ui] sender_images = false`. Turning it on means a DNS lookup and an HTTPS fetch to the sender's domain, which tells that domain you received their mail. `docs/src/accounts.md` says so.
- **DMARC from the topmost `Authentication-Results` only.** The receiving server adds that one last. Lower ones can come from anyone, so a forged `dmarc=pass` lower down is ignored. A `header.from` naming another domain is not a pass for the From domain. This relies on the server adding its own results; with a server that adds none, the topmost header is whatever the sender wrote. Acceptable for a first version.
- **One From address, a real host name.** Several From addresses, IP literals and single-label names get no logo.
- **The exact From domain.** BIMI's fallback to the organizational domain needs the public suffix list; it is left out for now.
- **`BIMI-Selector` picks the record** when it is `v=BIMI1; s=<label>`; otherwise `default`.
- **No VMC check.** The `a=` certificate is not verified, so any domain that passes DMARC can show whatever logo it publishes. Worth revisiting.
- **No migration.** The brand comes from the shown message's stored headers when it opens.
- **No logging.** A log line would name who mails you. A failed lookup shows the initials for the session and is not written to the cache, so the next start tries again.

## Library: `postvak-core::bimi`

- `brand(headers) -> Option<Brand>`: the record a message may show, `<selector>._bimi.<domain>`.
- `location(txt records) -> Option<String>`: the `l=` of the one `v=BIMI1` record, when it is an `https` URL that passes `net::public`. None when there is no record, more than one, or an empty `l=` (declined).
- `logo(cache_dir, brand, now, &dyn Net) -> Result<Option<svg>>`: answers from `cache/bimi/<record>.json` within a week. A missing record, a missing logo, or one over 32 KiB or not SVG text is cached as None.
- `Net` is the trait tests answer. `cache_logo` writes an answer, for fixtures.

## GUI: `postvak-gui::logos`

- `Internet` implements `Net`. TXT goes through `hickory-resolver` with the system's resolvers, the name fully qualified so search domains are never tried. The GET is `html::fetch::get`, the downloader HTML mail uses: public hosts only, checked on every redirect, never logged. A body over 32 KiB counts as no logo.
- `Logos` holds per brand a pending, ready (the SVG) or no logo. One worker thread looks brands up in turn and wakes the window. Tests swap `Logos::net`; the fixture uses an offline one.
- `load_body` takes the brand from the message's headers and asks for its logo. `tick` takes the worker's answers.
- The list reads a row's brand only once the row is in view (`list::visible` over the reported viewport), from its headers alone (`Store::headers`, as the thread summaries leave headers out), once per row and list. Sent and Drafts show the recipient, so their rows get no logo. `Row::logo` carries the record name and SVG; the list model draws each record's SVG once.
- `body::Pane` carries the record name and SVG. `bind` draws it with Slint's own SVG support (`Image::load_from_svg_data`), only when the record changes. An SVG Slint cannot draw shows the initials.
- `Avatar` in `common.slint` shows the image, in the reader and on each row. The software renderer does not clip to a rounded rectangle, so a path in the colour behind the avatar (the reader's background, or the row's fill) paints over the corners outside the circle.

## Dependencies

- `hickory-resolver` (no default features; `system-config`, `tokio`): DNS TXT, which std can't do. `tokio` is already a workspace dependency.

## Tests

- `bimi`: DMARC parsing (comments, other domains, a forged lower header), the selector, domain and location checks, and the cache flow with a fake `Net`.
- `logos`: the brand only with the setting on; a cached logo comes in once; a failed lookup caches nothing.
- `list`: which rows are in view, strips included; a row in view gets its logo, the others are not looked up; the window draws a row's logo.
- `store`: `headers` reads one message's headers.
- `body`: the pane carries the logo; no logo without a DMARC pass or with the setting off, and no worker then; the window draws the logo once.
- Snapshot `gui_sender_logo`.
