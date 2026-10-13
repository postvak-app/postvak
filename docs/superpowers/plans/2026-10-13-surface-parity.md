# Surface Parity Implementation Plan

> **GUI toolkit (2026-10-13):** the GUI steps below were written for egui, which the Slint rewrite (`2026-10-13-slint-gui.md`, design `../specs/2026-10-13-postvak-slint-gui-design.md`) replaces. Redo them against `gui/state.rs`, `bind.rs` and `ui/*.slint` before executing; the non-GUI steps stand.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Every feature the CLI has is also on MCP and in the GUI, with account add going through one library function and the daemon instead of its own IMAP connection.

**Architecture:** A new `account` module owns adding an account (validate, store the keyring secret, save config.toml); the daemon watches config.toml and starts the account itself, and each front end then asks it for one sync. MCP gains six tools and one scope, with `backend.rs` still the only MCP code touching store, rules or the daemon. The GUI gains an account form, a purge button and an apply button, all as `UiAction`s carried out in `app.rs`.

**Tech Stack:** Rust 2024, clap 4, rmcp 3.5, schemars, eframe/egui 0.36, egui_kittest, keyring-core 1 (its `mock` store in tests), rusqlite 0.40.

**Spec:** `docs/superpowers/specs/2026-10-13-surface-parity-design.md`

## Global Constraints

- Front ends never touch IMAP. The only IMAP work in this plan is the daemon's own sync of a new account.
- In `src/mcp/`, only `backend.rs` touches the store, rules or the daemon; in `src/gui/`, only `app.rs` and `mod.rs` change state, view modules draw from `&App` and return `UiAction`s. `tests/architecture.rs` enforces both and must keep passing unchanged.
- Help texts come from `postvak::help`, JSON rows from `postvak::output`, shared by the CLI and MCP.
- No `unwrap`, `expect`, `panic!` outside tests; a lock is read with `.unwrap_or_else(PoisonError::into_inner)`.
- No secret is ever logged or printed. `Secret` has no `Debug`; the GUI form's `Debug` redacts the password.
- Over MCP, account add takes a password command only; a `password` argument is rejected by the schema.
- GUI sizes come from `theme.rs`: `ROW` 24, `CONTROL` 28, `INSET` 16, `PAD` 8.
- After changing CLI flags, run `POSTVAK_BLESS=1 cargo test` to regenerate `docs/src/cli.md`, and commit it.
- Every new dependency has a one-line reason in `Cargo.toml`. This plan adds none.
- Commits use conventional commits. Three PRs: Tasks 1–6, Tasks 7–11, Tasks 12–17.

## Review Focus

1. **A wrong password on account add.** The account is saved first, and the first sync's outcome, good or bad, is reported the way `sync` reports it: the CLI prints the daemon's reply or error for the account, MCP returns the sync row, the GUI shows the account's status line. Running add again with the same name replaces it. Tested in Task 2 (`account_add_saves_then_reports_the_first_sync`) and Task 8 (`account_add_returns_the_account_and_its_first_sync`); the offline timing of a refusal is covered by the existing `sync_reports_a_refusal_as_the_accounts_error`.
2. **The daemon has not loaded the new config yet.** A sync requested within the daemon's 2 s config poll must not be refused as an unknown account. `account::await_started` waits for the account in the daemon's status, tested in Task 1 (`await_started_waits_for_the_daemon_to_pick_up_the_account`).
3. **`attachment_save` with a relative or missing directory.** A relative `dir` is refused by name, since the server's working directory belongs to the host; a missing directory is an error naming it. Tested in Task 5 (`attachment_save_needs_an_absolute_dir`).
4. **`rules_apply_existing` on a disabled rule, or a rule of a hidden account.** Both refused with the same words as the CLI, and nothing is sent to the daemon. Tested in Task 10 (`rules_apply_existing_refuses_disabled_and_hidden_rules`).
5. **The GUI form on a name that already exists.** The account is replaced in config.toml, and the window shows one entry for it, not two. Tested in Task 14 (`saving_an_existing_name_replaces_the_account`).

---

# Phase 1: the library, the CLI and the MCP read tools (`feat/surface-parity-core`)

### Task 1: `account::add` and `account::await_started`

**Files:**
- Create: `src/account.rs`
- Modify: `src/lib.rs`, `src/credentials.rs:118-137`

**Interfaces:**
- Produces: `pub fn add(config: &mut Config, paths: &Paths, account: AccountConfig, secret: Option<Secret>) -> Result<(), AddError>` and `pub fn await_started(client: &Client, name: &str, timeout: Duration) -> anyhow::Result<()>` in `postvak::account`; `pub enum AddError { Config(ConfigError), Credential(CredentialError), MissingSecret(String) }`.
- Produces: in tests, the keyring is `keyring_core::mock::Store`, so `credentials::store` and `credentials::resolve` round-trip in-process.

- [ ] **Step 1: Make the keyring a mock under `cfg(test)`**

In `src/credentials.rs`, add `not(test)` to the three `platform_store` cfgs and add a fourth for tests:

```rust
#[cfg(all(target_os = "macos", not(test)))]
fn platform_store() -> Result<std::sync::Arc<keyring_core::CredentialStore>, String> {
    apple_native_keyring_store::keychain::Store::new()
        .map(|s| s as std::sync::Arc<keyring_core::CredentialStore>)
        .map_err(|e| e.to_string())
}

#[cfg(all(target_os = "linux", not(test)))]
fn platform_store() -> Result<std::sync::Arc<keyring_core::CredentialStore>, String> {
    zbus_secret_service_keyring_store::Store::new()
        .map(|s| s as std::sync::Arc<keyring_core::CredentialStore>)
        .map_err(|e| e.to_string())
}

#[cfg(all(not(any(target_os = "macos", target_os = "linux")), not(test)))]
fn platform_store() -> Result<std::sync::Arc<keyring_core::CredentialStore>, String> {
    Err("no keyring store for this platform".to_string())
}

/// Tests never touch the OS keyring: an in-memory store that lives for the test process.
#[cfg(test)]
fn platform_store() -> Result<std::sync::Arc<keyring_core::CredentialStore>, String> {
    keyring_core::mock::Store::new()
        .map(|s| s as std::sync::Arc<keyring_core::CredentialStore>)
        .map_err(|e| e.to_string())
}
```

Add one test at the end of the `tests` module in `src/credentials.rs`:

```rust
    #[test]
    fn keyring_round_trips_in_tests() {
        let account = AccountConfig::new(
            "roundtrip",
            "h",
            "u@example.com",
            PasswordSource::Keyring { keyring: true },
        );
        store("roundtrip", &Secret::new("hunter2".into())).unwrap();
        assert_eq!(resolve(&account).unwrap().expose(), "hunter2");
    }
```

- [ ] **Step 2: Run it**

Run: `cargo test credentials::tests::keyring_round_trips_in_tests`
Expected: PASS. If the mock store's `new` signature differs, read `~/.cargo/registry/src/*/keyring-core-1*/src/mock.rs` and adjust; it returns `Result<Arc<Self>>` at the time of writing.

- [ ] **Step 3: Write the failing tests for `add` and `await_started`**

Create `src/account.rs` with only the test module for now:

```rust
//! Adding an account: the one writer of a new account into config.toml, for the CLI, MCP and the GUI alike.

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::config::{AccountConfig, Config, PasswordSource};
    use crate::credentials::{self, Secret};
    use crate::daemon::Client;
    use crate::daemon::test_support::{TestDaemon, options, recording_connector, write_config};
    use crate::paths::Paths;

    fn home() -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        (dir, paths)
    }

    fn keyring_account(name: &str) -> AccountConfig {
        AccountConfig::new(
            name,
            "imap.example.com",
            "me@example.com",
            PasswordSource::Keyring { keyring: true },
        )
    }

    fn command_account(name: &str) -> AccountConfig {
        AccountConfig::new(
            name,
            "imap.example.com",
            "me@example.com",
            PasswordSource::Command {
                command: "printf x".into(),
            },
        )
    }

    #[test]
    fn add_saves_a_command_account_without_a_secret() {
        let (_dir, paths) = home();
        let mut config = Config::default();
        add(&mut config, &paths, command_account("work"), None).unwrap();
        let saved = Config::load(&paths.config_file()).unwrap();
        assert_eq!(saved.accounts.len(), 1);
        assert_eq!(saved.accounts[0].name, "work");
        assert_eq!(config.accounts, saved.accounts);
    }

    #[test]
    fn add_stores_the_keyring_secret_then_saves() {
        let (_dir, paths) = home();
        let mut config = Config::default();
        let account = keyring_account("home");
        add(&mut config, &paths, account.clone(), Some(Secret::new("hunter2".into()))).unwrap();
        assert_eq!(credentials::resolve(&account).unwrap().expose(), "hunter2");
        assert!(paths.config_file().exists());
    }

    #[test]
    fn add_refuses_a_keyring_account_without_a_secret_and_writes_nothing() {
        let (_dir, paths) = home();
        let mut config = Config::default();
        let err = add(&mut config, &paths, keyring_account("nosecret"), None).unwrap_err();
        assert!(matches!(err, AddError::MissingSecret(ref name) if name == "nosecret"), "{err}");
        assert!(!paths.config_file().exists());
        assert!(config.accounts.is_empty());
    }

    #[test]
    fn add_replaces_an_account_of_the_same_name() {
        let (_dir, paths) = home();
        let mut config = Config::default();
        add(&mut config, &paths, command_account("work"), None).unwrap();
        let mut changed = command_account("work");
        changed.port = 1993;
        add(&mut config, &paths, changed, None).unwrap();
        let saved = Config::load(&paths.config_file()).unwrap();
        assert_eq!(saved.accounts.len(), 1);
        assert_eq!(saved.accounts[0].port, 1993);
    }

    #[test]
    fn add_leaves_config_untouched_on_a_validation_error() {
        let (_dir, paths) = home();
        let mut config = Config::default();
        add(&mut config, &paths, command_account("work"), None).unwrap();
        let before = std::fs::read_to_string(paths.config_file()).unwrap();
        let err = add(&mut config, &paths, command_account("my work"), None).unwrap_err();
        assert!(err.to_string().contains("may only contain letters, digits"), "{err}");
        assert_eq!(std::fs::read_to_string(paths.config_file()).unwrap(), before);
        assert_eq!(config.accounts.len(), 1);
    }

    #[test]
    fn await_started_waits_for_the_daemon_to_pick_up_the_account() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::under(home.path());
        write_config(&paths);
        let daemon = TestDaemon::serve(home, paths.clone(), options(recording_connector(), None));
        let client = Client::connect_or_start(&daemon.paths).unwrap();
        let mut config = Config::load(&paths.config_file()).unwrap();
        add(&mut config, &paths, command_account("late"), None).unwrap();
        await_started(&client, "late", Duration::from_secs(10)).unwrap();
        assert!(client.status().unwrap().accounts.iter().any(|a| a.name == "late"));
        let err = await_started(&client, "never", Duration::from_millis(300)).unwrap_err();
        assert!(err.to_string().contains("never"), "{err}");
    }
}
```

Add `pub mod account;` to `src/lib.rs`, alphabetically first.

- [ ] **Step 4: Run them to see them fail**

Run: `cargo test account::tests`
Expected: compile errors, `add`, `await_started` and `AddError` are not defined.

- [ ] **Step 5: Implement the module**

Above the test module in `src/account.rs`:

