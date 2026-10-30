//! The only code that writes rules.toml: proposals, approvals, rejections, and rules saved or removed by the window.
//! Each edit is validated as a whole file before it replaces the old one.
use std::path::Path;

use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, Value};

use crate::config::Config;
use crate::folder_name;
use crate::message::clean;
use crate::paths::{Paths, edit_toml};
use crate::rules::{
    Action, Lists, Rule, RuleFile, RulesError, compile, compile_with, invalid, parse,
};
use crate::store::Store;

/// Appends `rule` disabled and attributed to `by`, so it only acts once a human approves it.
pub fn propose(path: &Path, rule: Rule, by: &str) -> Result<(), RulesError> {
    propose_all(
        path,
        Proposals {
            rules: vec![rule],
            ..Proposals::default()
        },
        by,
    )
}

/// Where proposals go in rules.toml: after the last rule, or right before or after a rule already there.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Place {
    #[default]
    End,
    Before(String),
    After(String),
}

/// One or more rules an agent proposes at once, and where they go.
#[derive(Debug, Clone, Default)]
pub struct Proposals {
    pub rules: Vec<Rule>,
    pub place: Place,
    /// A rule whose name is taken by the caller's own pending proposal replaces it, instead of being refused.
    pub replace: bool,
    /// Each rule changes the existing rule of its name once approved, which runs as it is until then.
    pub edit: bool,
}

/// What `rules propose` reads on stdin and `rules_propose` takes: one rule, an array of rules, or `{"rules": [...]}`.
/// Either object may also carry `before` or `after` (a rule's name), `replace` and `edit`. A rule that does not parse is named
/// by its index in the batch.
pub fn read_proposals(value: serde_json::Value) -> Result<Proposals, RulesError> {
    use serde_json::Value as Json;
    let (items, mut object) = match value {
        Json::Array(items) => (items, serde_json::Map::new()),
        Json::Object(mut object) => match object.remove("rules") {
            Some(Json::Array(items)) => (items, object),
            Some(Json::Null) | None => {
                let mut rule = serde_json::Map::new();
                for key in ["before", "after", "edit", "replace"] {
                    if let Some(value) = object.remove(key) {
                        rule.insert(key.to_string(), value);
                    }
                }
                (vec![Json::Object(object)], rule)
            }
            Some(_) => {
                return Err(RulesError::Parse(
                    "`rules` must be an array of rules".into(),
                ));
            }
        },
        _ => {
            return Err(RulesError::Parse(
                "expected a rule, an array of rules or {\"rules\": [...]}".into(),
            ));
        }
    };
    let text = |key: &str, object: &mut serde_json::Map<String, Json>| match object.remove(key) {
        None | Some(Json::Null) => Ok(None),
        Some(Json::String(name)) => Ok(Some(name)),
        Some(_) => Err(RulesError::Parse(format!("`{key}` must be a rule's name"))),
    };
    let place = match (text("before", &mut object)?, text("after", &mut object)?) {
        (Some(_), Some(_)) => {
            return Err(RulesError::Parse(
                "give `before` or `after`, not both".into(),
            ));
        }
        (Some(name), None) => Place::Before(name),
        (None, Some(name)) => Place::After(name),
        (None, None) => Place::End,
    };
    let flag = |key: &str, object: &mut serde_json::Map<String, Json>| match object.remove(key) {
        None | Some(Json::Null) => Ok(false),
        Some(Json::Bool(value)) => Ok(value),
        Some(_) => Err(RulesError::Parse(format!("`{key}` must be true or false"))),
    };
    let replace = flag("replace", &mut object)?;
    let edit = flag("edit", &mut object)?;
    if edit && place != Place::End {
        return Err(RulesError::Parse(
            "an edit keeps its rule's place: give `edit` without `before` or `after`".into(),
        ));
    }
    if let Some(key) = object.keys().next() {
        return Err(RulesError::Parse(format!("unknown field `{key}`")));
    }
    let single = items.len() == 1;
    let mut rules = Vec::new();
    let mut errors = Vec::new();
    for (index, item) in items.into_iter().enumerate() {
        let name = item
            .get("name")
            .and_then(Json::as_str)
            .unwrap_or("")
            .to_string();
        match serde_json::from_value::<Rule>(item) {
            Ok(rule) => rules.push(rule),
            Err(e) if single => return Err(RulesError::Parse(e.to_string())),
            Err(e) => errors.push(format!("rule {index} '{name}': {e}")),
        }
    }
    if !errors.is_empty() {
        return Err(RulesError::Batch(errors.join("; ")));
    }
    Ok(Proposals {
        rules,
        place,
        replace,
        edit,
    })
}

/// Stores every rule of `proposals` disabled and attributed to `by`, in the order given, or none of them: the whole
/// batch is checked first, and each rule that fails is named by its index. A name already in the file is refused,
/// unless `replace` is set and that rule is `by`'s own pending proposal; a replaced rule keeps its place unless the
/// batch names one. With `edit`, each rule is stored as `<name> (edit)` right after the rule it changes.
pub fn propose_all(path: &Path, proposals: Proposals, by: &str) -> Result<(), RulesError> {
    let Proposals {
        mut rules,
        place,
        replace,
        edit,
    } = proposals;
    if rules.is_empty() {
        return Err(RulesError::Batch("no rules to propose".into()));
    }
    for rule in &mut rules {
        rule.enabled = false;
        rule.proposed_by = Some(by.to_string());
        rule.replaces = None;
        if edit {
            let stored = edit_name(&rule.name);
            rule.replaces = Some(std::mem::replace(&mut rule.name, stored));
        }
    }
    let tables = rules
        .iter()
        .map(rule_table)
        .collect::<Result<Vec<_>, _>>()?;
    edit_file(path, |doc| {
        let RuleFile {
            rules: existing,
            lists,
        } = parse(&doc.to_string())?;
        if edit {
            check_edits(&existing, &rules)?;
        }
        check_batch(&existing, &lists, &rules, by, replace, &place)?;
        if !edit {
            return place_tables(doc, tables, &place);
        }
        for (rule, table) in rules.iter().zip(tables) {
            // A pending edit being replaced stays where it is; a new one goes right after its rule.
            let place = match &rule.replaces {
                Some(target) if !existing.iter().any(|r| r.name == rule.name) => {
                    Place::After(target.clone())
                }
                _ => Place::End,
            };
            place_tables(doc, vec![table], &place)?;
        }
        Ok(())
    })
}

/// The name an edit of `name` is stored under until a human approves it.
pub fn edit_name(name: &str) -> String {
    format!("{name} (edit)")
}

/// Every edit of `batch` must change a rule of `existing` that is not itself a pending proposal.
fn check_edits(existing: &[Rule], batch: &[Rule]) -> Result<(), RulesError> {
    let mut errors = Vec::new();
    for (index, rule) in batch.iter().enumerate() {
        let target = rule.replaces.as_deref().unwrap_or_default();
        let reason = match existing.iter().find(|r| r.name == target) {
            None => "no rule to edit; propose it as a new rule",
            Some(r) if is_pending(r) => "is a pending proposal; propose it again with replace",
            Some(_) => continue,
        };
        if batch.len() == 1 {
            return Err(invalid(target, reason));
        }
        errors.push(format!("rule {index} '{target}': {reason}"));
    }
    if errors.is_empty() {
        return Ok(());
    }
    Err(RulesError::Batch(errors.join("; ")))
}

/// A rule an agent proposed that no human has approved yet.
fn is_pending(rule: &Rule) -> bool {
    !rule.enabled && rule.proposed_by.is_some()
}

/// Every reason the batch cannot be stored as a whole, each rule's named by its index; a lone rule's error stays as
/// `rules check` words it.
fn check_batch(
    existing: &[Rule],
    lists: &Lists,
    batch: &[Rule],
    by: &str,
    replace: bool,
    place: &Place,
) -> Result<(), RulesError> {
    let mut errors: Vec<(usize, RulesError)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (index, rule) in batch.iter().enumerate() {
        if let Err(e) = compile_with(std::slice::from_ref(rule), lists) {
            errors.push((index, e));
            continue;
        }
        if !seen.insert(rule.name.as_str()) {
            errors.push((index, invalid(&rule.name, "named twice in this batch")));
            continue;
        }
        match existing.iter().find(|r| r.name == rule.name) {
            None => {}
            Some(_) if !replace => errors.push((
                index,
                invalid(
                    &rule.name,
                    "duplicate rule name; to change your own pending proposal, propose it again with replace",
                ),
            )),
            Some(old) if !is_own_proposal(old, by) => errors.push((
                index,
                invalid(&rule.name, "is not your pending proposal, so it cannot be replaced"),
            )),
            Some(_) => {}
        }
    }
    if let Place::Before(anchor) | Place::After(anchor) = place
        && (!existing.iter().any(|r| &r.name == anchor) || seen.contains(anchor.as_str()))
    {
        errors.push((
            usize::MAX,
            RulesError::Parse(format!(
                "no rule named '{anchor}' outside this batch to place the proposals next to"
            )),
        ));
    }
    if batch.len() == 1 && errors.len() == 1 {
        return Err(errors.remove(0).1);
    }
    if errors.is_empty() {
        return Ok(());
    }
    let words: Vec<String> = errors
        .into_iter()
        .map(|(index, e)| match e {
            _ if index == usize::MAX => e.to_string(),
            RulesError::Invalid { rule, reason } => format!("rule {index} '{rule}': {reason}"),
            other => format!("rule {index} '{}': {other}", batch[index].name),
        })
        .collect();
    Err(RulesError::Batch(words.join("; ")))
}

/// A rule `by` proposed that no human has approved yet: the only kind an agent may replace or withdraw.
fn is_own_proposal(rule: &Rule, by: &str) -> bool {
    !rule.enabled && rule.proposed_by.as_deref() == Some(by)
}

