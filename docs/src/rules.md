# Rules

Rules live in `rules.toml` next to `config.toml`. Postvak reads the file on every sync, and the daemon syncs every account within seconds of the file changing, whoever changed it. A file that fails to validate is rejected as a whole, and the previous rules stay active until the daemon stops or that account's settings change. When there are none, no rules run and new mail notifies as the account's `notify` setting says; once the file is fixed, rules that were already enabled also run on the mail that arrived meanwhile. Each account reports the error once, and syncing and actions carry on. `postvak rules check` validates the file; `postvak rules test` shows what each rule would do to the mail Postvak has cached.

```toml
[[rules]]
name = "github to folder"
match.header.List-Id = { contains = "github.com" }
actions.move = "Lists/GitHub"
actions.read = true
```

## Rule keys

| Key | Default | Meaning |
|---|---|---|
| `name` | required | Unique. Renaming a rule makes it a new rule. |
| `account` | every account | Only for this account. |
| `folder` | `INBOX` | The folder the rule watches. `INBOX` matches in any case; other names match exactly. |
| `enabled` | `true` | `false` skips the rule. Proposals start disabled. |
| `proposed_by` | none | Set by `postvak rules propose`, and removed when the rule is approved. |
| `match` | required | Conditions that must all hold. At least one. |
| `actions` | required | What to do. At least one. |

`match` and `actions` are both tables, so each key appears once, and the order you write them in does not matter. Write them as dotted keys, one per line, as the examples do, or as one inline table: `actions = { move = "Lists", read = true }`.

A tag is printable ASCII without spaces, backslashes or `( ) { % * " ]`, and `$PostvakRestored` is reserved. `name`, `account`, `folder` and `move` folders must not be blank, may not contain control characters, and are at most 255 characters. On a server that ignores case in folder names, such as Gmail or Exchange, a `move` to `lists` while `Lists` exists goes to `Lists`; a server that respects case, such as Dovecot, creates `lists` beside it.

Folder names are written in plain text, `Archief/Één` included; Postvak encodes them as IMAP requires (modified UTF-7) and shows the server's folders decoded. A level of a `folder` or `move` folder (the parts between `/`) may not start or end with a space. A `move` folder also may not contain `*` or `%`, which IMAP reads as wildcards, or an empty level: `/Lists`, `Lists//GitHub` and `Lists/` are refused. A name typed with separate accents, as macOS can produce, finds the folder the server spells with precomposed letters (Unicode NFC) and the other way round, and a new folder is created in NFC.

Like any invalid rule, one of these names stops the whole file from loading, so no rule runs until it is fixed; a file that loaded before Postvak checked them can stop after an upgrade, and `postvak rules check` names the rule. Postvak used to show non-ASCII folders in their encoded form, such as `&AMkA6Q-n` for `Één`. A rule that names a folder that way still finds the folder while the account has it, so it keeps watching and moving to `Één`; otherwise it means a folder with exactly that name. `rules check` warns about such a rule and gives the plain name to write instead.

## Conditions

Text conditions take exactly one of:

- `contains`: a substring.
- `equals`: the whole value. On `from`, `to` and `cc` it also matches any single address in the field, so `equals = "a@example.com"` matches `Alice <a@example.com>, b@example.com`.
- `regex`: Rust `regex` syntax. Start with `(?-i)` to make it case-sensitive.

All three ignore case.

`contains` and `equals` also take a list, which matches when any value does: `subject = { contains = ["receipt", "invoice"] }`.

