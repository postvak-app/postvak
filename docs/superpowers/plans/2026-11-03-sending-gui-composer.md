# GUI Composer Implementation Plan (sending part 3, PR 1 of 3)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Reply, reply all, reply to list, forward and new mail from the window, sent through part 1's outbox, with the original drawn below the reply, the undo toast, the pending mark, address completion and the outbox dialog.

**Architecture:** The GUI builds nothing itself: `send::compose::compose` gains a preloaded original and header overrides, and the composer hands it a `SendRequest` on a worker thread through an injected `fn`, as `check_login` is injected today. A toolkit-free `compose` module in `postvak-gui` holds the composer's state, checks and questions; `state` owns one composer in the reader pane (above the existing body view, which already draws the original) and any number of new-mail windows; `ui/compose.slint` draws both.

**Tech Stack:** Rust, Slint 1.18.1 (`TextEdit`, `LineEdit`, a second `Window` component), part 1's `send` module, the FileChooser portal (`portal`) and `NSOpenPanel` (`objc2-app-kit`, already a dependency).

**Spec:** `docs/superpowers/specs/2026-11-03-sending-gui-design.md`. This PR covers: The composer (all of it), Leaving the composer (without Save draft), Sending and undo (without Edit in the outbox dialog), Keys, and their docs and tests. PR 2 (drafts) and PR 3 (signatures, Preferences › Sending, the add-account SMTP line) get their own plans once this one is in review.

**Prerequisite:** parts 1 and 2 are merged (`af340515`).

## Global Constraints

