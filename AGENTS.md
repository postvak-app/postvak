# Postvak — notes for agents

Spec: `docs/superpowers/specs/2026-10-06-postvak-core-design.md`. Read it before changing behaviour.

## Module map
Four crates in one workspace. `crates/postvak-core` holds every module below except `gui`, `update`, `mcp` and `main` + `cli/`; `crates/postvak-gui` holds `gui` (its crate root) and `update`; `crates/postvak-mcp` holds `mcp`; the root crate `postvak` is the binary, whose `gui` and `mcp` features turn the front-end crates on. Below the front ends nothing depends on Slint, Blitz or rmcp, and only `postvak-core` depends on rusqlite (the front ends only in tests), so nothing else can open a store. A published crate cannot reach files outside its directory, so what one embeds from `assets/` or `docs/src/` is a symlink inside it (`crates/postvak-gui/assets/`, `crates/postvak-core/src/agent-guide.md`).

- `account` adding an account (validate, test the login through the daemon, store the keyring secret, save config.toml) and waiting for the daemon to start it; the only code that adds one
- `actions` direct actions on chosen messages (mark, move, archive, delete); same `apply` as rules
- `autostart` opening the window at login (`gui --background`): a launch agent on macOS, an XDG autostart entry on Linux, the Background portal in a Flatpak (`autostart/portal.rs`); the only code that turns it on or off
- `badge` the unread count on the Linux task bar or dock entry (the Unity LauncherEntry signal over D-Bus) or the macOS Dock icon; the GUI keeps it current while open
- `config` accounts and identity (address + aliases)
- `credentials` keyring or password command; `Secret` has no Debug
- `daemon` the background process that owns sync and IMAP: `mod.rs` the server (socket, lock, log, idle exit), `wire` the JSON-lines protocol, `client` what front ends use (CLI, GUI, MCP; auto-starts the daemon), `service` launchd and systemd
- `engine` one sync thread per account, commands in and events out; applies a config change by restarting only the changed accounts, each once its old thread ended; what the daemon runs
- `gui` the Slint window (`postvak-gui`, feature `gui`, on by default), which talks to the daemon through `daemon::Client`: `state` holds the state and is the only code that changes it; `load` reads the list's rows from the stores, on the window thread for a view change and on a thread of its own after a sync event; `reader` reads the opened message (its row, raw bytes and MIME parse) on a thread of its own, coalescing fast steps through the list; `ui/*.slint` draws; `bind` shows the state in the window and turns its callbacks into `UiAction`s, with `rows` the lazy models (list rows, HTML strips); `list`, `folders`, `body`, `status`, `toolbar` (the reader's message actions), `rules` and `keys` word what the views show without a toolkit; `menu` says what each of the window's `MenuAction`s does; `avatar` picks the colour behind a sender's initials; `account` is the add-account form without a toolkit, for `lib.rs` (first run) and `state.rs`; `instance` keeps to one window (`gui.lock`, `gui.sock`): a second start hands the first what it was asked and exits; `persist` is `window.json`; tests use `test_support::Fixture` with `Driver` (the state, keys and a clock) or `Fixture::window()` (the real window under Slint's testing backend), and `snapshots` compares the views with `tests/snapshots/*.png`; `html` is the HTML body view, with its render thread (`html/render.rs`, Blitz), the parse that gives up on markup nested too deep (`html/depth.rs`), the net provider that serves `cid:`, `data:` and downloaded remote content from memory (`html/net.rs`), and the downloads the user asks for (`html/fetch.rs`, public hosts only, never logged); layout follows one grid, `Theme` in `ui/theme.slint` (kept equal to `theme.rs` by a test): rows `row` (24), icon buttons `control` (28), content `inset` (16) from a pane's edge and `pad` (8) inside a row, so use those rather than literal sizes
- `help` help texts shared by the CLI and the MCP tool descriptions
- `mail_ops` `MailOps` trait; `imap.rs` is the real client; `RecordingOps` is the test fake
- `main` + `cli/` clap only; no logic
- `mcp` the MCP server (`postvak-mcp`, feature `mcp`, on by default): `tools` defines the tools and their scopes, `backend` is the only code touching store, rules or the daemon, `install` registers hosts
- `message` header parsing, thread id, body text, the HTML part and its `cid:` images
- `notify` desktop notifications for new mail, where a click opens the message with `postvak gui --open`; on Linux over D-Bus (`notify/dbus.rs`), on macOS through `mac-notification-sys` (`notify/mac.rs`), whose clicks arrive on the run loop the daemon's main thread runs while it waits for clients
- `output` JSON rows shared by `--json` and MCP results
- `paths` platform dirs, atomic writes, `stable_exe` (the binary path that survives `brew upgrade` and AppImage mounts), whether this is the macOS app opened from Finder (`launched_as_app`)
- `rules` parse + validate + schema (`mod.rs`), pure `evaluate` (`engine.rs`), side effects (`apply.rs`), the only writer of rules.toml (`edit.rs`), what leaves the file valid but not doing what was meant: a file sync's conflicting copy, a rule for an account not set up here (`warnings.rs`)
- `search` the query syntax: `parse` reads what the user typed and never fails, `plan` builds the one FTS5 expression and the SQL predicates `Store::search` runs (operators such as `from:` and `after:` are column terms and predicates there); nothing else names the FTS table
- `store` one SQLite file per account; migrations in `crates/postvak-core/migrations/`
- `sync` per-account loop: sync folders in chunks, run rules, run commands, IDLE
- `time` the current Unix time, and timestamps shown in local time
- `trash` `.eml` backups before any rule delete
- `update` whether a newer release is out (GUI only): GitHub's latest release at most once a day, cached; never updates itself

`tests/architecture.rs` checks the ownership rules above that crate boundaries cannot: `mcp`, `rules` (rules.toml writes), `search` (the FTS table) and `gui` views; allowed exceptions live there with a reason. It also checks the dependency graph from `cargo metadata`: nothing `postvak-core` pulls in is Slint, Blitz or rmcp, and no other crate depends on rusqlite outside its tests.

## Surfaces
- A feature ships on the CLI, MCP and GUI together unless its spec names the exception; `help` and `output` are shared so the texts and JSON rows stay identical
- Logic lives in a library module. Anything that touches IMAP goes through the daemon as a `Command`. Front ends only call `postvak-core` and `daemon::Client`

## Platforms
- Postvak supports macOS and Linux. On Linux, KDE Plasma on Wayland comes first; GNOME, other desktops and X11 are supported wherever the desktop allows it
- A desktop integration (notifications, tray, unread badge, keyring, `mailto:` handler, autostart, theme, opening links and files) ships on every supported platform, not only the one it was built on. Where a desktop lacks the mechanism, such as GNOME without a tray, fall back to what works there and say so in `docs/src/`; never leave it silently missing
- The packages carry the integration too: the `.desktop` file and metainfo, Flatpak permissions and portals, the AppImage and `Info.plist` in the macOS app

## Definition of done
- `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test`, `cargo machete` and `cargo deny check` all pass
- New logic has a test; a bug fix has a regression test
- No `unwrap`, `expect`, `panic!`, `unreachable!`, `todo!` or `unimplemented!` outside tests (clippy denies them; tests use `panic!` for a case that cannot happen); where a call cannot fail, a narrow `#[allow]` says why
- Platform coverage reported honestly: say "compiled on" vs "ran on"
- Every new dependency has a one-line reason in `[workspace.dependencies]` of the root `Cargo.toml`; crates name it with `workspace = true`
- Do not broaden a task into adjacent features
- After changing CLI flags or rule types, run `POSTVAK_BLESS=1 cargo test` to regenerate `docs/src/cli.md` and `docs/src/rules.schema.json`, and commit them
- After GUI work, run `TZ=UTC UPDATE_SNAPSHOTS=1 cargo test -p postvak-gui snapshots` and look at the changed PNGs in `tests/snapshots/`; Slint's software renderer draws them, on any machine
- Prose lives in `docs/src/`; `README.md` contains `docs/src/index.md` verbatim

## Privacy
- Never read message bodies from a user's store, and never log bodies or secrets
- Schema, counts and your own test fixtures are fine

## Live IMAP tests
`tests/imap_live.rs` runs against two local Dovecot servers and is skipped unless `POSTVAK_TEST_IMAP_HOST` is set:

```sh
docker compose -f tests/dovecot/compose.yml up -d
POSTVAK_TEST_IMAP_HOST=localhost cargo test --features testing --test imap_live
```

Port 10993 advertises MOVE and UIDPLUS, port 11993 neither. Each test logs in as its own throwaway user. `tests/dovecot/gen-certs.sh` regenerates the test-only CA.

The notification click test runs against a stand-in server on a private session bus, so nothing reaches your desktop: `dbus-run-session -- env POSTVAK_TEST_DBUS=1 cargo test -p postvak-core notify::dbus badge`.

## Property tests
`crates/postvak-core/tests/fuzz.rs` feeds hostile input to the message, rules and wire parsers; the only assertion is no panic. Deeper search: `PROPTEST_CASES=20000 cargo test --release -p postvak-core --test fuzz`. A finding becomes a named test; unfixed ones go in `crates/postvak-core/tests/known_bugs.rs` as `#[ignore = "BUG: …"]`. `.github/workflows/nightly.yml` runs this search, and the one below, every night with ten times the cases.

## Model-based sync tests
`crates/postvak-core/tests/sync_model.rs` runs random server events and sync passes against `RecordingOps`; search deeper with `PROPTEST_CASES=2000 cargo test -p postvak-core --features testing --test sync_model`.

## Build speed
`mise install` brings kache, cargo-nextest and actionlint. Run `kache init` once per machine to make kache your `RUSTC_WRAPPER`; it edits your own `~/.cargo/config.toml`, so the repo does not do it for you. Edit loop: `cargo check`, `cargo nextest run`.

Run `packaging/icons.sh` after changing `assets/icon.svg`, `assets/logo.src.svg` or `assets/social-preview.src.svg`, and commit what it writes.

## Stacked PRs
When a change builds on one still in review, or is too big to review in one go, stack it: branch from the parent PR's branch and open the PR with that branch as its base, so its diff shows only its own commits. PRs are squash-merged and their branches deleted, so after the parent merges GitHub retargets the child to `main`; then merge `main` into the child (never rebase) to drop the parent's now-squashed commits from its diff. Merge bottom-up.

## Releasing
Conventional commits on `main` drive everything. release-plz keeps a release PR open, with one version, one changelog and one tag for all four crates (`release-plz.toml`); merging it publishes them to crates.io and pushes the `vX.Y.Z` tag, and dist's `release.yml` builds the binaries, creates the GitHub release and updates `postvak-app/homebrew-tap`. On the tag, `macos-app.yml` runs `packaging/macos/package.sh` on macOS (Postvak.app on a DMG, notarized once the Apple secrets below exist), attaches the DMG to the release and writes `Casks/postvak.rb` in the tap from `packaging/macos/postvak.rb.in`; rerun it for a tag via workflow_dispatch. Regenerate `release.yml` with `dist generate` after changing `dist-workspace.toml`; never edit it by hand. The same tag starts `appimage.yml`, which waits for that release, repackages its Linux binaries as AppImages (`packaging/linux/appimage.sh`, pinned appimagetool and runtime) and attaches them. `docker.yml` builds the binary without the GUI (CLI, daemon and MCP server) from the tag, on a native runner per architecture, and publishes it as the `ghcr.io/postvak-app/postvak` image for amd64 and arm64 (`packaging/docker/Dockerfile`). `flatpak.yml` does the same as Flatpak bundles (`packaging/linux/flatpak.sh`, manifest in `packaging/linux/flatpak/`), then starts `docs.yml`, whose every deploy mirrors the Flatpak repository from the live site and imports the latest release's bundles (`packaging/linux/flatpak-repo.sh`), so `flatpak update` finds them at `postvak.pataar.nl/flatpak/`.

One-time setup, by the repo owner:
1. Create `postvak-app/homebrew-tap` (public, empty); dist writes `Formula/postvak.rb` and `macos-app.yml` writes `Casks/postvak.rb`.
2. Add repo secret `HOMEBREW_TAP_TOKEN` to `postvak-app/postvak`: a fine-grained token with Contents read/write on `postvak-app/homebrew-tap`.
3. Add repo secret `RELEASE_PLZ_TOKEN`: a fine-grained token with Contents and Pull requests read/write on `postvak-app/postvak`.
4. Publish the current version by hand (crates.io requires the first publish of a crate with a token): `cargo publish --workspace`, then `git tag vX.Y.Z && git push origin vX.Y.Z` to run the first dist release.
5. On crates.io, add a trusted publisher for each of `postvak`, `postvak-core`, `postvak-gui` and `postvak-mcp`: repository `postvak-app/postvak`, workflow `release-plz.yml`.
6. For the macOS app, add repo secrets: `APPLE_CERTIFICATE` (base64 of the Developer ID Application .p12), `APPLE_CERTIFICATE_PASSWORD`, `KEYCHAIN_PASSWORD` (any random string), `APPLE_ID`, `APPLE_PASSWORD` (an app-specific password) and `APPLE_TEAM_ID`. Without them the DMG is signed ad hoc, and the cask keeps a `postflight` that clears the quarantine flag (the `@ADHOC@` lines in `packaging/macos/postvak.rb.in`); with them those lines are dropped. The packaging has never run on a Mac before its first PR run; check that DMG by hand once.
7. Under Settings → Code security, enable Dependabot alerts and security updates; version updates come from `.github/dependabot.yml`.
8. For a signed Flatpak repository, add repo secret `FLATPAK_GPG_PRIVATE_KEY`: an armored secret key without a passphrase, used only for this (`gpg --quick-gen-key "Postvak Flatpak repository" rsa4096 sign never`, then `gpg --armor --export-secret-keys <fingerprint>`); keep a backup offline. `docs.yml` signs every deploy with it and puts its public key in `postvak.flatpakrepo`. Losing or replacing the key means every user re-adds the remote.
9. After `docker.yml` first pushes the image, make the `postvak` package on ghcr.io public (Package settings → Change visibility); new packages start private.