```rust
use std::time::{Duration, Instant};

use anyhow::bail;

use crate::config::{AccountConfig, Config, ConfigError, PasswordSource};
use crate::credentials::{self, CredentialError, Secret};
use crate::daemon::Client;
use crate::paths::Paths;

/// How often `await_started` asks the daemon again.
const POLL: Duration = Duration::from_millis(100);

#[derive(Debug, thiserror::Error)]
pub enum AddError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Credential(#[from] CredentialError),
    #[error("account '{0}' keeps its password in the keyring, so a password is required")]
    MissingSecret(String),
}

/// Validates the account, stores a keyring password, then saves config.toml; an account of the same name is replaced.
/// The daemon watches config.toml and starts the account's sync itself, so a wrong login shows as that sync's error.
pub fn add(
    config: &mut Config,
    paths: &Paths,
    account: AccountConfig,
    secret: Option<Secret>,
) -> Result<(), AddError> {
    let mut candidate = config.clone();
    candidate.accounts.retain(|a| a.name != account.name);
    candidate.accounts.push(account.clone());
    candidate.validate()?;
    if matches!(account.password, PasswordSource::Keyring { .. }) {
        let secret = secret.ok_or_else(|| AddError::MissingSecret(account.name.clone()))?;
        credentials::store(&account.name, &secret)?;
    }
    candidate.save(&paths.config_file())?;
    *config = candidate;
    Ok(())
}

/// Waits until the daemon lists `name`, which it does within its config poll; a command for the account before that
/// would be refused as unknown.
pub fn await_started(client: &Client, name: &str, timeout: Duration) -> anyhow::Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        if client.status()?.accounts.iter().any(|a| a.name == name) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!(
                "the daemon did not pick up account '{name}' within {} s",
                timeout.as_secs_f32()
            );
        }
        std::thread::sleep(POLL);
    }
}
```

- [ ] **Step 6: Run the tests**

Run: `cargo test account::tests`
Expected: all six PASS. The daemon test takes up to ~3 s because the daemon polls config.toml every 2 s.

- [ ] **Step 7: Commit**

```bash
git add src/account.rs src/lib.rs src/credentials.rs
git commit -m "feat(account): add an account through one library function, mock keyring in tests"
```

---

### Task 2: `postvak account add` saves, then syncs through the daemon

**Files:**
- Modify: `src/cli/mod.rs:1123-1175` (`cmd_account_add`), `src/cli/mod.rs:14` (imports)
- Test: `tests/cli.rs`

**Interfaces:**
- Consumes: `postvak::account::{add, await_started}` from Task 1; `sync_account(&Client, &str) -> bool` and `Client::connect_or_start` already in `cli/mod.rs`.

- [ ] **Step 1: Write the failing black-box test**

Append to `tests/cli.rs`, next to `account_add_rejects_invalid_name`:

```rust
/// The wizard's answers in prompt order: name, host, port, CA file, username, address, storage, password command.
const ADD_ANSWERS: &str = "late\n127.0.0.1\n1\n\nme@example.com\n\nc\nprintf x\n";

#[test]
fn account_add_saves_then_reports_the_first_sync() {
    let (home, _store) = seeded_home(&[]);
    let out = postvak_stdin(home.path(), &["account", "add"], ADD_ANSWERS);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stdout.contains("saved to"), "{stdout}");
    assert!(!stdout.contains("Testing login"), "{stdout}");
    let config = std::fs::read_to_string(home.path().join("config/config.toml")).unwrap();
    assert!(config.contains("name = \"late\""), "{config}");
    assert!(config.contains("command = \"printf x\""), "{config}");
    // Port 1 refuses. Whether the daemon answers the first sync with a refusal or a pass that then goes offline
    // depends on timing, so only the report's account name is asserted, not its outcome.
    assert!(stdout.contains("late") || stderr.contains("[late]"), "{stdout}\n{stderr}");
}
```

`postvak_stdin` exists at `tests/cli.rs:459`; `seeded_home` writes `ONE_ACCOUNT`, so the home already has account `work`.

- [ ] **Step 2: Run it to see it fail**

Run: `cargo test --test cli account_add_saves`
Expected: FAIL, stdout contains "Testing login" and stderr lacks "first sync".

- [ ] **Step 3: Rewrite `cmd_account_add`**

Replace the body from `let account = AccountConfig { ... }` to the end of the function in `src/cli/mod.rs`:

```rust
    let account = AccountConfig {
        port,
        address,
        ca_file,
        ..AccountConfig::new(&name, &host, &username, password)
    };
    let secret = match &account.password {
        PasswordSource::Command { .. } => None,
        PasswordSource::Keyring { .. } => {
            Some(Secret::new(rpassword::prompt_password("Password: ")?))
        }
    };
    postvak::account::add(&mut config, paths, account, secret)?;
    println!("saved to {}", paths.config_file().display());
    let client = Client::connect_or_start(paths)?;
    postvak::account::await_started(&client, &name, ACCOUNT_START)?;
    if !sync_account(&client, &name) {
        bail!("the first sync of '{name}' failed; check the login and run `postvak account add` again");
    }
    Ok(())
}
```

Add near the other constants at the top of `src/cli/mod.rs`:

```rust
/// How long `account add` waits for the daemon to list the new account; it polls config.toml every 2 s.
const ACCOUNT_START: Duration = Duration::from_secs(10);
```

Remove `use postvak::mail_ops::MailOps;` and change `use postvak::credentials::{self, Secret};` to `use postvak::credentials::Secret;`. Change the clap doc comment on `AccountCommand::Add` from "Interactively add an IMAP account and test the login" to `#[command(about = postvak::help::ACCOUNT_ADD)]` once Task 3 adds that constant; for now leave the doc comment as "Interactively add an IMAP account; the daemon syncs it right away".

- [ ] **Step 4: Run the tests**

Run: `cargo test --test cli account_add`
Expected: both account_add tests PASS.

- [ ] **Step 5: Clippy and commit**

Run: `cargo clippy --all-targets --all-features -- -D warnings`
Expected: clean; an unused import of `credentials` or `MailOps` is the usual failure.

```bash
git add src/cli/mod.rs tests/cli.rs
git commit -m "feat(cli): account add saves first and reports the daemon's first sync"
```

---

### Task 3: Shared help texts and JSON rows

**Files:**
- Modify: `src/help.rs`, `src/output.rs`, `src/cli/mod.rs` (the `about`s of `account add`, `account list`, `daemon status`, `attachment save`, `trash purge`, `rules apply-existing`; `--json` on `daemon status`), `docs/src/cli.md` (blessed)

**Interfaces:**
- Produces: `help::{ACCOUNT_ADD, ACCOUNT_LIST, ATTACHMENT_SAVE, RULES_APPLY_EXISTING, STATUS, TRASH_PURGE}`; `output::planned(account: &str, planned: &actions::Planned) -> Value`, `output::rule_applied(account: &str, evaluated: usize, actions: usize, errors: &[String]) -> Value`, `output::status(status: &Status) -> serde_json::Result<Value>`.

- [ ] **Step 1: Write the failing unit tests in `src/output.rs`**

Add to its `tests` module:

```rust
    #[test]
    fn planned_rows_name_the_rule_folder_uid_and_action() {
        let planned = crate::actions::Planned {
            rule: "codes".into(),
            message: Message {
                folder: "INBOX".into(),
                uid: 7,
                subject: Some("Your code".into()),
                ..Default::default()
            },
            action: crate::rules::Action::Flag,
        };
        assert_eq!(
            planned_row("work", &planned),
            serde_json::json!({ "account": "work", "rule": "codes", "folder": "INBOX", "uid": 7, "action": "flag", "subject": "Your code" })
        );
    }

    #[test]
    fn rule_applied_rows_carry_counts_and_errors() {
        assert_eq!(
            rule_applied("work", 12, 3, &["x".to_string()]),
            serde_json::json!({ "account": "work", "evaluated": 12, "actions": 3, "errors": ["x"] })
        );
    }
```

Check `Action::Flag.label()` prints `flag` by reading `src/rules/mod.rs` (`fn label`); use whatever it prints.

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test output::tests`
Expected: compile errors for `planned_row` and `rule_applied`.

- [ ] **Step 3: Add the rows and the help texts**

In `src/output.rs`, after `rule`:

```rust
/// One row of a dry run: what `rule` would do to the message.
pub fn planned_row(account: &str, planned: &Planned) -> Value {
    json!({
        "account": account, "rule": planned.rule, "folder": planned.message.folder,
        "uid": planned.message.uid, "action": planned.action.label(), "subject": planned.message.subject,
    })
}

/// The outcome of running a rule over an account's existing mail.
pub fn rule_applied(account: &str, evaluated: usize, actions: usize, errors: &[String]) -> Value {
    json!({ "account": account, "evaluated": evaluated, "actions": actions, "errors": errors })
}

/// The daemon's status as it serialises; the CLI's `daemon status --json` and the MCP `status` tool share it.
pub fn status(status: &Status) -> serde_json::Result<Value> {
    serde_json::to_value(status)
}
```

with `use crate::actions::Planned;` and `use crate::daemon::wire::Status;` added to the imports.

In `src/help.rs`, alphabetically:

```rust
pub const ACCOUNT_ADD: &str = "Add an IMAP account; the daemon syncs it right away, and a wrong login shows as that sync's error";
pub const ACCOUNT_LIST: &str = "Each configured account: name, username and server; never the password source";
pub const ATTACHMENT_SAVE: &str = "Save attachment N, as numbered by the attachment list, into a directory; returns the written path";
pub const RULES_APPLY_EXISTING: &str = "Run one enabled rule against mail that predates it; dry run lists what it would do";
pub const STATUS: &str = "The daemon's pid, version, uptime, client count and each account's activity";
pub const TRASH_PURGE: &str = "Remove .eml backups older than the account's retention period";
```

In `src/cli/mod.rs`, replace the doc comments with `#[command(about = postvak::help::X)]` on: `AccountCommand::Add` (ACCOUNT_ADD), `AccountCommand::List` (ACCOUNT_LIST), `DaemonCommand::Status` (STATUS), `AttachmentCommand::Save` (ATTACHMENT_SAVE), `TrashCommand::Purge` (TRASH_PURGE), `RulesCommand::ApplyExisting` (RULES_APPLY_EXISTING). Add `--json` to `daemon status`:

```rust
    #[command(about = postvak::help::STATUS)]
    Status {
        #[arg(long)]
        json: bool,
    },
```

and in `cmd_daemon`:

```rust
        DaemonCommand::Status { json } => {
            let status = client.status()?;
            if json {
                println!("{}", postvak::output::status(&status)?);
            } else {
                print_status(&status);
            }
            Ok(())
        }
```

Make MCP's `rules_test` use the shared row: in `src/mcp/backend.rs`, replace the `rows.push(json!({ "account": acc.name, "rule": p.rule, ... }))` block inside `rules_test` with `rows.push(output::planned_row(&acc.name, &p));`.

- [ ] **Step 4: Run the tests and bless the CLI docs**

Run: `cargo test output::tests && cargo test mcp::tests::rules_test_previews_subjects`
Expected: PASS.

Run: `POSTVAK_BLESS=1 cargo test --test cli_docs` (if the blessing test lives elsewhere, `ggrep -rl POSTVAK_BLESS tests/` names it) then `cargo test`
Expected: `docs/src/cli.md` changes for the six commands and `daemon status --json`; the second run passes.

- [ ] **Step 5: Commit**

```bash
git add src/help.rs src/output.rs src/cli/mod.rs src/mcp/backend.rs docs/src/cli.md
git commit -m "feat: share the help texts and JSON rows of account, status, attachment save, trash purge and apply-existing"
```

---

### Task 4: MCP `accounts` and `status`

**Files:**
- Modify: `src/mcp/tools.rs` (catalog, dispatch), `src/mcp/backend.rs`, `src/mcp/tests.rs`, `docs/src/mcp.md`

**Interfaces:**
- Consumes: `output::account`, `output::status` (Task 3), `LazyClient::client()`.
- Produces: `Backend::accounts() -> Result<Vec<Value>>`, `Backend::status() -> Result<Value>`.

- [ ] **Step 1: Write the failing tests in `src/mcp/tests.rs`**

Extend the `"read,rules:propose"` case of `tools_list_per_scope_set_is_exact_and_alphabetical` to:

```rust
            &[
                "accounts",
                "folders",
                "list",
                "log",
                "rules_check",
                "rules_list",
                "rules_propose",
                "rules_schema",
                "rules_test",
                "search",
                "status",
                "sync",
                "trash_list",
            ],
```

Add:

