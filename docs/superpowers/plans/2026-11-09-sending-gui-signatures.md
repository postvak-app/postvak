# Sending from the GUI, part 3: signatures, Preferences › Sending and the add-account SMTP line

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Signatures per address on every surface that sends (GUI composer and `postvak send`), a Preferences › Sending page (undo delay, signatures, each account's SMTP server), and the SMTP server shown when an account is added in the GUI.

**Architecture:** Signatures live in config.toml under `[[sending.signatures]]`, written only by `config`. `SendingConfig::signature_for` picks one for a From address, and `send::signature::sign` puts it under the text. The CLI signs in `compose::message_draft`. The GUI composer signs its own text, so what you see is what is sent, and its requests say `signature: false`. Preferences writes through `config::update_sending` and `account::set_smtp`. The add-account save guesses SMTP through the daemon after the IMAP login test, as `postvak account add` does.

**Tech Stack:** Rust, Slint 1.18, toml_edit, the daemon `Client`.

**Spec:** `docs/superpowers/specs/2026-11-03-sending-gui-design.md` (sections Signatures, Preferences › Sending, Adding an account, Surfaces, Docs, Tests).

**Branch:** `feat/gui-signatures`, from `main` (#568 is merged).

## Global Constraints

- config.toml is written only by `config`; every write merges into the file as it reads then, keeping comments and other sections.
- A signature goes under a `-- ` line (dash, dash, space), RFC 3676's separator, at the end of the user's text, above any quote or forward block.
- `on_replies` defaults to `true`; when `false`, a reply or forward gets no signature.
- `--raw` never adds a signature; `--no-signature` turns it off for `postvak send`; MCP gets nothing.
- Undo delays offered: exactly `config::UNDO_SECS` (`[0, 5, 10, 20, 30]`), 0 reading "Off".
- Never log recipients, subjects, bodies or signatures (a signature is message text).
- GUI layout uses `Theme` sizes and `Hairline`; no literal sizes.
- After the CLI flag change: `POSTVAK_BLESS=1 cargo test`, commit `docs/src/cli.md`. After GUI work: `TZ=UTC UPDATE_SNAPSHOTS=1 cargo test -p postvak-gui snapshots`, look at every changed PNG.
- Definition of done (AGENTS.md): fmt, clippy `-D warnings`, nextest, doc tests, machete, deny.

## Review Focus

1. **An edited or deleted signature, then From changes:** expected: the user's text is left alone. No signature is put back or swapped once the text no longer ends with the one that was put in. Test in Task 3.
2. **An alias pattern and an exact address that both match From:** expected: the exact address's signature wins, whatever their order in the file. Test in Task 1.
3. **A draft reopened from Drafts, or a composer restored after a crash, then sent:** expected: exactly one signature in the message. Test in Task 3.
4. **Signature text that is hostile to TOML** (quotes, `"""`, backslashes, CRLF, trailing spaces): expected: it round trips through config.toml unchanged, apart from CRLF becoming LF. Test in Task 1.
5. **Guess again or Save guess failing** (daemon down, no server answers): expected: Preferences shows why and config.toml is unchanged. Test in Task 4.

---

### Task 1: Signatures in config.toml

**Files:**
- Modify: `crates/postvak-core/src/config.rs` (`SendingConfig`, `validate`, new `Signature`, `update_sending`)
- Test: same file's `tests`

**Interfaces:**
- Produces:
  - `pub struct Signature { pub address: String, pub text: String, #[serde(default = "yes")] pub on_replies: bool }`, which denies unknown fields.
  - `SendingConfig.signatures: Vec<Signature>`, `#[serde(default, skip_serializing_if = "Vec::is_empty")]`.
  - `impl SendingConfig { pub fn signature_for(&self, from: &str, answering: bool) -> Option<&str> }` returns the text.
  - `pub fn update_sending(path: &Path, change: impl FnOnce(&mut SendingConfig)) -> Result<Config, ConfigError>`.

- [ ] **Step 1: Write the failing tests**
  - `a_signature_round_trips_through_the_file`: write `[[sending.signatures]]` with `text = "Pieter \"P\" W.\\n\"\"\"\\nExample BV  "` and `on_replies = false` via `update_sending`, then `Config::load`. The text and `on_replies` are equal. A comment elsewhere in the file and `[ui]` stay. A text given with `\r\n` reads back with `\n`.
  - `signature_for_prefers_the_exact_address_to_a_pattern`: signatures `[*@example.com → "Shop", pieter@example.com → "Pieter"]`. `signature_for("Pieter@Example.com", false) == Some("Pieter")`, `signature_for("info@example.com", false) == Some("Shop")`, `signature_for("x@other.org", false) == None`.
  - `on_replies_false_leaves_answers_unsigned`: `signature_for(addr, true) == None` and `signature_for(addr, false) == Some(..)`.
  - `validate_refuses_a_signature_without_an_address_or_twice_for_one`: an empty `address`, an address without `@`, and the same address twice (case ignored) are each `ConfigError::Invalid` naming `sending.signatures`.
  - `update_sending_sets_undo_secs`: `update_sending(path, |s| s.undo_secs = 30)` reads back 30. `undo_secs = 7` is refused by `validate` (add that check if missing: it must be in `UNDO_SECS`).

- [ ] **Step 2: Run** `cargo nextest run -p postvak-core config::` and expect them to fail to compile.

- [ ] **Step 3: Implement.** `update_sending` mirrors `update_ui` (same `edit_toml`/`merge_table` shape), with `signatures` written as an array of tables (`[[sending.signatures]]`). Patterns match with the existing `alias_regex`. CRLF is normalised to LF in `update_sending` before writing.

- [ ] **Step 4: Run** `cargo nextest run -p postvak-core config::` and expect PASS.

- [ ] **Step 5: Commit** `feat(config): signatures per address under [sending]`

---

### Task 2: `postvak send` signs, `--no-signature`

**Files:**
- Create: `crates/postvak-core/src/send/signature.rs` (and `pub mod signature;` in `send.rs`)
- Modify: `crates/postvak-core/src/send/compose.rs` (`SendRequest.signature`, `message_draft`)
- Modify: `src/cli/mod.rs` (`SendArgs.no_signature`), `crates/postvak-core/src/help.rs` if the flag text lives there
- Modify: every `SendRequest { .. }` literal (GUI `composing.rs`, `drafts.rs`, tests). GUI requests set `signature: false`.
- Docs: `docs/src/sending.md` (new "Signatures" section, under "Your name on From"), `docs/src/cli.md` (blessed)

**Interfaces:**
- Consumes: `SendingConfig::signature_for` (Task 1).
- Produces:
  - `pub fn sign(text: &str, signature: &str) -> String`: `text` with its trailing blank lines trimmed, then `"\n\n-- \n"` and `signature` with its trailing newlines trimmed. An empty `text` gives `"\n\n-- \n<sig>"`, so the editor opens with room above the signature.
  - `pub fn signed_by(text: &str, signature: &str) -> bool`: whether `text` ends with what `sign` puts there.
  - `SendRequest.signature: bool`: sign the text with From's signature. The CLI sets it to `!no_signature`; the GUI and `drafts` set it to `false`.

- [ ] **Step 1: Write the failing tests**
  - `signature.rs`: `sign("Hi\n\n", "P\n") == "Hi\n\n-- \nP"`; `sign("", "P") == "\n\n-- \nP"`; `signed_by(&sign("Hi", "P"), "P")`; `!signed_by("Hi\n\n-- \nQ", "P")`.
  - `compose.rs`: `a_given_text_is_signed_with_from_s_signature`: `Body::Given("Hi")` with a signature for the account's address produces `draft.text == "Hi\n\n-- \nPieter"`.
  - `a_reply_signs_above_the_quote`: the signature comes before `On … wrote:`.
  - `no_signature_and_on_replies_false_leave_it_off`.
  - `the_editor_opens_with_the_signature`: with `Body::Edit`, the template passed to `edit` contains `"\n\n-- \nPieter"` above the quote, and returning the template unchanged is still refused ("left empty or unchanged").
  - `the_signature_follows_from`: `request.from = Some(alias)` with its own signature uses the alias's.
  - `--raw` is untouched; `sendmail` has no signature path, so there is no test.
  - Update `equal_to_the_cli`: the CLI side gets `signature: true` and a configured signature, and the GUI composer's text contains `sign(text, sig)`. The bytes must still be equal.

- [ ] **Step 2: Run** `cargo nextest run -p postvak-core send::` and expect FAIL.

- [ ] **Step 3: Implement.** In `message_draft`, find From first: `request.from`, else what `draft_for` picks with an empty text. Then sign `given`, or the empty text for `Body::Edit`, before the real `draft_for` call. `compose()` passes `&config.sending` into `message_draft`. Add the clap flag `--no-signature` ("Leave the signature off"), which conflicts with `--raw`.

- [ ] **Step 4: Run** `cargo nextest run --workspace` and expect PASS, then run `POSTVAK_BLESS=1 cargo test` and check that `docs/src/cli.md` changed only by the flag.

- [ ] **Step 5: Commit** `feat(send): signatures, and --no-signature`

---

### Task 3: The composer's signature

**Files:**
- Modify: `crates/postvak-gui/src/compose.rs` (`Composer.signature`, `Composer::sign`)
- Modify: `crates/postvak-gui/src/state/composing.rs` (call `sign` after new mail opens, a prefill fills, From changes, a draft or recovery file opens)
- Test: `compose.rs` tests, `composing.rs` tests

**Interfaces:**
- Consumes: `send::signature::{sign, signed_by}`, `SendingConfig::signature_for`, and `App.config.sending`.
- Produces:
  - `Composer.signature: Option<String>`: the signature this composer put in its text. `#[serde(default)]`, so recovery files written before this change still load.
  - `pub(crate) fn sign(&mut self, sending: &SendingConfig)`: makes the text end with From's signature. It does nothing while `loading`. It also does nothing when `self.signature` is set but the text no longer ends with it, because then the user edited it. Otherwise it strips the old signature and appends the new one, or none. Called on the window thread; it reads no file.

- [ ] **Step 1: Write the failing tests** (without a toolkit, in `compose.rs`):
  - `new_mail_opens_signed_and_unchanged`: `text == "\n\n-- \nPieter"` and `!changed()`.
  - `a_filled_reply_is_signed_unless_on_replies_is_false`.
  - `changing_from_swaps_an_untouched_signature`: work → alias, and the text ends with the alias's signature, the user's text above it kept.
  - `an_edited_or_deleted_signature_stays_as_written` (Review Focus 1): edit one character of the signature, change From, and the text is unchanged. Delete the signature, change From, and none is added.
  - `a_reopened_draft_keeps_one_signature` (Review Focus 3): `from_draft` of a draft whose text is `sign("Hi", "Pieter")`, then `sign()`, gives a text with `"-- \n"` once, and a From change still swaps it.
  - In `composing.rs`: `a_restored_composer_sends_one_signature`: restore from a recovery file, send, and the queued request's text has one `-- ` line and `signature == false`.

- [ ] **Step 2: Run** `cargo nextest run -p postvak-gui compose` and expect FAIL.

- [ ] **Step 3: Implement.** `from_draft` sets `signature` when the text is `signed_by` From's signature, so the swap works on a reopened draft. `opened_as` is taken after signing, so a signed but untouched composer reads as unchanged.

- [ ] **Step 4: Run** `cargo nextest run -p postvak-gui` and expect PASS. Run the snapshots and look at `gui_compose_*.png`: the reply composer now shows the signature.

- [ ] **Step 5: Commit** `feat(gui): the composer signs with From's signature`

---

### Task 4: Preferences › Sending

**Files:**
- Modify: `crates/postvak-gui/src/preferences.rs` (`Section::Sending`, `Setting::UndoSecs`, `Setting::Signature`, `undo_choices`, `smtp_line`)
- Modify: `crates/postvak-gui/src/state/mod.rs` (two UiActions run on a worker, their answers, and the error on the page)
- Modify: `crates/postvak-gui/src/bind.rs`, `crates/postvak-ui/ui/preferences.slint` (a third tab, "Sending")
- Modify: `crates/postvak-gui/src/snapshots.rs` (new snapshot `gui_preferences_sending`)
- Docs: `docs/src/sending.md` (Preferences › Sending), `docs/src/gui.md` (Preferences line)

**Interfaces:**
- Consumes: `config::update_sending` (Task 1), `account::set_smtp` and `SmtpChange::{Guess, SaveGuess}`, `send::server_for`, and `Store::smtp_state`.
- Produces:
  - `Setting::UndoSecs(u64)`.
  - `Setting::Signature { address: String, text: String, on_replies: bool }`: sets the signature for the address, replacing an existing one (case ignored). An empty `text` removes it.
  - `pub(crate) fn undo_choices() -> Vec<(u64, String)>`: `[(0,"Off"),(5,"5 seconds"),(10,"10 seconds"),(20,"20 seconds"),(30,"30 seconds")]`.
  - `pub(crate) fn smtp_line(account: &AccountConfig, state: Option<&SmtpState>) -> (String, bool)`: the words, and whether Save guess is offered. The words are `"smtp.x.org:465 (TLS), from config.toml"`, `"smtp.x.org:587 (STARTTLS), guessed"` (Save offered), or `"Not set up: no server answered"`.
  - `UiAction::SmtpSaveGuess(String)` and `UiAction::SmtpGuessAgain(String)` run `set_smtp` on `worker::oneshot` through a `LazyClient`. A fn pointer field on `App` lets tests fake it. They refresh the config when answered, and put a refusal in `Preferences.error`.

- [ ] **Step 1: Write the failing tests**
  - `undo_choices_are_the_spec_s_five`.
  - `smtp_line_says_where_the_server_comes_from` for the three cases above.
  - `a_signature_setting_adds_replaces_and_removes`.
  - Window test `the_sending_page_saves_undo_and_a_signature`: open Preferences, choose Sending, pick 30 seconds, type a signature for the account's address, and press Save. Then config.toml has `undo_secs = 30` and the signature.
  - `a_failed_guess_says_why_and_writes_nothing` (Review Focus 5): the faked `set_smtp` returns `Err`, `Preferences.error` holds the reason, and config.toml is byte-for-byte unchanged.

- [ ] **Step 2: Run** `cargo nextest run -p postvak-gui preferences` and expect FAIL.

- [ ] **Step 3: Implement the page.** The undo delay is a `ChoiceSetting`. Signatures get one row per address of every account: the account's address, then each alias pattern. A row has the address, a multi-line `TextEdit`, the switch "Also on replies and forwards", and a Save button that is enabled when the row differs from config. A signature written per keystroke would rewrite config.toml on every letter, hence the button. Then, per account, `smtp_line` with Save guess (when offered) and Guess again, which are disabled while one runs.

- [ ] **Step 4: Run** `cargo nextest run -p postvak-gui`, then the snapshots, and look at `gui_preferences_sending.png` in light and dark.

- [ ] **Step 5: Commit** `feat(gui): Preferences › Sending`

---

### Task 5: The SMTP server when adding an account

**Files:**
- Modify: `crates/postvak-gui/src/account.rs` (`Fields.smtp_host`, `smtp_port`, `smtp_starttls`; a `FindSmtp` fn type; `Step::Saved` carries the SMTP)
- Modify: `crates/postvak-gui/src/state/mod.rs` (`account_added` toast, the form's "not set up" state), `crates/postvak-gui/src/lib.rs` (first-run window, same)
- Modify: `crates/postvak-ui/ui/account.slint` (an "Outgoing server" line with Change, and the note with a `Link`), `startup.slint` if it binds the form's properties
- Docs: `docs/src/gui.md` and `docs/src/install.md`, wherever adding an account is described

**Interfaces:**
- Consumes: `account::smtp_for_new(client, account, given, Some(secret))`.
- Produces:
  - `pub(crate) type FindSmtp = fn(&Paths, &AccountConfig, Option<SmtpConfig>, &Secret) -> Result<Option<SmtpConfig>, String>`. The real one opens a `Client` and calls `smtp_for_new`; tests fake it. It runs after `check` passes and before `account::add`. A given server that fails its login refuses the save, as the CLI does. A guess that finds nothing still saves.
  - `Step::Saved(Box<AccountConfig>)`: the account now carries `smtp` as found, and None when sending is not set up.
  - `pub(crate) fn smtp_given(&self) -> Result<Option<SmtpConfig>, String>` on `Fields`: None when the host is empty. The port is empty for TLS's default, otherwise a number.

- [ ] **Step 1: Write the failing tests**
  - `the_guess_is_saved_with_the_account`: the faked `FindSmtp` returns `smtp.example.com`, implicit TLS. config.toml then has the account's `[accounts.smtp]`, and the window's toast reads `"Added 'work'; it sends through smtp.example.com:465"`.
  - `no_guess_still_adds_and_says_sending_is_not_set_up`: the faked fn returns `Ok(None)`. The account is in config.toml, and the form stays open with `"Sending is not set up: no SMTP server answered."` and the link "How to set one up" (`https://postvak.pataar.nl/docs/sending.html#the-smtp-server`). Done closes it.
  - `a_typed_server_that_fails_refuses_the_save`: Change, type a host, and the faked fn returns `Err`. Nothing is saved and the form shows why.
  - `Fields` Debug still redacts the password, and the SMTP fields are not secret.

- [ ] **Step 2: Run** `cargo nextest run -p postvak-gui account` and expect FAIL.

- [ ] **Step 3: Implement.** The form shows "Outgoing server: found after the login test · Change". Change reveals host, port and STARTTLS fields beside the IMAP ones. While the save runs, the progress reads "Finding the SMTP server…" after `Tested`, so add a `Step::Found` or extend `progress`, whichever touches less. The first-run window does the same: with no SMTP, it shows the note and Done goes on to the mail window.

- [ ] **Step 4: Run** `cargo nextest run -p postvak-gui`, then the snapshots, and look at the add-account form's PNG(s).

- [ ] **Step 5: Commit** `feat(gui): show the SMTP server when adding an account`

---

## After the last task

- Full definition of done, plus the live IMAP tests (`docker compose -f tests/dovecot/compose.yml up -d`, then `POSTVAK_TEST_IMAP_HOST=localhost cargo test --features testing --test imap_live`).
- `AGENTS.md`: under `config`, the writers become `save_account`, `save_theme` and `update_sending`. Under `gui`, `preferences` gains the Sending page.
- Push the branch and open a plain PR on `main`.
