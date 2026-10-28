# Rules

Rules live in `rules.toml` next to `config.toml`. Postvak reads the file on every sync, and the daemon syncs every account within seconds of the file changing, whoever changed it. A file that fails to validate is rejected as a whole, and the previous rules stay active until the daemon stops or that account's settings change. When there are none, no rules run and new mail notifies as the account's `notify` setting says; once the file is fixed, rules that were already enabled also run on the mail that arrived meanwhile. Each account reports the error once, and syncing and actions carry on. `postvak rules check` validates the file; `postvak rules test` shows what each rule would do to the mail Postvak has cached.

```toml
[[rules]]
name = "github to folder"
match.header = { name = "List-Id", contains = "github.com" }
actions = [{ move = "Lists/GitHub" }, "mark_read"]
```

## Rule keys

| Key | Default | Meaning |
|---|---|---|
| `name` | required | Unique. Renaming a rule makes it a new rule. |
| `account` | every account | Only for this account. |
| `folder` | `INBOX` | The folder the rule watches. |
| `enabled` | `true` | `false` skips the rule. Proposals start disabled. |
| `proposed_by` | none | Set by `postvak rules propose`. |
| `match` | required | Conditions that must all hold. At least one. |
| `actions` | required | What to do. At least one. |

A tag is printable ASCII without spaces, backslashes or `( ) { % * " ]`, and `$PostvakRestored` is reserved. `name`, `account`, `folder` and `move` folders must not be blank, may not contain control characters, and are at most 255 characters.

## Conditions

Text conditions take exactly one of:

- `contains`: a case-insensitive substring.
- `equals`: the whole value, case-insensitive. On `from`, `to` and `cc` it also matches any single address in the field, so `equals = "a@example.com"` matches `Alice <a@example.com>, b@example.com`.
- `regex`: Rust `regex` syntax. Start with `(?i)` for case-insensitive.

`contains` and `equals` also take a list, which matches when any value does: `subject = { contains = ["receipt", "invoice"] }`.

| Key | Takes | Matches |
|---|---|---|
| `from`, `to`, `cc`, `subject` | text condition | That header. |
| `body` | text condition | The plain-text body; HTML mail is converted. Postvak downloads the body of new mail in the rule's folder for this. |
| `header` | text condition plus `name`, or a list of them | Any header, such as `List-Id`, unfolded and with encoded words (`=?UTF-8?…?=`) decoded. Of a repeated header only the last copy counts. Every header in a list must match. |
| `from_domain` | domain, or a list of them | The From address is in that domain or a subdomain of it: `paypal.com` matches `service@paypal.com` and `x@mail.paypal.com`, not `x@notpaypal.com`. Only the address counts, never the display name, and case does not matter. A list matches when any domain does. |
| `to_domain` | domain, or a list of them | The same, for any address in To. |
| `authenticated` | `true` or `false` | Your provider vouches that the mail comes from its From domain. Needs the account's `trusted_authserv_ids` (see [Authenticated mail](#authenticated-mail)). |
| `older_than` | duration: `30m`, `1h`, `2days` | Mail that arrived at least this long ago. |
| `larger_than` | size: `500KB`, `5MB`, `1GB` | Mail larger than this. A bare number is bytes, and `1KB` is 1024 bytes. |
| `seen` | `true` or `false` | Read or unread mail. |
| `flagged` | `true` or `false` | Flagged or unflagged mail. |
| `to_me` | `true` or `false` | To, Cc or Delivered-To holds your address or an alias. `false` catches list and bcc mail. |
| `alias` | address, `*` as wildcard | Mail sent to that alias. |
| `tag` | IMAP keyword, such as `$label1` | Mail carrying that keyword, ignoring case. Thunderbird shows keywords as tags. |
| `any` | list of conditions | Holds when at least one entry holds. |
| `none` | list of conditions | Holds when no entry holds. |

Each entry of `any` and `none` is a set of conditions that must all hold, written like `match` itself, so `none = [{ from = …, subject = … }]` only excludes mail that matches both. To exclude either, give each its own entry. Entries can hold `any` and `none` again, up to eight levels deep. A rule whose conditions need the body never fires on a message whose body could not be downloaded, even through `none`.

## Authenticated mail

Anyone can write any From address. A receiving mail server checks the sender's DKIM signature, SPF and DMARC, and writes its verdict in an `Authentication-Results` header at the top of the message. `authenticated = true` holds when that verdict says DMARC passed for the From domain. When it gives no DMARC pass or fail for that domain, DKIM passing for a domain aligned with the From domain is enough (the same domain, or a parent or subdomain of it within the same organisation, so a `mail.paypal.com` signature counts for `paypal.com`, and a `sendgrid.net` one does not); a DMARC fail always wins, since the domain's own policy may demand a closer match. SPF alone is not enough, since it vouches for the envelope sender, not the From address. A From with no address or with several is never authenticated.

