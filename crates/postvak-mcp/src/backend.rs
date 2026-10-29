//! The only MCP code that touches the store, rules.toml or the daemon; the daemon owns every IMAP connection.
use anyhow::{Context as _, Result, bail};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use postvak_core::actions;
use postvak_core::config::{AccountConfig, Config};
use postvak_core::daemon::Client;
use postvak_core::daemon::client::LazyClient;
use postvak_core::message;
use postvak_core::output::{self, Fields};
use postvak_core::page;
/// What `list` shows, a plain value like `Filter`.
pub(crate) use postvak_core::page::ListRequest;
use postvak_core::paths::Paths;
use postvak_core::rules::edit::Place;
use postvak_core::rules::{self, Action, Rule, RuleFile};
use postvak_core::search::{self, Scope};
use postvak_core::senders::{self, GroupBy};
/// The `list` filter, a plain value, so the tools need not name the store.
pub(crate) use postvak_core::store::Filter;
use postvak_core::store::{Message, Store};
use postvak_core::sync::{Command, Event};
use postvak_core::time;
use postvak_core::trash::Trash;

const MAX_LIMIT: u32 = 500;

pub struct Backend {
    config: Config,
    /// The `--account` filter; empty shows every account.
    only: Vec<String>,
    /// The one folder `attachment_save` writes into, or below; None when there is no Downloads folder and no
    /// `--save-dir`.
    save_dir: Option<PathBuf>,
    daemon: LazyClient,
    paths: Paths,
}

impl Backend {
    /// The accounts in `only`, or all of them; an unknown name is an error.
    pub fn new(config: &Config, paths: &Paths, only: &[String]) -> Result<Backend> {
        Backend::build(config, paths, only, LazyClient::new(paths))
    }

    /// Like `new`, over a daemon connection the caller made.
    pub fn with_client(
        config: &Config,
        paths: &Paths,
        only: &[String],
        client: Client,
    ) -> Result<Backend> {
        Backend::build(config, paths, only, LazyClient::with_client(paths, client))
    }

    fn build(
        config: &Config,
        paths: &Paths,
        only: &[String],
        daemon: LazyClient,
    ) -> Result<Backend> {
        if let Some(name) = only.iter().find(|n| config.account(n).is_none()) {
            bail!("no account named '{name}'");
        }
        Ok(Backend {
            config: config.clone(),
            only: only.to_vec(),
            save_dir: paths.downloads_dir(),
            daemon,
            paths: paths.clone(),
        })
    }

    /// Writes saved attachments into `dir` instead of the Downloads folder.
    pub fn saving_into(self, dir: Option<PathBuf>) -> Backend {
        Backend {
            save_dir: dir,
            ..self
        }
    }

    fn config(&self) -> Config {
        self.config.clone()
    }

    /// Every configured account's name, visible or not: an approved rule's clock restarts in all of them.
    fn all_accounts(&self) -> Vec<String> {
        self.config()
            .accounts
            .iter()
            .map(|a| a.name.clone())
            .collect()
    }

    /// The accounts this server shows.
    fn visible(&self) -> Vec<AccountConfig> {
        self.config()
            .accounts
            .into_iter()
            .filter(|a| self.only.is_empty() || self.only.contains(&a.name))
            .collect()
    }

    /// The named visible account, or every visible one; none at all is an error that says what to do.
    fn select(&self, name: Option<&str>) -> Result<Vec<AccountConfig>> {
        let visible = self.visible();
        if visible.is_empty() {
            bail!("no accounts configured; ask the human to run `postvak account add`");
        }
        match name {
            Some(name) => Ok(vec![
                visible
                    .into_iter()
                    .find(|a| a.name == name)
                    .with_context(|| format!("no account named '{name}'"))?,
            ]),
            None => Ok(visible),
        }
    }

    /// One account: the named one, or the only visible one.
    fn single(&self, name: Option<&str>) -> Result<AccountConfig> {
        let mut selected = self.select(name)?;
        match (name, selected.len()) {
            (Some(_), _) | (None, 1) => Ok(selected.swap_remove(0)),
            (None, _) => bail!("several accounts are visible; pass account"),
        }
    }

