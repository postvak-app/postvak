# Forward Rule Action Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A `forward` rule action that sends matching new mail on through part 1's outbox, held until a human approves the rule's exact definition, with a rate limit, loop guard and once-only forwarding.

**Architecture:** The action parses into `Action::Forward(Box<Forward>)` in `rules`. The rules pass in `sync` queues a forward through a `ForwardQueue` handle onto the account's outbox (part 1's `Senders`) before the plan's other actions run. Approval, once-only records and rate counts live in the account store; `rules::forward` holds the pure decisions (approval key, loop check, rate check, what to do when queueing fails).

**Tech Stack:** Rust, rusqlite migrations, `mail-parser`, part 1's `send` module, Slint for the rule editor, rmcp for MCP rows.

**Spec:** `docs/superpowers/specs/2026-11-02-forward-rule-design.md`

**Prerequisite:** part 1 (`docs/superpowers/plans/2026-11-02-sending.md`) is merged: `send::reply::forward`, `ForwardOptions`, `AttachmentFilter`, `send::outbox::Entry`, `send::sender::Senders`, `send::server_for`, `send::check` and migration 027 exist.

## Global Constraints

- Definition of done from `AGENTS.md` (fmt, clippy `-D warnings`, test, machete, deny); no `unwrap`/`expect`/`panic!` outside tests.
- `rules/edit.rs` stays the only writer of rules.toml; approvals are never written there.
- A forward runs only in `Mode::Normal`; in `Mode::ApplyExisting`, previews, `rules test` and coverage it is shown as `forward (new mail only)` and not run.
- No undo delay for rule forwards: `Entry.due = now`.
- Defaults: `subject_prefix = "Fwd: "`, `as = "inline"`, `attachments = true`, `max_size` limit = part 1's 25 MiB fallback with `skip`, `rate = 20` per rule per hour, counted per account.
- Loop header: `X-Postvak-Forwarded: <our address>`, appended; skip when one holds an address `Identity::is_me` accepts, when there are 3 already, or on `Auto-Submitted: auto-replied`.
- Approval key: the rule's name and `CompiledRule.definition`; any change needs a new approval; approving restarts the rule's clock (`approve_from_now`).
- Rate exceeded: the message is not forwarded, gets its other actions, the rule's approvals are withdrawn in that account, and the notice reads `Forwarding paused for "<rule>": more than <rate> in an hour`.
- Cannot queue (fetch fails, builder refuses, outbox write fails, `from` not ours): none of the rule's actions run on that message; retried next pass. `max_size = "skip"` is not a failure: other actions run.
- Log row action text: `forward:<first recipient>` plus ` (+N)` for N others.
- Never log recipients beyond what the action log row already holds; never log bodies.
- After rule type changes: `POSTVAK_BLESS=1 cargo test`; after GUI work: `TZ=UTC UPDATE_SNAPSHOTS=1 cargo test -p postvak-gui snapshots`.

## Review Focus

1. **Forward + delete in one rule, raw fetch fails**: the message is not deleted (nothing runs) and is forwarded on the next pass — test in Task 4.
2. **An approved rule widened by an MCP edit proposal the agent approves**: the forward waits again — test in Task 6.
3. **Chain shared → own → bookkeeper**: the second hop forwards and the message then carries two headers — test in Task 3.
4. **The 21st match within an hour**: not forwarded, archived anyway, approval withdrawn, one notice (not one per message) — test in Task 4.
5. **A rule for every account with an explicit `from`**: refused by `rules check` with a message naming `account` — test in Task 1.

---

### Task 1: The action in rules.toml

**Files:**
- Modify: `crates/postvak-core/src/rules/mod.rs`, `crates/postvak-core/src/rules/warnings.rs`
- Generated: `docs/src/rules.schema.json`

