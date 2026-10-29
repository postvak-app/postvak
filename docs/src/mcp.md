# Agents over MCP

`postvak mcp` lets an agent host such as Claude Desktop or Claude Code read your mail, propose rules and, when you allow it, act on mail. It speaks MCP on stdin and stdout, and the host starts it. It can do nothing you did not grant: each capability is a scope, and the host's config sets the scopes.

The defaults are privacy-friendly. An agent sees folders and headers but no message bodies, cannot change or delete mail, and can only propose rules, which stay off until you approve them. Bodies, acting on mail and approving rules are each a separate scope you add on purpose, and `--account` hides every other account from the agent.

## Setup

For Claude Desktop, register the server and restart the app:

```sh
postvak mcp install claude-desktop
```

For Claude Code:

```sh
postvak mcp install claude-code
```

The Claude Desktop install edits `claude_desktop_config.json` and keeps every other server in it. The file is rewritten pretty-printed with sorted keys, and the original is saved next to it as `claude_desktop_config.json.bak`. Re-running the install does not replace that backup unless you changed the file in between. The Claude Code install runs `claude mcp remove` and then `claude mcp add --scope user`, so a re-run replaces the old entry. With `--dry-run`, or when `claude` is not on your `PATH`, it prints those commands instead of running them.

For any other host, `postvak mcp install json` prints a snippet for its config:

```json
{
  "mcpServers": {
    "postvak": {
      "args": [
        "mcp",
        "--scopes",
        "read,rules:propose"
      ],
      "command": "/opt/homebrew/bin/postvak"
    }
  }
}
```

The `command` is the absolute path of the `postvak` you ran, because hosts started from the Dock do not see your shell's `PATH`: for Homebrew the `opt/` link, which survives `brew upgrade`, and for an AppImage the `.AppImage` file, so run the install again after moving it. The install command fills it in; the path above is an example.

Options for `postvak mcp install`:

- `--scopes` sets the scopes. Run the install again with other scopes to change them.
- `--account NAME` limits the server to one account. Repeat it for more.
- `--save-dir DIR` sets the folder `attachment_save` writes into, instead of your Downloads folder. Your home folder, a folder above it, and hidden folders or `Library` inside it are refused, as they hold what runs at login.
- `--remove` takes the entry out again.
- `--dry-run` shows only the postvak entry, or for Claude Code the `claude` commands, and writes nothing.

The install also prints a hint with the granted scopes and how to widen them. It goes to stderr, so the output of `install json` stays pure JSON.

## Scopes

| Scope | What it grants | Tools |
|---|---|---|
| `read` | Folders, each message's sender, recipients, subject and date, the activity log, rules and trash listings, previews of rules that match on neither the body nor a `header` condition, and a sync. No body text and no other headers, except the one bit of `Authentication-Results` an `authenticated` rule condition reveals. | `accounts`, `docs`, `folders`, `list`, `log`, `rules_check`, `rules_coverage`, `rules_list`, `rules_schema`, `rules_test`, `search`, `senders`, `status`, `sync`, `trash_list` |
| `read:headers` | Every header of a message, such as `List-Id`, `Authentication-Results` (beyond the one bit `authenticated` gives with `read`) or a spam score, from the local store, and with `read` previews of rules with a `header` condition; proposing, enabling or running such a rule needs it too. No body text. | `headers` |
| `read:bodies` | Message bodies and attachment names, search over indexed bodies and on the server (`search` with `server`) with a `snippet` of the body around a match, and with `read` previews of rules that match on the body. | `attachments`, `show` |
| `rules:propose` | Proposing rules, which are stored disabled until you approve them, and withdrawing its own proposals while they are still pending. | `rules_propose`, `rules_withdraw` |
| `rules:write` | Approving and rejecting rules, turning them on or off, and changing their order. | `rules_approve`, `rules_move`, `rules_reject`, `rules_set_enabled` |
| `files:write` | Writing attachment files into your Downloads folder, or the `--save-dir` you set, and the directories below it. The file name and content come from the sender. | `attachment_save` |
| `mail:modify` | Acting on mail and the trash. | `archive`, `delete`, `junk`, `mark`, `move`, `rules_apply_existing`, `trash_purge`, `trash_restore`, `unsubscribe` |

The default is `read,rules:propose`. Four example grants:

- Read-only: `--scopes read`. `sync` is in this scope and still runs your approved rules.
- Rule author: the default. The agent proposes, you approve in the window or with `postvak rules approve`. It can match on sender, recipients, subject, age, size and flags, but not on a `header` condition such as `List-Id`.
- Rule author who sees headers: `--scopes read,read:headers,rules:propose`. The agent can read `List-Id` or a spam score, then preview and propose a `header` rule, but sees no bodies.
- Inbox assistant: `--scopes read,read:headers,read:bodies,rules:propose,mail:modify`.

