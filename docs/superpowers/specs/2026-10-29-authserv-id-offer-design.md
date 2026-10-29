# Offering the authserv-id to trust

Issue #400. `authenticated` needs the account's `trusted_authserv_ids`, which had to be found by hand. Postvak now offers one, read from the mail, and writes it only on the user's yes.

## What is offered

- `message::auth::common_authserv_id` looks at the topmost `Authentication-Results` header of each message. It offers that header's authserv-id when every message has one with the same id and there are at least `SUGGEST_MIN` (10) messages. One message without the header, or topped by another id, offers nothing. Neither does an id with anything but letters, digits, `.` and `-`, since the offer puts it in a command the user may paste into a shell.
- `rules::AccountFolders::read` runs it over the newest 20 messages of the account's INBOX in its store, as `authserv_id`. No IMAP request is involved.
- Trusting a server is safe only when it writes the header on every message it receives, since otherwise a sender's own header can sit on top (`docs/src/rules.md`). The same id topping the newest 20 messages from different senders is the evidence for that. A table of known providers is not used: its answer would still have to be confirmed by the mail, so it adds nothing but upkeep.

## Where it is offered

The offer appears only where it matters: an account that trusts no authserv-id, while a rule that uses `authenticated` runs on it.

- `rules::authenticated_warnings`, moved out of `rules::warnings` because it reads the stores, adds `: the newest mail in its INBOX all has one from 'ID', which `postvak account trust ACCOUNT ID` lists` to the existing warning. The CLI's `rules check`, MCP `rules_check` (visible accounts only) and the Rules view show it. The daemon logs it too, and works it out again after each sync.
- `rules::authserv_offers` gives the (account, id) pairs. The Rules view shows each one as a "Trust ID for ACCOUNT" button under its banner.
- MCP can only report the offer: agents do not change account config.

## The yes

- CLI: `postvak account trust ACCOUNT ID`. Running it is the yes. It takes any ID, validated like one written by hand.
- GUI: the button.
- Both call `account::trust_authserv_id`. It reads the account from config.toml as it is now, adds the id (lower-cased, without a trailing dot, once) and saves through `config::save_account`. The daemon notices the change and restarts that account. The window takes the account's new identity at once and absorbs its own write.