**Interfaces:**
- Consumes: part 1 `send::reply::{ForwardAs, AttachmentFilter}`
- Produces:
  - `rules::Forward { to: Vec<String>, cc: Vec<String>, bcc: Vec<String>, from: Option<String>, subject_prefix: String, note: Option<String>, as_: ForwardAs, attachments: AttachmentSelection, max_size: Option<SizeRule>, rate: u32 }`
  - `rules::AttachmentSelection { All, None, Filter { type_: Option<String>, name: Option<TextMatch>, max_size: Option<u64> } }` — TOML `true` / `false` / table; part 1's `AttachmentFilter` gains a `Filter` variant built from it
  - `rules::SizeRule { limit: u64, over: OverSize }`, `OverSize { Skip, Strip }` — TOML `max_size = "5MB"` or `{ limit = "5MB", over = "strip" }`
  - `Action::Forward(Box<Forward>)`; `Actions.forward: Option<ForwardTable>` (placed before `after` in the struct, after the comment that keeps `after` last); `Action::waits_with_after` true for it
  - `Rule::forward(&self) -> Option<&Forward>`
  - Validation in `rules::parse` / `check`: `to` non-empty; every address an addr-spec; `attachments` filter refused with `as = "attachment"`; `from` refused without `account`; `from` must satisfy that account's `Identity::is_me` (in `warnings`, which has the config)
- [x] **Step 1: Write the failing tests** in `rules/mod.rs`:
  - `forward_parses_with_defaults`: `actions.forward = { to = "a@x.org" }` → `to == ["a@x.org"]`, `subject_prefix == "Fwd: "`, `as_ == Inline`, `attachments == All`, `rate == 20`.
  - `forward_takes_lists_filters_and_size`: the spec's invoice example parses; `max_size = { limit = "5MB", over = "strip" }` → `limit == 5_000_000`.
  - `forward_refusals`: empty `to`; `to = "not an address"`; `attachments = { type = "application/pdf" }` with `as = "attachment"`; `from` without `account` (Review Focus 5: message contains `account`).
  - `forward_round_trips_through_from_list` (`Actions::from_list` ↔ `into_list`).
  - In `warnings.rs`: `a_from_the_account_does_not_own_is_a_warning`.
- [x] **Step 2: Run** `cargo nextest run -p postvak-core rules::` — FAIL.
- [x] **Step 3: Implement.**
- [x] **Step 4: Run** — PASS; `POSTVAK_BLESS=1 cargo test` and check the schema diff.
- [x] **Step 5: Commit** `feat(rules): a forward action in rules.toml`

### Task 2: Approval, once-only and rate records in the store

**Files:**
- Create: `crates/postvak-core/migrations/028-forward/up.sql`
- Modify: `crates/postvak-core/src/store.rs`

**Interfaces:**
- Produces:
  - Tables: `forward_approvals (rule TEXT PRIMARY KEY, definition TEXT NOT NULL, approved_at INTEGER NOT NULL)`, `forwarded (rule TEXT NOT NULL, key TEXT NOT NULL, at INTEGER NOT NULL, PRIMARY KEY (rule, key))` with an index on `(rule, at)`, `forward_failures (rule TEXT NOT NULL, key TEXT NOT NULL, reason TEXT NOT NULL, first_at INTEGER NOT NULL, last_at INTEGER NOT NULL, PRIMARY KEY (rule, key))`
  - `Store::approve_forward(&self, rule: &str, definition: &str, now: i64) -> Result<()>`
  - `Store::forward_approved(&self, rule: &str, definition: &str) -> Result<bool>` — true only when the stored definition equals `definition`
  - `Store::withdraw_forward(&self, rule: &str) -> Result<()>`
  - `Store::was_forwarded(&self, rule: &str, key: &str) -> Result<bool>`; `Store::record_forwarded(&self, rule: &str, key: &str, now: i64) -> Result<()>`
  - `Store::forwards_since(&self, rule: &str, since: i64) -> Result<u32>`
  - `Store::record_forward_failure(&self, rule: &str, key: &str, reason: &str, now: i64) -> Result<()>`, `Store::clear_forward_failure(&self, rule: &str, key: &str) -> Result<()>`, `Store::forward_failures(&self, rule: &str) -> Result<Vec<(String, String, i64)>>`
  - `rules::forward::once_key(msg: &Message, uidvalidity: u32) -> String` — `id:<Message-ID>` or `uid:<folder>/<uidvalidity>/<uid>` (in `crates/postvak-core/src/rules/forward.rs`, created here)
- [x] **Step 1: Write the failing tests:**
  - `forward_approval_matches_the_exact_definition`: approve `("inv", "d1")` → `forward_approved("inv", "d1")` true, `("inv", "d2")` false; `withdraw_forward` → false.
  - `forwarded_is_recorded_once_and_counted_in_the_window`.
  - `once_key_uses_message_id_else_folder_uidvalidity_uid`.
  - `forward_failures_keep_first_and_last_time`.