```rust
#[tokio::test]
async fn accounts_lists_the_visible_accounts_without_secrets() {
    let fx = fixture(&["home", "work"]);
    let client = connect(&fx, "read", &["work"]).await;
    let report = rows(&call(&client, "accounts", json!({})).await);
    assert_eq!(report.len(), 1);
    assert_eq!(report[0]["account"], "work");
    assert_eq!(report[0]["port"], 1);
    assert!(report[0].get("password").is_none());
    assert!(!report[0].to_string().contains("printf"));
}

#[tokio::test]
async fn status_reports_the_daemon_and_the_visible_accounts() {
    let fx = fixture_with_daemon(&["home", "work"], recording_connector());
    let client = connect(&fx, "read", &["work"]).await;
    let report = call(&client, "status", json!({})).await;
    let report = report.structured_content.unwrap();
    assert!(report["pid"].as_u64().is_some(), "{report}");
    let names: Vec<&str> = report["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["work"]);
}
```

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test mcp::tests::accounts_lists mcp::tests::status_reports mcp::tests::tools_list_per_scope`
Expected: FAIL with "unknown tool accounts" / "unknown tool status".

- [ ] **Step 3: Add the tools**

In `src/mcp/tools.rs` `catalog`, in alphabetical position:

```rust
        ToolDef::new::<NoArgs>("accounts", Scope::Read, Effect::ReadOnly, help::ACCOUNT_LIST),
```
```rust
        ToolDef::new::<NoArgs>("status", Scope::Read, Effect::ReadOnly, help::STATUS),
```

In `dispatch`:

```rust
        "accounts" => {
            let _: NoArgs = args(arguments)?;
            rows(backend.accounts()?)
        }
```
```rust
        "status" => {
            let _: NoArgs = args(arguments)?;
            backend.status()
        }
```

In `src/mcp/backend.rs`:

```rust
    /// The visible accounts' listing rows; never the password source.
    pub fn accounts(&self) -> Result<Vec<Value>> {
        Ok(self.select(None)?.iter().map(|acc| output::account(acc)).collect())
    }

    /// The daemon's status, with only the visible accounts.
    pub fn status(&self) -> Result<Value> {
        let mut status = self.daemon.client()?.status()?;
        status
            .accounts
            .retain(|account| self.is_visible(Some(account.name.as_str())));
        Ok(output::status(&status)?)
    }
```

- [ ] **Step 4: Run the tests**

Run: `cargo test mcp::tests`
Expected: PASS, including `annotations_mark_reads_and_destructive_tools` (both new tools are read-only).

- [ ] **Step 5: Document and commit**

In `docs/src/mcp.md`, add `accounts` and `status` to the `read` row of the scope table (keep the list alphabetical), and under `read:` in the Tools section:

```
- `accounts` lists the visible accounts: name, username, host, port, address and aliases. Never the password source.
- `status` is `postvak daemon status --json`: the daemon's pid, version, uptime and each visible account's activity.
```

```bash
git add src/mcp/tools.rs src/mcp/backend.rs src/mcp/tests.rs docs/src/mcp.md
git commit -m "feat(mcp): accounts and status tools"
```

---

### Task 5: MCP `attachment_save`

**Files:**
- Modify: `src/mcp/tools.rs`, `src/mcp/backend.rs`, `src/mcp/tests.rs`, `docs/src/mcp.md`

**Interfaces:**
- Consumes: `message::save_attachment(raw: &[u8], n: usize, dir: &Path) -> Result<PathBuf, _>` (used by the CLI at `cli/mod.rs:783`).
- Produces: `Backend::attachment_save(account, folder, uid, n, dir: &Path) -> Result<Value>`.

- [ ] **Step 1: Write the failing tests**

`attachments_lists_names_and_sizes` at `src/mcp/tests.rs:592` seeds a message with an attachment inline. Lift its raw message into a constant next to `add_with_raw` and a helper, then use both in the existing test and the two new ones:

```rust
/// A message with one PDF attachment, `bill.pdf`, 6 bytes.
const ATTACHED: &str = "From: a@example.com\r\nSubject: Files\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=b\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\nsee attached\r\n--b\r\nContent-Type: application/pdf\r\nContent-Disposition: attachment; filename=\"bill.pdf\"\r\n\r\n%PDF-1\r\n--b--\r\n";

fn add_attached(fx: &Fixture, account: &str, uid: u32) {
    fx.add(account, fixture_message("INBOX", uid, "Files", "see attached"));
    fx.store(account)
        .set_raw("INBOX", uid, ATTACHED.as_bytes(), "see attached")
        .unwrap();
}

#[tokio::test]
async fn attachment_save_writes_the_file_and_returns_its_path() {
    let fx = fixture(&["work"]);
    add_attached(&fx, "work", 1);
    let dir = fx.paths.cache_dir.join("saved");
    std::fs::create_dir_all(&dir).unwrap();
    let client = connect(&fx, "read:bodies", &[]).await;
    let report = call(&client, "attachment_save", json!({ "uid": 1, "n": 1, "dir": dir })).await;
    let report = report.structured_content.unwrap();
    let path = std::path::PathBuf::from(report["path"].as_str().unwrap());
    assert!(path.starts_with(&dir), "{report}");
    assert!(path.is_file());
    assert_eq!(report["account"], "work");
}

#[tokio::test]
async fn attachment_save_needs_an_absolute_dir() {
    let fx = fixture(&["work"]);
    add_attached(&fx, "work", 1);
    let client = connect(&fx, "read:bodies", &[]).await;
    let relative = call(&client, "attachment_save", json!({ "uid": 1, "n": 1, "dir": "out" })).await;
    assert!(error_text(&relative).contains("absolute"), "{relative:?}");
    let missing = fx.paths.cache_dir.join("nowhere");
    let gone = call(&client, "attachment_save", json!({ "uid": 1, "n": 1, "dir": missing })).await;
    assert!(error_text(&gone).contains("nowhere"), "{gone:?}");
}
```

Also extend the `"read:bodies"` case of `tools_list_per_scope_set_is_exact_and_alphabetical` to `&["attachment_save", "attachments", "show"]`.

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test mcp::tests::attachment_save`
Expected: FAIL with "unknown tool attachment_save".

- [ ] **Step 3: Implement**

In `src/mcp/tools.rs`:

```rust
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct AttachmentSaveArgs {
    /// The uid, as `list` returns it
    uid: u32,
    /// The attachment's index, as `attachments` numbers it
    n: usize,
    /// An absolute path of an existing directory to save into
    dir: std::path::PathBuf,
    /// Required when several accounts are visible
    account: Option<String>,
    /// Folder name as `folders` lists it; default INBOX
    #[serde(default = "inbox")]
    folder: String,
}
```

In `catalog`, after `archive`:

```rust
        ToolDef::new::<AttachmentSaveArgs>(
            "attachment_save",
            Scope::ReadBodies,
            Effect::Changes,
            help::ATTACHMENT_SAVE,
        ),
```

In `dispatch`:

```rust
        "attachment_save" => {
            let a: AttachmentSaveArgs = args(arguments)?;
            backend.attachment_save(a.account.as_deref(), &a.folder, a.uid, a.n, &a.dir)
        }
```

In `src/mcp/backend.rs`, after `attachments`:

```rust
    /// Saves attachment `n` into `dir`, which must be absolute: the server's working directory is the host's, not
    /// the agent's.
    pub fn attachment_save(
        &self,
        account: Option<&str>,
        folder: &str,
        uid: u32,
        n: usize,
        dir: &std::path::Path,
    ) -> Result<Value> {
        if !dir.is_absolute() {
            bail!("dir must be an absolute path");
        }
        if !dir.is_dir() {
            bail!("no directory {}", dir.display());
        }
        let (acc, store, msg) = self.message(account, folder, uid)?;
        let raw = self.daemon.raw_message(&acc.name, &store, &msg)?;
        let path = message::save_attachment(&raw, n, dir)
            .with_context(|| format!("saving attachment {n}"))?;
        Ok(json!({ "account": acc.name, "folder": folder, "uid": uid, "n": n, "path": path }))
    }
```

Add `"attachment_save"` to the `changes` list in `annotations_mark_reads_and_destructive_tools`.

- [ ] **Step 4: Run the tests**

Run: `cargo test mcp::tests`
Expected: PASS.

- [ ] **Step 5: Document and commit**

`docs/src/mcp.md`: add `attachment_save` to the `read:bodies` row of the scope table, and under `read:bodies:` in Tools:

```
- `attachment_save` saves attachment `n` into `dir`, an absolute path of an existing directory, and returns the written `path`. It fetches the message once if it is not stored yet.
```

```bash
git add src/mcp/tools.rs src/mcp/backend.rs src/mcp/tests.rs docs/src/mcp.md
git commit -m "feat(mcp): attachment_save tool"
```

---

### Task 6: Docs and the first PR

**Files:**
- Modify: `docs/src/accounts.md:3`, `docs/src/index.md:14`, `README.md` (the same line, since it mirrors `docs/src/index.md`)

- [ ] **Step 1: Update the prose**

`docs/src/accounts.md` first paragraph: replace "tests the login and writes `config.toml`" with "writes `config.toml` and syncs the account once through the background daemon; a wrong login shows as that sync's error, and running it again with the same name replaces the account". In `docs/src/index.md` and the identical line of `README.md`, change the comment `# asks for host, user and password, then tests the login` to `# asks for host, user and password, then syncs once`.

- [ ] **Step 2: Full check**

Run: `cargo fmt --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test && cargo machete && cargo audit`
Expected: all pass. `cargo test` includes the docs test that compares `README.md` with `docs/src/index.md`.

- [ ] **Step 3: Commit and open the PR**

```bash
git add docs/src/accounts.md docs/src/index.md README.md
git commit -m "docs: account add syncs instead of testing the login"
git push -u origin HEAD
```

PR title: `feat: account add through the library, and MCP accounts, status and attachment_save`. The body links the spec and lists the platforms the tests ran on.

---

# Phase 2: the MCP write tools (`feat/surface-parity-mcp-write`)

Branch from `main` once the first PR is merged: `git switch -c feat/surface-parity-mcp-write --no-track origin/main`.

### Task 7: The `accounts:write` scope and a backend that reloads its accounts

**Files:**
- Modify: `src/mcp/mod.rs:21-48`, `src/mcp/backend.rs:19-95`, `src/mcp/tests.rs:184-194`, `src/cli/mod.rs` (the two `--scopes` help strings; `ggrep -n "scopes" src/cli/mod.rs` finds them), `docs/src/mcp.md` (scope table)

**Interfaces:**
- Produces: `Scope::AccountsWrite` printing as `accounts:write`; `Backend` holds `config: RwLock<Config>` and `only: Vec<String>`, with `select` returning `Vec<AccountConfig>` and `single` returning `AccountConfig` (owned); `Backend::replace_config(&self, Config)`.

- [ ] **Step 1: Write the failing tests**

In `src/mcp/tests.rs`, change `backend_refuses_no_accounts_and_unknown_accounts` to:

```rust
#[test]
fn backend_without_accounts_opens_but_every_lookup_says_so() {
    let fx = fixture(&[]);
    let backend = Backend::new(&fx.config(), &fx.paths, &[]).unwrap();
    let err = backend.folders(None).unwrap_err().to_string();
    assert!(err.contains("no accounts configured"), "{err}");
    let fx = fixture(&["work"]);
    let err = Backend::new(&fx.config(), &fx.paths, &["play".into()])
        .err()
        .unwrap()
        .to_string();
    assert!(err.contains("play"), "{err}");
}
```

In `scopes_parse_and_unknown_ones_name_the_valid_ones`, add after the first assertion:

```rust
    assert!(parse_scopes("accounts:write").unwrap().contains(&super::Scope::AccountsWrite));
```

Add a case to `tools_list_per_scope_set_is_exact_and_alphabetical` (the array becomes `[(&str, &[&str]); 5]`): `("accounts:write", &[])` for now; Task 8 fills it.

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test mcp::tests::backend_without_accounts mcp::tests::scopes_parse`
Expected: FAIL.

- [ ] **Step 3: Add the scope**

In `src/mcp/mod.rs`:

```rust
pub enum Scope {
    AccountsWrite,
    MailModify,
    Read,
    ReadBodies,
    RulesPropose,
    RulesWrite,
}

