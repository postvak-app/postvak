# Command-Line Help for `postvak`

This document contains the help content for the `postvak` command-line program.

**Command Overview:**

* [`postvak`↴](#postvak)
* [`postvak run`↴](#postvak-run)
* [`postvak daemon`↴](#postvak-daemon)
* [`postvak daemon status`↴](#postvak-daemon-status)
* [`postvak daemon stop`↴](#postvak-daemon-stop)
* [`postvak gui`↴](#postvak-gui)
* [`postvak autostart`↴](#postvak-autostart)
* [`postvak autostart on`↴](#postvak-autostart-on)
* [`postvak autostart off`↴](#postvak-autostart-off)
* [`postvak autostart status`↴](#postvak-autostart-status)
* [`postvak service`↴](#postvak-service)
* [`postvak service install`↴](#postvak-service-install)
* [`postvak service remove`↴](#postvak-service-remove)
* [`postvak sync`↴](#postvak-sync)
* [`postvak attachment`↴](#postvak-attachment)
* [`postvak attachment list`↴](#postvak-attachment-list)
* [`postvak attachment save`↴](#postvak-attachment-save)
* [`postvak rules`↴](#postvak-rules)
* [`postvak rules check`↴](#postvak-rules-check)
* [`postvak rules test`↴](#postvak-rules-test)
* [`postvak rules schema`↴](#postvak-rules-schema)
* [`postvak rules propose`↴](#postvak-rules-propose)
* [`postvak rules withdraw`↴](#postvak-rules-withdraw)
* [`postvak rules approve`↴](#postvak-rules-approve)
* [`postvak rules reject`↴](#postvak-rules-reject)
* [`postvak rules move`↴](#postvak-rules-move)
* [`postvak rules list`↴](#postvak-rules-list)
* [`postvak rules lists`↴](#postvak-rules-lists)
* [`postvak rules coverage`↴](#postvak-rules-coverage)
* [`postvak rules apply-existing`↴](#postvak-rules-apply-existing)
* [`postvak folder`↴](#postvak-folder)
* [`postvak folder create`↴](#postvak-folder-create)
* [`postvak folder delete`↴](#postvak-folder-delete)
* [`postvak folder rename`↴](#postvak-folder-rename)
* [`postvak folders`↴](#postvak-folders)
* [`postvak list`↴](#postvak-list)
* [`postvak search`↴](#postvak-search)
* [`postvak senders`↴](#postvak-senders)
* [`postvak headers`↴](#postvak-headers)
* [`postvak show`↴](#postvak-show)
* [`postvak unsubscribe`↴](#postvak-unsubscribe)
* [`postvak mark`↴](#postvak-mark)
* [`postvak move`↴](#postvak-move)
* [`postvak archive`↴](#postvak-archive)
* [`postvak delete`↴](#postvak-delete)
* [`postvak junk`↴](#postvak-junk)
* [`postvak log`↴](#postvak-log)
* [`postvak trash`↴](#postvak-trash)
* [`postvak trash list`↴](#postvak-trash-list)
* [`postvak trash restore`↴](#postvak-trash-restore)
* [`postvak trash purge`↴](#postvak-trash-purge)
* [`postvak undo`↴](#postvak-undo)
* [`postvak account`↴](#postvak-account)
* [`postvak account add`↴](#postvak-account-add)
* [`postvak account list`↴](#postvak-account-list)
* [`postvak account trust`↴](#postvak-account-trust)
* [`postvak crashes`↴](#postvak-crashes)
* [`postvak crashes show`↴](#postvak-crashes-show)
* [`postvak guide`↴](#postvak-guide)
* [`postvak mcp`↴](#postvak-mcp)
* [`postvak mcp install`↴](#postvak-mcp-install)

## `postvak`

Your inbox, organized by rules you write

**Usage:** `postvak <COMMAND>`

###### **Subcommands:**

* `run` — Run the daemon in the foreground: sync all accounts continuously and apply rules; fails when a daemon already runs; Ctrl-C stops
* `daemon` — Inspect or stop the daemon that syncs your accounts; other commands start it when needed
* `gui` — Open the mail window; starts the daemon when needed
* `autostart` — Open the window at login, hidden in the tray: a launch agent on macOS, an autostart entry on Linux
* `service` — Start the daemon at login: a launchd agent on macOS, a systemd user unit on Linux
* `sync` — Ask the daemon to sync now and apply rules; reports new messages and actions per account, or why one could not sync
* `attachment` — List or save a message's attachments
* `rules` — Inspect and test rules.toml
* `folder` — Create, rename or delete a folder on the server
* `folders` — List folders with message and unread counts
* `list` — List recent messages, newest first, every account in one order
* `search` — Search subject, addresses, attachment names and bodies, newest first; words, "phrases", -not and key:value operators such as from:acme, in:Archive, is:unread or after:7d; the last word also matches as a prefix; close matches such as corrected typos follow when exact hits are few
* `senders` — Group one folder's stored mail by sender address or registrable domain, the largest group first: message and unread counts, first and last date, a few subjects and the enabled rules that already match some of it
* `headers` — Every header of one message, in order, unfolded and decoded as header rules see it; no body
* `show` — Show one message
* `unsubscribe` — Leave a message's mailing list: by one-click POST when the sender offers it (RFC 8058), else print the page or address from its List-Unsubscribe header to open. Once the sender accepted it, delete can remove the message
* `mark` — Mark messages read or unread, flagged or unflagged
* `move` — Move messages to another folder, creating it if needed
* `archive` — Move messages to the Archive folder
* `delete` — Move messages to Trash; inside Trash, or without one, delete them keeping a local .eml backup
* `junk` — Report messages as junk: tag them $Junk, which trains server spam filters, and move them to the Junk folder; --not tags them $NotJunk and moves them to INBOX
* `log` — Show what rules and direct actions did, newest first, with the number of the action each row is part of, which undo takes
* `trash` — Deleted mail kept for the retention period
* `undo` — Put back what one logged action did: move its mail back, reverse its read, flagged and tag changes, and restore what it deleted from the backups; mail changed since is left alone and said so. Without a number, the newest action of the CLI, the window or an agent that is not undone yet, never a rule's
* `account` — Manage accounts
* `crashes` — List the local crash reports, newest first; they are never sent anywhere
* `guide` — Print the agent guide, how an LLM should drive Postvak, or another page of the documentation
* `mcp` — Serve Postvak to an agent host over MCP on stdio; hosts start this, see `postvak mcp install`



## `postvak run`

Run the daemon in the foreground: sync all accounts continuously and apply rules; fails when a daemon already runs; Ctrl-C stops

**Usage:** `postvak run`



## `postvak daemon`

Inspect or stop the daemon that syncs your accounts; other commands start it when needed

**Usage:** `postvak daemon <COMMAND>`

###### **Subcommands:**

* `status` — The daemon's pid, version, uptime, client count and each account's activity, and whether its server offers UIDPLUS, without which a delete can also remove mail another app marks deleted at that moment
* `stop` — Stop the daemon; it starts again when a command needs it



## `postvak daemon status`

The daemon's pid, version, uptime, client count and each account's activity, and whether its server offers UIDPLUS, without which a delete can also remove mail another app marks deleted at that moment

**Usage:** `postvak daemon status [OPTIONS]`

###### **Options:**

* `--json`



## `postvak daemon stop`

Stop the daemon; it starts again when a command needs it

**Usage:** `postvak daemon stop`



## `postvak gui`

Open the mail window; starts the daemon when needed

**Usage:** `postvak gui [OPTIONS]`

###### **Options:**

* `--safe-mode` — Draw with the software renderer and ignore the saved window layout, for when the window will not open; entered by itself after two starts in a row that died before the window showed
* `--open <UID>` — Show this message (a uid in --folder, as `list` prints it); with a window already open, that window shows it
* `--account <ACCOUNT>` — The account of the message to show; the first account when left out
* `--folder <FOLDER>` — The folder of the message to show

  Default value: `INBOX`
* `--background` — Start out of the way, as at login: in the tray, minimized on macOS, or shown where there is no tray
* `--rules` — Show the Rules view, where proposals wait for approval; with a window already open, that window shows it



## `postvak autostart`

Open the window at login, hidden in the tray: a launch agent on macOS, an autostart entry on Linux

**Usage:** `postvak autostart <COMMAND>`

###### **Subcommands:**

* `on` — Open the window at login; in a Flatpak the desktop may ask first
* `off` — Stop opening the window at login
* `status` — Say whether the window opens at login



## `postvak autostart on`

Open the window at login; in a Flatpak the desktop may ask first

**Usage:** `postvak autostart on`



## `postvak autostart off`

Stop opening the window at login

**Usage:** `postvak autostart off`



## `postvak autostart status`

Say whether the window opens at login

**Usage:** `postvak autostart status`



## `postvak service`

Start the daemon at login: a launchd agent on macOS, a systemd user unit on Linux

**Usage:** `postvak service <COMMAND>`

###### **Subcommands:**

* `install` — Install and start the service; stops a daemon that is already running so the service's takes over
* `remove` — Stop and remove the service



## `postvak service install`

Install and start the service; stops a daemon that is already running so the service's takes over

**Usage:** `postvak service install [OPTIONS]`

###### **Options:**

* `--dry-run` — Print the file and the commands and change nothing



## `postvak service remove`

Stop and remove the service

**Usage:** `postvak service remove [OPTIONS]`

###### **Options:**

* `--dry-run` — Print the commands and change nothing



## `postvak sync`

Ask the daemon to sync now and apply rules; reports new messages and actions per account, or why one could not sync

**Usage:** `postvak sync [OPTIONS]`

###### **Options:**

* `--account <ACCOUNT>`



## `postvak attachment`

List or save a message's attachments

**Usage:** `postvak attachment <COMMAND>`

###### **Subcommands:**

* `list` — Index, type, size and name of each attachment
* `save` — Save attachment N, as numbered by the attachment list, into a directory; returns the written path



## `postvak attachment list`

Index, type, size and name of each attachment

**Usage:** `postvak attachment list [OPTIONS] <UID>`

###### **Arguments:**

* `<UID>`

###### **Options:**

* `--account <ACCOUNT>`
* `--folder <FOLDER>`

  Default value: `INBOX`
* `--json`



## `postvak attachment save`

Save attachment N, as numbered by the attachment list, into a directory; returns the written path

**Usage:** `postvak attachment save [OPTIONS] <UID> <N>`

###### **Arguments:**

* `<UID>`
* `<N>`

###### **Options:**

* `--dir <DIR>`

  Default value: `.`
* `--account <ACCOUNT>`
* `--folder <FOLDER>`

  Default value: `INBOX`



## `postvak rules`

Inspect and test rules.toml

**Usage:** `postvak rules <COMMAND>`

###### **Subcommands:**

* `check` — Validate rules.toml, and warn about what keeps it from doing what it says, such as a conflicting copy a file sync left beside it or a rule for an account not set up here
* `test` — Dry run: print what each rule would do to the cached messages, and warn where `authenticated` cannot hold; naming a rule previews it even while disabled
* `schema` — JSON Schema for rules.toml; a proposal is one entry of `rules`
* `propose` — Read one rule, a JSON array of rules or {"rules": [...]} on stdin and add them disabled, in order, for a human to approve; all or none are stored. {"lists": {...}} proposes new values for named lists, alone or beside "rules"
* `withdraw` — Remove your own pending proposal: a disabled rule whose proposed_by is yours, or with `list` your pending change of that list; anything else is left for a human
* `approve` — Enable a disabled rule, such as a proposal, or write an edit proposal over the rule it edits; with `list`, write that list's pending change into [lists] and restart the clock of every rule that uses it
* `reject` — Remove a pending proposal; with `list`, that list's pending change, whoever proposed it
* `move` — Move a rule right before or after another, which changes the order the rules run in: earlier rules run first
* `list` — Every rule in file order, the order they run in: its position, its whole definition, whether it is enabled, who proposed it and the values of the lists it uses
* `lists` — Every named list in rules.toml, which conditions refer to with {"list": "name"}: its values, the rules that use it, and its pending change with who proposed it
* `coverage` — Run the rules over one folder's stored mail without acting: how many messages a rule matches, none matches and several match; the top senders of unmatched mail; for each contested message every rule that matches and what would run under the real precedence
* `apply-existing` — Run one enabled rule against mail that predates it; dry run lists what it would do



## `postvak rules check`

Validate rules.toml, and warn about what keeps it from doing what it says, such as a conflicting copy a file sync left beside it or a rule for an account not set up here

**Usage:** `postvak rules check`



## `postvak rules test`

Dry run: print what each rule would do to the cached messages, and warn where `authenticated` cannot hold; naming a rule previews it even while disabled

**Usage:** `postvak rules test [OPTIONS] [NAME]`

###### **Arguments:**

* `<NAME>`

###### **Options:**

* `--account <ACCOUNT>`
* `--stdin` — Preview one rule read as JSON from stdin instead of rules.toml
* `--as-of <AS_OF>` — Evaluate as if it were this time, to see what older_than and younger_than will do: a span ahead of now (15min, 1d), a local date (2026-11-01), a local time (2026-11-01T14:00) or an RFC 3339 timestamp; the mail and its read, flagged and tags stay as they are now, and nothing is applied
* `--as-arrival` — Evaluate every stored message as if it arrived just now, to see what younger_than and older_than holds and after waits do to new mail; with as_of, that much later, such as 20min; the mail and its read, flagged and tags stay as they are now, and nothing is applied
* `--list <LIST>` — Preview approving this list's pending change: per rule that uses it, the cached messages it matches now and after



## `postvak rules schema`

JSON Schema for rules.toml; a proposal is one entry of `rules`

**Usage:** `postvak rules schema`

`postvak guide rules` explains every key, with examples.



## `postvak rules propose`

Read one rule, a JSON array of rules or {"rules": [...]} on stdin and add them disabled, in order, for a human to approve; all or none are stored. {"lists": {...}} proposes new values for named lists, alone or beside "rules"

**Usage:** `postvak rules propose [OPTIONS]`

###### **Options:**

* `--by <BY>` — Who proposes it, recorded as proposed_by = "cli:WHO"
* `--before <BEFORE>` — Put the rules right before this rule instead of at the end
* `--after <AFTER>` — Put the rules right after this rule instead of at the end
* `--replace` — Replace your own pending proposal of the same name instead of refusing the name
* `--edit` — Propose changes to the existing rules of these names, stored as '<name> (edit)'; each rule runs unchanged until approved



## `postvak rules withdraw`

Remove your own pending proposal: a disabled rule whose proposed_by is yours, or with `list` your pending change of that list; anything else is left for a human

**Usage:** `postvak rules withdraw [OPTIONS] <NAME>`

###### **Arguments:**

* `<NAME>`

###### **Options:**

* `--by <BY>` — Who proposed it, as given to `rules propose --by`
* `--list` — NAME is a list with a pending change, not a rule



## `postvak rules approve`

Enable a disabled rule, such as a proposal, or write an edit proposal over the rule it edits; with `list`, write that list's pending change into [lists] and restart the clock of every rule that uses it

**Usage:** `postvak rules approve [OPTIONS] <NAME>`

###### **Arguments:**

* `<NAME>`

###### **Options:**

* `--list` — NAME is a list with a pending change, not a rule



## `postvak rules reject`

Remove a pending proposal; with `list`, that list's pending change, whoever proposed it

**Usage:** `postvak rules reject [OPTIONS] <NAME>`

###### **Arguments:**

* `<NAME>`

###### **Options:**

* `--list` — NAME is a list with a pending change, not a rule



## `postvak rules move`

Move a rule right before or after another, which changes the order the rules run in: earlier rules run first

**Usage:** `postvak rules move [OPTIONS] <NAME>`

###### **Arguments:**

* `<NAME>`

###### **Options:**

* `--before <BEFORE>` — Put it right before this rule
* `--after <AFTER>` — Put it right after this rule



## `postvak rules list`

Every rule in file order, the order they run in: its position, its whole definition, whether it is enabled, who proposed it and the values of the lists it uses

**Usage:** `postvak rules list [OPTIONS]`

###### **Options:**

* `--json`



## `postvak rules lists`

Every named list in rules.toml, which conditions refer to with {"list": "name"}: its values, the rules that use it, and its pending change with who proposed it

**Usage:** `postvak rules lists [OPTIONS]`

###### **Options:**

* `--json`



## `postvak rules coverage`

Run the rules over one folder's stored mail without acting: how many messages a rule matches, none matches and several match; the top senders of unmatched mail; for each contested message every rule that matches and what would run under the real precedence

**Usage:** `postvak rules coverage [OPTIONS]`

###### **Options:**

* `--account <ACCOUNT>`
* `--folder <FOLDER>`

  Default value: `INBOX`
* `--include-pending` — Count the proposals still waiting for approval as if they were enabled
* `--group-by <GROUP_BY>` — How to group the senders of unmatched mail

  Default value: `address`

  Possible values:
  - `address`:
    The sender's address
  - `domain`:
    The registrable domain of the sender's address

* `--limit <LIMIT>` — At most this many unmatched senders and contested messages per account; the counts cover everything

  Default value: `20`
* `--as-of <AS_OF>` — Evaluate as if it were this time, to see what older_than and younger_than will do: a span ahead of now (15min, 1d), a local date (2026-11-01), a local time (2026-11-01T14:00) or an RFC 3339 timestamp; the mail and its read, flagged and tags stay as they are now, and nothing is applied
* `--as-arrival` — Evaluate every stored message as if it arrived just now, to see what younger_than and older_than holds and after waits do to new mail; with as_of, that much later, such as 20min; the mail and its read, flagged and tags stay as they are now, and nothing is applied
* `--json`



## `postvak rules apply-existing`

Run one enabled rule against mail that predates it; dry run lists what it would do

**Usage:** `postvak rules apply-existing [OPTIONS] <NAME>`

###### **Arguments:**

* `<NAME>`

###### **Options:**

* `--account <ACCOUNT>`
* `--dry-run`
* `--yes` — Run even when the rule would permanently delete mail from the server



## `postvak folder`

Create, rename or delete a folder on the server

**Usage:** `postvak folder <COMMAND>`

###### **Subcommands:**

* `create` — Create a folder and subscribe to it
* `delete` — Delete a folder; one holding mail needs --trash or --permanent
* `rename` — Rename a folder and the folders under it; rules that name it follow



## `postvak folder create`

Create a folder and subscribe to it

**Usage:** `postvak folder create [OPTIONS] <NAME>`

###### **Arguments:**

* `<NAME>`

###### **Options:**

* `--account <ACCOUNT>`



## `postvak folder delete`

Delete a folder; one holding mail needs --trash or --permanent

**Usage:** `postvak folder delete [OPTIONS] <NAME>`

###### **Arguments:**

* `<NAME>`

###### **Options:**

* `--account <ACCOUNT>`
* `--trash` — Move the mail to Trash first
* `--permanent` — Delete the mail with the folder, keeping an .eml backup



## `postvak folder rename`

Rename a folder and the folders under it; rules that name it follow

**Usage:** `postvak folder rename [OPTIONS] <FROM> <TO>`

###### **Arguments:**

* `<FROM>`
* `<TO>`

###### **Options:**

* `--account <ACCOUNT>`



## `postvak folders`

List folders with message and unread counts

**Usage:** `postvak folders [OPTIONS]`

###### **Options:**

* `--account <ACCOUNT>`
* `--json`



## `postvak list`

List recent messages, newest first, every account in one order

**Usage:** `postvak list [OPTIONS]`

###### **Options:**

* `--account <ACCOUNT>`
* `--folder <FOLDER>`

  Default value: `INBOX`
* `--limit <LIMIT>`

  Default value: `50`
* `--json`
* `--threads` — Group by conversation, the most recently active thread first
* `--unread` — Only unread messages; with --threads, threads with an unread message
* `--flagged` — Only flagged messages; with --threads, threads with a flagged message
* `--attachments` — Only messages with attachments, guessed from the headers; with --threads, threads with one
* `--cursor <CURSOR>` — Continue after the page that ended with this `next`, with the same other arguments; the rows of every account come newest first in one order, and the limit counts them together
* `--fields <FIELDS>` — Only these keys in each JSON row, such as uid, from_addr, subject, date; compact stands for account, folder, uid, date, from_addr, subject and flags, plus depth in a thread view and match in a search; account always stays
* `--count-only` — Return only how many match in all, messages or in a thread view threads, as {"count": N}, without rows; takes no cursor or fields



## `postvak search`

Search subject, addresses, attachment names and bodies, newest first; words, "phrases", -not and key:value operators such as from:acme, in:Archive, is:unread or after:7d; the last word also matches as a prefix; close matches such as corrected typos follow when exact hits are few

**Usage:** `postvak search [OPTIONS] <QUERY>`

Operators:
  from:acme, to:x, cc:x, subject:x, attachment:x  the word in that column; to: covers To, Cc and Delivered-To; a quoted value is a phrase; -from:x excludes
  from:me, to:me, cc:me                           your address or an alias, from the account's config
  in:Archive, in:"Projects/Postvak"              one folder
  has:attachment                                  with an attachment, as guessed from the headers
  is:unread, is:read, is:flagged                  by flag
  before:2026-01-15, after:2026-01-01             by date, in local time; before: is exclusive, after: inclusive
  after:7d, before:3w, after:6mo, after:1y        days, weeks, months or years back from now
  account:work                                    one account, where several are searched
Anything else, such as an unknown key or a date that does not parse, searches as a word.

###### **Arguments:**

* `<QUERY>`

###### **Options:**

* `--account <ACCOUNT>` — Only this account; the same as account: in the query
* `--folder <FOLDER>` — Only this folder; the same as in: in the query
* `--server` — Ask the server too, which searches bodies not downloaded yet; each folder costs a round trip. One page: no --cursor or --count-only
* `--exact` — Only what matches as typed: no close matches such as corrected typos
* `--limit <LIMIT>`

  Default value: `50`
* `--json`
* `--cursor <CURSOR>` — Continue after the page that ended with this `next`, with the same other arguments; the rows of every account come newest first in one order, and the limit counts them together
* `--fields <FIELDS>` — Only these keys in each JSON row, such as uid, from_addr, subject, date; compact stands for account, folder, uid, date, from_addr, subject and flags, plus depth in a thread view and match in a search; account always stays
* `--count-only` — Return only how many match in all, messages or in a thread view threads, as {"count": N}, without rows; takes no cursor or fields



## `postvak senders`

Group one folder's stored mail by sender address or registrable domain, the largest group first: message and unread counts, first and last date, a few subjects and the enabled rules that already match some of it

**Usage:** `postvak senders [OPTIONS]`

###### **Options:**

* `--account <ACCOUNT>`
* `--folder <FOLDER>`

  Default value: `INBOX`
* `--group-by <GROUP_BY>`

  Default value: `address`

  Possible values:
  - `address`:
    The sender's address
  - `domain`:
    The registrable domain of the sender's address

* `--limit <LIMIT>` — At most this many groups per account

  Default value: `50`
* `--after <AFTER>` — Only mail that arrived on or after this date: 2026-01-15, or 30d, 3w, 6mo, 1y back from now
* `--before <BEFORE>` — Only mail that arrived before this date: 2026-01-15, or 30d, 3w, 6mo, 1y back from now
* `--json`



## `postvak headers`

Every header of one message, in order, unfolded and decoded as header rules see it; no body

**Usage:** `postvak headers [OPTIONS] <UID>`

###### **Arguments:**

* `<UID>`

###### **Options:**

* `--account <ACCOUNT>`
* `--folder <FOLDER>`

  Default value: `INBOX`
* `--json`



## `postvak show`

Show one message

**Usage:** `postvak show [OPTIONS] <UID>`

###### **Arguments:**

* `<UID>`

###### **Options:**

* `--account <ACCOUNT>`
* `--folder <FOLDER>`

  Default value: `INBOX`
* `--raw` — Print the raw RFC 5322 message instead of the text body
* `--json`



## `postvak unsubscribe`

Leave a message's mailing list: by one-click POST when the sender offers it (RFC 8058), else print the page or address from its List-Unsubscribe header to open. Once the sender accepted it, delete can remove the message

**Usage:** `postvak unsubscribe [OPTIONS] <UID>`

###### **Arguments:**

* `<UID>`

###### **Options:**

* `--account <ACCOUNT>`
* `--folder <FOLDER>`

  Default value: `INBOX`
* `--dry-run` — Print what would happen without contacting the sender
* `--json`



## `postvak mark`

Mark messages read or unread, flagged or unflagged

**Usage:** `postvak mark [OPTIONS] <HOW> <UIDS>...`

###### **Arguments:**

* `<HOW>`

  Possible values: `flagged`, `read`, `unflagged`, `unread`

* `<UIDS>` — Message uids in --folder, as `list` prints them

###### **Options:**

* `--account <ACCOUNT>`
* `--folder <FOLDER>`

  Default value: `INBOX`
* `--dry-run` — Print what would happen without touching the server



## `postvak move`

Move messages to another folder, creating it if needed

**Usage:** `postvak move [OPTIONS] --to <TO> <UIDS>...`

###### **Arguments:**

* `<UIDS>` — Message uids in --folder, as `list` prints them

###### **Options:**

* `--to <TO>`
* `--account <ACCOUNT>`
* `--folder <FOLDER>`

  Default value: `INBOX`
* `--dry-run` — Print what would happen without touching the server



## `postvak archive`

Move messages to the Archive folder

**Usage:** `postvak archive [OPTIONS] <UIDS>...`

###### **Arguments:**

* `<UIDS>` — Message uids in --folder, as `list` prints them

###### **Options:**

* `--account <ACCOUNT>`
* `--folder <FOLDER>`

  Default value: `INBOX`
* `--dry-run` — Print what would happen without touching the server



## `postvak delete`

Move messages to Trash; inside Trash, or without one, delete them keeping a local .eml backup

**Usage:** `postvak delete [OPTIONS] <UIDS>...`

###### **Arguments:**

* `<UIDS>` — Message uids in --folder, as `list` prints them

###### **Options:**

* `--account <ACCOUNT>`
* `--folder <FOLDER>`

  Default value: `INBOX`
* `--dry-run` — Print what would happen without touching the server



## `postvak junk`

Report messages as junk: tag them $Junk, which trains server spam filters, and move them to the Junk folder; --not tags them $NotJunk and moves them to INBOX

**Usage:** `postvak junk [OPTIONS] <UIDS>...`

###### **Arguments:**

* `<UIDS>` — Message uids in --folder, as `list` prints them

###### **Options:**

* `--account <ACCOUNT>`
* `--folder <FOLDER>`

  Default value: `INBOX`
* `--dry-run` — Print what would happen without touching the server
* `--not` — Not junk after all



## `postvak log`

Show what rules and direct actions did, newest first, with the number of the action each row is part of, which undo takes

**Usage:** `postvak log [OPTIONS]`

###### **Options:**

* `--account <ACCOUNT>`
* `--limit <LIMIT>`

  Default value: `50`
* `--json`



## `postvak trash`

Deleted mail kept for the retention period

**Usage:** `postvak trash <COMMAND>`

###### **Subcommands:**

* `list` — Deleted mail kept as .eml backups for the retention period
* `restore` — Append a trashed .eml back into its original folder
* `purge` — Remove .eml backups older than the account's retention period



## `postvak trash list`

Deleted mail kept as .eml backups for the retention period

**Usage:** `postvak trash list [OPTIONS]`

###### **Options:**

* `--account <ACCOUNT>`



## `postvak trash restore`

Append a trashed .eml back into its original folder

**Usage:** `postvak trash restore [OPTIONS] <FILE>`

###### **Arguments:**

* `<FILE>`

###### **Options:**

* `--account <ACCOUNT>`



## `postvak trash purge`

Remove .eml backups older than the account's retention period

**Usage:** `postvak trash purge [OPTIONS]`

###### **Options:**

* `--account <ACCOUNT>`



## `postvak undo`

Put back what one logged action did: move its mail back, reverse its read, flagged and tag changes, and restore what it deleted from the backups; mail changed since is left alone and said so. Without a number, the newest action of the CLI, the window or an agent that is not undone yet, never a rule's

**Usage:** `postvak undo [OPTIONS] [ACTION]`

###### **Arguments:**

* `<ACTION>` — The action's number, as `log` shows it

###### **Options:**

* `--account <ACCOUNT>` — Required with a number when several accounts are set up
* `--dry-run` — Show what would be put back without touching the server
* `--json`



## `postvak account`

Manage accounts

**Usage:** `postvak account <COMMAND>`

###### **Subcommands:**

* `add` — Add an IMAP account; the daemon tests the login first and nothing is saved when it fails, then syncs it once
* `list` — Each configured account: name, username and server; never the password source
* `trust` — Trust a server's Authentication-Results headers for the authenticated rule condition: add AUTHSERV_ID, the name at the start of the topmost one, to the account's trusted_authserv_ids



## `postvak account add`

Add an IMAP account; the daemon tests the login first and nothing is saved when it fails, then syncs it once

**Usage:** `postvak account add`



## `postvak account list`

Each configured account: name, username and server; never the password source

**Usage:** `postvak account list [OPTIONS]`

###### **Options:**

* `--json`



## `postvak account trust`

Trust a server's Authentication-Results headers for the authenticated rule condition: add AUTHSERV_ID, the name at the start of the topmost one, to the account's trusted_authserv_ids

**Usage:** `postvak account trust <ACCOUNT> <AUTHSERV_ID>`

###### **Arguments:**

* `<ACCOUNT>`
* `<AUTHSERV_ID>`



## `postvak crashes`

List the local crash reports, newest first; they are never sent anywhere

**Usage:** `postvak crashes [COMMAND]`

###### **Subcommands:**

* `show` — Print one crash report, by the id `postvak crashes` lists



## `postvak crashes show`

Print one crash report, by the id `postvak crashes` lists

**Usage:** `postvak crashes show <ID>`

###### **Arguments:**

* `<ID>`



## `postvak guide`

Print the agent guide, how an LLM should drive Postvak, or another page of the documentation

**Usage:** `postvak guide [PAGE]`

###### **Arguments:**

* `<PAGE>` — The page; without one, the agent guide

  Possible values:
  - `agent-guide`:
    How an agent should read mail, write and propose rules, and act on mail
  - `rules`:
    Every key of a rule: its conditions and actions, with examples
  - `search`:
    The search syntax: words, phrases, operators and close matches
  - `cli`:
    Every command and flag of the postvak command line
  - `mcp`:
    The MCP server: each scope and the tools it unlocks, and what the tools return




## `postvak mcp`

Serve Postvak to an agent host over MCP on stdio; hosts start this, see `postvak mcp install`

**Usage:** `postvak mcp [OPTIONS]
       mcp <COMMAND>`

###### **Subcommands:**

* `install` — Register `postvak mcp` with an agent host; re-run it to change the scopes

###### **Options:**

* `--scopes <SCOPES>` — Comma-separated: read, read:headers, read:bodies, rules:propose, rules:write, files:write, mail:modify

  Default value: `read,rules:propose`
* `--account <ACCOUNT>` — Only this account; repeatable; default every account
* `--save-dir <SAVE_DIR>` — The one folder files:write saves attachments into, or below; default your Downloads folder



## `postvak mcp install`

Register `postvak mcp` with an agent host; re-run it to change the scopes

**Usage:** `postvak mcp install [OPTIONS] <TARGET>`

###### **Arguments:**

* `<TARGET>`

  Possible values: `claude-code`, `claude-desktop`, `json`


###### **Options:**

* `--scopes <SCOPES>` — Comma-separated: read, read:headers, read:bodies, rules:propose, rules:write, files:write, mail:modify

  Default value: `read,rules:propose`
* `--account <ACCOUNT>` — Only this account; repeatable; default every account
* `--save-dir <SAVE_DIR>` — The one folder files:write saves attachments into, or below; default your Downloads folder
* `--remove` — Take the entry out again
* `--dry-run` — Print the change and write nothing



<hr/>

<small><i>
    This document was generated automatically by
    <a href="https://crates.io/crates/clap-markdown"><code>clap-markdown</code></a>.
</i></small>
