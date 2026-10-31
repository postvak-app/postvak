//! The rules view: hit counts, applying a rule to existing mail, drafts, dry runs and the rules file.
use super::*;

impl App {
    /// Counts each rule's hits in every account's store on a thread of its own; one count at a time.
    pub(super) fn count_hits(&mut self) {
        if self.counting_hits.is_some() {
            return;
        }
        let paths = self.paths.clone();
        let names: Vec<String> = self.accounts.iter().map(|a| a.name.clone()).collect();
        self.counting_hits = Some(worker::oneshot("rule-hits", self.wake.clone(), move || {
            let counted = names
                .iter()
                .map(|name| Store::open_account(&paths, name))
                .collect::<Result<Vec<_>, _>>()
                .and_then(|stores| {
                    rule_file::hits::total(
                        &stores.iter().collect::<Vec<_>>(),
                        postvak_core::time::now(),
                    )
                });
            counted.unwrap_or_else(|e| {
                log::warn!("could not count the rules' hits: {e}");
                HashMap::new()
            })
        }));
    }

    pub(super) fn receive_hits(&mut self) {
        let Some(answer) = &self.counting_hits else {
            return;
        };
        match worker::poll(answer) {
            None => {}
            Some(found) => {
                self.counting_hits = None;
                if let Ok(hits) = found {
                    self.rules.set_hits(&hits);
                    self.rule_hits = hits;
                }
            }
        }
    }

    /// Asks each account the rule covers to run it over the mail it holds, once the user agreed to any mail it would
    /// expunge; `RuleApplied` reports back per account.
    pub(super) fn apply_rule(&mut self, name: &str) {
        let Some(scope) = self
            .rules
            .file
            .rules
            .iter()
            .find(|rule| rule.name == name && rule.enabled)
            .map(|rule| rule.account.clone())
        else {
            return;
        };
        if let Some(account) = scope
            .as_deref()
            .filter(|account| !self.accounts.iter().any(|a| &a.name == account))
        {
            let message =
                format!("rule '{name}' names account '{account}', which this window does not list");
            self.note_error(None, message);
            return;
        }
        if self.expunge_check.is_some() {
            return;
        }
        let paths = self.paths.clone();
        let names: Vec<String> = self.accounts.iter().map(|a| a.name.clone()).collect();
        let rule = name.to_string();
        self.expunge_check = Some(worker::oneshot(
            "expunge-check",
            self.wake.clone(),
            move || {
                let warnings = expunge_warnings(&paths, &rule, &names).map_err(|e| e.to_string());
                (rule, warnings)
            },
        ));
    }

    pub(super) fn receive_expunge_check(&mut self) {
        let Some(answer) = &self.expunge_check else {
            return;
        };
        let Some(found) = worker::poll(answer) else {
            return;
        };
        self.expunge_check = None;
        match found {
            Ok((rule, Ok(warnings))) if warnings.is_empty() => self.send_apply_rule(&rule),
            Ok((rule, Ok(warnings))) => {
                self.apply_confirm = Some(ApplyConfirm {
                    rule,
                    warning: warnings.join("\n"),
                });
            }
            Ok((rule, Err(e))) => {
                self.note_error(None, format!("could not plan rule '{rule}': {e}"))
            }
            Err(Stopped) => self.note_error(None, "could not plan the rule".into()),
        }
    }

    pub(super) fn send_apply_rule(&mut self, name: &str) {
        for index in 0..self.accounts.len() {
            let covered = self.rules.file.rules.iter().any(|rule| {
                rule.name == name
                    && rule.enabled
                    && rule
                        .account
                        .as_deref()
                        .is_none_or(|account| account == self.accounts[index].name)
            });
            let command = Command::ApplyRule { name: name.into() };
            if covered && !self.send(index, command) {
                self.note_error(Some(index), DAEMON_LOST.into());
            }
        }
    }

