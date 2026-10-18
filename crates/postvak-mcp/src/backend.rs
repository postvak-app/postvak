//! The only MCP code that touches the store, rules.toml or the daemon; the daemon owns every IMAP connection.
use anyhow::{Context as _, Result, bail};
use std::sync::{PoisonError, RwLock};
use std::time::Duration;

use serde_json::{Value, json};

use postvak_core::actions;
use postvak_core::config::{AccountConfig, Config};
use postvak_core::daemon::Client;
use postvak_core::daemon::client::LazyClient;
use postvak_core::message;
use postvak_core::output;
use postvak_core::paths::Paths;
use postvak_core::rules::{self, Action, Rule, RuleFile};
/// The `list` filter, a plain value, so the tools need not name the store.
pub(crate) use postvak_core::store::Filter;
use postvak_core::store::{Message, Store};
use postvak_core::sync::{Command, Event};
use postvak_core::time;
use postvak_core::trash::Trash;

pub const MAX_LIMIT: u32 = 500;
const ACCOUNT_START: Duration = Duration::from_secs(10);

pub struct Backend {
    /// Replaced by `account_add`, so a server outlives the config it started with.
    config: RwLock<Config>,
    /// The `--account` filter; empty shows every account.
    only: Vec<String>,
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
            config: RwLock::new(config.clone()),
            only: only.to_vec(),
            daemon,
            paths: paths.clone(),
        })
    }

    fn config(&self) -> Config {
        self.config
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
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
            bail!("no accounts configured; call account_add or run `postvak account add`");
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

    pub fn list(
        &self,
        account: Option<&str>,
        folder: &str,
        limit: u32,
        threads: bool,
        filter: Filter,
    ) -> Result<Vec<Value>> {
        let limit = limit.min(MAX_LIMIT);
        let mut rows = Vec::new();
        for acc in self.select(account)? {
            let store = self.store(&acc)?;
            if threads {
                for thread in store.threads(folder, limit, filter)? {
                    for (m, depth) in thread.iter().zip(output::depths(&thread)) {
                        let mut row = message_row(&acc.name, m)?;
                        row["depth"] = depth.into();
                        rows.push(row);
                    }
                }
            } else {
                for m in store.messages(folder, limit, filter)? {
                    rows.push(message_row(&acc.name, &m)?);
                }
            }
        }
        Ok(rows)
    }

    /// `bodies` searches stored body text too; without it the query matches subject and addresses as plain words.
    /// `server` asks the mail server as well, which searches bodies, so it needs `bodies`.
    pub fn search(
        &self,
        query: &str,
        account: Option<&str>,
        folder: Option<&str>,
        limit: u32,
        bodies: bool,
        server: bool,
    ) -> Result<Vec<Value>> {
        if server && !bodies {
            bail!("a server search matches message bodies, which needs the read:bodies scope");
        }
        let limit = limit.min(MAX_LIMIT);
        let mut rows = Vec::new();
        for acc in self.select(account)? {
            let store = self.store(&acc)?;
            let mut found = if bodies {
                store.search(query, folder, limit)?
            } else {
                store.search_headers(query, folder, limit)?
            };
            if server {
                let hits = self.daemon.search_server(&acc.name, folder, query)?;
                found = actions::with_server_hits(&store, found, &hits, limit)?;
            }
            for m in &found {
                rows.push(message_row(&acc.name, m)?);
            }
        }
        Ok(rows)
    }

    /// The message row and its plain body text, fetched once if not stored yet.
    pub fn show(&self, account: Option<&str>, folder: &str, uid: u32) -> Result<(Value, String)> {
        let (acc, store, msg) = self.message(account, folder, uid)?;
        let raw = self.daemon.raw_message(&acc.name, &store, &msg)?;
        Ok((message_row(&acc.name, &msg)?, message::body_text(&raw)))
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

    /// Saves attachment `n` into `dir`, which must be absolute: the server's working directory is the host's, not
    /// the agent's.
    pub fn attachment_save(
        &self,
        account: Option<&str>,
        folder: &str,
        uid: u32,
        n: usize,
        dir: &std::path::Path,
    ) -> Result<Value> {
        if !dir.is_absolute() {
            bail!("dir must be an absolute path");
        }
        if !dir.is_dir() {
            bail!("no directory {}", dir.display());
        }
        let (acc, store, msg) = self.message(account, folder, uid)?;
        let raw = self.daemon.raw_message(&acc.name, &store, &msg)?;
        let path = message::save_attachment(&raw, n, dir)
            .with_context(|| format!("saving attachment {n}"))?;
        Ok(json!({ "account": acc.name, "folder": folder, "uid": uid, "n": n, "path": path }))
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

    /// Saves the account and reports its first sync; refused while `--account` hides accounts, since the new one
    /// would be invisible to this server.
    pub fn account_add(&self, account: AccountConfig) -> Result<Value> {
        if !self.only.is_empty() {
            bail!("this server is limited with --account and cannot add accounts");
        }
        let name = account.name.clone();
        {
            // Start from the file, not the start-up snapshot, or edits made since would be lost on save.
            let mut held = self.config.write().unwrap_or_else(PoisonError::into_inner);
            let mut config = Config::load(&self.paths.config_file())?;
            // Replacing would let an agent point the user's account at a server it chose.
            if config.accounts.iter().any(|a| a.name == name) {
                bail!("account '{name}' exists already; an agent can only add new accounts");
            }
            postvak_core::account::add(&mut config, &self.paths, account.clone(), None)?;
            *held = config;
        }
        postvak_core::account::await_started(&*self.daemon.client()?, &name, ACCOUNT_START)
            .map_err(|e| {
                e.context(format!(
                    "account '{name}' was saved to config.toml and the daemon will start it"
                ))
            })?;
        let sync = self.sync(Some(&name))?.swap_remove(0);
        Ok(json!({ "account": output::account(&account), "sync": sync }))
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

    /// Rules scoped to a hidden account are left out; rules for every account stay.
    pub fn rules_list(&self) -> Result<Vec<Value>> {
        let file = rules::load(&self.paths.rules_file())?;
        Ok(file
            .rules
            .iter()
            .filter(|r| self.is_visible(r.account.as_deref()))
            .map(output::rule)
            .collect())
    }

    pub fn rules_check(&self) -> Result<Value> {
        let count = rules::compile(&rules::load(&self.paths.rules_file())?)?.len();
        Ok(json!({ "rules": count }))
    }

    pub fn rules_schema(&self) -> Result<Value> {
        Ok(serde_json::from_str(&rules::schema())?)
    }

    /// Previews one rule as if enabled, on the cached mail of the visible accounts; rows never carry mail text.
    /// Without `bodies` a body condition is refused, since its matches would reveal body text.
    pub fn rules_test(
        &self,
        mut rule: Rule,
        account: Option<&str>,
        bodies: bool,
    ) -> Result<Vec<Value>> {
        rule.enabled = true;
        let compiled = rules::compile(&RuleFile { rules: vec![rule] })?;
        refuse_body_rule(&compiled, bodies)?;
        let mut rows = Vec::new();
        for acc in self.select(account)? {
            let store = self.store(&acc)?;
            for p in actions::planned(&compiled, &store, &acc, &acc.identity()?, time::now())? {
                rows.push(output::planned_row(&acc.name, &p));
            }
        }
        Ok(rows)
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
        bodies: bool,
    ) -> Result<Vec<Value>> {
        self.ensure_rule_visible(name, false)?;
        let compiled = vec![rules::enabled_rule(&self.paths.rules_file(), name)?];
        refuse_body_rule(&compiled, bodies)?;
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
            let command = Command::ApplyRule {
                name: name.to_string(),
            };
            let (evaluated, actions, errors) =
                self.daemon
                    .request(&acc.name, command, |reply| match reply {
                        Event::RuleApplied {
                            evaluated,
                            actions,
                            errors,
                            ..
                        } => Ok((evaluated, actions, errors)),
                        other => Err(other),
                    })?;
            rows.push(output::rule_applied(&acc.name, evaluated, actions, &errors));
        }
        Ok(rows)
    }

    /// While accounts are hidden, a rule without `account` gets the only visible one, or is refused.
    pub fn propose(&self, mut rule: Rule, by: &str, bodies: bool) -> Result<Value> {
        refuse_body_rule(
            &rules::compile(&RuleFile {
                rules: vec![rule.clone()],
            })?,
            bodies,
        )?;
        match rule.account.as_deref() {
            Some(account) if !self.is_visible(Some(account)) => {
                bail!("no account named '{account}'")
            }
            None if self.narrowed() => rule.account = Some(self.single(None)?.name),
            _ => {}
        }
        let (name, account) = (rule.name.clone(), rule.account.clone());
        rules::edit::propose(&self.paths.rules_file(), rule, by)?;
        Ok(json!({ "proposed": name, "account": account, "enabled": false, "proposed_by": by }))
    }

    /// Enables the rule and restarts its clock in every account, so it acts only on mail that arrives from now on.
    pub fn approve(&self, name: &str, bodies: bool) -> Result<Value> {
        self.ensure_rule_visible(name, true)?;
        refuse_named_body_rule(&self.paths, name, bodies)?;
        rules::edit::approve_from_now(&self.paths, &self.all_accounts(), name, time::now())?;
        Ok(json!({ "rule": name, "enabled": true }))
    }

    pub fn reject(&self, name: &str) -> Result<Value> {
        self.ensure_rule_visible(name, false)?;
        rules::edit::reject(&self.paths.rules_file(), name)?;
        Ok(json!({ "rejected": name }))
    }

    pub fn set_enabled(&self, name: &str, enabled: bool, bodies: bool) -> Result<Value> {
        self.ensure_rule_visible(name, enabled)?;
        if enabled {
            refuse_named_body_rule(&self.paths, name, bodies)?;
        }
        rules::edit::set_enabled(&self.paths.rules_file(), name, enabled)?;
        Ok(json!({ "rule": name, "enabled": enabled }))
    }
}

fn refuse_named_body_rule(paths: &Paths, name: &str, bodies: bool) -> Result<()> {
    let file = rules::load(&paths.rules_file())?;
    let named: Vec<Rule> = file.rules.into_iter().filter(|r| r.name == name).collect();
    refuse_body_rule(&rules::compile(&RuleFile { rules: named })?, bodies)
}

/// Without read:bodies an agent may not propose, enable or run a rule that matches on the body: which mail it acts on,
/// or a dry run lists, would reveal body text.
fn refuse_body_rule(compiled: &[rules::CompiledRule], bodies: bool) -> Result<()> {
    if !bodies && compiled.iter().any(|r| r.needs_body()) {
        bail!("matching on the body needs the read:bodies scope");
    }
    Ok(())
}

/// A message's listing row: the CLI's `--json` shape with its account, without the body.
fn message_row(account: &str, m: &Message) -> Result<Value> {
    let mut row = output::with_account(account, m)?;
    if let Some(object) = row.as_object_mut() {
        object.remove("body_text");
    }
    Ok(row)
}
