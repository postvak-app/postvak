# Security policy

Postvak holds IMAP credentials and can delete mail, so security reports are welcome.

## Reporting a vulnerability

Report it privately through [GitHub's private vulnerability reporting](https://github.com/postvak-app/postvak/security/advisories/new). Do not open a public issue.

Include the version (`postvak --version`), your platform, and the steps to reproduce. Leave out real passwords, tokens and message contents.

You can expect a first reply within a week. Once a fix is released, the advisory is published with credit to you, unless you would rather stay anonymous.

## Supported versions

Only the latest release gets security fixes.

## In scope

- Leaking credentials, for example through logs, error messages or files with loose permissions
- A rule or command deleting or moving mail it should not touch
- Skipping the `.eml` backup before a rule deletes mail
- TLS verification problems when talking to the IMAP server
- Crafted messages that crash Postvak or corrupt the local store
- Running a `password_command` from a `config.toml` that another user can write; Postvak refuses unless the file and its folder belong to you or root and are not group- or world-writable ([Passwords](docs/src/accounts.md#passwords))

## Out of scope

- Reading the local mail store or the `.eml` backups with access to your disk, a backup, or your user account. They are not encrypted; Postvak relies on full-disk encryption such as FileVault or LUKS ([Mail on disk](docs/src/accounts.md#mail-on-disk))
