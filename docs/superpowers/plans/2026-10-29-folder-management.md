# Folder Management Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Create, rename and delete IMAP folders from the GUI and the CLI.

**Architecture:** A new `postvak-core` module `folder_ops` holds the checks and the server work; the daemon runs it on the account's sync thread as three new `sync::Command`s. A rename's rules.toml rewrite is a `rules::edit` function the front end calls once the daemon confirms the rename, since only the front end has every account's config. The GUI adds three menu items and one dialog state per operation; the CLI adds `postvak folder`.

**Tech Stack:** Rust workspace, rusqlite, async-imap behind `MailOps`, Slint, clap.

**Spec:** `docs/superpowers/specs/2026-10-29-folder-management-design.md`

## Global Constraints

- Not offered over MCP; `docs/src/` says why.
- Protected (no rename, no delete): INBOX and any folder whose `special_use` is set; no rename of a folder with a protected folder under it.
- New and renamed names are NFC and pass `folder_name::check_new` (empty level, edge whitespace, `*`, `%`, name already known).
- Delete is refused while the folder has subfolders: "Delete or move its subfolders first."
- Delete of a non-empty folder needs a mode (`trash` / `permanent`) and the confirmed count; a different server count is refused.
- Permanent delete backs every message up with `Trash::save` first; one failed backup deletes nothing.
- Split rule copies are named `<name> (<account>)`.
- Offline: commands are already failed by `fail_requests` (`sync.rs`); no queuing work needed, only a test.
- AGENTS.md definition of done: fmt, clippy `-D warnings`, test, machete, deny; no `unwrap`/`expect` outside tests; `POSTVAK_BLESS=1 cargo test` after CLI flags; snapshots after GUI work.

## Review Focus

- Renaming `Work` to `Work2` must not touch `Workshop` (child match is `old + delimiter`, not a string prefix). Test in Task 2 and Task 4.
- A folder name that differs only in Unicode normalization (NFD `Café`) is the same folder: create refuses it, rename finds rules naming it. Test in Task 1 and Task 4.
- Mail arriving between the dialog and the delete: the count check refuses. Test in Task 3.
- The server refuses RENAME or DELETE: store and rules.toml stay untouched and the server's message is shown. Test in Task 3.
- Rename while a rule targets a *child* (`Work/Clients`): the rule follows to `Work2/Clients`. Test in Task 4.

---

### Task 1: Name checks and protection

**Files:**
- Modify: `crates/postvak-core/src/folder_name.rs`
- Modify: `crates/postvak-core/src/rules/mod.rs:671-690` (move `check_folder_shape` out, call the new one)

**Interfaces:**
- Produces: `folder_name::check_shape(name: &str, target: bool) -> Result<(), String>` (moved verbatim from `rules::check_folder_shape`; levels still split on `/` as today)
- Produces: `folder_name::check_new(store: &Store, name: &str) -> Result<String, String>`: NFC of `name` after `check_shape(name, true)`, refused with `"a folder named {known} already exists"` when `known(store, name)` finds one
- Produces: `folder_name::protected(store: &Store, name: &str) -> Result<Option<String>, StoreError>`: `Some("INBOX")` for INBOX in any case, `Some(role)` for a folder with `special_use`, else `None`
- Produces: `folder_name::protected_below(store: &Store, name: &str) -> Result<Option<String>, StoreError>`: the first stored folder under `name + delimiter` that `protected` names, as `"{child} ({role})"`
- Produces: `folder_name::children(store: &Store, name: &str) -> Result<Vec<String>, StoreError>`: stored folders whose name starts with `name` + the folder's delimiter, sorted

- [ ] **Step 1: Write failing tests in `folder_name.rs`**: `check_new_refuses_a_known_name_in_nfd` (store has `Café` NFC; `check_new(store, "Cafe\u{301}")` is `Err`), `check_new_returns_nfc`, `check_new_refuses_wildcards_and_empty_levels` (`"A/*"`, `"A//B"`, `"A/"`), `inbox_and_special_use_are_protected` (`inbox` → `Some("INBOX")`, a `\Trash` folder → `Some("\\Trash")`, plain → `None`), `children_match_by_delimiter_not_prefix` (store `Work`, `Work/Clients`, `Workshop` with delimiter `/`; `children("Work") == ["Work/Clients"]`), `protected_below_names_the_child` (`Mail`, `Mail/Sent` with `\Sent`).
- [ ] **Step 2: Run** `cargo nextest run -p postvak-core folder_name`; expect compile failure.
- [ ] **Step 3: Implement** the five functions; `rules::compile` calls `folder_name::check_shape`.
- [ ] **Step 4: Run** `cargo nextest run -p postvak-core folder_name rules`; all pass, including the existing `move folder level` tests in `rules/mod.rs`.
- [ ] **Step 5: Commit** `feat(core): check new folder names and protected folders`