    fn store(&self, account: &AccountConfig) -> Result<Store> {
        Ok(Store::open_account(&self.paths, &account.name)?)
    }

    fn message(
        &self,
        account: Option<&str>,
        folder: &str,
        uid: u32,
    ) -> Result<(AccountConfig, Store, Message)> {
        let acc = self.single(account)?;
        let store = self.store(&acc)?;
        let msg = store
            .message(folder, uid)?
            .with_context(|| format!("no message {folder}/{uid}"))?;
        Ok((acc, store, msg))
    }

    pub fn folders(&self, account: Option<&str>) -> Result<Vec<Value>> {
        let mut rows = Vec::new();
        for acc in self.select(account)? {
            let store = self.store(&acc)?;
            for f in store.folders()? {
                rows.push(output::folder(
                    &acc.name,
                    &f,
                    store.message_count(&f.name)?,
                    store.unread_count(&f.name)?,
                ));
            }
        }
        Ok(rows)
    }

    /// The stores of the accounts `account` selects, each with its identity when `identity`, for a page of `list`
    /// or `search`.
    fn sources(&self, account: Option<&str>, identity: bool) -> Result<Vec<page::Source>> {
        self.select(account)?
            .into_iter()
            .map(|acc| {
                Ok(page::Source {
                    store: self.store(&acc)?,
                    identity: if identity {
                        Some(acc.identity()?)
                    } else {
                        None
                    },
                    account: acc.name,
                })
            })
            .collect()
    }

    /// A page of `list`, at most `limit` rows (or threads), after `cursor`, with the kept `fields`; and the cursor
    /// of the next page when there is one.
    pub fn list(
        &self,
        account: Option<&str>,
        request: &ListRequest,
        limit: u32,
        cursor: Option<&str>,
        fields: &Fields,
    ) -> Result<(Vec<Value>, Option<String>)> {
        let found = page::list(
            &self.sources(account, false)?,
            request,
            limit.min(MAX_LIMIT),
            cursor,
        )?;
        let rows = found
            .rows
            .iter()
            .map(|row| output::list_row(row, fields))
            .collect::<serde_json::Result<_>>()?;
        Ok((rows, found.next))
    }

    /// How many messages, or threads, `list` has in all.
    pub fn list_count(&self, account: Option<&str>, request: &ListRequest) -> Result<u64> {
        Ok(page::list_count(&self.sources(account, false)?, request)?)
    }

    /// `bodies` lets stored body text decide a match; without it only the header columns do.
    /// `server` asks the mail server as well, which searches bodies, so it needs `bodies`; it returns one page and
    /// takes no cursor.
    #[allow(clippy::too_many_arguments)] // the tool's own arguments, one per flag of `postvak search`
    pub fn search(
        &self,
        query: &str,
        account: Option<&str>,
        folder: Option<&str>,
        limit: u32,
        bodies: bool,
        server: bool,
        exact: bool,
        cursor: Option<&str>,
        fields: &Fields,
    ) -> Result<(Vec<Value>, Option<String>)> {
        if server && !bodies {
            bail!("a server search matches message bodies, which needs the read:bodies scope");
        }
        if server && cursor.is_some() {
            bail!(page::SERVER_TAKES_NO_CURSOR);
        }
        let limit = limit.min(MAX_LIMIT);
        let parsed = search::parse(query, postvak_core::time::now());
        let account = search::named_account(account, &parsed)?;
        let sources = self.sources(account, true)?;
        let request = page::SearchQuery {
            query: &parsed,
            scope: if bodies { Scope::All } else { Scope::Headers },
            folder,
            close: !exact,
        };
        let mut found = page::search(&sources, &request, limit, cursor)?;
        if server {
            for source in &sources {
                let hits = self.daemon.search_server(&source.account, folder, query)?;
                found =
                    page::with_server_hits(found, &source.account, &source.store, &hits, limit)?;
            }
        }
        let rows = found
            .rows
            .iter()
            .map(|row| output::search_row(row, fields))
            .collect::<serde_json::Result<_>>()?;
        Ok((rows, found.next))
    }