    /// Trusts `id` for `name` in config.toml, which the daemon notices itself, and shows the account's new identity.
    pub(super) fn trust_authserv(&mut self, name: &str, id: &str) {
        match postvak_core::account::trust_authserv_id(&self.paths, name, id) {
            Ok(config) => {
                // Absorbs this write, so the poll does not take it for an outside edit.
                self.config_watch.changed();
                if let Some(account) = self.accounts.iter_mut().find(|a| a.name == name) {
                    account.identity = config.account(name).and_then(|a| a.identity().ok());
                }
                self.rules.check(&self.paths.rules_file(), &self.accounts);
            }
            Err(e) => self.note_error(None, format!("could not trust {id}: {e}")),
        }
    }

    /// Reloads the rules view; the daemon notices the change itself and syncs every account.
    pub(super) fn rules_changed(&mut self) {
        self.rules_watch.changed();
        self.rules = RulesState::load(
            &self.paths.rules_file(),
            std::mem::take(&mut self.rules.file),
            &self.accounts,
        );
        self.rules.set_hits(&self.rule_hits);
        self.dry_runs_stale = true;
        let on_disk = self.rule_draft_opened.as_ref().and_then(|opened| {
            let name = opened.original.as_deref()?;
            let rule = self
                .rules
                .file
                .rules
                .iter()
                .find(|rule| rule.name == name)?;
            Some(Draft::from_rule(rule))
        });
        let Some(on_disk) = on_disk else { return };
        if !self.draft_edited() {
            // A rule only looked at shows the file as it now is, so Save writes nothing back.
            let tested = self.rule_draft.as_mut().and_then(|d| d.tested.take());
            self.open_draft(Some(Draft { tested, ..on_disk }));
            return;
        }
        let Some(opened) = &mut self.rule_draft_opened else {
            return;
        };
        // The edited rule follows its switch as the file now has it, from the list or from outside, unless the editor
        // changed it; so a later Save does not turn it back. Any other change outside is not merged: Save says so first.
        let switched = Draft {
            enabled: opened.enabled,
            ..on_disk.clone()
        };
        if switched.edited(opened) {
            self.rule_draft_stale = true;
        }
        if let Some(draft) = self
            .rule_draft
            .as_mut()
            .filter(|d| d.enabled == opened.enabled)
        {
            draft.enabled = on_disk.enabled;
        }
        *opened = on_disk;
    }

    pub(super) fn open_draft(&mut self, draft: Option<Draft>) {
        self.rule_draft_stale = false;
        self.rule_draft_opened = draft.clone();
        self.rule_draft = draft;
    }

    /// Whether the open rule has changes that Save has not written.
    pub(crate) fn draft_edited(&self) -> bool {
        (self.rule_draft.as_ref())
            .zip(self.rule_draft_opened.as_ref())
            .is_some_and(|(draft, opened)| draft.edited(opened))
    }

    /// Writes the open rule; on failure it stays open with the reason.
    pub(super) fn save_draft(&mut self) -> bool {
        let path = self.paths.rules_file();
        let edited = self.draft_edited();
        let Some(draft) = &mut self.rule_draft else {
            return false;
        };
        if std::mem::take(&mut self.rule_draft_stale) && edited {
            let name = draft.original.as_deref().unwrap_or(&draft.name);
            draft.error = Some(format!(
                "'{name}' changed in rules.toml while you edited it. Save again to replace that change with this form, or discard your edits to see it."
            ));
            return false;
        }
        let saved = draft.to_rule().and_then(|rule| {
            rule_file::edit::save_rule(&path, draft.original.as_deref(), &rule)
                .map(|()| rule.name)
                .map_err(|e| e.to_string())
        });
        match saved {
            Ok(name) => {
                draft.original = Some(name);
                draft.error = None;
                self.rule_draft_opened = Some(draft.clone());
                self.rules_changed();
                true
            }
            Err(e) => {
                draft.error = Some(e);
                false
            }
        }
    }

    /// Closes an unsaved rule; a saved one asks first, then leaves rules.toml.
    pub(super) fn remove_draft(&mut self, confirmed: bool) {
        let Some(draft) = &mut self.rule_draft else {
            return;
        };
        let Some(name) = draft.original.clone() else {
            self.open_draft(None);
            return;
        };
        if !confirmed {
            draft.confirm_remove = true;
            return;
        }
        match rule_file::edit::remove_rule(&self.paths.rules_file(), &name) {
            Ok(()) => {
                self.open_draft(None);
                self.rules_changed();
            }
            Err(e) => {
                draft.confirm_remove = false;
                draft.error = Some(e.to_string());
            }
        }
    }