### Task 2: Store rename and the MailOps calls

**Files:**
- Modify: `crates/postvak-core/src/store.rs:44-98` (lift the rename branch of `decode_folder_names`)
- Modify: `crates/postvak-core/src/mail_ops.rs` (trait + `RecordingOps`), `crates/postvak-core/src/mail_ops/imap.rs:207-213`, `crates/postvak-core/src/sync/tests.rs:2683-2691` (the wrapper forwards the new calls)

**Interfaces:**
- Produces: `Store::rename_folder(&self, from: &str, to: &str) -> Result<(), StoreError>`: in one transaction, `from` and every stored folder under `from + delimiter` take the new prefix, with `messages`, `rule_log` and `rule_moves` following (same four UPDATEs as migration 017, `defer_foreign_keys` on). A free function `rename_folder_rows(tx, from, to)` is what both migration 017 and this method call.
- Produces on `MailOps`: `fn rename_folder(&mut self, from: &str, to: &str) -> MailResult<()>`, `fn delete_folder(&mut self, name: &str) -> MailResult<()>` (drop the `cfg(testing)` on the `ImapOps` one and make it the trait impl), `fn unsubscribe_folder(&mut self, name: &str) -> MailResult<()>`. `RecordingOps` records `rename_folder A B`, `delete_folder A`, `unsubscribe_folder A`, renames children in its `folders`/`mail` maps, has `fail_next_rename` / `fail_next_delete`, and refuses DELETE of a folder with children as Dovecot does (`[INUSE]`).

- [ ] **Step 1: Write failing tests**: in `store.rs` `rename_folder_keeps_messages_and_rule_history_of_children` (`Work`, `Work/Clients`, `Workshop`; after `rename_folder("Work","Archive/Work")` messages of the first two are under the new names, `Workshop` untouched, `rule_log` rows follow); migration 017's existing tests stay green.
- [ ] **Step 2: Run** `cargo nextest run -p postvak-core store`; expect failure.
- [ ] **Step 3: Implement** `rename_folder_rows`, `Store::rename_folder`, the three `MailOps` methods on `ImapOps` (`session.rename`, `session.delete`, `session.unsubscribe`, each name through `self.wire`) and `RecordingOps`.
- [ ] **Step 4: Run** `cargo nextest run -p postvak-core --features testing`; pass.
- [ ] **Step 5: Commit** `feat(core): rename folders in the store and on the server`

### Task 3: `folder_ops` and the daemon commands

**Files:**
- Create: `crates/postvak-core/src/folder_ops.rs` (+ `pub mod folder_ops;` in `lib.rs`)
- Modify: `crates/postvak-core/src/sync.rs:307-341` (`Command`), `:60` (`Event`), `run_command` (~1118)
- Modify: `crates/postvak-core/src/trash.rs:113` (restore creates a missing folder)
- Modify: `crates/postvak-core/src/daemon/client.rs` (three request helpers)
- Modify: `AGENTS.md` module map: one line for `folder_ops`
- Test: `crates/postvak-core/src/sync/tests.rs`

**Interfaces:**
- Consumes: Task 1 checks, Task 2 store and `MailOps` calls
- Produces in `sync`:
  ```rust
  pub enum DeleteMode { Trash, Permanent }   // serde snake_case
  Command::CreateFolder { name: String }
  Command::RenameFolder { from: String, to: String }
  Command::DeleteFolder { name: String, mode: Option<DeleteMode>, count: u32 }
  Event::FolderCreated { account, name, request }
  Event::FolderRenamed { account, from, to, request }
  Event::FolderDeleted { account, name, request }
  Event::FolderNotEmpty { account, name, count: u32, has_trash: bool, request }
  ```
