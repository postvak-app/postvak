# Rule edit proposals — design

Status: built, 2026-10-29. Extends `2026-10-28-rule-proposals-design.md`.

## Goal

An agent can propose new rules and change its own pending ones, but cannot touch a rule a human approved: fixing a rule's match or adding an action means asking the human to edit rules.toml. An agent proposes a change to any rule; the rule keeps working as it is until a human approves the change, which then takes its place.

## Decisions

- **Proposing.** `rules_propose` and `postvak rules propose` take `edit: true` beside `replace`, `before` and `after`, for the whole call. Each rule of the call is written as it should read once approved, under the name of an existing rule that is not a pending proposal. A name that is not there, or is a pending proposal, is refused (`rule 'x': no rule to edit; propose it as a new rule`, `rule 'x': is a pending proposal; propose it again with replace`). `edit` with `before` or `after` is refused: an edit keeps the rule's place. Renaming through an edit is not supported; the edited rule keeps its name.
- **Storage.** The edit is stored in rules.toml as a disabled rule right after the one it changes, named `<name> (edit)`, with `proposed_by` and a new key `replaces = "<name>"`. `Rule` gains `replaces: Option<String>` (schema: "The rule this proposal changes once approved; set by `rules propose`"). A rule has at most one pending edit: a second edit of the same rule is refused unless it is the caller's own and `replace` is set, which swaps it in place as for new proposals.
- **What runs.** Nothing changes for sync: the edit is disabled and the rule it replaces keeps running. `rules coverage` and `senders` with pending proposals included (`include_pending`) run the edit in place of the rule it replaces, not beside it. `rules test <name> (edit)` previews the edit as it previews any proposal.
- **Approving.** `rules approve "<name> (edit)"` writes the edit over the rule it replaces in one locked edit: the same merge `save_rule` does, so the rule's comments, place and unchanged lines stay, then removes the proposal. The rule keeps its own `enabled`: an edit changes what a rule does, not whether it runs. When the rule is enabled its clock restarts in every account, as on any approval, so a broader match does not reach mail from before the approval. Approval is refused when the rule named by `replaces` is gone (renamed or removed since): `rule 'x (edit)': the rule it edits, 'x', is no longer in rules.toml; reject it`.
- **Rejecting and withdrawing** work as today and remove only the proposal.
- **Warnings.** An edit whose `replaces` names no rule is a `rules::warnings` warning, not an error, so removing a rule in the window never makes the file invalid.
- **Listing.** `rules list` and `rules_list` rows carry `replaces` (null for anything else), from `output::rule`; the CLI's table shows `edits <name>` in the proposed-by column.
- **MCP scope and sight.** Proposing an edit is `rules:propose`. The rule being edited must be visible to the server, as for enabling (`ensure_rule_visible`): a server narrowed with `--account` cannot edit a rule for every account, since the change would reach accounts it does not see. The edit's `account`, when given, must be visible too, and a narrowed server fills in its one account as for new proposals. `rules_propose` answers with the stored name, `<name> (edit)`, and `rules_approve` of an edit with the rule it changed.
- **GUI.** The proposal card of an edit shows the rule's TOML as a line diff against the rule it replaces: `-` for removed lines, `+` for added, a space for unchanged, in the `marks` column a new proposal already fills with `+`. A new rule's block keeps its added tint; an edit's block is outlined instead, its marks in the text colour. Its note names the rule it changes. Its chip reads "edit proposed by …". Approve and Reject work as for any proposal. The diff is a small longest-common-subsequence over lines in `gui/rules.rs`; rule tables are a few lines, so no dependency.

## Out of scope

Proposing to remove a rule; renaming through an edit; edits that move a rule.

## Tests

- `rules::edit`: `edit` stores `<name> (edit)` disabled after the rule with `replaces`, the rule untouched; refused for a missing rule, a pending proposal, with `before`/`after`, and a second edit unless the caller's own with `replace`; approve merges in place keeping comments and the rule's `enabled`, removes the proposal and restarts the clock; approve refuses an orphaned edit; reject and withdraw remove only the proposal.
- `rules::coverage`: with pending proposals an edit runs in its rule's place.
- `rules::warnings`: an orphaned edit warns.
- `read_proposals`: `edit` parsed, non-boolean and `edit` with placement refused.
- MCP backend: an edit of a hidden account's rule reads as absent.
- GUI: the edit card's diff marks; approving an edit updates the rule's row; snapshot of the card.
