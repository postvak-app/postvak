# Named lists in rules.toml

Issue #410. The same domains and subject patterns get copied into several rules, and changing one copy but not the others fails silently. Rules can now refer to a named value in `[lists]` instead.

## Decisions

- A reference is `{ list = "name" }`, never a marked string: `from_domain` already accepts `@kids` as the domain `kids`, so a typo would quietly become a rule that never matches.
- Two kinds of list. An array of strings is a domain list, for `from_domain` and `to_domain`. A table with exactly one of `contains`, `equals`, `regex` is a text list, for `from`, `to`, `cc`, `subject`, `body` and `header.<name>`. Using one where the other belongs is an error.
- Domain conditions mix references with domains in place: `from_domain = [{ list = "kids" }, "extra.example"]`. A text condition that refers to a list holds nothing else, since a regex cannot merge with `contains`.
- A list cannot refer to another list. No cycles to detect, nothing transitive to preview.
- Every list is checked whether a rule uses it or not, and an error names the list (`list 'kids': …`); a bad reference names the rule and its place (`rule 'x': match.any[0].from_domain: no list named 'kid' in [lists]`).

## How it works

`rules::lists::resolve` replaces every reference with the list's values before `compile_match`, so the engine never sees a reference. The compiled rule carries `definition`, the JSON of its account, folder, resolved match and actions, which `rule_first_seen` compares: changing a list restarts the clock of exactly the rules that use it. A rule without references keeps the definition string it had, so upgrading restarts no clock.

Anything that compiles a rule outside its file (a proposal's check, `rules test --stdin`, the MCP preview, the window's dry run and its Why?) compiles it against the file's lists with `compile_with`.

## Surfaces

- `rules list --json` / MCP `rules_list`: the reference stays in `match`, and `lists` holds the values of each list the rule uses, by name.
- `rules coverage` / `senders` (CLI and MCP): each contest and sender group carries `lists`, the lists its matching rules use; the text output adds `(lists a, b)` after the rule names.
- GUI: a reference reads by name (`subject in list alerts`, `from domain list kids or extra.example`), is kept out of the editor's rows like other conditions the form cannot edit, and is saved as written.
- Agents can propose rules that refer to existing lists.

## List proposals

One list change changes every rule that uses it, so agents change lists the way they change rules: by proposal, which a human approves.

- Storage: `[[list_proposals]]` entries with `name`, `proposed_by` and `value`, kept apart from `[lists]`, so a pending change can never reach a rule. The whole-file check validates each value and allows one pending change per list; whether approving would break a rule is checked when proposing and when approving, so a stale proposal never stops the rules.
- Proposing: `rules propose` / `rules_propose` take `lists` (name to value), alone or beside `rules`, in the same all-or-none batch. Refused when the value is malformed, equals the current list, would leave any rule invalid, or the list has another proposer's pending change; `replace` swaps the proposer's own. Rules in a batch compile against approved lists only.
- Addressing: `--list` on `rules approve`, `reject` and `withdraw`; the MCP tools take `{"list": "kids"}` instead of `{"name": …}`.
- Approving writes the value into `[lists]` (a new `[lists]` table goes above the rules; a changed value keeps its comment), drops the proposal, and restarts the clock of every rule that uses the list in every account's store.
- Preview: `rules::coverage::list_change` counts, for each enabled rule that uses the list in an account, the stored messages of its folder it matches now and once approved, and how many it gains and loses. `rules test --list`, MCP `rules_test` with `list`, and the window's card show it. `rules coverage --include-pending` applies pending list changes.
- Listing: `rules lists` / `rules_lists` give every list, its values, the rules that use it and its pending change.
- A server narrowed to some accounts refuses to propose or approve a change of a list that a rule of a hidden account, or of every account, uses.
- The daemon notifies about a new list change as about a new rule proposal ("A list change waits for your approval"). The GUI shows it among the proposals: the list's line before and after, a sentence naming the rules it reaches, and the dry run per rule and account; Approve change and Reject, no Edit. The inbox banner counts list changes too.
- Deleting a list stays a manual edit.

## Later

The rule editor offers existing lists when editing a domain or subject condition and shows a list's values on demand.
