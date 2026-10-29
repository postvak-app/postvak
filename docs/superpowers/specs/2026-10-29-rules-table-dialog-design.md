# The rules as a table, edited in a dialog

Issue #401. The Rules view edited a rule in a pane beside the list, which took half the view and kept the rules from showing at a glance. A rule is now edited in a dialog, and the view shows the rules as a table. GUI only: the CLI and MCP already list rules in full and edit them through `rules.toml` or their own commands.

## The table

- Proposals stay first, as full-width cards with their TOML, Approve, Reject and Preview on cached mail. The table holds only the rules in `rules.toml` that are not waiting for approval.
- One row per rule: # (its place in the order rules run, from 1), a switch, Name (with "proposed by" when an agent wrote it), Account, Folder, Conditions and Actions, each cut short when it does not fit.
- A click on a heading sorts by that column, and a second click reverses it. The sort is `SheetSort`, which Activity and Backups use too, so leaving the view drops it. # sorts by run order rather than as text, so the table can always go back to the order the rules run in. Sorting only changes how the table is shown, never `rules.toml`.
- The table is drawn by hand rather than with `StandardTableView`, whose cells hold only text and so cannot hold the switch.
- A click on a row, anywhere but its switch, opens the rule.
- A Filter rules field in the title row narrows the proposals and the table to those with every whitespace-separated word of it, ignoring case, in what their card or row shows (name, proposed by, account, folder, conditions, actions; a proposal's one-line summary). Like the sort, it only changes what is shown and leaving the view clears it. A row keeps its #, the header keeps counting every rule and proposal, and a filter that leaves no rule says so. GUI only: `rules list` and `rules_list` show the whole file, which is short enough to read or grep.

## The dialog

- `Dialog::Rule` is open while there is a draft and the Rules view shows. It sits under Apply's confirmation and the unsaved-changes question, so both show over it.
- Esc or the X closes it. With unsaved edits, closing asks Save, Discard or Cancel, as opening another rule already did: `UiAction::Close(Dialog::Rule)` is held in `leave_draft` like the other ways of leaving a draft.
- Save writes the rule and closes the dialog. A save that fails keeps it open with the reason.
- Apply to existing mail moved from the row to the dialog of a saved rule. It is enabled only when the rule is on and has no unsaved edits, so what runs is what `rules.toml` says.
