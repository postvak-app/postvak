# Surface parity — design

Status: draft, 2026-10-13

## Goal

Make the Surfaces rule in `AGENTS.md` true: every feature ships on the CLI, MCP and GUI unless its spec names the exception, and logic lives in library modules with IMAP work going through the daemon. An audit on 2026-10-13 found these gaps:

| Feature | CLI | MCP | GUI |
|---|---|---|---|
| account add | yes | no | no |
| account list | yes | no | partial, the folder pane |
| status | yes | no | partial, the status row |
| attachment save | yes | no | yes |
| rules apply-existing | yes | no | no |
| trash purge | yes | no | no |

One rule violation: `postvak account add` opens its own IMAP connection to test the login, outside the daemon.

## Decisions

- **Account add saves first and syncs through the daemon.** No login probe before saving. The daemon watches `config.toml`, starts the new account's thread; the CLI and MCP wait for it to list the account and request one sync, while the GUI shows the sync the daemon starts on its own. A wrong password surfaces as that sync's error; running add again with the same name replaces the account. The CLI's "Testing login..." line goes.
- **Over MCP, account add takes a password command only.** A plain password would pass through the agent host's context and logs. Keyring passwords stay CLI and GUI.
- **Account add gets its own scope, `accounts:write`.** Adding an account is neither mail nor rules, and a host should be able to grant everything else without it. It is not in the default scope set.
- **`attachment_save` is `read:bodies`.** The agent already sees the content; writing it to a directory it names adds little.
- **`trash_purge` and `rules_apply_existing` are `mail:modify`.** Purge deletes backups for good; apply-existing acts on stored mail.
- **GUI password entry is keyring only.** A GUI user has no terminal for a password command.
- **GUI rules editing stays as it is.** Propose, test, check and schema are agent and CLI features; the GUI opens the file. This is the one named exception, already in the GUI spec.

## Library

New module `src/account.rs`, the only code that adds an account:

```rust
pub fn add(config: &mut Config, paths: &Paths, account: AccountConfig, secret: Option<Secret>) -> Result<(), AddError>
```

- Validates the account through `Config::validate` after replacing any account of the same name.
- With `PasswordSource::Keyring` a `secret` is required and is stored through `credentials::store` before config is saved, so the daemon never reads a half-added account. With `PasswordSource::Command` no secret is taken; the command is not run here.
- Saves config through `Config::save`.
- `AddError` wraps `ConfigError` and `CredentialError`, and names a missing secret.

The CLI wizard keeps only its prompts and calls `add`, then `sync_account` for the new account, which already prints the daemon's errors.

Trash purge (`Trash::purge`), apply-existing (`sync::Command::ApplyRule`) and its dry run (`actions::planned`) exist already in the library. Nothing moves.

## Daemon

No wire changes. `Status`, `SyncNow` and `ApplyRule` are already there.

## Output and help

`output` gains `status(&Status) -> serde_json::Result<Value>`, `planned_row(account, &Planned) -> Value` for `actions::planned` rows and `rule_applied(account, evaluated, actions, errors) -> Value`. `help` gains one text per new tool, used by both the CLI and the MCP descriptions: `ACCOUNT_ADD`, `ACCOUNT_LIST`, `STATUS`, `ATTACHMENT_SAVE`, `TRASH_PURGE`, `RULES_APPLY_EXISTING`.

## MCP

Six tools. Arguments mirror the CLI flags.

| Tool | Scope | Arguments | Result |
|---|---|---|---|
| `account_add` | `accounts:write` | `name`, `host`, `port` (default 993), `username`, `password_command`, `address`, `ca_file` | the account row, then the sync result for that account |
| `accounts` | `read` | none | `output::account` rows |
| `status` | `read` | none | `output::status` |
| `attachment_save` | `read:bodies` | `account`, `folder`, `uid`, `n`, `dir` | the written path |
| `trash_purge` | `mail:modify` | `account` (optional, all when absent) | per account the removed count |
| `rules_apply_existing` | `mail:modify` | `name`, `account` (optional), `dry_run` | dry run: planned rows; otherwise per account evaluated, actions and errors |

`Scope::AccountsWrite` is added to `Scope::ALL`, `parse_scopes`, the two `--scopes` help strings and the scope table in `docs/src/mcp.md`. `backend.rs` stays the only MCP code touching store, rules or the daemon. `account_add` calls `account::add`, waits for the daemon to list the account, then requests `SyncNow`, the same as the CLI.

## GUI

Three additions. Each is a `UiAction` carried out in `app.rs`; the view modules only draw.

- **Account form.** Fields: name, host, port, username, password, address. The first-run screen in `startup.rs` shows it instead of pointing at the terminal, and a toolbar button opens the same form when accounts exist. Save runs `account::add` with a keyring secret on a worker thread, since the OS keyring may prompt. The daemon starts the new account from the saved config and syncs it itself, so the window only adds the account to its list and shows the sync on its status line. Esc closes the form.
- **Purge.** A "Purge" button in the Trash view with the retention days in its hover text. It runs `Trash::purge` for every account and shows the total removed in the status row.
- **Apply to existing mail.** A button per enabled rule in the Rules view. It sends `ApplyRule` for each account the rule applies to and shows evaluated and action counts in the status row. No dry run in the GUI; the row text is the result.

Layout follows `theme.rs`: rows `ROW`, buttons `CONTROL`, `INSET` and `PAD`.

## Errors

- A failed `account::add` leaves config untouched, since the secret is stored first and config is written atomically.
- A sync error after add is reported, not retried; the account stays saved.
- `attachment_save` into a directory that does not exist is an error naming the directory, as in the CLI.
- MCP write tools without their scope are refused with the scope name, as today.

## Testing

- `account::add` unit tests: replaces an account of the same name, refuses a keyring account without a secret, stores the secret before config is written, leaves config untouched on a validation error.
- CLI black-box test in `tests/cli.rs`: `account add` with a password command, then `account list --json` shows it.
- MCP: `tools_list_per_scope_set_is_exact_and_alphabetical` gains the six tools and the new scope; one test per tool against `Fixture`; `account_add` with a plain `password` argument is rejected by the schema.
- GUI with `egui_kittest`: the first-run form saves and the window leaves the problem screen; the toolbar opens the form; purge and apply buttons produce the status-row text. Snapshots for the form, light and dark.
- `tests/architecture.rs` unchanged; it must keep passing.
- Docs: `mcp.md` scope table and tools, `cli.md` blessed, `accounts.md` gains the GUI form and the MCP tool.

## Sequencing

Three PRs, each leaving the rule truer:

1. `feat: account add through the library, and MCP accounts, status and attachment_save` — the library module, the CLI wizard on top of it, the three read-side tools.
2. `feat(mcp): account_add, trash_purge and rules_apply_existing` — the write tools and the new scope.
3. `feat(gui): account form, trash purge and apply rule to existing mail`.

## Out of scope

- Editing or removing an account from the GUI or MCP; the CLI has neither either.
- A GUI rules editor; `docs/superpowers/plans/2026-10-08-rules-editor.md` covers it.
- Testing the login before saving.