A sender can write an `Authentication-Results` header too, so Postvak only reads the ones written by servers you list in the account's `trusted_authserv_ids` (see [Accounts](accounts.md)), recognised by the server name at their start (the authserv-id), and of those only the topmost, the one the last server added. To find the name, run `postvak headers UID` on a message you received, which shows its headers topmost first, and take the name after `Authentication-Results:` in the topmost one, such as `mx.google.com` for Gmail. List only a server that writes the header on every message it receives: on mail it left without one, the topmost header with its name would be the sender's. Postvak does not guess the name from the account's IMAP host for that reason. Without a list, no mail is authenticated, so `authenticated = true` never holds and `authenticated = false` holds for all mail; `rules check` warns about a rule that uses `authenticated` for such an account. Mail without a header from a listed server is not authenticated, so a rule with `authenticated = false` also catches it.

Use it with `from_domain` on rules that move mail you would act on, such as security alerts or invoices, so that mail which only claims to come from that domain stays in your inbox:

```toml
[[rules]]
name = "paypal"
match.from_domain = ["paypal.com", "paypal.nl"]
match.authenticated = true
actions = [{ move = "Finance" }]
```

## Actions

| Action | Effect |
|---|---|
| `"delete"` | Saves the message as `.eml` in the local trash, then removes it from the server. Later rules don't run for that message. |
| `"mark_read"` | Marks it read. |
| `"flag"` | Flags it. |
| `"archive"` | Moves it to the server's Archive folder. Fails when the server has none. |
| `{ move = "Folder/Sub" }` | Moves it to that folder, creating the folder if needed and subscribing to it, so mail apps that show only subscribed folders list it. |
| `{ tag = "$label1" }` | Adds that IMAP keyword, which Thunderbird and other clients show as a tag. The server must accept custom keywords. |
| `"notify"` | Notifies even when the message was moved. |
| `"silent"` | Never notifies. |
| `"stop"` | Later rules don't run for that message. |

A `delete` wins: when any matching rule deletes a message, no other rule's actions run for it. After a rule with `stop`, later rules in the file don't run for that message; earlier ones already did. Otherwise flags and tags are set before a move, and only the first `move` or `archive` that matches a message runs.

## When rules act

- On every sync, in file order. Because rules run again on each sync, `older_than` and `seen` can fire later, for example an hour after you read a sign-in code.
- A rule acts only on mail that arrived after the rule was enabled, so adding a rule never touches your history. `postvak rules apply-existing NAME` is the explicit opt-in. Run it with `--dry-run` first. When the rule would delete mail from the server for good (`delete`), it prints how many messages per account and how long their `.eml` backups are kept, then asks you to type `yes`; without a terminal it refuses unless you pass `--yes`. Moves and flags run without asking.
- A rule approved with `postvak rules approve` acts on mail that arrives after the approval. A rule you add or enable by editing the file acts on mail that arrives after the next sync picks it up.
- Renaming a rule, editing its account, folder, match or actions, or disabling and enabling it again, restarts that clock.
- Rules move a message out of a folder once. When it comes back, because you moved it back or a rule on another machine did, no rule moves or archives it again; the other actions still run. So two rules that move mail between two folders stop after one round. Postvak tells messages apart by Message-ID and arrival time, which servers keep when mail moves.
- Mail restored with `postvak trash restore` carries the `$PostvakRestored` keyword. Rules never act on it again. This needs a server that accepts custom keywords; without one, the same rule can delete restored mail again.

## Notifications

New INBOX mail notifies unless a rule moved, archived or deleted it, or a matching rule says `silent`; marking, flagging and tagging still notify. `notify` forces a notification for moved mail. When several matching rules say `notify` or `silent`, the last one wins. Only INBOX mail notifies, so `notify` on a rule for another folder does nothing. With `notify = false` on the account, only rules that say `notify` notify. Deleted mail, and mail found by the first sync of a folder, never notifies.

On Linux a notification shows the sender and the subject with Postvak's icon, and asks the desktop for its new-mail sound, which follows Do Not Disturb and your volume; your desktop's notification settings list Postvak, so you can turn the sound or the popups off there. When one sync finds more than three new messages for an account, they come as one notification ("12 new messages", naming the senders), which opens the newest. Plasma shows the account beside Postvak's name. Clicking a notification opens the message in the window, which starts if it is not open. On Wayland an open window cannot bring itself to the front, so Plasma highlights it on the task bar and GNOME says "Postvak is ready".