    /// How many messages `search` finds in all, close matches included where they would show.
    pub fn search_count(
        &self,
        query: &str,
        account: Option<&str>,
        folder: Option<&str>,
        bodies: bool,
        exact: bool,
    ) -> Result<u64> {
        let parsed = search::parse(query, postvak_core::time::now());
        let account = search::named_account(account, &parsed)?;
        let request = page::SearchQuery {
            query: &parsed,
            scope: if bodies { Scope::All } else { Scope::Headers },
            folder,
            close: !exact,
        };
        Ok(page::search_count(&self.sources(account, true)?, &request)?)
    }

    /// The account name, the message and its plain body text, fetched once if not stored yet.
    pub fn show(
        &self,
        account: Option<&str>,
        folder: &str,
        uid: u32,
    ) -> Result<(String, Message, String)> {
        let (acc, store, msg) = self.message(account, folder, uid)?;
        let raw = self.daemon.raw_message(&acc.name, &store, &msg)?;
        Ok((acc.name, msg, message::body_text(&raw)))
    }

    /// The message and every field of its stored header block; nothing is fetched.
    pub fn headers(
        &self,
        account: Option<&str>,
        folder: &str,
        uid: u32,
    ) -> Result<(String, Message, Vec<message::HeaderField>)> {
        let (acc, _store, msg) = self.message(account, folder, uid)?;
        let fields = message::header_fields(&msg.headers);
        Ok((acc.name.clone(), msg, fields))
    }

    /// Unsubscribes by one click where the message allows it; otherwise the row names the page or address to open.
    pub fn unsubscribe(
        &self,
        account: Option<&str>,
        folder: &str,
        uid: u32,
        dry_run: bool,
    ) -> Result<Value> {
        let (acc, _store, msg) = self.message(account, folder, uid)?;
        let report = postvak_core::unsubscribe::unsubscribe(&msg.headers, dry_run)?;
        Ok(output::with_account(&acc.name, &report)?)
    }

    pub fn attachments(&self, account: Option<&str>, folder: &str, uid: u32) -> Result<Vec<Value>> {
        let (acc, store, msg) = self.message(account, folder, uid)?;
        let raw = self.daemon.raw_message(&acc.name, &store, &msg)?;
        message::attachments(&raw)
            .iter()
            .map(|a| Ok(output::with_account(&acc.name, a)?))
            .collect()
    }

    /// Saves attachment `n` into the save folder, or into `dir` below it. The sender names the file and can steer the
    /// agent, so a `dir` that resolves elsewhere, through `..` or a symlink, is refused: outside it a file could run at
    /// login. `dir` must be absolute, as the server's working directory is the host's, not the agent's.
    pub fn attachment_save(
        &self,
        account: Option<&str>,
        folder: &str,
        uid: u32,
        n: usize,
        dir: Option<&Path>,
    ) -> Result<Value> {
        let dir = self.save_target(dir)?;
        let (acc, store, msg) = self.message(account, folder, uid)?;
        let raw = self.daemon.raw_message(&acc.name, &store, &msg)?;
        let path = message::save_attachment(&raw, n, &dir)
            .with_context(|| format!("saving attachment {n}"))?;
        Ok(json!({ "account": acc.name, "folder": folder, "uid": uid, "n": n, "path": path }))
    }

    /// The existing directory `attachment_save` writes into, with symlinks resolved.
    fn save_target(&self, dir: Option<&Path>) -> Result<PathBuf> {
        let root = self
            .save_dir
            .as_deref()
            .context("no folder to save into; the human can start the server with --save-dir")?;
        let root = root
            .canonicalize()
            .with_context(|| format!("no save folder {}", root.display()))?;
        let home = directories::BaseDirs::new().and_then(|d| d.home_dir().canonicalize().ok());
        check_save_dir(&root, home.as_deref())?;
        let Some(dir) = dir else {
            return Ok(root);
        };
        if !dir.is_absolute() {
            bail!("dir must be an absolute path");
        }
        let resolved = dir
            .canonicalize()
            .with_context(|| format!("no directory {}", dir.display()))?;
        if !resolved.starts_with(&root) {
            bail!(
                "{} is outside the save folder {}; leave dir out to save there",
                dir.display(),
                root.display()
            );
        }
        if !resolved.is_dir() {
            bail!("no directory {}", dir.display());
        }
        Ok(resolved)
    }

