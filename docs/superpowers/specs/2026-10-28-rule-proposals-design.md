# Rule proposals: batches, read-back and withdrawal — design

Status: built, 2026-10-28. Issues #349 and #350. Extends the proposal flow of `2026-10-06-postvak-core-design.md` and `2026-10-07-postvak-mcp-design.md`.

## Goal

An agent migrating 52 filters from another mail app needed 52 `rules_propose` calls, could not see what it had stored or where, and could not take back its own mistake. Proposals stay "you propose, a human approves": nothing here enables a rule or changes one a human approved.

## Decisions

- **Batches.** `rules_propose` takes one rule's fields, as before, or `{"rules": [...]}`. `postvak rules propose` reads one rule, a JSON array or `{"rules": [...]}` on stdin. The whole batch is checked before anything is written: each rule on its own (the `rules check` validation), names repeated inside the batch, names already in the file. Every failing rule is named in one error by its 0-based index and name, `rule 3 'x': ...`, and nothing is stored. A lone rule keeps today's wording, `rule 'x': ...`. The rules are appended in the given order, so a batch keeps its source's precedence.
- **One input schema.** Hosts such as the Anthropic API refuse a tool input schema with a top-level `anyOf`, so `rules_propose`'s schema is one object: the rule's properties plus `rules`, `before`, `after` and `replace`, none required. `rules::edit::read_proposals` reads either form and refuses unknown keys and `before` with `after`. Its result keeps today's shape for one rule's fields (`{"proposed": name, "account": ...}`) and lists `{"name", "account"}` objects for `rules`.
- **File order.** `rules list --json` and `rules_list` return the rules in file order, the order they run in, each as its whole definition (what `rules propose` takes) plus `position`, its 0-based index in `rules.toml`. Under `--account`, rules of hidden accounts are left out and positions skip; they are file positions, never renumbered. The text `rules list` keeps its three columns.
- **The reverse-order bug.** `rule_table` built each new table by parsing a one-rule document, and the table kept that document's position. toml_edit writes tables sorted by position, so every appended rule sorted above the earlier proposals and sequential proposals came out reversed after the first. The table's position is now cleared, so it is written where it sits in the array. This also fixes rules the window adds.
- **Placement.** `--before NAME` / `--after NAME` (MCP `before` / `after`) put the whole batch, in order, right next to a rule already in the file; the anchor may not be one of the batch's own names. Without them proposals go after the last rule. Before the first rule, the file's leading comments (up to their last blank line) stay at the top.
- **Withdraw and replace.** `postvak rules withdraw NAME [--by WHO]` and `rules_withdraw` remove a rule only when it is disabled and its `proposed_by` is the caller's own: `cli:WHO` (or `cli` without `--by`, as `propose` records it) on the CLI, `mcp:<client>` on MCP. `--replace` on propose (MCP `replace`) lets a batch rule take the name of the caller's own pending proposal: it replaces that rule in place, keeping its comments, or moves it to `before`/`after` when given. The swap is one locked edit of `rules.toml`. Withdraw is in the `rules:propose` scope, as it can only undo what that scope added. Approving a rule removes its `proposed_by`, so a rule a human approved and later turned off is not a pending proposal again.
- **Writes** go through `rules/edit.rs` only (`propose_all`, `withdraw`), under the rules lock, and the edited file must validate as a whole.

## Surfaces

CLI and MCP get everything above. **Exception: the GUI gets no withdraw and no batch or placement.** A person in the window already approves, rejects, edits, reorders by editing and removes any rule; withdraw exists so an agent can undo its own proposal without a human, and batches and placement exist to save an agent round trips. The window shows the rules in file order, so a placed or batched proposal appears where it was stored.

## Tests

- `rules::edit`: sequential proposals keep their order (the regression); a batch is appended in order and disabled; a batch with bad rules stores nothing and names each by index; a lone rule keeps the old wording; before/after placement and the file head; replace only the caller's own pending proposal, in place or moved; withdraw only the caller's own pending proposal; `read_proposals` forms and refusals.
- `output`: a rule row carries the whole rule and its position.
- MCP: batch in order, all or none, placed; `rules_list` rows whole with positions; withdraw and replace refuse others' and approved rules; the input schema is a plain object with every key and none required; tool lists and annotations include `rules_withdraw`.
- CLI: a batch from an array and from `{"rules": [...]}` with `--before`, read back with `rules list --json`, refused all or none, replaced, and withdrawn only with the right `--by`.
