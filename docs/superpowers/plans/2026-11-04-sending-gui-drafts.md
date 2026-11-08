# GUI Drafts Implementation Plan (sending part 3, PR 2 of 3)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [x]`) syntax for tracking.

**Goal:** Drafts in the account's Drafts folder on the server: Save draft from the composer, opening a draft again (Postvak's own and other clients'), deleting the draft once its message is sent, Edit in the outbox dialog. Also the composer's bind code moves from two macros to an exported Slint global.

**Architecture:** Core builds a draft with part 1's functions, the same way `compose` builds a message, plus one `X-Postvak-Draft` header, and reads one back into fields. A new sync `Command::SaveDraft` APPENDs to Drafts and then expunges the copy it replaces, which it finds with `UID SEARCH HEADER Message-ID`. Sending a draft records its Message-ID in the outbox entry, and `FinishSent` deletes it once the message is `sent`. The GUI adds Save draft, the three-way leave question, opening drafts, and Edit in the outbox dialog.

**Tech Stack:** Rust, Slint 1.18.1, part 1's `send` module, `imap` via `MailOps`.

**Spec:** `docs/superpowers/specs/2026-11-03-sending-gui-design.md`, the section "Drafts", plus the Save draft parts of "The composer", "Leaving the composer" and "Sending and undo" (Edit in the outbox dialog).

**Prerequisite and branching:** PR #532 (`feat/gui-composer`, the composer) is in review. Stack this on it with `gh stack` (AGENTS.md "Stacked PRs"): `gh stack init` on `feat/gui-composer` if it is not a stack yet, then `gh stack add feat/gui-drafts`. Keep the branch linear with `gh stack sync`. Never merge `main` into it, and never rebase or force-push by hand.

## Global Constraints

- The definition of done from `AGENTS.md`: fmt, clippy with `-D warnings`, test, machete and deny. No `unwrap`, `expect` or `panic!` outside tests.
- Every draft is built by part 1's functions, the same path `send::compose::compose` uses. The GUI never builds or parses MIME itself.
- Blocking work (store reads, daemon calls, files) runs on worker threads, never on the window thread. Daemon work goes through `Command`s and their events, or injected `fn`s as in `state/composing.rs`.
- Never log recipients, subjects, bodies or draft contents. A log line names only the account and the Message-ID.
- The Drafts folder is the one with special-use `\Drafts`, else the one named `Drafts` as `folder_name::same` compares. Without one, Save draft is refused with exactly `This account has no Drafts folder`, and Postvak creates none.
- A draft is APPENDed with `\Draft` and `\Seen`. The copy it replaces is expunged (not moved to Trash), and only after the APPEND succeeded. Every save gets a new Message-ID.
- The header is `X-Postvak-Draft`. The kind is one of `reply`, `reply-all`, `reply-list`, `forward`, `forward-attachment`, `new`, and the original is named by its Message-ID.
- Composer names start with `Compose`/`compose`. The rule editor already owns `Draft`, `SaveDraft`, `EditDraft` and `Dialog::Unsaved`. In the GUI, the server-side draft is a `ComposeDraft`, and the UI actions are `ComposeSaveDraft` and `EditComposeDraft`.
- Keys: `Ctrl+S` (`Cmd+S`) saves a draft in the composer. After GUI work: `TZ=UTC UPDATE_SNAPSHOTS=1 cargo test -p postvak-gui snapshots`, then look at every changed PNG.
- Report "compiled on" vs "ran on" honestly.

## Review Focus

