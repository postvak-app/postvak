# Quickstart

```sh
brew install postvak-app/tap/postvak    # or cargo, mise, the macOS app, an AppImage or a Flatpak: see Install
postvak account add                # asks for host, user and password, then syncs once
postvak sync                       # first sync of every folder
postvak list                       # newest mail in INBOX
postvak gui                        # the mail window
postvak mcp install claude-desktop # let Claude read your inbox and propose rules
```

Postvak ships with no rules. Add your own to `rules.toml` next to `config.toml`, in `~/.config/postvak/` on Linux or `~/Library/Application Support/postvak/` on macOS. For example, this one deletes read sign-in codes after an hour:

```toml
[[rules]]
name = "purge sign-in codes"
match.subject = { regex = "sign.?in|verification code|magic link" }
match.older_than = "1h"
match.read = true
actions.delete = true
```

```sh
postvak rules test       # dry run: what would each rule do?
postvak run              # keep syncing and applying rules in the foreground
postvak service install  # run in the background at login
```

Deleted mail is kept as `.eml` for 30 days: `postvak trash list` and `postvak trash restore FILE`.