- [x] **Step 2: Run** `cargo nextest run -p postvak-core store::tests::forward rules::forward` — FAIL.
- [x] **Step 3: Implement.**
- [x] **Step 4: Run** — PASS.
- [x] **Step 5: Commit** `feat(store): forward approvals, once-only records and failures`

### Task 3: The pure forward decisions

**Files:**
- Modify: `crates/postvak-core/src/rules/forward.rs`

**Interfaces:**
- Consumes: `Identity::is_me`, `Forward` (Task 1)
- Produces:
  - `forward::LOOP_HEADER: &str = "X-Postvak-Forwarded"`
  - `forward::loop_check(headers: &[u8], identity: &Identity) -> Option<Skip>`; `forward::Skip { Loop, TooManyHops, AutoReply, OverSize }` with `fn log_text(&self) -> &'static str`
  - `forward::with_loop_header(raw: &[u8], our_address: &str) -> Vec<u8>` — prepends the header line to the header block, keeps existing ones
  - `forward::over_rate(sent_last_hour: u32, rate: u32) -> bool` (`sent_last_hour >= rate`)
  - `forward::log_action(forward: &Forward) -> String` — `forward:a@x.org`, `forward:a@x.org (+2)` counting `to`, `cc`, `bcc`
  - `forward::options(forward: &Forward) -> send::reply::ForwardOptions`
- [x] **Step 1: Write the failing tests:**
  - `a_header_with_our_address_is_a_loop`; `a_header_with_another_address_is_not` (Review Focus 3: then `with_loop_header` gives two headers, both present).
  - `three_headers_are_too_many_hops`; `auto_replied_is_skipped`; `auto_generated_newsletters_are_not_skipped` (`Auto-Submitted: auto-generated` → None).
  - `log_action_counts_the_others`.
  - `over_rate_at_the_limit`: `over_rate(20, 20)` true, `over_rate(19, 20)` false.
- [x] **Step 2: Run** `cargo nextest run -p postvak-core rules::forward` — FAIL.
- [x] **Step 3: Implement.**
- [x] **Step 4: Run** — PASS. Add `loop_check` to `tests/fuzz.rs`.
- [x] **Step 5: Commit** `feat(rules): loop, rate and log decisions for forwards`

### Task 4: Forwarding in the rules pass

**Files:**
- Modify: `crates/postvak-core/src/rules/apply.rs`, `crates/postvak-core/src/rules/engine.rs`, `crates/postvak-core/src/sync.rs`, `crates/postvak-core/src/engine.rs`, `crates/postvak-core/src/daemon/mod.rs`, `crates/postvak-core/src/notify.rs`

**Interfaces:**
- Consumes: Tasks 1–3; part 1 `send::reply::forward`, `send::outbox::Entry`, `send::sender::Senders::queue`, `send::server_for`
- Produces:
  - `rules::apply::ForwardQueue` trait: `fn queue(&self, account: &str, keep: Vec<u8>, entry: Entry) -> io::Result<()>`; `Senders` implements it; `RecordingQueue` (test) records calls and can be told to fail
  - `apply(...)` gains `queue: &dyn ForwardQueue` and `identity: &Identity`; before any other action, for each `Action::Forward` in the plan when `ctx.mode == Mode::Normal`: skip if `was_forwarded`; `loop_check` → skip (log `forward skipped: <log_text>`), other actions run; `over_rate` → withdraw approval, raise the notice once per rule per pass, other actions run; `fetch_raw` + `send::reply::forward` + `with_loop_header` + `send::check` + `queue` → on any error: `record_forward_failure`, return `Ok(0)` without running anything else for this message; on success: `record_forwarded`, `clear_forward_failure`, log row
  - `Mode::ApplyExisting`: `Action::Forward` dropped from the plan before `apply`
  - Rules waiting (not approved by `forward_approved(name, definition)`, or `server_for` None) are filtered out in `run_rules_with` before matching, so none of their actions run
  - `notify::forward_paused_notice(rule: &str, rate: u32) -> Notice`
  - `actions/undo.rs`: a `forward:` log row is not undone; the undo result names it (`The forward to a@x.org was already sent`) and undoes the rest
  - The daemon passes its `Senders` into each account thread (`Spawner` gains `queue: Arc<dyn ForwardQueue + Send + Sync>`)