| Key | Takes | Matches |
|---|---|---|
| `from`, `to`, `cc`, `subject` | text condition | That header. |
| `body` | text condition | The plain-text body; HTML mail is converted. Postvak downloads the body of new mail in the rule's folder for this. |
| `header` | a table of header names, each with a text condition | Any header, such as `match.header.List-Id = { contains = "github" }`, unfolded and with encoded words (`=?UTF-8?…?=`) decoded. Of a repeated header only the last copy, nearest the body, counts. Every header named must match. |
| `from_domain` | domain, or a list of them | The From address is in that domain or a subdomain of it: `paypal.com` matches `service@paypal.com` and `x@mail.paypal.com`, not `x@notpaypal.com`. Only the address counts, never the display name, and case does not matter. A list matches when any domain does. |
| `to_domain` | domain, or a list of them | The same, for any address in To; not Cc. |
| `authenticated` | `true` or `false` | Your provider vouches that the mail comes from its From domain. Needs the account's `trusted_authserv_ids` (see [Authenticated mail](#authenticated-mail)). |
| `older_than` | span: `30min`, `1h`, `2d`, `3w`, `6mo`, `1y` | Mail that arrived at least this long ago. Units may be written out (`2days`); a bare `m` is refused, since it could mean minutes or months. Months and years follow the calendar. |
| `younger_than` | span, as for `older_than` | Mail that arrived less than this long ago: the opposite of `older_than`. |
| `larger_than` | size: `500KB`, `5MB`, `1GB` | Mail larger than this. A bare number is bytes, and `1KB` is 1024 bytes. |
| `read` | `true` or `false` | Read or unread mail. |
| `flagged` | `true` or `false` | Flagged or unflagged mail. |
| `to_me` | `true` or `false` | Your address or an alias is among the recipients: To, Cc or Delivered-To. `false` catches list and bcc mail. |
| `alias` | address, `*` as wildcard | An address among the same recipients is that alias. `*` matches any run of characters; nothing else is special, so this is no regex. |
| `tag` | IMAP keyword, such as `$label1` | Mail carrying that keyword, ignoring case, including one a rule above it in the file just added. Thunderbird shows keywords as tags. |
| `any` | list of conditions | Holds when at least one entry holds. |
| `none` | list of conditions | Holds when no entry holds. |

Keys named after a header (`from`, `to`, `cc`, `from_domain`, `to_domain`) read that header alone; `to_me` and `alias` read every recipient.

Each entry of `any` and `none` is a set of conditions that must all hold, written like `match` itself, so `none = [{ from = …, subject = … }]` only excludes mail that matches both. To exclude either, give each its own entry. Entries can hold `any` and `none` again, up to eight levels deep. A rule whose conditions need the body never fires on a message whose body could not be downloaded, even through `none`.

## Authenticated mail

Anyone can write any From address. A receiving mail server checks the sender's DKIM signature, SPF and DMARC, and writes its verdict in an `Authentication-Results` header at the top of the message. `authenticated = true` holds when that verdict says DMARC passed for the From domain. When it gives no DMARC pass or fail for that domain, DKIM passing for a domain aligned with the From domain is enough (the same domain, or a parent or subdomain of it within the same organisation, so a `mail.paypal.com` signature counts for `paypal.com`, and a `sendgrid.net` one does not); a DMARC fail always wins, since the domain's own policy may demand a closer match. SPF alone is not enough, since it vouches for the envelope sender, not the From address. A From with no address or with several is never authenticated.

A sender can write an `Authentication-Results` header too, so Postvak only reads the ones written by servers you list in the account's `trusted_authserv_ids` (see [Accounts](accounts.md)), recognised by the server name at their start (the authserv-id), and of those only the topmost, the one the last server added. To find the name, run `postvak headers UID` on a message you received, which shows its headers topmost first, and take the name after `Authentication-Results:` in the topmost one, such as `mx.google.com` for Gmail. List only a server that writes the header on every message it receives: on mail it left without one, the topmost header with its name would be the sender's. Postvak does not guess the name from the account's IMAP host for that reason. It does read the mail: when the topmost header of every one of the newest 20 messages in an account's INBOX (and at least 10 of them) names the same server, the warning below offers that server, with the command that lists it, `postvak account trust ACCOUNT SERVER`; the window's Rules view offers it as a button. Nothing is listed until you run the command or press the button. Without a list, no mail is authenticated, so `authenticated = true` never holds and `authenticated = false` holds for all mail; `rules check` warns about a rule that uses `authenticated` for such an account, and `rules test`, the MCP `rules_test` tool and the window's preview warn the same way about the rule they preview, before it is stored. Mail without a header from a listed server is not authenticated, so a rule with `authenticated = false` also catches it.