/// Puts `tables` into the document. A table whose name is already there replaces that rule: in place when `place` is
/// the end, moved with the others otherwise. The rest go together, in order, at `place`.
fn place_tables(
    doc: &mut DocumentMut,
    tables: Vec<Table>,
    place: &Place,
) -> Result<(), RulesError> {
    let name_of = |table: &Table| {
        table
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    let mut rest = Vec::new();
    for table in tables {
        let name = name_of(&table);
        let found = doc
            .get_mut("rules")
            .and_then(|item| item.as_array_of_tables_mut())
            .and_then(|rules| {
                let index = rules.iter().position(|t| name_of(t) == name)?;
                Some((rules, index))
            });
        let orphaned = match found {
            Some((rules, index)) if *place == Place::End => {
                if let Some(old) = rules.get_mut(index) {
                    merge(old, &table);
                }
                None
            }
            Some((rules, index)) => {
                rest.push(table);
                remove_at(rules, index)
            }
            None => {
                rest.push(table);
                None
            }
        };
        if let Some(orphaned) = orphaned {
            let trailing = format!("{orphaned}{}", doc.trailing().as_str().unwrap_or(""));
            doc.set_trailing(trailing);
        }
    }
    let anchor = match place {
        Place::End => {
            for table in rest {
                let after_text = !doc.to_string().trim().is_empty();
                append(doc, table, after_text)?;
            }
            return Ok(());
        }
        Place::Before(anchor) | Place::After(anchor) => anchor,
    };
    let rules = doc
        .get_mut("rules")
        .and_then(|item| item.as_array_of_tables_mut())
        .ok_or_else(|| RulesError::NotFound(anchor.clone()))?;
    let found = rules
        .iter()
        .position(|t| name_of(t) == *anchor)
        .ok_or_else(|| RulesError::NotFound(anchor.clone()))?;
    let at = match place {
        Place::After(_) => found + 1,
        _ => found,
    };
    // Before the first rule, the comments that head the file (up to its last blank line) stay at the top; the ones
    // right above that rule stay with it.
    let mut head = String::new();
    if at == 0
        && let Some(first) = rules.get_mut(0)
    {
        let prefix = first
            .decor()
            .prefix()
            .and_then(|p| p.as_str())
            .unwrap_or("")
            .to_string();
        if let Some(split) = prefix.rfind("\n\n") {
            head = prefix[..split + 2].to_string();
            first
                .decor_mut()
                .set_prefix(prefix[split + 2..].to_string());
        }
    }
    let count = rest.len();
    for (offset, mut table) in rest.into_iter().enumerate() {
        let gap = if offset == 0 && at == 0 {
            std::mem::take(&mut head)
        } else {
            "\n".to_string()
        };
        let own = table
            .decor()
            .prefix()
            .and_then(|p| p.as_str())
            .unwrap_or("");
        let prefix = format!("{gap}{own}");
        table.decor_mut().set_prefix(prefix);
        rules.insert(at + offset, table);
    }
    // The rule that now follows the batch starts a blank line below it, its comments still above it.
    if let Some(next) = rules.get_mut(at + count) {
        let prefix = next.decor().prefix().and_then(|p| p.as_str()).unwrap_or("");
        if !prefix.starts_with('\n') {
            let prefix = format!("\n{prefix}");
            next.decor_mut().set_prefix(prefix);
        }
    }
    Ok(())
}

/// Moves the rule named `name` to `place`, which changes when it runs. The comments right above it go with it; those a
/// blank line above them, such as the file's heading, stay where they are. A pending edit of the rule, which sits right
/// after it, moves with it, and a rule placed after another lands after that one's pending edit.
pub fn move_rule(path: &Path, name: &str, place: &Place) -> Result<(), RulesError> {
    edit_file(path, |doc| {
        let edit = pending_edit(doc, name);
        let place = match place {
            Place::After(anchor) => {
                Place::After(pending_edit(doc, anchor).unwrap_or(anchor.clone()))
            }
            other => other.clone(),
        };
        if let Place::Before(anchor) | Place::After(anchor) = &place
            && (anchor == name || Some(anchor) == edit.as_ref())
        {
            return Err(invalid(name, "cannot be placed next to itself"));
        }
        let mut tables = vec![take_rule(doc, name)?];
        if let Some(edit) = edit {
            tables.push(take_rule(doc, &edit)?);
        }
        place_tables(doc, tables, &place)
    })
}

/// The name of the pending edit of the rule named `name`, if there is one.
fn pending_edit(doc: &DocumentMut, name: &str) -> Option<String> {
    let rules = doc.get("rules")?.as_array_of_tables()?;
    let edit = rules
        .iter()
        .find(|t| t.get("replaces").and_then(|v| v.as_str()) == Some(name))?;
    Some(edit.get("name")?.as_str()?.to_string())
}

/// Takes the rule named `name` out of the document, with the comments right above it; those a blank line above them
/// stay for the next rule.
fn take_rule(doc: &mut DocumentMut, name: &str) -> Result<Table, RulesError> {
    let rules = doc
        .get_mut("rules")
        .and_then(|item| item.as_array_of_tables_mut())
        .ok_or_else(|| invalid(name, "no such rule"))?;
    let index = rules
        .iter()
        .position(|t| t.get("name").and_then(|v| v.as_str()) == Some(name))
        .ok_or_else(|| invalid(name, "no such rule"))?;
    let mut table = rules
        .get(index)
        .cloned()
        .ok_or_else(|| invalid(name, "no such rule"))?;
    let prefix = table
        .decor()
        .prefix()
        .and_then(|p| p.as_str())
        .unwrap_or("")
        .to_string();
    let own_start = prefix.rfind("\n\n").map_or(0, |split| split + 2);
    let stays = prefix[..own_start].to_string();
    let stays = if stays.trim().is_empty() {
        String::new()
    } else {
        stays
    };
    // Kept, the table's place in the file as read would win over where it is put.
    table.set_position(None);
    table
        .decor_mut()
        .set_prefix(prefix[own_start..].trim_start_matches('\n').to_string());
    if let Some(old) = rules.get_mut(index) {
        old.decor_mut().set_prefix(stays);
    }
    if let Some(orphaned) = remove_at(rules, index) {
        let trailing = format!("{orphaned}{}", doc.trailing().as_str().unwrap_or(""));
        doc.set_trailing(trailing);
    }
    Ok(table)
}

/// Removes `name` when it is `by`'s own pending proposal: disabled, and proposed by `by`. Anything else, an approved
/// rule or another proposer's, is left for a human.
pub fn withdraw(path: &Path, name: &str, by: &str) -> Result<(), RulesError> {
    edit(path, name, |rules, index| {
        let table = rules
            .get(index)
            .ok_or_else(|| invalid(name, "no such rule"))?;
        let own =
            is_disabled(table) && table.get("proposed_by").and_then(|v| v.as_str()) == Some(by);
        if !own {
            return Err(invalid(
                name,
                format!("is not a pending proposal by {by}; only a human can remove it"),
            ));
        }
        Ok(remove_at(rules, index))
    })
}

/// Turns a rule on or off, keeping the file's comments and layout.
pub fn set_enabled(path: &Path, name: &str, enabled: bool) -> Result<(), RulesError> {
    edit(path, name, |rules, index| {
        let table = rules
            .get_mut(index)
            .ok_or_else(|| invalid(name, "no such rule"))?;
        write_enabled(table, enabled);
        Ok(None)
    })
}

/// Enables `name`, or writes it over the rule it edits; returns the name of the rule that now holds it.
fn approve(path: &Path, name: &str) -> Result<String, RulesError> {
    let mut approved = name.to_string();
    edit(path, name, |rules, index| {
        let table = rules
            .get_mut(index)
            .ok_or_else(|| invalid(name, "no such rule"))?;
        if !is_disabled(table) {
            return Err(invalid(name, "is already enabled"));
        }
        if let Some(target) = table.get("replaces").and_then(|v| v.as_str()) {
            approved = target.to_string();
            return approve_edit(rules, index, name);
        }
        write_enabled(table, true);
        // Approved, the rule is no longer anyone's proposal: turned off later, it must not read as pending again, which
        // would let its proposer replace or withdraw it.
        table.remove("proposed_by");
        Ok(None)
    })?;
    Ok(approved)
}

/// Merges the edit at `index` into the rule it replaces, which keeps its name, comments, place and `enabled`, then
/// removes the edit.
fn approve_edit(
    rules: &mut ArrayOfTables,
    index: usize,
    name: &str,
) -> Result<Option<String>, RulesError> {
    let mut new = rules
        .get(index)
        .ok_or_else(|| invalid(name, "no such rule"))?
        .clone();
    let target = new
        .remove("replaces")
        .and_then(|item| item.as_str().map(str::to_string))
        .unwrap_or_default();
    let found = rules
        .iter()
        .position(|t| t.get("name").and_then(|v| v.as_str()) == Some(target.as_str()))
        .ok_or_else(|| {
            invalid(
                name,
                format!("the rule it edits, '{target}', is no longer in rules.toml; reject it"),
            )
        })?;
    let old = rules
        .get_mut(found)
        .ok_or_else(|| invalid(name, "no such rule"))?;
    new.remove("proposed_by");
    new.remove("enabled");
    for key in ["name", "enabled"] {
        if let Some(item) = old.get(key) {
            new.insert(key, item.clone());
        }
    }
    merge(old, &new);
    Ok(remove_at(rules, index))
}

/// Approves `name`, or writes the edit it is over its rule, and restarts that rule's clock in the store of every account
/// in config.toml, read here rather than taken from the caller, so it acts only on mail that arrives from now on, even
/// where an older clock of the rule survived and whichever front end added the account. An unreadable config.toml
/// refuses before the rule is enabled. Returns the approved rule's name.
pub fn approve_from_now(paths: &Paths, name: &str, now: i64) -> Result<String, RulesError> {
    let config = Config::load(&paths.config_file())?;
    let approved = approve(&paths.rules_file(), name)?;
    for account in &config.accounts {
        Store::open_account(paths, &account.name)?.restart_rule_clock(&approved, now)?;
    }
    Ok(approved)
}

fn write_enabled(table: &mut toml_edit::Table, enabled: bool) {
    let mut value = Value::from(enabled);
    if let Some(old) = table.get("enabled").and_then(|item| item.as_value()) {
        *value.decor_mut() = old.decor().clone();
    }
    table["enabled"] = Item::Value(value);
}

pub fn reject(path: &Path, name: &str) -> Result<(), RulesError> {
    edit(path, name, |rules, index| {
        let table = rules
            .get(index)
            .ok_or_else(|| invalid(name, "no such rule"))?;
        if !is_disabled(table) || !table.contains_key("proposed_by") {
            return Err(invalid(
                name,
                "is not a pending proposal; edit rules.toml to remove it",
            ));
        }
        Ok(remove_at(rules, index))
    })
}

/// Adds `rule`, or replaces the rule named `replacing` with it in place. Keeps the file's comments, the other rules as
/// written, and each unchanged line of the replaced rule.
pub fn save_rule(path: &Path, replacing: Option<&str>, rule: &Rule) -> Result<(), RulesError> {
    let new = rule_table(rule)?;
    let Some(name) = replacing else {
        return edit_file(path, |doc| {
            let after_text = !doc.to_string().trim().is_empty();
            append(doc, new, after_text)
        });
    };
    edit(path, name, |rules, index| {
        let old = rules
            .get_mut(index)
            .ok_or_else(|| invalid(name, "no such rule"))?;
        merge(old, &new);
        Ok(None)
    })
}

/// Removes the rule named `name`, keeping the comments above it.
pub fn remove_rule(path: &Path, name: &str) -> Result<(), RulesError> {
    edit(path, name, |rules, index| Ok(remove_at(rules, index)))
}

/// Removes the table at `index`. Comments above a table belong to its decor, so they move to the next table, or are
/// returned for the document's end when it was the last; the blank line `propose` adds is dropped.
fn remove_at(rules: &mut ArrayOfTables, index: usize) -> Option<String> {
    let prefix = rules
        .get(index)
        .and_then(|table| table.decor().prefix())
        .and_then(|p| p.as_str())
        .unwrap_or("");
    let mut orphaned = prefix
        .strip_suffix('\n')
        .filter(|p| p.ends_with('\n'))
        .unwrap_or(prefix)
        .to_string();
    rules.remove(index);
    match rules.get_mut(index) {
        Some(next) => {
            let next_prefix = next.decor().prefix().and_then(|p| p.as_str()).unwrap_or("");
            orphaned.push_str(next_prefix);
            next.decor_mut().set_prefix(orphaned);
            None
        }
        None => Some(orphaned),
    }
}

/// A folder renamed from `from` to `to` in `account`; `delimiter` is the account's, so children (`from/x`) follow.
pub struct FolderRename<'a> {
    pub account: &'a str,
    pub accounts: &'a [String],
    pub from: &'a str,
    pub to: &'a str,
    pub delimiter: Option<&'a str>,
}

