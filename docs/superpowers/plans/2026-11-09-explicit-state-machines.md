# Explicit state machines

An audit of the lifecycles the code keeps in loose flags, `Option`s and counters, where some combinations cannot
happen but the types allow them. Each entry says what it is, what was done, or why it waits.

The rule this follows: a lifecycle gets an enum when two or more fields only make sense together, or when one field's
meaning depends on another. Flags that are truly independent stay flags. Anything written to disk, to the daemon's wire
or to `--json`/MCP keeps its serialized shape; the enum lives in memory and maps onto it.

## Done on this branch

| Where | Was | Now | Why |
|---|---|---|---|
| `gui` `App` | `daemon_lost: bool`, `reconnecting: Option<Receiver>`, `last_reconnect: f64` | `DaemonLink { Connected, Lost { last_attempt }, Reconnecting { started, attempt } }` | An attempt while connected, or a timestamp read while connected, are gone from the type. |
| `gui` `BodyState` | `armed: bool`, `read_sent: bool` | `ReadMark { Passive, Armed, Settled }` | Four combinations, three behaviours; `arm()` cannot undo a settle. |
| `gui` `App` | `list_loading: Option<Loading>`, `list_again: bool` | `Loading { again, .. }` inside the `Option` | "Another reload follows" without one running is no longer representable. |
| `gui` `ListState` | `server_searching: usize` | `server_searching: HashSet<usize>` | A count could not tell a real answer from a copy. **Bug fixed:** another client's answer for the same query, or any failed command, ended a running account's search early and doubled hits. Test: `a_server_search_ends_only_when_each_asked_account_answered_once`. |
| `sync` `run_session` | `full: bool`, `pass: bool`, `next: NextPass` | `due: NextPass`, `after: NextPass` | `full` without `pass` was representable and left stale after an interrupt; the loop now speaks the same enum commands already return. |
| `send::sender` | entry read under the lock, then `failed()` relocked to write it | `due_entry` and `record_failure` take the `MutexGuard` | **Bug fixed:** a cancel or retry between the read and the write was overwritten, bringing a cancelled message's entry back. The guard argument makes the race a compile error. The window has no user code in it, so no test can hit it deterministically. |
| `rules::apply` `forward_first` | a forward failure row cleared only by a queued forward | also cleared when the message is skipped (loop, `max_size`) | **Bug fixed:** a message held once, then skipped, kept its failure forever, so the rule's warning said its actions still waited. Test: `a_skip_after_a_hold_clears_the_failure`. |

## Next, ranked by value against risk

Each is its own PR: they touch serialized state or many sites, and each deserves its own review.

1. **Composer phase** (`gui/compose.rs` `Composer`): `loading`, `sending`, `saving` plus `App.compose_prefill`,
   `compose_sending`, `compose_saves`. States: Loading, LoadFailed, Idle, Saving { again, then }, Sending. Seven sites
   guard by hand against two being true at once. Risk: `sending` is in the recovery JSON; keep it serialized as a bool
   derived from the phase so old recovery files still warn "may have gone".
2. **Window request ids** (`gui` `Event::CommandFailed` handler). A refusal of this window's own command does not say
   which command, so it clears the folder dialog's wait, answers an undo and ends a server search on the same account,
   whatever failed. **Known bug:** a body fetch failing during a folder rename leaves the dialog open after the rename
   succeeds. An enum cannot fix this; `daemon::Client` needs to correlate its own requests. Then `FolderDialog`'s
   `error`/`waiting` becomes `Editing { error } | Waiting { about }`.
3. **Outbox entry** (`send/outbox.rs`): `Failed { retrying: bool }` is matched at about eight sites as "retrying";
   `copied`, `flagged`, `draft_deleted` only mean something while `Sent`; `due` means two things. Proposed: `Retrying`
   as its own state and the three owed steps inside `Sent`. Risk: the on-disk `.json` and `Command::FinishSent`; needs
   a deserializer that reads existing outbox files. **Latent bug:** a `Sent` entry whose `.eml` is gone never finishes,
   and keeps the daemon from exiting when idle.
4. **Engine slots** (`engine.rs`): `threads`, `wanted`, `stopping` encode Running, SpawnFailed, WaitingForOldThread and
   Draining. A spawn-failed account stays listed by `Engine::accounts()` on purpose (its `NotRunning` reason shows), but
   that also lets `account::await_started` answer Ok for a thread that never started; worth deciding explicitly. Proposed: `enum Slot` and an `AccountRun` answer replacing the daemon's
   `(running, restarting)` bool pair.
5. **Undo gestures** (`gui/undo.rs` `Taken`): one struct for three kinds (action, history undo, sent message), with
   `done.is_empty()` as a sentinel. **Bug:** Send now finds its message through `taken.last()`, so a gesture taken after
   the countdown (e.g. while the daemon is lost) makes it do nothing. Proposed: `enum Gesture`, and a countdown `Toast`
   variant carrying its gesture id.
6. **IMAP selection** (`mail_ops/imap.rs`, `sync/index.rs`, `sync.rs`): which folder is selected is tracked in four
   places kept equal by hand (`forget_selection`, twelve `used_connection = true`). Proposed: `MailOps::selection()`
   with an epoch bumped by every SELECT and reconnect.
7. **Forward-once table** (`migrations/028-forward`): `forwarded` and `forward_failures` together encode Unseen, Held,
   Forwarded. Failure rows of deleted mail or renamed rules are never pruned, and queue-then-record is not atomic.
   Needs a migration.
8. **Rule editor** (`gui` `App.rule_draft`, `rule_draft_opened`, `rule_draft_stale`): one `Option<RuleEditor>`.
   Correct today, since `open_draft` resets all three; about 40 sites to touch for clarity alone.
9. **Smaller**: `AccountForm { saving, testing }` and the first-run `Save { tested }` as one `SaveProgress`; the HTML
   view's `shown`/`slow`/`started` as `Appearance`; the daemon hub's three per-account maps as one, reset on restart
   (a stale "login rejected" refusal right after fixing the password); `OpenedDraft`'s original fields; `UndoOutcome`'s
   placeholder `Failed`; the page cursor's search-only fields.

## Looked at and left alone

- The GUI's dialogs (`help_open`, `folder_dialog`, `account_form`, …): several are open at once by design (Unsaved over
  Rule), ordered by `Dialog::TOP_FIRST`; one enum would lose that.
- Completion's `completing` and `completion`: both set is valid, the old list stays while a query runs.
- MCP tool arguments (`dry_run` + `yes`, `rule` xor `list`): a public JSON schema, and hosts refuse a top-level `anyOf`.
- Already explicit: `Activity`, `SendingStatus`, `UpdateCheck`, `OpenAtLogin`, `instance::Claim`, `Reader`, `FolderKind`.
