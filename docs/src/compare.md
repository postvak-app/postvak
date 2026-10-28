# Compared to other tools

Postvak sorts mail with rules you write, on your own machine or server, over IMAP. Plenty of tools sort mail; this page says where Postvak differs and when another one fits you better.

| | Postvak | imapfilter | Sieve | Thunderbird, Apple Mail | AI sorters |
|---|---|---|---|---|---|
| Runs | in the background, on your machine or server | where you run it | on the mail server | while the app is open | on the vendor's servers |
| Works with | any IMAP server with password login | any IMAP server | servers that offer it | the app's accounts | the services they support |
| Rules are | TOML | Lua | Sieve scripts | dialogs | a model's judgement |
| Acts on mail already in a folder | yes, on every sync | yes | no, only on delivery | when you run a filter | varies |
| Shows what a rule would do first | [`rules test`](cli.md#postvak-rules-test) | dry-run mode | no | no | no |
| Undo a delete | [trash](cli.md#postvak-trash-restore) | no | no | the app's trash | varies |
| Mail leaves your machine | no | no | no | no | yes |

## Scriptable filters: imapfilter, Sieve, procmail

[imapfilter](https://github.com/lefcha/imapfilter) is the closest tool: it logs in to any IMAP server and moves, flags or deletes what its searches match, with a dry-run mode. Its configuration is a Lua program, which can do anything but is a program to write and debug. Postvak's rules are a fixed set of conditions and actions in TOML, checked against a [schema](rules.md) before they run, and come with a mail window, search, notifications, a [log](cli.md#postvak-log) of what each rule did, and `.eml` backups of anything a rule deletes.

[Sieve](https://datatracker.ietf.org/doc/rfc5228) runs on the mail server at delivery, so it works while every device is off. It only sees a message once, as it arrives: it cannot delete a sign-in code an hour after you read it, or archive a newsletter once it is a week old. Postvak's rules [run on every sync](rules.md#when-rules-act), so they can.

procmail and similar local delivery agents filter mail as it is delivered to a mailbox on your own machine, which these days usually means fetching it first. Postvak leaves mail on the IMAP server and sorts it there, so every client you use sees the result.

**Pick another tool when** your server offers Sieve and a delivery-time rule is all you need, or when you want a filter you can program rather than configure.

## Mail client filters: Thunderbird, Apple Mail

Thunderbird's and Apple Mail's filters run inside the app, so they act only while it is open; mail that arrives while your laptop is closed waits in the inbox until you open it again. Postvak's [background service](daemon.md) runs at login or on a server, without the window, and keeps sorting while you are away.

A rule in a client lives in that client's settings on that one machine. Postvak's rules are a text file you can read, diff and keep in version control, [shared between devices](rules.md#sharing-rules-between-devices) through a file sync.

**Pick another tool when** you already live in one of these clients and only sort mail while you use it.

## AI sorters

Services such as SaneBox, Shortwave and Superhuman sort mail with a model trained on your behaviour or a prompt you write. They take no rules to write, and they can catch mail no pattern would.

Postvak sorts with rules instead, for these reasons:

- **Predictable.** The same mail gets the same outcome, every time. A model can file a message differently tomorrow, or delete one it misread.
- **Reviewable.** A rule is a few lines of TOML. [`rules test`](cli.md#postvak-rules-test) shows what it would do before it does it, and [`postvak log`](cli.md#postvak-log) says which rule did what.
- **Safe from the sender.** Mail is written by strangers. A rule matches patterns, so instructions hidden in a message cannot change what it does.
- **Private.** Your mail is never sent to a model to be sorted. Rules run on your own machine or server, and agents see only [what you grant](mcp.md#scopes): by default headers, no bodies.
- **Free and fast.** No subscription, no tokens per message and no API key. Rules run on every sync, without a round trip to a model, across years of mail.
- **AI where it helps.** An agent over MCP reads your inbox, [proposes a rule](rules.md#proposals) and shows its dry run. You approve it once, and from then on the rule does the work without the model.

**Pick another tool when** you would rather not write or approve rules at all, and are fine with a service reading your mail to sort it.

## Where Postvak falls short

- It logs in with a password only. Gmail needs an [app password](https://support.google.com/accounts/answer/185833); Outlook.com and Microsoft 365, which accept only OAuth for IMAP, do not work.
- IMAP over TLS on port 993 only; STARTTLS on port 143 is not supported yet.
- It reads and sorts mail; it does not send it.
- It runs on macOS and Linux, not Windows.