    pub fn log(&self, account: Option<&str>, limit: u32) -> Result<Vec<Value>> {
        let mut rows = Vec::new();
        for acc in self.select(account)? {
            for entry in self.store(&acc)?.log(limit.min(MAX_LIMIT))? {
                rows.push(output::with_account(&acc.name, &entry)?);
            }
        }
        Ok(rows)
    }

    /// Each backup's `file` is its bare name, which `trash_restore` takes.
    pub fn trash_list(&self, account: Option<&str>) -> Result<Vec<Value>> {
        let mut rows = Vec::new();
        for acc in self.select(account)? {
            for e in Trash::new(self.paths.trash_dir(&acc.name)).list()? {
                let subject = e.headers().subject;
                let file = e.path.file_name().map(|n| n.to_string_lossy().into_owned());
                rows.push(json!({ "account": acc.name, "saved_at": e.saved_at, "folder": e.folder, "uid": e.uid, "subject": subject, "file": file }));
            }
        }
        Ok(rows)
    }

    /// Deletes each visible account's backups older than its `trash_retention_days`, for good.
    pub fn trash_purge(&self, account: Option<&str>) -> Result<Vec<Value>> {
        let mut rows = Vec::new();
        for acc in self.select(account)? {
            let removed = Trash::new(self.paths.trash_dir(&acc.name))
                .purge(acc.trash_retention_days, time::now())?;
            rows.push(json!({ "account": acc.name, "removed": removed }));
        }
        Ok(rows)
    }

    /// A direct action on `uids` in `folder`, logged as done `by` the agent; `dry_run` reads the local store only and
    /// opens no connection.
    pub fn act(
        &self,
        account: Option<&str>,
        folder: &str,
        uids: &[u32],
        action: Action,
        dry_run: bool,
        by: &str,
    ) -> Result<Value> {
        if uids.is_empty() {
            bail!("uids must name at least one message");
        }
        let acc = self.single(account)?;
        let store = self.store(&acc)?;
        if dry_run {
            let (mut would, mut missing) = (Vec::new(), Vec::new());
            for &uid in uids {
                match store.message(folder, uid)? {
                    Some(m) => would.push(json!({
                        "uid": uid, "effect": actions::planned_effect(&store, &m, &action)?, "subject": m.subject,
                    })),
                    None => missing.push(uid),
                }
            }
            return Ok(
                json!({ "account": acc.name, "folder": folder, "dry_run": true, "would": would, "missing": missing }),
            );
        }
        let results = self.daemon.apply(&acc.name, folder, uids, &action, by)?;
        let failed: Vec<Value> = results
            .iter()
            .filter_map(|(uid, result)| {
                result
                    .as_ref()
                    .err()
                    .map(|message| json!({ "uid": uid, "error": message }))
            })
            .collect();
        Ok(
            json!({ "account": acc.name, "folder": folder, "action": action.command_label(), "done": results.len() - failed.len(), "failed": failed }),
        )
    }

    /// Restores a backup named as `trash_list` prints it; paths are refused, so nothing outside the trash directory is read.
    pub fn trash_restore(&self, account: Option<&str>, file: &str, dry_run: bool) -> Result<Value> {
        let bare = std::path::Path::new(file)
            .file_name()
            .is_some_and(|name| name == file);
        let Some((_, folder, _)) = Trash::parse_name(file).filter(|_| bare) else {
            bail!("file must be a name as trash_list returns it");
        };
        let acc = self.single(account)?;
        let path = self.paths.trash_dir(&acc.name).join(file);
        if !path.is_file() {
            bail!("no trash file {file}; see trash_list");
        }
        if dry_run {
            return Ok(
                json!({ "account": acc.name, "file": file, "dry_run": true, "would": format!("restore to {folder}") }),
            );
        }
        let folder = self.daemon.restore(&acc.name, path)?;
        Ok(json!({ "account": acc.name, "restored_to": folder }))
    }

