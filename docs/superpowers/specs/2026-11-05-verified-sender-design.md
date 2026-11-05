# A seal for verified senders

The reader shows a green seal after the sender's address when the account's provider vouches that the mail comes from the domain in its From address, and an amber warning seal when the provider found DMARC failed for that domain.

## What earns it

- The same check as the `authenticated` rule condition: the topmost `Authentication-Results` header written by a server in the account's `trusted_authserv_ids` says DMARC passed for the From domain, or, without a DMARC verdict for it, an aligned DKIM signature passed. `message::auth::verdict` gives that domain, the server and whether it passed, and `authenticated` is now built on it, so the two cannot drift.
- An account without a trusted authserv-id shows no seal on any mail. Trusting a server is a choice the user makes (`docs/superpowers/specs/2026-10-29-authserv-id-offer-design.md`); the seal does not guess one.

## What it says

- The seal is Phosphor's `seal-check` in `Theme.pane.success`, after the address, so it sits next to the domain it vouches for.
- Its tooltip, also its accessible label, names the server and the domain: "Verified by mx.example.org: this comes from example.org. Check that is the sender you expect." Authentication proves the domain, not who owns it: a look-alike such as `paypal-secure.com` passes as easily as `paypal.com`. A bare check would read as "safe"; naming the domain keeps the claim to what was checked.

## The warning

- Only an explicit `dmarc=fail` for the From domain in that same trusted header. DKIM or SPF failing alone, `dmarc=none` or no verdict give nothing: they are common on honest mail and say less.
- Phosphor's `seal-warning` in `Theme.pane.warning`, the colour of a caution rather than an error, since a mailing list that passes mail on without rewriting From fails DMARC too. Its tooltip: "Not verified: mx.example.org found this does not come from example.org, though it says so. It may be forged, or passed on by a mailing list."

## Sender logos

- `bimi::brand` read the topmost `Authentication-Results` whoever wrote it. On a provider that writes none, the topmost one is the sender's, so spoofed mail could show a brand's logo. It now reads the topmost header from a trusted server, as the seal does, through `auth::topmost_trusted` and `AuthResults::dmarc`.
- The cost: an account without a trusted authserv-id shows no logos. Preferences says so under Show sender logos ("BIMI logos, for senders a trusted server verified.").

## Scope

- The reader only. The message list does not show it: an icon on every row is noise next to the logos and flags there.
- GUI only, an exception to shipping on every surface: the CLI and MCP already filter on the same verdict with the `authenticated` rule condition, and the seal is a visual cue for reading mail in the window.