impl FolderRename<'_> {
    fn renamed(&self, folder: &str) -> Option<String> {
        if folder_name::same(folder, self.from) {
            return Some(self.to.to_string());
        }
        let delimiter = self.delimiter?;
        let (folder, from) = (folder_name::nfc(folder), folder_name::nfc(self.from));
        let rest = folder
            .strip_prefix(from.as_ref())?
            .strip_prefix(delimiter)?;
        Some(format!("{}{delimiter}{rest}", self.to))
    }

    fn in_scope(&self, rule: &Rule) -> bool {
        rule.account.as_deref().is_none_or(|a| a == self.account)
    }

    /// `rule` with its folders renamed, or `None` when it is another account's or names none of them.
    fn rewrite(&self, rule: &Rule) -> Option<Rule> {
        if !self.in_scope(rule) {
            return None;
        }
        let mut changed = rule.clone();
        let mut any = false;
        if let Some(to) = rule.folder.as_deref().and_then(|f| self.renamed(f)) {
            changed.folder = Some(to);
            any = true;
        }
        for action in &mut changed.actions {
            if let Action::Move(folder) = action
                && let Some(to) = self.renamed(folder)
            {
                *folder = to;
                any = true;
            }
        }
        any.then_some(changed)
    }

    fn splits(&self, rule: &Rule) -> bool {
        rule.account.is_none() && self.accounts.len() > 1
    }
}

/// The rules that name the renamed folder or a child, as `(name, will_split)`: a rule for every account is split into
/// one copy per account, so only the renamed account's follows.
pub fn rules_naming(file: &RuleFile, rename: &FolderRename) -> Vec<(String, bool)> {
    file.rules
        .iter()
        .filter(|rule| rename.rewrite(rule).is_some())
        .map(|rule| (rule.name.clone(), rename.splits(rule)))
        .collect()
}

/// Rewrites `folder` and `move` of the rules that name the renamed folder or a child, returning the names of the rules
/// changed or split. A rule for every account, with several accounts, becomes one copy per account in its place.
pub fn follow_rename(path: &Path, rename: &FolderRename) -> Result<Vec<String>, RulesError> {
    let mut touched = Vec::new();
    edit_file(path, |doc| {
        let file = parse(&doc.to_string())?;
        let Some(tables) = doc.get_mut("rules").and_then(Item::as_array_of_tables_mut) else {
            return Ok(());
        };
        let mut taken: Vec<String> = file.rules.iter().map(|r| r.name.clone()).collect();
        let split: Vec<&str> = file
            .rules
            .iter()
            .filter(|rule| rename.splits(rule) && rename.rewrite(rule).is_some())
            .map(|rule| rule.name.as_str())
            .collect();
        let mut rebuilt = Vec::new();
        for (rule, table) in file.rules.iter().zip(tables.iter()) {
            if rule.replaces.as_deref().is_some_and(|r| split.contains(&r)) {
                continue;
            }
            let edit = file
                .rules
                .iter()
                .zip(tables.iter())
                .find(|(edit, _)| edit.replaces.as_deref() == Some(rule.name.as_str()));
            let Some(renamed) = rename.rewrite(rule) else {
                rebuilt.push(table.clone());
                continue;
            };
            touched.push(rule.name.clone());
            if !rename.splits(rule) {
                rebuilt.push(rewritten(table, &renamed)?);
                continue;
            }
            let others = rename.accounts.iter().filter(|a| *a != rename.account);
            let copies = others
                .map(|account| (account.clone(), rule.clone()))
                .chain([(rename.account.to_string(), renamed)]);
            for (position, (account, mut copy)) in copies.enumerate() {
                copy.name = free_name(&taken, &format!("{} ({account})", rule.name));
                taken.push(copy.name.clone());
                copy.account = Some(account);
                let mut copied = rewritten(table, &copy)?;
                if position > 0 {
                    clear_positions(&mut copied);
                    copied.decor_mut().set_prefix("\n");
                }
                rebuilt.push(copied);
                let Some((edit, edit_table)) = edit.filter(|_| split.contains(&rule.name.as_str()))
                else {
                    continue;
                };
                let mut edit_copy = edit.clone();
                edit_copy.account = copy.account.clone();
                edit_copy = rename.rewrite(&edit_copy).unwrap_or(edit_copy);
                edit_copy.name = free_name(&taken, &edit_name(&copy.name));
                taken.push(edit_copy.name.clone());
                edit_copy.replaces = Some(copy.name.clone());
                let mut copied = rewritten(edit_table, &edit_copy)?;
                clear_positions(&mut copied);
                copied.decor_mut().set_prefix("\n");
                rebuilt.push(copied);
            }
        }
        tables.clear();
        for table in rebuilt {
            tables.push(table);
        }
        Ok(())
    })?;
    Ok(touched)
}

/// [`follow_rename`] with the rules it follows, as [`rules_naming`] reads them first, so a failed rewrite can name the
/// rules that still name `from` (`help::folder_rules_not_updated`). A file in which no rule names the folder is not
/// written.
pub fn rules_after_rename(
    path: &Path,
    rename: &FolderRename,
) -> Result<Vec<(String, bool)>, String> {
    let naming = crate::rules::load(path)
        .map(|file| rules_naming(&file, rename))
        .map_err(|e| e.to_string());
    if naming.as_ref().is_ok_and(Vec::is_empty) {
        return Ok(Vec::new());
    }
    match follow_rename(path, rename) {
        Ok(_) => Ok(naming.unwrap_or_default()),
        Err(e) => {
            let names = naming.map(|pairs| {
                let names = pairs.into_iter().map(|(name, _)| clean(&name, false));
                names.collect::<Vec<_>>()
            });
            Err(crate::help::folder_rules_not_updated(
                &clean(rename.from, false),
                &e.to_string(),
                names.as_deref().map_err(String::as_str),
            ))
        }
    }
}

/// Drops the file positions of `table` and its subtables, so a copy of a header-style rule prints where it is put.
fn clear_positions(table: &mut Table) {
    table.set_position(None);
    for (_, item) in table.iter_mut() {
        match item {
            Item::Table(nested) => clear_positions(nested),
            Item::ArrayOfTables(array) => array.iter_mut().for_each(clear_positions),
            _ => {}
        }
    }
}

/// A copy of `table` holding `rule`, with the unchanged lines and comments as written.
fn rewritten(table: &Table, rule: &Rule) -> Result<Table, RulesError> {
    let mut copy = table.clone();
    merge(&mut copy, &rule_table(rule)?);
    Ok(copy)
}

fn free_name(taken: &[String], wanted: &str) -> String {
    let mut name = wanted.to_string();
    let mut number = 2;
    while taken.contains(&name) {
        name = format!("{wanted} {number}");
        number += 1;
    }
    name
}

