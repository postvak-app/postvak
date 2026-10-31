# Previewing rules on mail as if it just arrived

`--as-of` moves the clock forward, but a hold such as `match.younger_than = "15min"` only matches mail younger than 15 minutes, so unless some stored mail is that new, `rules test` and `rules coverage` show nothing and an edited hold goes live untested. The same goes for `after` waits (#484).

## Core

- `rules::engine::Context` has `arrived_at`: when set, `older_than`, `younger_than` and `after` read it as each message's arrival instead of its `internaldate`. Only previews set it; sync never does.
- The arrival is the current time. Only relative spans read a message's arrival, so another arrival time would say nothing `as_of` cannot: `as_of` and the arrival are read against the same now, so `as_of 15min` is exactly 15 minutes after the arrival.
- As with `as_of`, the mail and its read, flagged and tags stay as they are, rows show the stored date, and nothing is applied.

## CLI and MCP

- `postvak rules test --as-arrival` and `postvak rules coverage --as-arrival`; stderr then starts with `as if every message arrived at YYYY-MM-DD HH:MM`. Not with `rules test --list`.
- `rules_test` and `rules_coverage` take `as_arrival: true`; `rules_test` refuses it with `list`.
- The help text (`help::AS_ARRIVAL`) is shared by both.

## GUI

Not in the window, as for `as_of`.
