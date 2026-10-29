# Compared to other tools

Postvak sorts your mail with rules you write, on any IMAP server, without the app open. No other tool here does all of this at once:

- **It sorts mail already in your folders**, not only as it arrives: it deletes a sign-in code an hour after you read it, or archives a newsletter once it is a week old.
- **You see what a rule would do before it does it**, and anything a rule deletes is kept as an `.eml` backup you can restore.
- **It keeps working without you**: at login, on a server or in Docker, while the mail window is closed.
- **It comes with a mail window, full-text search and an MCP server**, so you can read, search and have an agent propose rules in one place.
- **Your mail never leaves your machine** to be sorted.

<div class="compare">

| | Postvak | imapfilter | Sieve | Thunderbird, Apple Mail | AI sorters |
|---|---|---|---|---|---|
| Sorts without the app open | <span class="yes">✓</span> | <span class="yes">✓</span> | <span class="yes">✓</span> | <span class="no">✗</span> | <span class="yes">✓</span> |
| Sorts mail already in a folder | <span class="yes">✓</span> on every sync | <span class="yes">✓</span> | <span class="no">✗</span> on delivery only | when you run a filter | varies |
| Works with any IMAP server | <span class="yes">✓</span> with a password | <span class="yes">✓</span> | <span class="no">✗</span> if it offers Sieve | <span class="yes">✓</span> | <span class="no">✗</span> services they support |
| Rules without programming | <span class="yes">✓</span> TOML or a form | <span class="no">✗</span> Lua | <span class="no">✗</span> Sieve scripts | <span class="yes">✓</span> dialogs | no rules to write |
| Shows what a rule would do first | <span class="yes">✓</span> [dry run](cli.md#postvak-rules-test) | <span class="yes">✓</span> dry run | <span class="no">✗</span> | <span class="no">✗</span> | <span class="no">✗</span> |
| Undo a delete | <span class="yes">✓</span> [`.eml` backups](cli.md#postvak-trash-restore) | <span class="no">✗</span> | <span class="no">✗</span> | the app's trash | varies |
| Rules in a text file you can keep in git | <span class="yes">✓</span> | <span class="yes">✓</span> | on the server | <span class="no">✗</span> | <span class="no">✗</span> |
| Mail window and full-text search | <span class="yes">✓</span> | <span class="no">✗</span> | <span class="no">✗</span> | <span class="yes">✓</span> | varies |
| Your own AI agent can propose rules | <span class="yes">✓</span> [over MCP](mcp.md) | <span class="no">✗</span> | <span class="no">✗</span> | <span class="no">✗</span> | <span class="no">✗</span> |
| Mail stays on your machine | <span class="yes">✓</span> | <span class="yes">✓</span> | <span class="yes">✓</span> | <span class="yes">✓</span> | <span class="no">✗</span> |
| Price | free, open source | free, open source | part of your server | free | mostly a subscription |

</div>

Each tool below has cases where it fits better; [where Postvak falls short](#where-postvak-falls-short) lists its own gaps.

## Scriptable filters: imapfilter, Sieve, procmail

[imapfilter](https://github.com/lefcha/imapfilter) is the closest tool: it logs in to any IMAP server and moves, flags or deletes what its searches match, with a dry-run mode. Its configuration is a Lua program, which can do anything but is a program to write and debug. Postvak's rules are a fixed set of conditions and actions in TOML, checked against a [schema](rules.md) before they run, and come with a mail window, search, notifications, a [log](cli.md#postvak-log) of what each rule did, and `.eml` backups of anything a rule deletes.

[Sieve](https://datatracker.ietf.org/doc/rfc5228) runs on the mail server at delivery, so it works while every device is off. It only sees a message once, as it arrives: it cannot delete a sign-in code an hour after you read it, or archive a newsletter once it is a week old. Postvak's rules [run on every sync](rules.md#when-rules-act), so they can.

procmail and similar local delivery agents filter mail as it is delivered to a mailbox on your own machine, which these days usually means fetching it first. Postvak leaves mail on the IMAP server and sorts it there, so every client you use sees the result.

**Pick another tool when** your server offers Sieve and a delivery-time rule is all you need, or when you want a filter you can program rather than configure.

## Mail client filters: Thunderbird, Apple Mail

Thunderbird's and Apple Mail's filters run inside the app, so they act only while it is open; mail that arrives while your laptop is closed waits in the inbox until you open it again. Postvak's [background service](daemon.md) runs at login or on a server, without the window, and keeps sorting while you are away.

A rule in a client lives in that client's settings on that one machine. Postvak's rules are a text file you can read, diff and keep in version control, [shared between devices](rules.md#sharing-rules-between-devices) through a file sync.

**Pick another tool when** you already live in one of these clients and only sort mail while you use it.

## Search

Postvak keeps a local full-text index of every account, bodies and attachment names included, as Thunderbird does, so search answers as you type and works offline. It differs in a few ways:

- **One query everywhere.** The same query means the same thing in the window, on the [command line](cli.md#postvak-search) and over MCP, so an agent searches the way you do.
- **Operators.** `from:`, `to:`, `subject:`, `in:`, `is:unread`, `has:attachment`, dates such as `after:7d`, and `from:me` that knows your aliases. See [Search](search.md).
- **Forgiving.** Nothing you type is an error: an unbalanced quote or an unknown key searches as words. When few messages match, [close matches](search.md#close-matches) follow, so `invioce` still finds the invoice.
- **Fast.** About 35 ms to search 100,000 messages on an M1 Pro; see [Performance](perf.md).

**Pick another tool when** you need to search a large archive the moment you add it, or would rather not keep its text on disk. Postvak [indexes in the background](search.md#the-index), newest first, and until it is done asks the server for the rest; `index_bodies = false` keeps bodies off disk, at the cost of searching them only on the server.

## AI sorters

Services such as SaneBox, Shortwave and Superhuman sort mail with a model trained on your behaviour or a prompt you write. They take no rules to write, and they can catch mail no pattern would.

Postvak sorts with rules instead: predictable, reviewable, safe from instructions hidden in a message, and private. See [why rules, not an AI](index.md#why-rules-not-an-ai-that-sorts-your-mail) for the full case, and how an agent can still [propose a rule](rules.md#proposals) for you to approve.

**Pick another tool when** you would rather not write or approve rules at all, and are fine with a service reading your mail to sort it.

## Where Postvak falls short

- It logs in with a password only. Gmail needs an [app password](https://support.google.com/accounts/answer/185833); Outlook.com and Microsoft 365, which accept only OAuth for IMAP, do not work yet ([#370](https://github.com/postvak-app/postvak/issues/370)).
- It reads and sorts mail; it does not send it yet ([#372](https://github.com/postvak-app/postvak/issues/372)).
- It runs on macOS and Linux. On Windows, the command line and background service may work under WSL2, but are untested ([#371](https://github.com/postvak-app/postvak/issues/371)).
