# Accounts

`postvak account add` asks for the details (including an extra CA file, if your server needs one), tests the login and writes `config.toml`, keeping your comments and leaving out settings at their default. You can also edit the file by hand:

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
| `address` | the username | Your address, when the username is not one. |
| `aliases` | none | Other addresses that are you. `*` is a wildcard over the whole address. |
| `sync_interval_secs` | 120 | Full sync interval. New INBOX mail arrives sooner through IMAP IDLE. |
| `trash_retention_days` | 30 | How long deleted mail is kept as `.eml`. |
| `notify` | true | Desktop notification for new INBOX mail no rule handled. |
| `ca_file` | none | Absolute path to a PEM file with an extra trusted root certificate, for a server with a private CA. |

## Appearance

```toml
[ui]
check_updates = false
sender_images = false
theme = "system"
```

`theme` is `"system"` (follow the OS, the default), `"light"` or `"dark"`. The mail window's theme switch writes it.

`check_updates` (off by default) lets the mail window ask GitHub at most once a day whether a newer release is out, and link to its release notes at the foot of the folder pane. The request is a plain HTTPS request to `api.github.com` with the user agent `postvak/<version>`: it carries nothing about your accounts or mail, though GitHub sees your IP address as with any request. Postvak never updates itself; upgrade the way you installed it (`brew upgrade postvak`, `cargo install postvak`, a new AppImage). Set `check_updates = true` to turn the check on.

`sender_images` (off by default) shows the logo of each sender's mail domain in the reader, in place of their initials. The first time you open mail from a domain, the mail window asks that domain's web server for `https://<domain>/apple-touch-icon.png`, over HTTPS and only when the domain is a public host. That request tells the domain's owner your IP address and that someone there just read mail from them, much like loading remote content does, though it carries nothing about you or the message. A logo that arrives is kept in the cache directory (`sender-logos/`, one PNG per domain) and never asked for again; a domain without one keeps its initials and is asked again the next time the window opens. Postvak never uses Gravatar or any other service looked up by email address, which would tell a third party who mails you. Set `sender_images = true` to turn the logos on.

## Passwords

`{ keyring = true }` keeps the password in the macOS Keychain or the Secret Service, under service `postvak` and the account name. `account add` stores it there.

`{ command = "..." }` runs the command with `sh -c` and uses its output, without the trailing newline. A non-zero exit is an error, and the command's own error output shows in your terminal.

## Aliases

`address` plus `aliases` define "me". Rules use them through `to_me` and `alias`.