1. **Save twice quickly, or save while the first save is still in flight**: Drafts ends with exactly one copy, the newest. A second save waits for the first answer, because it must replace the Message-ID that save produced. Test in Task 6.
2. **The APPEND fails (quota, offline)**: the old copy stays, the composer stays open with the error, and the recovery file still holds the text. Test in Task 4 (core) and Task 6 (GUI).
3. **A draft edited in another client** (the text below the user's text no longer matches the quote Postvak would build): it opens as a foreign draft, whole text in the editor, and nothing is dropped. Test in Task 3.
4. **Sending a draft and then Undo**: the draft is still in Drafts, and the reopened composer still points at it, so a later save replaces it rather than adding a second copy. Test in Task 6.
5. **A draft whose message was sent, but the delete fails** (a lost connection after SMTP accepted it): `FinishSent` retries the delete like the Sent copy, until it succeeds or the draft is gone. Test in Task 5.

---

### Task 1: The composer's bind code as a Slint global

**Files:**
- Modify: `crates/postvak-gui/src/ui/compose.slint`, `crates/postvak-gui/src/ui/window.slint`, `crates/postvak-gui/src/bind.rs`, `crates/postvak-gui/src/lib.rs` (if it calls the macros)

**Interfaces:**
- Produces:
  - `export global ComposeState` in `ui/compose.slint`. It holds every property and callback that `MailWindow` and `ComposeWindow` repeat today for the composer (the fields, choices, file and part rows, completion, error spot, `can-send`/`fixed`, and the edit, send, close, discard, attach, remove, toggle and pick callbacks).
  - `Composer` binds to `ComposeState` rather than to properties passed down. Each window instance has its own copy of the global, as Slint globals are per component instance.
  - In `bind.rs`, `show_composer!` and `connect_composer!` become `fn show_composer(state: &ComposeState, composer: Option<&Composer>, …)` and `fn connect_composer(state: &ComposeState, target: ComposeTarget, ask: …)`. Both windows call them with `window.global::<ComposeState>()`.
  - The duplicated composer property and callback lists in `MailWindow` and `ComposeWindow` go away.
- [x] **Step 1: Run** `TZ=UTC cargo test -p postvak-gui snapshots` and `cargo nextest run -p postvak-gui` and record both passing: this is a refactor, so the existing tests are the net.
- [x] **Step 2: Implement.** No behaviour changes. `macro_rules!` no longer appears in `bind.rs` for the composer.
- [x] **Step 3: Run** the same two commands. Every test passes and no PNG changes (`git status tests/snapshots` is clean).
- [x] **Step 4: Commit** `refactor(gui): one ComposeState global for the composer in both windows`

### Task 2: Finding drafts on the server

**Files:**
- Create: `crates/postvak-core/src/send/drafts.rs` (and `pub mod drafts;` in `send.rs`)
- Modify: `crates/postvak-core/src/mail_ops.rs`, `crates/postvak-core/src/mail_ops/imap.rs`, `tests/imap_live.rs`

**Interfaces:**
- Produces:
  - `drafts::drafts_folder(folders: &[Folder]) -> Option<String>`: special-use `Drafts`, else `folder_name::same(name, "Drafts")`. Same shape as `sent::sent_folder`.
  - `MailOps::search_header(&mut self, header: &str, value: &str) -> MailResult<Vec<u32>>`: `UID SEARCH HEADER <header> <value>` in the selected folder. The value is quoted as the imap crate quotes strings. `RecordingOps` answers from its stored messages' raw headers, comparing case-insensitively without angle brackets.
- [x] **Step 1: Write the failing tests:** `drafts_folder_prefers_special_use_then_the_name` (in drafts.rs), and in `mail_ops.rs` `recording_ops_finds_a_message_by_its_message_id`. In `tests/imap_live.rs`, `search_header_finds_an_appended_message` runs on both servers, 10993 and 11993.
- [x] **Step 2: Run** `cargo nextest run -p postvak-core drafts mail_ops` and expect FAIL.
- [x] **Step 3: Implement.**
- [x] **Step 4: Run** the same command and expect PASS. Then run the live tests: `docker compose -f tests/dovecot/compose.yml up -d && POSTVAK_TEST_IMAP_HOST=localhost cargo test --features testing --test imap_live search_header`.
- [x] **Step 5: Commit** `feat(mail): find a message by a header, and the Drafts folder`

### Task 3: Building a draft and reading one back

**Files:**
- Modify: `crates/postvak-core/src/send/drafts.rs`, `crates/postvak-core/src/send/compose.rs`, `crates/postvak-core/src/send/build.rs`

**Interfaces:**
- Consumes: Task 1 of PR 1's `SendRequest`, plus `prefill`, `draft_for` and `apply_headers` in compose.rs.
- Produces:
  - `build::Draft` gains `postvak_draft: Option<String>`, written as the `X-Postvak-Draft` header when set.
  - `pub enum DraftKind { New, Reply(ReplyTo), Forward(ForwardAs) }`. The header value is `<kind>; original="<message-id>"; parts="0,2"`, where `parts` appears only for an inline forward that drops some of the original's attachments. The kind words are the Global Constraints'.
  - `drafts::build(config: &Config, paths: &Paths, client: &LazyClient, request: &SendRequest) -> anyhow::Result<Vec<u8>>` builds the message exactly as `compose` would, minus the checks a draft may fail (no recipient yet, too large) and minus the queueing. It gets a fresh Message-ID and the header. `compose` and `drafts::build` share one function for everything up to `build::build`.
  - `drafts::read(raw: &[u8], find_original: impl Fn(&str) -> Option<Vec<u8>>) -> anyhow::Result<OpenedDraft>`, with `pub struct OpenedDraft { pub kind: DraftKind, pub original: Option<Vec<u8>>, pub from: String, pub to: Vec<String>, pub cc: Vec<String>, pub bcc: Vec<String>, pub subject: String, pub text: String, pub files: Vec<(String, Vec<u8>)>, pub forward_parts: Option<Vec<usize>>, pub in_reply_to: Option<String>, pub references: Vec<String>, pub note: Option<&'static str>, pub message_id: String }`.
  - Reading a Postvak draft: if the original is found, take the decoded text part and remove the quote, attribution or forward block that `draft_for(original, body = "")` would make for that kind. The rest is the user's text. If that suffix is not where it should be, or the original is not found, read it as a foreign draft (my call: rebuilding the quote and matching it tells "edited in another client" apart from "as Postvak wrote it", with no offsets to go stale).
  - Reading a foreign draft: the whole text part goes in `text`. Failing that, the HTML part converted to text with `note = Some("This draft had only HTML; it opens as text.")`. Every attachment goes in `files`. `kind` is `Reply(Sender)` with `original` found by In-Reply-To when there is one and the original is in the store, else `New` keeping In-Reply-To and References.
- [x] **Step 1: Write the failing tests** in drafts.rs, with fixtures from `send::reply`'s tests:
  - `a_reply_draft_reads_back_its_text_and_kind` (each `ReplyTo`)
  - `a_forward_draft_reads_back_the_parts_kept`
  - `a_forward_as_attachment_draft_reads_back`
  - `a_draft_edited_in_another_client_opens_whole` (Review Focus 3)
  - `a_draft_whose_original_is_gone_opens_whole_keeping_the_thread`
  - `a_foreign_draft_opens_with_its_text_and_attachments`
  - `an_html_only_draft_opens_as_text_with_the_note`
  - `each_build_has_a_new_message_id`
  - `a_draft_without_recipients_builds`
  - `the_draft_is_the_message_compose_would_send` (same bytes as `compose` would queue for the same request, with Date, Message-ID, boundaries and `X-Postvak-Draft` aside)
- [x] **Step 2: Run** `cargo nextest run -p postvak-core send::drafts` and expect FAIL.
- [x] **Step 3: Implement.**
- [x] **Step 4: Run** `cargo nextest run -p postvak-core send` and expect PASS.
- [x] **Step 5: Commit** `feat(send): build a draft as the message would be sent, and read one back`

### Task 4: Saving and deleting drafts through the daemon

**Files:**
- Modify: `crates/postvak-core/src/sync.rs` (`Command`, `Event`, the command handler), `crates/postvak-core/src/send/drafts.rs`, `tests/imap_live.rs`

**Interfaces:**
- Produces:
  - `Command::SaveDraft { raw: Option<Vec<u8>> /* base64 on the wire, as FinishSent */, replaces: Option<String> }`. With `raw`, it APPENDs to Drafts with `\Draft` and `\Seen`; then, if `replaces` is set, it selects Drafts, runs `search_header("Message-ID", replaces)` and expunges what it finds (`mail_ops::delete_messages`). Without `raw`, it only deletes `replaces`.
  - `Event::DraftSaved { account, message_id: Option<String>, request: RequestId }`, where `message_id` is the new copy's ID (None for a delete). Refusals come back as `CommandFailed`: no Drafts folder, with exactly `This account has no Drafts folder`; a failed APPEND, with the server's reason. A delete that finds nothing is not an error.
  - `drafts::save(ops, store, account, raw: Option<&[u8]>, replaces: Option<&str>) -> Result<Option<String>, SaveDraftError>` is the pure part, used by the handler and its tests.
- [x] **Step 1: Write the failing tests** with `RecordingOps`:
  - `saving_appends_with_the_draft_flags`
  - `saving_again_deletes_the_copy_it_replaces_after_the_append`
  - `a_failed_append_keeps_the_old_copy` (Review Focus 2)
  - `no_drafts_folder_is_refused_with_the_words`
  - `deleting_only_removes_the_copy`
  - `deleting_a_draft_that_is_gone_is_fine`
  - Live: `save_draft_replaces_its_copy` on 10993 and 11993 (with UIDPLUS and without).
- [x] **Step 2: Run** `cargo nextest run -p postvak-core drafts sync::tests::draft` and expect FAIL.
- [x] **Step 3: Implement.**
- [x] **Step 4: Run** the same command and the live tests, and expect PASS.
- [x] **Step 5: Commit** `feat(sync): save a draft to Drafts, replacing its earlier copy`

### Task 5: A sent draft leaves Drafts

**Files:**
- Modify: `crates/postvak-core/src/send/compose.rs` (`SendRequest.draft`), `crates/postvak-core/src/send/outbox.rs` (`Entry`), `crates/postvak-core/src/send/sent.rs` (`Finish`, `finish`), `crates/postvak-core/src/sync.rs` (`FinishSent`, `SentFinished`), `crates/postvak-core/src/send/sender.rs` (resending `FinishSent` until done), `src/cli/mod.rs` (struct literal)

**Interfaces:**
- Produces:
  - `SendRequest.draft: Option<String>` (the draft's Message-ID).
  - `Entry.draft: Option<String>` and `Entry.draft_deleted: bool`, both `#[serde(default)]`.
  - `Finish` and `FinishSent` gain `draft: Option<String>` and `draft_deleted: bool`; `SentFinished` gains `draft_deleted`.
  - `finish` deletes the draft after the copy and the flag, with the same rules: a lost connection is an error, so the job is retried; any other refusal is logged by Message-ID and given up. The daemon resends `FinishSent` until all three are done, as it does today for two.
  - The CLI passes `draft: None`.
- [x] **Step 1: Write the failing tests:**
  - `a_sent_draft_is_deleted_from_drafts`
  - `a_lost_connection_while_deleting_the_draft_tries_again` (Review Focus 5)
  - `a_cancelled_or_failed_message_leaves_its_draft` (the outbox paths never touch Drafts before `sent`)
  - `an_old_outbox_entry_without_draft_fields_reads`
- [x] **Step 2: Run** `cargo nextest run -p postvak-core send::sent send::outbox sync::tests::finish` and expect FAIL.
- [x] **Step 3: Implement.**
- [x] **Step 4: Run** `cargo nextest run -p postvak-core` and expect PASS.
- [x] **Step 5: Commit** `feat(send): a draft leaves Drafts once its message is sent`

### Task 6: Save draft in the composer

**Files:**
- Modify: `crates/postvak-gui/src/compose.rs`, `crates/postvak-gui/src/state/composing.rs`, `crates/postvak-gui/src/state/mod.rs` (the event arm), `crates/postvak-gui/src/ui/compose.slint`, `crates/postvak-gui/src/bind.rs`, `crates/postvak-gui/src/keys.rs` (HELP)

**Interfaces:**
- Consumes: Task 3 `drafts::build`; Task 4 `Command::SaveDraft` and `Event::DraftSaved`; Task 5 `SendRequest.draft`; Task 1 `ComposeState`.
- Produces:
  - `Composer.draft: Option<ComposeDraft>`, with `pub(crate) struct ComposeDraft { pub message_id: String }`: the copy on the server now. It is kept in the recovery file.
  - `Composer.saving: bool`.
  - `UiAction::ComposeSaveDraft(ComposeTarget)` comes from the Save draft button and `Ctrl+S`. It builds the draft with `drafts::build` on a worker through an injected `BuildDraftFn` (`fn(&Paths, &Config, SendRequest) -> Result<Vec<u8>, String>`), then sends `Command::SaveDraft { raw, replaces: composer.draft }`. On `DraftSaved` it sets `draft`, clears `saving`, sets `opened_as` to the current fields (so `changed()` is false), and shows the toast `Draft saved`.
  - A save asked for while `saving` waits and runs once the answer comes (Review Focus 1). A `CommandFailed` puts the error on the composer, keeps `draft` as it was, and leaves the recovery file (Review Focus 2). The GUI matches the answer to its command by request id, as the folder dialog does.
  - The leave question becomes `Save draft`, `Discard`, `Cancel`. Save draft saves, then runs the pending action once `DraftSaved` comes. If the save fails, the composer stays open.
  - `Composer::request()` sets `draft: self.draft.map(|d| d.message_id)`, so a sent draft leaves Drafts. After Undo, the reopened composer keeps `draft` (Review Focus 4).
  - Discard of a composer with a `draft` asks `Delete this draft?`, then sends `SaveDraft { raw: None, replaces }`. Close leaves the draft.
- [x] **Step 1: Write the failing tests:**
  - `save_draft_sends_the_built_draft_and_marks_it_saved`
  - `a_second_save_replaces_the_first_copy`
  - `a_save_while_one_is_in_flight_waits_for_it` (Review Focus 1)
  - `a_refused_save_keeps_the_composer_and_the_text` (Review Focus 2)
  - `the_leave_question_offers_save_draft_and_goes_on_after_it`
  - `sending_a_saved_draft_names_it_and_undo_keeps_it` (Review Focus 4)
  - `discarding_a_saved_draft_asks_and_deletes_it`
  - `close_leaves_a_saved_draft`
  - `ctrl_s_saves`
- [x] **Step 2: Run** `cargo nextest run -p postvak-gui composing` and expect FAIL.
- [x] **Step 3: Implement.**
- [x] **Step 4: Run** `TZ=UTC UPDATE_SNAPSHOTS=1 cargo test -p postvak-gui snapshots`, add a snapshot `compose_leave_question_with_save_draft`, and look at it. Then run `cargo nextest run -p postvak-gui` and expect PASS.
- [x] **Step 5: Commit** `feat(gui): save a message as a draft in the server's Drafts`

### Task 7: Opening a draft

**Files:**
- Modify: `crates/postvak-gui/src/compose.rs` (`Composer::from_draft`), `crates/postvak-gui/src/state/composing.rs`, `crates/postvak-gui/src/state/opened.rs`, `crates/postvak-gui/src/body.rs`, `crates/postvak-gui/src/toolbar.rs`, `crates/postvak-gui/src/keys.rs`, `crates/postvak-gui/src/ui/body.slint`

**Interfaces:**
- Consumes: Task 3 `drafts::read` and `OpenedDraft`; Task 2 `drafts_folder`.
- Produces:
  - `Composer::from_draft(id, account: &str, identities: &[(String, Identity)], opened: OpenedDraft, files_dir: &Path) -> io::Result<Composer>`. The draft's own attachments are written to `<state_dir>/compose/<id>/files/<name>` (names made safe and unique) and listed as `files` with their sizes. The forwarded parts kept come from `forward_parts`. `draft` is `Some(ComposeDraft { message_id })`. A `note` shows on `Field::Note`. The files directory is removed with the recovery file.
  - `UiAction::EditComposeDraft` comes from an `Edit draft` button in the reader's toolbar, shown when the open message's folder is its account's Drafts folder, and from Enter on such a row in the list. It reads the raw message, which `BodyState` already holds once it is downloaded (else it waits for it, as Save .eml does). `drafts::read` runs on a worker, with `find_original` looking the Message-ID up in the account's store (`latest_by_message_id`, then the raw from the store or `FetchBody`). The result opens a reply or forward in the pane over its original, or new mail in a window.
  - Opening a draft that is already open in a composer brings that composer forward instead.
- [x] **Step 1: Write the failing tests:**
  - `edit_draft_shows_only_in_drafts`
  - `editing_a_reply_draft_opens_it_over_its_original`
  - `editing_a_new_mail_draft_opens_a_window`
  - `a_foreign_draft_opens_whole_with_its_files`
  - `the_draft_files_go_with_the_composer`
  - `enter_on_a_draft_row_edits_it`
  - `opening_a_draft_twice_shows_the_open_composer`
- [x] **Step 2: Run** `cargo nextest run -p postvak-gui edit_draft draft` and expect FAIL.
- [x] **Step 3: Implement.**
- [x] **Step 4: Run** `TZ=UTC UPDATE_SNAPSHOTS=1 cargo test -p postvak-gui snapshots`, add a snapshot `reader_draft_edit_button`, and look at it. Then run `cargo nextest run -p postvak-gui` and expect PASS.
- [x] **Step 5: Commit** `feat(gui): open a draft again from Drafts`

### Task 8: Edit in the outbox dialog

**Files:**
- Modify: `crates/postvak-gui/src/state/composing.rs`, `crates/postvak-gui/src/status.rs`, `crates/postvak-gui/src/ui/dialogs.slint`, `crates/postvak-gui/src/bind.rs`

**Interfaces:**
- Consumes: the outbox dialog (PR 1); `Outbox::get` (the entry's raw message from disk); `Client::cancel_send`; Task 4 `SaveDraft`; Task 7 opening.
- Produces: a failed entry's `Edit` button (`UiAction::EditOutbox(usize, String)`), which goes in this order, so the message is never lost:
  1. On a worker, read the entry's raw from the outbox (`Outbox::get`).
  2. Send `SaveDraft { raw, replaces: Entry.draft }`.
  3. Only on `DraftSaved`, cancel the entry with the injected `CancelFn`, then open the new draft as Task 7 does.
  If the save is refused, the entry stays failed in the outbox and the dialog shows why. If the cancel is refused after the save, the draft is opened anyway and the dialog still lists the failed entry.
- [x] **Step 1: Write the failing tests:** `edit_turns_a_failed_message_into_a_draft_and_opens_it`, `a_refused_draft_save_leaves_the_failed_message_in_the_outbox`.
- [x] **Step 2: Run** `cargo nextest run -p postvak-gui outbox` and expect FAIL.
- [x] **Step 3: Implement.**
- [x] **Step 4: Run** `TZ=UTC UPDATE_SNAPSHOTS=1 cargo test -p postvak-gui snapshots` (the `outbox_dialog` snapshot gains Edit; look at it). Then run `cargo nextest run -p postvak-gui` and expect PASS.
- [x] **Step 5: Commit** `feat(gui): edit a failed message from the outbox as a draft`

### Task 9: Docs and the full check

**Files:**
- Modify: `docs/src/gui.md` (Save draft, the leave question, Ctrl+S, opening drafts, Edit in the outbox; drop "Saving a message as a draft comes later"), `docs/src/sending.md` (drafts live in the server's Drafts folder), `docs/src/compare.md` (drop the "cannot save a message as a draft yet" clause), `AGENTS.md` (`send` gains `drafts`; the `gui` `compose` entry mentions drafts), and this plan (tick the boxes)

- [x] **Step 1: Write the docs.**
- [x] **Step 2: Run the definition of done:** `cargo fmt --check && cargo clippy --all-targets --all-features -- -D warnings && cargo nextest run --workspace --all-features && cargo test --doc --workspace && cargo machete && cargo deny check`, the live IMAP tests, and `TZ=UTC UPDATE_SNAPSHOTS=1 cargo test -p postvak-gui snapshots` with `git status` clean afterwards.
- [x] **Step 3: Commit** `docs: drafts`