Use it with `from_domain` on rules that move mail you would act on, such as security alerts or invoices, so that mail which only claims to come from that domain stays in your inbox:

```toml
[[rules]]
name = "paypal"
match.from_domain = ["paypal.com", "paypal.nl"]
match.authenticated = true
actions.move = "Finance"
```

## Actions

| Key | Effect |
|---|---|
| `delete = true` | Saves the message as `.eml` in the local trash, then removes it from the server. Later rules don't run for that message. |
| `archive = true` | Moves it to the server's Archive folder. Fails when the server has none. A rule cannot both archive and `move`. |
| `junk = true` | Reports it as junk, as the Junk button does: tags it `$Junk`, which trains the server's spam filter, and moves it to the server's Junk folder. Fails when the server has none. Only a delete wins over it; the rules' other actions don't run for that message. A rule cannot both junk and `move` or `archive`. |
| `move = "Folder/Sub"` | Moves it to that folder, creating the folder if needed and subscribing to it, so mail apps that show only subscribed folders list it. When the server refuses to create it and the move then fails, the error says both. A folder that only groups others and cannot hold mail, such as Gmail's `[Gmail]`, is refused. `/` separates levels on most servers; see the warnings below for servers that use `.`. |
| `read = true` | Marks it read; `false` marks it unread. |
| `flagged = true` | Flags it; `false` removes the flag. |
| `tag = "$label1"` | Adds that IMAP keyword, or each keyword of a list (`tag = ["$label1", "$work"]`), which Thunderbird and other clients show as tags. The server must accept custom keywords. |
| `notify = true` | Notifies even when the message was moved; `false` never notifies. |
| `stop = true` | Later rules don't run for that message. |

The order of the keys does not matter: flags and tags are set before a move. When several rules match a message, the first one in the file decides each thing only one rule can decide: a delete in any matching rule wins over everything else, then a `junk`, then the first `move` or `archive` runs, and the first `notify` says whether it notifies. After a rule with `stop`, later rules in the file don't run for that message; earlier ones already did.

A rule sees what the matching rules above it changed: `read`, `flagged` and `tag` test the message as those rules left it, in the same sync, not only once the change is back from the server. So one rule can tag mail and a later one route on that tag, as in `none = [{ tag = "Security" }]` after a rule that tags security alerts. A rule below never changes what a rule above it saw.

## When rules act

- On every sync, in file order. Because rules run again on each sync, `older_than`, `younger_than` and `read` can fire or stop firing later, for example an hour after you read a sign-in code.
- A rule acts only on mail that arrived after the rule was enabled, so adding a rule never touches your history. `postvak rules apply-existing NAME` is the explicit opt-in. Run it with `--dry-run` first. When the rule would delete mail from the server for good (`delete`), it prints how many messages per account and how long their `.eml` backups are kept, then asks you to type `yes`; without a terminal it refuses unless you pass `--yes`. Moves and flags run without asking.
- A rule approved with `postvak rules approve` acts on mail that arrives after the approval. A rule you add or enable by editing the file acts on mail that arrives after the next sync picks it up.
- Renaming a rule, editing its account, folder, match or actions, or disabling and enabling it again, restarts that clock.
- Rules move a message out of a folder once. When it comes back, because you moved it back or a rule on another machine did, no rule moves, archives or junks it again; the other actions still run. So two rules that move mail between two folders stop after one round. Postvak tells messages apart by Message-ID and arrival time, which servers keep when mail moves.
- Mail restored with `postvak trash restore` carries the `$PostvakRestored` keyword. Rules never act on it again. This needs a server that accepts custom keywords; without one, the same rule can delete restored mail again.

## Notifications

New INBOX mail notifies unless a rule moved, archived, junked or deleted it, or a matching rule says `notify = false`; marking, flagging and tagging still notify. `notify = true` forces a notification for moved mail. When several matching rules set `notify`, the first one wins. Only INBOX mail notifies, so `notify` on a rule for another folder does nothing. With `notify = false` on the account, only rules that say `notify = true` notify. Deleted mail, and mail found by the first sync of a folder, never notifies.

