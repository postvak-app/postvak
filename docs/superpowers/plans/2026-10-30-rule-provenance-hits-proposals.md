# Rule provenance, hit counts and the proposal card Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Show which rule acted on the open message, how often each rule acts, and redesigned proposals with an inbox banner, all read from the existing `rule_log`.

**Architecture:** `postvak-core` gains two `Store` queries over `rule_log` (by message and grouped by rule), an index for them, `rules::is_actor` to tell your own actions from rules', and `rules::hits` to sum counts across accounts. The CLI and MCP add the counts to their rule rows through `output::rule`. The GUI reads provenance on the reader thread with the message, counts and dry runs on `worker` threads, and draws the strip, the columns, the card and the banner in Slint.

**Tech Stack:** Rust, rusqlite (migrations in `crates/postvak-core/migrations/`), Slint 1.18, cargo-nextest.

**Spec:** `docs/superpowers/specs/2026-10-30-rule-provenance-hits-proposals-design.md`

## Global Constraints

- Hit counts ship on CLI, MCP and GUI together; the strip, card and banner are GUI only (spec, Surfaces).
- A hit is one message a rule acted on, counted once; the window is 30 days (`30 * 86_400` seconds before now).
- Direct actions log under actor names: `cli`, `cli:<who>`, `gui`, `mcp`, `mcp:<client>`. These never count as a rule's and never show in the strip.
- Nothing here runs on the window thread that scans a store: counts and dry runs use `worker::oneshot`; provenance rides the existing reader thread.
- AGENTS.md definition of done: fmt, clippy `-D warnings`, nextest, machete, deny; `POSTVAK_BLESS=1 cargo test` after CLI output changes; `TZ=UTC UPDATE_SNAPSHOTS=1 cargo test -p postvak-gui snapshots` after GUI work, PNGs looked at.
- Layout uses `Theme` tokens (`row`, `control`, `action`, `pad`, `inset`, `radius`), never literal sizes.

## Review Focus

- A rule named exactly like an actor (`gui`, `cli`): its log rows are indistinguishable from your own; expect it to get no strip and 0 hits rather than your actions being shown as the rule's. Test in Task 1.
- A message with no Message-ID, or one whose Message-ID is shared by a duplicate in another folder: no strip for the first; the second shows the newest rule action for that id. Test in Task 4.
- A rule renamed or removed since it acted: the strip says "(removed)" and drops Edit rule; the table's counts for the new name start at 0. Test in Task 4.
- Move back when the source folder no longer exists, or the message is already in it: the link is hidden. Test in Task 4.
- A proposal whose dry run fails (bad config, a store that will not open): the card shows the error text where the count goes, Approve still works. Test in Task 5.

---

### Task 1: Core log queries, actor names and the index

**Files:**
- Create: `crates/postvak-core/migrations/023-rule-log-index/up.sql`
- Modify: `crates/postvak-core/src/store.rs` (beside `log_action` / `log`), `crates/postvak-core/src/rules/mod.rs`
- Test: in-file `#[cfg(test)]` modules of both

**Interfaces:**
- Produces: `pub fn is_actor(name: &str) -> bool` in `rules`; `Store::rule_actions(&self, msg: &Message, archive: Option<&str>) -> Result<Vec<LogEntry>, StoreError>` (rule rows for this exact copy, newest first, per the spec's matching rule; `archive` is the account's archive folder); `Store::rule_hits(&self, since: i64) -> Result<Vec<RuleHits>, StoreError>` with `pub struct RuleHits { pub rule: String, pub messages: u64, pub last_fired: i64 }` where `messages` counts distinct `(folder, uid)` acted on at or after `since` and `last_fired` is the newest `at` ever.

- [ ] **Step 1: Write the failing tests**
  - `rules::tests::actor_names_are_the_names_direct_actions_log_under`: `is_actor` true for `cli`, `cli:me`, `gui`, `mcp`, `mcp:claude-desktop`; false for `travel`, `client`, `guide`.
  - `store::tests::rule_hits_count_messages_once_and_skip_actors`: log `travel` read+move on one message at t=100, `travel` on a second at t=10, `gui` trash on a third at t=100; `rule_hits(50)` gives exactly `[RuleHits { rule: "travel", messages: 1, last_fired: 100 }]`.
  - `store::tests::rule_actions_finds_the_moved_copy_and_not_its_twins`: `travel` flagged INBOX/1 in place, then moved it `move:Travel` (recorded in `rule_moves` with INBOX, `<a@x>`, internaldate 10); the message now Travel/7 with internaldate 10 gets both rows, newest first; a Sent copy with `<a@x>` and internaldate 11 gets none; a `gui` row is never returned.