/// `rule` as rules.toml holds it, in the docs' style: what a proposal adds to the file.
pub fn rule_toml(rule: &Rule) -> Result<String, RulesError> {
    let mut rules = ArrayOfTables::new();
    rules.push(rule_table(rule)?);
    let mut doc = DocumentMut::new();
    doc.insert("rules", Item::ArrayOfTables(rules));
    Ok(doc.to_string())
}

/// `rule` as a `[[rules]]` table in the docs' style: one block, nested tables as dotted keys with inline values, so
/// `match.header.List-Id = { … }` and `actions.move = "…"`, `actions` last, and no `enabled = true`.
fn rule_table(rule: &Rule) -> Result<Table, RulesError> {
    let text = toml::to_string(&RuleFile {
        rules: vec![rule.clone()],
        ..RuleFile::default()
    })
    .map_err(|e| RulesError::Parse(e.to_string()))?;
    let mut table = parse_doc(&text)?
        .remove("rules")
        .and_then(|item| item.into_array_of_tables().ok())
        .filter(|rules| !rules.is_empty())
        .map(|mut rules| rules.remove(0))
        .ok_or_else(|| invalid(&rule.name, "could not be written as TOML"))?;
    // The position is the table's place in the one-rule document above; kept, toml_edit would write the table at
    // that place in rules.toml rather than where it is added.
    table.set_position(None);
    for (_, item) in table.iter_mut() {
        match item {
            Item::Table(nested) => {
                nested.set_dotted(true);
                for (key, child) in nested.iter_mut() {
                    match child {
                        Item::Table(headers) if key.get() == "header" => {
                            headers.set_dotted(true);
                            for (_, header) in headers.iter_mut() {
                                inline(header);
                            }
                            headers.fmt();
                        }
                        other => inline(other),
                    }
                }
                nested.fmt();
            }
            other => inline(other),
        }
    }
    if let Some(actions) = table.remove("actions") {
        table.insert("actions", actions);
    }
    if rule.enabled {
        table.remove("enabled");
    }
    Ok(table)
}

/// `item` written on one line: `toml` puts a table, or an array of tables such as `[{ move = "x" }]`, under headers.
fn inline(item: &mut Item) {
    *item = match std::mem::take(item).into_value() {
        Ok(mut value) => {
            match &mut value {
                Value::Array(array) => array.fmt(),
                Value::InlineTable(table) => {
                    table.set_dotted(false);
                    table.fmt();
                }
                _ => {}
            }
            Item::Value(value)
        }
        Err(other) => other,
    };
}

/// Adds `table` after the last rule and after any comments that end the file, a blank line apart from what is above.
fn append(doc: &mut DocumentMut, mut table: Table, after_text: bool) -> Result<(), RulesError> {
    let trailing = doc.trailing().as_str().unwrap_or("").to_string();
    doc.set_trailing("");
    let gap = if after_text && !trailing.ends_with("\n\n") {
        "\n"
    } else {
        ""
    };
    let own = table
        .decor()
        .prefix()
        .and_then(|p| p.as_str())
        .unwrap_or("");
    let prefix = format!("{trailing}{gap}{own}");
    table.decor_mut().set_prefix(prefix);
    match doc.get_mut("rules") {
        None => {
            let mut rules = ArrayOfTables::new();
            rules.push(table);
            doc.insert("rules", Item::ArrayOfTables(rules));
        }
        Some(item) => item
            .as_array_of_tables_mut()
            .ok_or_else(|| {
                RulesError::Parse("`rules` is an inline array; add the rule there by hand".into())
            })?
            .push(table),
    }
    Ok(())
}

/// Makes `old` hold `new`'s keys and values: keys `new` lacks go, keys it adds come last, an unchanged value stays as
/// written, and a changed value keeps the old one's comment.
fn merge(old: &mut Table, new: &Table) {
    old.retain(|key, _| new.contains_key(key));
    for (key, item) in new.iter() {
        match (old.get_mut(key), item) {
            (Some(Item::Table(old_table)), Item::Table(new_table)) => merge(old_table, new_table),
            (Some(old_item), new_item) if same_value(old_item, new_item) => {}
            (Some(old_item), new_item) => {
                let decor = old_item.as_value().map(|value| value.decor().clone());
                *old_item = new_item.clone();
                if decor.is_some() {
                    // Written inline before, so it stays inline: `actions = { … }` does not turn into dotted keys.
                    inline(old_item);
                }
                if let (Some(decor), Some(value)) = (decor, old_item.as_value_mut()) {
                    *value.decor_mut() = decor;
                }
            }
            (None, new_item) => {
                old.insert(key, new_item.clone());
            }
        }
    }
}

/// Whether two items hold the same value however each is written: a header table and an inline table compare equal.
fn same_value(a: &Item, b: &Item) -> bool {
    let plain = |item: &Item| {
        let mut value = item.clone().into_value().ok()?;
        match &mut value {
            Value::InlineTable(table) => {
                table.set_dotted(false);
                table.fmt();
            }
            Value::Array(array) => array.fmt(),
            _ => {}
        }
        value.decor_mut().clear();
        Some(value.to_string())
    };
    plain(a).is_some_and(|text| Some(text) == plain(b))
}

fn is_disabled(table: &toml_edit::Table) -> bool {
    table.get("enabled").and_then(|v| v.as_bool()) == Some(false)
}

fn edit(
    path: &Path,
    name: &str,
    change: impl FnOnce(&mut ArrayOfTables, usize) -> Result<Option<String>, RulesError>,
) -> Result<(), RulesError> {
    edit_file(path, |doc| {
        let rules = doc
            .get_mut("rules")
            .and_then(|item| item.as_array_of_tables_mut())
            .ok_or_else(|| invalid(name, "no such rule"))?;
        let index = rules
            .iter()
            .position(|t| t.get("name").and_then(|v| v.as_str()) == Some(name))
            .ok_or_else(|| invalid(name, "no such rule"))?;
        if let Some(orphaned) = change(rules, index)? {
            let trailing = format!("{orphaned}{}", doc.trailing().as_str().unwrap_or(""));
            doc.set_trailing(trailing);
        }
        Ok(())
    })
}

/// Edits rules.toml under `.rules.lock`, so concurrent edits cannot lose each other's; the edited file must validate
/// as a whole, or nothing is written.
fn edit_file(
    path: &Path,
    change: impl FnOnce(&mut DocumentMut) -> Result<(), RulesError>,
) -> Result<(), RulesError> {
    edit_toml(path, ".rules.lock", |doc| {
        change(doc)?;
        compile(&parse(&doc.to_string())?)?;
        Ok(())
    })
}