impl Scope {
    pub const ALL: [Scope; 6] = [
        Scope::Read,
        Scope::ReadBodies,
        Scope::RulesPropose,
        Scope::RulesWrite,
        Scope::MailModify,
        Scope::AccountsWrite,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Scope::AccountsWrite => "accounts:write",
            Scope::MailModify => "mail:modify",
            Scope::Read => "read",
            Scope::ReadBodies => "read:bodies",
            Scope::RulesPropose => "rules:propose",
            Scope::RulesWrite => "rules:write",
        }
    }
}
```

In `src/cli/mod.rs`, both `--scopes` help strings that list the valid scopes gain `accounts:write` at the end.

- [ ] **Step 4: Make the backend reload its accounts**

In `src/mcp/backend.rs`, replace the struct and `build`, `select`, `single`, `is_visible`, `narrowed`, and `approve`'s use of `all_accounts`:

```rust
pub struct Backend {
    /// Replaced by `account_add`, so a server outlives the config it started with.
    config: RwLock<Config>,
    /// The `--account` filter; empty shows every account.
    only: Vec<String>,
    daemon: LazyClient,
    paths: Paths,
}
```

```rust
    fn build(
        config: &Config,
        paths: &Paths,
        only: &[String],
        daemon: LazyClient,
    ) -> Result<Backend> {
        if let Some(name) = only.iter().find(|n| config.account(n).is_none()) {
            bail!("no account named '{name}'");
        }
        Ok(Backend {
            config: RwLock::new(config.clone()),
            only: only.to_vec(),
            daemon,
            paths: paths.clone(),
        })
    }

    fn config(&self) -> Config {
        self.config.read().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Every configured account's name, visible or not: an approved rule's clock restarts in all of them.
    fn all_accounts(&self) -> Vec<String> {
        self.config().accounts.iter().map(|a| a.name.clone()).collect()
    }

    /// The accounts this server shows.
    fn visible(&self) -> Vec<AccountConfig> {
        self.config()
            .accounts
            .into_iter()
            .filter(|a| self.only.is_empty() || self.only.contains(&a.name))
            .collect()
    }

    /// The named visible account, or every visible one; none at all is an error that says what to do.
    fn select(&self, name: Option<&str>) -> Result<Vec<AccountConfig>> {
        let visible = self.visible();
        if visible.is_empty() {
            bail!("no accounts configured; call account_add or run `postvak account add`");
        }
        match name {
            Some(name) => Ok(vec![
                visible
                    .into_iter()
                    .find(|a| a.name == name)
                    .with_context(|| format!("no account named '{name}'"))?,
            ]),
            None => Ok(visible),
        }
    }

    /// One account: the named one, or the only visible one.
    fn single(&self, name: Option<&str>) -> Result<AccountConfig> {
        let mut selected = self.select(name)?;
        match (name, selected.len()) {
            (Some(_), _) | (None, 1) => Ok(selected.swap_remove(0)),
            (None, _) => bail!("several accounts are visible; pass account"),
        }
    }

    fn is_visible(&self, account: Option<&str>) -> bool {
        account.is_none_or(|a| self.visible().iter().any(|acc| acc.name == a))
    }

    /// Whether `--account` hides some configured account; a rule without `account` then reaches beyond this server.
    fn narrowed(&self) -> bool {
        self.visible().len() < self.all_accounts().len()
    }
```

`message` now returns `(AccountConfig, Store, Message)`. Every caller that wrote `for acc in self.select(account)?` keeps working on owned values; `approve` uses `&self.all_accounts()`; `propose`'s `self.single(None)?.name.clone()` becomes `self.single(None)?.name`; `ensure_rule_visible` builds its `visible` names from `self.visible()` instead of `self.accounts`. After the edit, `ggrep -n "self.accounts\|self.all_accounts\b" src/mcp/backend.rs` must find nothing but the two helpers. Add `use std::sync::{PoisonError, RwLock};`. Add:

```rust
    /// The config after `account::add`, so the next call sees the new account.
    fn replace_config(&self, config: Config) {
        *self.config.write().unwrap_or_else(PoisonError::into_inner) = config;
    }
```

(It is used in Task 8; `#[allow(dead_code)]` is not needed if Task 8 lands in the same PR before clippy runs on CI, but add it for now and remove it in Task 8.)

- [ ] **Step 5: Run the whole MCP suite**

Run: `cargo test mcp::tests && cargo clippy --all-targets --all-features -- -D warnings`
Expected: PASS and clean. The `select` on an empty config changes one message: `a_narrowed_backend_syncs_its_account_only_and_refuses_the_other` and friends still pass because they have accounts.

- [ ] **Step 6: Document and commit**

`docs/src/mcp.md` scope table gains a row:

```
| `accounts:write` | Adding an account with a password command. Never a plain password. | `account_add` |
```

and after "`rules:write` lets the agent approve its own rules…" add: "`accounts:write` lets the agent add mail accounts. It is not in the default, and it is refused on a server limited with `--account`."

```bash
git add src/mcp/mod.rs src/mcp/backend.rs src/mcp/tests.rs src/cli/mod.rs docs/src/mcp.md
git commit -m "feat(mcp): accounts:write scope, and a backend that can reload its accounts"
```

Then `POSTVAK_BLESS=1 cargo test` for the `--scopes` help change and commit `docs/src/cli.md` with `docs: bless cli.md for the accounts:write scope`.

---

### Task 8: MCP `account_add`

**Files:**
- Modify: `src/mcp/tools.rs`, `src/mcp/backend.rs`, `src/mcp/tests.rs`, `docs/src/mcp.md`, `docs/src/agent-guide.md`

**Interfaces:**
- Consumes: `account::{add, await_started}` (Task 1), `Backend::replace_config` and `Backend::sync` (Task 7), `output::account`.
- Produces: `Backend::account_add(account: AccountConfig) -> Result<Value>` returning `{ "account": row, "sync": sync-row }`.

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test]
async fn account_add_returns_the_account_and_its_first_sync() {
    let fx = fixture_with_daemon(&["work"], recording_connector());
    let client = connect(&fx, "accounts:write", &[]).await;
    let report = call(
        &client,
        "account_add",
        json!({ "name": "late", "host": "127.0.0.1", "port": 1, "username": "me@example.org", "password_command": "printf x" }),
    )
    .await;
    let report = report.structured_content.unwrap();
    assert_eq!(report["account"]["account"], "late", "{report}");
    assert!(report["account"].get("password").is_none());
    assert_eq!(report["sync"]["account"], "late", "{report}");
    let config = fx.config();
    assert!(config.accounts.iter().any(|a| a.name == "late"));
    // The server sees it too, without a restart.
    let client = connect(&fx, "read", &[]).await;
    let report = rows(&call(&client, "accounts", json!({})).await);
    assert_eq!(report.len(), 2);
}

#[tokio::test]
async fn account_add_rejects_a_plain_password_and_a_narrowed_server() {
    let fx = fixture_with_daemon(&["work"], recording_connector());
    let client = connect(&fx, "accounts:write", &[]).await;
    let refused = call(
        &client,
        "account_add",
        json!({ "name": "x", "host": "h", "username": "u@example.com", "password": "hunter2" }),
    )
    .await;
    assert!(error_text(&refused).contains("password"), "{refused:?}");
    assert!(!fx.config().accounts.iter().any(|a| a.name == "x"));
    let narrowed = connect(&fx, "accounts:write", &["work"]).await;
    let refused = call(
        &narrowed,
        "account_add",
        json!({ "name": "y", "host": "h", "username": "u@example.com", "password_command": "printf x" }),
    )
    .await;
    assert!(error_text(&refused).contains("--account"), "{refused:?}");
}
```

Set the `"accounts:write"` case of `tools_list_per_scope_set_is_exact_and_alphabetical` to `&["account_add"]`, and add `"account_add"` to the `changes` list of `annotations_mark_reads_and_destructive_tools`.

The recording connector accepts any login, so the first sync of `late` succeeds; the result shape is what is tested. The second `connect` in the first test reads `fx.config()` again, which is the saved file.

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test mcp::tests::account_add`
Expected: FAIL with "unknown tool account_add".

- [ ] **Step 3: Implement**

In `src/mcp/tools.rs`:

```rust
fn imaps() -> u16 {
    993
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct AccountAddArgs {
    /// Letters, digits, - and _; an existing account of this name is replaced
    name: String,
    /// IMAP host, TLS on `port`
    host: String,
    #[serde(default = "imaps")]
    port: u16,
    /// The IMAP login
    username: String,
    /// A shell command that prints the password, such as `pass show mail/work`; the password itself is never taken
    password_command: String,
    /// The mail address, when the username is not one
    address: Option<String>,
    /// Absolute path of a PEM file with an extra trusted root certificate
    ca_file: Option<std::path::PathBuf>,
}
```

In `catalog`, first entry:

```rust
        ToolDef::new::<AccountAddArgs>(
            "account_add",
            Scope::AccountsWrite,
            Effect::Changes,
            help::ACCOUNT_ADD,
        ),
```

In `dispatch`:

```rust
        "account_add" => {
            let a: AccountAddArgs = args(arguments)?;
            let password = crate::config::PasswordSource::Command {
                command: a.password_command,
            };
            let account = crate::config::AccountConfig {
                port: a.port,
                address: a.address,
                ca_file: a.ca_file,
                ..crate::config::AccountConfig::new(&a.name, &a.host, &a.username, password)
            };
            backend.account_add(account)
        }
```

`crate::config` is allowed in `tools.rs` by the architecture rule (only `store`, `daemon` and `rules` are reserved for the backend).

In `src/mcp/backend.rs`:

```rust
    /// Saves the account and reports its first sync; refused while `--account` hides accounts, since the new one
    /// would be invisible to this server.
    pub fn account_add(&self, account: AccountConfig) -> Result<Value> {
        if !self.only.is_empty() {
            bail!("this server is limited with --account and cannot add accounts");
        }
        let mut config = self.config();
        let name = account.name.clone();
        crate::account::add(&mut config, &self.paths, account.clone(), None)?;
        self.replace_config(config);
        crate::account::await_started(&self.daemon.client()?, &name, ACCOUNT_START)?;
        let sync = self.sync(Some(&name))?.swap_remove(0);
        Ok(json!({ "account": output::account(&account), "sync": sync }))
    }
```

with `const ACCOUNT_START: Duration = Duration::from_secs(10);` and `use std::time::Duration;` at the top. Remove the `#[allow(dead_code)]` from `replace_config`.

- [ ] **Step 4: Run the tests**

Run: `cargo test mcp::tests`
Expected: PASS. The first test takes a few seconds for the daemon's config poll.

- [ ] **Step 5: Document and commit**

`docs/src/mcp.md`, a new section under Tools after `mail:modify:`:

```
`accounts:write`:

- `account_add` saves an account with a `password_command`, such as `pass show mail/work`, and returns the account row and the result of its first sync. A wrong login shows as that sync's `error`; call it again with the same `name` to replace the account. It never takes a password; keyring passwords are set with `postvak account add` or in the window.
```

`docs/src/agent-guide.md`, in "Over MCP", add one sentence: "With `accounts:write`, `account_add` adds a mail account from a password command; never ask the human for the password itself."

```bash
git add src/mcp/tools.rs src/mcp/backend.rs src/mcp/tests.rs docs/src/mcp.md docs/src/agent-guide.md
git commit -m "feat(mcp): account_add with a password command"
```

---

### Task 9: MCP `trash_purge`

**Files:**
- Modify: `src/mcp/tools.rs`, `src/mcp/backend.rs`, `src/mcp/tests.rs`, `docs/src/mcp.md`

**Interfaces:**
- Consumes: `Trash::purge(retention_secs: i64, now: i64) -> io::Result<usize>`, `time::now()`.
- Produces: `Backend::trash_purge(account: Option<&str>) -> Result<Vec<Value>>` with rows `{ "account", "removed" }`.

- [ ] **Step 1: Write the failing test**

Read `trash_list_names_backups_and_restore_takes_the_name` at `src/mcp/tests.rs:797` for how it writes a backup with `Trash::save`, then add:

```rust
#[tokio::test]
async fn trash_purge_removes_only_backups_past_the_retention() {
    let fx = fixture(&["work"]);
    let trash = Trash::new(fx.paths.trash_dir("work"));
    let now = crate::time::now();
    let old = trash.save("INBOX", 1, b"Subject: old\r\n\r\n", now - 31 * 86_400).unwrap();
    let fresh = trash.save("INBOX", 2, b"Subject: fresh\r\n\r\n", now - 86_400).unwrap();
    let client = connect(&fx, "mail:modify", &[]).await;
    let report = rows(&call(&client, "trash_purge", json!({})).await);
    assert_eq!(report, vec![json!({ "account": "work", "removed": 1 })]);
    assert!(!old.exists());
    assert!(fresh.exists());
}
```

`use crate::trash::Trash;` is already imported in the tests module if the restore test uses it; otherwise add it. Add `"trash_purge"` to the `"mail:modify"` tool-list case (alphabetically after `trash_list` would be, so the list is `["archive", "delete", "mark", "move", "rules_apply_existing", "trash_purge", "trash_restore"]` once Task 10 lands; for now without `rules_apply_existing`), and to the `destructive` list in `annotations_mark_reads_and_destructive_tools`.

- [ ] **Step 2: Run it to see it fail**

Run: `cargo test mcp::tests::trash_purge`
Expected: FAIL with "unknown tool trash_purge".

- [ ] **Step 3: Implement**

`catalog`:

```rust
        ToolDef::new::<AccountArgs>(
            "trash_purge",
            Scope::MailModify,
            Effect::Destructive,
            help::TRASH_PURGE,
        ),
```

`dispatch`:

```rust
        "trash_purge" => {
            let a: AccountArgs = args(arguments)?;
            rows(backend.trash_purge(a.account.as_deref())?)
        }
```

`backend.rs`, after `trash_list`:

```rust
    /// Deletes each visible account's backups older than its `trash_retention_days`, for good.
    pub fn trash_purge(&self, account: Option<&str>) -> Result<Vec<Value>> {
        let mut rows = Vec::new();
        for acc in self.select(account)? {
            let removed = Trash::new(self.paths.trash_dir(&acc.name))
                .purge(acc.trash_retention_days as i64 * 86_400, time::now())?;
            rows.push(json!({ "account": acc.name, "removed": removed }));
        }
        Ok(rows)
    }
```

- [ ] **Step 4: Run, document, commit**

Run: `cargo test mcp::tests`
Expected: PASS.

`docs/src/mcp.md`: add `trash_purge` to the `mail:modify` row of the scope table and under `mail:modify:` in Tools: "- `trash_purge` deletes the backups older than each account's `trash_retention_days`, for good; it reports `removed` per account." Mention it in the Safety list next to the destructive tools: `delete`, `rules_approve`, `rules_set_enabled` and `trash_purge` are marked destructive.

```bash
git add src/mcp/tools.rs src/mcp/backend.rs src/mcp/tests.rs docs/src/mcp.md
git commit -m "feat(mcp): trash_purge tool"
```

---

### Task 10: MCP `rules_apply_existing`

**Files:**
- Modify: `src/mcp/tools.rs`, `src/mcp/backend.rs`, `src/mcp/tests.rs`, `docs/src/mcp.md`

**Interfaces:**
- Consumes: `rules::{load, compile}`, `CompiledRule::applies_to_account`, `actions::planned`, `output::{planned_row, rule_applied}` (Task 3), `sync::Command::ApplyRule { name }` and `Event::RuleApplied`.
- Produces: `Backend::rules_apply_existing(name: &str, account: Option<&str>, dry_run: bool) -> Result<Vec<Value>>`.

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test]
async fn rules_apply_existing_dry_run_lists_planned_rows() {
    let fx = fixture(&["work"]);
    fx.add("work", fixture_message("INBOX", 1, "Your code", ""));
    std::fs::write(
        fx.paths.rules_file(),
        "[[rules]]\nname = \"codes\"\nmatch.subject = { contains = \"code\" }\nactions = [\"flag\"]\n",
    )
    .unwrap();
    let client = connect(&fx, "mail:modify", &[]).await;
    let report = rows(&call(&client, "rules_apply_existing", json!({ "name": "codes", "dry_run": true })).await);
    assert_eq!(report.len(), 1, "{report:?}");
    assert_eq!(report[0]["rule"], "codes");
    assert_eq!(report[0]["uid"], 1);
    assert_eq!(report[0]["action"], "flag");
}

#[tokio::test]
async fn rules_apply_existing_goes_through_the_daemon() {
    let fx = fixture_with_daemon(&["work"], connector_with_mail());
    std::fs::write(
        fx.paths.rules_file(),
        "[[rules]]\nname = \"greet\"\nmatch.subject = { contains = \"Hi\" }\nactions = [\"flag\"]\n",
    )
    .unwrap();
    let client = connect(&fx, "mail:modify", &[]).await;
    call(&client, "sync", json!({})).await;
    let report = rows(&call(&client, "rules_apply_existing", json!({ "name": "greet" })).await);
    assert_eq!(report.len(), 1, "{report:?}");
    assert_eq!(report[0]["account"], "work");
    assert_eq!(report[0]["evaluated"], 1);
    assert_eq!(report[0]["actions"], 1);
    assert_eq!(report[0]["errors"], json!([]));
}

#[tokio::test]
async fn rules_apply_existing_refuses_disabled_and_hidden_rules() {
    let fx = fixture(&["home", "work"]);
    std::fs::write(
        fx.paths.rules_file(),
        format!(
            "[[rules]]\nname = \"off\"\nenabled = false\nmatch.subject = {{ contains = \"x\" }}\nactions = [\"flag\"]\n\n{}",
            scoped_rule("hidden", "home")
        ),
    )
    .unwrap();
    let client = connect(&fx, "mail:modify", &["work"]).await;
    let off = call(&client, "rules_apply_existing", json!({ "name": "off" })).await;
    assert!(error_text(&off).contains("rule 'off' is disabled"), "{off:?}");
    let hidden = call(&client, "rules_apply_existing", json!({ "name": "hidden" })).await;
    assert!(error_text(&hidden).contains("no rule named 'hidden'"), "{hidden:?}");
    let unknown = call(&client, "rules_apply_existing", json!({ "name": "nope" })).await;
    assert!(error_text(&unknown).contains("no rule named 'nope'"), "{unknown:?}");
}
```

`scoped_rule(name, account)` exists at `src/mcp/tests.rs:446`; check that it produces an enabled rule. Add `rules_apply_existing` to the `mail:modify` tool-list case and to the `changes` annotation list.

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test mcp::tests::rules_apply_existing`
Expected: FAIL with "unknown tool rules_apply_existing".

- [ ] **Step 3: Implement**

`tools.rs`:

```rust
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ApplyExistingArgs {
    /// The rule's name in rules_list; it must be enabled
    name: String,
    /// Only this account; default every visible account the rule applies to
    account: Option<String>,
    /// List what the rule would do from the local store, without touching the server
    #[serde(default)]
    dry_run: bool,
}
```

`catalog`, after `rules_approve`:

```rust
        ToolDef::new::<ApplyExistingArgs>(
            "rules_apply_existing",
            Scope::MailModify,
            Effect::Changes,
            help::RULES_APPLY_EXISTING,
        ),
```

`dispatch`:

```rust
        "rules_apply_existing" => {
            let a: ApplyExistingArgs = args(arguments)?;
            rows(backend.rules_apply_existing(&a.name, a.account.as_deref(), a.dry_run)?)
        }
```

`backend.rs`, after `rules_test`:

```rust
    /// Runs one enabled rule over the existing mail of the visible accounts it applies to, through the daemon; a dry
    /// run plans from the local store instead. A rule of a hidden account reads as absent.
    pub fn rules_apply_existing(
        &self,
        name: &str,
        account: Option<&str>,
        dry_run: bool,
    ) -> Result<Vec<Value>> {
        self.ensure_rule_visible(name, false)?;
        let file = rules::load(&self.paths.rules_file())?;
        let compiled: Vec<_> = rules::compile(&file)?
            .into_iter()
            .filter(|r| r.rule.name == name)
            .collect();
        if compiled.is_empty() {
            bail!("no rule named '{name}'");
        }
        if compiled.iter().any(|r| !r.rule.enabled) {
            bail!("rule '{name}' is disabled; approve or enable it first");
        }
        let mut rows = Vec::new();
        for acc in self.select(account)? {
            if !compiled.iter().any(|r| r.applies_to_account(&acc.name)) {
                continue;
            }
            if dry_run {
                let store = self.store(&acc)?;
                for p in actions::planned(&compiled, &store, &acc, &acc.identity()?, time::now())? {
                    rows.push(output::planned_row(&acc.name, &p));
                }
                continue;
            }
            let command = Command::ApplyRule { name: name.to_string() };
            let (evaluated, actions, errors) =
                self.daemon.request(&acc.name, command, |reply| match reply {
                    Event::RuleApplied {
                        evaluated,
                        actions,
                        errors,
                        ..
                    } => Ok((evaluated, actions, errors)),
                    other => Err(other),
                })?;
            rows.push(output::rule_applied(&acc.name, evaluated, actions, &errors));
        }
        Ok(rows)
    }
```

`ensure_rule_visible` ignores an unknown name, so the "no rule named" check after it covers both the unknown and the hidden case with the same words.

- [ ] **Step 4: Run, document, commit**

Run: `cargo test mcp::tests`
Expected: PASS.

`docs/src/mcp.md`: add `rules_apply_existing` to the `mail:modify` scope row and under `mail:modify:` in Tools: "- `rules_apply_existing` runs one enabled rule over the mail each account already holds, the way `postvak rules apply-existing` does, and reports `evaluated`, `actions` and `errors` per account. With `dry_run` it lists the planned rows from the local store." Also mention in the Safety list that it takes `dry_run`.

```bash
git add src/mcp/tools.rs src/mcp/backend.rs src/mcp/tests.rs docs/src/mcp.md
git commit -m "feat(mcp): rules_apply_existing tool"
```

---

### Task 11: The second PR

- [ ] **Step 1: Full check**

Run: `cargo fmt --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test && cargo machete && cargo audit`
Expected: all pass.

- [ ] **Step 2: Push and open the PR**

```bash
git push -u origin HEAD
```

PR title: `feat(mcp): account_add, trash_purge and rules_apply_existing`. The body links the spec, names the new scope and lists the platforms the tests ran on.

---

# Phase 3: the GUI (`feat/surface-parity-gui`)

Branch from `main` once the second PR is merged. After every GUI task, run the GUI tests on macOS with `cargo test gui::`; snapshots are regenerated once in Task 17 on Linux.

### Task 12: The account form widget

**Files:**
- Create: `src/gui/account.rs`
- Modify: `src/gui/mod.rs` (`mod account;`)

**Interfaces:**
- Produces: `pub(crate) struct AccountForm { name, host, port, username, password, address: String, error: Option<String>, saving: bool }` with `Default`, `Clone`, `PartialEq` and a redacting `Debug`; `AccountForm::account(&self) -> Result<(AccountConfig, Secret), String>`; `AccountForm::complete(&self) -> bool`; `pub(crate) enum FormAction { Changed(AccountForm), Save, Cancel }`; `pub(crate) fn show(form: &AccountForm, ui: &mut egui::Ui) -> Vec<FormAction>`.

- [ ] **Step 1: Write the failing tests**

Create `src/gui/account.rs` with only the test module:

```rust
//! The add-account form, drawn on the first-run window and from the toolbar; `app.rs` and `mod.rs` carry out its
//! actions.

#[cfg(test)]
mod tests {
    use eframe::egui;
    use egui_kittest::Harness;
    use egui_kittest::kittest::{NodeT, Queryable};

    use super::*;

    fn harness(form: AccountForm) -> Harness<'static, (AccountForm, Vec<FormAction>)> {
        Harness::builder()
            .with_size(egui::vec2(520.0, 360.0))
            .build_ui_state(
                |ui, state: &mut (AccountForm, Vec<FormAction>)| {
                    let actions = show(&state.0, ui);
                    for action in &actions {
                        if let FormAction::Changed(form) = action {
                            state.0 = form.clone();
                        }
                    }
                    state.1.extend(actions);
                },
                (form, Vec::new()),
            )
    }

    fn filled() -> AccountForm {
        AccountForm {
            name: "work".into(),
            host: "imap.example.com".into(),
            port: "993".into(),
            username: "me@example.com".into(),
            password: "hunter2".into(),
            address: String::new(),
            error: None,
            saving: false,
        }
    }

    #[test]
    fn debug_never_shows_the_password() {
        let text = format!("{:?}", filled());
        assert!(!text.contains("hunter2"), "{text}");
        assert!(text.contains("work"));
    }

    #[test]
    fn account_takes_the_fields_and_defaults_the_port() {
        let mut form = filled();
        form.port = String::new();
        let (account, secret) = form.account().unwrap();
        assert_eq!(account.name, "work");
        assert_eq!(account.port, 993);
        assert_eq!(account.address, None);
        assert_eq!(secret.expose(), "hunter2");
        form.port = "abc".into();
        assert!(form.account().unwrap_err().contains("port"));
        form.port = "143".into();
        form.address = "me@example.org".into();
        let (account, _) = form.account().unwrap();
        assert_eq!(account.port, 143);
        assert_eq!(account.address.as_deref(), Some("me@example.org"));
    }

    #[test]
    fn save_is_disabled_until_the_required_fields_are_filled() {
        let mut harness = harness(AccountForm::default());
        harness.run();
        assert!(harness.get_by_label("Save account").accesskit_node().is_disabled());
        let mut harness = harness(filled());
        harness.run();
        assert!(!harness.get_by_label("Save account").accesskit_node().is_disabled());
        harness.get_by_label("Save account").click();
        harness.step();
        assert!(harness.state().1.contains(&FormAction::Save));
    }

    #[test]
    fn typing_in_a_field_reports_the_changed_form() {
        let mut harness = harness(AccountForm::default());
        harness.run();
        harness.get_by_label("Account name").click();
        harness.run();
        harness.event(egui::Event::Text("home".into()));
        harness.run();
        assert_eq!(harness.state().0.name, "home");
    }

    #[test]
    fn the_error_shows_and_cancel_reports() {
        let mut form = filled();
        form.error = Some("config.toml: duplicate account name 'work'".into());
        let mut harness = harness(form);
        harness.run();
        harness.get_by_label("config.toml: duplicate account name 'work'");
        harness.get_by_label("Cancel").click();
        harness.step();
        assert!(harness.state().1.contains(&FormAction::Cancel));
    }
}
```

Add `mod account;` to `src/gui/mod.rs`, alphabetically first.

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test gui::account::tests`
Expected: compile errors.

- [ ] **Step 3: Implement the form**

Above the tests in `src/gui/account.rs`:

```rust
use std::fmt;

use eframe::egui;

use crate::config::{AccountConfig, PasswordSource};
use crate::credentials::Secret;
use crate::message::clean;

use super::theme::{self, CONTROL, PAD};

/// Width of the text fields.
const FIELD: f32 = 280.0;

#[derive(Clone, Default, PartialEq)]
pub(crate) struct AccountForm {
    pub name: String,
    pub host: String,
    /// Typed text; empty means 993.
    pub port: String,
    pub username: String,
    pub password: String,
    /// Empty means the username.
    pub address: String,
    /// The last save's error, shown under the fields.
    pub error: Option<String>,
    /// A save is in flight, so the fields and buttons are disabled.
    pub saving: bool,
}

impl fmt::Debug for AccountForm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AccountForm")
            .field("name", &self.name)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .field("address", &self.address)
            .field("error", &self.error)
            .field("saving", &self.saving)
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum FormAction {
    Cancel,
    Changed(AccountForm),
    Save,
}

impl AccountForm {
    /// Name, host, username and password are required; the rest has defaults.
    pub fn complete(&self) -> bool {
        [&self.name, &self.host, &self.username, &self.password]
            .iter()
            .all(|field| !field.trim().is_empty())
    }

    /// The account the fields describe, with a keyring password, and the secret to store for it.
    pub fn account(&self) -> Result<(AccountConfig, Secret), String> {
        let port = match self.port.trim() {
            "" => 993,
            text => text
                .parse()
                .map_err(|_| format!("port must be a number, not '{}'", clean(text, false)))?,
        };
        let address = Some(self.address.trim().to_string()).filter(|a| !a.is_empty());
        let account = AccountConfig {
            port,
            address,
            ..AccountConfig::new(
                self.name.trim(),
                self.host.trim(),
                self.username.trim(),
                PasswordSource::Keyring { keyring: true },
            )
        };
        Ok((account, Secret::new(self.password.clone())))
    }
}

/// Draws the form; a typed change comes back as the whole edited form, since views do not hold state.
pub(crate) fn show(form: &AccountForm, ui: &mut egui::Ui) -> Vec<FormAction> {
    let mut actions = Vec::new();
    let mut edited = form.clone();
    ui.add_enabled_ui(!form.saving, |ui| {
        theme::grid("account-form", ui).num_columns(2).show(ui, |ui| {
            for (label, value, password, hint) in [
                ("Account name", &mut edited.name, false, "letters, digits, - and _"),
                ("IMAP host", &mut edited.host, false, "imap.example.com"),
                ("Port", &mut edited.port, false, "993"),
                ("Username", &mut edited.username, false, "the IMAP login"),
                ("Password", &mut edited.password, true, ""),
                ("Email address", &mut edited.address, false, "only when the username is not one"),
            ] {
                ui.label(label);
                let field = egui::TextEdit::singleline(value)
                    .password(password)
                    .hint_text(hint)
                    .desired_width(FIELD);
                let response = theme::text_field(ui, field);
                response.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, true, label)
                });
                ui.end_row();
            }
        });
    });
    if edited != *form {
        actions.push(FormAction::Changed(edited));
    }
    if let Some(error) = &form.error {
        ui.add_space(PAD);
        ui.colored_label(ui.visuals().error_fg_color, clean(error, false));
    }
    ui.add_space(PAD);
    ui.horizontal(|ui| {
        let save = egui::Button::new("Save account").min_size(egui::vec2(0.0, CONTROL));
        if ui
            .add_enabled(form.complete() && !form.saving, save)
            .clicked()
        {
            actions.push(FormAction::Save);
        }
        if ui.add_enabled(!form.saving, egui::Button::new("Cancel")).clicked() {
            actions.push(FormAction::Cancel);
        }
        if form.saving {
            ui.spinner();
            ui.label("Saving and starting the first sync…");
        }
    });
    actions
}
```

`theme::text_field` exists at `src/gui/theme.rs:331`; read its signature and adapt if it takes the field differently. If `widget_info` on a `TextEdit` response does not make `get_by_label("Account name")` find the field in the test, use `ui.add(field).labelled_by(label_id)` with `let label_id = ui.label(label).id;` instead.

- [ ] **Step 4: Run the tests**

Run: `cargo test gui::account::tests`
Expected: PASS. `tests/architecture.rs` also passes: the module takes `&AccountForm`, not `&mut App`, and names no daemon or rules edit.

- [ ] **Step 5: Commit**

```bash
git add src/gui/account.rs src/gui/mod.rs
git commit -m "feat(gui): the add-account form widget"
```

---

### Task 13: The first-run window saves an account and opens the mail window

**Files:**
- Modify: `src/gui/mod.rs:42-56` (`run`), `src/gui/mod.rs:116-153` (`explain`, `Explain`), `src/gui/startup.rs` (`Problem::of` text, `show`, tests), `docs/src/gui.md:7`

**Interfaces:**
- Consumes: `account::{AccountForm, FormAction, show}` (Task 12), `postvak::account::add` (Task 1).
- Produces: `Explain::new(problem: Problem, paths: Option<Paths>, added: Arc<AtomicBool>) -> Explain`; `explain(error, paths: Option<&Paths>) -> anyhow::Result<()>` returning `Ok(())` only when an account was added and the mail window should be tried again, else the error.

- [ ] **Step 1: Write the failing tests in `src/gui/startup.rs`**

Change `harness` to build `Explain::new(problem, None)` and add a second helper and tests:

```rust
    fn first_run(paths: Option<Paths>) -> Harness<'static, Explain> {
        let problem = Problem::of(&anyhow::Error::new(NoAccounts), None);
        Harness::builder()
            .with_size(egui::vec2(520.0, 420.0))
            .build_ui_state(
                |ui, explain: &mut Explain| explain.show(ui),
                Explain::new(problem, paths, Arc::default()),
            )
    }

    fn added(harness: &Harness<'_, Explain>) -> bool {
        harness.state().added.load(Ordering::Relaxed)
    }

    #[test]
    fn without_paths_the_missing_account_only_points_at_the_terminal() {
        let mut harness = first_run(None);
        harness.run();
        harness.get_by_label_contains("`postvak account add`");
        assert!(harness.query_by_label("Save account").is_none());
    }

    #[test]
    fn the_first_run_form_saves_the_account_and_closes_to_open_the_window() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        let mut harness = first_run(Some(paths.clone()));
        harness.run();
        harness.get_by_label("Postvak has no mail account set up yet.");
        harness.state_mut().form.name = "work".into();
        harness.state_mut().form.host = "imap.example.com".into();
        harness.state_mut().form.username = "me@example.com".into();
        harness.state_mut().form.password = "hunter2".into();
        harness.run();
        harness.get_by_label("Save account").click();
        harness.run();
        assert!(added(&harness));
        let config = crate::config::Config::load(&paths.config_file()).unwrap();
        assert_eq!(config.accounts[0].name, "work");
        assert!(
            harness.output().viewport_output[&egui::ViewportId::ROOT]
                .commands
                .contains(&egui::ViewportCommand::Close)
        );
    }

    #[test]
    fn a_failed_first_run_save_shows_the_error_and_stays_open() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        let mut harness = first_run(Some(paths));
        harness.state_mut().form.name = "my work".into();
        harness.state_mut().form.host = "h".into();
        harness.state_mut().form.username = "me@example.com".into();
        harness.state_mut().form.password = "x".into();
        harness.run();
        harness.get_by_label("Save account").click();
        harness.run();
        assert!(!added(&harness));
        harness.get_by_label_contains("may only contain letters, digits");
    }
```

Add `use std::sync::Arc;`, `use std::sync::atomic::Ordering;` and `use crate::paths::Paths;` to the test imports. `Explain`'s fields `form` and `added` must be `pub(crate)` for these tests.

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test gui::startup::tests`
Expected: compile errors on `Explain::new`, `form`, `added`.

- [ ] **Step 3: Rework `Explain` and `run`**

In `src/gui/mod.rs`:

```rust
/// The window in place of the mail window when that cannot start. Without an account it holds the add form, and
/// `added`, shared with `explain`, tells `run` to try the mail window again once this window closed.
pub(crate) struct Explain {
    problem: startup::Problem,
    paths: Option<Paths>,
    pub(crate) form: account::AccountForm,
    pub(crate) added: Arc<AtomicBool>,
}

impl Explain {
    pub(crate) fn new(
        problem: startup::Problem,
        paths: Option<Paths>,
        added: Arc<AtomicBool>,
    ) -> Explain {
        Explain {
            problem,
            paths,
            form: account::AccountForm::default(),
            added,
        }
    }

    fn show(&mut self, ui: &mut egui::Ui) {
        let form = self
            .paths
            .as_ref()
            .filter(|_| self.problem.no_accounts)
            .map(|_| &self.form);
        let (close, actions) = startup::show(&self.problem, form, ui);
        for action in actions {
            match action {
                account::FormAction::Changed(form) => self.form = form,
                account::FormAction::Cancel => ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close),
                account::FormAction::Save => self.save(ui.ctx()),
            }
        }
        if close {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    /// Saves the first account; the mail window then starts the daemon, which syncs it.
    fn save(&mut self, ctx: &egui::Context) {
        let Some(paths) = &self.paths else { return };
        let saved = self.form.account().map_err(anyhow::Error::msg).and_then(|(account, secret)| {
            let mut config = Config::load(&paths.config_file())?;
            crate::account::add(&mut config, paths, account, Some(secret))?;
            Ok(())
        });
        match saved {
            Ok(()) => {
                self.added.store(true, Ordering::Relaxed);
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Err(e) => self.form.error = Some(format!("{e:#}")),
        }
    }
}
```

