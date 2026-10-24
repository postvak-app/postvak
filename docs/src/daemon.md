# Background sync

One process, the daemon, syncs every account, runs your rules and sends the notifications for new mail. The mail window, the command line and agents over MCP all talk to it, so they never fight over a mailbox.

## How it starts

You do not start it. Any command that needs the mail server starts it, and it stops a minute after the last client leaves. `postvak run` runs it in the foreground instead and never stops it by itself; Ctrl-C does. Only one daemon runs at a time.

## Always on

To keep mail syncing while no window is open, run the daemon at login:

```sh
postvak service install
```

This writes a launchd agent (`~/Library/LaunchAgents/io.github.postvak_app.postvak.plist`) on macOS or a systemd user unit (`~/.config/systemd/user/postvak.service`) on Linux, and starts it. A daemon that was already running is stopped first, so the service's daemon takes over. Running the command again is safe. `--dry-run` shows the file and the commands and changes nothing. `postvak service remove` stops the service and deletes the file. The service runs the postvak binary from the path it was installed from: for Homebrew that is the `opt/` link, so `brew upgrade` needs nothing more, and for an AppImage it is the `.AppImage` file. After moving postvak or the AppImage, run `postvak service install` again. With `POSTVAK_HOME` set, the service file passes it on, so the service's daemon uses that home too.

The service also lets the [search index](search.md#the-index) finish: the daemon indexes message bodies in its idle time, and a daemon that only runs while a window or a CLI command is open gets less of it.

## On a server

Postvak runs headless on any Linux machine, such as a NAS, a VPS or a Raspberry Pi. Rules act on the IMAP server, so your phone and other mail clients see the sorted mailbox. Two things differ from a desktop:

- **Passwords:** a server has no keyring, so answer `c` (command) when `postvak account add` asks where the password goes, with a command such as `cat /path/to/secret`; or write `config.toml` by hand (see [Accounts](accounts.md)). Set `notify = false` in each account; there is no desktop to notify.
- **Starting at boot:** `postvak service install` writes a systemd *user* unit, and user units only run while you are logged in. Run `loginctl enable-linger $USER` once to start it at boot.

### Docker

The image `ghcr.io/postvak-app/postvak` runs the daemon on amd64 and arm64. It holds the command line, the daemon and the MCP server, without the mail window. On Unraid, use this file with the Compose Manager plugin:

```yaml
services:
  postvak:
    image: ghcr.io/postvak-app/postvak:latest
    container_name: postvak
    volumes:
      - ./data:/data
    restart: unless-stopped
```

- **Data:** the image runs `postvak run` as user 1000 with `POSTVAK_HOME=/data`, so `config.toml` and `rules.toml` go in `./data/config/`, and the mail store in `./data/state/`. Run `mkdir data` before the first `docker compose up`: a directory Docker creates itself belongs to root, and the daemon cannot write it. If `./data` belongs to another user, set `user:` to its owner (`user: "99:100"` on Unraid).
- **Accounts:** put the password in a file under `./data/`, then run `docker compose exec postvak postvak account add` and answer `c` with `cat /data/imap-work`. Set `notify = false` in `./data/config/config.toml`. The daemon picks up config changes on its own.
- **Command line:** `docker compose exec postvak postvak rules test`, and so on. Errors go to `docker compose logs postvak`.
- **Agents:** `postvak mcp install` cannot reach a host outside the container, so add the server to the host's config by hand, with `docker` as the command (its absolute path, such as `/usr/local/bin/docker`, for a host started from the Dock) and `["exec", "-i", "postvak", "postvak", "mcp", "--scopes", "read,rules:propose"]` as its arguments. See [MCP](mcp.md) for the scopes.
- **Upgrades:** tags follow releases: `latest`, `X.Y` and `X.Y.Z`. `docker compose pull && docker compose up -d` upgrades. `gh attestation verify oci://ghcr.io/postvak-app/postvak:latest --repo postvak-app/postvak` checks that an image was built from this repository.

## Status and stopping

```sh
postvak daemon status    # pid, version, uptime, clients and what each account is doing
postvak daemon stop
```

Both only look for a running daemon; they never start one, and say `no daemon running` when there is none (`status --json` prints `null`). SIGTERM (`docker stop`, `systemctl --user stop postvak`) and Ctrl-C stop it the same way as `daemon stop`; a second one exits at once. After `stop`, the next command that needs the server starts it again, and an open mail window does so within 5 s. A service restarts it on its own.

## Logs

A daemon started by a command, or by the launchd service on macOS, writes `daemon.log` in its state directory: `~/Library/Application Support/postvak/` on macOS, `~/.local/state/postvak/` on Linux. The log is emptied at start when it is over 1 MB. It holds the daemon's errors, warnings and notes such as reconnects, but never senders or subjects. Under the systemd service the same output goes to the journal: `journalctl --user -u postvak`. `postvak run` in a terminal prints it there, plus each sync's counts and each new mail's sender and subject. The socket `daemon.sock` and the lock `daemon.lock` live there too. `postvak log` shows what rules and actions did.

## Troubleshooting

- `already running (pid N)`: a daemon holds the lock. Use it, or stop it with `postvak daemon stop` before running another.
- `socket path too long`: the state directory is nested too deep for a Unix socket. Shorten `POSTVAK_HOME`.
- `the daemon stopped; see <log>`: the daemon quit while a command was waiting. The log says why.
- `<account> is offline (<reason>); retrying at HH:MM`: that account could not reach its server. Everything else keeps syncing. `postvak sync` tries it again at once.
- `<account> is restarting`: `config.toml` changed that account and its old sync thread is still finishing. Try again in a moment.
- After an upgrade, the next command notices the old daemon, stops it and starts the new one. A daemon started by the service is restarted by launchd or systemd.
- `the daemon is version X, newer than this postvak (Y); restart this program`: a window or MCP server from before an upgrade is still open. It leaves the newer daemon alone; restart it.
