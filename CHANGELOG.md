# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.13.0](https://github.com/postvak-app/postvak/compare/v0.12.0...v0.13.0) - 2026-10-31

### Added

- *(rules)* preview stored mail as if it just arrived with --as-arrival ([#491](https://github.com/postvak-app/postvak/pull/491))
- *(gui)* select and copy the reader's subject, sender and recipients ([#486](https://github.com/postvak-app/postvak/pull/486))
- *(gui)* sender logos on the message list's rows ([#478](https://github.com/postvak-app/postvak/pull/478))

### Fixed

- *(trash)* restore takes a backup's name or path everywhere, and the window's purge reads retention from config.toml ([#489](https://github.com/postvak-app/postvak/pull/489))
- *(rules)* leave apply-existing runs out of hits_30d and last_fired ([#488](https://github.com/postvak-app/postvak/pull/488))
- *(gui)* show HTML mail whose stylesheet floats a table cell ([#480](https://github.com/postvak-app/postvak/pull/480))

### Other

- share front-end logic in core, read less mail, split the GUI state by job ([#490](https://github.com/postvak-app/postvak/pull/490))
- *(mcp)* serve the mcp page from the docs tool, give the sync default and how a stop hold delays tag rules ([#487](https://github.com/postvak-app/postvak/pull/487))

## [0.12.0](https://github.com/postvak-app/postvak/compare/v0.11.0...v0.12.0) - 2026-10-30

### Added

- *(gui)* pick a named list for a condition in the rule editor ([#476](https://github.com/postvak-app/postvak/pull/476))
- *(rules)* agents propose list changes for a human to approve ([#475](https://github.com/postvak-app/postvak/pull/475))
- *(rules)* a delay on a rule's actions (actions.after) ([#473](https://github.com/postvak-app/postvak/pull/473))
- *(rules)* named lists that several rules share ([#472](https://github.com/postvak-app/postvak/pull/472))
- *(rules)* preview rules as of another time with --as-of and as_of ([#471](https://github.com/postvak-app/postvak/pull/471))
- *(gui)* reader and folder pane feedback, remembered remote content, long proposals ([#462](https://github.com/postvak-app/postvak/pull/462))
- *(gui)* Preferences dialog that edits config.toml ([#467](https://github.com/postvak-app/postvak/pull/467))
- *(gui)* make the rule proposal's text selectable ([#463](https://github.com/postvak-app/postvak/pull/463))
- make accept loop poll interval configurable ([#461](https://github.com/postvak-app/postvak/pull/461))

### Fixed

- *(gui)* no task bar entries on Plasma for the tray and the scheme probe ([#468](https://github.com/postvak-app/postvak/pull/468))
- *(gui)* bring the browser to the front for links in the Flatpak ([#466](https://github.com/postvak-app/postvak/pull/466))
- *(gui)* stop each tray click leaving a launch entry on Plasma's task bar ([#465](https://github.com/postvak-app/postvak/pull/465))
- *(rules)* a direct move from the GUI or MCP is not remembered as a rule's move ([#464](https://github.com/postvak-app/postvak/pull/464))

### Other

- specs for undo and the Phase 5 screens; remove stale specs and plans ([#459](https://github.com/postvak-app/postvak/pull/459))

## [0.11.0](https://github.com/postvak-app/postvak/compare/v0.10.0...v0.11.0) - 2026-10-30

### Added

- *(gui)* fade dialogs in, and follow the desktop's reduced motion ([#457](https://github.com/postvak-app/postvak/pull/457))
- *(gui)* match the reader, proposal card, rules table and inbox banner to the canvas ([#456](https://github.com/postvak-app/postvak/pull/456))
- show which rule acted on a message, how often rules act, and proposals with their dry run ([#454](https://github.com/postvak-app/postvak/pull/454))
- *(gui)* group the reader's actions and add Reply, a More menu and attachment chips ([#452](https://github.com/postvak-app/postvak/pull/452))
- *(rules)* warn in rule previews where authenticated cannot hold ([#451](https://github.com/postvak-app/postvak/pull/451))
- *(gui)* give list rows an avatar, a preview line and date headings ([#447](https://github.com/postvak-app/postvak/pull/447))
- *(gui)* bring the folder pane in line with the redesign canvas ([#439](https://github.com/postvak-app/postvak/pull/439))
- offer to delete a message after its one-click unsubscribe succeeds ([#445](https://github.com/postvak-app/postvak/pull/445))
- *(gui)* give buttons a primary and danger style and lift the dark panes ([#438](https://github.com/postvak-app/postvak/pull/438))
- create, rename and delete folders ([#437](https://github.com/postvak-app/postvak/pull/437))
- *(rules)* change the order rules run in by dragging a row, rules move or rules_move ([#424](https://github.com/postvak-app/postvak/pull/424))
- *(rules)* let agents propose edits to existing rules for a human to approve ([#430](https://github.com/postvak-app/postvak/pull/430))
- *(sync)* fetch a folder's newest mail first on its first sync ([#432](https://github.com/postvak-app/postvak/pull/432))
- notify when a rule proposal waits for approval ([#429](https://github.com/postvak-app/postvak/pull/429))
- *(gui)* filter the rules in the Rules view ([#423](https://github.com/postvak-app/postvak/pull/423))

### Fixed

- *(rules)* later rules see the read, flagged and tag changes of earlier rules in the same pass ([#453](https://github.com/postvak-app/postvak/pull/453))
- *(gui)* bring the browser to the front when a link opens ([#450](https://github.com/postvak-app/postvak/pull/450))
- *(mcp)* reload config.toml when it changes, so new trusted_authserv_ids apply ([#442](https://github.com/postvak-app/postvak/pull/442))
- *(store)* thread a reply without References by the message its In-Reply-To names ([#433](https://github.com/postvak-app/postvak/pull/433))
- *(sync)* find special-use folders by common synonyms and under INBOX ([#431](https://github.com/postvak-app/postvak/pull/431))
- *(flatpak)* start the daemon in a sandbox of its own ([#425](https://github.com/postvak-app/postvak/pull/425))
- *(gui)* render HTML mail that stacks its table cells above 600 px ([#428](https://github.com/postvak-app/postvak/pull/428))
- *(sync)* use a \All folder named Archive as the archive when none is marked ([#426](https://github.com/postvak-app/postvak/pull/426))

### Other

- *(gui)* un-ignore the block-cell test and cover block cells in a narrow pane ([#455](https://github.com/postvak-app/postvak/pull/455))
- *(perf)* measure femtovg against the software renderer on NVIDIA and Wayland ([#446](https://github.com/postvak-app/postvak/pull/446))
- *(skills)* review and simplify the diff in the issue skill ([#444](https://github.com/postvak-app/postvak/pull/444))
- *(agents)* stack PRs with gh stack, rebasing only stacked branches ([#441](https://github.com/postvak-app/postvak/pull/441))
- lead the comparison page with what Postvak does and a scannable feature table ([#436](https://github.com/postvak-app/postvak/pull/436))
- serve the docs under /docs/ and the landing page at the root ([#435](https://github.com/postvak-app/postvak/pull/435))
- *(release)* build x86_64 macOS on an Apple silicon runner ([#434](https://github.com/postvak-app/postvak/pull/434))

## [0.10.0](https://github.com/postvak-app/postvak/compare/v0.9.0...v0.10.0) - 2026-10-29

### Added

- *(gui)* make dialogs modal and give buttons more room ([#422](https://github.com/postvak-app/postvak/pull/422))
- *(gui)* edit a rule in a dialog and list the rules as a sortable table ([#421](https://github.com/postvak-app/postvak/pull/421))
- *(account)* offer the authserv-id the INBOX shows for authenticated rules ([#414](https://github.com/postvak-app/postvak/pull/414))
- *(rules)* add a younger_than condition, the opposite of older_than ([#413](https://github.com/postvak-app/postvak/pull/413))

### Fixed

- *(gui)* stop the idle window drawing again every 2 seconds ([#420](https://github.com/postvak-app/postvak/pull/420))
- *(gui)* start another folder's list at the top ([#418](https://github.com/postvak-app/postvak/pull/418))

### Other

- free the worktree's target/ after merging in the issue skill ([#419](https://github.com/postvak-app/postvak/pull/419))
- drop the attribution footer from the issue skill ([#417](https://github.com/postvak-app/postvak/pull/417))
- *(gui)* take a scroll without showing the whole window again ([#415](https://github.com/postvak-app/postvak/pull/415))
- set up codegraph, rust-analyzer and Claude Code for contributors ([#398](https://github.com/postvak-app/postvak/pull/398))

## [0.9.0](https://github.com/postvak-app/postvak/compare/v0.8.0...v0.9.0) - 2026-10-29

### Added

- *(account)* suggest STARTTLS on port 143 when the login test cannot connect over TLS ([#395](https://github.com/postvak-app/postvak/pull/395))
- *(gui)* choose STARTTLS in the add-account form and mark an unencrypted account ([#394](https://github.com/postvak-app/postvak/pull/394))
- *(imap)* support STARTTLS and tls = "none", and send the ID command ([#393](https://github.com/postvak-app/postvak/pull/393))
- *(rules)* warn about a move to a special folder's name instead of its role ([#362](https://github.com/postvak-app/postvak/pull/362))
- *(rules)* warn about a rule watching a folder no account syncs ([#361](https://github.com/postvak-app/postvak/pull/361))
- *(rules)* [**breaking**] make actions a table and the rule syntax consistent ([#387](https://github.com/postvak-app/postvak/pull/387))
- sender statistics and rules coverage for a folder ([#376](https://github.com/postvak-app/postvak/pull/376))
- *(gui)* adapt HTML mail to the dark theme ([#382](https://github.com/postvak-app/postvak/pull/382))
- *(rules)* match the sender's domain and its authentication ([#379](https://github.com/postvak-app/postvak/pull/379))
- *(rules)* propose several rules at once, list them in full and withdraw your own ([#377](https://github.com/postvak-app/postvak/pull/377))
- cursor paging, field selection and counts for list and search ([#381](https://github.com/postvak-app/postvak/pull/381))
- *(mcp)* serve the documentation pages ([#365](https://github.com/postvak-app/postvak/pull/365))
- *(gui)* count the proposals waiting for approval beside Rules ([#368](https://github.com/postvak-app/postvak/pull/368))
- *(rules)* warn about a rule that moves mail to the folder it watches ([#360](https://github.com/postvak-app/postvak/pull/360))

### Fixed

- *(rules)* keep \Noselect folders and refuse a move into one ([#364](https://github.com/postvak-app/postvak/pull/364))
- *(mcp)* [**breaking**] wrap every field a stranger wrote in <untrusted_mail_content> ([#392](https://github.com/postvak-app/postvak/pull/392))
- *(cli)* fail without accounts, as MCP does, through one account selection in core ([#391](https://github.com/postvak-app/postvak/pull/391))
- *(rules)* match INBOX in any case, and move to the server's spelling of a folder ([#363](https://github.com/postvak-app/postvak/pull/363))
- *(gui)* dark HTML keeps scheme-switched backgrounds, dark headers and icon text right ([#390](https://github.com/postvak-app/postvak/pull/390))
- *(search)* keep exact matches first and close-match paging stable ([#389](https://github.com/postvak-app/postvak/pull/389))
- *(imap)* encode folder names as modified UTF-7 and check move targets ([#380](https://github.com/postvak-app/postvak/pull/380))
- *(gui)* wake the window when a one-off thread cannot start ([#388](https://github.com/postvak-app/postvak/pull/388))
- *(gui)* use content-y instead of the deprecated viewport-y ([#386](https://github.com/postvak-app/postvak/pull/386))
- *(macos)* clear makehybrid's FinderInfo from the app on the DMG ([#375](https://github.com/postvak-app/postvak/pull/375))
- *(message)* show an HTML part without markup as text ([#373](https://github.com/postvak-app/postvak/pull/373))
- *(rules)* subscribe to a folder a move creates ([#359](https://github.com/postvak-app/postvak/pull/359))
- *(gui)* context menus are no longer blurry on Wayland ([#345](https://github.com/postvak-app/postvak/pull/345))

### Other

- *(readme)* add macOS and Linux platform badges ([#396](https://github.com/postvak-app/postvak/pull/396))
- *(rules)* example of a delayed move so other devices still notify ([#385](https://github.com/postvak-app/postvak/pull/385))
- *(rules)* list the examples with the keys each one uses ([#383](https://github.com/postvak-app/postvak/pull/383))
- *(gui)* a worker helper, shared test helpers and Theme sizes ([#378](https://github.com/postvak-app/postvak/pull/378))
- *(macos)* compress the DMG with LZMA (ULMO) instead of zlib ([#374](https://github.com/postvak-app/postvak/pull/374))
- compare Postvak with other mail sorting tools ([#369](https://github.com/postvak-app/postvak/pull/369))
- new tagline "Your inbox, organized by rules you write." ([#366](https://github.com/postvak-app/postvak/pull/366))
- *(architecture)* only config.rs writes config.toml ([#358](https://github.com/postvak-app/postvak/pull/358))

## [0.8.0](https://github.com/postvak-app/postvak/compare/v0.7.0...v0.8.0) - 2026-10-27

### Added

- *(gui)* measure startup phases and memory, and report them in CI ([#325](https://github.com/postvak-app/postvak/pull/325))
- *(gui)* fetch a large message's attachments only when saved ([#324](https://github.com/postvak-app/postvak/pull/324))
- *(gui)* show a large message's text and HTML before its attachments download ([#323](https://github.com/postvak-app/postvak/pull/323))
- *(mcp)* add a read:headers scope, and gate header rule conditions on it ([#319](https://github.com/postvak-app/postvak/pull/319))

### Fixed

- *(cli)* [**breaking**] show --json returns the MCP shape ([#339](https://github.com/postvak-app/postvak/pull/339))
- *(actions)* say a placeholder folder is not synced yet ([#338](https://github.com/postvak-app/postvak/pull/338))
- *(gui)* clicking a folder while typing a search ends the typing ([#337](https://github.com/postvak-app/postvak/pull/337))
- *(sync)* end the rules pass when the connection drops ([#334](https://github.com/postvak-app/postvak/pull/334))
- *(gui)* say "1 result · in 1 message" and group every count shown in the window ([#336](https://github.com/postvak-app/postvak/pull/336))
- *(gui)* lay HTML mail out at 601 px or wider so responsive columns stay visible ([#335](https://github.com/postvak-app/postvak/pull/335))
- *(gui)* draw a very tall HTML page without panicking the software renderer ([#333](https://github.com/postvak-app/postvak/pull/333))
- *(gui)* render legacy table attributes in HTML mail ([#332](https://github.com/postvak-app/postvak/pull/332))
- *(message)* keep a comma in a display name from splitting an address field ([#331](https://github.com/postvak-app/postvak/pull/331))
- *(rules)* compare flags case-insensitively when acting ([#330](https://github.com/postvak-app/postvak/pull/330))

### Other

- *(deps)* bump the actions group with 3 updates ([#340](https://github.com/postvak-app/postvak/pull/340))
- *(deps)* mail-parser 1.0 ([#329](https://github.com/postvak-app/postvak/pull/329))
- *(gui)* stop the open-at-login test racing its own change ([#328](https://github.com/postvak-app/postvak/pull/328))
- *(rules)* select a rule folder only when a message needs the server ([#327](https://github.com/postvak-app/postvak/pull/327))
- *(imap)* let the Dovecot test servers start without IPv6 ([#326](https://github.com/postvak-app/postvak/pull/326))
- *(sync)* skip the SELECT of folders whose STATUS shows no change ([#322](https://github.com/postvak-app/postvak/pull/322))
- *(sync)* batch BODYSTRUCTURE fetches and keep the backlog folder selected while indexing ([#311](https://github.com/postvak-app/postvak/pull/311))

## [0.7.0](https://github.com/postvak-app/postvak/compare/v0.6.0...v0.7.0) - 2026-10-25

### Added

- say when an account's server lacks UIDPLUS ([#294](https://github.com/postvak-app/postvak/pull/294))
- *(gui)* Save as… in the desktop's own save dialog ([#286](https://github.com/postvak-app/postvak/pull/286))

### Fixed

- *(core)* keep two writers of one file or store from tripping over each other ([#305](https://github.com/postvak-app/postvak/pull/305))
- *(sync)* back off a flaky connection and jitter the delays ([#308](https://github.com/postvak-app/postvak/pull/308))
- *(credentials)* refuse a password command from a config.toml others can write ([#307](https://github.com/postvak-app/postvak/pull/307))
- *(rules)* restart an approved rule's clock in every account in config.toml ([#306](https://github.com/postvak-app/postvak/pull/306))
- *(sync)* stop retrying a login the server rejected ([#301](https://github.com/postvak-app/postvak/pull/301))
- *(gui)* open stores through the private account directory ([#302](https://github.com/postvak-app/postvak/pull/302))
- *(config)* lock config.toml writes and merge into the file as it reads ([#300](https://github.com/postvak-app/postvak/pull/300))
- *(gui)* close every dialog through one action ([#293](https://github.com/postvak-app/postvak/pull/293))

### Other

- *(store)* read the index backlog, filter-only searches and the attachments filter from indexes ([#304](https://github.com/postvak-app/postvak/pull/304))
- let the /issue skill pick the clearest, most valuable issue itself ([#295](https://github.com/postvak-app/postvak/pull/295))
- *(deps)* update dependencies and Blitz to 0.3.0-beta.4 ([#292](https://github.com/postvak-app/postvak/pull/292))
- let the /issue skill claim an issue with an in progress label ([#291](https://github.com/postvak-app/postvak/pull/291))
- skip the build for changes only under .claude/ ([#290](https://github.com/postvak-app/postvak/pull/290))
- add /issue skill to take an issue from open to merged ([#289](https://github.com/postvak-app/postvak/pull/289))
- *(sync)* let the model take a failed delete's \Deleted undo ([#287](https://github.com/postvak-app/postvak/pull/287))

## [0.6.0](https://github.com/postvak-app/postvak/compare/v0.5.0...v0.6.0) - 2026-10-24

### Added

- *(rules)* warn about a file sync's conflicting copy and rules for missing accounts ([#284](https://github.com/postvak-app/postvak/pull/284))
- open the window at login, out of the way ([#283](https://github.com/postvak-app/postvak/pull/283))
- *(macos)* notifications as Postvak that open the message, and the Dock badge ([#282](https://github.com/postvak-app/postvak/pull/282))
- desktop notifications that open the message, and one window ([#241](https://github.com/postvak-app/postvak/pull/241))
- *(search)* snippets for body hits in the window and JSON ([#244](https://github.com/postvak-app/postvak/pull/244))
- *(search)* operators on the CLI, MCP and the window ([#242](https://github.com/postvak-app/postvak/pull/242))
- *(search)* close matches on the CLI, MCP and the window ([#238](https://github.com/postvak-app/postvak/pull/238))
- *(sync)* [**breaking**] index message bodies in idle time ([#237](https://github.com/postvak-app/postvak/pull/237))
- *(search)* [**breaking**] rebuild the search index and one query syntax on every surface ([#235](https://github.com/postvak-app/postvak/pull/235))
- *(gui)* a sync progress hairline under the status line ([#236](https://github.com/postvak-app/postvak/pull/236))
- test an account's login through the daemon before saving it ([#231](https://github.com/postvak-app/postvak/pull/231))
- *(gui)* rules editor ([#215](https://github.com/postvak-app/postvak/pull/215))
- *(gui)* test the login with a first sync when adding an account ([#229](https://github.com/postvak-app/postvak/pull/229))

### Fixed

- *(rules)* clear \Deleted again when a delete's expunge fails ([#247](https://github.com/postvak-app/postvak/pull/247))
- *(gui)* view source and save .eml from a row menu act on that row ([#248](https://github.com/postvak-app/postvak/pull/248))
- *(mcp)* drop account_add and confine attachment_save to one save folder ([#249](https://github.com/postvak-app/postvak/pull/249))
- *(flatpak)* show the tray icon in the sandbox ([#246](https://github.com/postvak-app/postvak/pull/246))
- *(rules)* move mail out of a folder once, so opposing move rules and drag-backs settle ([#245](https://github.com/postvak-app/postvak/pull/245))
- *(deps)* ctutils, rustls-webpki and webpki-roots past RUSTSEC-2026-0381, -0382 and -0385 ([#239](https://github.com/postvak-app/postvak/pull/239))
- *(gui)* copy and paste on Wayland without X11, through Slint's patched clipboard ([#232](https://github.com/postvak-app/postvak/pull/232))
- *(gui)* sharp text in the rounded panes ([#230](https://github.com/postvak-app/postvak/pull/230))
- *(flatpak)* share X11 under Wayland so copy and paste work ([#228](https://github.com/postvak-app/postvak/pull/228))
- *(gui)* set the Wayland app id after the platform exists ([#227](https://github.com/postvak-app/postvak/pull/227))

### Other

- *(gui)* open a message without stalling the window ([#257](https://github.com/postvak-app/postvak/pull/257))
- batch IMAP actions, lean rules reads and a quicker daemon ([#280](https://github.com/postvak-app/postvak/pull/280))
- *(mcp)* the daemon tests that act on synced mail ask for the read scope, so their sync runs ([#243](https://github.com/postvak-app/postvak/pull/243))
- *(gui)* search as you type without stalling the window ([#256](https://github.com/postvak-app/postvak/pull/256))
- *(mcp)* trash restore saves its backup now, so the daemon's purge cannot race it ([#240](https://github.com/postvak-app/postvak/pull/240))
- *(agents)* platforms and desktop integrations everywhere ([#234](https://github.com/postvak-app/postvak/pull/234))
- *(deps)* bump the cargo group with 7 updates ([#223](https://github.com/postvak-app/postvak/pull/223))
- link the Docker image from the README ([#225](https://github.com/postvak-app/postvak/pull/225))
- link the README's claims to the pages that explain them ([#224](https://github.com/postvak-app/postvak/pull/224))

## [0.5.0](https://github.com/postvak-app/postvak/compare/v0.4.0...v0.5.0) - 2026-10-19

### Added

- *(packaging)* publish a headless Docker image on ghcr.io ([#218](https://github.com/postvak-app/postvak/pull/218))

### Fixed

- *(macos-app)* build the release's dSYM before packaging with --skip-build ([#221](https://github.com/postvak-app/postvak/pull/221))
- *(gui)* stop parsing HTML mail once it nests too deep ([#219](https://github.com/postvak-app/postvak/pull/219))
- *(daemon)* stop cleanly on SIGTERM and SIGINT ([#217](https://github.com/postvak-app/postvak/pull/217))

### Other

- *(release-plz)* refresh the release PR on every push to main ([#220](https://github.com/postvak-app/postvak/pull/220))
- *(gui)* nest just past the depth limit, not 100,000 levels deep ([#216](https://github.com/postvak-app/postvak/pull/216))

## [0.4.0](https://github.com/postvak-app/postvak/compare/v0.3.1...v0.4.0) - 2026-10-18

### Added

- *(gui,mcp)* confirm before rules apply-existing expunges mail ([#209](https://github.com/postvak-app/postvak/pull/209))
- local crash reports for the GUI and the daemon, without mail data ([#210](https://github.com/postvak-app/postvak/pull/210))
- *(cli)* confirm before rules apply-existing expunges mail ([#205](https://github.com/postvak-app/postvak/pull/205))
- *(gui)* sender brand logos (BIMI) in the reader ([#201](https://github.com/postvak-app/postvak/pull/201))
- *(gui)* Inkt window palettes, Bricolage Grotesque headings, Ossenbloed retired ([#195](https://github.com/postvak-app/postvak/pull/195))
- *(brand)* try the Kaart mark on the Inkt palette ([#194](https://github.com/postvak-app/postvak/pull/194))
- account add through the library, and MCP accounts, status and attachment_save ([#186](https://github.com/postvak-app/postvak/pull/186))
- *(rules)* add flagged, larger_than and stop, and decode header matches ([#183](https://github.com/postvak-app/postvak/pull/183))

### Fixed

- *(flatpak)* tolerate re-signing a commit the key already signed ([#213](https://github.com/postvak-app/postvak/pull/213))
- harden Postvak against the security audit's findings ([#211](https://github.com/postvak-app/postvak/pull/211))
- *(cli)* strip invisible characters from --json output ([#202](https://github.com/postvak-app/postvak/pull/202))
- *(imap)* give every command after login a timeout, reconnecting on a stalled server ([#208](https://github.com/postvak-app/postvak/pull/208))
- *(imap)* a move refused after the copy clears \Deleted from the source again ([#207](https://github.com/postvak-app/postvak/pull/207))
- *(mcp)* strip invisible characters and flag mail headers as untrusted ([#200](https://github.com/postvak-app/postvak/pull/200))

### Other

- over-engineering quick wins ([#212](https://github.com/postvak-app/postvak/pull/212))
- *(macos)* bundle a dSYM in Postvak.app so crash reports name file:line ([#214](https://github.com/postvak-app/postvak/pull/214))
- pull requests only type-check on macOS ([#206](https://github.com/postvak-app/postvak/pull/206))
- keep sccache read-only in the merge queue ([#204](https://github.com/postvak-app/postvak/pull/204))
- deterministic rules instead of an AI sorting your mail, as a selling point ([#203](https://github.com/postvak-app/postvak/pull/203))
- cache workspace crates with sccache, and keep main's caches warm ([#199](https://github.com/postvak-app/postvak/pull/199))
- unread counts from an index of unread mail, and a 2 ms list budget ([#193](https://github.com/postvak-app/postvak/pull/193))
- *(gui)* reload the list off the window thread after a sync event ([#192](https://github.com/postvak-app/postvak/pull/192))
- *(agents)* drop the first-publish note for the workspace crates ([#189](https://github.com/postvak-app/postvak/pull/189))
- deny unreachable!, check dependency rules, run property tests nightly ([#184](https://github.com/postvak-app/postvak/pull/184))
- *(macos-app)* push the cask only after release.yml has pushed the formula ([#185](https://github.com/postvak-app/postvak/pull/185))

## [0.3.1](https://github.com/postvak-app/postvak/compare/v0.3.0...v0.3.1) - 2026-10-17

### Added

- *(gui)* calmer inbox with sender avatars ([#169](https://github.com/postvak-app/postvak/pull/169))
- *(gui)* fall back to the software renderer and add safe mode ([#167](https://github.com/postvak-app/postvak/pull/167))
- Flatpak desktop app with its own repository on the docs site ([#161](https://github.com/postvak-app/postvak/pull/161))
- *(gui)* declutter the window and calm its look ([#151](https://github.com/postvak-app/postvak/pull/151))
- unsubscribe from mailing lists ([#148](https://github.com/postvak-app/postvak/pull/148))
- *(gui)* select rows with Shift-click, Cmd-click and Cmd+A ([#146](https://github.com/postvak-app/postvak/pull/146))
- search the mail server for bodies not downloaded yet ([#147](https://github.com/postvak-app/postvak/pull/147))
- report messages as junk or not junk ([#144](https://github.com/postvak-app/postvak/pull/144))
- filter the list to unread, flagged or with attachments ([#145](https://github.com/postvak-app/postvak/pull/145))
- *(gui)* fade in mail that arrives while the window is open ([#143](https://github.com/postvak-app/postvak/pull/143))
- *(gui)* say so when the message list is empty ([#142](https://github.com/postvak-app/postvak/pull/142))
- *(gui)* fade read state and account status changes ([#141](https://github.com/postvak-app/postvak/pull/141))
- *(gui)* show what is being dragged and where it will land ([#140](https://github.com/postvak-app/postvak/pull/140))
- *(gui)* drag messages onto a folder to move them ([#139](https://github.com/postvak-app/postvak/pull/139))
- *(gui)* fade hover backgrounds in and out ([#138](https://github.com/postvak-app/postvak/pull/138))
- *(gui)* rewrite the GUI in Slint ([#134](https://github.com/postvak-app/postvak/pull/134))
- *(gui)* All inboxes, every account's INBOX in one list ([#124](https://github.com/postvak-app/postvak/pull/124))
- *(gui)* unarchive from the Archive folder ([#123](https://github.com/postvak-app/postvak/pull/123))
- *(gui)* right-click menus for messages and folders ([#125](https://github.com/postvak-app/postvak/pull/125))
- *(gui)* select and copy text in the HTML view, and colour <font color> text ([#117](https://github.com/postvak-app/postvak/pull/117))

### Fixed

- *(config)* keep comments when saving config.toml ([#152](https://github.com/postvak-app/postvak/pull/152))
- *(gui)* bring the window to the front from the menu bar ([#137](https://github.com/postvak-app/postvak/pull/137))
- *(gui)* open right-click menus again ([#136](https://github.com/postvak-app/postvak/pull/136))
- *(gui)* render newsletter HTML properly, and load remote content on request ([#133](https://github.com/postvak-app/postvak/pull/133))
- *(gui)* line the UI up on the 4/8 px grid ([#122](https://github.com/postvak-app/postvak/pull/122))

### Other

- cargo-deny and SHA-pinned actions ([#179](https://github.com/postvak-app/postvak/pull/179))
- correct and complete the pages against the current code ([#177](https://github.com/postvak-app/postvak/pull/177))
- pin the workspace crates exactly and package only what they build from ([#178](https://github.com/postvak-app/postvak/pull/178))
- point the core spec and unstarted plans at the workspace crates ([#176](https://github.com/postvak-app/postvak/pull/176))
- split the crate into a workspace of core, gui, mcp and the binary ([#174](https://github.com/postvak-app/postvak/pull/174))
- *(store)* cover the thread list and unread counts with one index ([#170](https://github.com/postvak-app/postvak/pull/170))
- benchmark over generated stores, with a perf page in the docs ([#168](https://github.com/postvak-app/postvak/pull/168))
- *(imap)* prove chunked first sync with 12 messages instead of 1,200 ([#166](https://github.com/postvak-app/postvak/pull/166))
- *(gui)* check the layout grid as measured rules ([#164](https://github.com/postvak-app/postvak/pull/164))
- test with a ci profile and nextest, skip builds that only touch docs or packaging ([#163](https://github.com/postvak-app/postvak/pull/163))
- cut over-engineering found by a repo audit ([#149](https://github.com/postvak-app/postvak/pull/149))
- *(sync)* answer user commands sooner while the account syncs ([#121](https://github.com/postvak-app/postvak/pull/121))

## [0.3.0](https://github.com/postvak-app/postvak/compare/v0.2.0...v0.3.0) - 2026-10-09

### Changed

- **Renamed to Postvak.** The crate, binary, app, Homebrew formula and cask are now `postvak`, the repository is [postvak-app/postvak](https://github.com/postvak-app/postvak) and the docs are at [postvak.pataar.nl](https://postvak.pataar.nl/). Nothing carries over from a `postbode` install ([#111](https://github.com/postvak-app/postvak/pull/111))

### Added

- *(gui)* search field and tighter action spacing in the toolbar ([#102](https://github.com/postvak-app/postvak/pull/102))
- *(gui)* Ossenbloed visual identity ([#99](https://github.com/postvak-app/postvak/pull/99))
- *(cli)* add `account list` to print configured accounts ([#97](https://github.com/postvak-app/postvak/pull/97))
- *(gui)* reader toolbar with Save .eml, View source and a Text | HTML switch ([#91](https://github.com/postvak-app/postvak/pull/91))
- *(gui)* opt-in notice for a newer release in the status bar ([#90](https://github.com/postvak-app/postvak/pull/90))
- *(gui)* group counts by the device's region ([#82](https://github.com/postvak-app/postvak/pull/82))
- *(gui)* add a toolbar, a sync button and the version ([#81](https://github.com/postvak-app/postvak/pull/81))
- *(gui)* show the full date with seconds and offset in the reader ([#80](https://github.com/postvak-app/postvak/pull/80))
- *(gui)* remember window size, position and pane widths ([#83](https://github.com/postvak-app/postvak/pull/83))
- *(gui)* align message rows in columns with the date on the right ([#79](https://github.com/postvak-app/postvak/pull/79))
- *(gui)* show custom folders as a collapsible tree ([#78](https://github.com/postvak-app/postvak/pull/78))
- *(store)* keep each folder's IMAP hierarchy delimiter ([#77](https://github.com/postvak-app/postvak/pull/77))
- *(gui)* full-width folder rows with icons and counts, views pinned below ([#74](https://github.com/postvak-app/postvak/pull/74))
- *(gui)* add the Phosphor icon font ([#73](https://github.com/postvak-app/postvak/pull/73))
- *(rules)* any, none, tags and value lists in conditions ([#71](https://github.com/postvak-app/postvak/pull/71))
- AppImage for Linux, one file for window, CLI and MCP ([#66](https://github.com/postvak-app/postvak/pull/66))
- macOS app bundle, DMG and Homebrew cask ([#38](https://github.com/postvak-app/postvak/pull/38))
- *(gui)* render HTML mail with Blitz ([#65](https://github.com/postvak-app/postvak/pull/65))

### Fixed

- *(package)* ship the window icon the GUI embeds, so `cargo install postvak` builds
- *(sync)* forget stored folders a clean pass did not sync ([#109](https://github.com/postvak-app/postvak/pull/109))
- *(store)* renumber rule-definition migration to 007 ([#108](https://github.com/postvak-app/postvak/pull/108))
- two mail-loss paths (rule clock on edit, plain EXPUNGE) ([#105](https://github.com/postvak-app/postvak/pull/105))
- *(sync)* make syncing a Gmail account safe ([#104](https://github.com/postvak-app/postvak/pull/104))
- *(log)* never let RUST_LOG trace IMAP commands or keyring secret bytes ([#103](https://github.com/postvak-app/postvak/pull/103))
- *(gui)* draw text in Ubuntu Medium instead of egui's Ubuntu Light ([#95](https://github.com/postvak-app/postvak/pull/95))
- *(sync)* show archived, moved and trashed mail in the target folder ([#92](https://github.com/postvak-app/postvak/pull/92))
- *(gui)* give the macOS Dock icon Apple's margin ([#89](https://github.com/postvak-app/postvak/pull/89))
- *(gui)* prefer the integrated GPU on macOS ([#84](https://github.com/postvak-app/postvak/pull/84))
- *(gui)* keep selected text readable in text fields ([#75](https://github.com/postvak-app/postvak/pull/75))
- *(gui)* darken the light theme's teal and green to pass WCAG AA ([#72](https://github.com/postvak-app/postvak/pull/72))

### Other

- *(gui)* pin the shown version in tests so a release bump keeps the snapshots ([#107](https://github.com/postvak-app/postvak/pull/107))
- describe MCP's privacy-friendly scopes and move it out of the headline ([#106](https://github.com/postvak-app/postvak/pull/106))
- lead the README with rules and say what Postvak does not do ([#101](https://github.com/postvak-app/postvak/pull/101))
- show the light or dark screenshot to match the reader's theme ([#100](https://github.com/postvak-app/postvak/pull/100))
- *(daemon)* make the 'restarting' reply test independent of thread scheduling ([#98](https://github.com/postvak-app/postvak/pull/98))
- show moves, restores and unread counts before the daemon answers ([#96](https://github.com/postvak-app/postvak/pull/96))
- *(gui)* stop racing the HTML render thread with Harness::run ([#93](https://github.com/postvak-app/postvak/pull/93))
- *(gui)* align panes on one 16 px grid, truncate long reader headers ([#88](https://github.com/postvak-app/postvak/pull/88))
- show GUI screenshots from the snapshot tests ([#85](https://github.com/postvak-app/postvak/pull/85))
- running headless on a server or in Docker ([#76](https://github.com/postvak-app/postvak/pull/76))
- refresh after the AppImage and macOS app merges ([#69](https://github.com/postvak-app/postvak/pull/69))

## [0.2.0](https://github.com/pataar/postbode/compare/v0.1.0...v0.2.0) - 2026-10-07

### Added

- *(gui)* Wayland app id and a Linux desktop entry ([#34](https://github.com/pataar/postbode/pull/34))
- *(daemon)* one background daemon owns sync and IMAP; GUI, CLI and MCP are clients ([#16](https://github.com/pataar/postbode/pull/16))
- *(mcp)* MCP server with scopes, account filter and host install ([#15](https://github.com/pataar/postbode/pull/15))
- *(gui)* Backspace deletes on macOS
- *(gui)* Catppuccin Mocha and Latte palettes, coloured rows and the window icon
- *(gui)* activity and trash views with restore, and a key help window
- *(gui)* rules view with proposals and switches; react to rules.toml and config.toml edits
- *(gui)* search the account from the list with /
- *(gui)* show the message body with safe links, attachments and mark-read after a second
- *(gui)* archive, delete, move, read and flag with optimistic rows and rollback
- *(gui)* show per-account activity, errors and history in a status bar with a theme switch
- *(gui)* list threads with keyboard navigation, expansion and multi-select
- *(gui)* add postbode gui with the account and folder tree
- *(config)* add [ui] theme and save_theme that keeps comments
- *(rules)* add set_enabled for toggling a rule in place
- *(engine)* run accounts behind a lock and route commands to them
- *(sync)* drain commands between passes and wake IDLE for them
- *(sync)* add commands, command results and activity events
- *(sync)* fetch envelopes in resumable 500-message chunks with progress checkpoints
- *(store)* add initial_uid_next and thread summaries

### Fixed

- *(cli)* show times in local time, from a shared time module ([#58](https://github.com/pataar/postbode/pull/58))
- *(gui)* explain startup errors in a window ([#49](https://github.com/pataar/postbode/pull/49))
- *(config)* refuse `password = { keyring = false }` ([#57](https://github.com/pataar/postbode/pull/57))
- *(service)* pass POSTBODE_HOME on to the service's daemon ([#55](https://github.com/pataar/postbode/pull/55))
- *(daemon)* start without accounts ([#47](https://github.com/pataar/postbode/pull/47))
- *(daemon)* deliver a client's own action result once ([#46](https://github.com/pataar/postbode/pull/46))
- *(gui)* readable bold text in the dark theme ([#44](https://github.com/pataar/postbode/pull/44))
- *(daemon)* sync every account when rules.toml changes ([#45](https://github.com/pataar/postbode/pull/45))
- *(sync)* ghost rows and overwritten backups, found by model-based sync tests ([#37](https://github.com/pataar/postbode/pull/37))
- *(rules)* refuse blank and overlong names and folders ([#32](https://github.com/pataar/postbode/pull/32))
- store Homebrew's stable opt path for the daemon service and MCP hosts ([#29](https://github.com/pataar/postbode/pull/29))
- *(gui)* start the read delay when the body text appears
- *(gui)* keep the cursor on its thread when the thread gets new mail
- *(gui)* a click on the shown message opens it
- *(gui)* migrate the stores before the sync threads start
- *(gui)* open rules.toml in a text editor and log a failed opener
- *(gui)* offer a search hit's other folders in the move picker
- *(gui)* visible pane focus, cached body text and safer attachment names
- *(gui)* mark mail read only after the user opens it
- *(gui)* keep the cursor on its message across reloads
- *(gui)* visible focus ring on text fields and readable light-mode selection
- *(gui)* never link a bare scheme and keep the read timer across a fetched body
- *(gui)* stack optimistic edits per row and explain why actions are off
- *(gui)* keep progress ticks out of the history and errors on the line while offline
- *(gui)* keep egui widget focus out of the way of list keys
- *(sync)* only count synced mail as fresh so a moved-in row cannot hide mail below it
- *(sync)* mark mail in folders without rules as seen so a later body rule skips it
- *(sync)* end a full pass on a lost connection instead of skipping every folder
- *(sync)* clear the wake flag with acquire ordering before draining commands
- *(engine)* join sync threads on drop and report a failed spawn per account
- *(sync)* log a UIDVALIDITY change found between chunks
- *(sync)* persist which mail the rules have seen so aborted passes still notify
- *(sync)* stop draining after a lost connection and drain before INBOX-only passes
- *(sync)* return the real lost-connection error and test command failure paths

### Other

- *(gui)* repaint from the client's subscription, not a relay thread ([#52](https://github.com/pataar/postbode/pull/52))
- *(sync)* move the sync tests into sync/tests.rs ([#64](https://github.com/pataar/postbode/pull/64))
- build Message and AccountConfig fixtures from their defaults ([#62](https://github.com/pataar/postbode/pull/62))
- *(daemon)* end the own-completion test at its own account's sync ([#63](https://github.com/pataar/postbode/pull/63))
- share the action, restore and approve steps of the CLI and MCP ([#51](https://github.com/pataar/postbode/pull/51))
- *(store)* drop the unused Message-ID index ([#61](https://github.com/pataar/postbode/pull/61))
- say what tokio is for in Cargo.toml ([#60](https://github.com/pataar/postbode/pull/60))
- one flag check and one source of account defaults ([#59](https://github.com/pataar/postbode/pull/59))
- *(mail_ops)* a move no longer claims to return the new uid ([#56](https://github.com/pataar/postbode/pull/56))
- *(daemon)* keep test-only client and sync code out of the binary ([#50](https://github.com/pataar/postbode/pull/50))
- *(actions)* drop fetch_bodies' unused progress callback ([#48](https://github.com/pataar/postbode/pull/48))
- describe sync and restore as the daemon does them ([#43](https://github.com/pataar/postbode/pull/43))
- *(cli)* print rule errors without faking an event ([#54](https://github.com/pataar/postbode/pull/54))
- *(daemon)* make the two timing-dependent daemon tests deterministic ([#53](https://github.com/pataar/postbode/pull/53))
- *(gui)* snapshot tests for the main views ([#40](https://github.com/pataar/postbode/pull/40))
- property tests for message, rules and wire parsing ([#36](https://github.com/pataar/postbode/pull/36))
- generate app icons from icon.svg with packaging/icons.sh ([#33](https://github.com/pataar/postbode/pull/33))
- *(mcp)* sweep every tool with junk arguments ([#31](https://github.com/pataar/postbode/pull/31))
- deny unwrap, expect and panic in shipped code ([#28](https://github.com/pataar/postbode/pull/28))
- simplify gui selection, store folder columns and sync pass state ([#14](https://github.com/pataar/postbode/pull/14))
- *(gui)* say in the key help that delete from Trash is permanent
- describe what delete does and what the Trash view lists
- describe the mail window and the [ui] theme
- link the docs site and community files from the README ([#12](https://github.com/pataar/postbode/pull/12))
- sharpen crate keywords and forbid unsafe code ([#9](https://github.com/pataar/postbode/pull/9))
- publish the mdBook docs to GitHub Pages ([#5](https://github.com/pataar/postbode/pull/5))
- add peach-and-mint terminal logo and app icon
- *(engine)* make Engine::start infallible
- *(engine)* retry taking an account lock that a concurrent spawn briefly inherited
- *(sync)* a command sent while offline runs after reconnecting
- *(mail_ops)* split fetch_new into search_uids and fetch_envelopes

## [0.1.0](https://github.com/pataar/postbode/releases/tag/v0.1.0) - 2026-10-06

### Added

- trust an extra CA per account with ca_file
- bound IMAP connect with a timeout and enable TCP keepalive
- rules schema, propose, approve, reject and test --stdin
- list and save attachments
- list --threads groups conversations
- full-text search with optional body fetch
- mark, move, archive and delete commands with --dry-run
- direct message actions through the rules apply path
- rules skip mail restored from trash
- account column, account key and backup subjects in list, log and trash list output
- postbode CLI with run, sync, rules, list, show, log and trash
- per-account sync loop with IDLE, rules pass and trash purge
- apply rule actions with trash-first delete
- trash directory for deleted mail backups
- IMAP MailOps on async-imap with IDLE
- MailOps trait with recording fake
- pure rules evaluation with notification policy
- rules.toml parsing, validation and compilation
- per-account sqlite store with migrations
- header parsing, thread ids and body text extraction
- credentials from keyring or password command
- account config with aliases and identity matching
- platform paths and atomic file writes

### Fixed

- *(imap)* do not wait for DNS past the connect timeout
- *(cli)* ask for an extra CA file in account add
- hint when the keyring waits on a prompt, search bodies offline
- private rules lock, write through symlinked config, keep purging past a bad file
- small final-review fixes
- serialize rules.toml edits and use unique temp files
- delete preview and log say when mail is expunged
- clean and validate agent-supplied rule strings
- approving a rule starts its clock at approval time
- rejecting or approving a rule keeps the user's comments
- a disabled rule starts acting from when it is enabled
- strip control characters from error lines and rule names, sync exits non-zero on folder errors
- report database errors while loading rules as database errors
- show password command errors on the terminal
- create config and rules files private
- rules test rejects unknown names and run validates rules before starting
- reject unknown keys in account config
- forget first-seen times of rules removed from rules.toml
- ignore FETCH responses without INTERNALDATE or headers
- move into a folder that already exists on the server
- drop the re-select after each applied rule
- count folder messages with a COUNT query
- fsync the directory after an atomic rename
- fall back to folder names for unmarked special-use roles
- skip body fetches for messages found by an initial sync
- stable integer id for messages FTS and skip unchanged flag writes
- strip control characters from server text on the terminal and escape notification markup
- equals on from, to and cc matches a bare address
- skip flags a message already has and count only executed actions
- timer loop for servers without IDLE and reset backoff after a full cycle
- report a failing folder and keep syncing the others
- write each folder sync in one transaction after the network calls
- skip rule actions and show on a folder whose UIDVALIDITY changed
- validate new accounts, side-effect-free rule previews, report apply errors
- only newly tracked or reset folders count as initial sync
- isolate rule failures, reset backoff, skip initial-sync notifications
- skip same-folder moves and keep failed applies side-effect free
- absolute deadline for IMAP IDLE
- deleted mail stays silent and clamp older_than
- reject unknown top-level keys and empty rule values
- create private dirs recursively and fsync atomic writes

### Other

- release with release-plz, dist and a Homebrew tap
- fmt, clippy, machete, audit and tests on Linux and macOS with live IMAP
- live sync, rule delete, UIDVALIDITY and CLI exit-code tests
- live IMAP tests against Dovecot with and without MOVE
- plan 3 — CI, releases and live IMAP tests
- CLI store writes, approval clock, restore keyword caveat and trash retention
- approval timing, account scope, dry-run caveat and spec corrections
- spec matches the engine features as built
- regeneration steps in AGENTS.md
- mdBook docs, agent guide and SKILL.md, kept honest by tests
- fake IMAP move reports no new uid, like the real client
- plan for engine features
- share the sync pass, IMAP connect and date formatting
- look up fresh messages in a HashSet during the rules run
- drop unused trash::read_eml and tokio sync feature
- make RecordingOps match IMAP expunge and append semantics
- rely on mail-parser for html body text
- ensure_account is idempotent
- pin rust 1.99 and planned crate versions
- scaffold postbode package with spec and plan