- [ ] **Step 1: Write the failing tests** in `sync/tests.rs` with `RecordingOps` and `RecordingQueue`:
  - `a_forward_is_queued_before_the_delete_runs`: rule forward + delete; queue holds the raw; message expunged after.
  - `a_failed_fetch_runs_nothing_and_retries_next_pass` (Review Focus 1): `fetch_raw` errors once → not deleted, failure recorded; next pass → queued, deleted, failure cleared.
  - `an_unapproved_forward_rule_runs_nothing`; `a_rule_without_smtp_runs_nothing`.
  - `apply_existing_runs_the_other_actions_but_never_forwards`.
  - `a_message_is_forwarded_once_even_after_moving_back`.
  - `the_rate_limit_skips_the_forward_withdraws_and_notifies_once` (Review Focus 4): 25 matches in one pass, rate 20 → 20 queued, 25 archived, approval withdrawn, one notice.
  - `max_size_skip_runs_the_other_actions`; `max_size_strip_forwards_without_attachments`.
  - `a_forward_waits_with_after`.
  - `the_forward_is_due_now_and_carries_the_loop_header`.
  - In `actions/undo.rs` tests: `undo_of_a_forward_and_archive_moves_back_and_says_the_forward_was_sent`.
- [ ] **Step 2: Run** `cargo nextest run -p postvak-core sync::tests::forward` (name the tests with a `forward` prefix) — FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** — PASS; `cargo test -p postvak-core --features testing --test sync_model`.
- [ ] **Step 5: Commit** `feat(rules): forward matching new mail through the outbox`

### Task 5: Previews, tests and coverage

**Files:**
- Modify: `crates/postvak-core/src/rules/coverage.rs`, `crates/postvak-core/src/rules/engine.rs` (`explain`), `crates/postvak-core/src/output.rs`

