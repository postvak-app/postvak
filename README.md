<p align="center"><img src="assets/logo.svg" alt="Postvak" width="382"></p>

<p align="center">
  <a href="https://github.com/postvak-app/postvak/actions/workflows/ci.yml"><img src="https://github.com/postvak-app/postvak/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://postvak.pataar.nl/"><img src="https://img.shields.io/badge/docs-postvak.pataar.nl-orange" alt="Docs"></a>
  <a href="LICENSE-MIT"><img src="https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue" alt="License: MIT OR Apache-2.0"></a>
</p>

# Postvak

Rules for your IMAP inbox. Test them with a [dry run](https://postvak.pataar.nl/cli.html#postvak-rules-test) and [undo any delete](https://postvak.pataar.nl/cli.html#postvak-trash-restore). Postvak syncs your mail into a local store and runs your rules: move mail into folders, mark it read, or delete transient mail such as sign-in codes and magic links once you no longer need it. Without rules, it leaves your mail alone.

It runs headless as a [background service](https://postvak.pataar.nl/daemon.html), at login, [on a server](https://postvak.pataar.nl/daemon.html#on-a-server) or [in Docker](https://postvak.pataar.nl/daemon.html#docker). On top come a keyboard-driven [mail window](https://postvak.pataar.nl/gui.html) for reading (`postvak gui`), a full [command line](https://postvak.pataar.nl/cli.html), and an MCP server for agents (see [Agents over MCP](https://postvak.pataar.nl/mcp.html)). Postvak reads and sorts mail; it does not send it. It runs on macOS and Linux; see [Install](https://postvak.pataar.nl/install.html).

![The Postvak mail window](https://raw.githubusercontent.com/postvak-app/postvak/main/tests/snapshots/gui_inbox_light.png#gh-light-mode-only)
![The Postvak mail window](https://raw.githubusercontent.com/postvak-app/postvak/main/tests/snapshots/gui_inbox_dark.png#gh-dark-mode-only)

## Quickstart

Install Postvak, add an account and write your first rule: see the [Quickstart](https://postvak.pataar.nl/quickstart.html).
## Why Postvak?

Rules you write, not a model that guesses: the same mail gets the same outcome every time, a hidden instruction in a message cannot change what happens, and your mail is never sent anywhere to be sorted. Unlike server-side filters, rules work on any IMAP server and run on every sync, so they can act later: an hour after you read a sign-in code, or once a newsletter is a week old. An agent can [propose a rule](https://postvak.pataar.nl/rules.html#proposals); you approve it once. See how Postvak [compares to imapfilter, Sieve, mail client filters and AI sorters](https://postvak.pataar.nl/compare.html).

Postvak is licensed under MIT or Apache-2.0, at your option. The mail window's HTML view includes Stylo, the CSS engine from Servo and Firefox, which is under MPL-2.0. The mail window is made with [Slint](https://slint.dev/) under its royalty-free licence; Help ▸ About Postvak shows its attribution.

## More

- [Documentation](https://postvak.pataar.nl/): install, accounts, rules, agent guide and CLI reference
- [Contributing](CONTRIBUTING.md) and [Code of Conduct](CODE_OF_CONDUCT.md)
- [Security policy](SECURITY.md): how to report a vulnerability
- [Changelog](CHANGELOG.md)