- The definition of done from `AGENTS.md`: fmt, clippy with `-D warnings`, test, machete and deny all pass. No `unwrap`, `expect` or `panic!` outside tests.
- Every message goes through `send::compose::compose`, with the same `build`, `reply`, `check` and `queue` as `postvak send`. The GUI never builds or checks MIME itself.
- Front ends only call `postvak-core` and `daemon::Client`. Blocking daemon calls (`queue_send`, `cancel_send`, `retry_send`, prefill's `FetchBody`) run on `worker::oneshot` threads, never on the window thread.
- Composer names start with `Compose`/`compose`, because the rule editor already owns `Draft`, `SaveDraft`, `EditDraft` and `Dialog::Unsaved`.
- Never log recipients, subjects or bodies, including in the recovery files' error paths.
- The layout grid comes from `ui/theme.slint` (`row`, `control`, `inset`, `pad`, the `dialog*` widths, `Hairline`); no literal sizes.
- Keys: `r` reply, `Shift+R` reply all, `l` reply to list, `f` forward, `Ctrl+N` (`Cmd+N`) new mail. In the composer: `Ctrl+Enter` send, `Escape` close. `Ctrl+R` stays Sync now.
- After GUI work: `TZ=UTC UPDATE_SNAPSHOTS=1 cargo test -p postvak-gui snapshots`, and look at every changed PNG.
- Report "compiled on" vs "ran on" honestly. macOS code (`NSOpenPanel`) is compiled only in CI from Linux.

## Review Focus

1. **Reply to a large message whose body is not downloaded yet**: the composer opens at once and shows "Loading the message…". Its fields fill once prefill's `FetchBody` returns, and Send is disabled until then. Test in Task 5.
2. **A rule moves the original while the reply is being written**: Send still works, because the composer holds the original's bytes from prefill. The flag goes on the original by Message-ID, as part 1 does. Test in Task 1.
3. **The daemon is gone when Send is pressed**: the composer stays open with everything typed, the error shows in the composer, and nothing is queued. Test in Task 5.
4. **Undo after the message already left `waiting`**: the toast says "Already on its way" and no composer reopens. Test in Task 5.
5. **Ctrl+Enter pressed twice quickly**: one message is queued. Send is ignored while one is in flight. Test in Task 5.

---

### Task 1: One send path with a preloaded original and edited headers

**Files:**
- Modify: `crates/postvak-core/src/send/compose.rs`, `crates/postvak-core/src/send/reply.rs`, `src/cli/mod.rs` (struct literal only)
- Test: the `tests` modules of both core files

**Interfaces:**
- Produces:
  - `SendRequest` gains `original: Option<Vec<u8>>` (preloaded raw; `None` means look it up by folder and uid as now) and `headers: Option<HeaderEdits>`.
  - `pub struct HeaderEdits { pub from: String, pub to: Vec<String>, pub cc: Vec<String>, pub bcc: Vec<String>, pub subject: String }`. Applied to the built `Draft` after `draft_for`, replacing what the builder chose. `from` goes through `check::check_from` and `display_name_for`, as `--from` does today.
  - `SendRequest` gains `forward_parts: Option<Vec<usize>>`, which becomes `ForwardOptions.attachments = AttachmentFilter::Only(parts)`.
  - `AttachmentFilter::Only(Vec<usize>)`: the indices into the original's attachment list (the order `reply::forward` re-attaches them in) that go along. Drop the `Copy` derive. `All` and `None` mean what they do today, and part 2's rule forward is unchanged.
  - `pub fn prefill(config: &Config, paths: &Paths, client: &LazyClient, account: Option<&str>, kind: &SendKind) -> anyhow::Result<Prefilled>`, with `pub struct Prefilled { pub account: String, pub draft: Draft, pub original: Option<Vec<u8>>, pub attachments: Vec<(String, u64)> }`. It returns the draft `draft_for` makes with an empty body, the raw original it read, and the original's attachments as (file name, size) in `Only`'s index order.
  - `pub fn forward_attachments(original: &[u8]) -> Vec<(String, u64)>` in `reply.rs`, used by `prefill`.

- [x] **Step 1: Write the failing tests** in `send/compose.rs`:
  - `edited_headers_replace_the_builders`: a reply request with `headers = Some(HeaderEdits { to: ["c@x.org"], subject: "New", .. })` and `original = Some(fixture)`. Its draft has `to == ["c@x.org"]` and `subject == "New"`, and keeps `in_reply_to` and the quote.
  - `a_preloaded_original_is_used_without_the_store`: a reply request with `original = Some(fixture)` and a folder/uid that are not in the store. It builds, and the `Original.message_id` is the fixture's (Review Focus 2).
  - `without_edits_the_request_is_the_clis`: the same request with `headers = None` gives the same draft as before this change.
  - `prefill_has_the_recipients_and_an_empty_text_above_the_quote`.
  - In `reply.rs`: `only_the_listed_attachments_go_along` (a two-attachment fixture with `Only(vec![1])` attaches the second only), and `forward_attachments_lists_names_and_sizes_in_order`.
- [x] **Step 2: Run** `cargo nextest run -p postvak-core send::compose send::reply` and expect FAIL.
- [x] **Step 3: Implement.** Split `compose` so that the original's lookup is skipped when `request.original` is set. Apply `headers` after the editor step (the GUI never uses `Body::Edit`). Build `prefill` from the existing `original_for` and `draft_for`. Add `original: None, headers: None, forward_parts: None` to the CLI's `SendRequest`.
- [x] **Step 4: Run** `cargo nextest run -p postvak-core send rules::forward` and expect PASS (part 2 untouched).
- [x] **Step 5: Commit** `feat(send): a preloaded original, edited headers and chosen attachments in a send request`

### Task 2: Addresses from the user's own mail

**Files:**
- Modify: `crates/postvak-core/src/store.rs`
- Test: `store` tests

**Interfaces:**
- Produces: `Store::addresses(&self, prefix: &str, limit: usize) -> Result<Vec<KnownAddress>, StoreError>`, with `pub struct KnownAddress { pub name: Option<String>, pub address: String, pub latest: i64, pub written_to: bool }`.
  - Matching: the prefix, case-insensitive, at the start of the address or of any word of the name.
  - Rows the user wrote to come first: the `to_addr`/`cc_addr` of messages in the folder whose special-use is `\Sent`, else the one `folder_name` calls Sent.
  - The rest come from `from_addr`. Each group is ordered by its latest `date`, descending, and an address shows once.
  - Parse the header columns with `message::mailboxes`.
- [x] **Step 1: Write the failing tests:** `addresses_put_people_written_to_first`, `addresses_match_a_word_of_the_name_and_ignore_case`, `addresses_show_once_with_their_latest_date`, `addresses_respect_the_limit`.
- [x] **Step 2: Run** `cargo nextest run -p postvak-core store::tests::addresses` and expect FAIL.
- [x] **Step 3: Implement** with one SQL query per group, limited to `limit * 4` candidate rows before deduplication (my call: a prefix of one letter on a large store must stay fast; the window asks again per keystroke).
- [x] **Step 4: Run** the same command and expect PASS.
- [x] **Step 5: Commit** `feat(store): addresses from the user's own mail, for completion`

### Task 3: `mailto:` links

**Files:**
- Create: `crates/postvak-core/src/send/mailto.rs` (and `pub mod mailto;` in `send/mod.rs`)

**Interfaces:**
- Produces: `pub fn parse(url: &str) -> Option<Mailto>`, with `pub struct Mailto { pub to: Vec<String>, pub cc: Vec<String>, pub bcc: Vec<String>, pub subject: String, pub body: String }`.
  - Follows RFC 6068: percent-decoding, addresses comma-separated in the path and in `to=`.
  - Other header fields (`In-Reply-To`, `From` and the rest) are ignored, so a link cannot set them. `None` for a URL that is not `mailto:`.
- [x] **Step 1: Write the failing tests:** `a_plain_address`; `several_recipients_and_fields_are_decoded` (`mailto:a@x.org,b@x.org?cc=c%40x.org&subject=Hi%20there&body=Line%0Aline`); `other_headers_are_ignored` (`?from=evil@x.org&in-reply-to=x` leaves them out); `not_a_mailto_url_is_none`.
- [x] **Step 2: Run** `cargo nextest run -p postvak-core send::mailto` and expect FAIL.
- [x] **Step 3: Implement** with `percent-encoding` (already in the workspace for the GUI; add it to `postvak-core` with `workspace = true`).
- [x] **Step 4: Run** the same command and expect PASS.
- [x] **Step 5: Commit** `feat(send): read mailto: links`

### Task 4: The composer without a toolkit

**Files:**
- Create: `crates/postvak-gui/src/compose.rs`, with the `compose` entry in `AGENTS.md`'s `gui` line ("`compose` is the composer without a toolkit: its fields, checks, questions and recovery file")

**Interfaces:**
- Consumes: Task 1 `Prefilled`, `HeaderEdits`, `SendRequest`, `SendKind`, `forward_attachments`; Task 3 `Mailto`; `config::Identity`, `check::check_address` (or part 1's address check by its existing name).
- Produces:
  - `pub(crate) enum ComposeKind { New, Reply(ReplyTo), Forward }`
  - `pub(crate) struct Composer { pub id: u64, pub account: String, pub kind: ComposeKind, pub original: Option<OriginalRef>, pub from: String, pub from_choices: Vec<String>, pub to: String, pub cc: String, pub bcc: String, pub show_cc_bcc: bool, pub subject: String, pub text: String, pub files: Vec<PathBuf>, pub forward_parts: Vec<(String, u64, bool)>, pub forward_as_attachment: bool, pub loading: bool, pub sending: bool, pub error: Option<(Field, String)>, opened_as: Snapshot }`. Here `OriginalRef { folder: String, uid: u32, raw: Vec<u8> }`, `pub(crate) enum Field { From, To, Cc, Bcc, Subject, Attachments, Send }`, and `Snapshot` is the fields as they opened, for `changed()`.
  - `Composer::reply(id, account, folder, uid, to: ReplyTo) -> Composer`, which starts as `loading`. `Composer::forward(id, account, folder, uid)`. `Composer::new_mail(id, account: &str, identity: &Identity, mailto: Option<Mailto>)`. `Composer::fill(&mut self, prefilled: Prefilled, identity: &Identity)` ends loading.
  - `from_choices`: the account's address and every alias without a wildcard. For a reply, the starting `from` is `Prefilled.draft.from`. For new mail, the account's address. For new mail, the choices hold every account's addresses as `account\taddress` pairs, and `account` follows the chosen one.
  - `Composer::changed(&self) -> bool`; `Composer::leave_question(&self) -> Option<&'static str>` is `Some("Discard this message?")` when `changed()`.
  - `Composer::check(&self, identity: &Identity, limit: u64) -> Result<(), (Field, String)>`. It requires at least one recipient, checks every address with part 1's check and the From with `check_from`, and keeps the total attachment size under `limit` (the stored `SIZE`, else 25 MB as part 1 uses). `Composer::attachment_total(&self) -> u64`.
  - `Composer::request(&self) -> SendRequest`: `Body::Given(text)`, `original: Some(raw)`, `headers: Some(..)` and `forward_parts`. `as_` is `Attachment` when `forward_as_attachment`.
  - `Composer::split_addresses(field: &str) -> Vec<String>` uses `send::edit::split_recipients`.
  - `Composer::field_error(&self, field: Field) -> Option<String>`: the same address check for one field, run when it loses focus, so a bad address is marked in place before Send.
- [x] **Step 1: Write the failing tests:**
  - `a_reply_starts_loading_and_fills_from_the_prefill`
  - `reply_all_fills_cc`
  - `a_forward_lists_the_originals_attachments_all_kept`
  - `new_mail_from_a_mailto_link`
  - `nothing_typed_leaves_without_asking`
  - `a_typed_word_asks_before_leaving`
  - `check_names_the_field` (an empty To gives `Field::To`; a bad Cc gives `Field::Cc`; 30 MB of files against a 25 MB limit gives `Field::Attachments`)
  - `the_request_is_what_the_fields_say`
  - `a_wildcard_alias_is_not_a_choice_but_may_be_typed`
  - `a_bad_address_is_marked_when_its_field_is_left`
- [x] **Step 2: Run** `cargo nextest run -p postvak-gui compose` and expect FAIL.
- [x] **Step 3: Implement** to the interfaces above.
- [x] **Step 4: Run** the same command and expect PASS.
- [x] **Step 5: Commit** `feat(gui): the composer's state, checks and questions`

### Task 5: The composer in the window's state, sending and undo

**Files:**
- Create: `crates/postvak-gui/src/state/composing.rs` (an `App` impl block, as `acting.rs` is)
- Modify: `crates/postvak-gui/src/state/mod.rs`, `crates/postvak-gui/src/state/acting.rs`, `crates/postvak-gui/src/undo.rs`, `crates/postvak-gui/src/keys.rs`, `crates/postvak-gui/src/body.rs`, `crates/postvak-gui/src/state/opened.rs`, `AGENTS.md` (`state`'s list of `App` method files gains `composing`)

**Interfaces:**
- Consumes: Task 4 `Composer`; Task 1 `prefill`, `compose`; `daemon::Client::cancel_send`.
- Produces:
  - `App.composer: Option<Composer>`, the reader pane's.
  - Injected like `check_login`:
    - `App.prefill: PrefillFn`, `fn(&Paths, &Config, &str, &SendKind) -> Result<Prefilled, String>`
    - `App.queue_send: QueueFn`, `fn(&Paths, &Config, SendRequest) -> Result<Queued, String>`
    - `App.cancel_send: CancelFn`, `fn(&Paths, &str, &str) -> Result<(), CancelError>`, with `enum CancelError { TooLate, Failed(String) }`
  - The real functions build a `LazyClient`, as `test_login` builds its `Client`.
  - `pub(crate) enum ComposeTarget { Pane, Window(u64) }`. Every `Compose*` action below carries one as its first field, so Task 7 only adds windows.
  - `UiAction` gains:
    - `Reply(ReplyTo)`, `Forward`
    - `ComposeEdit(ComposeField, String)`, with `ComposeField` the editable `Field`s plus `Text`
    - `ComposeShowCcBcc`, `ComposeAttach`, `ComposeRemoveFile(usize)`, `ComposeToggleForwardPart(usize)`, `ComposeForwardAsAttachment(bool)`
    - `ComposeSend`, `ComposeClose`, `ComposeDiscard`
  - `Dialog::LeaveCompose`, with `UiAction::LeaveComposeDiscard` and `UiAction::LeaveComposeCancel`, and the pending `UiAction` that asked to leave, run after Discard.
  - `Taken` gains `outbox: Option<(usize, String)>` (account, outbox id) and `composer: Option<Box<Composer>>`. `undo_newest` on such a gesture calls `cancel_send` on a worker. On `Ok` it puts the composer back: in the pane, after `Dialog::LeaveCompose` if another composer holds it. On `TooLate` the toast says `Already on its way`.
  - The toast after Send: `Sending to <first recipient> in <n> s` with Undo, or `Sent` without Undo when `undo_secs == 0`. The first recipient is `Queued.first_recipient`, and ` and <k> more` is added when `Queued.more > 0`.
  - Keys in `keys::action`: `r`, `Shift+R`, `l` and `f` with focus on the list or the reader, not while typing. The reader's Reply button becomes `UiAction::Reply(ReplyTo::Sender)`, and the `More` menu gains Reply all, Reply to list (shown when the message has `List-Post`) and Forward. `reply_url` and its tests go.
  - Leaving: selecting another row, changing folder or view, or `Reply` while a changed composer is open raises `Dialog::LeaveCompose`. An unchanged composer closes silently.
- [x] **Step 1: Write the failing tests** with `test_support::Driver` and fakes for the three functions:
  - `reply_opens_the_composer_over_the_original_and_fills_it` (Review Focus 1: `loading` and Send disabled until the prefill answers)
  - `send_queues_the_request_and_shows_the_toast`
  - `undo_reopens_the_composer_as_it_was`
  - `z_undoes_a_send_too`
  - `undo_too_late_says_so_and_reopens_nothing` (Review Focus 4)
  - `a_failed_queue_keeps_the_composer_with_the_error` (Review Focus 3)
  - `a_second_send_while_one_is_in_flight_is_ignored` (Review Focus 5)
  - `leaving_a_changed_composer_asks_and_discard_goes_on`
  - `an_unchanged_composer_closes_when_leaving`
  - `the_keys_open_each_kind`
- [x] **Step 2: Run** `cargo nextest run -p postvak-gui composing` and expect FAIL.
- [x] **Step 3: Implement.** The composer's `sending` flag is set before the worker starts and cleared by its answer. The composer is kept in `Taken` until the outbox entry leaves `waiting`: drop it when `read_outboxes` no longer lists the id as waiting.
- [x] **Step 4: Run** `cargo nextest run -p postvak-gui` and expect PASS.
- [x] **Step 5: Commit** `feat(gui): reply and forward from the reader, sent through the outbox with undo`

### Task 6: Drawing the reply composer

**Files:**
- Create: `crates/postvak-gui/src/ui/compose.slint`
- Modify: `crates/postvak-gui/src/ui/window.slint`, `crates/postvak-gui/src/ui/body.slint` (if the body view needs a height input), `crates/postvak-gui/src/bind.rs`, `crates/postvak-gui/src/persist.rs`, `crates/postvak-gui/src/snapshots.rs`

**Interfaces:**
- Consumes: Task 5's `UiAction`s and `App.composer`.
- Produces:
  - `export component Composer`: a button row (Send, Close, Discard; `Ctrl+Enter` shown); From as a `ComboBox`, or a `LineEdit` when the From was typed; To with a `Cc Bcc` button; Cc and Bcc rows when shown; Subject; a `TextEdit` for the text; the file list with remove buttons; for a forward, the original's parts with check boxes and the "Send as attachment" toggle; and the error line under the field it names.
  - The reader pane, while `App.composer` is set, stacks the composer, a `Splitter` and the existing body view.
  - `persist::Layout` gains `#[serde(default)] compose_height: Option<f32>`, the composer's share of the pane, dragged by the splitter.
- [x] **Step 1: Write the failing tests:** a window test `the_reply_composer_sits_above_the_original` (`Fixture::window()`: after `r`, the subject field holds `Re: ` and the body view is still shown), and `the_split_is_saved_with_the_window` (a `persist` round trip with `compose_height`).
- [x] **Step 2: Run** `cargo nextest run -p postvak-gui window persist` and expect FAIL.
- [x] **Step 3: Implement.** Bind the fields with `bind`'s existing pattern (properties out, callbacks to `UiAction`s).
- [x] **Step 4: Run** `TZ=UTC UPDATE_SNAPSHOTS=1 cargo test -p postvak-gui snapshots`. Add snapshots `compose_reply_html_light` and `compose_reply_html_dark` (a reply over an HTML fixture) and `compose_forward_attachments`, and look at each PNG.
- [x] **Step 5: Run** `cargo nextest run -p postvak-gui` and expect PASS.
- [x] **Step 6: Commit** `feat(gui): draw the composer above the original, with a splitter`

### Task 7: New mail in its own window

**Files:**
- Modify: `crates/postvak-gui/src/ui/compose.slint` (`export component ComposeWindow inherits Window`, holding `Composer`), `crates/postvak-gui/src/state/composing.rs`, `crates/postvak-gui/src/bind.rs`, `crates/postvak-gui/src/lib.rs`, `crates/postvak-gui/src/menu.rs`, `crates/postvak-gui/src/toolbar.rs`, `crates/postvak-gui/src/keys.rs`

**Interfaces:**
- Produces:
  - `App.new_mail: Vec<Composer>`, one per open `ComposeWindow`, matched by `Composer.id`.
  - `UiAction::NewMail(Option<Mailto>)`. A `mailto:` link in a message (in `opened.rs`, where it calls `OpenUrl` today) opens one with the link's fields.
  - The window's close button runs `ComposeClose` for its id.
  - `Ctrl+N`, File ▸ New message and a toolbar button open one. New mail's account is the account of the folder in view, else the first.
- [x] **Step 1: Write the failing tests:**
  - `ctrl_n_opens_a_new_mail_window_on_the_viewed_account`
  - `two_new_mail_windows_keep_their_own_fields`
  - `closing_a_changed_new_mail_window_asks`
  - `a_mailto_link_opens_new_mail_with_its_fields`
  - `undo_of_new_mail_reopens_it_in_a_window`
- [x] **Step 2: Run** `cargo nextest run -p postvak-gui new_mail` and expect FAIL.
- [x] **Step 3: Implement.** Keep the `ComposeWindow` instances beside `MailWindow` in `lib.rs`'s run loop. Each binds to `App.new_mail` by id. `Dialog::LeaveCompose` for a window is drawn in that window.
- [x] **Step 4: Run** `TZ=UTC UPDATE_SNAPSHOTS=1 cargo test -p postvak-gui snapshots` with a new `compose_new_mail` snapshot, and look at it.
- [x] **Step 5: Run** `cargo nextest run -p postvak-gui` and expect PASS.
- [x] **Step 6: Commit** `feat(gui): write new mail in a window of its own`

### Task 8: Attaching files

**Files:**
- Modify: `crates/postvak-core/src/portal.rs` (`open_files`)
- Create: `crates/postvak-gui/src/open_files.rs`, the counterpart of `save_as.rs` (portal on Linux, `NSOpenPanel` on macOS), with its entry in `AGENTS.md`'s `gui` line
- Modify: `crates/postvak-gui/src/state/composing.rs`, `crates/postvak-gui/src/ui/compose.slint`

**Interfaces:**
- Produces:
  - `portal::open_files(parent: &str, title: &str) -> Result<Vec<PathBuf>, PortalError>`: FileChooser `OpenFile` with `multiple = true`, as `save_file` calls `SaveFile`.
  - `open_files::ask(wake: Wake) -> Receiver<Vec<PathBuf>>`, injected into `App` as `ask_open_paths` like `ask_save_path`.
  - `ComposeAttach` opens it, and the chosen files join `Composer.files`. The total is shown beside the list as `12.4 MB of 25 MB`, and the check error appears as soon as it is over the limit.
  - Dropping files: use Slint 1.18.1's external file drop if it has one. If it has none, leave it out, and Task 10 changes the spec's line to say attaching is by the button only, with the follow-up named.
- [x] **Step 1: Write the failing tests:** `chosen_files_are_attached_and_counted`, `too_much_shows_at_once`, `a_cancelled_dialog_attaches_nothing`, and in core `portal` the request's options (`multiple`) as `save_file`'s tests check them.
- [x] **Step 2: Run** `cargo nextest run -p postvak-gui attach && cargo nextest run -p postvak-core portal` and expect FAIL.
- [x] **Step 3: Implement.**
- [x] **Step 4: Run** the same commands and expect PASS. Run `cargo check --target aarch64-apple-darwin -p postvak-gui` if the target is installed; otherwise CI's macOS job is the first compile.
- [x] **Step 5: Commit** `feat(gui): attach files to a message`

### Task 9: Completion, the pending mark and the outbox dialog

**Files:**
- Modify: `crates/postvak-gui/src/state/composing.rs`, `crates/postvak-gui/src/state/mod.rs` (`read_outboxes`), `crates/postvak-gui/src/list.rs`, `crates/postvak-gui/src/load.rs`, `crates/postvak-gui/src/status.rs`, `crates/postvak-gui/src/ui/compose.slint`, `crates/postvak-gui/src/ui/list.slint`, `crates/postvak-gui/src/ui/dialogs.slint`, `crates/postvak-gui/src/ui/status.slint`

**Interfaces:**
- Consumes: Task 2 `Store::addresses`; `output`'s outbox rows (`postvak outbox --json`); `Client::retry_send`.
- Produces:
  - **Completion.** While an address field has focus, the last address being typed asks every account's store for `addresses(prefix, 8)` on a `Worker` (the newest keystroke wins). The merged list puts `written_to` rows first, then orders by `latest`. A popup under the field shows `Name <address>`, with Up, Down, Enter and Tab to pick.
  - **Pending mark.** `Row` gains `message_id: Option<String>` and `pending: bool`. `read_outboxes` also keeps, per account, the set of `Entry.original.message_id` for entries in `waiting` or `sending`, and a row whose `message_id` is in its account's set is `pending`. It is drawn as a small clock icon beside the subject (`icons.rs`).
  - **The outbox dialog.** `Dialog::Outbox`, opened by clicking the status bar's "Sending 1 message" or "1 message failed". It lists every account's waiting and failed entries, read from `Outbox::list` (the same entries `output` turns into `postvak outbox` rows), with the account, recipient count, when it is due, and the failure reason. A failed entry has Retry (`UiAction::RetrySend(usize, String)`, on a worker via an injected `RetryFn`).
- [x] **Step 1: Write the failing tests:**
  - `completion_lists_people_written_to_first_across_accounts`
  - `picking_a_completion_replaces_the_typed_part`
  - `a_reply_waiting_marks_its_original`
  - `the_mark_goes_once_sent`
  - `the_outbox_dialog_lists_waiting_and_failed`
  - `retry_sends_a_failed_one_again`
- [x] **Step 2: Run** `cargo nextest run -p postvak-gui completion pending outbox` and expect FAIL.
- [x] **Step 3: Implement.**
- [x] **Step 4: Run** `TZ=UTC UPDATE_SNAPSHOTS=1 cargo test -p postvak-gui snapshots` with new `compose_completion` and `outbox_dialog` snapshots and the list row with the mark, and look at them.
- [x] **Step 5: Run** `cargo nextest run -p postvak-gui` and expect PASS.
- [x] **Step 6: Commit** `feat(gui): complete addresses, mark answers waiting to go, and list the outbox`

### Task 10: Not losing what was typed, docs and the full check

**Files:**
- Modify: `crates/postvak-gui/src/compose.rs` (recovery), `crates/postvak-gui/src/state/composing.rs`, `crates/postvak-gui/src/lib.rs` (quit path), `docs/src/gui.md`, `docs/src/compare.md`, `docs/superpowers/specs/2026-11-03-sending-gui-design.md` (only if Task 8 left drop out), `docs/superpowers/plans/2026-11-03-sending-gui-composer.md` (tick the boxes)

**Interfaces:**
- Produces:
  - `compose::recovery_dir(paths: &Paths) -> PathBuf`, which is `<state_dir>/compose/`, with one `<id>.json` per open composer: `Composer` minus `loading`, `sending` and `error`, with the original as its `OriginalRef` without `raw`. Raw is fetched again on restore by folder and uid, or the composer reopens as new mail with In-Reply-To kept when the original is gone. Written with `paths::write_atomic`, at most every 5 s while `changed()`, from the window's tick.
  - `compose::save_recovery(dir, &Composer) -> io::Result<()>`, `compose::load_recoveries(dir) -> Vec<Composer>` (an unreadable file is logged by name and skipped), and `compose::remove_recovery(dir, id)`.
  - The file is removed on send (once queued), on Discard, and on Close of an unchanged composer. On start, each file reopens its composer (pane or window), with the line `Restored after Postvak closed`.
  - Quitting writes every composer's file once more and does not ask.
- [x] **Step 1: Write the failing tests:**
  - `a_composer_round_trips_through_its_file`
  - `a_queued_or_discarded_composer_leaves_no_file`
  - `start_reopens_left_composers_with_the_line`
  - `quitting_writes_the_files_without_asking`
  - `an_unreadable_file_is_skipped`
- [x] **Step 2: Run** `cargo nextest run -p postvak-gui recovery` and expect FAIL.
- [x] **Step 3: Implement.**
- [x] **Step 4: Write the docs.** `gui.md`: replying, forwarding, new mail, attachments, completion, the undo toast, the pending mark, the outbox dialog and the keys. `compare.md`: drop the composer from what is missing, keeping HTML writing and drafts (drafts until PR 2).
- [x] **Step 5: Run the definition of done:** `cargo fmt --check && cargo clippy --all-targets --all-features -- -D warnings && cargo nextest run --workspace --all-features && cargo test --doc --workspace && cargo machete && cargo deny check`. Then `TZ=UTC UPDATE_SNAPSHOTS=1 cargo test -p postvak-gui snapshots`, and confirm with `git status` that no PNG changed unexpectedly.
- [x] **Step 6: Commit** `feat(gui): keep an unsent message across a crash or quit` and `docs: the composer`
