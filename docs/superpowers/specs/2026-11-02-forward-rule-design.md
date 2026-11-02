# Sending mail, part 2: the forward rule action

A rule can file, flag, tag or delete mail, but not pass it on: "send invoices to the bookkeeper", "copy what reaches the shared address to my own" (#372). This adds a `forward` action. It sends through part 1 (`2026-11-02-sending-design.md`): the account's SMTP settings, the outbox, the sender thread and the forward builder, so part 2 is built after part 1.

## Goals

- A rule that forwards matching new mail, with control over recipients, sender address, subject prefix, a note, inline or as attachment, and which attachments go along.
- No forward rule sends anything the user did not see and approve, however the rule got into rules.toml.
- A wrong rule cannot send a flood: a rate limit, no forwarding of existing mail, no loops.

## Non-goals

- `redirect` (resending with the original From): SPF and DMARC reject it too often.
- Catching up: mail that arrived while a rule waited for approval is not forwarded afterwards.
- Forwarding over MCP directly: an agent can propose a forward rule, never send.

## Anti-goals

- Mail leaving because an agent with `rules:write` wrote or approved a rule.
- A rule that forwards half its job: archiving invoices that were never forwarded.
- Hundreds of forwards from one typo or one mail loop.

## The action

```toml
[[rules]]
name = "invoices"
match.from = { contains = "billing@" }
match.has_part = "application/pdf"
actions.archive = true
actions.forward = { to = ["books@example.org"], note = "Invoice for the records", attachments = { type = "application/pdf" } }
```

`actions.forward` is a table:

- `to`: one address or a list; required. `cc`, `bcc`: optional, the same.
- `from`: our address to send as, which must be the account's or one an alias matches, and so needs the rule's `account`: `rules check` refuses a `from` on a rule for every account; default the address of ours the message was sent to (part 1's `our_address`), else the account's.
- `subject_prefix`: default `Fwd: `; may be empty; `{placeholders}` as in `move` (`{subaddress}`). Not added when the subject already starts with a forward prefix (part 1's list).
- `note`: text above the forwarded message.
- `as`: `inline` (default) or `attachment` (the original untouched as `message/rfc822`).
- `attachments`: `true` (default, all), `false` (none), or a table filtering them with the attachment conditions of #520: `type` (`type/subtype` or `type/*`), `name` (a text condition on the file name), `max_size` (bytes or `5MB`). Inline only; with `as = "attachment"` the original goes whole and the key is refused. Attachments left out are named under the forwarded message (`Not forwarded: big.zip (40 MB)`).
- `max_size`: when the whole message is bigger, `skip` (default: not forwarded, logged) or `strip` (forwarded without attachments, named as above). Default limit: the part 1 size limit.
- `rate`: forwards per hour for this rule; default 20.

The schema, `rules check` and the warnings validate it like the other actions; `docs/src/rules.schema.json` regenerates.

## Running a forward

- **When.** Only in `Mode::Normal`: on new mail. In `Mode::ApplyExisting` (`rules apply-existing`, the GUI's apply to existing mail) and in previews and coverage the forward is left out of what runs and shown as `forward (new mail only)`; the rule's other actions run as they would.
- **Order.** Forward comes first in the plan, before delete, trash, junk, moves and flags: the raw message is copied into the outbox, then the other actions run. `apply` today returns early on a delete; the forward is queued before that.
- **`after`.** A forward waits with the rule's `after`, like every action but `notify` and `stop`.
- **Undo delay.** None: a rule forward is due at once.
- **Body.** The raw message is fetched (`fetch_raw`) only for messages a forward rule matches.
- **Cannot queue.** When the forward cannot be queued (the raw message cannot be fetched, the builder refuses the original, the outbox write fails, `from` is not ours on this account), none of the rule's actions run on that message: it stays as it is and the rule tries again on the next pass, as `apply` already refuses a delete it cannot back up. A failure that lasts shows on the rule in the Rules view and in `rules check`. `max_size = "skip"` is the user's own choice, so there the other actions do run.
- **Once.** A message is forwarded by a rule once, recorded when its forward is queued: the store keeps (rule, Message-ID), or (rule, folder, UIDVALIDITY, UID) for a message without one. A message moved back, synced again or matched after an edit is not forwarded again.
- **Loops.** Every rule forward adds `X-Postvak-Forwarded: <our address>`, keeping any the message already has. A message is not forwarded (logged as skipped; its other actions run) when one of those headers holds an address of this account (`Identity::is_me`), when it carries three of them already, or when it has `Auto-Submitted: auto-replied`. So a chain such as shared address → own account → bookkeeper works, and a loop stops at the first repeat.
- **Rate.** A forward over the rule's `rate` in the last hour is not sent; the message gets the other actions. The rule's approval is withdrawn (see below), so the rule waits again, and a notification says `Forwarding paused for "invoices": more than 20 in an hour`.
- **Failure.** A forward goes through the outbox like any send; a failed one stays there with its reason (`postvak outbox`), so nothing is lost when the other actions already moved or deleted the original.
- **Log.** Each forward is an action log row, `forward:<first recipient>` plus the count of others, so `postvak log`, the reader's provenance strip and undo see it. Undo cannot recall a sent forward; it says so and undoes the other actions.

## Approval

Every forward rule needs approval before it runs. Approval is kept in the account stores, never in rules.toml, as the rule's name and its whole definition in the canonical form `rules_seen` already stores. Any change to the rule (its match, folder, `rate`, note or recipients) needs a new approval, so an agent cannot widen an approved rule through an edit proposal it approves itself. Approving restarts the rule's clock (`approve_from_now`), so it acts on mail that arrives from then on.

- **Who approves.** The GUI and the CLI: `postvak rules approve <name>` (the same command as for proposals; on a forward rule it also approves its addresses), and the Rules view's Approve button, which shows the addresses before it acts. Saving a forward rule in the GUI's rule editor approves it, since the user just wrote the addresses there. A rule written in rules.toml by hand, or over MCP, waits.
- **MCP cannot approve a forward.** The MCP `approve` tool (scope `rules:write`) still enables a rule, but leaves a forward unapproved and says so in its result; `rules:write` can write a forward rule, which then waits like a proposal. The MCP docs say what `rules:write` risks and that forwards are held.
- **While waiting.** The whole rule waits: none of its actions run, so it never archives what it did not forward. The same holds when the account has no SMTP server (part 1); `rules check` and the warnings say why, and `postvak rules` lists the rule as `waiting for approval` or `waiting: sending not set up`.
- **Told.** A rule that starts waiting raises the desktop notification that proposals use (`A rule waits for your approval`), and shows in the Rules view with its addresses.
- **Accounts.** A rule for every account is approved in each account's store at once. An account added later starts unapproved: the rule waits there, and the notification says so.

## Surfaces

- **rules.toml / CLI**: the action, `rules approve`, `rules`/`rules check` showing waiting forward rules, `rules test` showing `forward to books@example.org (+1)`.
- **GUI**: the rule editor gains Forward with every option above (recipients, From from the account's addresses, prefix, note, inline or attachment, attachment filter, size handling, rate); the Rules view shows waiting forward rules with their addresses and Approve; previews show the forward as new-mail-only.
- **MCP**: `rules:propose` and `rules:write` take the action through the schema; nothing sends without the human approval above. Its rule rows, and `--json` rule rows, carry `forward_waiting`: the accounts where the rule waits for approval, empty when approved everywhere.

## Docs

`docs/src/rules.md` (the action, approval, the limits), `docs/src/mcp.md` (what `rules:write` risks, and that forwards stay held), `docs/src/sending.md` (rule forwards in the outbox), schema and CLI pages regenerated.

## Tests

- Parse and validate each option, refusals included (`attachments` with `as = "attachment"`, a `from` that is not ours, no `to`).
- Plan order: forward queued before a delete in the same rule; the outbox holds the message after the original is gone.
- Mode: no forward under `ApplyExisting`, other actions still run.
- Once, loops, rate (the 21st in an hour not sent, approval withdrawn, notice raised), `after`.
- Approval: unapproved rule runs nothing; any definition change needs a new approval, including an edit proposal approved over MCP; MCP approve leaves a forward unapproved; GUI save approves; an account added later waits.
- Cannot queue: a failed fetch leaves the message untouched and it is forwarded on the next pass; `max_size = "skip"` still runs the other actions.
- Loops: a header with our address skips; another account's address forwards and adds ours; three headers skip.
- Attachment filter and size handling through the forward builder, including the "Not forwarded" lines.
- A live test: a forward rule on Dovecot, through submission, arriving at the sink with `X-Postvak-Forwarded`.

## My calls

Forward first in the plan, the once-only key and when it is written, the loop header's name, the log row's wording, and the GUI save that approves. Rate is counted per account.