    /// Asks the daemon for one sync with rules per visible account; a refusal is that account's `error`.
    pub fn sync(&self, account: Option<&str>) -> Result<Vec<Value>> {
        let mut rows = Vec::new();
        for acc in self.select(account)? {
            let synced = self
                .daemon
                .request(&acc.name, Command::SyncNow, |reply| match reply {
                    Event::Synced {
                        new_messages,
                        actions,
                        errors,
                        ..
                    } => Ok((new_messages, actions, errors)),
                    other => Err(other),
                });
            rows.push(match synced {
                Ok((new_messages, actions, errors)) => json!({ "account": acc.name, "new_messages": new_messages, "actions": actions, "errors": errors }),
                Err(e) => json!({ "account": acc.name, "error": format!("{e:#}") }),
            });
        }
        Ok(rows)
    }

    /// The visible accounts' listing rows; never the password source.
    pub fn accounts(&self) -> Result<Vec<Value>> {
        Ok(self.visible().iter().map(output::account).collect())
    }

    /// The daemon's status, with only the visible accounts.
    pub fn status(&self) -> Result<Value> {
        let mut status = self.daemon.client()?.status()?;
        status
            .accounts
            .retain(|account| self.is_visible(Some(account.name.as_str())));
        Ok(output::status(&status)?)
    }

    fn is_visible(&self, account: Option<&str>) -> bool {
        account.is_none_or(|a| self.visible().iter().any(|acc| acc.name == a))
    }

    /// Whether the server was started with `--account`, so accounts added later stay hidden too; a rule without `account` then reaches beyond this server.
    fn narrowed(&self) -> bool {
        !self.only.is_empty()
    }

    /// A rule scoped to a hidden account is as good as absent, and while accounts are hidden a rule for every account
    /// cannot be turned on. An unknown name is left to the edit's own error.
    fn ensure_rule_visible(&self, name: &str, enabling: bool) -> Result<()> {
        let file = rules::load(&self.paths.rules_file())?;
        match file.rules.iter().find(|r| r.name == name) {
            Some(rule) if !self.is_visible(rule.account.as_deref()) => {
                bail!("no rule named '{name}'")
            }
            Some(rule) if enabling && rule.account.is_none() && self.narrowed() => {
                let visible: Vec<String> = self.visible().into_iter().map(|a| a.name).collect();
                bail!(
                    "rule '{name}' applies to every account; this server only sees {}",
                    visible.join(", ")
                )
            }
            _ => Ok(()),
        }
    }

    /// Every rule in file order, each with its position in the file. Rules scoped to a hidden account are left out,
    /// so positions may skip; rules for every account stay.
    pub fn rules_list(&self) -> Result<Vec<Value>> {
        let file = rules::load(&self.paths.rules_file())?;
        let mut rows = Vec::new();
        for (position, rule) in file.rules.iter().enumerate() {
            if self.is_visible(rule.account.as_deref()) {
                rows.push(output::rule(rule, position)?);
            }
        }
        Ok(rows)
    }

    /// The number of rules, and what leaves the file valid but not doing what was meant. A server narrowed with
    /// `--account` warns only about the rules it may see, so a warning cannot reveal a hidden account.
    pub fn rules_check(&self) -> Result<Value> {
        let path = self.paths.rules_file();
        let mut file = rules::load(&path)?;
        let count = rules::compile(&file)?.len();
        file.rules
            .retain(|r| !self.narrowed() || self.is_visible(r.account.as_deref()));
        let names = self.all_accounts();
        let accounts: Vec<&str> = names.iter().map(String::as_str).collect();
        let config = self.config();
        let untrusting: Vec<&str> = config
            .accounts
            .iter()
            .filter(|a| !a.can_authenticate())
            .map(|a| a.name.as_str())
            .collect();
        let warnings = rules::warnings(&path, &file.rules, &accounts, &untrusting);
        // Every account counts here: a rule for all of them is quiet when a hidden one syncs its folder, and the rules
        // left are those for visible accounts or for all of them, so a warning names no hidden account.
        let watched = rules::watched_folder_warnings(
            &file.rules,
            &rules::account_folders(&self.paths, &names),
        );
        // These name the account, so only the visible ones.
        let shown: Vec<String> = self.visible().into_iter().map(|a| a.name).collect();
        let folders = rules::folder_warnings(&self.paths, &file.rules, &shown);
        let warnings = [warnings, watched, folders].concat();
        Ok(json!({ "rules": count, "warnings": warnings }))
    }

