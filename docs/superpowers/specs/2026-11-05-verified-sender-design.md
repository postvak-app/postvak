# A seal for verified senders

The reader shows a green seal after the sender's address when the account's provider vouches that the mail comes from the domain in its From address.

## What earns it

- The same check as the `authenticated` rule condition: the topmost `Authentication-Results` header written by a server in the account's `trusted_authserv_ids` says DMARC passed for the From domain, or, without a DMARC verdict for it, an aligned DKIM signature passed. `message::auth::vouched` gives that domain and the server, and `authenticated` is now `vouched(..).is_some()`, so the two cannot drift.
- An account without a trusted authserv-id shows no seal on any mail. Trusting a server is a choice the user makes (`docs/superpowers/specs/2026-10-29-authserv-id-offer-design.md`); the seal does not guess one.

## What it says

- The seal is Phosphor's `seal-check` in `Theme.pane.success`, after the address, so it sits next to the domain it vouches for.
- Its tooltip, also its accessible label, names the server and the domain: "Verified by mx.example.org: this comes from example.org. Check that is the sender you expect." Authentication proves the domain, not who owns it: a look-alike such as `paypal-secure.com` passes as easily as `paypal.com`. A bare check would read as "safe"; naming the domain keeps the claim to what was checked.

## Scope

- The reader only. The message list does not show it: an icon on every row is noise next to the logos and flags there.
- GUI only, an exception to shipping on every surface: the CLI and MCP already filter on the same verdict with the `authenticated` rule condition, and the seal is a visual cue for reading mail in the window.
- Not here: a warning when DMARC fails for the From domain, and making sender logos (`bimi::brand`) use the trusted servers instead of the topmost header.
