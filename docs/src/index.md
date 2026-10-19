# Postvak

Rules for your IMAP inbox. Test them with a [dry run](https://postvak.pataar.nl/cli.html#postvak-rules-test) and [undo any delete](https://postvak.pataar.nl/cli.html#postvak-trash-restore). Postvak syncs your mail into a local store and runs your rules: move mail into folders, mark it read, or delete transient mail such as sign-in codes and magic links once you no longer need it. Without rules, it leaves your mail alone.

It runs headless as a [background service](https://postvak.pataar.nl/daemon.html), at login, [on a server](https://postvak.pataar.nl/daemon.html#on-a-server) or [in Docker](https://postvak.pataar.nl/daemon.html#docker). On top come a keyboard-driven [mail window](https://postvak.pataar.nl/gui.html) for reading (`postvak gui`), a full [command line](https://postvak.pataar.nl/cli.html), and an MCP server for agents (see [Agents over MCP](https://postvak.pataar.nl/mcp.html)). Postvak reads and sorts mail; it does not send it. It runs on macOS and Linux; see [Install](https://postvak.pataar.nl/install.html).

![The Postvak mail window](https://raw.githubusercontent.com/postvak-app/postvak/main/tests/snapshots/gui_inbox_light.png#gh-light-mode-only)
![The Postvak mail window](https://raw.githubusercontent.com/postvak-app/postvak/main/tests/snapshots/gui_inbox_dark.png#gh-dark-mode-only)

## Quickstart

Install Postvak, add an account and write your first rule: see the [Quickstart](https://postvak.pataar.nl/quickstart.html).
## Why rules, not an AI that sorts your mail?

Many AI mail clients hand every message to a model and let it decide what happens. Postvak sorts with rules you can read, and lets AI help you write them.

- **Predictable.** The same mail gets the same outcome, every time. A model can file a message differently tomorrow, or delete one it misread.
- **Reviewable.** A rule is a few lines of TOML. [`rules test`](https://postvak.pataar.nl/cli.html#postvak-rules-test) shows what it would do before it does it, and [`postvak log`](https://postvak.pataar.nl/cli.html#postvak-log) says which rule did what.
- **Safe from the sender.** Mail is written by strangers. A rule matches patterns, so instructions hidden in a message cannot change what it does.
- **Private.** Your mail is never sent to a model to be sorted. Rules run on your own machine or server, and agents see only [what you grant](https://postvak.pataar.nl/mcp.html#scopes): by default headers, no bodies.
- **Free and fast.** No tokens per message and no API key. Rules run on every sync, without a round trip to a model, across years of mail.
- **AI where it helps.** An agent over MCP reads your inbox, [proposes a rule](https://postvak.pataar.nl/rules.html#proposals) and shows its dry run. You approve it once, and from then on the rule does the work without the model.

## Why not server-side filters?

- Works with any IMAP server, without Sieve or a webmail settings page.
- Rules go further than most filters: regex on any header or the body, age, size, read and flagged state, aliases and tags, combined with `any` and `none`; actions move, archive, tag, flag, mark read, delete or silence notifications. See [Rules](https://postvak.pataar.nl/rules.html).
- Rules [run on every sync](https://postvak.pataar.nl/rules.html#when-rules-act), not once on delivery, so they can act later: an hour after you read a sign-in code, or once a newsletter is a week old.
- `rules test` shows what every rule would do before it does it, and deleted mail [can be restored](https://postvak.pataar.nl/cli.html#postvak-trash-restore).
- [Passwords](https://postvak.pataar.nl/accounts.html#passwords) stay in the macOS Keychain or the Secret Service, or come from a command such as `pass`.

Postvak is licensed under MIT or Apache-2.0, at your option. The mail window's HTML view includes Stylo, the CSS engine from Servo and Firefox, which is under MPL-2.0. The mail window is made with [Slint](https://slint.dev/) under its royalty-free licence; Help ▸ About Postvak shows its attribution.