    pub fn rules_schema(&self) -> Result<Value> {
        Ok(serde_json::from_str(&rules::schema())?)
    }

    /// Previews one rule as if enabled, on the cached mail of the visible accounts; rows never carry mail text.
    /// A condition on text `sight` does not cover is refused, since its matches would reveal that text.
    pub fn rules_test(
        &self,
        mut rule: Rule,
        account: Option<&str>,
        sight: Sight,
    ) -> Result<Vec<Value>> {
        rule.enabled = true;
        let compiled = rules::compile(&RuleFile { rules: vec![rule] })?;
        refuse_unseen(&compiled, sight)?;
        let mut rows = Vec::new();
        for acc in self.select(account)? {
            let store = self.store(&acc)?;
            for p in actions::planned(&compiled, &store, &acc, &acc.identity()?, time::now())? {
                rows.push(output::planned_row(&acc.name, &p));
            }
        }
        Ok(rows)
    }

    /// The rules the analysis tools run: the enabled ones, with `include_pending` the waiting proposals too, less those
    /// `sight` may not run, whose names come back second.
    fn analysis_rules(
        &self,
        include_pending: bool,
        sight: Sight,
    ) -> Result<(Vec<rules::CompiledRule>, Vec<String>)> {
        let all = rules::coverage::rules(&rules::load(&self.paths.rules_file())?, include_pending)?;
        let (seen, unseen): (Vec<_>, Vec<_>) = all
            .into_iter()
            .partition(|rule| refuse_unseen(std::slice::from_ref(rule), sight).is_ok());
        let skipped = unseen
            .into_iter()
            .filter(|rule| self.is_visible(rule.rule.account.as_deref()))
            .map(|rule| rule.rule.name)
            .collect();
        Ok((seen, skipped))
    }

    /// Sender statistics of `folder` in the visible accounts; `dates` is the `after` and `before` bounds as typed.
    pub fn senders(
        &self,
        account: Option<&str>,
        folder: &str,
        by: GroupBy,
        limit: u32,
        dates: (Option<&str>, Option<&str>),
        sight: Sight,
    ) -> Result<Value> {
        let now = time::now();
        let date = |value: Option<&str>| {
            value
                .map(|v| {
                    search::date(v, now).with_context(|| {
                        format!("'{v}' is not a date; use 2026-01-15, or 30d, 3w, 6mo or 1y")
                    })
                })
                .transpose()
        };
        let (after, before) = (date(dates.0)?, date(dates.1)?);
        let (compiled, skipped) = self.analysis_rules(false, sight)?;
        let mut rows = Vec::new();
        for acc in self.select(account)? {
            let store = self.store(&acc)?;
            let identity = acc.identity()?;
            let ctx = rules::coverage::context(&acc.name, &identity, now);
            let found = senders::in_folder(&store, &compiled, &ctx, folder, after, before, by)?;
            for sender in found.iter().take(limit.min(MAX_LIMIT) as usize) {
                rows.push(output::sender(&acc.name, folder, sender)?);
            }
        }
        Ok(json!({ "rows": rows, "skipped_rules": skipped }))
    }

    /// Rules coverage of `folder` in the visible accounts, one row per account.
    pub fn rules_coverage(
        &self,
        account: Option<&str>,
        folder: &str,
        include_pending: bool,
        by: GroupBy,
        limit: u32,
        sight: Sight,
    ) -> Result<Value> {
        let (compiled, skipped) = self.analysis_rules(include_pending, sight)?;
        let limit = limit.min(MAX_LIMIT) as usize;
        let mut rows = Vec::new();
        for acc in self.select(account)? {
            let store = self.store(&acc)?;
            let identity = acc.identity()?;
            let ctx = rules::coverage::context(&acc.name, &identity, time::now());
            let mut coverage = rules::coverage::in_folder(&store, &compiled, &ctx, folder, by)?;
            coverage.unmatched_senders.truncate(limit);
            coverage.contests.truncate(limit);
            rows.push(output::coverage(&acc.name, folder, &coverage)?);
        }
        Ok(json!({ "rows": rows, "skipped_rules": skipped }))
    }