**Interfaces:**
- Produces:
  - `Act.action` for a forward: `forward to a@x.org (+1) (new mail only)` in coverage and previews; `rules test` on a message shows `forward to a@x.org (+1)`
  - `postvak rules` (text) marks a waiting rule `waiting for approval` or `waiting: sending not set up`, and `rules check` reports a lasting forward failure
  - `output::rule` rows gain `forward_waiting: Vec<String>` (accounts where the rule's current definition is not approved, or that have no SMTP server); empty for a rule without forward
- [ ] **Step 1: Write the failing tests:** `coverage_shows_forward_as_new_mail_only`; `rule_rows_list_the_accounts_where_a_forward_waits`.
- [ ] **Step 2: Run** `cargo nextest run -p postvak-core coverage output` — FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** — PASS.
- [ ] **Step 5: Commit** `feat(rules): show forwards in previews, coverage and rule rows`

### Task 6: Approving

**Files:**
- Modify: `crates/postvak-core/src/rules/edit.rs`, `crates/postvak-mcp/src/backend.rs`, `crates/postvak-mcp/src/tools.rs`, `src/cli/mod.rs`, `crates/postvak-core/src/help.rs`, `crates/postvak-core/src/sync.rs` (waiting notices)

**Interfaces:**
- Consumes: `Store::approve_forward`, `CompiledRule.definition`
- Produces:
  - `rules::edit::approve_forward(paths: &Paths, name: &str, now: i64) -> Result<(), RulesError>` — compiles the rule as rules.toml holds it now, writes its definition into every configured account's store (`Store::approve_forward`) and restarts its clock there; refuses a rule without forward
  - `approve_from_now` keeps its behaviour and does not approve forwards; the CLI's `rules approve <name>` calls `approve_from_now` when the rule is a pending proposal and `approve_forward` when it has a forward (both when both); it no longer refuses an enabled rule that has an unapproved forward
  - MCP `approve`: unchanged path, result gains `"forward": "waits for approval in Postvak (postvak rules approve <name> or the Rules view)"` when the rule has a forward
  - On each rules load in the sync thread: a forward rule whose definition is not approved in this account and was not waiting before raises `notify::proposals_notice` with `Proposed::Rule(name)` (once per definition)
- [ ] **Step 1: Write the failing tests:**
  - `approve_forward_approves_every_account_and_restarts_the_clock`.
  - In `postvak-mcp/src/tests.rs`: `mcp_approve_leaves_a_forward_waiting`; `an_edit_proposal_approved_over_mcp_makes_the_forward_wait_again` (Review Focus 2: approve by human, propose edit widening `match`, MCP approve → `forward_waiting` lists the account, `run_rules` queues nothing).
  - `cli_rules_approve_approves_an_enabled_forward_rule` (CLI integration test file).
  - `a_new_waiting_forward_rule_notifies_once`; `an_account_added_later_waits`.
- [ ] **Step 2: Run** `cargo nextest run -p postvak-core edit && cargo nextest run -p postvak-mcp approve` — FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** — PASS; `POSTVAK_BLESS=1 cargo test`.
- [ ] **Step 5: Commit** `feat(rules): approve forward rules from the CLI; MCP cannot`

### Task 7: The GUI

**Files:**
- Modify: `crates/postvak-gui/src/draft.rs`, `crates/postvak-gui/src/rules.rs`, `crates/postvak-gui/src/state/rules_view.rs` (or the `rules_view` part of `state`), `crates/postvak-gui/ui/*.slint` (rule editor, rules view)
- Snapshots: `crates/postvak-gui/tests/snapshots/*.png`

**Interfaces:**
- Consumes: `Forward`, `rules::edit::approve_forward`, `forward_waiting`
- Produces:
  - `Draft` gains `forward: Option<ForwardDraft>` with fields mirroring `Forward` as text inputs; `ForwardDraft::to_forward(&self) -> Result<Forward, String>` with the same refusals as Task 1
  - The editor's From choice lists the account's address and named, non-wildcard aliases; disabled (account's own) when the rule is for every account
  - Saving a rule with a forward calls `approve_forward` after the rules.toml write succeeds
  - The Rules view shows `Waiting for approval · to books@example.org (+1)` on a waiting forward rule, with Approve (calls `approve_forward`) beside it; a rule waiting for SMTP shows `Waiting: sending not set up` without Approve; a lasting failure shows `Forward failing: <reason>`
  - `draft::summary` reads `forward to books@example.org`
- [ ] **Step 1: Write the failing tests:**
  - In `draft.rs`: `forward_draft_round_trips`; `forward_draft_refuses_like_the_rules_file`; `summary_names_the_first_recipient`.
  - In `rules.rs`: `a_waiting_forward_rule_offers_approve_with_its_addresses`; `a_rule_waiting_for_smtp_offers_no_approve`.
  - In the window tests (`Fixture::window()`): `saving_a_forward_rule_approves_it`.
- [ ] **Step 2: Run** `cargo nextest run -p postvak-gui draft rules` — FAIL.
- [ ] **Step 3: Implement**, using `Theme` sizes.
- [ ] **Step 4: Run** — PASS; `TZ=UTC UPDATE_SNAPSHOTS=1 cargo test -p postvak-gui snapshots`, look at the changed PNGs.
- [ ] **Step 5: Commit** `feat(gui): edit and approve forward rules`

### Task 8: Live test and docs

**Files:**
- Modify: `tests/smtp_live.rs` (from part 1), `docs/src/rules.md`, `docs/src/mcp.md`, `docs/src/sending.md`, `AGENTS.md` (module map: `rules/forward.rs`)

- [ ] **Step 1: Write** `a_forward_rule_reaches_the_sink_with_the_loop_header`: approve via `approve_forward`, deliver a matching message to Dovecot, run a sync pass, assert Mailpit received it with `X-Postvak-Forwarded` and `Fwd: ` subject.
- [ ] **Step 2: Run** `docker compose -f tests/dovecot/compose.yml up -d && POSTVAK_TEST_IMAP_HOST=localhost cargo test --features testing --test smtp_live forward` — PASS.
- [ ] **Step 3: Write the docs:** `rules.md` (the action and every option, approval and what resets it, new mail only, rate, loops, waiting states), `mcp.md` (what `rules:write` risks: writing and approving rules, and that forward rules stay held until approved in Postvak), `sending.md` (rule forwards in the outbox, no undo delay), AGENTS module map line for `rules` gains `forward.rs` (loop, rate, approval key decisions).
- [ ] **Step 4: Run** `cargo test` (README/index check) and `mdbook build docs` — PASS.
- [ ] **Step 5: Commit** `docs: forward rules`