- Produces in `folder_ops`: `create(ops, store, name) -> Result<String, FolderError>` (returns the NFC name), `rename(ops, store, from, to) -> Result<String, FolderError>`, `delete(ops, store, trash, name, mode: Option<DeleteMode>, count: u32, now: i64) -> Result<Deleted, FolderError>` where `enum Deleted { Done, NotEmpty { count: u32, has_trash: bool } }`; `FolderError` (thiserror) with `Protected(String)`, `Invalid(String)`, `HasSubfolders`, `CountChanged { shown: u32, now: u32 }`, `Mail(MailError)`, `Store(StoreError)`, `Backup(io::Error)`, `LeftEmpty(MailError)` ("the folder's mail was moved, but deleting the empty folder failed: …").
- Produces in `daemon::Client`: `create_folder(&self, account, name) -> anyhow::Result<String>`, `rename_folder(&self, account, from, to) -> anyhow::Result<String>`, `delete_folder(&self, account, name, mode, count) -> anyhow::Result<Deleted>`; same shape as `Client::restore`.

Order inside each operation: checks → server → store. Create: `check_new`, CREATE, SUBSCRIBE (warn on failure, as `missing_target`), `upsert_folder` with the parent's delimiter. Rename: `protected`, `protected_below`, `check_new(to)`, RENAME, SUBSCRIBE new + UNSUBSCRIBE old (warn only), `Store::rename_folder`. Delete: `protected`, `children` non-empty → `HasSubfolders`; `status(name).messages` is the count; 0 → DELETE; else `mode` None → `NotEmpty`; count ≠ `count` → `CountChanged`; `Trash` → select, `search_uids(1)`, `move_messages(uids, trash_folder)`; `Permanent` → select, every uid `fetch_raw` + `Trash::save` (all before any delete), then DELETE; on DELETE failure after moves/backups → `LeftEmpty`; then `store.remove_folder`. The trash folder is `rules::apply::special_folder(store, "Trash")` (make it `pub(crate)`). Each command sets `run.used_connection = true` and the event names the folder so `load` refreshes the tree.

