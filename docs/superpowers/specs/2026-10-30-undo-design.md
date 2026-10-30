# Undo

Status: draft, 2026-10-30. Phase 4 of the GUI redesign toward the Claude Design canvas (https://claude.ai/artifact/FZbXdN5FXZ6RdxL9jP6Te7). The canvas shows a toast after an action, "Archived 1 message · Undo z"; nothing in Postvak can reverse an archive, move or mark today except doing the opposite by hand. Not yet reviewed with the owner: the calls marked as such below are the author's.

Surfaces: CLI, MCP and GUI together. The toast and its key are the GUI's; `postvak undo` and the MCP `undo` tool reverse the same things through the same library code.

## Goals

- One keypress puts back what the last action did: the messages return to the folder they left, and read, flagged and tag changes are reversed.
- An agent or a script can reverse an action it took, or one a rule took, by the action's number in the log.
- Undo never makes things worse: what it cannot put back exactly, it leaves alone and says so.

## Non-goals

- Redo. Undoing an undo is not offered; doing the action again is.
- Undoing rule changes (approve, reject, edit, reorder) or folder operations (create, rename, delete). `rules.toml` and the folder tree have their own history in the log and in Backups.
- Undoing what another mail client or the server did.
- Keeping flags and the arrival date on a message brought back from a backup (see Deletes).

## Anti-goals

- Undo must never move, flag or delete a message the action did not touch. Each message is found again by what the log recorded, and a message that cannot be found unambiguously is skipped, not guessed.
- Undo must never re-run the rules on what it put back: a message moved back to the Inbox must not be filed again by the rule that filed it (`rule_moves` already guarantees this for a rule's move).
- Undo must not lose data: a delete's undo is a restore of its backup, and the backup is only removed once the restore succeeded, as `Trash::restore` does now.

## What one action is

- An **action** is one `Command::Apply` as the daemon runs it: one account, one folder, one `Action`, the UIDs chosen, and `by` (`gui`, `cli`, `mcp:<client>`, or a rule's name). A GUI selection across accounts sends one Apply per account and folder; the GUI's undo takes back all of them together.
- Every `rule_log` row an Apply writes gets the same action id (new column, a new migration). Rule actions get one per rule per sync pass and folder. `postvak log`, MCP `log` and the GUI's Activity view show the id.
- The log records, per row, what undo needs and does not have today: the message's internaldate (to find a moved copy by Message-ID and internaldate, as `rule_moves` does) and, for an archive or a move to Trash, the folder it went to (the archive and Trash folders can change later).
- Rows an undo writes carry the id of the action they reverse (`undoes`). An action with an undo row is undone and is not offered again.

## What undo does, per logged row

- **`move:<folder>`, `archive`, `junk`, `not_junk`, `trash` (a move to the Trash folder):** move the message from where it went back to the row's `folder`. It is found in the destination by Message-ID and internaldate; with no Message-ID, or more than one match, it is skipped.
- **`read`, `read:false`, `flagged`, `flagged:false`, `tag:<keyword>`:** set the opposite. `notify`, `notify:false` and `stop` change no mail and have nothing to undo. A flag the action did not change is not logged (the batch skips messages already in that state), so reversing every logged row restores exactly the state before.
- **delete (expunged after a backup):** restore the backup with `Trash::restore`. The restored message is a new one: a new UID, no flags, today's arrival date; the undo result says so. Once the backup is purged (`trash_retention_days`), it cannot be undone.
- **A message changed since:** if a message is no longer where the action left it (moved again, deleted, or its flag changed back), its row is skipped and reported as "changed since", and the rest are undone.
- Undo goes through the daemon as a new `Command::Undo { action }` per account, so it has the account's IMAP connection and syncs the folders it touched, like Apply. The library function lives in `actions` beside `run`.

## GUI

- After each direct action from the window, a toast at the bottom of the list pane, as on the canvas: "Archived 1 message · Undo" with the `z` keycap. It stays about 8 seconds, or until the next action replaces it; hovering keeps it.
- `z` and Ctrl+Z (Cmd+Z on macOS) undo the newest action taken in this window that is not undone yet, also after its toast went. Both keys are free today. Keys help lists them.
- While the undo runs, the toast reads "Undoing…"; then "Moved 1 message back to Inbox", or what was skipped and why ("1 message changed since and was left alone").
- The rows come back the way they left: the GUI's optimistic edits (`Optimistic::Hidden`, `Seen`, `Flagged`) apply in reverse until the daemon's `ActionDone` arrives.
- The Activity view gets an Undo link on each action row that can still be undone, for rule actions too.
- The reader's **Move back** stays as it is: a move of the open message. It is not undo.

## CLI and MCP

- `postvak undo [ID] [--account A] [--dry-run] [--json]`. Without an id it undoes the newest direct action of any front end that is not undone yet, never a rule's; with one, that action, a rule's included. The output says per message what was put back or skipped and why.
- MCP `undo { account, action, dry_run }` with scope `mail:modify`, the same scope as the actions it reverses. The tool requires an id: an agent names what it reverses. The ground rule to preview first applies.
- Both share their JSON rows through `output`, and their help texts through `help`.

## A bug to fix first

`apply.rs` records a direct move in `rule_moves` when `planned.rule != RULE_NAME` (`"cli"`), so moves from the GUI (`"gui"`) and MCP (`"mcp…"`) count as a rule's move: a rule then never files that message again if it comes back. It should test `!rules::is_actor(&planned.rule)`, as `Store::rule_actions` does. Fix it in its own PR with a regression test before undo builds on `rule_moves`.

## Calls made (the author's, for the owner to check)

- The default `postvak undo` takes back a direct action, not a rule's: rules run on every sync, and "the last thing" is what you did.
- No time limit other than a delete's backup expiring. The changed-since check is what keeps an old undo safe.
- One toast at a time; a new action replaces it rather than stacking.
- A delete's undo restores without the old flags rather than storing flags in the backup; widening the backup format is not worth it for this.

## Left to the builder

- The migration's shape: columns on `rule_log` or a table of actions; how a rule pass gets its id.
- Whether to read COPYUID (UIDPLUS) to record the new UID of a moved message, falling back to Message-ID and internaldate; it would make the lookup exact on servers that send it.
- The toast's component in `ui/`, its timing, and how it sits over the list on a narrow window.
- Snapshot coverage: the toast in both themes, and the toast after a partial undo.