with `use std::sync::atomic::{AtomicBool, Ordering};` added to the imports. `run` becomes a loop; `explain` returns `Ok(())` only when the window added an account, so the loop tries the mail window again, and otherwise hands the original error back:

```rust
pub fn run(paths: Result<Paths>) -> Result<()> {
    // Events can arrive before the window has a context; they wait in the channel for its first frame.
    let window: Arc<OnceLock<egui::Context>> = Arc::default();
    let paths = match paths {
        Ok(paths) => paths,
        Err(e) => return explain(e, None),
    };
    loop {
        match start(&paths, window.clone()) {
            Ok((config, connection)) => return open(config, paths, connection, &window),
            // Ok only when the first-run window added an account: try the mail window again.
            Err(e) => explain(e, Some(&paths))?,
        }
    }
}
```

```rust
/// Shows why the mail window could not start until the user closes it, then hands the error back for stderr and the
/// exit code. With no account yet the window holds the add form, and `Ok` means one was added. Without a display
/// there is no window, and stderr is all there is.
fn explain(error: anyhow::Error, paths: Option<&Paths>) -> Result<()> {
    let log = paths.map(Paths::daemon_log);
    let problem = startup::Problem::of(&error, log.as_deref());
    let added = Arc::new(AtomicBool::new(false));
    let shown = eframe::run_native(
        "Postvak",
        explain_options(),
        Box::new({
            let added = added.clone();
            let paths = paths.cloned();
            move |cc| {
                theme::install(&cc.egui_ctx);
                Ok(Box::new(Explain::new(problem, paths, added)))
            }
        }),
    );
    if let Err(e) = shown {
        log::debug!("could not show the startup error in a window: {e}");
    }
    if added.load(Ordering::Relaxed) {
        return Ok(());
    }
    Err(error)
}
```

`Paths::daemon_log` returns a `PathBuf`, so `log.as_deref()` is an `Option<&Path>`. `explain_options` grows the window to `native_options([520.0, 420.0])` so the form fits.

In `src/gui/startup.rs`:

- `Problem` gains `pub no_accounts: bool`, true only in the `NoAccounts` branch of `of`, and that branch's `fix` becomes `"Fill in the account below, or run `postvak account add` in a terminal."`. Update the two tests that compare `Problem` fields.
- `show` takes the optional form and returns the form's actions too:

```rust
/// Draws the problem, with the add-account form when `form` is given; true when the user asked to close the window.
pub(crate) fn show(
    problem: &Problem,
    form: Option<&AccountForm>,
    ui: &mut egui::Ui,
) -> (bool, Vec<FormAction>) {
    let mut close = false;
    let mut actions = Vec::new();
    central_panel(ui, false).show(ui, |ui| {
        ui.heading(&problem.summary);
        ui.add_space(8.0);
        if let Some(form) = form {
            actions = account::show(form, ui);
        } else {
            ui.label(&problem.fix);
        }
        if let Some((label, url)) = problem.link {
            ui.hyperlink_to(label, url);
        }
        if let Some(detail) = &problem.detail {
            ui.add_space(8.0);
            egui::ScrollArea::vertical()
                .max_height(ui.available_height() - 40.0)
                .show(ui, |ui| {
                    ui.colored_label(theme::palette(ui).error, detail);
                });
        }
        if form.is_none() {
            ui.add_space(12.0);
            close = ui.button("Close").clicked();
        }
    });
    (close, actions)
}
```

with `use super::account::{self, AccountForm, FormAction};`. The form's own Cancel closes the window, so the Close button is only drawn without a form. `the_window_states_the_missing_account` builds with `None` paths through `harness` and keeps its `Close` lookup.

- [ ] **Step 4: Run the tests**

Run: `cargo test gui::startup gui::account`
Expected: PASS. `tests/architecture.rs` exempts `mod.rs`, which is where the save happens.

- [ ] **Step 5: Document and commit**

`docs/src/gui.md` line 7: replace "for instance because no account is set up yet, a small window says why and what to do" with "a small window says why and what to do; with no account set up yet, that window holds the add-account form, and saving it opens the mail window".

```bash
git add src/gui/mod.rs src/gui/startup.rs docs/src/gui.md
git commit -m "feat(gui): add the first account from the first-run window"
```

---

### Task 14: Add an account from the toolbar

**Files:**
- Modify: `src/gui/app.rs` (`UiAction`, `App` fields, `new`, `show`, `apply`, `receive`), `src/gui/toolbar.rs`, `src/gui/icons.rs`, `src/gui/test_support.rs` (nothing unless a helper is needed), `docs/src/gui.md` (Toolbar section)

**Interfaces:**
- Consumes: `AccountForm`, `FormAction`, `account::show` (Task 12), `postvak::account::add`.
- Produces: `UiAction::{OpenAccountForm, AccountForm(FormAction)}`; `App::account_form: Option<AccountForm>`; `App::account_saving: Option<Receiver<Result<AccountConfig, String>>>`; `App::add_account_state(name: String)`.

- [ ] **Step 1: Write the failing tests in `src/gui/app.rs` tests**

```rust
    fn fill_form(harness: &mut egui_kittest::Harness<'_, App>, name: &str) {
        let form = harness.state_mut().account_form.as_mut().unwrap();
        form.name = name.into();
        form.host = "imap.example.com".into();
        form.username = "me@example.com".into();
        form.password = "hunter2".into();
    }

    /// Runs frames until the save thread answered, at most `secs` seconds.
    fn finish_save(harness: &mut egui_kittest::Harness<'_, App>, secs: u64) {
        let deadline = std::time::Instant::now() + Duration::from_secs(secs);
        while harness.state().account_saving.is_some() {
            assert!(std::time::Instant::now() < deadline, "the save did not finish");
            std::thread::sleep(Duration::from_millis(20));
            harness.run();
        }
    }

    #[test]
    fn the_toolbar_opens_the_form_and_saving_adds_the_account() {
        let fx = Fixture::new(&["work"]);
        let (mut harness, _wires) = fx.harness();
        harness.get_by_label("Add account").click();
        harness.run();
        assert!(harness.state().account_form.is_some());
        fill_form(&mut harness, "home");
        harness.run();
        harness.get_by_label("Save account").click();
        harness.run();
        finish_save(&mut harness, 5);
        assert!(harness.state().account_form.is_none());
        let names: Vec<&str> = harness.state().accounts.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["work", "home"]);
        assert_eq!(
            harness.state().view,
            View::Folder { account: 1, folder: "INBOX".into() }
        );
        let config = Config::load(&fx.paths.config_file()).unwrap();
        assert!(config.accounts.iter().any(|a| a.name == "home"));
        assert!(!harness.state().config_changed, "the app's own save is no outside edit");
    }

    #[test]
    fn saving_an_existing_name_replaces_the_account() {
        let fx = Fixture::new(&["work"]);
        let (mut harness, _wires) = fx.harness();
        harness.get_by_label("Add account").click();
        harness.run();
        fill_form(&mut harness, "work");
        harness.run();
        harness.get_by_label("Save account").click();
        harness.run();
        finish_save(&mut harness, 5);
        let names: Vec<&str> = harness.state().accounts.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["work"]);
        let config = Config::load(&fx.paths.config_file()).unwrap();
        assert_eq!(config.accounts.len(), 1);
        assert_eq!(config.accounts[0].host, "imap.example.com");
    }

    #[test]
    fn a_failed_save_keeps_the_form_open_with_the_error() {
        let fx = Fixture::new(&["work"]);
        let (mut harness, _wires) = fx.harness();
        harness.get_by_label("Add account").click();
        harness.run();
        fill_form(&mut harness, "my home");
        harness.run();
        harness.get_by_label("Save account").click();
        harness.run();
        finish_save(&mut harness, 5);
        let form = harness.state().account_form.as_ref().unwrap();
        assert!(form.error.as_deref().unwrap_or_default().contains("may only contain"), "{form:?}");
        assert!(!form.saving);
    }

    #[test]
    fn escape_closes_the_form() {
        let fx = Fixture::new(&["work"]);
        let (mut harness, _wires) = fx.harness();
        harness.get_by_label("Add account").click();
        harness.run();
        harness.key_press(egui::Key::Escape);
        harness.run();
        assert!(harness.state().account_form.is_none());
    }
```

Add `use crate::config::Config;` to the test imports if missing.

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test gui::app::tests::the_toolbar_opens`
Expected: FAIL, no "Add account" button.

- [ ] **Step 3: Implement**

`src/gui/icons.rs`: add `pub(crate) const ADD_ACCOUNT: &str = ph::USER_PLUS;` (alphabetical). If `egui-phosphor` 0.14 names it differently, pick `ph::PLUS_CIRCLE`.

`src/gui/toolbar.rs`, after the action buttons' `ui.scope` and before the search field:

```rust
        if icon_button(ui, true, icons::ADD_ACCOUNT, "Add account", "Add a mail account").clicked() {
            actions.push(UiAction::OpenAccountForm);
        }
```

`src/gui/app.rs`:

- `UiAction` gains, alphabetically: `AccountForm(FormAction)` and `OpenAccountForm`.
- `App` gains `pub(crate) account_form: Option<AccountForm>` and `pub(crate) account_saving: Option<Receiver<Result<AccountConfig, String>>>`, both `None` in `new`.
- Imports: `use super::account::{self, AccountForm, FormAction};` and `use crate::config::AccountConfig;`.
- In `show`, where the move picker or help overlay is drawn (read `show_panes` and the help window code near `help_open`), draw the form as a modal window:

```rust
        // Titled differently from the toolbar button, so a label query finds one of them.
        if let Some(form) = &self.account_form {
            egui::Window::new("New account")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(&ctx, |ui| {
                    for action in account::show(form, ui) {
                        actions.push(UiAction::AccountForm(action));
                    }
                });
        }
```

- In `apply`:

```rust
            UiAction::AccountForm(FormAction::Cancel) => self.account_form = None,
            UiAction::AccountForm(FormAction::Changed(form)) => self.account_form = Some(form),
            UiAction::AccountForm(FormAction::Save) => self.save_account(),
            UiAction::OpenAccountForm => {
                if self.account_form.is_none() {
                    self.account_form = Some(AccountForm::default());
                }
            }
```

- In the `Escape` arm, first: `if self.account_form.is_some() { self.account_form = None; } else if ...`.
- The save, on a thread because the OS keyring may prompt:

```rust
    /// Saves on a thread, since the OS keyring may prompt; `receive_saved` picks up the result.
    fn save_account(&mut self) {
        let Some(form) = self.account_form.as_mut().filter(|form| !form.saving) else {
            return;
        };
        let (account, secret) = match form.account() {
            Ok(parts) => parts,
            Err(message) => {
                form.error = Some(message);
                return;
            }
        };
        form.saving = true;
        form.error = None;
        let paths = self.paths.clone();
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let saved = Config::load(&paths.config_file())
                .map_err(|e| e.to_string())
                .and_then(|mut config| {
                    crate::account::add(&mut config, &paths, account.clone(), Some(secret))
                        .map_err(|e| e.to_string())
                })
                .map(|()| account);
            let _ = sender.send(saved);
        });
        self.account_saving = Some(receiver);
    }

    /// The save thread's answer: a new account joins the window, a replaced one keeps its place.
    fn receive_saved(&mut self) {
        let Some(receiver) = &self.account_saving else { return };
        let saved = match receiver.try_recv() {
            Ok(saved) => saved,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err("the save did not finish".into()),
        };
        self.account_saving = None;
        match saved {
            Ok(account) => {
                self.account_form = None;
                self.config_mtime = mtime(&self.paths.config_file());
                let index = match self.accounts.iter().position(|a| a.name == account.name) {
                    Some(index) => index,
                    None => {
                        self.add_account_state(account.name.clone());
                        self.accounts.len() - 1
                    }
                };
                self.push_history(format!("{}: account saved; syncing", account.name));
                self.select_view(View::Folder {
                    account: index,
                    folder: "INBOX".into(),
                });
            }
            Err(message) => {
                if let Some(form) = &mut self.account_form {
                    form.saving = false;
                    form.error = Some(message);
                }
            }
        }
    }

    /// An account the daemon will start from the saved config; its events arrive by name.
    pub(crate) fn add_account_state(&mut self, name: String) {
        let account = account_state(&self.paths, name, None);
        self.accounts.push(account);
    }
