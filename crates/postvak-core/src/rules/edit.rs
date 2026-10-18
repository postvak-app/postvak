//! The only code that writes rules.toml: proposals, approvals, rejections, and rules saved or removed by the window.
//! Each edit is validated as a whole file before it replaces the old one.
use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, Value};

use crate::paths::{Paths, create_private_dir, read_or_empty, write_atomic};
use crate::rules::{Rule, RuleFile, RulesError, compile, invalid, parse};
use crate::store::Store;

/// Appends `rule` disabled and attributed to `by`, so it only acts once a human approves it.
pub fn propose(path: &Path, mut rule: Rule, by: &str) -> Result<(), RulesError> {
    rule.enabled = false;
    rule.proposed_by = Some(by.to_string());
    let snippet = toml::to_string(&RuleFile { rules: vec![rule] })
        .map_err(|e| RulesError::Parse(e.to_string()))?;
    let _lock = lock(path)?;
    let mut text = read_or_empty(path)?;
    if !text.is_empty() {
        if !text.ends_with('\n') {
            text.push('\n');
        }
        text.push('\n');
    }
    text.push_str(&snippet);
    save(path, &text)
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

fn approve(path: &Path, name: &str) -> Result<(), RulesError> {
    edit(path, name, |rules, index| {
        let table = rules
            .get_mut(index)
            .ok_or_else(|| invalid(name, "no such rule"))?;
        if !is_disabled(table) {
            return Err(invalid(name, "is already enabled"));
        }
        write_enabled(table, true);
        Ok(None)
    })
}

/// Approves `name` and restarts its clock in each account's store, so it acts only on mail that arrives from now on,
/// even where an older clock of the rule survived.
pub fn approve_from_now(
    paths: &Paths,
    accounts: &[String],
    name: &str,
    now: i64,
) -> Result<(), RulesError> {
    approve(&paths.rules_file(), name)?;
    for account in accounts {
        Store::open_account(paths, account)?.restart_rule_clock(name, now)?;
    }
    Ok(())
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
        let _lock = lock(path)?;
        let text = read_or_empty(path)?;
        let mut doc = parse_doc(&text)?;
        append(&mut doc, new, !text.trim().is_empty())?;
        return save(path, &doc.to_string());
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

/// `rule` as a `[[rules]]` table in the docs' style: `match.from = { … }` dotted keys with inline tables, `actions`
/// last, and no `enabled = true`.
fn rule_table(rule: &Rule) -> Result<Table, RulesError> {
    let text = toml::to_string(&RuleFile {
        rules: vec![rule.clone()],
    })
    .map_err(|e| RulesError::Parse(e.to_string()))?;
    let mut table = parse_doc(&text)?
        .remove("rules")
        .and_then(|item| item.into_array_of_tables().ok())
        .filter(|rules| !rules.is_empty())
        .map(|mut rules| rules.remove(0))
        .ok_or_else(|| invalid(&rule.name, "could not be written as TOML"))?;
    if let Some(Item::Table(conditions)) = table.get_mut("match") {
        conditions.set_dotted(true);
        for (_, item) in conditions.iter_mut() {
            *item = match std::mem::take(item).into_value() {
                Ok(value) => Item::Value(value),
                Err(other) => other,
            };
        }
        conditions.fmt();
    }
    // Actions that are all tables, such as `move`, would come out as `[[rules.actions]]` headers.
    if let Some(actions) = table.remove("actions") {
        let actions = match actions.into_value() {
            Ok(mut value) => {
                if let Value::Array(array) = &mut value {
                    array.fmt();
                }
                Item::Value(value)
            }
            Err(other) => other,
        };
        table.insert("actions", actions);
    }
    if rule.enabled {
        table.remove("enabled");
    }
    Ok(table)
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
    table.decor_mut().set_prefix(format!("{trailing}{gap}"));
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
    let gone: Vec<String> = old
        .iter()
        .map(|(key, _)| key.to_string())
        .filter(|key| !new.contains_key(key))
        .collect();
    for key in gone {
        old.remove(&key);
    }
    for (key, item) in new.iter() {
        match (old.get_mut(key), item) {
            (Some(Item::Table(old_table)), Item::Table(new_table)) => merge(old_table, new_table),
            (Some(old_item), new_item) if same_value(old_item, new_item) => {}
            (Some(old_item), new_item) => {
                let decor = old_item.as_value().map(|value| value.decor().clone());
                *old_item = new_item.clone();
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
            Value::InlineTable(table) => table.fmt(),
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
    let _lock = lock(path)?;
    let mut doc = parse_doc(&read_or_empty(path)?)?;
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
    save(path, &doc.to_string())
}

/// An exclusive lock on `.rules.lock` beside rules.toml, held until dropped, so concurrent edits cannot lose each other's.
fn lock(path: &Path) -> Result<File, RulesError> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::other("path has no parent"))?;
    create_private_dir(dir)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(".rules.lock"))?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    file.lock()?;
    Ok(file)
}

fn parse_doc(text: &str) -> Result<DocumentMut, RulesError> {
    text.parse()
        .map_err(|e: toml_edit::TomlError| RulesError::Parse(e.to_string()))
}

fn save(path: &Path, text: &str) -> Result<(), RulesError> {
    compile(&parse(text)?)?;
    write_atomic(path, text.as_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::{Action, TextMatch, load};

    const HUMAN: &str = "# my rules\n[[rules]]\nname = \"keep\" # do not touch\nmatch.seen = true\nactions = [\"flag\"]\n";

    fn proposal(name: &str) -> Rule {
        serde_json::from_str(&format!(
            r#"{{"name": "{name}", "match": {{"subject": {{"contains": "code"}}}}, "actions": [{{"move": "Codes"}}, "mark_read"]}}"#
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
            "# head\n[[rules]]\nname = \"keep\"\nmatch.seen = true\nactions = [\"flag\"]\n\n# above\n",
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
            "[[rules]]\nname = \"x\"\nenabled = false # waiting for review\nproposed_by = \"cli\"\nmatch.seen = true\nactions = [\"flag\"]\n",
        );
        approve(&path, "x").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("enabled = true # waiting for review\n"),
            "{text}"
        );
    }

    #[test]
    fn lock_file_is_private() {
        let (_dir, path) = rules_file("");
        let lock_path = path.parent().unwrap().join(".rules.lock");
        std::fs::write(&lock_path, "").unwrap();
        std::fs::set_permissions(&lock_path, std::fs::Permissions::from_mode(0o644)).unwrap();
        drop(lock(&path).unwrap());
        let mode = std::fs::metadata(&lock_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn set_enabled_toggles_one_rule_and_keeps_the_rest() {
        const TWO_RULES: &str = "# keep me\n[[rules]]\nname = \"a\"   # inline\nmatch.seen = true\nactions = [\"flag\"]\n\n[[rules]]\nname = \"b\"\nenabled = false # off for now\nmatch.seen = true\nactions = [\"flag\"]\n";

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
            r#"{{"name": "{name}", "match": {{"subject": {{"contains": "{subject}"}}}}, "actions": ["delete"]}}"#
        ))
        .unwrap()
    }

    const TWO: &str = "# my rules\n\n# codes go after an hour\n[[rules]]\nname = \"codes\" # sign-in codes\nmatch.subject = { contains = \"code\" } # any code\nmatch.seen = true\nactions = [\"delete\"]\n\n[[rules]]\nname = \"keep\"   # do not touch\nmatch.seen = true\nactions = [\"flag\"]\n";

    const KEEP: &str =
        "\n[[rules]]\nname = \"keep\"   # do not touch\nmatch.seen = true\nactions = [\"flag\"]\n";

    #[test]
    fn save_rule_appends_a_new_rule_in_the_docs_style() {
        let (_dir, path) = rules_file(HUMAN);
        save_rule(&path, None, &rule("codes", "code")).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!(
                "{HUMAN}\n[[rules]]\nname = \"codes\"\nmatch.subject = {{ contains = \"code\" }}\nactions = [\"delete\"]\n"
            )
        );
    }

    #[test]
    fn save_rule_writes_table_actions_inline() {
        let (_dir, path) = rules_file(HUMAN);
        let mut moved = rule("receipts", "receipt");
        moved.actions = vec![Action::Move("Receipts".into())];
        save_rule(&path, None, &moved).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.ends_with("actions = [{ move = \"Receipts\" }]\n"),
            "{text}"
        );
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
        changed.matches.seen = Some(true);
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
        let headers = "[[rules]]\nname = \"codes\"\nactions = [\"delete\"]\n\n[rules.match]\nseen = true # after reading\n\n[rules.match.subject]\ncontains = \"code\"\n";
        let (_dir, path) = rules_file(headers);
        let mut changed = rule("codes", "code");
        changed.matches.seen = Some(false);
        save_rule(&path, Some("codes"), &changed).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            headers.replace("seen = true", "seen = false")
        );
    }

    #[test]
    fn save_rule_keeps_any_tables_and_value_lists_as_written() {
        let grouped = "[[rules]]\nname = \"codes\"\nactions = [\"delete\"]\n\n[rules.match]\nseen = true\nsubject = { contains = [\"code\", \"pin\"] }\n\n[[rules.match.any]]\nfrom = { contains = \"a\" }\n\n[[rules.match.any]]\nto_me = false\n";
        let (_dir, path) = rules_file(grouped);
        let mut changed = load(&path).unwrap().rules.remove(0);
        changed.matches.seen = Some(false);
        save_rule(&path, Some("codes"), &changed).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            grouped.replace("seen = true", "seen = false")
        );
    }

    #[test]
    fn save_rule_renames_in_place() {
        let (_dir, path) = rules_file(TWO);
        let mut renamed = rule("sign-in codes", "code");
        renamed.matches.seen = Some(true);
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
    fn remove_rule_removes_any_rule_and_keeps_the_comments_above_it() {
        let (_dir, path) = rules_file(TWO);
        remove_rule(&path, "codes").unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("# my rules\n\n# codes go after an hour\n{KEEP}")
        );
        assert!(remove_rule(&path, "codes").is_err());
    }
}