On Linux a notification shows the sender and the subject with Postvak's icon, and asks the desktop for its new-mail sound, which follows Do Not Disturb and your volume; your desktop's notification settings list Postvak, so you can turn the sound or the popups off there. When one sync finds more than three new messages for an account, they come as one notification ("12 new messages", naming the senders), which opens the newest. Plasma shows the account beside Postvak's name. Clicking a notification opens the message in the window, which starts if it is not open. On Wayland an open window cannot bring itself to the front, so Plasma highlights it on the task bar and GNOME says "Postvak is ready".

On macOS a notification shows the sender and the subject as Postvak's once Postvak.app is installed (the DMG or `brew install --cask postvak`); with only the `postvak` command installed, macOS shows them under its default sender. Bursts of new mail come as one notification, as on Linux. Clicking a notification opens the message in the window, which starts if it is not open. Notification Center keeps notifications until you clear them, so only the latest 64 open their message; a click on an older one just brings Postvak to the front. System Settings › Notifications › Postvak turns the sound or the banners off.

## Examples

- [Delete sign-in codes](#delete-sign-in-codes): `from`, `subject`, `older_than`, `read`, `delete`
- [Move list mail out of the inbox](#move-list-mail-out-of-the-inbox): `to_me`, `header`, `move`, `notify`
- [Give an alias its own folder](#give-an-alias-its-own-folder): `alias`, `move`, `notify`
- [Move shop mail after notifying](#move-shop-mail-after-notifying): `older_than`, `move`
- [Leave important mail in INBOX for your phone](#leave-important-mail-in-inbox-for-your-phone): `younger_than`, `stop`
- [Tag shop mail except receipts](#tag-shop-mail-except-receipts): `none`, `tag`
- [Flag certain people or topics](#flag-certain-people-or-topics): `any`, `flagged`
- [Set large mail aside](#set-large-mail-aside): `larger_than`, `flagged`, `move`, `stop`
- [Archive read mail after 30 days](#archive-read-mail-after-30-days): `read`, `older_than`, `archive`
- [Tag spam by its score](#tag-spam-by-its-score): `header` with a regex

### Delete sign-in codes

Delete sign-in codes and magic links an hour after you read them:

```toml
[[rules]]
name = "purge sign-in codes"
match.from = { regex = "no-?reply@" }
match.subject = { regex = "sign.?in|verification code|magic link" }
match.older_than = "1h"
match.read = true
actions.delete = true
```

### Move list mail out of the inbox

Move list mail that is not addressed to you, without a notification:

```toml
[[rules]]
name = "list mail"
match.to_me = false
match.header.List-Unsubscribe = { regex = "." }
actions.move = "Lists"
actions.notify = false
```

### Give an alias its own folder

Give a shop alias its own folder, but still notify:

```toml
[[rules]]
name = "shop alias"
match.alias = "*@shop.example.com"
actions.move = "Shopping"
actions.notify = true
```

### Move shop mail after notifying

Leave shop mail in INBOX for 15 minutes before moving it, so mail apps on your other devices, which mostly notify only for INBOX, see it first. Postvak notifies too, since nothing moved the message when it arrived. The move runs on the first full sync after those 15 minutes, so up to `sync_interval_secs` later:

```toml
[[rules]]
name = "shop mail, after notifying"
match.from = { contains = "shop.example.com" }
match.older_than = "15min"
actions.move = "Shopping"
```

### Leave important mail in INBOX for your phone

When several rules move mail you want your phone to notify about, or a later rule moves everything else, hold the mail back with one rule near the top of the file instead. For 15 minutes after it arrives, `stop` keeps the rules below it from moving it; after that they run as usual:

```toml
[[rules]]
name = "hold important mail for the phone"
match.younger_than = "15min"
match.any = [
  { from_domain = ["bank.example", "work.example"] },
  { subject = { regex = "sign-?in alert|new device" } },
]
actions.stop = true
```

### Tag shop mail except receipts

Tag shop mail as Important (Thunderbird's `$label1`), except receipts and invoices:

```toml
[[rules]]
name = "tag shop mail"
match.from = { contains = "shop.example.com" }
match.none = [{ subject = { contains = ["receipt", "invoice"] } }]
actions.tag = "$label1"
```

### Flag certain people or topics

Flag mail from either of two people, or about an outage:

```toml
[[rules]]
name = "flag the important ones"
match.any = [
  { from = { equals = ["alice@example.com", "bob@example.com"] } },
  { subject = { contains = ["outage", "incident"] } },
]
actions.flagged = true
```

### Set large mail aside

Move large mail you have not flagged to its own folder, and leave it to no other rule:

```toml
[[rules]]
name = "large mail"
match.larger_than = "10MB"
match.flagged = false
actions.move = "Large"
actions.stop = true
```

### Archive read mail after 30 days

```toml
[[rules]]
name = "archive old read mail"
match.read = true
match.older_than = "30d"
actions.archive = true
```

### Tag spam by its score

Filter junk on your provider's spam score. Many servers score mail but only move it to Junk above a high threshold; SpamAssassin writes the score as stars in `X-Spam-Level`, and some providers use their own header, such as `X-Soverin-Spam-Level`. Look at the headers of a few spam and legitimate messages with `postvak headers UID` to find yours, tag first to see what a threshold catches, then change the action to `actions.junk = true` once it only catches spam:

```toml
[[rules]]
name = "spam suspects"
match.header.X-Spam-Level = { regex = '^\*{2,}' }
actions.tag = "$SpamSuspect"
```

## Sharing rules between devices

`rules.toml` is a plain file, so a file sync such as Syncthing can keep it the same on every device; the daemon picks up a synced change within seconds. Sync that file alone: `config.toml` holds settings for this device, such as a `password = { command = "..." }`, and on macOS the mail stores live in the same folder. With Syncthing, share the folder that holds `rules.toml` (see [Accounts](accounts.md); `~/.var/app/io.github.postvak_app.postvak/config/postvak/` for the Flatpak) and give it this `.stignore`:

```
!/rules.toml
*
```

A symlink from that folder to a `rules.toml` elsewhere works outside the Flatpak too, since Postvak writes through it; Syncthing does not follow symlinks, so the real file has to be in the synced folder. The Flatpak cannot reach a file outside its sandbox, so share its own folder instead.

Every device that runs Postvak runs the rules itself. Moving or marking mail that another device already handled does nothing, but a deleted message's `.eml` backup is only on the device that deleted it.

When two devices change the file before they sync, the sync keeps one version and leaves the other beside it, such as `rules.sync-conflict-20261024-101500-ABCDEFG.toml` from Syncthing or `rules (conflicted copy 2026-10-24 101500).toml` from Nextcloud. Postvak ignores that copy, so its changes are lost until you move them into `rules.toml`. `postvak rules check`, the Rules view, the MCP `rules_check` tool and the daemon's log warn about such a copy, and about a rule whose `account` is not set up on this device, since it never runs there; name each account the same on every device. They also warn about a rule that moves mail to the folder it watches (INBOX in any case), since that move does nothing, and about a rule that uses `authenticated` for an account without `trusted_authserv_ids`, since no mail is authenticated there, naming the server its INBOX offers (see [Authenticated mail](#authenticated-mail)). They warn about a rule watching a folder that none of the accounts it is for syncs, such as a misspelled name, once each of those accounts has finished a pass through all its folders without errors; a rule for every account stays quiet while one account syncs the folder. Once an account has finished such a pass, they also warn about a `move` to a folder it does not have that is named like a special one (Trash, Junk, Spam, Archive, Sent, Drafts) while the account keeps that role in another folder, such as `[Gmail]/Trash`: move to that folder instead, or use `archive` for the archive and `junk` for the junk folder. Once an account has synced, they warn about a `move` folder that account does not have yet and that does not fit its server: one with `/` on a server that separates levels with another character, such as `.` on Courier, where it would become one folder with `/` in its name, and a top-level name on a server that keeps every folder under `INBOX.`, where creating it usually fails. Neither is an error, since the same file can serve accounts on other servers.

## Proposals

Agents never edit `rules.toml`. They run `postvak rules propose`, which appends the rule with `enabled = false` and `proposed_by` set. It takes one rule or several at once, stored in the order given or not at all, and with `--before NAME` or `--after NAME` puts them next to that rule instead of at the end, since order decides which rule runs first. An agent can take back its own pending proposal with `postvak rules withdraw NAME --by WHO`, or change it with `rules propose --replace`; neither touches an approved rule or another proposer's.

To change a rule that is already there, an agent runs `rules propose --edit` with the rule written as it should read, under its own name. The change is stored right after the rule as a disabled rule named `<name> (edit)`, with `replaces = "<name>"`, and the rule runs unchanged until you approve. Approving writes the change over the rule, which keeps its comments, place and on or off state, removes the proposal, and restarts the rule's clock so it acts as changed only on mail that arrives from then on. A rule has one pending edit at a time. If the rule is renamed or removed in the meantime, the edit can no longer be approved and `rules check` warns about it; reject it. In the window, an edit's card shows the rule's lines as a diff: `-` for what goes, `+` for what comes.

To review a proposal:

- `postvak rules list` shows every rule in file order, the order they run in, with its state and proposer; a pending proposal is `off` with a proposer, and an edit adds which rule it edits, then how many messages the rule acted on in the last 30 days. `--json` adds each rule's whole definition, its 0-based `position` in the file, `hits_30d` (those messages, each counted once, over every account) and `last_fired` (when it last acted, as Unix seconds, or `null` if never). Actions you took yourself are not counted.
- `postvak rules test NAME` previews what a proposal would do.
- `postvak rules coverage --include-pending` shows what the proposals change for a folder as a whole; see [Coverage](#coverage).
- `postvak rules approve NAME` enables it, or for an edit writes it over the rule it edits.
- `postvak rules reject NAME` removes it.

While the [daemon](daemon.md) runs, a new proposal brings a desktop notification ("A rule waits for your approval", naming it; several within two seconds come as one), on Linux and macOS alike. Clicking it opens the window on Rules, as `postvak gui --rules` does. Proposals already waiting when the daemon starts do not notify again, and neither does a proposal changed with `--replace`, which keeps its name.

`postvak rules move NAME --before OTHER` or `--after OTHER` changes when a rule runs, keeping the file's comments; the comments right above the rule move with it, and so does a pending edit of it, which stays right after it. The Rules view does the same when you drag a row, and the MCP `rules_move` tool for an agent granted `rules:write`.

## Coverage

`postvak rules coverage --folder INBOX` runs every enabled rule over the mail Postvak has stored for a folder, the way `rules test` does, and changes nothing. It counts the mail that a rule matches, that no rule matches, and that several rules match, which it calls contested. For mail that no rule matches, it lists the senders who send the most. For each contested message, it lists every rule that matches, then what would run: a delete on its own, or else every flag change and only the first move. A broad rule early in the file that takes mail meant for a later rule shows up here. `--include-pending` counts the proposals still waiting for approval as if they were on, each edit in place of the rule it edits. `--group-by domain` groups the senders by domain, and `--limit` (20 by default) caps the senders and contested messages shown. The counts always cover the whole folder.

`postvak senders --folder INBOX` groups a folder's mail by sender, or with `--group-by domain` by domain, the biggest group first. Each group shows its message and unread counts, its first and last date, a few subjects, and the enabled rules that already match some of it. `mail.shop.co.uk` and `shop.co.uk` count as one domain. `--after` and `--before` take a date such as `2026-01-15`, or `30d`, `3w`, `6mo` or `1y` back from now.

Neither command has a place in the window yet; both are also MCP tools, `rules_coverage` and `senders`.

`postvak rules schema` prints the JSON Schema of this file, and `rules.schema.json` in these docs holds the same schema.