- [ ] **Step 2: Run** `cargo nextest run -p postvak-core actor_names rule_hits rule_actions` — expect compile failure.
- [ ] **Step 3: Implement.** Migration 023: `CREATE INDEX rule_log_message ON rule_log(message_id); CREATE INDEX rule_log_rule_at ON rule_log(rule_name, at);`. Queries filter actors in Rust with `is_actor` (the actor set has prefixes, so SQL `NOT IN` cannot express it).
- [ ] **Step 4: Run** the same command, then `cargo nextest run -p postvak-core` — PASS, including the migration tests.
- [ ] **Step 5: Commit** `feat(core): query the rule log by message and by rule`.

### Task 2: Hit counts on the CLI and MCP

**Files:**
- Create: `crates/postvak-core/src/rules/hits.rs`
- Modify: `crates/postvak-core/src/rules/mod.rs` (`pub mod hits;`), `crates/postvak-core/src/output.rs:207` (`rule`), `src/cli/mod.rs:1565` (`RulesCommand::List`), `crates/postvak-mcp/src/backend.rs:549` (`rules_list`), `docs/src/cli.md` (blessed), `docs/src/mcp.md`
- Test: `hits.rs` tests, `output.rs` test `rule_rows_carry_the_whole_rule_and_its_position`, `crates/postvak-mcp/src/tests.rs`

**Interfaces:**
- Consumes: `Store::rule_hits`, `RuleHits` (Task 1).
- Produces: `pub struct Hits { pub last_30d: u64, pub last_fired: Option<i64> }` (Default = 0, None); `pub const WINDOW: i64 = 30 * 86_400;` `pub fn total(stores: &[&Store], now: i64) -> Result<HashMap<String, Hits>, StoreError>` summing `messages` and taking the max `last_fired` across stores. `output::rule(rule: &Rule, position: usize, hits: &Hits) -> serde_json::Result<Value>` adds `hits_30d` and `last_fired` (number or null).

- [ ] **Step 1: Failing tests:** `hits::tests::counts_add_up_across_accounts` (two stores, `travel` 2 + 1 → `last_30d: 3`, `last_fired` the later); output test asserts `row["hits_30d"] == 3` and `row["last_fired"] == Value::Null` for `Hits { last_30d: 3, last_fired: None }`; MCP test `rules_list_rows_carry_hit_counts`.
- [ ] **Step 2: Run** `cargo nextest run -p postvak-core hits output -p postvak-mcp rules_list` — FAIL.
- [ ] **Step 3: Implement** `total`, pass `&hits.get(&rule.name).cloned().unwrap_or_default()` at both callers. CLI text output appends `\t<last_30d> in 30 days`; JSON via `output::rule`. MCP `rules_list` sums only visible accounts' stores, so a hidden account's counts never leak.
- [ ] **Step 4: Run** `POSTVAK_BLESS=1 cargo test`, then `cargo nextest run --workspace` — PASS; check the `docs/src/cli.md` diff.
- [ ] **Step 5: Commit** `feat(rules): show how often each rule acted in rules list`.

### Task 3: Hit count columns in the Rules table

**Files:**
- Modify: `crates/postvak-gui/src/rules.rs` (row wording, sort), `crates/postvak-gui/src/state.rs` (counts field, worker, refresh on sync event), `crates/postvak-gui/src/ui/rules.slint` (`RuleData`, two columns), `crates/postvak-gui/src/bind.rs`
- Test: `crates/postvak-gui/src/rules.rs` tests

**Interfaces:**
- Consumes: `rules::hits::{total, Hits}`.
- Produces: `App::rule_hits: HashMap<String, Hits>`; `RuleData.hits: string` ("212", "0" or "no matches yet") and `RuleData.fired: string` (list-style date or "—"); two new `SheetSort` columns, numeric and by time.