    /// Runs one enabled rule over the existing mail of the visible accounts it applies to, through the daemon; a dry
    /// run plans from the local store instead. Refuses to expunge mail without `yes`. A rule of a hidden account reads
    /// as absent.
    pub fn rules_apply_existing(
        &self,
        name: &str,
        account: Option<&str>,
        dry_run: bool,
        yes: bool,
        sight: Sight,
    ) -> Result<Vec<Value>> {
        self.ensure_rule_visible(name, false)?;
        let compiled = vec![rules::enabled_rule(&self.paths.rules_file(), name)?];
        refuse_unseen(&compiled, sight)?;
        let accounts: Vec<_> = self
            .select(account)?
            .into_iter()
            .filter(|acc| compiled.iter().any(|r| r.applies_to_account(&acc.name)))
            .collect();
        if !dry_run && !yes {
            let mut warnings = Vec::new();
            for acc in &accounts {
                let store = self.store(acc)?;
                warnings.extend(actions::expunge_warning(
                    &compiled,
                    &store,
                    acc,
                    &acc.identity()?,
                    time::now(),
                )?);
            }
            if !warnings.is_empty() {
                bail!(
                    "{}; check with dry_run, then call again with yes once the user agrees",
                    warnings.join("; ")
                );
            }
        }
        let mut rows = Vec::new();
        for acc in accounts {
            if dry_run {
                let store = self.store(&acc)?;
                for p in actions::planned(&compiled, &store, &acc, &acc.identity()?, time::now())? {
                    rows.push(output::planned_row(&acc.name, &p));
                }
                continue;
            }
            let (evaluated, actions, errors) = self.daemon.apply_rule(&acc.name, name)?;
            rows.push(
                json!({ "account": acc.name, "evaluated": evaluated, "actions": actions, "errors": errors }),
            );
        }
        Ok(rows)
    }

    /// Stores the rules of `proposals` disabled, all or none. While accounts are hidden, a rule without `account` gets
    /// the only visible one, or is refused, and a rule of a hidden account can be neither replaced nor named as the
    /// place. `arguments` are one rule's fields or `{"rules": [...]}`, as `rules::edit::read_proposals` reads them.
    /// The result names each rule with the account it got.
    pub fn propose(&self, arguments: Value, by: &str, sight: Sight) -> Result<Vec<Value>> {
        let mut proposals = rules::edit::read_proposals(arguments)?;
        // A rule that does not compile is left to propose_all, which names every failing rule of the batch.
        for rule in &mut proposals.rules {
            if let Ok(compiled) = rules::compile(&RuleFile {
                rules: vec![rule.clone()],
            }) {
                refuse_unseen(&compiled, sight)?;
            }
            match rule.account.as_deref() {
                Some(account) if !self.is_visible(Some(account)) => {
                    bail!("no account named '{account}'")
                }
                None if self.narrowed() => rule.account = Some(self.single(None)?.name),
                _ => {}
            }
        }
        if let Place::Before(anchor) | Place::After(anchor) = &proposals.place {
            self.ensure_rule_visible(anchor, false)?;
        }
        if proposals.replace {
            for rule in &proposals.rules {
                self.ensure_rule_visible(&rule.name, false)?;
            }
        }
        let proposed: Vec<Value> = proposals
            .rules
            .iter()
            .map(|r| json!({ "name": r.name, "account": r.account }))
            .collect();
        rules::edit::propose_all(&self.paths.rules_file(), proposals, by)?;
        Ok(proposed)
    }

