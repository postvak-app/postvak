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

## Later (stacked on this)

1. List proposals: `rules propose` / MCP `rules_propose` take a new or changed list, `withdraw`, `approve` and `reject` work on it, its preview names every rule it affects, and approving restarts their clocks.
2. The rule editor offers existing lists when editing a domain or subject condition and shows a list's values on demand.
