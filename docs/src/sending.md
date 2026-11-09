# Sending mail

Postvak sends new mail, replies and forwards from the command line with `postvak send`, and from the window (see
[Writing mail](gui.md#writing-mail)). Every message goes through the daemon's outbox: it waits there for a few seconds,
so you can still take it back, then goes out over SMTP, and a copy is saved in your Sent folder. Agents over MCP get no
tool to send.

Drafts live in the account's Drafts folder on the server, not in Postvak: the window saves them there and opens them from there (see [Writing mail](gui.md#writing-mail)).

## The SMTP server

Most accounts need no setup. When you add an account, and once for every account without SMTP settings when the daemon
starts, Postvak tries the likely servers and keeps the first that logs in:

1. `smtp.<domain>`, where `<domain>` is your IMAP host without a leading `imap.` or `mail.`;
2. the IMAP host itself;

each on port 465 with TLS, then on port 587 with STARTTLS, logging in with the account's IMAP username and password. A
guess never logs in without TLS, and never tries a host outside your IMAP host's own domain. It sends nothing.

For a new account, `postvak account add` saves what it found to `config.toml`. For an account you already had, the daemon
keeps the guess to itself and never writes `config.toml`; save it there with:

```sh
postvak account smtp work --save
```

When no server answered, `postvak daemon status` says "Sending not set up". Give the server yourself, which is checked by
logging in before it is saved:

```sh
postvak account smtp work --smtp-host smtp.example.com --smtp-tls starttls
postvak account smtp work --guess      # try the guess again
```

In the window, File ▸ Preferences… › Sending shows, per account, the server it sends through and whether it comes from
`config.toml` or the guess, or "Not set up: no server answered". Save guess writes a guessed server to `config.toml`, as
`--save` does, and Guess again tries the guess again, as `--guess` does; both log in first, and save nothing when that
fails, saying why. A server of your own is given on the command line or in `config.toml`.

`postvak account add` takes the same `--smtp-host`, `--smtp-port` and `--smtp-tls` to skip the guess. In `config.toml`
the settings are a table per account:

```toml
[accounts.smtp]
host = "smtp.example.com"
port = 465                     # default by tls: 465 implicit, 587 starttls
tls = "implicit"               # implicit | starttls; there is no plain-text choice
username = "me"                # default: the account's IMAP username
password = { keyring = true }  # default: the account's IMAP password
save_sent = true               # default: off for Gmail and Outlook, on elsewhere
```

`password = { keyring = true }` here reads a keyring entry of its own, `<account>/smtp`; leave it out to use the IMAP
password. `password = { command = "…" }` works as for IMAP.

## Your name on From

```toml
[[accounts]]
name = "work"
display_name = "Pieter Willekens"
aliases = ["*@shop.example.com", { address = "info@example.com", name = "Example Shop" }]
```

Mail from the account goes out as `Pieter Willekens <pieter@example.com>`. An alias written as a table may carry a name
of its own; a wildcard alias, or one without a name, uses `display_name`. Without one, From is the bare address.

## Signatures

```toml
[[sending.signatures]]
address = "pieter@example.com"
text = "Pieter Willekens\nExample B.V."

[[sending.signatures]]
address = "*@shop.example.com"
text = "The Example Shop"
on_replies = false
```

`postvak send` puts the signature for the From address under your text, after a line `-- `, and above the quote of a
reply or forward. `address` is an address or a pattern with `*`, as an alias; an exact address wins over a pattern.
`on_replies = false` keeps a signature off replies and forwards; it defaults to `true`. In the editor the signature is
already there, so you can change it or delete it for this one message. `--no-signature` leaves it off. `--raw` sends
the message as given, without one, and so do rule forwards; MCP has no tool that sends.

In the window, File ▸ Preferences… › Sending has a signature per address: each account's address, then its aliases.
Write it, choose whether replies and forwards carry it, and press Save; an empty one removes it. A new message window
signs with it from then on, below the text you write, and you can change or delete it there for that message.

## postvak send

```sh
# A new message; the text comes from stdin
echo "See you at ten." | postvak send --to linus@example.org --subject "Tomorrow"

# Written in your editor ($VISUAL, else $EDITOR, else vi): the default when stdin is a terminal
postvak send --to linus@example.org --subject "Tomorrow" --edit

# With attachments
postvak send --to linus@example.org --subject "Report" --attach report.pdf --body-file note.txt

# Reply to the sender, to everyone, or to the mailing list of INBOX message 42
postvak send --reply INBOX/42
postvak send --reply-all INBOX/42
postvak send --reply-list INBOX/42

# Forward, with the original's text and attachments, or the whole message attached
postvak send --forward INBOX/42 --to boss@example.org
postvak send --forward INBOX/42 --as attachment --to boss@example.org

# From an alias
postvak send --from info@example.com --to linus@example.org --subject "Order" < order.txt
```

A reply goes to Reply-To, else From; a reply to a message of your own (in Sent) goes where that message went. Reply to
all keeps the original To and Cc and leaves out every address of yours. The text you write goes above the quote, as in
Gmail, Apple Mail and Outlook; in the editor you may also answer between the quoted lines. A reply or forward of an HTML
message is sent with the original as HTML too, inside a quote, with its images; the text you write is plain text.

Postvak checks every message before it queues it: From must be your address or an alias's, every recipient a real
address, and the message within the server's size limit. A refused message is not queued.

Once the message has gone, the original gets the `\Answered` flag for a reply or `$Forwarded` for a forward.

## The outbox and the undo delay

```text
$ postvak send --reply INBOX/42 < answer.txt
Sending to Linus Example in 10 s (postvak outbox cancel 3f2a9c1b)
```

The message waits in the outbox for the undo delay, then goes. Until then:

```sh
postvak outbox                    # what waits, and what failed
postvak outbox cancel 3f2a9c1b    # take it back; prints the message
postvak outbox retry 3f2a9c1b     # send a waiting or failed one now
```

In the window, the toast after Send counts down with Undo (`z`) and Send now.

Set the delay in `config.toml`, or with Undo send in File ▸ Preferences… › Sending, to 0, 5, 10 (the default), 20 or 30
seconds, or skip it for one message with `--now`:

```toml
[sending]
undo_secs = 10
```

The outbox lives on disk, so a message the daemon accepted survives a crash or a restart, and the daemon keeps running
while a message waits. A message the daemon was handing to the server when it stopped is never sent twice: it is marked
failed with "may have been sent; check Sent before retrying". A server that says to try later is tried again for an
hour; a refusal, such as an unknown recipient, fails the message at once, and no recipient gets it.

### Rule forwards

A [forward rule](rules.md#forwarding) puts its forward in the same outbox, due at once: no person is there to take it back during an undo delay. A forward the server refuses stays in `postvak outbox` with the reason, so nothing is lost when the rule's other actions already moved or deleted the original.

## Sent copies

After the server took the message, the daemon saves a copy, read, in the folder marked as Sent, else in one called
`Sent`. The copy keeps Bcc, so you can see who you sent it to; the recipients never see it. Gmail and Outlook file a copy
themselves, so Postvak saves none there unless `save_sent = true`. When there is no Sent folder or the copy failed,
`postvak daemon status` says so.

## mutt, aerc and git send-email

`postvak send --raw` reads a whole message from stdin and sends it as it is, except that Bcc is removed, like
`sendmail`. It skips the undo delay, waits for the server's answer and exits as sendmail does: 0 when sent, 75 when the
server said to try later (the message stays in the outbox and is retried), 69 when it was refused. It saves no copy in
Sent, since these programs file their own, unless you pass `--save-sent`. The account is the one whose address or alias
is the message's From, or `--account`.

```sh
# mutt / neomutt
set sendmail = "postvak send --raw"

# aerc, in accounts.conf
outgoing = postvak send --raw

# git send-email
git config sendemail.sendmailCmd "postvak send --raw"
```

Of sendmail's options, `-i`, `-oi`, `-t` and `-f <address>` are accepted (`-f` must equal From); any other is refused.
Recipients are the addresses given, and with `-t` or none given, the message's To, Cc and Bcc.

## Agents

There is no MCP tool to send, and there will not be one. An agent can propose a [forward rule](rules.md#forwarding), but it forwards nothing until you approve it in Postvak. An agent that you give a shell can still run `postvak send`
like any other command; give it one only if you mean it to send mail as you.
