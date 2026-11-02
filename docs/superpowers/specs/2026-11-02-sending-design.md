# Sending mail, part 1: SMTP, the outbox and `postvak send`

Postvak reads and sorts mail; it cannot reply, forward or write one (#372). Sending ships in four parts, each with its own spec; this one is part 1, the base the others build on: an account's SMTP settings, the daemon's outbox, and `postvak send` for new mail, replies and forwards. The decisions for the later parts are kept at the end so they are not lost.

## Goals

- Send new mail, a reply (to the sender or to all) and a forward from the command line, from the account's address or an alias.
- An undo delay: a sent message waits in the daemon's outbox and can be cancelled until it goes.
- A copy in Sent, unless the provider files one itself.
- SMTP settings that need no typing for the common provider, for new and existing accounts.

## Non-goals

- Writing HTML. The user writes plain text; a rich-text composer is a follow-up after part 3. The one HTML Postvak sends is the wrapper around a quoted HTML original (see Quoting an HTML original).
- The GUI composer, signatures, Preferences › Sending and the undo toast (part 3).
- Drafts and anything over MCP (part 4). MCP gets no tool to send, now or later.
- The forward rule action (part 2), though its forward builder is written here.

## Anti-goals

- A guessed SMTP server that receives the user's password over plain text, or on a host outside the account's own domain.
- A message that goes out other than as the user meant it: a From the account does not own, a header Postvak rewrote, a Bcc recipient visible to the others.
- A sent message that is lost because the daemon stopped.
- Recipients, subjects or bodies in the log.

## SMTP settings

`config.toml` gets an optional table per account:

```toml
[accounts.smtp]
host = "smtp.example.com"
port = 465              # default by tls: 465 implicit, 587 starttls
tls = "implicit"        # implicit | starttls; no "none"
username = "me"         # default: the account's IMAP username
password = { keyring = true }  # default: the account's IMAP password source
save_sent = true        # default: guessed, see Sent copy
```

No `smtp` table means sending is not set up for the account, unless a guess for it is stored (below). `config` stays the only writer (`save_account`). An SMTP password of its own in the keyring has its own entry, `<account>/smtp`; without one, `password` follows the IMAP source and reads the IMAP entry.

**Guess, check, save.** When an account is added, and once for every existing account without `smtp` when the daemon starts, the daemon tries in turn: `smtp.<domain>` and then the IMAP host itself, where `<domain>` is the IMAP host without a leading `imap.` or `mail.`; on each, port 465 with implicit TLS, then 587 with STARTTLS. A try connects, says EHLO and authenticates with the SMTP login (the IMAP one by default); it sends nothing. The first that authenticates is used. For `account add` it is saved to config.toml with the account. For an existing account the daemon never writes config.toml: it stores the guess in the account's store and sends with it, and `postvak account smtp <name> --save` (later the GUI) writes it to config.toml; an `smtp` table there always wins. A guessed host must have the same registrable domain as the IMAP host (`psl`), and a guess never logs in without TLS. When none works the account stays without `smtp`; `postvak status` and the account's status say "Sending not set up" and point to the docs. The store records the attempt with the IMAP host and login it was made for, so the daemon does not retry on every start; it tries again when either changes. The guess also stores the server's EHLO capabilities (`SIZE`, `8BITMIME`), and so does every later connection.

`postvak account add` takes `--smtp-host`, `--smtp-port` and `--smtp-tls` to skip the guess; `postvak account smtp <name>` sets them for an existing account with the same flags, guesses again with `--guess`, or writes the stored guess to config.toml with `--save`; each is checked by logging in before it is saved.

## Names on From

An account gets an optional `display_name` (its `name` key already names the account), sent as `From: Pieter Willekens <pieter@example.com>`. An entry in `aliases` may be a table, `{ address = "shop@example.com", name = "Example Shop" }`, beside the plain strings it holds today, so existing configs stay valid. A wildcard alias, or an alias without a name, uses the account's `display_name`; without one, From is the bare address.

## The outbox

Every send goes through the daemon as a `Command::Send`, as anything touching a server does. The daemon writes the message to its outbox before it replies, so an accepted send survives a crash. Each account has a sender thread of its own beside the IMAP sync thread (my call): a long sync, a broken IMAP login or IDLE never holds a due message or a cancel back. What needs IMAP afterwards (the Sent copy, the flag on the original) is handed to the sync thread as commands, kept in the outbox until done, and retried when IMAP is back.

- `<state_dir>/outbox/<account>/<id>.eml`, the message as it will be sent (Bcc kept, see Sending), and `<id>.json` beside it: recipients, when it is due, its state (`waiting`, `sending`, `failed` with the reason), whether to save a copy in Sent, and the message it answers or forwards with the flag to set there. Written with `paths`' atomic writes.
- Due = now + the undo delay: `[sending] undo_secs` in config.toml, one of 0, 5, 10 (default), 20 or 30. `postvak send --now` skips it for that message, and `--raw` always does (see Raw).
- States: `waiting`, `sending`, `sent` (the server accepted it; the Sent copy or flag is still to do), `failed` (with the reason). `sending` is written and synced to disk before the message is handed to the server, and `sent` right after the server accepts it.
- When the daemon starts it sends everything already due, at once. A message still `sending` is never sent again: it becomes `failed` with "may have been sent; check Sent before retrying", so a crash can delay or need a check but never sends twice.
- The daemon does not exit for idleness while an account has a message `waiting`, `sending`, a `failed` one still being retried, or `sent` work left.
- `Command::CancelSend { id }` removes a `waiting` message and returns it; once `sending`, it is too late and says so.
- A temporary failure (a 4xx reply, a lost connection) is retried with backoff for an hour, then `failed`; a permanent one (5xx, including any recipient refused) fails at once. Nothing is sent to some recipients and not others: a refused recipient fails the whole message before DATA. A failed message stays in the outbox until retried or cancelled.
- The daemon's status gains the outbox: how many are waiting and failed per account.

The CLI says where the message is: `Sending to Linus Example in 10 s (postvak outbox cancel 3f2a)`. `postvak outbox` lists the waiting and failed ones, `postvak outbox cancel <id>` takes one back (printing it to stdout, or `--json`), `postvak outbox retry <id>` sends a failed one again. MCP and the GUI do not get these in part 1; the GUI's status bar shows "Sending 1 message" and "1 message failed" from the status (the composer and undo toast come in part 3).

## Building and checking a message

`postvak send` takes a message two ways, both checked strictly; a message that fails a check is refused with the reason and nothing is queued.

**Flags.** `--to`, `--cc`, `--bcc` (repeatable), `--subject`, `--from` (default: the account's address), the body from stdin, `--body-file` or the editor (below), `--attach <file>` (repeatable). Postvak builds the message: Date, Message-ID on the account's domain, MIME-Version, `text/plain; charset=utf-8; format=flowed` (RFC 3676: the user's lines wrapped at 72 with soft breaks, quoted lines kept as they are, quoted-printable only where a line still needs it), attachments as `multipart/mixed` with types guessed from the extension (`application/octet-stream` otherwise). Headers with non-ASCII text are RFC 2047 encoded.

**Raw.** `--raw` reads a whole RFC 5322 message from stdin and works as a sendmail replacement for mutt, aerc and git send-email, as msmtp does. Recipients are the addresses given as arguments, and with `-t` (or no arguments) also the message's To, Cc and Bcc; git send-email passes its Bcc that way. Of sendmail's flags, `-i`, `-oi`, `-t` and `-f <address>` are accepted (`-f` must equal From); any other is refused. The account is `--account`, else the one whose address or alias matches From; none or several is refused. Postvak sends the message as it is, except for removing Bcc. A raw message skips the undo delay, and `postvak send --raw` waits for the server's answer, exiting as sendmail does: 0 sent, 75 (`EX_TEMPFAIL`) on a temporary failure, after which the message stays in the outbox and is retried, 69 (`EX_UNAVAILABLE`) on a permanent one. It saves no Sent copy unless `--save-sent` is given, since mutt (`record`), aerc (`copy-to`) and most git setups file their own.

**Editor.** `--edit`, and by default whenever stdin is a terminal and neither `--body-file` nor `--raw` is given, opens `$VISUAL`, else `$EDITOR`, else `vi`, on a file holding `To:`, `Cc:`, `Bcc:` and `Subject:` lines, a blank line, and the body: for a reply or forward, the empty space for the user's text above the quote, which the user may also cut and answer in between. From, In-Reply-To and References are not in the file. The headers read back are checked like `--to`; a file left empty or unchanged aborts and queues nothing, as `git commit` does.

**Checks, both ways:**
- From is one address: the account's, or one an alias pattern matches. With a wildcard alias, the address must be complete (no `*`).
- At least one recipient; every address parses as an addr-spec. A raw message's headers parse with `mail-parser`.
- A raw message has From, Date and Message-ID, CRLF or LF line ends but no bare CR, lines of at most 998 octets, and ASCII-only headers (RFC 2047 encoded words allowed); 8-bit bodies only with a declared charset and a `Content-Transfer-Encoding` of `8bit`, sent only when the server offers 8BITMIME.
- The whole message is under the server's `SIZE` limit, else 25 MB; and 8-bit only where the server offers 8BITMIME. Both are checked when queued against the capabilities stored from the last connection; the server's own answer at send time still decides, and a refusal then makes the message `failed`.

## Reply and forward

`--reply <folder>/<uid>`, `--reply-all <folder>/<uid>` and `--reply-list <folder>/<uid>` answer a stored message; `--forward <folder>/<uid>` forwards it, with `--as inline` (default) or `--as attachment`. The body still comes from stdin, `--body-file` or the editor and goes above the quote, as Gmail, Apple Mail and Outlook place it; in the editor the user can answer in between. The original's text comes from the store, fetched through the daemon (`FetchBody`) when it is not there yet.

- **From** defaults to our address the original was sent to (the identity's match against To, Cc, Delivered-To), else the account's address.
- **Reply**: To is Reply-To or else From; when From is one of ours (a reply to a message in Sent), To is the original To instead, as other clients do. Reply-all keeps the original To in To and its Cc in Cc, minus every address of ours and duplicates. Reply-list sends to the address in `List-Post` alone and is refused for a message without one. Subject gets `Re: ` unless it starts with a reply or forward prefix (case-insensitive: `Re:`, `Aw:`, `SV:`, `Antw:`, `Fwd:`, `Fw:`, `WG:`, `TR:`, `Doorst:`); a forward's prefix becomes `Re:` only when there is none yet. The attribution line is English and fixed: other clients do not parse it. In-Reply-To is the original Message-ID; References its References plus that ID. The quote is the original's text part (its HTML converted to text when it has none), each line prefixed `> `, under `On <date>, <name> wrote:` in the user's locale-free English form; for an HTML original see below.
- **Forward**: Subject gets `Fwd: ` unless it already starts with a forward prefix (`Fwd:`, `Fw:`, `WG:`, `TR:`, `Doorst:`), so Outlook's `FW: x` stays `FW: x`. `inline`: a `---------- Forwarded message ----------` block with From, Date, Subject, To and Cc, the text part, and the original's attachments attached again. `attachment`: the original raw message as a `message/rfc822` part, untouched, so its DKIM signature still holds. No In-Reply-To; References only when `inline`.
- When the message has been sent (not when queued), the original gets `\Answered` for a reply and `$Forwarded` for a forward, through the same flag path as actions. The original is looked up again by Message-ID first, since a rule may have moved it during the undo delay; when it is gone, the flag is skipped. When the server refuses `$Forwarded` (no `\*` in PERMANENTFLAGS), that is skipped silently.

### Quoting an HTML original

A reply, or an `inline` forward, of a message that has an HTML part is sent as `multipart/alternative`, so every client shows the original as close to how it arrived as it can:

- `text/plain`: the user's text, then the `> ` quote as above.
- `text/html`: the user's text, HTML-escaped, as `<p>` paragraphs (`<br>` within one); then the attribution line and the original in `<div class="gmail_quote"><blockquote type="cite" class="gmail_quote" style="margin:0 0 0 .8ex;border-left:1px solid #ccc;padding-left:1ex">…</blockquote></div>`. `type="cite"` gives Apple Mail and Thunderbird their quote bar; the `gmail_quote` class lets Gmail fold it; the inline style is what Outlook shows. A forward puts its header block in the HTML too, above the original, without the blockquote.
- The original's HTML is the content of its `<body>`, with its `<style>` elements kept in front of it. Removed: `<script>`, `<iframe>`, `<object>`, `<embed>`, `<form>`, `<meta http-equiv>`, `<base>`, every `on*` attribute and `javascript:` URL. Nothing else is rewritten; remote images stay remote links, as the original had them.
- Its `cid:` images go along: the HTML part and those images become `multipart/related`, with the Content-IDs kept. Its attachments go along only on a forward.
- A plain-text original keeps the plain-text message; no HTML is made for it.

The parse uses `html5ever` (my call): already in the workspace for the GUI, new to `postvak-core`, and neither Blitz nor Slint, so the architecture test allows it; `message` already finds the HTML part and its `cid:` images.

The forward builder is a library function taking the original and the options (`as`, prefix, note, which attachments), so part 2's rule action calls it with its own options.

## Sending

The account's sender thread sends over SMTP with `lettre` (my call: SMTP client and message builder in one maintained crate, built on rustls with ring like the IMAP side, tokio transport). The envelope sender is the From address. Bcc is removed from the message transmitted; it is kept in the Sent copy, as other clients do. The connection uses the account's `ca_file` too.

## Sent copy

After the server accepted the message, the daemon APPENDs it, with Bcc and flag `\Seen`, to the folder with special-use `\Sent`, else one named `Sent` (compared as `folder_name` does), else none and the status says so once. Whether to: `save_sent` when set; else off for hosts known to file sent mail themselves (Gmail: `smtp.gmail.com`, `*.google.com`; Outlook.com and Microsoft 365: `smtp-mail.outlook.com`, `smtp.office365.com`), on otherwise. A failed APPEND does not fail the send; it is logged and shown in the status.

## Surfaces

Part 1 ships on the CLI only, with the status in the GUI; this is the exception to shipping on every surface at once. The GUI composer is part 3 and MCP never sends (part 4 drafts). `help` holds the CLI texts; `--json` rows for `outbox` and `send` come from `output`.

## Docs

- `docs/src/sending.md`: setting up SMTP, the guess and how to fix a failed one, `postvak send` with examples (mutt, git send-email), the outbox and undo delay, Sent copies.
- `docs/src/index.md` (and so `README.md`) and `docs/src/compare.md`: drop "it does not send it"; compare lists what is still missing (composer, HTML) until part 3 ships.
- The CLI page regenerates (`POSTVAK_BLESS=1`).
- The page says plainly that an agent given a shell can send through `postvak send`.

## Tests

- Unit: the checks (each refusal), the reply and forward builders (headers, quoting, the subject prefix rule, recipient lists), the HTML quote (the structure, each removed element and attribute, `cid:` images kept, a plain original sending no HTML), the guess's candidates and its domain check, outbox state changes and the restart path, Sent folder choice.
- A `SendOps`-style fake beside `RecordingOps` for the daemon tests: queue, cancel, due on start, temporary and permanent failures.
- Fuzz: `--raw` input and hostile HTML originals for the quote go through `tests/fuzz.rs`.
- Live: an SMTP server joins `tests/dovecot/compose.yml` (my call: Dovecot's own `submission` service with relay to a local sink, so the Sent copy and APPEND run against the same Dovecot; if that proves awkward, Mailpit), behind the same `POSTVAK_TEST_IMAP_HOST` switch.

## Left to the builder

The outbox id format, backoff steps, the exact status wording, and how the CLI waits for the daemon's reply.

## Later parts

**Part 2: the forward rule action.** Options: `to` (one or more), `cc`, `bcc`, `from` (default: the address the message was sent to), `subject_prefix` (default `Fwd: `, may be empty, `{placeholders}` as in move targets), `note`, `as` (`inline` | `attachment`), `attachments` (`all`, `none`, or a filter by MIME type, file name or size using the conditions from #520; dropped ones are listed), `max_size` (skip, or send without attachments). Also: `$Forwarded` on the original, a message forwarded once only, a per-rule rate limit, each forward in the action log. A `redirect` mode that keeps the original From is likely not done (SPF, DMARC). Approval: every forward rule needs approval before it runs; adding one in the GUI or CLI approves it, one written in rules.toml by hand or over MCP (`rules:write`) waits for `postvak rules approve <name>` or the GUI. Approval is stored in the store, keyed by the rule's name and its addresses, never in rules.toml; changing `to`, `cc` or `bcc` needs a new one. `docs/src/mcp.md` says what `rules:write` risks. Open: whether a rule's forward waits out the undo delay.

**Part 3: the GUI.** The decisions in #372: the composer in the reader pane for a reply or forward, its own window for new mail; the original shown rendered as the reader shows it (HTML through Blitz, images, dark theme), never as `> ` text in the editor, so writing a reply does not mean scrolling past a converted quote; From with the account's address and aliases; Send (`Ctrl+Enter`), Save draft, Discard; signatures per address in Preferences › Sending (plain text, `-- ` line, optional on replies); Undo send setting and the counting toast with Undo and `z`; the pending mark on the list row. The add-account form shows the guessed SMTP server. HTML sending follows this part.

**Part 4: MCP drafts.** A `mail:draft` scope outside the default set saves a draft to Drafts; no `mail:send`, ever. The GUI marks an agent's draft "By an agent" with the host, offering "Review and send" and "Discard draft"; recipients not in the thread or not in the user's mail are flagged.
