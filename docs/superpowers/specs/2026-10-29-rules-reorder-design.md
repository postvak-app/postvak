# Changing the order rules run in

Rules run in file order: the first matching `move` and `notify` win, and `stop` skips the rules after it. Changing that order meant editing `rules.toml` by hand. Now it ships on every surface.

## Core

- `rules::edit::move_rule(path, name, place)` puts a rule right before or after another, or at the end (`Place`, the same type proposals use). It refuses a missing rule or anchor, and a rule placed next to itself.
- The comments right above the rule move with it. Comments a blank line above them, such as the file's heading, stay where they are. Nothing else in the file changes, and the edited file must validate as a whole, as every rules.toml edit does.

## CLI and MCP

- `postvak rules move NAME --before OTHER | --after OTHER`; one of the two is required.
- `rules_move { name, before | after }`, scope `rules:write`, marked destructive: moving a `delete` rule ahead of a `stop` changes what gets deleted. A server narrowed with `--account` refuses to move a rule for every account, as it refuses to turn one on, since its new place changes what it does to hidden accounts. A rule or anchor of a hidden account reads as absent.

## GUI

- A row of the Rules table can be dragged onto another row while the table shows run order: unsorted, or sorted by # ascending. Sorted by anything else, a drop has no clear place, so dragging is off.
- Dropped on the upper half of a row, the rule goes before it; on the lower half, after it. A line above or below the row shows where. A drop that would leave the rule where it is does nothing.
- The drag carries `rule:N`, so a message row's number dropped on the table, or a rule row dropped on a folder, is refused.
- Proposals are cards above the table and are not dragged; a moved rule keeps its place relative to them as the anchor rule has it.
