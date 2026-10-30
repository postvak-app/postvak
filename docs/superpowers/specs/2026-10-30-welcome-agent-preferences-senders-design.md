# Welcome, Connect an agent, Preferences and Senders

Status: draft, 2026-10-30. Phase 5 of the GUI redesign toward the Claude Design canvas (https://claude.ai/artifact/FZbXdN5FXZ6RdxL9jP6Te7). The canvas has a Welcome board; it has no board for the other three, and its notes list them as "Missing surfaces: Senders view, Connect an agent, Preferences". Their layout below follows the canvas's existing parts (the Rules view's header and table, the dialogs, the proposal card's code block) rather than a board. Not yet reviewed with the owner: the calls marked as such below are the author's.

The four parts share nothing but the canvas and ship as four PRs, in any order: Welcome links to Connect an agent, so the last of those two to land adds the link.

## Goals

- A first start looks like Postvak, not like an error: the window opens, says what Postvak is, and puts adding an account one click away.
- Connecting Claude or another agent is a dialog, not a command to look up.
- Every setting that only lives in `config.toml` today can be changed in the window.
- The sender statistics the CLI and MCP have (`senders`) are a view in the window, with a way to write a rule from a row.

## Non-goals

- Editing or removing an account's connection (host, port, username, password, TLS). No code for it exists on any surface; it is a feature of its own.
- Sending mail or a compose window; the welcome footer says Postvak does not send.
- An onboarding tour, or rules made for you on first start.

## Anti-goals

- The welcome must never be shown to someone with an account, and never hide an account that failed to load: a broken `config.toml` keeps the startup window's error, as today.
- Connect an agent must never grant a scope the user did not see ticked; the default stays `read,rules:propose`.
- Preferences must never write `config.toml` except through `config`, the only writer of it, and must keep the file's comments and unknown keys the way `save_theme` does.

## 1. Welcome (first run)

What changes: with no account in `config.toml`, the main window opens instead of the startup window's form, its sidebar reading "No accounts yet" with Senders, Rules, Activity and Backups dimmed, and the Welcome board in the message pane.

- Eyebrow "Welcome to Postvak"; heading "Your inbox, organized by rules **you** write." in the display face at 44px, "you" in the brand colour with the highlighter underline; the lede "Postvak syncs your mail into a local store on this computer and runs your rules on every sync. Nothing is sent to a model to be sorted."
- Three cards, each with the brand bar, a mono step label, a title and a line:
  - **01 Add an account.** "Any IMAP server. Your password stays in the system keyring." The primary pill "+ Add account" opens the existing Add account dialog (`AccountDialog`, the same `AccountForm`).
  - **02 Write or approve a rule.** Dimmed until an account exists. "Start from a sender, or let an agent draft one. Every rule is dry-run first." "See examples →" opens `docs/src/rules.md` online (the Rules view is empty without an account).
  - **03 · optional Connect an agent.** "Claude reads headers and proposes rules. You approve every one." The ink code block `$ postvak mcp install`, and a link that opens the Connect an agent dialog once part 2 has landed.
- Footer: "Reads and sorts mail; it does not send it. Deleted mail is backed up for 30 days." (with the configured retention), then "Keys" and the `?` keycap.
- After the first account is saved, the window loads it as it loads a config change today, and the welcome goes.
- On a narrow window the three cards stack.
- The startup window stays for the problems it explains (no config dir, a config that does not parse); only its no-accounts case moves.

## 2. Connect an agent

A dialog, opened from File ▸ Connect an agent… and from the Welcome's third card.

- **Where:** a choice of Claude Desktop, Claude Code, or Another app (shows the JSON snippet to copy), as `postvak mcp install <claude-desktop|claude-code|json>` offers.
- **What it may do:** the scopes as switches, each with the line `docs/src/mcp.md` gives it; `read` and `rules:propose` on by default, `mail:modify`, `files:write`, `read:bodies` and `rules:write` off and marked as what they let the agent do to mail.
- **Which accounts:** all, or chosen ones (`--account`).
- A preview of the entry it will write, in the proposal card's code block; then **Connect** (primary), which runs the same install as the CLI and says what it did ("Added Postvak to Claude Desktop. Restart Claude Desktop to use it.") or why it could not. **Remove** when Postvak is already registered with that host.
- In a Flatpak, where install prints commands instead of acting, the dialog shows those commands with a Copy button.
- The install code moves to `postvak-core` (or a crate without rmcp), since the GUI may not depend on `postvak-mcp`: `install`, `Entry` and `Scope` hold no rmcp types. The CLI and the MCP crate call it from there.

Surfaces: the CLI already has `mcp install`; the MCP server cannot install itself. This is the GUI catching up, not a new feature.

## 3. Preferences

A dialog (`dialog-large`), opened from File ▸ Preferences… (Postvak ▸ Settings… on macOS, Ctrl+, / Cmd+,). Two sections, as a list on the left:

- **General:** Theme (System, Light, Dark; also stays in View ▸ Theme), Open at login (also stays in File), Check for updates (`check_updates`), Show sender logos (`sender_images`, with what it fetches and from where), HTML mail colours (Adapt to the theme, or Original).
- **Accounts:** per account, its address and server read-only, then Notifications (`notify`), Sync every (`sync_interval_secs`), Keep deleted mail for (`trash_retention_days`), Search message text (`index_bodies`, with what turning it off means for search), and the aliases (`aliases`) as an editable list. Trusted authserv-ids are listed with a Remove.
- Each change saves at once, as the theme does today, through new `config` writers beside `save_theme`; an account's change reaches the daemon as a config change, which restarts only that account's sync.
- Surfaces: GUI only. The CLI's way to change these is `config.toml`, which `docs/src/configuration.md` documents; agents get no tool to change settings. This is the exception the Surfaces rule asks a spec to name.

## 4. Senders

A view, pinned in the sidebar above Rules with the people icon, as on the Main board.

- Header as the Rules view: eyebrow "SENDERS", the title, and "N senders in Inbox"; a folder picker (default the Inbox; All inboxes when there are several accounts), By address / By domain, and a date range (Any time, Last 30 days, Last year).
- A table like the Rules table: Sender (initial avatar, as the list), Messages (with the Rules table's bar), Unread, Last seen, Subjects (the newest one, the others on hover) and Rules (the names of the rules that match, or "no rule"). Sorted by Messages, every column sortable.
- On a row: **Rule from sender** opens a new rule for that address or domain, as the reader's button does; **Show messages** opens a search `from:<address>` in that folder.
- Rows come from `senders::in_folder` off the window thread, refreshed after a sync event, as the hit counts are.
- Surfaces: the CLI and MCP have `senders` already (`2026-10-28-senders-and-coverage-design.md`, which left the GUI out); the GUI shows the same rows (`output::sender`'s fields).

## Calls made (the author's, for the owner to check)

- The welcome replaces the startup form in the main window, rather than restyling the startup window: the canvas draws it inside the window, sidebar and all.
- "See examples →" opens the rules documentation, since the Rules view has nothing to show before an account exists.
- The welcome footer says "Deleted mail is backed up", not the canvas's "Every delete is backed up": a delete that moves to the Trash folder makes no `.eml` backup.
- Preferences saves each change at once, with no OK/Cancel, matching the theme menu.
- Senders is a view, not a dialog, because it is a table you work from, like Rules.

## Left to the builder

- Where the install code lands (a module in `postvak-core`, or a small crate of its own) and how `tests/architecture.rs` records it.
- The exact layout of Preferences and Connect an agent within the Theme grid and dialog widths.
- Which `config` writer shape keeps comments and unknown keys (`save_theme` shows one way).
- Snapshot coverage: the Welcome in both themes and narrow, each dialog, and the Senders view with and without matching rules.
