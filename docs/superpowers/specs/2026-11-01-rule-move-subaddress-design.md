# Moving mail to a folder named by its +tag

Issue #501. One rule gives every `+tag` address its own folder: `actions.move = "Tags/{subaddress}"`.

## Behaviour

- `{subaddress}` may appear in any level of a `move` folder but the first, and each one is replaced. `rules check` refuses it in the first level, where a tag such as `trash` would reach an existing folder. It is filled in per message by `rules::engine::evaluate`, so sync, `rules test`, `rules coverage`, the MCP tools and the GUI's previews all see the real folder and nothing downstream knows about the placeholder.
- The tag comes from the first recipient (Delivered-To, which the receiving server writes, then To and Cc) with a usable tag (below) that is the rule's top-level `match.alias`, or, for a rule without one, your own address or an alias, with or without its tag. It is the text between the first `+` of the local part and the `@`.
- The sender chooses the tag (anyone can write to `me+anything@`), and the move creates folders, so the tag must be 1 to 64 ASCII letters, digits, `-` and `_`. It is lower-cased, so case variants share one folder. No `.` or `/`, so a tag is always one level, also on servers whose delimiter is `.`.
- A message without such a tag is not moved; the rule's other actions still run, and since nothing moved it, it notifies as usual.
- `rules check` accepts the template as any other move folder; the placeholder is not checked further.

## Not in scope

`{list_id}`: `List-Id` is written by the sender with no address of yours involved. If it comes, it moves only into folders that already exist (option 2 in the issue).
