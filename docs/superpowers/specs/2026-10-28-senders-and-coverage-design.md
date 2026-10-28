# Sender statistics and rules coverage — design

Status: decided in the issues (#352, #354) and by the workflow that built them; no open questions.

## Goal

An agent that suggests rules, and the human who approves them, need two answers before they write or enable one:

- **Who sends the mail in a folder** (#352). Today that takes `list` (capped at 500), paging with `search 'before:…'`, and grouping outside Postvak: for a 1,757-message INBOX, four calls and about 1.2 MB of JSON.
- **What the rules file does to that folder as a whole** (#354): which mail no rule matches, and which mail several rules match, with the one that wins under the real precedence. Previewing rules one by one hides those conflicts: a broad rule early in the file quietly takes mail that a later sender rule was written for.

Both are analysis over the local store. Nothing connects to the server, changes mail, writes `rule_log` or touches a rule's clock.

## Surfaces

| Surface | Sender statistics | Rules coverage |
|---|---|---|
| CLI | `postvak senders --folder F [--account A] [--group-by address\|domain] [--limit N] [--after D] [--before D] [--json]` | `postvak rules coverage --folder F [--account A] [--include-pending] [--group-by address\|domain] [--limit N] [--json]` |
| MCP | `senders` (scope `read`) | `rules_coverage` (scope `read`) |
| GUI | none | none |

**GUI exception.** Both are tools for agents and for the command line, where an agent or a person reads a ranked table to decide what rule to write. The window's Rules view shows rules one at a time and has no place for a folder-wide table; a coverage view there is a feature of its own, not part of this change.

The help texts are `help::SENDERS` and `help::RULES_COVERAGE`, and the JSON rows `output::sender` and `output::coverage`, so the CLI's `--json` and the MCP results are the same rows.

## Sender statistics

`senders::in_folder` reads the folder's stored messages in the date range, newest first, and groups them:

- **Key.** `group_by: address` (default) is the first address of `From`, lowercased. `domain` is the registrable domain of that address, from the public suffix list (`psl`): `news@mail.shop.co.uk` and `orders@shop.co.uk` are both `shop.co.uk`. A host the list does not know, such as `localhost`, is its own domain. Mail without a sender address forms a group with an empty `sender`.
- **Per group:** `count`, `unread`, `first` and `last` (arrival times as Unix seconds), up to three distinct `subjects`, newest first, and `rules`: the enabled rules that match at least one of the group's messages, in `rules.toml` order.
- **Order.** The largest group first; equal counts, the most recent first; then by key. `limit` (default 50; at most 500 over MCP) cuts the list per account.
- **Dates.** `after` and `before` read like the search operators: `2026-01-15` is local midnight, `7d`, `3w`, `6m` and `1y` count back from now; `after` is inclusive, `before` exclusive. A value that does not parse is an error here, not a word as in search.
- **A rule's match** is `engine::rule_matches`: the same checks `evaluate` makes before it plans a rule's actions (enabled, account, folder, a stored body when the rule tests one, the restore keyword, the conditions), evaluated as `rules test` does, over mail older than the rule. Whether an earlier rule's `stop` or delete would keep it from acting is coverage's question, not this one's.

The grouping runs in Rust over the folder's rows rather than in SQL: neither the bare address inside `From` nor the registrable domain can be computed in SQLite, and the rules have to see each message anyway. The store reads only the date range (`Store::messages_between`), and reads body text only when a rule that could match the folder tests the body.

## Rules coverage

`rules::coverage::in_folder` runs the rules over every stored message of the folder, without side effects:

- **Rules.** The enabled rules, in file order. `include_pending` adds the proposals still waiting for a human (disabled, with `proposed_by`), each where it stands in the file; a rule disabled by hand stays out.
- **Counts.** `messages`; `matched`, where at least one rule matches; `unmatched`; `contested`, where two or more match (also counted in `matched`).
- **Unmatched senders.** The unmatched mail grouped as `senders` groups it (`group_by`, default `address`), the largest group first; their `rules` are empty by definition.
- **Contested messages**, newest first: the message row (never body text), `rules` with every rule that matches, and `acts`, what would run: the plan from `apply::plan_for` (so `stop`, a delete's stop, and mail a rule already moved out of the folder count as they do in a sync), then `apply`'s precedence: a delete alone; otherwise every flag change and only the first move or archive. Each act is `{rule, action}` with the action's label (`move:Deliveries`, `flag`).
- `limit` (default 20; at most 500 over MCP) cuts the unmatched senders and the contested messages per account; the counts always cover the whole folder.

Coverage evaluates as `rules apply-existing --dry-run` would (`Mode::ApplyExisting`): over all stored mail, including mail older than a rule, which a sync would leave alone.

## MCP and scopes

Both tools are read-only and in `read`. They return `{"rows": [...], "skipped_rules": [...]}`.

A rule with a `header` condition needs `read:headers`, and one on the body `read:bodies`, before the agent may learn which mail it matches (`refuse_unseen`). The rule tools that take one rule refuse it; these tools run the whole file, so instead of refusing they leave such rules out and name them in `skipped_rules`. The coverage then reads as if those rules did not exist, which the agent is told. Rules scoped to an account the server hides are never named.

## Tests

- `senders`: keys by address and registrable domain; counts, unread, dates, distinct subjects and matching rules per group; disabled rules never match.
- `rules::coverage`: counts; a delete wins and stops later rules; an early move takes the mail a later rule was written for while its flag still runs; `stop` leaves later rules matching but not acting; pending proposals count only when asked.
- `Store::messages_between`: the range bounds and that body text is read only when asked.
- MCP: both tools, `skipped_rules` without `read:headers`, the domain grouping, a bad date, `limit`, the tool list per scope.
- CLI: `senders` and `rules coverage`, text and `--json`.
