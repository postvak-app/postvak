# Testing an account's login before saving it — design

Status: implemented, 2026-10-19. Supersedes "Account add saves first and syncs through the daemon" in `2026-10-13-surface-parity-design.md`.

## Goal

Adding an account with a wrong password, host or port should save nothing. Until now the CLI, MCP and GUI saved first and let the first sync show the error, which left a broken account in `config.toml` and, on a replace, overwrote a working keyring password with the wrong one.

## Surfaces

All three, through one library call, `account::test_login`. The help text for `account add` and the MCP tool description change with it.

## Decisions

- **The daemon logs in, the front end does not.** The old CLI probe was dropped because it opened its own IMAP connection outside the daemon. The test is a daemon request instead, so IMAP still goes through the daemon only.
- **A request of its own, not a `Command`.** `Command`s go to a running account's thread; an account under test has none. `TestLogin { id, account, password }` sits next to `status` and `shutdown`. The daemon logs in on a thread of its own (a login can take the 30 s connect timeout), logs out, and replies `done` or an error naming the host. It starts no thread, opens no store and writes nothing.
- **The password travels over the socket.** The typed password is not in the keyring yet, so the daemon cannot fetch it. The socket is the user's own; `wire::Password` keeps it out of `Debug` and wipes it on drop. It is never logged.
- **A password command runs in the front end.** `test_login` without a secret resolves the account's password source where it is called, so the daemon never runs a command for an account that is not saved.
- **Validation first.** An account `config.toml` would refuse is refused before any login, with the same message as before.
- **No first-sync wait in the GUI.** Once the login passed, the form saves and closes; the account's first sync shows on its line, as before. A first sync of a large mailbox can take minutes, too long to keep a form open. The CLI and MCP still request one sync after saving and report it.
- **Esc during the test gives up.** Nothing is saved yet, so the GUI closes the form and the save stops once the test answers. Once the save writes, Esc waits for it, as before. Closing the first-run window works the same way.

## Wire

```json
{"test_login":{"id":11,"account":{"name":"work","host":"imap.example.com","username":"me@example.com","password":{"keyring":true}},"password":"…"}}
{"reply":{"id":11,"outcome":{"ok":"done"}}}
{"reply":{"id":11,"outcome":{"error":"imap.example.com: login failed: [AUTHENTICATIONFAILED] Authentication failed."}}}
```

The protocol number stays 1: a client only talks to a daemon of its own version, and restarts an older one.

## Tests

- Core: a good and a wrong password through a test daemon, nothing saved and no account started; a password command; an invalid name refused before the login. The wire's `Debug` never shows the password.
- CLI: a refused connection saves nothing and says so.
- MCP: a wrong password command saves nothing.
- GUI: the form and the first-run window keep the fields and say why on a refused login; Esc or closing during the test saves nothing.
