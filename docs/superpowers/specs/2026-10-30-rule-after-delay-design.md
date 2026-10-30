# A delay on a rule's actions (`actions.after`)

Issue #469. A rule that should act on mail only after the phone saw it, or once it is read, took two rules: a hold rule near the top (`younger_than` + `stop`) that repeats the senders of the rule it holds back, and the rule itself. `actions.after` puts the delay on the rule it belongs to.

Surfaces: rules.toml and its schema, so the CLI, MCP and GUI together. `rules test`, the MCP `rules_test`, `rules coverage` and the MCP `rules_coverage` show a waiting action and when it runs; the GUI rule editor offers After as an action with a value.

## Syntax

```toml
actions.after = "15min"            # a span after arrival, as older_than takes
actions.after = "read"             # until the message is read
actions.after = ["read", "2d"]     # whichever comes first
```

A value is `read` (any case) or a span `time::Span` parses; anything else is refused with the span error plus ", or read". A rule whose only other actions are `notify`, `stop` or none is refused: there is nothing to wait with. In memory each value is one `Action::After(String)` in the rule's action list, as each tag is one `Action::Tag`, so the editor and `rule_toml` need nothing of their own. In the `Actions` table `after` is the last field: serde reads a list into a struct by position, and the old `actions = ["delete"]` syntax must keep failing on a bool.

## Semantics

Rules run over every stored message on every pass, so a delay is a check, not a timer: on each pass a rule with `after` is either due (any value has come: the message carries `\Seen`, as the rules above left its flags, or it arrived at least that span ago, the same cutoff as `older_than`) or waiting.

While a matching rule waits:

- Its `stop` holds at once: the rules below do not run for the message.
- Its `notify` / `notify = false` already count, since a notification is about arrival.
- Its other actions go to `Plan::waiting` with a `Due` (the earliest span's end, and whether reading ends the wait). Nothing runs them; the next pass looks again.
- A waiting move is no move, so the message still notifies by default.
- A waiting delete does not stop the rules below, and its flag changes are not carried on to them, since neither has happened.

Once due, the rule acts exactly as a rule without `after`. The first full sync after the due time runs it, so up to `sync_interval_secs` late, as with `older_than`. Mail read on another device counts once its `\Seen` is synced.

## Previews

`actions::planned` returns waiting actions with `Planned::waits` set; the CLI prints `(waits: runs at 2026-10-30 15:04 or once read)` after the action, and the JSON row has `"waits"`. `expunges` never counts a waiting delete, so `apply-existing` does not warn about mail it will not delete yet. A coverage `Act` has `waits` for an action that waits; contested messages list them after what runs now.

## Non-goals

- A timer that wakes the daemon at the due time.
- `after` on direct actions from the GUI, CLI or MCP; it is a rule action only, and `apply` ignores it.
