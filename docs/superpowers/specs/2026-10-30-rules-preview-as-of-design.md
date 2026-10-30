# Previewing rules as of another time

`rules test` and `rules coverage` evaluate against the current time, so a rule with `older_than` or `younger_than` could only be checked for what it does now. An agent writing a 15-minute hold saw the message held and could not see the move that takes over afterwards, short of waiting (#470).

## Core

- `time::as_of(text, now)` reads the time a preview evaluates as of: a span ahead of `now` in `older_than`'s syntax (`15min`, `1d`, `6mo`; months follow the calendar), a local date (its midnight, as search's `after:` reads one), a local `YYYY-MM-DDTHH:MM` (seconds optional), or an RFC 3339 timestamp. Past times are allowed. A span too far ahead for the calendar, and a local time a clock change skips, are refused.
- Only the evaluation's `now` changes. The mail is what is stored now, and its read, flagged and tags stay as they are; only `older_than` and `younger_than` read `now`. Nothing is applied.

## CLI and MCP

- `postvak rules test --as-of WHEN` and `postvak rules coverage --as-of WHEN`. With `--as-of`, stderr starts with `as of YYYY-MM-DD HH:MM`; `rules coverage --json` rows carry `as_of`, Unix seconds.
- `rules_test` and `rules_coverage` take `as_of`. Their results always say the time used: `as_of` at the top of `rules_test`'s result and in each `rules_coverage` row, Unix seconds like `internaldate`.
- The help text (`help::AS_OF`) is shared by both.

## GUI

Not in the window. This is a testing aid for whoever writes rules, agents and scripts first; a time control in the proposal's dry run or the coverage view is a separate change.