On macOS a notification shows the sender and the subject as Postvak's once Postvak.app is installed (the DMG or `brew install --cask postvak`); with only the `postvak` command installed, macOS shows them under its default sender. Bursts of new mail come as one notification, as on Linux. Clicking a notification opens the message in the window, which starts if it is not open. Notification Center keeps notifications until you clear them, so only the latest 64 open their message; a click on an older one just brings Postvak to the front. System Settings › Notifications › Postvak turns the sound or the banners off.

## Examples

Delete sign-in codes and magic links an hour after you read them:

```toml
[[rules]]
name = "purge sign-in codes"
match.from = { regex = "no-?reply@" }
match.subject = { regex = "(?i)sign.?in|verification code|magic link" }
match.older_than = "1h"
match.seen = true
actions = ["delete"]
```

Move list mail that is not addressed to you, without a notification:

```toml
[[rules]]
name = "list mail"
match.to_me = false
match.header = { name = "List-Unsubscribe", regex = "." }
actions = [{ move = "Lists" }, "silent"]
```

Give a shop alias its own folder, but still notify:

```toml
[[rules]]
name = "shop alias"
match.alias = "*@shop.example.com"
actions = [{ move = "Shopping" }, "notify"]
```

Tag shop mail as Important (Thunderbird's `$label1`), except receipts and invoices:

```toml
[[rules]]
name = "tag shop mail"
match.from = { contains = "shop.example.com" }
match.none = [{ subject = { contains = ["receipt", "invoice"] } }]
actions = [{ tag = "$label1" }]
```

Flag mail from either of two people, or about an outage:

```toml
[[rules]]
name = "flag the important ones"
match.any = [
  { from = { equals = ["alice@example.com", "bob@example.com"] } },
  { subject = { contains = ["outage", "incident"] } },
]
actions = ["flag"]
```

Move large mail you have not flagged to its own folder, and leave it to no other rule:

```toml
[[rules]]
name = "large mail"
match.larger_than = "10MB"
match.flagged = false
actions = [{ move = "Large" }, "stop"]
```

Archive read mail after 30 days:

```toml
[[rules]]
name = "archive old read mail"
match.seen = true
match.older_than = "30days"
actions = ["archive"]
```

Filter junk on your provider's spam score. Many servers score mail but only move it to Junk above a high threshold; SpamAssassin writes the score as stars in `X-Spam-Level`, and some providers use their own header, such as `X-Soverin-Spam-Level`. Look at the headers of a few spam and legitimate messages with `postvak headers UID` to find yours, tag first to see what a threshold catches, then change the action to `{ move = "Junk" }` once it only catches spam:

```toml
[[rules]]
name = "spam suspects"
match.header = { name = "X-Spam-Level", regex = '^\*{2,}' }
actions = [{ tag = "$SpamSuspect" }]
```

## Sharing rules between devices

`rules.toml` is a plain file, so a file sync such as Syncthing can keep it the same on every device; the daemon picks up a synced change within seconds. Sync that file alone: `config.toml` holds settings for this device, such as a `password = { command = "..." }`, and on macOS the mail stores live in the same folder. With Syncthing, share the folder that holds `rules.toml` (see [Accounts](accounts.md); `~/.var/app/io.github.postvak_app.postvak/config/postvak/` for the Flatpak) and give it this `.stignore`:

```
!/rules.toml
*
```

A symlink from that folder to a `rules.toml` elsewhere works outside the Flatpak too, since Postvak writes through it; Syncthing does not follow symlinks, so the real file has to be in the synced folder. The Flatpak cannot reach a file outside its sandbox, so share its own folder instead.

Every device that runs Postvak runs the rules itself. Moving or marking mail that another device already handled does nothing, but a deleted message's `.eml` backup is only on the device that deleted it.

When two devices change the file before they sync, the sync keeps one version and leaves the other beside it, such as `rules.sync-conflict-20261024-101500-ABCDEFG.toml` from Syncthing or `rules (conflicted copy 2026-10-24 101500).toml` from Nextcloud. Postvak ignores that copy, so its changes are lost until you move them into `rules.toml`. `postvak rules check`, the Rules view, the MCP `rules_check` tool and the daemon's log warn about such a copy, and about a rule whose `account` is not set up on this device, since it never runs there; name each account the same on every device. They also warn about a rule that moves mail to the folder it watches (INBOX in any case), since that move does nothing, and about a rule that uses `authenticated` for an account without `trusted_authserv_ids`, since no mail is authenticated there.

## Proposals

Agents never edit `rules.toml`. They run `postvak rules propose`, which appends the rule with `enabled = false` and `proposed_by` set. To review a proposal:

- `postvak rules list` shows every rule with its state and proposer; a pending proposal is `off` with a proposer.
- `postvak rules test NAME` previews what a proposal would do.
- `postvak rules approve NAME` enables it.
- `postvak rules reject NAME` removes it.

`postvak rules schema` prints the JSON Schema of this file, and `rules.schema.json` in these docs holds the same schema.