    /// Removes the caller's own pending proposal; a rule of a hidden account reads as absent.
    pub fn withdraw(&self, name: &str, by: &str) -> Result<Value> {
        self.ensure_rule_visible(name, false)?;
        rules::edit::withdraw(&self.paths.rules_file(), name, by)?;
        Ok(json!({ "withdrawn": name }))
    }

    /// Enables the rule and restarts its clock in every account, so it acts only on mail that arrives from now on.
    pub fn approve(&self, name: &str, sight: Sight) -> Result<Value> {
        self.ensure_rule_visible(name, true)?;
        refuse_named_unseen(&self.paths, name, sight)?;
        rules::edit::approve_from_now(&self.paths, name, time::now())?;
        Ok(json!({ "rule": name, "enabled": true }))
    }

    pub fn reject(&self, name: &str) -> Result<Value> {
        self.ensure_rule_visible(name, false)?;
        rules::edit::reject(&self.paths.rules_file(), name)?;
        Ok(json!({ "rejected": name }))
    }

    pub fn set_enabled(&self, name: &str, enabled: bool, sight: Sight) -> Result<Value> {
        self.ensure_rule_visible(name, enabled)?;
        if enabled {
            refuse_named_unseen(&self.paths, name, sight)?;
        }
        rules::edit::set_enabled(&self.paths.rules_file(), name, enabled)?;
        Ok(json!({ "rule": name, "enabled": enabled }))
    }
}

fn refuse_named_unseen(paths: &Paths, name: &str, sight: Sight) -> Result<()> {
    let file = rules::load(&paths.rules_file())?;
    let named: Vec<Rule> = file.rules.into_iter().filter(|r| r.name == name).collect();
    refuse_unseen(&rules::compile(&RuleFile { rules: named })?, sight)
}

/// Which mail text the granted scopes show the agent: `read:bodies` and `read:headers`.
#[derive(Clone, Copy, Debug, Default)]
pub struct Sight {
    pub bodies: bool,
    pub headers: bool,
}

/// An agent may not propose, enable or run a rule that matches on text it cannot see: which mail the rule acts on, or
/// a dry run lists, would reveal that text. Bodies need read:bodies, `header` conditions read:headers.
fn refuse_unseen(compiled: &[rules::CompiledRule], sight: Sight) -> Result<()> {
    if !sight.bodies && compiled.iter().any(|r| r.needs_body()) {
        bail!("matching on the body needs the read:bodies scope");
    }
    if !sight.headers && compiled.iter().any(|r| r.matches_header()) {
        bail!("a header condition needs the read:headers scope");
    }
    Ok(())
}

/// A save folder that holds home, or lies in a hidden folder or `Library` in it, would let the agent reach the places
/// that run files at login (`~/.config/autostart`, `~/Library/LaunchAgents`) or that hold keys (`~/.ssh`).
fn check_save_dir(root: &Path, home: Option<&Path>) -> Result<()> {
    let Some(home) = home else {
        return Ok(());
    };
    let hidden = root
        .strip_prefix(home)
        .ok()
        .and_then(|rest| rest.components().next())
        .map(|first| first.as_os_str().to_string_lossy().into_owned());
    let unsafe_root = home.starts_with(root)
        || hidden.is_some_and(|first| first.starts_with('.') || first == "Library");
    if unsafe_root {
        bail!(
            "the save folder {} holds or sits among the files that run at login; choose a folder such as ~/Downloads with --save-dir",
            root.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::check_save_dir;
    use std::path::Path;

    #[test]
    fn a_save_folder_may_not_hold_home_or_its_hidden_folders() {
        let home = Some(Path::new("/home/me"));
        for fine in ["/home/me/Downloads", "/home/me/Mail/saved", "/srv/mail"] {
            assert!(check_save_dir(Path::new(fine), home).is_ok(), "{fine}");
        }
        for refused in [
            "/home/me",
            "/home",
            "/",
            "/home/me/.config",
            "/home/me/.config/autostart",
            "/home/me/.ssh",
            "/home/me/Library/LaunchAgents",
        ] {
            assert!(
                check_save_dir(Path::new(refused), home).is_err(),
                "{refused}"
            );
        }
        assert!(check_save_dir(Path::new("/home/me"), None).is_ok());
    }
}
