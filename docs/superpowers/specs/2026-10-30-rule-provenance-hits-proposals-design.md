# Rule provenance, hit counts and the proposal card

Phase 3 of the GUI redesign toward the Claude Design canvas (https://claude.ai/artifact/FZbXdN5FXZ6RdxL9jP6Te7, boards Reader, Rules and Main). Postvak's promise is mail sorted by rules you can read; these three pieces let you see what a rule did, whether it still earns its place, and what an agent proposes before you approve it. Everything here reads what Postvak already stores, chiefly the `rule_log` table every rule action is written to; nothing changes how rules run.

Surfaces: hit counts ship on the CLI, MCP and GUI together. The provenance strip and the proposal card and banner are GUI only: they present the action log (`postvak log`, MCP `log`) and the proposals (`rules list`, `rules_list`, `rules_test`) that the CLI and MCP already show in full.

## Goals

- Opening a message a rule acted on says which rule, what it did and when, and lets you fix that rule or put the message back, without leaving the reader.
- The Rules table shows how often each rule acts and when it last did, so a rule that stopped matching stands out.
- A waiting proposal reads like the canvas: what it would do in a sentence, how much mail it would touch with examples, and one obvious Approve; and the inbox says that one is waiting.

## Non-goals

- A general undo (Phase 4). Move back below is an ordinary move of one message, not undo.
- Strips for actions you took yourself, from the GUI, CLI or MCP.
- Editing a rule inside the strip or the card; editing stays in the rule dialog.
- Storing when a rule was proposed. The canvas shows "3 min ago" on a proposal; Postvak keeps no such time, and adding `proposed_at` to `rules.toml` is not worth a schema change now. The card shows who proposed it, without an age.

## Anti-goals

- The strip must never name a rule for something you did by hand. Direct actions are logged in `rule_log` too, under the name of who took them (`actions::RULE_NAME` is `"cli"`; the daemon's `Command::Apply` carries `by`, such as `"gui"`). Telling the two apart reliably is a requirement, not a nicety.
- Opening the Rules view or the inbox must not stall the window while dry runs or counts are computed.
- Nothing here deletes, moves or approves anything on its own.

## The provenance strip (reader)

- Shown under the sender line, above the attachments, on a message a rule acted on; absent otherwise. Drawn as the canvas's Reader board: a 36px rounded strip on `pane.panel`, the rules glyph in `pane.accent`, the sentence in `pane.secondary` with the folder and the rule name in `pane.text` (the rule name in the mono font), and the actions on the right as links.
- The sentence is the last rule action on this message: "Moved to work / Travel by rule travel at 07:02". Other actions read the same way in plain words (archived, flagged, marked read, tagged …, moved to Junk). The time is written as the list writes dates ("07:02", "yesterday", "14 Oct"). When more than one rule action was logged for the message, the sentence ends with "and N more", which opens a small popup listing each (rule, action, time), newest first.
- The message is found in the log by its Message-ID, because a rule's move gives it a new UID in another folder. A message without a Message-ID gets no strip.
- **Edit rule** opens that rule in the rule dialog, as Rule from sender opens a draft. When the rule is no longer in `rules.toml`, the strip says "by rule travel (removed)" and offers no Edit rule.
- **Move back** moves the message to the folder the rule took it from: `rule_log.folder` holds the folder the message was in when the rule acted. Shown only when the last action was a move or archive and the message is not already back there. It sends the same `Command::Apply` with `Action::Move` that dragging to a folder sends.
- **Why?** re-evaluates the rule as it reads now against this message, with the pure `rules::evaluate`, and lists its conditions, each marked as matched or not. It says plainly that this is the rule as it reads now, so it may differ from when it acted.

## Hit counts (Rules table, CLI, MCP)

- Two columns at the end of the Rules table, as on the canvas: **Last 30 days** and **Last fired**. A hit is one message a rule acted on, counted once however many actions it took on it. Counts are summed over every account the rule watches (one store per account). Rules applied by hand with Apply to existing mail count too; they are in the same log.
- Last fired is the newest such action, worded like the list's dates. A rule with no hits ever reads "no matches yet" and "—"; one with hits only longer ago reads 0 and its date.
- Both columns sort like every other column (`SheetSort`), numerically and by time.
- The counts are read off the window thread and refreshed after a sync event, not live.
- `postvak rules list` (text and `--json`) and MCP `rules_list` gain `hits_30d` and `last_fired` (Unix seconds, or null) per rule, through the shared `output` rows so the JSON is identical. Regenerate `docs/src/cli.md` if the text output's columns are documented there.
- Speed: one grouped count per store over `rule_log` by `rule_name` and `at`. If it is slow on a large log, add an index in a new migration; measure first.

## The proposal card (Rules view)

The existing card already has who proposed it, the one-line summary, the TOML or diff, Approve, Reject and Preview. It changes to the canvas's layout:

- Header: "Agent proposal · <proposer>", with the proposer as `proposed_by` names it (`mcp:claude-desktop` shown as "claude-desktop").
- Title: the rule's name in the display face; under it the summary sentence the card already words.
- "rules.toml · adds N lines" (or "changes N lines" for an edit proposal) above the TOML or diff, as now.
- The dry run shows without a click: the count in large type ("38 messages would be archived", worded from the rule's main action), the subjects of the first three messages it would hit with their dates, and "N more · Nothing is deleted. Runs on new mail once approved." It reuses the existing preview on cached mail, extended to return up to three samples, and runs off the window thread; the card shows "Counting…" until it is in. The Preview button goes.
- Buttons: **Approve rule** (primary pill), **Reject** (outlined), and **Edit before approving →**, which opens the proposal in the rule dialog.

## The inbox banner

- While any proposal is waiting, a card at the top of the message list in an Inbox and in All inboxes, not in other folders or in search results.
- One proposal: "Agent proposal" in small capitals, then "Claude suggests <summary>. Would affect 38 messages." and a **Review** pill that opens the Rules view. The count appears once its dry run is in, and the sentence reads without it until then. Several: "3 rules waiting for you" and Review.
- It cannot be dismissed; it goes when the proposals are approved or rejected.

## Left to the builder

- Exact strip wording for each action, and the popup's layout for "and N more".
- How direct actions are told apart from rule actions in `rule_log`, within the anti-goal above; for example, whether rule names may clash with the names direct actions log under, and whether a log row's name must match a rule in `rules.toml` or must not be a known actor name.
- Whether hit counts come from a new `Store` method or extend `log`, and the query's shape and any index.
- How Why? lays out its conditions.
- Snapshot coverage: at least a reader with the strip in both themes, the Rules table with the new columns, the redesigned proposal card, and the inbox with the banner.