```

and a free function that `App::new` uses too, in place of the closure body that builds `Account` today:

```rust
/// An account's window state over its store, with the folders it holds now.
fn account_state(paths: &Paths, name: String, activity: Option<Activity>) -> Account {
    // `open_account` creates the account's directories, so a just-added account has a store too.
    let store = Store::open_account(paths, &name).map_err(|e| format!("could not open the store: {e}"));
    let mut account = Account {
        activity,
        error: None,
        folders: Vec::new(),
        name,
        pending: HashMap::new(),
        queued: 0,
        store,
    };
    account.reload_folders();
    account
}
```

so in `new` the map becomes `.map(|AccountStatus { activity, name }| account_state(&paths, name, activity))`.

- Call `self.receive_saved();` in `show` right after `self.receive(now);`. Make sure the frame repaints while a save is in flight: `if self.account_saving.is_some() { ctx.request_repaint_after(Duration::from_millis(50)); }`.
- In `poll_files`, skip the config.toml check while `self.account_saving.is_some()`: the app's own write must not raise the "config.toml changed" banner before `receive_saved` records the new mtime.

- [ ] **Step 4: Run the tests**

Run: `cargo test gui::`
Expected: PASS, including `the_actions_are_enabled_on_a_row_and_disabled_outside_a_folder` (the new button is always enabled and is not in `ACTIONS`).

- [ ] **Step 5: Document and commit**

`docs/src/gui.md`, in "Toolbar and status bar": "The last toolbar button adds a mail account: name, host, port, username, password and address. The password goes to the OS keyring. The account's first sync starts right away, and a wrong login shows on its status line."

```bash
git add src/gui/app.rs src/gui/toolbar.rs src/gui/icons.rs docs/src/gui.md
git commit -m "feat(gui): add an account from the toolbar"
```

---

### Task 15: Purge from the Trash view

**Files:**
- Modify: `src/gui/rules.rs` (`show_trash`, tests), `src/gui/app.rs` (`UiAction::PurgeTrash`, `apply`), `docs/src/gui.md` (Rules, Activity and Backups)

**Interfaces:**
- Consumes: `Trash::purge`, `Config::load`.
- Produces: `UiAction::PurgeTrash`.

- [ ] **Step 1: Write the failing test in `src/gui/rules.rs` tests**

```rust
    #[test]
    fn purge_removes_old_backups_and_reports_the_count() {
        let fx = Fixture::new(&["work"]);
        let trash = Trash::new(fx.paths.trash_dir("work"));
        let now = crate::time::now();
        let old = trash.save("INBOX", 1, b"From: a@example.com\r\nSubject: old\r\n\r\n", now - 31 * 86_400).unwrap();
        let fresh = trash.save("INBOX", 2, b"From: b@example.com\r\nSubject: fresh\r\n\r\n", now - 86_400).unwrap();
        let (mut harness, _wires) = fx.harness();
        harness.state_mut().select_view(View::Trash);
        harness.run();
        harness.get_by_label("Purge").click();
        harness.run();
        assert!(!old.exists());
        assert!(fresh.exists());
        assert!(harness.query_by_label("old").is_none());
        assert!(harness.query_by_label("fresh").is_some());
        assert!(
            harness
                .state()
                .history
                .iter()
                .any(|line| line.text == "removed 1 backup older than the retention period")
        );
    }
```

- [ ] **Step 2: Run it to see it fail**

Run: `cargo test gui::rules::tests::purge_removes`
Expected: FAIL, no "Purge" button.

- [ ] **Step 3: Implement**

In `show_trash`, before the early return on an empty trash, so the button is there in both cases:

```rust
    if ui
        .button("Purge")
        .on_hover_text("Delete the backups older than each account's retention period, for good")
        .clicked()
    {
        actions.push(UiAction::PurgeTrash);
    }
```

In `app.rs`, `UiAction::PurgeTrash` (alphabetical) and in `apply`:

```rust
            UiAction::PurgeTrash => self.purge_trash(),
```

```rust
    /// Deletes each account's backups past its retention; the retention lives in config.toml, read here.
    fn purge_trash(&mut self) {
        let config = match Config::load(&self.paths.config_file()) {
            Ok(config) => config,
            Err(e) => return self.note_error(None, format!("could not read config.toml: {e}")),
        };
        let now = crate::time::now();
        let mut removed = 0;
        for account in &config.accounts {
            let trash = crate::trash::Trash::new(self.paths.trash_dir(&account.name));
            match trash.purge(account.trash_retention_days as i64 * 86_400, now) {
                Ok(count) => removed += count,
                Err(e) => self.note_error(None, format!("{}: could not purge: {e}", account.name)),
            }
        }
        let noun = if removed == 1 { "backup" } else { "backups" };
        self.push_history(format!("removed {removed} {noun} older than the retention period"));
        self.view_dirty = true;
    }
```

`view_dirty` makes `reload_view` list the trash again; confirm by reading `reload_view` for `View::Trash`.

- [ ] **Step 4: Run, document, commit**

Run: `cargo test gui::rules`
Expected: PASS.

`docs/src/gui.md`, in "Rules, Activity and Backups", add to the Backups sentence: "Purge deletes the backups older than each account's retention period, as `postvak trash purge` does."

```bash
git add src/gui/rules.rs src/gui/app.rs docs/src/gui.md
git commit -m "feat(gui): purge old backups from the Trash view"
```

---

### Task 16: Apply a rule to existing mail

**Files:**
- Modify: `src/gui/rules.rs` (`show_rules`, tests), `src/gui/app.rs` (`UiAction::ApplyRule`, `apply`, `handle`'s `RuleApplied` arm), `docs/src/gui.md`

**Interfaces:**
- Consumes: `Command::ApplyRule { name }`, `Event::RuleApplied { evaluated, actions, errors, .. }`, `App::send(index, command)`.
- Produces: `UiAction::ApplyRule(String)`.

- [ ] **Step 1: Write the failing test in `src/gui/rules.rs` tests**

```rust
    #[test]
    fn apply_sends_the_rule_to_its_accounts_and_reports_the_outcome() {
        let fx = Fixture::new(&["home", "work"]);
        std::fs::write(
            fx.paths.rules_file(),
            format!("{RULES}\n[[rules]]\nname = \"only-work\"\naccount = \"work\"\nmatch.subject = {{ contains = \"x\" }}\nactions = [\"flag\"]\n"),
        )
        .unwrap();
        let (mut harness, wires) = fx.harness();
        harness.state_mut().select_view(View::Rules);
        harness.run();
        let buttons = harness.query_all_by_label("Apply to existing mail").count();
        assert_eq!(buttons, 2, "one per enabled rule; the disabled proposal has none");
        harness.query_all_by_label("Apply to existing mail").nth(1).unwrap().click();
        harness.run();
        assert_eq!(
            wires.sent(),
            [("work".to_string(), Command::ApplyRule { name: "only-work".into() })]
        );
        wires
            .events
            .send(Event::RuleApplied {
                account: "work".into(),
                request: 0,
                evaluated: 12,
                actions: 3,
                errors: vec!["INBOX/4: gone".into()],
            })
            .unwrap();
        harness.run();
        let history: Vec<&str> = harness.state().history.iter().map(|l| l.text.as_str()).collect();
        assert!(history.contains(&"work: rule applied: 3 actions on 12 messages"), "{history:?}");
        assert!(history.contains(&"work: INBOX/4: gone"), "{history:?}");
    }
```

`RULES` holds `newsletters` (enabled, every account) and `codes` (disabled proposal). Clicking the first Apply would send to both accounts; the test clicks the second, scoped to `work`. `query_all_by_label` is the kittest query `src/gui/list.rs` already uses.

- [ ] **Step 2: Run it to see it fail**

Run: `cargo test gui::rules::tests::apply_sends`
Expected: FAIL, no "Apply to existing mail" button.

- [ ] **Step 3: Implement**

In `show_rules`, the rules grid becomes four columns: after the `proposed by` label and before `ui.end_row()`:

```rust
                        if rule.enabled
                            && ui
                                .button("Apply to existing mail")
                                .on_hover_text("Run this rule once over the mail each account already holds")
                                .clicked()
                        {
                            actions.push(UiAction::ApplyRule(rule.name.clone()));
                        } else if !rule.enabled {
                            ui.label("");
                        }
```

and `.num_columns(4)`.

In `app.rs`, `UiAction::ApplyRule(String)` and in `apply`:

```rust
            UiAction::ApplyRule(name) => {
                let scope = self
                    .rules
                    .rules
                    .iter()
                    .find(|rule| rule.name == name)
                    .and_then(|rule| rule.account.clone());
                for index in 0..self.accounts.len() {
                    if scope.as_deref().is_none_or(|account| account == self.accounts[index].name) {
                        self.send(index, Command::ApplyRule { name: name.clone() });
                    }
                }
            }
```

In `handle`, split `Event::RuleApplied` out of the `BodiesFetched | RuleApplied | Synced` arm:

```rust
            Event::RuleApplied {
                evaluated,
                actions,
                errors,
                ..
            } => {
                let name = self.accounts[index].name.clone();
                self.push_history(format!(
                    "{name}: rule applied: {actions} actions on {evaluated} messages"
                ));
                for message in errors {
                    self.note_error(Some(index), message);
                }
                self.refresh(index);
            }
```

`note_error` prefixes the account name, so the history line reads `work: INBOX/4: gone`.

- [ ] **Step 4: Run, document, commit**

Run: `cargo test gui::`
Expected: PASS, including the existing rules view tests (the grid gained a column).

`docs/src/gui.md`, in "Rules, Activity and Backups": "Each enabled rule has an Apply to existing mail button, which runs it once over the mail already in the accounts it applies to, as `postvak rules apply-existing` does; the outcome shows in the history."

```bash
git add src/gui/rules.rs src/gui/app.rs docs/src/gui.md
git commit -m "feat(gui): apply a rule to existing mail from the Rules view"
```

---

### Task 17: Snapshots, full check and the third PR

**Files:**
- Modify: `src/gui/snapshots.rs`, `tests/snapshots/*.png`

- [ ] **Step 1: Add a snapshot of the form**

In `src/gui/snapshots.rs`, after `trash`:

```rust
#[test]
fn add_account_form() {
    let (_fx, mut harness, _wires) = inbox("light");
    harness.get_by_label("Add account").click();
    harness.run();
    snapshot(&mut harness, "gui_add_account");
}
```

- [ ] **Step 2: Regenerate the snapshots on Linux**

The memory note `gui-snapshots-on-mac.md` describes the Docker setup. In that container:

```bash
TZ=UTC UPDATE_SNAPSHOTS=1 cargo test gui::snapshots
```

Expected: `gui_add_account.png` is new; `gui_rules.png` and `gui_trash.png` change for the new buttons; `gui_inbox_*` change for the toolbar button. Look at each changed PNG: the form is centred, the buttons sit on the grid, nothing overlaps.

- [ ] **Step 3: Full check**

Run: `cargo fmt --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test && cargo machete && cargo audit`
Expected: all pass.

- [ ] **Step 4: Commit and open the PR**

```bash
git add src/gui/snapshots.rs tests/snapshots/
git commit -m "test(gui): snapshot the add-account form and the new buttons"
git push -u origin HEAD
```

PR title: `feat(gui): account form, trash purge and apply rule to existing mail`. The body links the spec, says the snapshots were regenerated on Linux with lavapipe, and names the platforms the other tests ran on.