    pub(super) fn edit_rules(&mut self, edit: impl FnOnce(&Path) -> Result<(), RulesError>) {
        match edit(&self.paths.rules_file()) {
            Ok(()) => self.rules_changed(),
            Err(e) => self.note_error(None, e.to_string()),
        }
    }

    /// What `rule` would do to every account's cached mail if it were enabled, as `postvak rules test` shows it. Reads
    /// config.toml and the stores only. ponytail: scans the cache on the window thread; move it to a thread if large
    /// stores stall the window.
    pub(super) fn preview(&self, rule: Rule) -> String {
        let names: Vec<String> = self.accounts.iter().map(|a| a.name.clone()).collect();
        let run = rules::dry_run(
            &self.paths,
            &names,
            &self.rules.file.lists,
            &rule,
            postvak_core::time::now(),
        );
        if let Some(error) = run.error {
            return error;
        }
        std::iter::once(draft::preview_text(&rule, run.count, run.needs_body))
            .chain(run.warnings)
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Counts each proposal's dry run on a thread of its own once the proposals or the cached mail changed; one run at
    /// a time, and a change during it starts another after it.
    pub(super) fn run_dry_runs(&mut self) {
        if !self.dry_runs_stale || self.dry_running.is_some() {
            return;
        }
        self.dry_runs_stale = false;
        let file = &self.rules.file;
        if !file.rules.iter().any(rules::is_proposal) && file.list_proposals.is_empty() {
            return;
        }
        let paths = self.paths.clone();
        let file = file.clone();
        let names: Vec<String> = self.accounts.iter().map(|a| a.name.clone()).collect();
        self.dry_running = Some(worker::oneshot("dry-runs", self.wake.clone(), move || {
            let now = postvak_core::time::now();
            let runs = (file.rules.iter().filter(|rule| rules::is_proposal(rule)))
                .map(|rule| {
                    (
                        rule.name.clone(),
                        rules::dry_run(&paths, &names, &file.lists, rule, now),
                    )
                })
                .collect();
            let list_runs = (file.list_proposals.iter())
                .map(|p| {
                    (
                        p.name.clone(),
                        rules::list_dry_run(&paths, &names, &file, &p.name, now),
                    )
                })
                .collect();
            (runs, list_runs)
        }));
    }

    pub(super) fn receive_dry_runs(&mut self) {
        let Some(answer) = &self.dry_running else {
            return;
        };
        if let Some(found) = worker::poll(answer) {
            self.dry_running = None;
            if let Ok((runs, list_runs)) = found {
                self.rules.dry_runs = runs;
                self.rules.list_runs = list_runs;
            }
        }
    }

    pub(super) fn open_rules_file(&mut self) {
        let path = self.paths.rules_file();
        let opened = (|| -> std::io::Result<()> {
            if !path.exists() {
                postvak_core::paths::write_atomic(&path, b"")?;
            }
            (self.launch)(path.as_os_str(), true)
        })();
        if let Err(e) = opened {
            self.note_error(None, format!("could not open rules.toml: {e}"));
        }
    }
}

/// What rule `name` would expunge in each of the window's `accounts` it covers, planned from their stores.
fn expunge_warnings(paths: &Paths, name: &str, accounts: &[String]) -> anyhow::Result<Vec<String>> {
    let compiled = [rule_file::enabled_rule(&paths.rules_file(), name)?];
    let config = Config::load(&paths.config_file())?;
    let accounts = accounts
        .iter()
        .map(|account| {
            config
                .account(account)
                .ok_or_else(|| anyhow::anyhow!("no account named '{account}'"))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let covered = postvak_core::actions::covered_accounts(&compiled, &accounts);
    let now = postvak_core::time::now();
    Ok(postvak_core::actions::expunge_warnings(
        paths, &compiled, &covered, now,
    )?)
}
