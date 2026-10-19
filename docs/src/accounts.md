# Accounts

`postvak account add` asks for the details (including an extra CA file, if your server needs one), has the background daemon test the login, and only when it passes writes `config.toml`, keeping your comments and leaving out settings at their default, and syncs the account once. A wrong login saves nothing; running it again with the same name replaces the account. Agents with the `accounts:write` scope can add an account through the MCP `account_add` tool, with a password command and never a plain password; see [MCP](mcp.md). In the window, Mailbox ▸ Add account… does the same, with the password in the OS keyring, and keeps the form open with the error when the login fails; with no account set up yet, the window that opens in place of the mail window holds the same form. You can also edit the file by hand:

| | Linux | macOS |
|---|---|---|
| `config.toml`, `rules.toml` | `~/.config/postvak/` | `~/Library/Application Support/postvak/` |
| Mail store and trash | `~/.local/state/postvak/accounts/<name>/` | `~/Library/Application Support/postvak/accounts/<name>/` |

```toml
[[accounts]]
name = "work"
host = "imap.example.com"
port = 993
username = "me@example.com"
password = { keyring = true }
address = "me@example.com"
aliases = ["me@example.org", "*@shop.example.com"]
sync_interval_secs = 120
trash_retention_days = 30
notify = true
```

| Key | Default | Meaning |
|---|---|---|
| `name` | required | Letters, digits, `-` and `_`. Used in `--account` and for the store directory. |
| `host`, `port` | port 993 | IMAP over TLS. STARTTLS on port 143 is not supported yet. |
| `username` | required | The IMAP login. |
| `password` | required | `{ keyring = true }` or `{ command = "pass show mail/work" }`. |
| `address` | the username | Your address. Required when the username is not an email address. |
| `aliases` | none | Other addresses that are you. `*` is a wildcard over the whole address. |
| `sync_interval_secs` | 120 | Full sync interval, at least 10. New INBOX mail arrives sooner through IMAP IDLE. |
| `trash_retention_days` | 30 | How long deleted mail is kept as `.eml`. |
| `notify` | true | Desktop notification for new INBOX mail that no rule moved, archived, deleted or silenced (see [Notifications](rules.md#notifications)). |
| `ca_file` | none | Absolute path to a PEM file with an extra trusted root certificate, for a server with a private CA. Postvak trusts the public web roots it ships with, not the system's certificate store, so a CA you installed on your system goes here too. |

## Appearance

```toml
[ui]
check_updates = false
sender_images = false
theme = "system"
```

`theme` is `"system"` (follow the OS, the default), `"light"` or `"dark"`. The mail window's theme switch writes it.

`check_updates` (off by default) lets the mail window ask GitHub at most once a day whether a newer release is out, and link to its release notes at the foot of the folder pane. The request is a plain HTTPS request to `api.github.com` with the user agent `postvak/<version>`: it carries nothing about your accounts or mail, though GitHub sees your IP address as with any request. Postvak never updates itself; upgrade the way you installed it (`brew upgrade postvak`, `cargo install postvak`, a new AppImage). Set `check_updates = true` to turn the check on.

`sender_images` (off by default) shows the logo a sender's domain publishes through BIMI, the standard Apple Mail and Gmail use, in place of the sender's initials in the reader. A logo only shows on mail your server marked as passing DMARC for the From domain, in the topmost `Authentication-Results` header, so spoofed mail keeps the initials. The privacy cost: for each sender domain, the mail window looks up a DNS record at that domain and fetches its logo over HTTPS, which tells the domain that you received their mail and roughly when you read it. Answers are cached for a week in the cache directory, and nothing about the lookup is logged. The fetch goes only to public hosts and accepts at most 32 KiB. The logo's certificate (VMC) is not checked, so any domain that passes DMARC can show whatever logo it publishes.

## Passwords

`{ keyring = true }` keeps the password in the macOS Keychain or the Secret Service, under service `postvak` and the account name. `account add` stores it there.

`{ command = "..." }` runs the command with `sh -c` and uses its output, without trailing newlines. A non-zero exit or an empty output is an error. The command's own error output goes where the daemon's output goes (see [Logs](daemon.md#logs)), or to your terminal under `postvak run`.

## Aliases

`address` plus `aliases` define "me". Rules use them through `to_me` and `alias`.