- [ ] **Step 1: Failing tests:** `the_table_shows_hits_and_last_fired` (log one `newsletters` hit, open Rules, tick until shown; row reads "1" and the list's date for its time; a rule without hits reads "no matches yet" and "—"); `the_hit_columns_sort_by_number_not_text` (counts 9 and 10 sort 9 before 10).
- [ ] **Step 2: Run** `cargo nextest run -p postvak-gui the_table_shows_hits the_hit_columns` — FAIL.
- [ ] **Step 3: Implement.** Count with `worker::oneshot` when the Rules view opens and after `Event::Synced`; dates through the same function the list uses for its date column.
- [ ] **Step 4: Run** `cargo nextest run -p postvak-gui rules::` — PASS.
- [ ] **Step 5: Commit** `feat(gui): show each rule's hits and when it last fired`.

### Task 4: The provenance strip

**Files:**
- Modify: `crates/postvak-gui/src/reader.rs` (`Read`), `crates/postvak-gui/src/body.rs` (wording), `crates/postvak-gui/src/state.rs` (actions), `crates/postvak-gui/src/ui/body.slint` (strip), `crates/postvak-gui/src/ui/window.slint`, `crates/postvak-gui/src/bind.rs`
- Test: `crates/postvak-gui/src/body.rs` tests

**Interfaces:**
- Consumes: `Store::rule_actions` (Task 1), `rules::evaluate`.
- Produces: `Read.provenance: Vec<LogEntry>`; `body::Provenance { sentence: String, rule: String, more: Vec<String>, removed: bool, move_back: Option<String> }` with `pub(crate) fn provenance(entries: &[LogEntry], rules: &[Rule], current_folder: &str, folders: &[String], now: i64) -> Option<Provenance>`; `UiAction::EditRule(String)` (existing, if present, else add), `UiAction::MoveBack(String)` sending `Command::Apply` with `Action::Move`; `UiAction::WhyRule(String)` producing `Vec<(String, bool)>` (condition wording, matched).

- [ ] **Step 1: Failing tests:**
  - `a_rule_move_shows_who_moved_it` (log `travel` `move:Travel` from INBOX for message `<1@example.com>`, shown in Travel; the strip reads "Moved to Travel by rule travel at 09:13", Edit rule opens the `travel` rule).
  - `move_back_returns_the_message_to_where_the_rule_took_it` (click Move back sends `Apply { folder: "Travel", uids: [1], action: Move("INBOX") }`).
  - `your_own_actions_show_no_strip` (only a `gui` row → no strip).
  - `a_removed_rule_says_so_and_offers_no_edit` (rule gone from rules.toml → "(removed)", no Edit rule).
  - `no_message_id_no_strip`; `move_back_hides_when_its_folder_is_gone`.
  - `two_rule_actions_read_and_1_more` (sentence ends "and 1 more").
  - `why_marks_each_condition` (a rule with `from` matching and `subject` not → `[("from contains …", true), ("subject contains …", false)]`).
- [ ] **Step 2: Run** `cargo nextest run -p postvak-gui body::tests::a_rule_move body::tests::move_back body::tests::your_own body::tests::a_removed body::tests::no_message_id body::tests::two_rule body::tests::why` — FAIL.
- [ ] **Step 3: Implement.** Query in `reader::read` when the message has a Message-ID. Strip per the spec's reader section: `Theme.action` tall, `Theme.radius`, `pane.panel`, rules glyph in `pane.accent`, links on the right; "and N more" opens a `ContextMenuArea`-style popup listing each entry; Why? opens a small popup with the conditions. Action wording: one function from `Action::label()` text to the sentence's verb phrase.
- [ ] **Step 4: Run** `cargo nextest run -p postvak-gui` — PASS.
- [ ] **Step 5: Commit** `feat(gui): say which rule acted on the open message`.

### Task 5: The proposal card

**Files:**
- Modify: `crates/postvak-gui/src/rules.rs` (`proposals`, `Proposal`), `crates/postvak-gui/src/state.rs` (`preview` → worker with samples; remove the `ponytail` shortcut comment), `crates/postvak-gui/src/draft.rs` (`preview_text` wording), `crates/postvak-gui/src/ui/rules.slint` (`ProposalData`, card)
- Test: `crates/postvak-gui/src/rules.rs` tests

**Interfaces:**
- Produces: `DryRun { count: usize, samples: Vec<(String, String)> /* subject, list-style date */, error: Option<String> }` per proposal name in `App::rules.dry_runs: HashMap<String, DryRun>`; `ProposalData` gains `header` ("Agent proposal · claude-desktop"), `lines` ("rules.toml · adds 7 lines" / "changes 3 lines"), `count`, `count-label` ("messages would be archived"), `samples: [SampleData]`, `rest` ("35 more · Nothing is deleted. Runs on new mail once approved."), `counting: bool`; callback `edit-proposal(int)`.

- [ ] **Step 1: Failing tests:** `a_proposal_counts_its_dry_run_without_a_click` (open Rules, tick until `count == "1"`, sample subject shown); `edit_before_approving_opens_the_proposal_in_the_dialog`; `a_failed_dry_run_shows_why_and_approve_still_works`; update tests that clicked Preview.
- [ ] **Step 2: Run** `cargo nextest run -p postvak-gui rules::` — FAIL.
- [ ] **Step 3: Implement.** `proposed_by` shown without its `mcp:` / `cli:` prefix. Approve rule is `TextButton { primary: true }`, Reject `framed`, Edit before approving a `Link`. Saving an edited proposal keeps it a proposal (my call; the spec leaves it open): the card refreshes with a new dry run.
- [ ] **Step 4: Run** `cargo nextest run -p postvak-gui` — PASS.
- [ ] **Step 5: Commit** `feat(gui): show a proposal's dry run on its card`.

### Task 6: The inbox banner

**Files:**
- Modify: `crates/postvak-gui/src/list.rs` (wording), `crates/postvak-gui/src/ui/list.slint` (banner above the rows), `crates/postvak-gui/src/bind.rs`, `crates/postvak-gui/src/state.rs` (Review → `SelectView(View::Rules)`)
- Test: `crates/postvak-gui/src/list.rs` tests

**Interfaces:**
- Consumes: proposals and `dry_runs` (Task 5).
- Produces: `pub(crate) fn banner(app: &App) -> Option<String>`: `None` outside an Inbox or All inboxes or in a search; one proposal → "Claude suggests <summary>. Would affect 38 messages." (the second sentence only once counted); several → "3 rules waiting for you".

- [ ] **Step 1: Failing tests:** `the_inbox_says_a_proposal_waits` (one proposal; banner text; Review opens Rules); `the_banner_counts_several_and_leaves_other_folders_alone`.
- [ ] **Step 2: Run** `cargo nextest run -p postvak-gui list::tests::the_inbox_says list::tests::the_banner` — FAIL.
- [ ] **Step 3: Implement.** "Claude" comes from the proposer when it names a known agent host, else "An agent". Card on `pane.faint`, `Theme.radius`, "AGENT PROPOSAL" at `caps-size`, Review a primary pill.
- [ ] **Step 4: Run** `cargo nextest run -p postvak-gui` — PASS.
- [ ] **Step 5: Commit** `feat(gui): say in the inbox when a rule proposal waits`.

### Task 7: Snapshots, docs and the definition of done

**Files:**
- Modify: `crates/postvak-gui/src/snapshots.rs` (new: `reader_provenance_light`, `reader_provenance_dark`, `rules_hits_and_proposal`, `inbox_banner`), `docs/src/gui.md`, `tests/snapshots/*.png`

- [ ] **Step 1:** Add the four snapshot tests, run `TZ=UTC UPDATE_SNAPSHOTS=1 cargo test -p postvak-gui snapshots`, look at every changed PNG against the canvas boards.
- [ ] **Step 2:** Describe the strip, the columns, the card and the banner in `docs/src/gui.md`.
- [ ] **Step 3:** Run `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo nextest run --workspace --all-features`, `cargo machete`, `cargo deny check` — all pass.
- [ ] **Step 4: Commit** `test(gui): snapshot the rule provenance, hits, proposal card and banner`.
