# Rules on the weekday and time of arrival

Rules could not depend on when mail arrives in the week, so "no notifications for work mail in the evening" or "move newsletters to Later during working hours" could not be written (#502).

## Conditions

- `match.arrived_days`: local weekdays, such as `mon-fri`, `sat,sun` or `fri-mon`; a day by its first three letters or in full, any case. A range wraps past Sunday.
- `match.arrived_hours`: a local window `HH:MM-HH:MM`, the start included and the end not; an end before the start wraps past midnight. A window that starts where it ends is refused.
- Two flat keys rather than one `arrived = { days, hours }` table, so each is a plain value like `older_than` and the window's rule editor shows each as an ordinary row.
- With both set, each is checked on its own against the arrival's local weekday and clock: `fri` with `22:00-07:00` matches Friday 23:00 and Friday 03:00, not Saturday 03:00.
- `none = [{ arrived_hours = … }]` gives the inverse.

## Arrival

The arrival is the message's `internaldate`, the same moment `older_than` reads, so the same message gets the same outcome on every sync. A preview with `--as-arrival` / `as_arrival` reads its arrival instead. Local time is the time zone of the machine the daemon runs on.

## Surfaces

rules.toml is shared, so the CLI and MCP take the keys through the schema. The window's rule editor offers them as "arrived on" and "arrived between".
