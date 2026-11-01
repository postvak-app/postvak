# Accounts

`postvak account add` asks for the details (including an extra CA file, if your server needs one), has the background daemon test the login, and only when it passes writes `config.toml`, keeping your comments and leaving out settings at their default, and syncs the account once. A wrong login saves nothing; when implicit TLS cannot connect but the server offers STARTTLS on port 143, the error says to use that. Running it again with the same name replaces the account. In the window, Mailbox ▸ Add account… does the same, with the password in the OS keyring, and keeps the form open with the error when the login fails; with no account set up yet, the window that opens in place of the mail window holds the same form. Agents over MCP cannot add accounts, as a password command runs as a shell command as you; see [MCP](mcp.md). You can also edit the file by hand:

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
| `host`, `port` | port 993 | The IMAP server. |
| `tls` | by port | `"implicit"`: TLS from the first byte, as on port 993. `"starttls"`: a plain connection upgraded with STARTTLS before the login, as on port 143; a server that does not offer STARTTLS is refused. Unset, port 143 means `"starttls"` and any other port `"implicit"`. `"none"` is for a server with no TLS at all: it still upgrades when the server offers STARTTLS, and otherwise logs in in plain text, so anyone on the network can read the password and the mail; `postvak daemon status` and the window then mark the account unencrypted. Set `"none"` only in `config.toml` or the `postvak account add` wizard; the window's form offers TLS and STARTTLS. |
| `username` | required | The IMAP login. |
| `password` | required | `{ keyring = true }` or `{ command = "pass show mail/work" }`. |
| `address` | the username | Your address. Required when the username is not an email address. |
| `aliases` | none | Other addresses that are you. `*` is a wildcard over the whole address. |
| `sync_interval_secs` | 120 | Full sync interval, at least 10. New INBOX mail arrives sooner through IMAP IDLE. A pass opens only the folders whose status shows a change, so a short interval costs little. |
| `trash_retention_days` | 30 | How long deleted mail is kept as `.eml`. |
| `notify` | true | Desktop notification for new INBOX mail that no rule moved, archived, deleted or silenced (see [Notifications](rules.md#notifications)). |
| `index_bodies` | true | Download and index the text of every message in the background, so search covers bodies (see [Search](search.md)). Off, only the subject, addresses and the bodies of mail you opened are searched. |
| `ca_file` | none | Absolute path to a PEM file with an extra trusted root certificate, for a server with a private CA. Postvak trusts the public web roots it ships with, not the system's certificate store, so a CA you installed on your system goes here too. |
| `trusted_authserv_ids` | none | The servers whose `Authentication-Results` headers the `authenticated` rule condition believes, by the name at the header's start, such as `["mx.google.com"]`. List only a server of your provider that writes that header on every message it receives. Without one, no mail is authenticated. `postvak account trust ACCOUNT ID` adds one. See [Authenticated mail](rules.md#authenticated-mail). |

## Appearance

```toml
[ui]
check_updates = false
html_colours = "adapt"
remote_content = []
sender_images = false
theme = "system"
```

`theme` is `"system"` (follow the OS, the default), `"light"` or `"dark"`. The mail window's theme switch writes it.

`html_colours` is how HTML mail looks in the dark theme: `"adapt"` (the default) turns light pages dark and keeps text readable, unless the mail brings its own dark design; `"original"` shows every message in its sender's colours on a white page, as in the light theme. With `"adapt"`, the reader's **Original colours** switch shows the open message in its sender's colours (see [HTML mail](gui.md#html-mail)); with `"original"` there is nothing to switch, so it is hidden. Only the mail window reads it; the CLI and MCP never render HTML.

`remote_content` lists the sender domains whose HTML mail loads remote content without asking, while the account's provider vouches for the sender (`trusted_authserv_ids`); without a trusted authserv-id it does nothing. The mail window adds a domain when you load remote content from such a sender, and removes it with **Stop loading** (see [HTML mail](gui.md#html-mail)).

`check_updates` (off by default) lets the mail window ask GitHub at most once a day whether a newer release is out, and link to its release notes at the foot of the folder pane. The request is a plain HTTPS request to `api.github.com` with the user agent `postvak/<version>`: it carries nothing about your accounts or mail, though GitHub sees your IP address as with any request. Postvak never updates itself; upgrade the way you installed it (`brew upgrade postvak`, `cargo install postvak`, a new AppImage). Set `check_updates = true` to turn the check on.

`sender_images` (off by default) shows the logo a sender's domain publishes through BIMI, the standard Apple Mail and Gmail use, in place of the sender's initials in the reader and the message list. A logo only shows on mail your server marked as passing DMARC for the From domain, in the topmost `Authentication-Results` header, so spoofed mail keeps the initials. The privacy cost: for each sender domain whose mail scrolls into view in the list, the mail window looks up a DNS record at that domain and fetches its logo over HTTPS, which tells the domain that you received their mail and roughly when you looked at it, even mail you never open. Answers are cached for a week in the cache directory, and nothing about the lookup is logged. The fetch goes only to public hosts and accepts at most 32 KiB. The logo's certificate (VMC) is not checked, so any domain that passes DMARC can show whatever logo it publishes.

## Mail on disk

Postvak keeps your mail on disk unencrypted; an option to encrypt it is not there yet ([#508](https://github.com/postvak-app/postvak/issues/508)). Each account's folder (see the table above) holds `mail.db`, with the headers of every synced message, the text of the bodies it indexed (`index_bodies`) or you opened, and the whole message when a rule needed it, and `trash/`, with an `.eml` backup of each message a rule or action deleted until `trash_retention_days` passes. Only you can open those folders (mode 0700), but anyone who has your disk, a backup of it, or a program running as you can read all of it. Keep your mail private with full-disk encryption: FileVault on macOS, LUKS or an encrypted home on Linux. A backup or cloud sync of your home folder copies the mail too, so leave the account folders out of it if the backup is not encrypted. Postvak syncs `mail.db` from the server again, but `trash/` holds the only copy of the mail it deleted.

## Passwords

`{ keyring = true }` keeps the password in the macOS Keychain or the Secret Service, under service `postvak` and the account name. `account add` stores it there.

`{ command = "..." }` runs the command with `sh -c` and uses its output, without trailing newlines. A non-zero exit or an empty output is an error. The command's own error output goes where the daemon's output goes (see [Logs](daemon.md#logs)), or to your terminal under `postvak run`.

A password command is code that runs as you, so Postvak checks `config.toml` first, the way OpenSSH checks `~/.ssh/config`: the file and the folder it sits in (after following symlinks, so a file kept in your dotfiles is checked where it lives) must belong to you or to root, and must not be writable by group or others. A folder with the sticky bit, such as `/tmp`, passes on its write bits. If a check fails, the account does not connect and the status line says which path and the fix, for example ``chmod go-w ~/.config/postvak/config.toml``. Fix it and the account connects on its next retry, without a restart. The check covers only accounts that use a command; a keyring account never runs one.

## Aliases

`address` plus `aliases` define "me". Rules use them through `to_me` and `alias`, and a search through `from:me`, `to:me` and `cc:me`.