fn parse_doc(text: &str) -> Result<DocumentMut, RulesError> {
    text.parse()
        .map_err(|e: toml_edit::TomlError| RulesError::Parse(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::{Action, TextMatch, load};

    const HUMAN: &str = "# my rules\n[[rules]]\nname = \"keep\" # do not touch\nmatch.read = true\nactions = { flagged = true }\n";

    fn proposal(name: &str) -> Rule {
        serde_json::from_str(&format!(
            r#"{{"name": "{name}", "match": {{"subject": {{"contains": "code"}}}}, "actions": {{"move": "Codes", "read": true}}}}"#
        ))
        .unwrap()
    }

    fn rules_file(text: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rules.toml");
        std::fs::write(&path, text).unwrap();
        (dir, path)
    }

    #[test]
    fn propose_appends_disabled_and_keeps_the_rest_of_the_file() {
        let (_dir, path) = rules_file(HUMAN);
        propose(&path, proposal("codes"), "cli:test").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with(HUMAN), "{text}");
        let added = &parse(&text).unwrap().rules[1];
        assert_eq!(
            (added.enabled, added.proposed_by.as_deref()),
            (false, Some("cli:test"))
        );
        assert_eq!(
            added.actions,
            [Action::Move("Codes".into()), Action::MarkRead]
        );
    }

    #[test]
    fn a_proposal_may_use_the_files_lists_but_not_an_unknown_one() {
        let lists = "[lists]\nkids = [\"school.example\"]\n\n";
        let (_dir, path) = rules_file(&format!("{lists}{HUMAN}"));
        let mut rule = proposal("kids");
        rule.matches = serde_json::from_str(r#"{"from_domain": {"list": "kids"}}"#).unwrap();
        propose(&path, rule.clone(), "cli").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with(lists), "{text}");
        assert_eq!(parse(&text).unwrap().rules[1].matches, rule.matches);

        rule.name = "typo".into();
        rule.matches = serde_json::from_str(r#"{"from_domain": {"list": "kid"}}"#).unwrap();
        let error = propose(&path, rule, "cli").unwrap_err().to_string();
        assert!(error.contains("no list named 'kid'"), "{error}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    }

    #[test]
    fn propose_into_a_missing_file_creates_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rules.toml");
        propose(&path, proposal("codes"), "cli").unwrap();
        assert_eq!(load(&path).unwrap().rules.len(), 1);
    }

    #[test]
    fn invalid_proposal_leaves_the_file_untouched() {
        let (_dir, path) = rules_file(HUMAN);
        assert!(
            propose(&path, proposal("keep"), "cli").is_err(),
            "duplicate name"
        );
        let mut bad = proposal("bad");
        bad.matches.subject = Some(TextMatch {
            regex: Some("(".into()),
            ..Default::default()
        });
        assert!(propose(&path, bad, "cli").is_err(), "invalid regex");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), HUMAN);
    }

    #[test]
    fn approve_enables_in_place_and_keeps_comments() {
        let (_dir, path) = rules_file(HUMAN);
        propose(&path, proposal("codes"), "cli").unwrap();
        approve(&path, "codes").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("# my rules") && text.contains("# do not touch"),
            "{text}"
        );
        assert!(parse(&text).unwrap().rules.iter().all(|r| r.enabled));
        assert!(approve(&path, "codes").is_err(), "already enabled");
        assert!(approve(&path, "missing").is_err());
    }

    #[test]
    fn an_approved_rule_turned_off_is_not_its_proposers_to_replace_or_withdraw() {
        let (_dir, path) = rules_file(HUMAN);
        propose(&path, proposal("codes"), "mcp:host").unwrap();
        approve(&path, "codes").unwrap();
        set_enabled(&path, "codes", false).unwrap();
        let paused = std::fs::read_to_string(&path).unwrap();
        assert!(!paused.contains("proposed_by"), "{paused}");

        let mut replacing = batch(&["codes"]);
        replacing.replace = true;
        assert!(
            propose_all(&path, replacing, "mcp:host").is_err(),
            "replace"
        );
        assert!(withdraw(&path, "codes", "mcp:host").is_err(), "withdraw");
        assert!(reject(&path, "codes").is_err(), "reject");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), paused);
    }

    const TWO_ACCOUNTS: &str = "[[accounts]]\nname = \"work\"\nhost = \"imap.example.com\"\nusername = \"w@example.com\"\npassword = { keyring = true }\n\n[[accounts]]\nname = \"home\"\nhost = \"mail.home.test\"\nusername = \"h\"\naddress = \"h@home.test\"\npassword = { keyring = true }\n";

    fn first_seen(paths: &Paths, account: &str, name: &str, at: i64) -> i64 {
        Store::open_account(paths, account)
            .unwrap()
            .rule_first_seen(name, "definition", at)
            .unwrap()
    }

    #[test]
    fn approve_from_now_restarts_the_clock_of_every_account_in_config_toml() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        std::fs::write(paths.config_file(), TWO_ACCOUNTS).unwrap();
        std::fs::write(paths.rules_file(), HUMAN).unwrap();
        propose(&paths.rules_file(), proposal("codes"), "cli").unwrap();
        // An older clock survives in both stores, as when the rule ran before and was disabled again.
        for account in ["work", "home"] {
            assert_eq!(first_seen(&paths, account, "codes", 1), 1);
        }
        approve_from_now(&paths, "codes", 500).unwrap();
        for account in ["work", "home"] {
            assert_eq!(first_seen(&paths, account, "codes", 900), 500, "{account}");
        }
        assert!(load(&paths.rules_file()).unwrap().rules[1].enabled);
    }

    #[test]
    fn approve_from_now_with_an_unreadable_config_leaves_the_rule_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        std::fs::write(paths.config_file(), "accounts = [").unwrap();
        std::fs::write(paths.rules_file(), HUMAN).unwrap();
        propose(&paths.rules_file(), proposal("codes"), "cli").unwrap();
        assert!(matches!(
            approve_from_now(&paths, "codes", 500),
            Err(RulesError::Config(_))
        ));
        assert!(!load(&paths.rules_file()).unwrap().rules[1].enabled);
    }

    #[test]
    fn reject_removes_only_pending_proposals() {
        let (_dir, path) = rules_file(HUMAN);
        propose(&path, proposal("codes"), "cli").unwrap();
        assert!(
            reject(&path, "keep").is_err(),
            "a human rule is not a proposal"
        );
        reject(&path, "codes").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(parse(&text).unwrap().rules.len(), 1);
        assert!(text.contains("# do not touch"), "{text}");
    }

    #[test]
    fn reject_keeps_comments_above_the_proposal() {
        let human = format!("{HUMAN}\n# TODO newsletters rule\n# [[rules]]\n# name = \"draft\"\n");
        let (_dir, path) = rules_file(&human);
        propose(&path, proposal("codes"), "cli").unwrap();
        reject(&path, "codes").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), human);

        propose(&path, proposal("codes"), "cli").unwrap();
        propose(&path, proposal("other"), "cli").unwrap();
        reject(&path, "codes").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("# TODO newsletters rule\n# [[rules]]\n# name = \"draft\""),
            "{text}"
        );
        assert_eq!(parse(&text).unwrap().rules.len(), 2);
    }

    #[test]
    fn reject_of_the_last_table_keeps_comment_order() {
        let (_dir, path) = rules_file(
            "# head\n[[rules]]\nname = \"keep\"\nmatch.read = true\nactions = { flagged = true }\n\n# above\n",
        );
        propose(&path, proposal("codes"), "cli").unwrap();
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("# trailing\n");
        std::fs::write(&path, text).unwrap();
        reject(&path, "codes").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let at = |needle: &str| text.find(needle).unwrap();
        assert!(
            at("# head") < at("# above") && at("# above") < at("# trailing"),
            "{text}"
        );
        assert_eq!(parse(&text).unwrap().rules.len(), 1);
    }

    /// Regression (#350): each appended table kept the position it had in the one-rule document it was built in, so
    /// toml_edit wrote the second and later proposals above the ones before them.
    #[test]
    fn sequential_proposals_keep_their_order() {
        let (_dir, path) = rules_file(HUMAN);
        for i in 0..5 {
            propose(&path, proposal(&format!("p{i}")), "cli").unwrap();
        }
        assert_eq!(names(&path), ["keep", "p0", "p1", "p2", "p3", "p4"]);
    }

    fn names(path: &Path) -> Vec<String> {
        load(path)
            .unwrap()
            .rules
            .into_iter()
            .map(|r| r.name)
            .collect()
    }

    fn batch(names: &[&str]) -> Proposals {
        Proposals {
            rules: names.iter().map(|n| proposal(n)).collect(),
            ..Proposals::default()
        }
    }

    #[test]
    fn a_batch_is_appended_in_order_disabled() {
        let (_dir, path) = rules_file(HUMAN);
        propose_all(&path, batch(&["a", "b", "c"]), "mcp:host").unwrap();
        let file = load(&path).unwrap();
        assert_eq!(names(&path), ["keep", "a", "b", "c"]);
        assert!(
            file.rules[1..]
                .iter()
                .all(|r| !r.enabled && r.proposed_by.as_deref() == Some("mcp:host"))
        );
        assert!(std::fs::read_to_string(&path).unwrap().starts_with(HUMAN));
    }

    #[test]
    fn a_batch_with_any_bad_rule_stores_nothing_and_names_each_by_index() {
        let (_dir, path) = rules_file(HUMAN);
        let mut proposals = batch(&["a", "keep", "b", "a"]);
        proposals.rules[2].actions.clear();
        let err = propose_all(&path, proposals, "cli")
            .unwrap_err()
            .to_string();
        assert!(err.starts_with("nothing proposed: "), "{err}");
        for part in [
            "rule 1 'keep': duplicate rule name",
            "rule 2 'b': actions must not be empty",
            "rule 3 'a': named twice in this batch",
        ] {
            assert!(err.contains(part), "{part} in {err}");
        }
        assert!(!err.contains("rule 0"), "{err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), HUMAN);
        assert!(propose_all(&path, batch(&[]), "cli").is_err());
    }

    #[test]
    fn a_lone_rule_keeps_the_rules_check_wording() {
        let (_dir, path) = rules_file(HUMAN);
        let err = propose(&path, proposal("keep"), "cli")
            .unwrap_err()
            .to_string();
        assert!(err.starts_with("rule 'keep': duplicate rule name"), "{err}");
    }

    #[test]
    fn a_batch_goes_before_or_after_a_named_rule() {
        let (_dir, path) = rules_file(TWO);
        let mut proposals = batch(&["a", "b"]);
        proposals.place = Place::After("codes".into());
        propose_all(&path, proposals, "cli").unwrap();
        assert_eq!(names(&path), ["codes", "a", "b", "keep"]);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.starts_with("# my rules\n\n# codes go after an hour\n"),
            "{text}"
        );
        assert!(text.ends_with(KEEP), "{text}");

        let mut proposals = batch(&["c"]);
        proposals.place = Place::Before("codes".into());
        propose_all(&path, proposals, "cli").unwrap();
        assert_eq!(names(&path), ["c", "codes", "a", "b", "keep"]);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.starts_with("# my rules\n\n[[rules]]\nname = \"c\"\n"),
            "{text}"
        );
        assert!(
            text.contains("\n\n# codes go after an hour\n[[rules]]\nname = \"codes\""),
            "{text}"
        );

        let mut proposals = batch(&["d"]);
        proposals.place = Place::Before("missing".into());
        assert!(propose_all(&path, proposals, "cli").is_err());
        let mut proposals = batch(&["e"]);
        proposals.place = Place::After("e".into());
        assert!(
            propose_all(&path, proposals, "cli").is_err(),
            "anchor in the batch"
        );
    }

    #[test]
    fn replace_swaps_only_the_callers_own_pending_proposal() {
        let (_dir, path) = rules_file(HUMAN);
        propose_all(&path, batch(&["a", "b"]), "cli:me").unwrap();
        propose(&path, proposal("theirs"), "cli:other").unwrap();

        let mut changed = proposal("a");
        changed.matches.read = Some(true);
        let again = Proposals {
            rules: vec![changed.clone()],
            ..Proposals::default()
        };
        assert!(
            propose_all(&path, again.clone(), "cli:me").is_err(),
            "without replace"
        );

        let replacing = Proposals {
            replace: true,
            ..again
        };
        propose_all(&path, replacing.clone(), "cli:me").unwrap();
        assert_eq!(names(&path), ["keep", "a", "b", "theirs"], "in place");
        assert_eq!(load(&path).unwrap().rules[1].matches.read, Some(true));

        assert!(
            propose_all(&path, replacing.clone(), "cli:other").is_err(),
            "not theirs"
        );
        let mut human = replacing.clone();
        human.rules[0].name = "keep".into();
        assert!(propose_all(&path, human, "cli:me").is_err(), "a human rule");

        let moved = Proposals {
            place: Place::Before("keep".into()),
            ..replacing
        };
        propose_all(&path, moved, "cli:me").unwrap();
        assert_eq!(names(&path), ["a", "keep", "b", "theirs"], "moved");
        set_enabled(&path, "b", true).unwrap();
        let mut approved = batch(&["b"]);
        approved.replace = true;
        assert!(propose_all(&path, approved, "cli:me").is_err(), "approved");
    }

    #[test]
    fn withdraw_removes_only_the_callers_own_pending_proposal() {
        let (_dir, path) = rules_file(HUMAN);
        propose(&path, proposal("mine"), "mcp:me").unwrap();
        propose(&path, proposal("theirs"), "mcp:other").unwrap();
        propose(&path, proposal("approved"), "mcp:me").unwrap();
        set_enabled(&path, "approved", true).unwrap();
        for (name, by) in [
            ("theirs", "mcp:me"),
            ("approved", "mcp:me"),
            ("keep", "mcp:me"),
            ("missing", "mcp:me"),
        ] {
            assert!(withdraw(&path, name, by).is_err(), "{name}");
        }
        withdraw(&path, "mine", "mcp:me").unwrap();
        assert_eq!(names(&path), ["keep", "theirs", "approved"]);
        assert!(std::fs::read_to_string(&path).unwrap().starts_with(HUMAN));
    }

    #[test]
    fn read_proposals_takes_one_rule_an_array_or_an_object() {
        let rule = r#"{"name": "a", "match": {"read": true}, "actions": {"flagged": true}}"#;
        let read = |text: &str| read_proposals(serde_json::from_str(text).unwrap());

        let one = read(rule).unwrap();
        assert_eq!(
            (one.rules.len(), one.place, one.replace),
            (1, Place::End, false)
        );
        let one = read(&rule.replace(
            "{\"name\"",
            "{\"after\": \"x\", \"replace\": true, \"name\"",
        ))
        .unwrap();
        assert_eq!((one.place, one.replace), (Place::After("x".into()), true));

        let array = read(&format!("[{rule}, {}]", rule.replace("\"a\"", "\"b\""))).unwrap();
        assert_eq!(array.rules.len(), 2);
        let object = read(&format!("{{\"rules\": [{rule}], \"before\": \"x\"}}")).unwrap();
        assert_eq!(object.place, Place::Before("x".into()));
        // A host that sends every optional key, unset ones as null, still proposes one rule.
        let nulls = read(&rule.replace(
            "{\"name\"",
            "{\"rules\": null, \"before\": null, \"after\": null, \"replace\": null, \"name\"",
        ))
        .unwrap();
        assert_eq!(
            (nulls.rules.len(), nulls.place, nulls.replace),
            (1, Place::End, false)
        );

        let err = read(&format!("[{rule}, {{\"name\": \"bad\"}}]"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("rule 1 'bad'"), "{err}");
        for bad in [
            "3".to_string(),
            format!("{{\"rules\": [{rule}], \"other\": 1}}"),
            format!("{{\"rules\": [{rule}], \"before\": \"x\", \"after\": \"y\"}}"),
            format!("{{\"rules\": {rule}}}"),
            rule.replace("\"name\"", "\"nmae\""),
        ] {
            assert!(read(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn concurrent_proposals_are_all_kept() {
        let (_dir, path) = rules_file(HUMAN);
        std::thread::scope(|scope| {
            for i in 0..8 {
                let path = &path;
                scope.spawn(move || propose(path, proposal(&format!("p{i}")), "cli").unwrap());
            }
        });
        let names: std::collections::HashSet<String> = load(&path)
            .unwrap()
            .rules
            .into_iter()
            .map(|r| r.name)
            .collect();
        assert_eq!(names.len(), 9, "{names:?}");
    }

    #[test]
    fn approve_keeps_a_comment_on_the_enabled_line() {
        let (_dir, path) = rules_file(
            "[[rules]]\nname = \"x\"\nenabled = false # waiting for review\nproposed_by = \"cli\"\nmatch.read = true\nactions = { flagged = true }\n",
        );
        approve(&path, "x").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("enabled = true # waiting for review\n"),
            "{text}"
        );
    }

    #[test]
    fn set_enabled_toggles_one_rule_and_keeps_the_rest() {
        const TWO_RULES: &str = "# keep me\n[[rules]]\nname = \"a\"   # inline\nmatch.read = true\nactions = { flagged = true }\n\n[[rules]]\nname = \"b\"\nenabled = false # off for now\nmatch.read = true\nactions = { flagged = true }\n";

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rules.toml");
        std::fs::write(&path, TWO_RULES).unwrap();
        let b_section =
            |text: &str| text[text.find("[[rules]]\nname = \"b\"").unwrap()..].to_string();

        set_enabled(&path, "a", false).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.starts_with("# keep me\n[[rules]]\nname = \"a\"   # inline\n"),
            "{text}"
        );
        assert!(text.contains("enabled = false"));
        assert_eq!(b_section(&text), b_section(TWO_RULES));

        set_enabled(&path, "b", true).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("enabled = true # off for now"), "{text}");
        assert!(set_enabled(&path, "missing", true).is_err());
    }

    fn rule(name: &str, subject: &str) -> Rule {
        serde_json::from_str(&format!(
            r#"{{"name": "{name}", "match": {{"subject": {{"contains": "{subject}"}}}}, "actions": {{"delete": true}}}}"#
        ))
        .unwrap()
    }

    const TWO: &str = "# my rules\n\n# codes go after an hour\n[[rules]]\nname = \"codes\" # sign-in codes\nmatch.subject = { contains = \"code\" } # any code\nmatch.read = true\nactions = { delete = true }\n\n[[rules]]\nname = \"keep\"   # do not touch\nmatch.read = true\nactions = { flagged = true }\n";

    const KEEP: &str = "\n[[rules]]\nname = \"keep\"   # do not touch\nmatch.read = true\nactions = { flagged = true }\n";

    #[test]
    fn save_rule_appends_a_new_rule_in_the_docs_style() {
        let (_dir, path) = rules_file(HUMAN);
        save_rule(&path, None, &rule("codes", "code")).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!(
                "{HUMAN}\n[[rules]]\nname = \"codes\"\nmatch.subject = {{ contains = \"code\" }}\nactions.delete = true\n"
            )
        );
    }

    #[test]
    fn save_rule_writes_headers_and_actions_as_dotted_keys() {
        let (_dir, path) = rules_file(HUMAN);
        let mut moved = rule("receipts", "receipt");
        moved.matches.header = Some(
            [("List-Id".to_string(), moved.matches.subject.take().unwrap())]
                .into_iter()
                .collect(),
        );
        moved.actions = vec![
            Action::Move("Receipts".into()),
            Action::Tag("$a".into()),
            Action::Tag("$b".into()),
        ];
        save_rule(&path, None, &moved).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.ends_with(
                "match.header.List-Id = { contains = \"receipt\" }\nactions.move = \"Receipts\"\nactions.tag = [\"$a\", \"$b\"]\n"
            ),
            "{text}"
        );
        assert_eq!(load(&path).unwrap().rules[1].actions, moved.actions);
    }

    #[test]
    fn save_rule_into_a_missing_or_comment_only_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rules.toml");
        save_rule(&path, None, &rule("codes", "code")).unwrap();
        assert_eq!(load(&path).unwrap().rules.len(), 1);

        let (_dir, path) = rules_file("# my rules\n");
        save_rule(&path, None, &rule("codes", "code")).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.starts_with("# my rules\n\n[[rules]]\nname = \"codes\"\n"),
            "{text}"
        );
    }

    #[test]
    fn save_rule_replaces_in_place_and_keeps_comments_and_unchanged_lines() {
        let (_dir, path) = rules_file(TWO);
        let mut changed = rule("codes", "verification code");
        changed.matches.read = Some(true);
        save_rule(&path, Some("codes"), &changed).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            text,
            TWO.replace(
                "{ contains = \"code\" }",
                "{ contains = \"verification code\" }"
            )
        );
        assert!(text.ends_with(KEEP), "{text}");
    }

    #[test]
    fn save_rule_edits_header_style_tables_without_restyling_them() {
        let headers = "[[rules]]\nname = \"codes\"\nactions = { delete = true }\n\n[rules.match]\nread = true # after reading\n\n[rules.match.subject]\ncontains = \"code\"\n";
        let (_dir, path) = rules_file(headers);
        let mut changed = rule("codes", "code");
        changed.matches.read = Some(false);
        save_rule(&path, Some("codes"), &changed).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            headers.replace("read = true", "read = false")
        );
    }

    #[test]
    fn save_rule_keeps_any_tables_and_value_lists_as_written() {
        let grouped = "[[rules]]\nname = \"codes\"\nactions = { delete = true }\n\n[rules.match]\nread = true\nsubject = { contains = [\"code\", \"pin\"] }\n\n[[rules.match.any]]\nfrom = { contains = \"a\" }\n\n[[rules.match.any]]\nto_me = false\n";
        let (_dir, path) = rules_file(grouped);
        let mut changed = load(&path).unwrap().rules.remove(0);
        changed.matches.read = Some(false);
        save_rule(&path, Some("codes"), &changed).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            grouped.replace("read = true", "read = false")
        );
    }

    #[test]
    fn save_rule_renames_in_place() {
        let (_dir, path) = rules_file(TWO);
        let mut renamed = rule("sign-in codes", "code");
        renamed.matches.read = Some(true);
        save_rule(&path, Some("codes"), &renamed).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("name = \"sign-in codes\" # sign-in codes\n"),
            "{text}"
        );
        let names: Vec<String> = load(&path)
            .unwrap()
            .rules
            .into_iter()
            .map(|r| r.name)
            .collect();
        assert_eq!(names, ["sign-in codes", "keep"]);
    }

    #[test]
    fn save_rule_refuses_a_taken_name_a_vanished_rule_and_bad_rules() {
        let (_dir, path) = rules_file(TWO);
        assert!(
            save_rule(&path, None, &rule("keep", "x")).is_err(),
            "new rule, taken name"
        );
        assert!(
            save_rule(&path, Some("codes"), &rule("keep", "x")).is_err(),
            "rename onto a taken name"
        );
        match save_rule(&path, Some("gone"), &rule("gone", "x")) {
            Err(RulesError::Invalid { reason, .. }) => assert_eq!(reason, "no such rule"),
            other => panic!("expected no such rule, got {other:?}"),
        }
        let mut bad = rule("codes", "x");
        bad.matches.subject = Some(TextMatch {
            regex: Some("(".into()),
            ..Default::default()
        });
        assert!(
            save_rule(&path, Some("codes"), &bad).is_err(),
            "invalid regex"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), TWO);
    }

    #[test]
    fn save_rule_refuses_an_inline_rules_array() {
        let (_dir, path) = rules_file("rules = []\n");
        assert!(save_rule(&path, None, &rule("codes", "code")).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "rules = []\n");
    }

    #[test]
    fn move_rule_changes_the_order_and_takes_its_own_comments_along() {
        let (_dir, path) = rules_file(TWO);
        move_rule(&path, "codes", &Place::After("keep".into())).unwrap();
        assert_eq!(names(&path), ["keep", "codes"]);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.starts_with("# my rules\n\n[[rules]]\nname = \"keep\""),
            "{text}"
        );
        assert!(
            text.contains(
                "}\n\n# codes go after an hour\n[[rules]]\nname = \"codes\" # sign-in codes\n"
            ),
            "{text}"
        );

        move_rule(&path, "codes", &Place::Before("keep".into())).unwrap();
        assert_eq!(names(&path), ["codes", "keep"]);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), TWO);

        move_rule(&path, "codes", &Place::End).unwrap();
        assert_eq!(names(&path), ["keep", "codes"]);
    }

    #[test]
    fn move_rule_refuses_a_missing_rule_or_anchor_and_itself() {
        let (_dir, path) = rules_file(TWO);
        for (name, place) in [
            ("missing", Place::End),
            ("codes", Place::Before("missing".into())),
            ("codes", Place::After("codes".into())),
        ] {
            assert!(move_rule(&path, name, &place).is_err(), "{name} {place:?}");
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), TWO);
    }

    #[test]
    fn remove_rule_removes_any_rule_and_keeps_the_comments_above_it() {
        let (_dir, path) = rules_file(TWO);
        remove_rule(&path, "codes").unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("# my rules\n\n# codes go after an hour\n{KEEP}")
        );
        assert!(remove_rule(&path, "codes").is_err());
    }

    fn follow(
        text: &str,
        accounts: &[&str],
        account: &str,
        from: &str,
        to: &str,
    ) -> (Vec<String>, String) {
        let (_dir, path) = rules_file(text);
        let accounts: Vec<String> = accounts.iter().map(|a| a.to_string()).collect();
        let rename = FolderRename {
            account,
            accounts: &accounts,
            from,
            to,
            delimiter: Some("/"),
        };
        let changed = follow_rename(&path, &rename).unwrap();
        (changed, std::fs::read_to_string(&path).unwrap())
    }

    const WORK: &str = "[[rules]]\nname = \"r\" # keep\naccount = \"a\"\nfolder = \"Work\"\nmatch.read = true\nactions.move = \"Work/Old\"\n";

    #[test]
    fn follow_rename_rewrites_folder_and_move() {
        let (changed, text) = follow(WORK, &["a"], "a", "Work", "Job");
        assert_eq!(changed, ["r"]);
        assert_eq!(
            text,
            WORK.replace("\"Work\"", "\"Job\"")
                .replace("Work/Old", "Job/Old")
        );
    }

    #[test]
    fn follow_rename_follows_children_not_lookalikes() {
        let text = "[[rules]]\nname = \"a\"\nfolder = \"Work/Clients\"\nmatch.read = true\nactions.flagged = true\n\n[[rules]]\nname = \"b\"\nfolder = \"Workshop\"\nmatch.read = true\nactions.flagged = true\n";
        let (changed, text) = follow(text, &["a"], "a", "Work", "Work2");
        assert_eq!(changed, ["a"]);
        assert!(text.contains("folder = \"Work2/Clients\""), "{text}");
        assert!(text.contains("folder = \"Workshop\""), "{text}");
    }

    #[test]
    fn follow_rename_matches_nfd_names() {
        let text = "[[rules]]\nname = \"a\"\nfolder = \"Cafe\u{301}\"\nmatch.read = true\nactions.flagged = true\n";
        let (changed, text) = follow(text, &["a"], "a", "Caf\u{e9}", "Bar");
        assert_eq!(changed, ["a"]);
        assert!(text.contains("folder = \"Bar\""), "{text}");
    }

    #[test]
    fn follow_rename_leaves_other_accounts_rules() {
        let (changed, text) = follow(WORK, &["a", "b"], "b", "Work", "Job");
        assert!(changed.is_empty());
        assert_eq!(text, WORK);
    }

    #[test]
    fn follow_rename_rewrites_unscoped_rule_with_one_account() {
        let unscoped = WORK.replace("account = \"a\"\n", "");
        let (changed, text) = follow(&unscoped, &["a"], "a", "Work", "Job");
        assert_eq!(changed, ["r"]);
        assert!(
            text.contains("name = \"r\" # keep") && text.contains("folder = \"Job\""),
            "{text}"
        );
    }

    #[test]
    fn follow_rename_splits_unscoped_rule_with_several_accounts() {
        let unscoped = WORK.replace("account = \"a\"\n", "");
        let (changed, text) = follow(&unscoped, &["a", "b", "c"], "b", "Work", "Job");
        assert_eq!(changed, ["r"]);
        let rules = parse(&text).unwrap().rules;
        let seen: Vec<_> = rules
            .iter()
            .map(|r| {
                (
                    r.name.as_str(),
                    r.account.as_deref().unwrap(),
                    r.folder.as_deref().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            seen,
            [
                ("r (a)", "a", "Work"),
                ("r (c)", "c", "Work"),
                ("r (b)", "b", "Job")
            ]
        );
        assert!(text.contains("name = \"r (a)\" # keep"), "{text}");
    }

    #[test]
    fn split_copies_avoid_taken_names() {
        let unscoped = format!(
            "{}\n[[rules]]\nname = \"r (a)\"\nmatch.read = true\nactions.flagged = true\n",
            WORK.replace("account = \"a\"\n", "")
        );
        let (_, text) = follow(&unscoped, &["a", "b"], "b", "Work", "Job");
        let names: Vec<_> = parse(&text)
            .unwrap()
            .rules
            .into_iter()
            .map(|r| r.name)
            .collect();
        assert_eq!(names, ["r (a) 2", "r (b)", "r (a)"]);
    }

    #[test]
    fn rules_naming_marks_splits() {
        let text = format!(
            "{WORK}\n[[rules]]\nname = \"all\"\nfolder = \"Work\"\nmatch.read = true\nactions.flagged = true\n\n[[rules]]\nname = \"inbox\"\nmatch.read = true\nactions.flagged = true\n"
        );
        let file = parse(&text).unwrap();
        let accounts = ["a".to_string(), "b".to_string()];
        let rename = FolderRename {
            account: "a",
            accounts: &accounts,
            from: "Work",
            to: "Job",
            delimiter: Some("/"),
        };
        assert_eq!(
            rules_naming(&file, &rename),
            [("r".to_string(), false), ("all".to_string(), true)]
        );
    }

    const HEADERS: &str = "[[rules]]\nname = \"r\"\nfolder = \"Work\"\n\n[rules.match]\nread = true\n\n[rules.actions]\nflagged = true\n\n[[rules]]\nname = \"s\"\n\n[rules.match]\nread = false\n\n[rules.actions]\ndelete = true\n";

    #[test]
    fn follow_rename_splits_a_header_style_rule() {
        let (_, text) = follow(HEADERS, &["a", "b", "c"], "b", "Work", "Job");
        let rules = parse(&text).unwrap().rules;
        let seen: Vec<_> = rules
            .iter()
            .map(|r| (r.name.as_str(), r.matches.read, r.actions.len()))
            .collect();
        assert_eq!(
            seen,
            [
                ("r (a)", Some(true), 1),
                ("r (c)", Some(true), 1),
                ("r (b)", Some(true), 1),
                ("s", Some(false), 1)
            ]
        );
        assert_eq!(text.matches("[rules.match]").count(), 4, "{text}");
    }

    #[test]
    fn rules_after_rename_names_the_rules_left_behind_when_the_rewrite_fails() {
        let (_dir, path) = rules_file(WORK);
        let accounts = ["a".to_string()];
        let rename = FolderRename {
            account: "a",
            accounts: &accounts,
            from: "Work",
            to: "Jo*b",
            delimiter: Some("/"),
        };
        let error = rules_after_rename(&path, &rename).unwrap_err();
        assert!(error.ends_with("these rules still name Work: r"), "{error}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), WORK);
        let rename = FolderRename {
            to: "Job",
            ..rename
        };
        assert_eq!(
            rules_after_rename(&path, &rename),
            Ok(vec![("r".to_string(), false)])
        );
    }

    #[test]
    fn rules_after_rename_makes_no_file_when_no_rule_names_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rules.toml");
        let rename = FolderRename {
            account: "a",
            accounts: &["a".to_string()],
            from: "Work",
            to: "Job",
            delimiter: None,
        };
        assert_eq!(rules_after_rename(&path, &rename), Ok(Vec::new()));
        assert!(!path.exists());
    }

    #[test]
    fn follow_rename_leaves_a_file_no_rule_names_untouched() {
        let (changed, text) = follow(HEADERS, &["a", "b"], "b", "Nowhere", "Job");
        assert!(changed.is_empty());
        assert_eq!(text, HEADERS);
    }

    const NEWS: &str = "[[rules]]\nname = \"news\"\nmatch.read = true\nactions.move = \"Lists\"\n\n[[rules]]\nname = \"news (edit)\"\nenabled = false\nproposed_by = \"mcp:me\"\nreplaces = \"news\"\nmatch.read = false\nactions.move = \"Lists/Old\"\n";

    #[test]
    fn a_split_rule_takes_its_pending_edit_along_one_per_account() {
        let (_dir, path) = rules_file(NEWS);
        let accounts: Vec<String> = ["a", "b", "c"].map(String::from).to_vec();
        let rename = FolderRename {
            account: "b",
            accounts: &accounts,
            from: "Lists",
            to: "Mail/Lists",
            delimiter: Some("/"),
        };
        follow_rename(&path, &rename).unwrap();
        let rules = load(&path).unwrap().rules;
        let seen: Vec<_> = rules
            .iter()
            .map(|r| {
                (
                    r.name.as_str(),
                    r.account.as_deref().unwrap(),
                    r.replaces.as_deref(),
                    r.enabled,
                    r.actions.clone(),
                )
            })
            .collect();
        let to = |folder: &str| vec![Action::Move(folder.into())];
        assert_eq!(
            seen,
            [
                ("news (a)", "a", None, true, to("Lists")),
                (
                    "news (a) (edit)",
                    "a",
                    Some("news (a)"),
                    false,
                    to("Lists/Old")
                ),
                ("news (c)", "c", None, true, to("Lists")),
                (
                    "news (c) (edit)",
                    "c",
                    Some("news (c)"),
                    false,
                    to("Lists/Old")
                ),
                ("news (b)", "b", None, true, to("Mail/Lists")),
                (
                    "news (b) (edit)",
                    "b",
                    Some("news (b)"),
                    false,
                    to("Mail/Lists/Old")
                ),
            ]
        );
        assert!(
            rules
                .iter()
                .skip(1)
                .step_by(2)
                .all(|r| r.proposed_by.as_deref() == Some("mcp:me"))
        );
        assert_eq!(approve(&path, "news (b) (edit)").unwrap(), "news (b)");
        let rules = load(&path).unwrap().rules;
        let approved = rules.iter().find(|r| r.name == "news (b)").unwrap();
        assert!(approved.enabled && approved.replaces.is_none() && !approved.matches.read.unwrap());
        assert_eq!(approved.actions, to("Mail/Lists/Old"));
    }

    #[test]
    fn an_edit_of_a_rule_rewritten_in_place_keeps_replacing_it() {
        let (changed, text) = follow(NEWS, &["a"], "a", "Lists", "Mail/Lists");
        assert_eq!(changed, ["news", "news (edit)"]);
        let rules = parse(&text).unwrap().rules;
        assert_eq!(rules[1].replaces.as_deref(), Some("news"));
        assert_eq!(rules[1].actions, [Action::Move("Mail/Lists/Old".into())]);
    }

    fn edit_of(names: &[&str]) -> Proposals {
        Proposals {
            edit: true,
            ..batch(names)
        }
    }

    #[test]
    fn an_edit_is_stored_disabled_after_its_rule_which_stays_as_it_is() {
        let (_dir, path) = rules_file(&format!(
            "{HUMAN}\n[[rules]]\nname = \"last\"\nmatch.read = true\nactions = {{ flagged = true }}\n"
        ));
        let mut edit = edit_of(&["keep"]);
        edit.rules[0].proposed_by = Some("spoofed".into());
        propose_all(&path, edit, "mcp:me").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with(HUMAN), "{text}");
        let rules = load(&path).unwrap().rules;
        assert_eq!(names(&path), ["keep", "keep (edit)", "last"]);
        let edit = &rules[1];
        assert!(!edit.enabled);
        assert_eq!(edit.proposed_by.as_deref(), Some("mcp:me"));
        assert_eq!(edit.replaces.as_deref(), Some("keep"));
        assert!(rules[0].enabled && rules[0].replaces.is_none());
    }

    #[test]
    fn an_edit_needs_a_rule_that_is_not_a_proposal() {
        let (_dir, path) = rules_file(HUMAN);
        propose(&path, proposal("codes"), "mcp:me").unwrap();
        let before = std::fs::read_to_string(&path).unwrap();
        let err = propose_all(&path, edit_of(&["missing"]), "mcp:me")
            .unwrap_err()
            .to_string();
        assert!(err.contains("no rule to edit"), "{err}");
        let err = propose_all(&path, edit_of(&["codes"]), "mcp:me")
            .unwrap_err()
            .to_string();
        assert!(err.contains("propose it again with replace"), "{err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    }

    #[test]
    fn a_moved_rule_takes_its_pending_edit_along_and_lands_after_its_anchors() {
        let (_dir, path) = rules_file(TWO);
        propose_all(&path, edit_of(&["codes"]), "mcp:me").unwrap();
        move_rule(&path, "codes", &Place::After("keep".into())).unwrap();
        assert_eq!(names(&path), ["keep", "codes", "codes (edit)"]);
        move_rule(&path, "keep", &Place::After("codes".into())).unwrap();
        assert_eq!(names(&path), ["codes", "codes (edit)", "keep"]);
        assert!(move_rule(&path, "codes", &Place::After("codes (edit)".into())).is_err());
        assert!(move_rule(&path, "codes", &Place::After("codes".into())).is_err());
        assert_eq!(names(&path), ["codes", "codes (edit)", "keep"]);
    }

    #[test]
    fn a_rule_has_one_pending_edit_which_only_its_proposer_replaces() {
        let (_dir, path) = rules_file(HUMAN);
        propose_all(&path, edit_of(&["keep"]), "mcp:me").unwrap();
        assert!(
            propose_all(&path, edit_of(&["keep"]), "mcp:me").is_err(),
            "without replace"
        );
        let mut theirs = edit_of(&["keep"]);
        theirs.replace = true;
        assert!(
            propose_all(&path, theirs.clone(), "mcp:other").is_err(),
            "another proposer"
        );
        theirs.rules[0].actions = vec![Action::Flag];
        propose_all(&path, theirs, "mcp:me").unwrap();
        assert_eq!(names(&path), ["keep", "keep (edit)"]);
        assert_eq!(load(&path).unwrap().rules[1].actions, vec![Action::Flag]);
    }

    #[test]
    fn a_replaces_key_is_only_written_by_an_edit() {
        let (_dir, path) = rules_file(HUMAN);
        let mut sneaky = proposal("codes");
        sneaky.replaces = Some("keep".into());
        propose(&path, sneaky, "mcp:me").unwrap();
        assert!(load(&path).unwrap().rules[1].replaces.is_none());
    }

    #[test]
    fn approving_an_edit_writes_it_over_its_rule_in_place() {
        let (_dir, path) = rules_file(&format!(
            "{HUMAN}\n# paused for now\n[[rules]]\nname = \"off\"\nenabled = false # by hand\nmatch.read = true\nactions = {{ flagged = true }}\n"
        ));
        propose_all(&path, edit_of(&["keep", "off"]), "mcp:me").unwrap();
        assert_eq!(approve(&path, "keep (edit)").unwrap(), "keep");
        assert_eq!(approve(&path, "off (edit)").unwrap(), "off");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.starts_with("# my rules\n[[rules]]\nname = \"keep\" # do not touch\n"),
            "{text}"
        );
        assert!(
            text.contains("# paused for now\n") && text.contains("enabled = false # by hand"),
            "{text}"
        );
        assert!(
            !text.contains("proposed_by") && !text.contains("replaces"),
            "{text}"
        );
        let rules = load(&path).unwrap().rules;
        assert_eq!(names(&path), ["keep", "off"]);
        assert!(
            rules[0].enabled && !rules[1].enabled,
            "each keeps its own enabled"
        );
        assert!(rules.iter().all(|r| r.actions == proposal("x").actions));
    }

    #[test]
    fn an_edit_whose_rule_is_gone_cannot_be_approved_but_can_be_rejected() {
        let (_dir, path) = rules_file(HUMAN);
        propose_all(&path, edit_of(&["keep"]), "mcp:me").unwrap();
        remove_rule(&path, "keep").unwrap();
        let err = approve(&path, "keep (edit)").unwrap_err().to_string();
        assert!(err.contains("is no longer in rules.toml"), "{err}");
        reject(&path, "keep (edit)").unwrap();
        assert!(names(&path).is_empty());
    }

    #[test]
    fn approve_from_now_restarts_the_clock_of_the_edited_rule() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        std::fs::write(paths.config_file(), TWO_ACCOUNTS).unwrap();
        std::fs::write(paths.rules_file(), HUMAN).unwrap();
        assert_eq!(first_seen(&paths, "work", "keep", 1), 1);
        propose_all(&paths.rules_file(), edit_of(&["keep"]), "cli").unwrap();
        approve_from_now(&paths, "keep (edit)", 500).unwrap();
        assert_eq!(first_seen(&paths, "work", "keep", 900), 500);
    }

    #[test]
    fn read_proposals_takes_edit_but_not_with_a_place() {
        let rule = r#"{"name": "a", "match": {"read": true}, "actions": {"flagged": true}}"#;
        let read = |text: &str| read_proposals(serde_json::from_str(text).unwrap());
        assert!(
            read(&rule.replace("{\"name\"", "{\"edit\": true, \"name\""))
                .unwrap()
                .edit
        );
        assert!(
            read(&format!("{{\"rules\": [{rule}], \"edit\": true}}"))
                .unwrap()
                .edit
        );
        assert!(
            !read(&rule.replace("{\"name\"", "{\"edit\": null, \"name\""))
                .unwrap()
                .edit
        );
        for bad in [
            rule.replace("{\"name\"", "{\"edit\": 1, \"name\""),
            rule.replace("{\"name\"", "{\"edit\": true, \"after\": \"x\", \"name\""),
        ] {
            assert!(read(&bad).is_err(), "{bad}");
        }
    }
}