Restore: `Trash::restore` takes `store: &Store`; when `folder_name::known(store, &folder)` is `None`, it creates and subscribes the folder first (create failure is ignored, as `missing_target` does, and the APPEND's error is what is reported).

- [ ] **Step 1: Write failing sync tests** (pattern: the existing `Command::Restore` tests): `create_folder_creates_subscribes_and_stores_nfc`, `create_folder_refuses_a_known_name`, `rename_folder_moves_children_in_store`, `rename_refuses_inbox_and_special_use`, `rename_refuses_a_folder_holding_sent`, `rename_refused_by_server_leaves_store_alone`, `delete_empty_folder`, `delete_refuses_subfolders`, `delete_without_mode_answers_not_empty_with_count_and_trash`, `delete_to_trash_moves_every_message_then_deletes`, `delete_permanent_backs_up_every_message_first`, `delete_permanent_backup_failure_deletes_nothing` (trash dir read-only), `delete_refuses_a_changed_count` (count 3 confirmed, server has 4 → `CountChanged{shown:3,now:4}`), `delete_failing_after_moves_says_left_empty`, `folder_commands_are_failed_while_offline`; in `trash.rs` `restore_recreates_a_deleted_folder`.
- [ ] **Step 2: Run** `cargo nextest run -p postvak-core --features testing folder restore`; expect failure.
- [ ] **Step 3: Implement** `folder_ops`, the enums, the `run_command` arms (each maps `folder_ops` to its `Event`, `Err` to `CommandFailed` with `FolderError`'s text), the client helpers and the `Trash::restore` change (update its callers in `sync.rs`).
- [ ] **Step 4: Run** `cargo nextest run -p postvak-core --features testing` and `cargo test --test architecture`; pass.
- [ ] **Step 5: Commit** `feat(core): create, rename and delete folders through the daemon`

### Task 4: Rules follow a rename

**Files:**
- Modify: `crates/postvak-core/src/rules/edit.rs`

**Interfaces:**
- Produces: `pub struct FolderRename<'a> { pub account: &'a str, pub accounts: &'a [String], pub from: &'a str, pub to: &'a str, pub delimiter: Option<&'a str> }`
- Produces: `pub fn rules_naming(file: &RuleFile, rename: &FolderRename) -> Vec<(String, bool)>`: the rules that name `from` or a child, as `(name, will_split)`, for the dialog and the CLI's preview
- Produces: `pub fn follow_rename(path: &Path, rename: &FolderRename) -> Result<Vec<String>, RulesError>`: rewrites `folder` and `move` (matching with `folder_name::same`, children by `from + delimiter`); returns the names changed or split

Rules: a rule whose `account` is another account is untouched. `account == Some(rename.account)`, or `None` with `accounts.len() == 1`, is rewritten in place. `None` with several accounts is split: in the original's place, one copy per other account (`account` set, old names, named `<name> (<account>)`), then one for the renamed account with the new names, named `<name> (<rename.account>)`. A split copy's name already in the file gets ` 2`, ` 3`. Written through the module's existing load/save path so validation and comments behave as `save_rule`.

- [ ] **Step 1: Write failing tests**: `follow_rename_rewrites_folder_and_move`, `follow_rename_follows_children_not_lookalikes` (`Work/Clients` → `Work2/Clients`, `Workshop` untouched), `follow_rename_matches_nfd_names`, `follow_rename_leaves_other_accounts_rules`, `follow_rename_rewrites_unscoped_rule_with_one_account`, `follow_rename_splits_unscoped_rule_with_several_accounts` (accounts `a`,`b`,`c`, rename in `b`: three rules in the original's place, `rule (a)` and `rule (c)` keep the old folder, `rule (b)` has the new one, order a, c, b), `rules_naming_marks_splits`.
- [ ] **Step 2: Run** `cargo nextest run -p postvak-core rules::edit`; expect failure.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** same; pass.
- [ ] **Step 5: Commit** `feat(rules): rules follow a renamed folder`

### Task 5: CLI `postvak folder`

**Files:**
- Modify: `src/cli/mod.rs` (new `Command::Folder { command: FolderCommand }` next to `Folders`), `crates/postvak-core/src/help.rs` (`FOLDER`, `FOLDER_CREATE`, `FOLDER_RENAME`, `FOLDER_DELETE`)
- Regenerate: `docs/src/cli.md` with `POSTVAK_BLESS=1 cargo test`

**Interfaces:**
- Consumes: Task 3 client helpers, Task 4 `follow_rename` / `rules_naming`
- Produces: `postvak folder create <NAME> [--account A]`, `postvak folder rename <FROM> <TO> [--account A]`, `postvak folder delete <NAME> [--account A] [--trash | --permanent]` (the two conflict)

Output: create prints `created <name>`; rename prints `renamed <from> to <to>` then one line per rule changed (`rule '<n>' now moves to <to>` / `rule '<n>' split per account`); if the rename succeeded and `follow_rename` failed, exit non-zero with `renamed on the server, but rules.toml was not updated: <e>; these rules still name <from>: …`. Delete first sends with `mode: None, count: 0`; on `NotEmpty` without a flag it fails with `<name> holds <count> messages; pass --trash or --permanent`; with a flag it resends with that count. `--trash` with `has_trash == false` fails `the account has no Trash folder; use --permanent`.

- [ ] **Step 1: Write failing CLI tests** in the existing CLI test file (pattern: other `assert_cmd` tests there): argument parsing (`--trash` and `--permanent` conflict) and the not-empty message, using the testing daemon the other CLI tests use.
- [ ] **Step 2: Run** `cargo nextest run cli`; fail.
- [ ] **Step 3: Implement**; then `POSTVAK_BLESS=1 cargo test`.
- [ ] **Step 4: Run** `cargo nextest run`; pass.
- [ ] **Step 5: Commit** `feat(cli): postvak folder create, rename and delete` (with `docs/src/cli.md`)

### Task 6: GUI menu and dialogs

**Files:**
- Modify: `crates/postvak-gui/src/folders.rs:88-112` (menu), `crates/postvak-gui/src/state.rs` (`UiAction`, `Dialog`, dialog states, event handling), `crates/postvak-gui/src/bind.rs`, `crates/postvak-gui/src/ui/dialogs.slint`, `crates/postvak-gui/src/ui/window.slint`
- Test: `crates/postvak-gui/src/folders.rs` tests, `state.rs` tests, `snapshots.rs`

**Interfaces:**
- Consumes: `folder_name::{protected, protected_below, children, check_new}`, `rules::edit::{rules_naming, follow_rename}`, the Task 3 commands and events
- Produces: `UiAction::{NewFolder(usize, Option<String>), RenameFolder(usize, String), DeleteFolder(usize, String), FolderName(String), FolderConfirm(Option<DeleteMode>)}`; `Dialog::Folder` (one dialog, its `FolderDialog` state says which operation) placed after `Dialog::Rule` in `TOP_FIRST`
- `pub(crate) struct FolderDialog { account: usize, kind: FolderKind, name: String, error: Option<String>, waiting: Option<RequestId> }`, `enum FolderKind { New { parent: Option<String> }, Rename { from: String, rules: Vec<(String, bool)> }, Delete { folder: String, not_empty: Option<(u32, bool)> } }`

Menu: below the separator, before Mark all as read: `New folder…` (always, on a folder or account row), `Rename…` and `Delete…` (only on a folder row, disabled when `protected` or, for rename, `protected_below` is `Some`). New under a folder joins with its delimiter; on an account row it is top level, under `INBOX` + delimiter when `folder_name::under_inbox`. Rename prefills the current last level and shows the `rules_naming` lines. Delete sends `mode: None` at once and shows "Deleting…" until the answer; `FolderNotEmpty` turns it into the two-button question (`Move N messages to Trash` only when `has_trash`, `Delete N messages permanently`); `FolderDeleted` closes it. `check_new` runs on each keystroke and disables the confirm button with its error. `FolderRenamed` calls `follow_rename` and notes its error in the status bar; a selected folder that was renamed or deleted moves the view to the renamed folder or INBOX. `CommandFailed` for the dialog's request shows in the dialog.

Slint: one `FolderDialog` component in `dialogs.slint`, built from `Dialog` with the theme's `dialog` width, a `LineEdit` for new/rename and the buttons; no literal sizes (use `Theme`).

- [ ] **Step 1: Write failing tests**: `folders.rs` `menu_offers_folder_management` and `menu_disables_rename_and_delete_for_inbox_and_special_use` and `menu_disables_rename_above_a_protected_folder`; `state.rs` with `Fixture`/`Driver`: `new_folder_under_selected_joins_with_delimiter`, `new_folder_name_error_disables_confirm`, `rename_lists_rules_that_follow`, `delete_not_empty_offers_trash_and_permanent`, `delete_without_trash_offers_permanent_only`, `renamed_selected_folder_keeps_the_view`, `deleted_selected_folder_returns_to_inbox`, `escape_closes_the_folder_dialog`.
- [ ] **Step 2: Run** `cargo nextest run -p postvak-gui folder`; fail.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo nextest run -p postvak-gui`; pass. Add snapshots for the three dialogs (new, rename with a split rule line, delete not empty); `TZ=UTC UPDATE_SNAPSHOTS=1 cargo test -p postvak-gui snapshots`, look at the PNGs.
- [ ] **Step 5: Commit** `feat(gui): create, rename and delete folders from the folder tree`

### Task 7: Live IMAP test, docs, done

**Files:**
- Modify: `tests/imap_live.rs`, `docs/src/` (the folders page, or a new `folders.md` linked from `SUMMARY.md`), `README.md` only if `docs/src/index.md` changes

- [ ] **Step 1: Write live tests**: `folder_create_rename_delete` on port 10993 (create `Live/A`, `Live/A/B`; rename `Live/A` → `Live/C`, `Live/C/B` listed; delete `Live/C` refused; delete `Live/C/B` empty; fill `Live/C` with 2 messages, delete without mode → not empty 2, delete `trash` → Trash has 2; again with `permanent` → 2 files in the trash dir; `trash restore` one → `Live/C` recreated).
- [ ] **Step 2: Run** `docker compose -f tests/dovecot/compose.yml up -d` then `POSTVAK_TEST_IMAP_HOST=localhost cargo test --features testing --test imap_live folder`; pass.
- [ ] **Step 3: Docs**: create, rename (rules follow, splitting rules for several accounts), delete (Trash or permanent, restore from Postvak's trash), protected folders, and that the MCP server leaves folder management to the person on purpose.
- [ ] **Step 4: Definition of done**: `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test`, `cargo machete`, `cargo deny check`; all pass.
- [ ] **Step 5: Commit** `docs: managing folders`