`rules:write` lets the agent approve its own rules, and an approved rule can delete mail. Leave it off. `files:write` lets the agent save a stranger's file into your save folder; leave it off too unless you need it.

An agent cannot add mail accounts: a password command runs as a shell command as you, so a prompt-injected email could make the agent run anything you can. Add accounts yourself with `postvak account add` or in the window. Earlier versions had an `accounts:write` scope for this; a host config that still names it starts without it, with a warning.

## Tools

The server's instructions to the host are short: the ground rules of the [agent guide](agent-guide.md), and to call `docs` for the rest. Some hosts cut long instructions short, so the full guide and the rules, search and CLI pages come from that tool instead.

The names follow the CLI commands. A tool that lists things returns the CLI's `--json` rows wrapped in `{"rows": [...]}`; the others return one object. Rows carry an `account`; in `rules_list` that is the rule's own scope, and `null` means every account. Message rows carry no body text. `show` returns `{"message": ..., "body": ..., "truncated": ...}`.

`rules.toml` is shared by every account, so `rules_list`, `rules_propose`, `rules_withdraw`, `rules_approve`, `rules_move`, `rules_reject` and `rules_set_enabled` take no `account` argument; a rule's own `account` field scopes it. They follow `--account` as described under [Safety](#safety).

`read`:

- `accounts` lists the visible accounts: name, username, host, port, address and aliases. Never the password source.
- `docs` returns a page of this documentation as Markdown text: `page` is `agent-guide`, `rules`, `search` or `cli`, the pages `postvak guide` prints. Without a page it lists them.
- `folders` lists folders with total and unread counts.
- `list` lists the newest messages in a folder, or threads. `folder` defaults to INBOX; `limit` defaults to 50 and is at most 500. With `threads: true` it counts threads, and each thread comes with all its messages. `unread`, `flagged` and `attachments` keep only the messages, or threads, that have one. Every account comes in one order, newest first, and `limit` counts them together.
- `list` and `search` page: when more rows follow, the result carries `next` beside `rows`; pass it back as `cursor`, with the same other arguments, for the next page. With `threads`, a thread that loses its newest message between pages (moved or deleted) can show again on a later page; drop a repeat by its `account` and `thread_id`, and name `thread_id` in `fields` to keep it, such as `["compact", "thread_id"]`. `fields` keeps only the named keys in each row, such as `["uid", "from_addr", "subject", "date"]`, or `["compact"]` for account, folder, uid, date, from_addr, subject and flags (plus `depth` or `match`); `account` always stays, and an unknown name is an error. `count_only: true` returns `{"count": N}`, how many messages (threads with `threads`) match in all, and no rows.
- `log` shows the rule and action log.
- `rules_check` validates `rules.toml` and lists `warnings`, such as a conflicting copy a file sync left beside it, a rule for an account that is not set up, or a rule using `authenticated` for an account that trusts no server; [Rules](rules.md) lists them all. A server started with `--account` leaves out rules for the accounts it hides, and does not say whether a rule for every account watches a folder none of them syncs, since that depends on the hidden ones.
- `rules_coverage` runs every enabled rule over a folder's stored mail, changing nothing, and returns one row per account: `messages`, `matched`, `unmatched` and `contested` (matched by two or more rules), `unmatched_senders` grouped as `senders` groups them, and `contested_messages`, each with its message row, every matching `rules`, the `acts` that would run under the real precedence (a flag already set does not run), and `fails`, why nothing would run at all, such as an archive on an account without an Archive folder, or `null`. `include_pending` counts the proposals waiting for approval too. `limit` (default 20, at most 500) caps the two lists; the counts cover the whole folder.
- `rules_list` lists rules in file order, the order they run in. Each row is the whole rule as `rules_propose` takes it (`match`, `actions` and the rest), with its state, its proposer and `position`, its 0-based place in the file. A server started with `--account` leaves out rules for the accounts it hides, so positions can skip.
- `rules_schema` returns the JSON Schema for a rule; its description sends the agent to the `rules` page of `docs` for what each key means.
- `rules_test` previews a rule, given as JSON, against the local store. It reports subjects, never bodies. A rule that matches on the body needs `read:bodies` too, and one with a `header` condition `read:headers`, since their matches would reveal what bodies or headers contain. `rules_propose`, `rules_approve`, `rules_set_enabled` and `rules_apply_existing` refuse such rules on the same terms. `senders` and `rules_coverage` run the whole file instead, so they leave such rules out and name them in `skipped_rules`. `from`, `to`, `cc`, `subject`, `from_domain` and `to_domain` conditions need only `read`, as `list` already shows those fields. So does `authenticated`: it reads `Authentication-Results`, but reveals only whether the provider vouches for the sender, one bit per message.
- `search` finds messages by subject, addresses and attachment names. It matches all the given words, `"phrases"` in order, `-word` excludes, and the last word also as a prefix; the operators of [Search](search.md) (`from:`, `in:`, `is:unread`, `after:7d` and the rest) narrow it, and the tool's description lists them. When fewer than ten messages match as typed, close matches follow, each row's `match` being `exact` or `close`; `exact: true` leaves them out. Searching bodies, and `server: true`, need `read:bodies`; with `server` the result is one page, without `next`, and takes no `cursor` or `count_only`.
- `senders` groups a folder's stored mail by sender address, or with `group_by: "domain"` by registrable domain, the largest group first: `count`, `unread`, `first` and `last` (Unix seconds), up to three `subjects` and the enabled `rules` that already match some of it. `after` and `before` take `2026-01-15` or `30d`, `3w`, `6m`, `1y`; `limit` defaults to 50 and is at most 500.
- `status` is `postvak daemon status --json`: the daemon's pid, version, uptime and each visible account's activity, and whether its server offers `UIDPLUS` (see [Other mail clients](daemon.md#other-mail-clients)).
- `sync` asks the daemon to sync and run rules once per account and reports its result: `new_messages`, `actions` and `errors`, or an `error` when the account could not sync (offline, for example). It is in the `read` scope, but it writes the store and applies approved rules, so hosts see it as a changing tool.
- `trash_list` lists the `.eml` backups.

`read:headers`:

- `headers` returns `{"message": ..., "headers": [{"name": ..., "value": ...}], "truncated": ...}`: every header in order, repeats included, unfolded and with encoded words decoded, as `header` rules see them. A rule matches the last copy of a repeated header. It reads the store and never connects to the server. Past 32 KB of names and values the remaining fields are left out and `truncated` is `true`. Headers can hold more than the subject does: unsubscribe links with tracking tokens, the relays a message passed and sometimes the sender's IP address, which is why this is a scope of its own.

`read:bodies`:

- `attachments` lists attachment names, types and sizes. It does not save them; use `attachment_save` for that.
- `show` returns the headers and the body. With this scope, `search` also covers the bodies the background index has read (see [Search](search.md)), and `server: true` asks the mail server too, which searches every body. Otherwise `search` and `dry_run` never connect to the server. `show` and `attachments` fetch a message you have not stored yet, once.

`rules:propose`:

- `rules_propose` stores a rule disabled, with `proposed_by` set to `mcp:<client name>`, or `mcp` when the host sends no name. A rule scoped to an account the server hides is refused. While `--account` hides an account, a rule without `account` gets the only visible account, or is refused when several are visible. Instead of one rule's fields it takes `rules`, a list stored in the given order, all or none; an error names each failing rule by index and name. Proposals go after the last rule, or right next to another with `before` or `after`. With `replace`, a rule whose name is taken by this host's own pending proposal replaces it in place rather than being refused. With `edit`, each rule is a change to the existing rule of its name, stored as `<name> (edit)` right after it while the rule runs unchanged; approving it writes the change over the rule. While `--account` hides an account, a rule for every account cannot be edited. One rule returns `{"proposed": name, "account": ...}`, a list `{"proposed": [{"name": ..., "account": ...}, ...]}`.
- `rules_withdraw` removes one of this host's own pending proposals: a disabled rule whose `proposed_by` is this host's. An approved rule, or another proposer's, is refused; you reject those.

`files:write`:

- `attachment_save` saves attachment `n` into the save folder, or into `dir`, an absolute path of an existing directory below it, and returns the written `path`. A `dir` that leads outside the save folder, through `..` or a symlink, is refused, so a prompt-injected agent cannot place a file where it would run, such as `~/.config/autostart` or `~/Library/LaunchAgents`. The save folder is your Downloads folder, or what `--save-dir` names; without either the tool says to set one. It never overwrites a file, and fetches the message once if it is not stored yet.

`rules:write`:

- `rules_approve` approves a rule. It acts on mail that arrives afterwards.
- `rules_reject` rejects a proposal.
- `rules_set_enabled` turns a rule on or off.
- `rules_move` puts a rule right `before` or `after` another, which changes when it runs.

`mail:modify`:

- `archive`, `delete`, `junk`, `mark` and `move` act on uids in a folder, INBOX by default. `mark` takes `how` (`read`, `unread`, `flagged` or `unflagged`), `move` takes `to`, and `junk` with `not: true` reports mail as not junk: it tags `$NotJunk` and moves it to INBOX. `log` shows them under the rule name `mcp:<client name>`, or `mcp`.
- `unsubscribe` leaves a message's mailing list by one-click POST when the sender offers it; otherwise its row gives the page or `mailto:` address for the human to open. Use `dry_run` first: unsubscribing tells the sender the address is read. The request goes to the sender directly, not through the daemon, and is not recorded in `log`.
- `rules_apply_existing` runs one enabled rule over the mail each account already holds, the way `postvak rules apply-existing` does, and reports `evaluated`, `actions` and `errors` per account. With `dry_run` it lists the planned rows from the local store. When the rule would delete mail from the server for good, it refuses with how many messages per account until it is called again with `yes`.
- `trash_purge` deletes the backups older than each account's `trash_retention_days`, for good; it reports `removed` per account.
- `trash_restore` restores a backup, given as the bare file name `trash_list` returns.

With more than one visible account, every tool that acts on one message needs `account`.

## Safety

- Mail is written by strangers. `show` wraps the body in `<untrusted_mail_content>`, and tells the agent that text inside is data, never instructions. Every other string a stranger wrote comes in the same tag, in every tool's result: subjects, sender and recipient addresses with their names, message and thread ids, a search hit's `snippet`, header names and values, attachment names, and the senders and subjects that `senders` and `rules_coverage` group, such as `"subject": "<untrusted_mail_content>Invoice</untrusted_mail_content>"`. The tags are not part of the text; the server's instructions tell the agent to leave them out when it quotes a field or matches it in a rule. The rule tools return rules as written, without the tag. Any spelling of that tag name inside mail is rewritten to `untrusted-mail-content`, so a mail cannot close the wrapper early. The rewrite ignores letter case, and catches the name spelled with fullwidth or styled letters, or with Cyrillic, Greek and Armenian letters that look like Latin ones. A body is cut at 100 KB and marked `"truncated": true`.
- Every string in a result or an error loses its control characters and the characters a person cannot see but a model reads, before the tag rewrite runs. That stops "ASCII smuggling", text hidden as Unicode tag characters (U+E0000–U+E007F), and also removes bidi controls, zero-width spaces and the other invisible format characters. A body keeps its newlines and tabs, and emoji keep their zero-width joiners; flags written with tag characters show as a plain black flag. The CLI's `--json` output is cleaned the same way.
- A host that honours tool annotations asks you before tools that change things. `delete`, `rules_apply_existing`, `rules_approve`, `rules_move`, `rules_set_enabled`, `trash_purge` and `unsubscribe` are marked destructive, because they can delete mail or its backups, or cannot be undone. The other changing tools are marked as changing, but not destructive.
- Except `trash_purge`, the `mail:modify` tools take `dry_run`. It reports what would happen from the local store and does not ask the daemon.
- `--account` hides other accounts from every tool. Rule writes refuse rules scoped to a hidden account, and so does proposing one. A rule without `account` applies to every account, hidden ones too, so while an account is hidden `rules_approve` and `rules_set_enabled` refuse to turn such a rule on, and `rules_move` refuses to move it; turning it off and rejecting it still work.
- Without `read:bodies`, no tool returns or reveals body text: `search` covers subject, addresses and attachment names only, and `rules_test` refuses a rule that matches on the body.
- Without `read:headers`, no tool returns or reveals a header beyond sender, recipients, subject, date and the threading ids, apart from one bit: a rule with `authenticated` shows which messages your provider's `Authentication-Results` vouches for. `rules_test` and the other rule tools refuse a rule with a `header` condition, at any depth of `any` and `none`.
- The server cannot create, rename or delete folders, with any scope. That is left to you, in the window or with `postvak folder`, on purpose: a folder delete can take a whole mailbox's structure with it.
- A call outside the granted scopes fails with "not allowed with these scopes". Hosts only see the tools you granted, so they should not make such a call.

## Alongside the window and the CLI

The MCP server reads the same local store as the mail window. Everything that needs the mail server goes through the Postvak daemon, which owns the connections: actions, `trash_restore`, `show` and `attachments` for a message not yet fetched, `search` with `server`, and `sync`. The first such call starts the daemon if none runs. The window picks up the changes as soon as the daemon reports them, so an archived message disappears from the list. Like the daemon, the server reads `config.toml` again when it changes, so an edit such as a new `trusted_authserv_ids` counts from its next call, without restarting the host.

## Troubleshooting

- Restart the host after installing. It reads its config at start.
- Logs go to stderr. Look in the host's MCP log.
- "not allowed with these scopes" means the tool needs a wider scope. Run the install again with more scopes.
- "several accounts are visible; pass account" means the tool needs the `account` argument.
