//! The Rules, Activity and Trash views: what replaces the list and body columns.
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use postvak_core::help;
use postvak_core::message::clean;
use postvak_core::paths::Paths;
use postvak_core::rules::hits::Hits;
use postvak_core::rules::{self, Rule};
use postvak_core::store::{LogEntry, Store};
use postvak_core::trash::{Trash, TrashEntry};

use postvak_core::time::date_time;

use super::draft;
use super::state::{Account, App, UiAction};

/// Entries shown in the Activity view.
const LOG_LIMIT: u32 = 500;

pub(crate) struct RulesState {
    pub error: Option<String>,
    /// What leaves the file valid but not doing what was meant, from `rules::warnings`.
    pub warnings: Vec<String>,
    /// (account, authserv-id) pairs the Rules view offers to trust, from `rules::authserv_offers`.
    pub offers: Vec<(String, String)>,
    /// Each proposal's dry run on the cached mail, by name, once counted; a reload drops them.
    pub dry_runs: BTreeMap<String, DryRun>,
    pub rules: Vec<Rule>,
    /// The rules as the view words them, worded once per load rather than on every frame.
    pub proposals: Vec<Proposal>,
    pub lines: Vec<RuleLine>,
}

impl RulesState {
    /// Shows `hits` on the rules they count; a rule they do not name has none.
    pub fn set_hits(&mut self, hits: &HashMap<String, Hits>) {
        for line in &mut self.lines {
            line.hits = hits.get(&line.name).copied().unwrap_or_default();
        }
    }

    /// The file's rules, or the previous ones when it does not parse. A rule that does not compile is reported but
    /// still listed, so it can be switched off.
    pub fn load(path: &Path, previous: Vec<Rule>, accounts: &[Account]) -> RulesState {
        let (error, rules) = match rules::load(path) {
            Ok(file) => (
                rules::compile(&file).err().map(|e| e.to_string()),
                file.rules,
            ),
            Err(e) => (Some(e.to_string()), previous),
        };
        let mut state = RulesState {
            error,
            warnings: Vec::new(),
            offers: Vec::new(),
            dry_runs: BTreeMap::new(),
            proposals: proposals(&rules),
            lines: rule_lines(&rules),
            rules,
        };
        state.check(path, accounts);
        state
    }

    /// Reads the warnings and offers again, which the stores and the files beside rules.toml change.
    pub fn check(&mut self, path: &Path, accounts: &[Account]) {
        let synced: Vec<rules::AccountFolders> = accounts
            .iter()
            .map(|a| rules::AccountFolders::read(&a.name, a.store.as_ref().ok()))
            .collect();
        // The accounts that trust no authserv-id, for the warning about `authenticated`.
        let untrusting: Vec<&str> = accounts
            .iter()
            .filter(|a| a.identity.as_ref().is_some_and(|id| !id.can_authenticate()))
            .map(|a| a.name.as_str())
            .collect();
        self.warnings = warnings(path, &self.rules, accounts, &untrusting, &synced);
        self.offers = rules::authserv_offers(&self.rules, &untrusting, &synced);
    }
}

/// What leaves the rules valid but not doing what was meant: `rules::warnings`, then what the accounts' folders show,
/// read through the stores the window holds open rather than opening each again.
fn warnings(
    path: &Path,
    rules: &[Rule],
    accounts: &[Account],
    untrusting: &[&str],
    synced: &[rules::AccountFolders],
) -> Vec<String> {
    let names: Vec<&str> = accounts.iter().map(|a| a.name.as_str()).collect();
    let stores: Vec<(&str, &Store)> = accounts
        .iter()
        .filter_map(|a| Some((a.name.as_str(), a.store.as_ref().ok()?)))
        .collect();
    [
        rules::warnings(path, rules, &names),
        rules::authenticated_warnings(rules, untrusting, synced),
        rules::watched_folder_warnings(rules, synced),
        rules::open_folder_warnings(rules, &stores),
    ]
    .concat()
}

/// "Trust mx.google.com for work", on the button that takes an offer.
pub(crate) fn offer_label(account: &str, id: &str) -> String {
    format!("Trust {} for {}", clean(id, false), clean(account, false))
}

/// A rule waiting for approval, as the Rules view shows it.
pub(crate) struct Proposal {
    pub name: String,
    pub label: String,
    /// "Agent proposal · claude-desktop", or "Edit proposal · …" for an edit.
    pub by: String,
    pub summary: String,
    /// What it does to rules.toml: "rules.toml · adds 6 lines", or "Changes rule 'x' · 2 lines".
    pub note: String,
    /// Whether it changes a rule rather than adding one, so `marks` holds "-" and " " too.
    pub edits: bool,
    /// The TOML it adds, or for an edit the rule's lines both as they are and as the edit has them.
    pub toml: String,
    /// One mark beside each line of `toml`, as a diff marks lines: "+" added, "-" removed, " " unchanged.
    pub marks: String,
    /// What it does as a sentence, and each line of `toml` with its mark, both split into styled runs.
    pub sentence: Vec<Run>,
    pub code: Vec<(String, Vec<Run>)>,
}

impl Proposal {
    fn matches(&self, filter: &str) -> bool {
        matches_filter(&[&self.label, &self.by, &self.summary], filter)
    }
}

/// A rule in the Rules view's table.
pub(crate) struct RuleLine {
    pub name: String,
    pub label: String,
    pub enabled: bool,
    /// "work", or "every account".
    pub account: String,
    pub folder: String,
    /// "proposed by agent", or empty.
    pub by: String,
    /// "from contains \"news@\", subject contains \"weekly\"".
    pub conditions: String,
    /// "archive, mark read".
    pub actions: String,
    /// How often it acted, once counted; until then, none.
    pub hits: Hits,
    /// "work · Inbox": the account and folder it watches.
    pub watches: String,
    /// It deletes mail, which the table marks; a deleted message is kept as an .eml backup.
    pub deletes: bool,
}

impl RuleLine {
    fn matches(&self, filter: &str) -> bool {
        let texts = [
            &*self.label,
            &self.by,
            &self.account,
            &self.folder,
            &self.conditions,
            &self.actions,
        ];
        matches_filter(&texts, filter)
    }
}

/// Whether every word of `filter` is in one of `texts`, ignoring case; an empty filter matches everything.
fn matches_filter(texts: &[&str], filter: &str) -> bool {
    let texts: Vec<String> = texts.iter().map(|text| text.to_lowercase()).collect();
    filter
        .to_lowercase()
        .split_whitespace()
        .all(|word| texts.iter().any(|text| text.contains(word)))
}

/// What the Rules table says when it shows no rule: none in the file, or none that `filter` matches.
pub(crate) fn no_rules(filter: &str) -> String {
    if filter.trim().is_empty() {
        "rules.toml has no rules yet.".into()
    } else {
        format!("No rule matches “{}”.", filter.trim())
    }
}

/// The proposals the Rules view shows for `filter`, as indices into `proposals`.
pub(crate) fn shown_proposals(proposals: &[Proposal], filter: &str) -> Vec<usize> {
    (0..proposals.len())
        .filter(|&i| proposals[i].matches(filter))
        .collect()
}

/// How many messages a proposal's card names before "N more".
pub(crate) const SAMPLES: usize = 3;

/// What a rule would do to the cached mail if it were on: how many messages it would act on, the newest few, whether it
/// needs bodies, and what makes the answer less than it seems; or why it could not be counted.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct DryRun {
    pub count: usize,
    /// The newest `SAMPLES` messages it would act on: subject and date.
    pub samples: Vec<(String, i64)>,
    pub needs_body: bool,
    pub warnings: Vec<String>,
    pub error: Option<String>,
}

/// `rule`'s dry run on the cached mail of `accounts`, read from config.toml and the stores only, as `postvak rules test`
/// runs it. It opens the stores itself, so it can run off the window thread.
pub(crate) fn dry_run(paths: &Paths, accounts: &[String], rule: &Rule, now: i64) -> DryRun {
    let failed = |error: String| DryRun {
        error: Some(error),
        ..DryRun::default()
    };
    let rule = Rule {
        enabled: true,
        ..rule.clone()
    };
    let compiled = match rules::compile(&rules::RuleFile { rules: vec![rule] }) {
        Ok(compiled) => compiled,
        Err(e) => return failed(e.to_string()),
    };
    let config = match postvak_core::config::Config::load(&paths.config_file()) {
        Ok(config) => config,
        Err(e) => return failed(e.to_string()),
    };
    let mut matched = BTreeMap::new();
    let mut warnings = Vec::new();
    for name in accounts {
        let store = match Store::open_account(paths, name) {
            Ok(store) => store,
            Err(e) => return failed(format!("{name}: {e}")),
        };
        let Some(settings) = config.account(name) else {
            continue;
        };
        let planned = settings
            .identity()
            .map_err(|e| e.to_string())
            .and_then(|identity| {
                postvak_core::actions::planned(&compiled, &store, settings, &identity, now)
                    .map_err(|e| e.to_string())
            });
        match planned {
            Ok(planned) => {
                for p in planned {
                    let at = p.message.date.unwrap_or(p.message.internaldate);
                    let subject = clean(p.message.subject.as_deref().unwrap_or_default(), false);
                    matched.insert(
                        (name.clone(), p.message.folder, p.message.uid),
                        (subject, at),
                    );
                }
            }
            Err(e) => return failed(format!("{name}: {e}")),
        }
        warnings.extend(rules::preview_warnings(&compiled, settings, &store));
    }
    let mut samples: Vec<(String, i64)> = matched.into_values().collect();
    samples.sort_by_key(|(_, at)| std::cmp::Reverse(*at));
    let count = samples.len();
    samples.truncate(SAMPLES);
    DryRun {
        count,
        samples,
        needs_body: compiled.iter().any(|c| c.needs_body()),
        warnings: warnings.iter().map(|w| clean(w, false)).collect(),
        error: None,
    }
}

/// What a dry run's count means, from the rule's first action that changes mail: "38 messages would be archived".
pub(crate) fn would_be(rule: &Rule, count: usize) -> String {
    use postvak_core::rules::Action;
    let verb = rule
        .actions
        .iter()
        .find_map(|action| match action {
            Action::Archive => Some("archived".to_string()),
            Action::Move(folder) => Some(format!(
                "moved to {}",
                clean(super::folders::label(folder), false)
            )),
            Action::Delete => Some("deleted".into()),
            Action::Trash => Some("moved to Trash".into()),
            Action::Junk => Some("moved to Junk".into()),
            Action::NotJunk => Some("marked not junk".into()),
            Action::MarkRead => Some("marked read".into()),
            Action::MarkUnread => Some("marked unread".into()),
            Action::Flag => Some("flagged".into()),
            Action::Unflag => Some("unflagged".into()),
            Action::Tag(keyword) => Some(format!("tagged {}", clean(keyword, false))),
            Action::Notify | Action::Silent | Action::Stop => None,
        })
        .unwrap_or_else(|| "matched".into());
    let noun = super::numbers::noun(count as u64, "message", "messages");
    format!("{noun} would be {verb}")
}

/// How many messages a dry run counted beyond its samples, "35 more", or empty.
pub(crate) fn dry_run_more(run: &DryRun) -> String {
    let more = run.count.saturating_sub(run.samples.len());
    if more == 0 {
        return String::new();
    }
    format!("{} more", super::numbers::count(more as u64))
}

/// The line under a dry run's samples, after `dry_run_more`: that approving is what makes it act.
pub(crate) fn dry_run_rest(run: &DryRun) -> String {
    let more = if dry_run_more(run).is_empty() {
        ""
    } else {
        " · "
    };
    let body = if run.needs_body {
        " Only mail whose body is downloaded is checked."
    } else {
        ""
    };
    format!("{more}Nothing is deleted. Runs on new mail once approved.{body}")
}

/// Who proposed a rule as the card names them: an MCP client or CLI user by name, without the `mcp:` or `cli:`.
pub(crate) fn proposer(by: &str) -> &str {
    by.split_once(':').map_or(by, |(_, who)| who)
}

/// The Last 30 days and Last fired cells of a rule: its count and when it last acted, or that it never has.
pub(crate) fn hit_cells(hits: &Hits) -> (String, String) {
    match hits.last_fired {
        Some(at) => (
            hits.last_30d.to_string(),
            super::list::list_date(at, &chrono::Local::now()),
        ),
        None => ("no matches yet".into(), "—".into()),
    }
}

/// "5 rules · 1 waiting for you", beside the Rules title; the rules alone while no proposal waits.
pub(crate) fn counts(rules: usize, proposals: usize) -> String {
    let rules = super::numbers::counted(rules as u64, "rule", "rules");
    if proposals == 0 {
        return rules;
    }
    format!(
        "{rules} · {} waiting for you",
        super::numbers::count(proposals as u64)
    )
}

/// A run of a proposal's sentence or TOML drawn in one style: one of the `RUN_` kinds.
pub(crate) type Run = (String, i32);
pub(crate) const RUN_PLAIN: i32 = 0;
/// A verb or field of the sentence, in bold.
pub(crate) const RUN_STRONG: i32 = 1;
/// A value of the sentence, as a token in the code face.
pub(crate) const RUN_TOKEN: i32 = 2;
pub(crate) const RUN_KEY: i32 = 3;
pub(crate) const RUN_STRING: i32 = 4;
/// `true`, `false` or a number.
pub(crate) const RUN_LITERAL: i32 = 5;
/// A table heading such as `[[rules]]`.
pub(crate) const RUN_TABLE: i32 = 6;

/// What a rule does, in a sentence: "When mail **from** `news@` arrives in `work / Inbox`, **archive** it and **mark it
/// read**." A condition the sentence has no words for shows as the rules table words it.
pub(crate) fn sentence(rule: &Rule) -> Vec<Run> {
    let plain = |text: &str| (text.to_string(), RUN_PLAIN);
    let mut runs = vec![plain("When mail ")];
    let conditions: Vec<Vec<Run>> = draft::match_labels(&rule.matches)
        .iter()
        .map(|label| condition_runs(&clean(label, false)))
        .collect();
    join(&mut runs, conditions);
    let account = rule
        .account
        .as_deref()
        .map_or("every account".into(), |a| clean(a, false));
    let folder = clean(
        super::folders::label(rule.folder.as_deref().unwrap_or("INBOX")),
        false,
    );
    runs.push(plain(" arrives in "));
    runs.push((format!("{account} / {folder}"), RUN_TOKEN));
    runs.push(plain(", "));
    join(&mut runs, rule.actions.iter().map(action_runs).collect());
    runs.push(plain("."));
    runs
}

/// `parts` one after another: commas between them and "and" before the last.
fn join(runs: &mut Vec<Run>, parts: Vec<Vec<Run>>) {
    let count = parts.len();
    for (i, part) in parts.into_iter().enumerate() {
        if i > 0 {
            let between = if i + 1 == count { " and " } else { ", " };
            runs.push((between.into(), RUN_PLAIN));
        }
        runs.extend(part);
    }
}

/// A condition as the rules table words it, `from contains "news@"`, put in the sentence's words: **from** `news@`.
fn condition_runs(label: &str) -> Vec<Run> {
    let fields = [
        ("body ", "with body"),
        ("cc ", "cc"),
        ("from ", "from"),
        ("subject ", "with subject"),
        ("to ", "to"),
    ];
    for (prefix, said) in fields {
        let value = label
            .strip_prefix(prefix)
            .and_then(|rest| rest.strip_prefix("contains \""))
            .and_then(|rest| rest.strip_suffix('"'));
        if let Some(value) = value {
            return vec![
                (said.into(), RUN_STRONG),
                (" ".into(), RUN_PLAIN),
                (value.into(), RUN_TOKEN),
            ];
        }
    }
    vec![(label.into(), RUN_TOKEN)]
}

/// An action in the sentence's words: its verb in bold, then what it acts on.
fn action_runs(action: &rules::Action) -> Vec<Run> {
    use rules::Action;
    let strong = |text: &str| (text.to_string(), RUN_STRONG);
    let plain = |text: &str| (text.to_string(), RUN_PLAIN);
    let token = |text: String| (text, RUN_TOKEN);
    match action {
        Action::Archive => vec![strong("archive"), plain(" it")],
        Action::Delete => vec![strong("delete"), plain(" it")],
        Action::Flag => vec![strong("flag"), plain(" it")],
        Action::Junk => vec![strong("mark it junk")],
        Action::MarkRead => vec![strong("mark it read")],
        Action::MarkUnread => vec![strong("mark it unread")],
        Action::Move(folder) => vec![
            strong("move"),
            plain(" it to "),
            token(clean(super::folders::label(folder), false)),
        ],
        Action::NotJunk => vec![strong("mark it not junk")],
        Action::Notify => vec![strong("notify"), plain(" you")],
        Action::Silent => vec![strong("leave"), plain(" you unnotified")],
        Action::Stop => vec![strong("stop"), plain(" later rules")],
        Action::Tag(keyword) => vec![strong("tag"), plain(" it "), token(clean(keyword, false))],
        Action::Trash => vec![strong("move"), plain(" it to "), token("Trash".into())],
        Action::Unflag => vec![strong("unflag"), plain(" it")],
    }
}

/// One line of TOML split into what the code block colours: keys, strings, literals and table headings.
pub(crate) fn toml_runs(line: &str) -> Vec<Run> {
    if line.trim_start().starts_with('[') {
        return vec![(line.into(), RUN_TABLE)];
    }
    let chars: Vec<char> = line.chars().collect();
    let mut runs: Vec<Run> = Vec::new();
    let mut push = |text: String, kind: i32| match runs.last_mut() {
        Some(last) if last.1 == kind && kind == RUN_PLAIN => last.0.push_str(&text),
        _ => runs.push((text, kind)),
    };
    let word = |c: char| c.is_alphanumeric() || "_.-+".contains(c);
    let mut i = 0;
    while i < chars.len() {
        let start = i;
        if chars[i] == '"' {
            i += 1;
            while i < chars.len() && chars[i] != '"' {
                i += if chars[i] == '\\' { 2 } else { 1 };
            }
            i = (i + 1).min(chars.len());
            push(chars[start..i].iter().collect(), RUN_STRING);
        } else if word(chars[i]) {
            while i < chars.len() && word(chars[i]) {
                i += 1;
            }
            let text: String = chars[start..i].iter().collect();
            let before_equals = chars[i..].iter().find(|c| !c.is_whitespace()) == Some(&'=');
            let literal = text == "true" || text == "false" || text.parse::<f64>().is_ok();
            let kind = if before_equals {
                RUN_KEY
            } else if literal {
                RUN_LITERAL
            } else {
                RUN_PLAIN
            };
            push(text, kind);
        } else {
            while i < chars.len() && chars[i] != '"' && !word(chars[i]) {
                i += 1;
            }
            push(chars[start..i].iter().collect(), RUN_PLAIN);
        }
    }
    runs
}

/// Whether the list shows `rule` as a proposal rather than a rule.
pub(crate) fn is_proposal(rule: &Rule) -> bool {
    !rule.enabled && rule.proposed_by.is_some()
}

/// The lines above the Rules view when rules.toml does not load, or loads but warns.
pub(crate) fn banner(state: &RulesState) -> Option<String> {
    let error = state.error.as_deref().map(|error| {
        format!(
            "{} — sync keeps the previous rules until this is fixed",
            clean(error, false)
        )
    });
    let lines: Vec<String> = error
        .into_iter()
        .chain(state.warnings.iter().map(|w| clean(w, false)))
        .collect();
    (!lines.is_empty()).then(|| lines.join("\n"))
}

fn proposals(rules: &[Rule]) -> Vec<Proposal> {
    let toml = |rule: &Rule| {
        let toml = rules::rule_toml(rule).unwrap_or_else(|e| e.to_string());
        clean(&toml, true).trim_end().to_owned()
    };
    rules
        .iter()
        .filter(|rule| is_proposal(rule))
        .map(|rule| {
            let by = clean(rule.proposed_by.as_deref().unwrap_or_default(), false);
            let edited = rule
                .replaces
                .as_deref()
                .and_then(|target| rules.iter().find(|r| r.name == target));
            let lines = match edited {
                Some(old) => {
                    // Compared as the rule will read once approved: its own name and `enabled`, no proposal keys.
                    let new = Rule {
                        name: old.name.clone(),
                        enabled: old.enabled,
                        proposed_by: old.proposed_by.clone(),
                        replaces: old.replaces.clone(),
                        ..rule.clone()
                    };
                    diff(&toml(old), &toml(&new))
                }
                None => toml(rule)
                    .lines()
                    .map(|line| ('+', line.to_owned()))
                    .collect(),
            };
            let code = (lines.iter())
                .map(|(mark, line)| (mark.to_string(), toml_runs(line)))
                .collect();
            let (marks, toml): (Vec<String>, Vec<String>) = lines
                .into_iter()
                .map(|(mark, line)| (mark.to_string(), line))
                .unzip();
            let changed = marks.iter().filter(|mark| mark.as_str() != " ").count();
            let lines = super::numbers::noun(changed as u64, "line", "lines");
            let note = match edited {
                Some(old) => format!(
                    "Changes rule '{}' · {changed} {lines}",
                    clean(&old.name, false)
                ),
                None => format!("rules.toml · adds {changed} {lines}"),
            };
            let who = proposer(&by);
            Proposal {
                name: rule.name.clone(),
                label: clean(&rule.name, false),
                by: match edited {
                    Some(_) => format!("Edit proposal · {who}"),
                    None => format!("Agent proposal · {who}"),
                },
                summary: draft::summary(rule),
                sentence: sentence(rule),
                code,
                note,
                edits: edited.is_some(),
                marks: marks.join("\n"),
                toml: toml.join("\n"),
            }
        })
        .collect()
}

/// `old` and `new` line by line, each marked as a diff marks it: ' ' in both, '-' only in `old`, '+' only in `new`.
fn diff(old: &str, new: &str) -> Vec<(char, String)> {
    let (old, new): (Vec<&str>, Vec<&str>) = (old.lines().collect(), new.lines().collect());
    // common[i][j]: the longest run of lines old[i..] and new[j..] share, in order.
    let mut common = vec![vec![0usize; new.len() + 1]; old.len() + 1];
    for i in (0..old.len()).rev() {
        for j in (0..new.len()).rev() {
            common[i][j] = if old[i] == new[j] {
                common[i + 1][j + 1] + 1
            } else {
                common[i + 1][j].max(common[i][j + 1])
            };
        }
    }
    let (mut i, mut j, mut lines) = (0, 0, Vec::new());
    while i < old.len() || j < new.len() {
        if i < old.len() && j < new.len() && old[i] == new[j] {
            lines.push((' ', old[i].to_owned()));
            (i, j) = (i + 1, j + 1);
        } else if i < old.len() && (j == new.len() || common[i + 1][j] >= common[i][j + 1]) {
            lines.push(('-', old[i].to_owned()));
            i += 1;
        } else {
            lines.push(('+', new[j].to_owned()));
            j += 1;
        }
    }
    lines
}

fn rule_lines(rules: &[Rule]) -> Vec<RuleLine> {
    let line = |rule: &Rule| {
        let account = rule
            .account
            .as_deref()
            .map_or("every account".into(), |a| clean(a, false));
        let folder = rule
            .folder
            .as_deref()
            .map_or("Inbox".into(), |f| clean(crate::folders::label(f), false));
        let actions: Vec<String> = rule.actions.iter().map(draft::action_label).collect();
        let watches = format!("{account} · {folder}");
        RuleLine {
            name: rule.name.clone(),
            label: clean(&rule.name, false),
            enabled: rule.enabled,
            account,
            folder,
            by: rule
                .proposed_by
                .as_deref()
                .map(|by| format!("proposed by {}", clean(by, false)))
                .unwrap_or_default(),
            conditions: clean(&draft::match_labels(&rule.matches).join(", "), false),
            actions: clean(&actions.join(", "), false),
            hits: Hits::default(),
            watches,
            deletes: rule.actions.contains(&rules::Action::Delete),
        }
    };
    rules
        .iter()
        .filter(|rule| !is_proposal(rule))
        .map(line)
        .collect()
}

/// A table column: its heading, its narrowest width, and its share of the room left over.
pub(crate) type Column = (&'static str, f32, f32);

pub(crate) const ACTIVITY_COLUMNS: [Column; 6] = [
    ("Time", 130.0, 0.0),
    ("Account", 90.0, 0.0),
    ("Rule", 110.0, 0.0),
    ("Action", 130.0, 0.0),
    ("Folder", 90.0, 0.0),
    ("Subject", 200.0, 1.0),
];
pub(crate) const NO_ACTIVITY: &str = "No rule or action has run yet.";
pub(crate) const TRASH_COLUMNS: [Column; 5] = [
    ("Deleted", 130.0, 0.0),
    ("Account", 90.0, 0.0),
    ("Folder", 90.0, 0.0),
    ("From", 200.0, 1.0),
    ("Subject", 200.0, 1.0),
];
pub(crate) const NO_TRASH: &str =
    "Trash is empty. Deleted mail is kept here as .eml for the account's retention period.";

/// How the Activity or Backups table is sorted: by one column's text, up or down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SheetSort {
    pub column: usize,
    pub ascending: bool,
}

/// The order a table shows `rows` in, as indices into them: their own order (newest first), or by `sort`'s column.
/// The sort is stable, so rows with equal cells keep their own order.
pub(crate) fn sheet_order<T: Ord, const N: usize>(
    rows: &[[T; N]],
    sort: Option<SheetSort>,
) -> Vec<usize> {
    let mut order: Vec<usize> = (0..rows.len()).collect();
    if let Some(SheetSort { column, ascending }) = sort.filter(|sort| sort.column < N) {
        order.sort_by(|&a, &b| {
            let by = rows[a][column].cmp(&rows[b][column]);
            if ascending { by } else { by.reverse() }
        });
    }
    order
}

/// The lines the Rules table shows for `filter`, in its order, as indices into `lines`.
pub(crate) fn shown_rules(lines: &[RuleLine], sort: Option<SheetSort>, filter: &str) -> Vec<usize> {
    (rule_order(lines, sort).into_iter())
        .filter(|&i| lines[i].matches(filter))
        .collect()
}

/// The order the Rules table shows `lines` in, as indices into them. Its columns are #, Name, Watches, When, Then,
/// Last 30 days and Last fired; # is the order they run in, which is also the order without `sort`. The last two sort by
/// their numbers and times, a rule that never fired first.
fn rule_order(lines: &[RuleLine], sort: Option<SheetSort>) -> Vec<usize> {
    match sort {
        Some(SheetSort {
            column: 0,
            ascending: false,
        }) => (0..lines.len()).rev().collect(),
        Some(SheetSort { column, ascending }) if column >= 5 => {
            let keys: Vec<[(u64, i64); 1]> = (lines.iter())
                .map(|l| match column {
                    5 => [(l.hits.last_30d, 0)],
                    _ => [(0, l.hits.last_fired.unwrap_or(i64::MIN))],
                })
                .collect();
            sheet_order(
                &keys,
                Some(SheetSort {
                    column: 0,
                    ascending,
                }),
            )
        }
        Some(sort) if sort.column > 0 => {
            let cells: Vec<[&str; 4]> = (lines.iter())
                .map(|l| [&*l.label, &l.watches, &l.conditions, &l.actions])
                .collect();
            sheet_order(
                &cells,
                Some(SheetSort {
                    column: sort.column - 1,
                    ..sort
                }),
            )
        }
        _ => (0..lines.len()).collect(),
    }
}

/// Whether the Rules table shows the order the rules run in, the only order in which a dragged row has a place.
pub(crate) fn reorderable(sort: Option<SheetSort>) -> bool {
    sort.is_none_or(|sort| sort.column == 0 && sort.ascending)
}

/// What the plain text of a dragged rule row says: `rule:` and its row, so a message row's number cannot pass for one.
pub(crate) fn drag_data(row: usize) -> String {
    format!("rule:{row}")
}

/// The row a drag carries, when it is a rule row of `drag_data`.
pub(crate) fn dragged_row(text: &str) -> Option<usize> {
    text.strip_prefix("rule:")?.parse().ok()
}

/// What dropping the Rules table's row `from` above or `below` its row `to` asks for, rows counted as the table shows
/// them for `filter`; `None` when the table is sorted by anything but run order, a row is missing, or the rule would
/// stay where it is.
pub(crate) fn dropped(
    lines: &[RuleLine],
    sort: Option<SheetSort>,
    filter: &str,
    from: usize,
    to: usize,
    below: bool,
) -> Option<UiAction> {
    let shown = shown_rules(lines, sort, filter);
    let (from, to) = (*shown.get(from)?, *shown.get(to)?);
    let (moved, target) = (lines.get(from)?, lines.get(to)?);
    let stays = from == to || (below && to + 1 == from) || (!below && from + 1 == to);
    if !reorderable(sort) || stays {
        return None;
    }
    Some(UiAction::MoveRule {
        name: moved.name.clone(),
        anchor: target.name.clone(),
        below,
    })
}

/// The Activity view's rows, cell by cell.
pub(crate) fn activity_cells(app: &App) -> Vec<[String; 6]> {
    let row = |(account, entry): &(String, LogEntry)| {
        [
            date_time(entry.at),
            account.clone(),
            clean(&entry.rule_name, false),
            clean(&entry.action, false),
            clean(crate::folders::label(&entry.folder), false),
            entry
                .subject
                .as_deref()
                .map(|subject| clean(subject, false))
                .unwrap_or_default(),
        ]
    };
    app.activity_log.iter().map(row).collect()
}

/// The Backups view's rows, cell by cell.
pub(crate) fn trash_cells(app: &App) -> Vec<[String; 5]> {
    let row = |row: &TrashRow| {
        [
            date_time(row.entry.saved_at),
            app.accounts[row.account].name.clone(),
            clean(crate::folders::label(&row.entry.folder), false),
            clean(&row.from, false),
            clean(&row.subject, false),
        ]
    };
    app.trash.iter().map(row).collect()
}

pub(crate) struct TrashRow {
    pub account: usize,
    pub entry: TrashEntry,
    pub from: String,
    pub subject: String,
}

/// Every account's rule and action log, newest first.
pub(crate) fn activity_log(accounts: &[Account]) -> Vec<(String, LogEntry)> {
    let mut entries = Vec::new();
    for account in accounts {
        let Ok(store) = &account.store else { continue };
        match store.log(LOG_LIMIT) {
            Ok(found) => {
                entries.extend(found.into_iter().map(|entry| (account.name.clone(), entry)));
            }
            Err(e) => log::warn!("[{}] could not read the log: {e}", account.name),
        }
    }
    entries.sort_by_key(|(_, entry)| std::cmp::Reverse(entry.at));
    entries.truncate(LOG_LIMIT as usize);
    entries
}

/// Every account's `.eml` backups, newest first, with sender and subject from their headers. The view reloads on every
/// sync event, so a backup in `known` keeps the sender and subject read before: a backup is never rewritten, and only
/// new files are opened.
pub(crate) fn trash_rows(accounts: &[Account], paths: &Paths, known: &[TrashRow]) -> Vec<TrashRow> {
    let known: HashMap<&Path, &TrashRow> = known
        .iter()
        .map(|row| (row.entry.path.as_path(), row))
        .collect();
    let mut rows = Vec::new();
    for (index, account) in accounts.iter().enumerate() {
        match Trash::new(paths.trash_dir(&account.name)).list() {
            Ok(entries) => rows.extend(entries.into_iter().map(|entry| {
                let (from, subject) = match known.get(entry.path.as_path()) {
                    Some(row) => (row.from.clone(), row.subject.clone()),
                    None => {
                        let headers = entry.headers();
                        (
                            headers.from.unwrap_or_default(),
                            headers.subject.unwrap_or_default(),
                        )
                    }
                };
                TrashRow {
                    account: index,
                    entry,
                    from,
                    subject,
                }
            })),
            Err(e) => log::warn!("[{}] could not list the trash: {e}", account.name),
        }
    }
    rows.sort_by_key(|row| std::cmp::Reverse(row.entry.saved_at));
    rows
}

/// The Purge button's hover text: what it does, and each account's retention.
pub(crate) fn purge_hint(app: &App) -> String {
    let days: Vec<String> = app
        .retention
        .iter()
        .map(|(name, days)| format!("{name}: {days} days"))
        .collect();
    format!("{} ({})", help::TRASH_PURGE, days.join(", "))
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::{Duration, SystemTime};

    use crate::state::View;
    use crate::test_support::{Fixture, Open, Wires, message};
    use postvak_core::store::LogEntry;
    use postvak_core::sync::{Command, Event};
    use postvak_core::trash::Trash;
    use slint::language::StandardListViewItem;
    use slint::{Model, ModelRc};

    const RULES: &str = "# keep me\n[[rules]]\nname = \"newsletters\"\nmatch.from = { contains = \"news@\" }\nactions = { archive = true }\n\n[[rules]]\nname = \"codes\"\nenabled = false\nproposed_by = \"agent\"\nmatch.subject = { contains = \"code\" }\nactions = { delete = true }\n";

    fn touch(path: &Path) {
        let file = if path.is_dir() {
            std::fs::File::open(path).unwrap()
        } else {
            std::fs::File::options().write(true).open(path).unwrap()
        };
        file.set_modified(SystemTime::now() + Duration::from_secs(5))
            .unwrap();
    }

    fn view(fx: &Fixture, view: View) -> (Open, Wires) {
        let (open, wires) = fx.window();
        open.with(|live| live.app.select_view(view));
        open.tick();
        (open, wires)
    }

    fn rules_view(fx: &Fixture) -> (Open, Wires) {
        std::fs::write(fx.paths.rules_file(), RULES).unwrap();
        view(fx, View::Rules)
    }

    fn enabled(fx: &Fixture, name: &str) -> bool {
        let file = postvak_core::rules::load(&fx.paths.rules_file()).unwrap();
        file.rules.iter().find(|r| r.name == name).unwrap().enabled
    }

    #[test]
    fn the_authserv_id_the_inbox_offers_is_trusted_with_one_click() {
        let fx = Fixture::new(&["work"]);
        for uid in 1..=postvak_core::message::auth::SUGGEST_MIN as u32 {
            fx.add(
                "work",
                postvak_core::store::Message {
                    headers: b"Authentication-Results: mx.google.com; dmarc=pass\r\n\r\n".to_vec(),
                    ..message("INBOX", uid, "hello")
                },
            );
        }
        std::fs::write(
            fx.paths.rules_file(),
            "[[rules]]\nname = \"paypal\"\nmatch.from_domain = \"paypal.com\"\nmatch.authenticated = true\nactions.flagged = true\n",
        )
        .unwrap();
        let (open, _wires) = view(&fx, View::Rules);
        assert_eq!(
            open.labels_containing("`postvak account trust work mx.google.com`")
                .len(),
            1
        );
        open.click("Trust mx.google.com for work");
        open.tick();
        assert!(open.find("Trust mx.google.com for work").is_empty());
        assert!(open.labels_containing("trusts no server").is_empty());
        let config = postvak_core::config::Config::load(&fx.paths.config_file()).unwrap();
        assert!(config.accounts[0].can_authenticate());
        assert!(
            !open.with(|live| live.app.config_changed),
            "its own write is no outside edit"
        );
    }

    #[test]
    fn apply_asks_before_a_rule_expunges_mail() {
        let fx = Fixture::new(&["work"]);
        fx.add(
            "work",
            crate::test_support::message("INBOX", 1, "Your code"),
        );
        std::fs::write(
            fx.paths.rules_file(),
            "[[rules]]\nname = \"codes\"\nmatch.subject = { contains = \"code\" }\nactions = { delete = true }\n",
        )
        .unwrap();
        let (open, wires) = view(&fx, View::Rules);
        let warning = "work: would permanently delete 1 message from the server; .eml backups kept locally for 30 days";
        open.click("codes");
        open.click("Apply to existing mail");
        open.tick();
        assert!(wires.sent().is_empty(), "sent before confirming");
        assert_eq!(open.find(warning).len(), 1);
        open.click("Cancel");
        open.tick();
        assert!(open.find(warning).is_empty());
        assert!(wires.sent().is_empty(), "cancel sent it");
        open.click("Apply to existing mail");
        open.tick();
        open.click("Delete and apply");
        open.tick();
        assert!(open.find(warning).is_empty());
        assert_eq!(
            wires.sent(),
            [(
                "work".to_string(),
                Command::ApplyRule {
                    name: "codes".into()
                }
            )]
        );
    }

    #[test]
    fn apply_runs_an_enabled_rule_on_its_accounts_and_shows_the_outcome() {
        let fx = Fixture::new(&["home", "work"]);
        let only_work = "\n[[rules]]\nname = \"only-work\"\naccount = \"work\"\nmatch.subject = { contains = \"x\" }\nactions = { flagged = true }\n";
        std::fs::write(fx.paths.rules_file(), format!("{RULES}{only_work}")).unwrap();
        let (open, wires) = view(&fx, View::Rules);
        let apply = |rule: &str| {
            open.click(rule);
            open.click("Apply to existing mail");
            open.press("escape");
        };
        apply("only-work");
        let only = |name: &str, account: &str| {
            (
                account.to_string(),
                Command::ApplyRule { name: name.into() },
            )
        };
        assert_eq!(wires.sent(), [only("only-work", "work")]);
        apply("newsletters");
        assert_eq!(
            wires.sent(),
            [only("newsletters", "home"), only("newsletters", "work")]
        );
        let applied = |errors: Vec<String>| Event::RuleApplied {
            account: "work".into(),
            request: 7,
            evaluated: 12,
            actions: 3,
            errors,
        };
        open.receive(&wires, [applied(Vec::new())]);
        assert_eq!(open.find("work: 3 actions on 12 messages").len(), 1);
        wires
            .events
            .send(applied(vec!["INBOX/4: gone".into()]))
            .unwrap();
        open.tick();
        assert_eq!(
            open.find("work: INBOX/4: gone").len(),
            1,
            "a failure wins the line"
        );
    }

    #[test]
    fn a_rule_for_an_account_the_window_does_not_list_says_so() {
        let fx = Fixture::new(&["work"]);
        let gone = "[[rules]]\nname = \"old\"\naccount = \"gone\"\nmatch.subject = { contains = \"x\" }\nactions = { flagged = true }\n";
        std::fs::write(fx.paths.rules_file(), gone).unwrap();
        let (open, wires) = view(&fx, View::Rules);
        open.click("old");
        open.click("Apply to existing mail");
        assert!(wires.sent().is_empty());
        let why = "rule 'old' names account 'gone', which this window does not list";
        assert_eq!(
            open.with(|live| live.app.error.clone()).as_deref(),
            Some(why)
        );
    }

    #[test]
    fn one_action_on_one_message_is_worded_singular() {
        let fx = Fixture::new(&["work"]);
        let (open, wires) = rules_view(&fx);
        let applied = Event::RuleApplied {
            account: "work".into(),
            request: 1,
            evaluated: 1,
            actions: 1,
            errors: vec![],
        };
        open.receive(&wires, [applied]);
        assert_eq!(open.find("work: 1 action on 1 message").len(), 1);
    }

    #[test]
    fn purge_removes_backups_past_their_retention_and_says_how_many() {
        let fx = Fixture::new(&["work"]);
        let dir = fx.paths.trash_dir("work");
        std::fs::create_dir_all(&dir).unwrap();
        let trash = Trash::new(dir);
        let now = postvak_core::time::now();
        let raw = |subject: &str| format!("From: a@example.com\r\nSubject: {subject}\r\n\r\nx\r\n");
        let old = trash
            .save("INBOX", 1, raw("old").as_bytes(), now - 31 * 86_400)
            .unwrap();
        let fresh = trash
            .save("INBOX", 2, raw("fresh").as_bytes(), now - 86_400)
            .unwrap();
        let (open, _wires) = view(&fx, View::Trash);
        assert_eq!(
            open.get("Purge").accessible_description().unwrap(),
            "Remove .eml backups older than the account's retention period (work: 30 days)"
        );
        open.click("Purge");
        assert!(!old.exists());
        assert!(fresh.exists());
        assert!(open.find("old").is_empty());
        assert_eq!(open.find("fresh").len(), 1);
        let line = "removed 1 backup older than the retention period";
        assert_eq!(open.find(line).len(), 1, "the status row says it");
        open.with(|live| live.app.apply(crate::state::UiAction::ToggleHistory));
        assert!(open.find(line).is_empty(), "opening the history clears it");
    }

    #[test]
    fn a_failing_account_is_named_and_the_others_are_still_purged() {
        let fx = Fixture::new(&["broken", "work"]);
        std::fs::write(fx.paths.trash_dir("broken"), "not a directory").unwrap();
        let dir = fx.paths.trash_dir("work");
        std::fs::create_dir_all(&dir).unwrap();
        let now = postvak_core::time::now();
        let old = Trash::new(dir)
            .save(
                "INBOX",
                1,
                b"From: a@example.com\r\n\r\nx\r\n",
                now - 31 * 86_400,
            )
            .unwrap();
        let (open, _wires) = view(&fx, View::Trash);
        open.with(|live| live.app.apply(crate::state::UiAction::PurgeTrash));
        assert!(!old.exists());
        open.with(|live| {
            let error = live.app.error.clone().unwrap();
            assert!(error.starts_with("could not purge: broken: "), "{error}");
            assert!(!error.contains("work"), "{error}");
            let notice = live.app.notice.clone().unwrap();
            assert_eq!(
                notice,
                "removed 1 backup older than the retention period; 1 account failed"
            );
        });
    }

    #[test]
    fn a_proposal_shows_its_toml_and_approve_enables_it() {
        let fx = Fixture::new(&["work"]);
        // A clock left from when the rule ran before must not let it act on older mail.
        fx.store("work").restart_rule_clock("codes", 1).unwrap();
        let (open, wires) = rules_view(&fx);
        assert_eq!(open.find("AGENT PROPOSAL · AGENT").len(), 1);
        assert_eq!(
            open.find("When mail with subject code arrives in every account / Inbox, delete it.")
                .len(),
            1
        );
        assert_eq!(open.labels_containing("name = \"codes\"").len(), 1);
        open.click("Approve rule");
        assert!(enabled(&fx, "codes"));
        assert!(fx.store("work").rule_first_seen("codes", "", 0).unwrap() > 1);
        assert!(
            std::fs::read_to_string(fx.paths.rules_file())
                .unwrap()
                .starts_with("# keep me\n")
        );
        assert!(wires.sent().is_empty());
        assert!(open.find("Approve rule").is_empty());
    }

    const EDIT: &str = "[[rules]]\nname = \"newsletters\"\nmatch.from = { contains = \"news@\" }\nactions = { archive = true }\n\n[[rules]]\nname = \"newsletters (edit)\"\nenabled = false\nproposed_by = \"agent\"\nreplaces = \"newsletters\"\nmatch.from = { contains = \"news@\" }\nactions = { read = true }\n";

    #[test]
    fn an_edit_shows_what_it_changes_in_its_rule() {
        let rules = postvak_core::rules::parse(EDIT).unwrap().rules;
        let edit = &super::proposals(&rules)[0];
        assert_eq!(edit.by, "Edit proposal · agent");
        assert_eq!(edit.note, "Changes rule 'newsletters' · 2 lines");
        assert_eq!(
            edit.toml,
            "[[rules]]\nname = \"newsletters\"\nmatch.from = { contains = \"news@\" }\nactions.archive = true\nactions.read = true"
        );
        assert_eq!(edit.marks, " \n \n \n-\n+");
        let new = &super::proposals(&postvak_core::rules::parse(RULES).unwrap().rules)[0];
        assert_eq!(
            new.note,
            format!("rules.toml · adds {} lines", new.toml.lines().count())
        );
        assert_eq!(new.marks, vec!["+"; new.toml.lines().count()].join("\n"));
    }

    #[test]
    fn approving_an_edit_changes_its_rule() {
        let fx = Fixture::new(&["work"]);
        std::fs::write(fx.paths.rules_file(), EDIT).unwrap();
        let (open, _wires) = view(&fx, View::Rules);
        assert_eq!(open.find("Changes rule 'newsletters' · 2 lines").len(), 1);
        open.click("Approve rule");
        let file = postvak_core::rules::load(&fx.paths.rules_file()).unwrap();
        assert_eq!(file.rules.len(), 1);
        assert_eq!(file.rules[0].name, "newsletters");
        assert_eq!(
            file.rules[0].actions,
            vec![postvak_core::rules::Action::MarkRead]
        );
    }

    #[test]
    fn the_header_counts_rules_and_proposals_and_rows_say_what_they_do() {
        assert_eq!(super::counts(1, 2), "1 rule · 2 waiting for you");
        assert_eq!(super::counts(5, 0), "5 rules");
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = rules_view(&fx);
        assert_eq!(open.find("1 rule · 1 waiting for you").len(), 1);
        assert_eq!(open.find("every account · Inbox").len(), 1);
        assert_eq!(open.find("from contains \"news@\"").len(), 1);
        assert_eq!(open.find("archive").len(), 1);
        let folders: Vec<String> = open.with(|live| {
            live.app
                .rules
                .lines
                .iter()
                .map(|l| l.folder.clone())
                .collect()
        });
        assert!(folders.iter().all(|f| f == "Inbox"), "{folders:?}");
    }

    #[test]
    fn a_filter_matches_every_word_anywhere_in_a_row_ignoring_case() {
        let texts = ["Newsletters", "work", "INBOX", "from contains \"news@\""];
        assert!(super::matches_filter(&texts, ""));
        assert!(super::matches_filter(&texts, "  NEWS  work "));
        assert!(super::matches_filter(&texts, "inbox news@"));
        assert!(!super::matches_filter(&texts, "news home"));
        assert_eq!(super::no_rules(" "), "rules.toml has no rules yet.");
        assert_eq!(super::no_rules(" spam "), "No rule matches “spam”.");
    }

    #[test]
    fn the_filter_field_narrows_rules_and_proposals_and_leaving_the_view_clears_it() {
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = rules_view(&fx);
        let shown = |open: &Open| {
            let window = open.window();
            (
                window.get_rules().row_count(),
                window.get_proposals().row_count(),
            )
        };
        assert_eq!(shown(&open), (1, 1));
        open.get("Filter rules").set_accessible_value("CODE");
        open.tick();
        assert_eq!(shown(&open), (0, 1));
        assert_eq!(open.find("No rule matches “CODE”.").len(), 1);
        assert_eq!(
            open.find("1 rule · 1 waiting for you").len(),
            1,
            "the header still counts them all"
        );
        open.get("Filter rules").set_accessible_value("archive");
        open.tick();
        assert_eq!(shown(&open), (1, 0));
        open.tap("newsletters");
        let opened = open.with(|live| live.app.rule_draft.as_ref()?.original.clone());
        assert_eq!(opened.as_deref(), Some("newsletters"));
        open.with(|live| live.app.select_view(View::Rules));
        open.tick();
        assert_eq!(shown(&open), (1, 1));
    }

    /// The first proposal card's dry run: its count, what the count means, and its sample subjects.
    fn dry_run_shown(open: &Open) -> (String, String, Vec<String>) {
        let proposals = open.window().get_proposals();
        let Some(card) = proposals.row_data(0) else {
            return Default::default();
        };
        let samples = (0..card.samples.row_count())
            .filter_map(|i| card.samples.row_data(i))
            .map(|s| s.subject.to_string())
            .collect();
        (
            card.count.to_string(),
            card.count_label.to_string(),
            samples,
        )
    }

    #[test]
    fn a_proposal_counts_its_dry_run_without_a_click() {
        let fx = Fixture::new(&["work"]);
        fx.add("work", message("INBOX", 1, "your code is 1234"));
        fx.add("work", message("INBOX", 2, "lunch"));
        let (open, wires) = rules_view(&fx);
        open.tick_until(|open| dry_run_shown(open).0 == "1");
        assert_eq!(
            dry_run_shown(&open),
            (
                "1".to_string(),
                "message would be deleted".to_string(),
                vec!["your code is 1234".to_string()]
            )
        );
        assert!(open.find("Preview on cached mail").is_empty());
        assert!(wires.sent().is_empty());
        assert!(!enabled(&fx, "codes"), "a dry run changes nothing");
    }

    #[test]
    fn a_dry_run_warns_when_authenticated_cannot_hold_on_an_account() {
        let fx = Fixture::new(&["work"]);
        std::fs::write(
            fx.paths.rules_file(),
            "[[rules]]\nname = \"bank\"\nenabled = false\nproposed_by = \"agent\"\nmatch.authenticated = true\nactions = { flagged = true }\n",
        )
        .unwrap();
        let (open, _wires) = view(&fx, View::Rules);
        // The view's warnings above the cards say it too, at once; wait for the card's own dry run.
        open.tick_until(|open| dry_run_shown(open).0 == "0");
        let card = open.window().get_proposals().row_data(0).unwrap();
        assert!(
            card.warnings
                .contains("rule 'bank' uses authenticated, but account 'work' trusts no server's"),
            "{}",
            card.warnings
        );
    }

    #[test]
    fn a_failed_dry_run_shows_why_and_leaves_approve_enabled() {
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = fx.window();
        std::fs::write(fx.paths.config_file(), "not toml [").unwrap();
        std::fs::write(fx.paths.rules_file(), RULES).unwrap();
        touch(&fx.paths.rules_file());
        open.with(|live| live.app.select_view(View::Rules));
        open.advance(5.0);
        open.tick_until(|open| {
            let proposals = open.window().get_proposals();
            proposals
                .row_data(0)
                .is_some_and(|card| !card.error.is_empty())
        });
        // Approving reads config.toml too, so here it would fail on its own; the dry run must not be what blocks it.
        assert_eq!(open.get("Approve rule").accessible_enabled(), Some(true));
    }

    #[test]
    fn a_store_that_will_not_open_fails_the_dry_run_rather_than_undercounting() {
        let fx = Fixture::new(&["work"]);
        std::fs::write(fx.paths.mail_db("work"), "not a database, not even close").unwrap();
        let rule = postvak_core::rules::parse(RULES).unwrap().rules.remove(1);
        let run = super::dry_run(&fx.paths, &["work".to_string()], &rule, 0);
        assert!(
            run.error
                .as_deref()
                .is_some_and(|e| e.starts_with("work: ")),
            "{run:?}"
        );
    }

    #[test]
    fn edit_before_approving_opens_the_proposal_in_the_dialog() {
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = rules_view(&fx);
        open.click("Edit before approving →");
        let opened = open.with(|live| {
            live.app
                .rule_draft
                .as_ref()
                .and_then(|d| d.original.clone())
        });
        assert_eq!(opened.as_deref(), Some("codes"));
    }

    #[test]
    fn agent_text_is_drawn_without_control_characters() {
        let sneaky = "[[rules]]\nname = \"codes\"\nenabled = false\nproposed_by = \"age\\u001bnt\"\nmatch.subject = { contains = \"\\u001b[2Jcode\" }\nactions = { delete = true }\n";
        let fx = Fixture::new(&["work"]);
        std::fs::write(fx.paths.rules_file(), sneaky).unwrap();
        let (open, _wires) = view(&fx, View::Rules);
        assert_eq!(open.find("AGENT PROPOSAL · AGENT").len(), 1);
        assert_eq!(
            open.labels_containing("with subject [2Jcode arrives").len(),
            1
        );
        assert!(open.labels_containing("\u{1b}").is_empty());
    }

    #[test]
    fn reject_takes_the_proposal_out() {
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = rules_view(&fx);
        open.click("Reject");
        let file = postvak_core::rules::load(&fx.paths.rules_file()).unwrap();
        assert!(file.rules.iter().all(|r| r.name != "codes"));
        assert!(open.find("Approve rule").is_empty());
    }

    #[test]
    fn the_switch_turns_a_rule_off_and_keeps_comments() {
        let fx = Fixture::new(&["work"]);
        let (open, wires) = rules_view(&fx);
        open.get("Enable newsletters")
            .invoke_accessible_default_action();
        assert!(!enabled(&fx, "newsletters"));
        assert!(
            std::fs::read_to_string(fx.paths.rules_file())
                .unwrap()
                .starts_with("# keep me\n")
        );
        assert!(wires.sent().is_empty());
    }

    #[test]
    fn open_rules_toml_opens_it_in_the_text_editor() {
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = rules_view(&fx);
        open.click("Open rules.toml");
        let path = fx.paths.rules_file().to_string_lossy().into_owned();
        assert_eq!(crate::test_support::launched(), [path]);
    }

    #[test]
    fn an_edit_from_outside_reloads_the_view() {
        let fx = Fixture::new(&["work"]);
        let (open, wires) = rules_view(&fx);
        let more = format!(
            "{RULES}\n[[rules]]\nname = \"receipts\"\nmatch.subject = {{ contains = \"receipt\" }}\nactions = {{ move = \"Receipts\" }}\n"
        );
        std::fs::write(fx.paths.rules_file(), more).unwrap();
        touch(&fx.paths.rules_file());
        open.advance(5.0);
        assert_eq!(open.find("receipts").len(), 1);
        assert!(wires.sent().is_empty());
    }

    #[test]
    fn a_parse_error_shows_a_banner_and_keeps_the_rules() {
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = rules_view(&fx);
        std::fs::write(fx.paths.rules_file(), "[[rules]\nbroken").unwrap();
        touch(&fx.paths.rules_file());
        open.advance(5.0);
        assert_eq!(
            open.labels_containing("sync keeps the previous rules")
                .len(),
            1
        );
        assert_eq!(open.find("newsletters").len(), 1);
    }

    #[test]
    fn a_conflicting_copy_and_a_rule_for_a_missing_account_show_in_the_banner() {
        let fx = Fixture::new(&["work"]);
        let gone = "[[rules]]\nname = \"old\"\naccount = \"gone\"\nmatch.read = true\nactions = { flagged = true }\n";
        std::fs::write(fx.paths.rules_file(), format!("{RULES}{gone}")).unwrap();
        let (open, _wires) = view(&fx, View::Rules);
        assert_eq!(open.labels_containing("account 'gone'").len(), 1);
        assert!(open.labels_containing("conflicting copy").is_empty());
        // Syncthing adds the copy without touching rules.toml.
        let copy = fx
            .paths
            .rules_file()
            .with_file_name("rules.sync-conflict-20261024-101500-ABCDEFG.toml");
        std::fs::write(copy, "").unwrap();
        touch(&fx.paths.config_dir);
        open.advance(5.0);
        let banner = open.labels_containing("conflicting copy");
        assert_eq!(banner.len(), 1);
        assert!(banner[0].contains("account 'gone'"), "{banner:?}");
        assert_eq!(open.find("newsletters").len(), 1);
    }

    /// The folder warnings read the store, which holds the account's folders only once it synced.
    #[test]
    fn a_folder_warning_shows_once_a_sync_brought_the_folders() {
        let fx = Fixture::new(&["work"]);
        let rule = "[[rules]]\nname = \"gh\"\nmatch.read = true\nactions.move = \"Lists/GitHub\"\n";
        std::fs::write(fx.paths.rules_file(), rule).unwrap();
        let (open, wires) = view(&fx, View::Rules);
        assert!(open.labels_containing("'Lists/GitHub'").is_empty());
        let store = postvak_core::store::Store::open_account(&fx.paths, "work").unwrap();
        store
            .upsert_folder(&postvak_core::store::Folder {
                name: "INBOX".into(),
                uidvalidity: 1,
                last_uid: 0,
                special_use: None,
                delimiter: Some(".".into()),
            })
            .unwrap();
        let synced = Event::Synced {
            account: "work".into(),
            new_messages: 0,
            actions: 0,
            requests: vec![],
            errors: vec![],
        };
        wires.events.send(synced).unwrap();
        open.tick();
        let banner = open.labels_containing("'Lists/GitHub'");
        assert_eq!(banner.len(), 1, "{banner:?}");
        assert!(banner[0].contains("write 'Lists.GitHub'"), "{banner:?}");
    }

    #[test]
    fn a_config_change_asks_to_reopen_the_window() {
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = fx.window();
        touch(&fx.paths.config_file());
        open.advance(5.0);
        assert_eq!(
            open.find("config.toml changed — reopen the window to show it")
                .len(),
            1
        );
    }

    #[test]
    fn the_theme_switch_raises_no_banner() {
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = fx.window();
        open.window()
            .invoke_menu_item(crate::ui::MenuAction::ThemeDark);
        open.advance(5.0);
        assert!(open.labels_containing("reopen the window").is_empty());
    }

    fn logged(fx: &Fixture, account: &str, at: i64, subject: &str) {
        let entry = LogEntry {
            id: 0,
            at,
            rule_name: "newsletters".into(),
            folder: "INBOX".into(),
            uid: 1,
            message_id: None,
            subject: Some(subject.into()),
            action: "archive".into(),
            trash_file: None,
        };
        fx.store(account).log_action(&entry).unwrap();
    }

    fn backup(fx: &Fixture) -> std::path::PathBuf {
        let dir = fx.paths.trash_dir("work");
        std::fs::create_dir_all(&dir).unwrap();
        let raw = b"From: Shop <shop@example.com>\r\nSubject: your receipt\r\n\r\nthanks\r\n";
        Trash::new(dir)
            .save("INBOX", 7, raw, 1_790_000_000)
            .unwrap()
    }

    #[test]
    fn activity_merges_every_account_newest_first() {
        let fx = Fixture::new(&["home", "work"]);
        logged(&fx, "work", 100, "work old");
        logged(&fx, "home", 200, "home new");
        logged(&fx, "work", 300, "work newest");
        let (open, _wires) = view(&fx, View::Activity);
        let order: Vec<(String, Option<String>)> = open.with(|live| {
            live.app
                .activity_log
                .iter()
                .map(|(account, e)| (account.clone(), e.subject.clone()))
                .collect()
        });
        let expected = [
            ("work", "work newest"),
            ("home", "home new"),
            ("work", "work old"),
        ];
        let expected: Vec<(String, Option<String>)> = expected
            .iter()
            .map(|(a, s)| (a.to_string(), Some(s.to_string())))
            .collect();
        assert_eq!(order, expected);
        assert_eq!(open.find("work newest").len(), 1);
        assert_eq!(open.find("Subject").len(), 1);
    }

    #[test]
    fn the_activity_table_names_the_inbox_as_the_folder_pane_does() {
        let fx = Fixture::new(&["home"]);
        logged(&fx, "home", 100, "hello");
        let (open, _wires) = view(&fx, View::Activity);
        let cells = open.window().get_sheet_cells();
        let folder = cells.row_data(0).unwrap().row_data(4).unwrap().text;
        assert_eq!(folder, "Inbox");
    }

    #[test]
    fn a_click_on_a_heading_sorts_the_table_and_a_second_click_reverses_it() {
        let fx = Fixture::new(&["home", "work"]);
        logged(&fx, "work", 100, "b old");
        logged(&fx, "home", 200, "c new");
        logged(&fx, "work", 300, "a newest");
        let (open, _wires) = view(&fx, View::Activity);
        let subjects = || -> Vec<String> {
            let cells = open.window().get_sheet_cells();
            let row = |r: ModelRc<StandardListViewItem>| r.row_data(5).unwrap().text.to_string();
            cells.iter().map(row).collect()
        };
        assert_eq!(subjects(), ["a newest", "c new", "b old"]);
        open.tap("Subject");
        assert_eq!(subjects(), ["a newest", "b old", "c new"]);
        open.tap("Subject");
        assert_eq!(subjects(), ["c new", "b old", "a newest"]);
        open.with(|live| live.app.select_view(View::Activity));
        open.tick();
        assert_eq!(
            subjects(),
            ["a newest", "c new", "b old"],
            "a view opens unsorted"
        );
    }

    #[test]
    fn a_drop_moves_a_rule_only_in_run_order_and_only_when_its_place_changes() {
        use super::UiAction;
        let line = |name: &str| super::RuleLine {
            name: name.into(),
            label: name.into(),
            enabled: true,
            account: String::new(),
            folder: String::new(),
            by: String::new(),
            conditions: String::new(),
            actions: String::new(),
            hits: Default::default(),
            watches: String::new(),
            deletes: false,
        };
        let lines = ["a", "b", "c"].map(line);
        let moved = |from, to, below| super::dropped(&lines, None, "", from, to, below);
        let action = |name: &str, anchor: &str, below| {
            Some(UiAction::MoveRule {
                name: name.into(),
                anchor: anchor.into(),
                below,
            })
        };
        assert_eq!(moved(2, 0, false), action("c", "a", false));
        assert_eq!(moved(0, 2, true), action("a", "c", true));
        assert_eq!(moved(0, 1, true), action("a", "b", true));
        for (from, to, below) in [(1, 1, true), (1, 0, true), (1, 2, false), (1, 9, false)] {
            assert_eq!(
                moved(from, to, below),
                None,
                "{from} on {to}, below {below}"
            );
        }
        let by = |column, ascending| Some(super::SheetSort { column, ascending });
        assert!(super::reorderable(by(0, true)));
        for sort in [by(0, false), by(1, true)] {
            assert_eq!(
                super::dropped(&lines, sort, "", 2, 0, false),
                None,
                "{sort:?}"
            );
        }
        assert_eq!(
            super::dropped(&lines, None, "c", 0, 0, false),
            None,
            "the filter shows c alone"
        );
        let filtered = ["x a", "x b", "y", "x c"].map(line);
        assert_eq!(
            super::dropped(&filtered, None, "x", 2, 0, false),
            action("x c", "x a", false),
            "rows count as the filtered table shows them"
        );
        assert_eq!(super::dragged_row(&super::drag_data(4)), Some(4));
        assert_eq!(super::dragged_row("4"), None, "a message row");
    }

    #[test]
    fn a_proposal_reads_as_a_sentence_with_its_values_as_tokens() {
        let rule = postvak_core::rules::parse(
            "[[rules]]\nname = \"kettle\"\naccount = \"work\"\nmatch.from = { contains = \"news@kettle.example\" }\nactions = { archive = true, read = true }\n",
        )
        .unwrap()
        .rules
        .remove(0);
        let runs = super::sentence(&rule);
        let text: String = runs.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(
            text,
            "When mail from news@kettle.example arrives in work / Inbox, archive it and mark it read."
        );
        assert!(runs.contains(&("news@kettle.example".to_string(), super::RUN_TOKEN)));
        assert!(runs.contains(&("from".to_string(), super::RUN_STRONG)));
        assert!(runs.contains(&("mark it read".to_string(), super::RUN_STRONG)));
    }

    #[test]
    fn toml_lines_split_into_keys_strings_and_literals() {
        use super::{RUN_KEY, RUN_LITERAL, RUN_PLAIN, RUN_STRING, RUN_TABLE};
        let run = |t: &str, k| (t.to_string(), k);
        assert_eq!(super::toml_runs("[[rules]]"), [run("[[rules]]", RUN_TABLE)]);
        assert_eq!(
            super::toml_runs("match.from = { contains = \"a\\\"b\" }"),
            [
                run("match.from", RUN_KEY),
                run(" = { ", RUN_PLAIN),
                run("contains", RUN_KEY),
                run(" = ", RUN_PLAIN),
                run("\"a\\\"b\"", RUN_STRING),
                run(" }", RUN_PLAIN),
            ]
        );
        assert_eq!(
            super::toml_runs("actions.read = true"),
            [
                run("actions.read", RUN_KEY),
                run(" = ", RUN_PLAIN),
                run("true", RUN_LITERAL)
            ]
        );
    }

    #[test]
    fn the_hit_columns_sort_by_number_and_time_not_text() {
        use postvak_core::rules::hits::Hits;
        let line = |name: &str, last_30d, last_fired| super::RuleLine {
            name: name.into(),
            label: name.into(),
            enabled: true,
            account: String::new(),
            folder: String::new(),
            by: String::new(),
            conditions: String::new(),
            actions: String::new(),
            hits: Hits {
                last_30d,
                last_fired,
            },
            watches: String::new(),
            deletes: false,
        };
        let lines = [
            line("ten", 10, Some(200)),
            line("nine", 9, Some(1_000)),
            line("never", 0, None),
        ];
        let by = |column| {
            Some(super::SheetSort {
                column,
                ascending: true,
            })
        };
        assert_eq!(super::shown_rules(&lines, by(5), ""), [2, 1, 0]);
        assert_eq!(super::shown_rules(&lines, by(6), ""), [2, 0, 1]);
    }

    #[test]
    fn the_table_shows_hits_and_last_fired() {
        let fx = Fixture::new(&["work"]);
        std::fs::write(
            fx.paths.rules_file(),
            "[[rules]]\nname = \"newsletters\"\nmatch.from = { contains = \"news@\" }\nactions = { archive = true }\n\n[[rules]]\nname = \"quiet\"\nmatch.from = { contains = \"x@\" }\nactions = { archive = true }\n",
        )
        .unwrap();
        fx.store("work")
            .log_action(&LogEntry {
                id: 0,
                at: postvak_core::time::now(),
                rule_name: "newsletters".into(),
                folder: "INBOX".into(),
                uid: 1,
                message_id: None,
                subject: None,
                action: "archive".into(),
                trash_file: None,
            })
            .unwrap();
        let (open, _wires) = view(&fx, View::Rules);
        let cells = |open: &Open| {
            let rules = open.window().get_rules();
            (0..rules.row_count())
                .filter_map(|i| rules.row_data(i))
                .map(|r| (r.label.to_string(), r.hits.to_string(), r.fired.to_string()))
                .collect::<Vec<_>>()
        };
        open.tick_until(|open| cells(open).first().is_some_and(|c| c.1 == "1"));
        let shown = cells(&open);
        assert_ne!(shown[0].2, "—", "newsletters fired today");
        assert_eq!(
            shown[1],
            (
                "quiet".to_string(),
                "no matches yet".to_string(),
                "—".to_string()
            )
        );
    }

    #[test]
    fn a_sorted_table_orders_by_the_column_and_keeps_ties_in_their_own_order() {
        let rows = [["b", "1"], ["a", "2"], ["b", "3"]].map(|row| row.map(String::from));
        assert_eq!(super::sheet_order(&rows, None), [0, 1, 2]);
        let up = super::SheetSort {
            column: 0,
            ascending: true,
        };
        assert_eq!(super::sheet_order(&rows, Some(up)), [1, 0, 2]);
        let down = super::SheetSort {
            ascending: false,
            ..up
        };
        assert_eq!(super::sheet_order(&rows, Some(down)), [0, 2, 1]);
        let missing = super::SheetSort {
            column: 9,
            ascending: true,
        };
        assert_eq!(super::sheet_order(&rows, Some(missing)), [0, 1, 2]);
    }

    #[test]
    fn empty_activity_and_backups_say_so() {
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = view(&fx, View::Activity);
        assert_eq!(open.find(super::NO_ACTIVITY).len(), 1);
        open.with(|live| live.app.select_view(View::Trash));
        open.tick();
        assert_eq!(open.find(super::NO_TRASH).len(), 1);
    }

    #[test]
    fn down_chooses_a_backup_from_the_keyboard() {
        let fx = Fixture::new(&["work"]);
        let file = backup(&fx);
        let (open, wires) = view(&fx, View::Trash);
        open.press("down");
        open.click("Restore");
        assert_eq!(
            wires.sent(),
            [("work".to_string(), Command::Restore { file })]
        );
    }

    #[test]
    fn a_trash_reload_reads_only_new_backups() {
        let fx = Fixture::new(&["work"]);
        let file = backup(&fx);
        let (open, wires) = view(&fx, View::Trash);
        assert_eq!(open.find("your receipt").len(), 1);
        // A backup is never rewritten; changing this one shows whether a reload opens it again.
        std::fs::write(
            &file,
            "From: x@example.com\r\nSubject: read again\r\n\r\nx\r\n",
        )
        .unwrap();
        let later = Trash::new(fx.paths.trash_dir("work"))
            .save(
                "INBOX",
                9,
                b"From: y@example.com\r\nSubject: new backup\r\n\r\nx\r\n",
                2_000_000_000,
            )
            .unwrap();
        let synced = Event::Synced {
            account: "work".into(),
            new_messages: 0,
            actions: 0,
            requests: vec![],
            errors: vec![],
        };
        open.receive(&wires, [synced]);
        assert_eq!(open.find("your receipt").len(), 1);
        assert!(open.find("read again").is_empty());
        assert_eq!(open.find("new backup").len(), 1);
        assert!(later.exists());
    }

    #[test]
    fn trash_lists_backups_and_restore_sends_the_command() {
        let fx = Fixture::new(&["work"]);
        let file = backup(&fx);
        let (open, wires) = view(&fx, View::Trash);
        assert_eq!(open.find("your receipt").len(), 1);
        assert_eq!(open.find("Shop <shop@example.com>").len(), 1);
        open.click("Restore");
        assert!(
            wires.sent().is_empty(),
            "Restore does nothing before a backup is chosen"
        );
        open.tap("your receipt");
        open.click("Restore");
        assert_eq!(
            wires.sent(),
            [("work".to_string(), Command::Restore { file: file.clone() })]
        );
        assert!(open.find("your receipt").is_empty());
        std::fs::remove_file(&file).unwrap();
        let restored = Event::Restored {
            account: "work".into(),
            folder: "INBOX".into(),
            request: 0,
        };
        open.receive(&wires, [restored]);
        assert!(open.find("your receipt").is_empty());
        let history = open.with(|live| {
            live.app
                .history
                .iter()
                .any(|line| line.text == "work: restored to INBOX")
        });
        assert!(history);
    }
}

#[cfg(test)]
mod editor_tests {
    use slint::{ComponentHandle, Model};

    use crate::state::View;
    use crate::test_support::{Fixture, Open, Wires, message};
    use crate::ui::RuleEditor;
    use postvak_core::rules::Action;

    const RULES: &str = "# keep me\n[[rules]]\nname = \"newsletters\"\nmatch.from = { contains = \"news@\" }\nactions = { archive = true }\n";

    fn two_rules() -> String {
        format!(
            "{RULES}\n[[rules]]\nname = \"receipts\"\nmatch.subject = {{ contains = \"receipt\" }}\nactions = {{ flagged = true }}\n"
        )
    }

    fn file(fx: &Fixture) -> String {
        std::fs::read_to_string(fx.paths.rules_file()).unwrap()
    }

    fn rules_view(fx: &Fixture, rules: &str) -> (Open, Wires) {
        std::fs::write(fx.paths.rules_file(), rules).unwrap();
        let (open, wires) = fx.window();
        open.with(|live| live.app.select_view(View::Rules));
        open.tick();
        (open, wires)
    }

    /// Runs `f` on the editor's global, as the window's fields would, then lets the window catch up.
    fn editor(open: &Open, f: impl FnOnce(&RuleEditor)) {
        f(&open.window().global::<RuleEditor>());
        open.tick();
    }

    fn open_newsletters(fx: &Fixture) -> (Open, Wires) {
        let (open, wires) = rules_view(fx, RULES);
        open.click("newsletters");
        (open, wires)
    }

    fn draft_name(open: &Open) -> Option<String> {
        open.with(|live| live.app.rule_draft.as_ref().map(|d| d.name.clone()))
    }

    #[test]
    fn a_click_beside_an_edited_rule_asks_before_closing_it() {
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = open_newsletters(&fx);
        editor(&open, |e| e.invoke_value_edited(0, "letters@".into()));
        open.tap("Backups");
        assert_eq!(open.find("This rule has unsaved changes.").len(), 1);
        assert_eq!(open.with(|live| live.app.view.clone()), View::Rules);
        open.tap("Backups");
        assert!(
            open.find("This rule has unsaved changes.").is_empty(),
            "the click cancels the question"
        );
        let opened = open.with(|live| live.app.rule_draft.as_ref()?.original.clone());
        assert_eq!(opened.as_deref(), Some("newsletters"));
    }

    /// newsletters open with an unsaved edit, then receipts clicked in the list.
    fn leave_edited_newsletters(fx: &Fixture) -> (Open, Wires) {
        let (open, wires) = rules_view(fx, &two_rules());
        open.click("newsletters");
        editor(&open, |e| e.invoke_value_edited(0, "letters@".into()));
        open.click("receipts");
        assert_eq!(open.find("This rule has unsaved changes.").len(), 1);
        (open, wires)
    }

    #[test]
    fn a_new_rule_is_saved_below_the_others_and_keeps_comments() {
        let fx = Fixture::new(&["work"]);
        let (open, wires) = rules_view(&fx, RULES);
        open.click("New rule");
        editor(&open, |e| {
            e.invoke_name_edited("receipts".into());
            e.invoke_add_condition();
            e.invoke_field_chosen(0, 3);
            e.invoke_value_edited(0, "receipt".into());
            e.invoke_action_chosen(5);
            e.invoke_action_value_edited(0, "Receipts".into());
        });
        open.click("Save");
        assert_eq!(
            file(&fx),
            format!(
                "{RULES}\n[[rules]]\nname = \"receipts\"\nmatch.subject = {{ contains = \"receipt\" }}\nactions.move = \"Receipts\"\n"
            )
        );
        assert!(draft_name(&open).is_none(), "a save closes the dialog");
        assert!(wires.sent().is_empty());
    }

    #[test]
    fn escape_closes_an_unchanged_rule_without_asking() {
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = open_newsletters(&fx);
        assert_eq!(open.find("Edit rule").len(), 1);
        open.press("escape");
        assert!(draft_name(&open).is_none());
        assert!(open.find("This rule has unsaved changes.").is_empty());
    }

    #[test]
    fn closing_an_edited_rule_asks_and_discard_drops_the_edit() {
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = open_newsletters(&fx);
        editor(&open, |e| e.invoke_value_edited(0, "letters@".into()));
        open.press("escape");
        assert_eq!(open.find("This rule has unsaved changes.").len(), 1);
        open.press("escape");
        assert_eq!(
            draft_name(&open).as_deref(),
            Some("newsletters"),
            "Esc cancels the question only"
        );
        open.click("Close");
        open.click("Discard");
        assert!(draft_name(&open).is_none());
        assert_eq!(file(&fx), RULES);
    }

    #[test]
    fn closing_an_edited_rule_can_save_it_first() {
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = open_newsletters(&fx);
        editor(&open, |e| e.invoke_value_edited(0, "letters@".into()));
        open.press("escape");
        editor(&open, |e| e.invoke_unsaved_save());
        assert!(draft_name(&open).is_none());
        assert_eq!(file(&fx), RULES.replace("news@", "letters@"));
    }

    #[test]
    fn apply_waits_until_the_rule_is_saved_and_on() {
        let fx = Fixture::new(&["work"]);
        let (open, wires) = rules_view(&fx, RULES);
        open.click("New rule");
        assert!(
            open.find("Apply to existing mail").is_empty(),
            "nothing saved to apply"
        );
        open.press("escape");
        open.click("newsletters");
        editor(&open, |e| e.invoke_value_edited(0, "letters@".into()));
        open.click("Apply to existing mail");
        assert!(wires.sent().is_empty(), "an unsaved edit is not applied");
        editor(&open, |e| e.invoke_value_edited(0, "news@".into()));
        open.click("Apply to existing mail");
        assert_eq!(wires.sent().len(), 1);
    }

    #[test]
    fn a_filtered_row_keeps_its_number_and_opens_its_own_rule() {
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = rules_view(&fx, &two_rules());
        open.get("Filter rules").set_accessible_value("receipt");
        open.tick();
        let rules = open.window().get_rules();
        let row = rules.row_data(0).unwrap();
        assert_eq!((rules.row_count(), row.number.as_str()), (1, "2"));
        open.click("Enable receipts");
        let rules = postvak_core::rules::load(&fx.paths.rules_file())
            .unwrap()
            .rules;
        let enabled: Vec<bool> = rules.iter().map(|r| r.enabled).collect();
        assert_eq!(
            enabled,
            [true, false],
            "the switch turns off the filtered rule"
        );
        open.tap("receipts");
        assert_eq!(draft_name(&open).as_deref(), Some("receipts"));
    }

    #[test]
    fn a_heading_sorts_the_rules_and_the_number_keeps_their_order() {
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = rules_view(&fx, &two_rules());
        let shown = || -> Vec<(String, String)> {
            let rules = open.window().get_rules();
            (rules.iter())
                .map(|r| (r.number.to_string(), r.label.to_string()))
                .collect()
        };
        let pair = |n: &str, name: &str| (n.to_string(), name.to_string());
        assert_eq!(shown(), [pair("1", "newsletters"), pair("2", "receipts")]);
        open.click("Then");
        assert_eq!(
            shown(),
            [pair("1", "newsletters"), pair("2", "receipts")],
            "archive before flag"
        );
        open.click("Then");
        assert_eq!(shown(), [pair("2", "receipts"), pair("1", "newsletters")]);
        assert_eq!(
            open.find("Then ↓").len(),
            0,
            "the arrow is drawn, not part of the label"
        );
        open.click("receipts");
        assert_eq!(
            draft_name(&open).as_deref(),
            Some("receipts"),
            "a click opens the rule it shows"
        );
        open.press("escape");
        open.click("#");
        assert_eq!(shown(), [pair("1", "newsletters"), pair("2", "receipts")]);
        open.click("#");
        assert_eq!(shown(), [pair("2", "receipts"), pair("1", "newsletters")]);
    }

    #[test]
    fn a_rule_row_dropped_on_another_moves_the_rule_in_rules_toml() {
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = rules_view(&fx, &two_rules());
        let labels = || -> Vec<String> {
            (open.window().get_rules().iter())
                .map(|r| r.label.to_string())
                .collect()
        };
        let window = open.window();
        assert!(window.get_rules_reorderable());
        let data = window.invoke_rule_drag_data(1);
        assert_eq!(window.invoke_dragged_rule(data), 1);
        window.invoke_rule_dropped(1, 0, false);
        open.tick();
        assert_eq!(labels(), ["receipts", "newsletters"]);
        let names: Vec<String> = (postvak_core::rules::load(&fx.paths.rules_file())
            .unwrap()
            .rules)
            .into_iter()
            .map(|r| r.name)
            .collect();
        assert_eq!(names, ["receipts", "newsletters"]);

        open.click("Name");
        assert!(!open.window().get_rules_reorderable());
        let before = file(&fx);
        open.window().invoke_rule_dropped(0, 1, true);
        open.tick();
        assert_eq!(
            file(&fx),
            before,
            "a table sorted by name has no place to drop on"
        );
    }

    #[test]
    fn editing_a_rule_changes_only_its_value() {
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = open_newsletters(&fx);
        open.get("Condition 1 value")
            .set_accessible_value("letters@");
        open.tick();
        open.click("Save");
        assert_eq!(file(&fx), RULES.replace("news@", "letters@"));
    }

    #[test]
    fn a_field_typed_in_shows_the_next_rule_opened() {
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = rules_view(&fx, &two_rules());
        open.click("newsletters");
        open.get("Condition 1 value")
            .set_accessible_value("letters@");
        open.tick();
        open.click("receipts");
        open.click("Discard");
        open.tick();
        let value = open.get("Condition 1 value").accessible_value();
        assert_eq!(value.as_deref(), Some("receipt"));
    }

    #[test]
    fn conditions_the_form_cannot_edit_are_shown_and_saved_unchanged() {
        let grouped = RULES.replace(
            "actions = { archive = true }",
            "match.subject = { contains = [\"weekly\", \"digest\"] }\nactions = { archive = true }",
        );
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = rules_view(&fx, &grouped);
        open.click("newsletters");
        assert_eq!(
            open.find("subject contains \"weekly\" or \"digest\"").len(),
            1
        );
        assert_eq!(
            open.find("Edit these in rules.toml; Save keeps them as they are.")
                .len(),
            1
        );
        editor(&open, |e| e.invoke_value_edited(0, "letters@".into()));
        open.click("Save");
        assert_eq!(file(&fx), grouped.replace("news@", "letters@"));
    }

    #[test]
    fn renaming_warns_first_and_saves_in_place() {
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = open_newsletters(&fx);
        assert!(
            open.labels_containing("Renaming makes this a new rule")
                .is_empty()
        );
        editor(&open, |e| e.invoke_name_edited("news".into()));
        assert_eq!(
            open.find(
                "Renaming makes this a new rule: it acts only on mail that arrives after you save."
            )
            .len(),
            1
        );
        open.click("Save");
        assert_eq!(file(&fx), RULES.replace("newsletters", "news"));
    }

    #[test]
    fn a_taken_name_is_refused_and_shown() {
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = rules_view(&fx, &two_rules());
        open.click("newsletters");
        editor(&open, |e| e.invoke_name_edited("receipts".into()));
        open.click("Save");
        assert_eq!(file(&fx), two_rules());
        assert_eq!(open.labels_containing("duplicate").len(), 1);
    }

    #[test]
    fn a_rule_removed_elsewhere_is_not_saved_back() {
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = open_newsletters(&fx);
        std::fs::write(fx.paths.rules_file(), "# emptied\n").unwrap();
        editor(&open, |e| e.invoke_value_edited(0, "letters@".into()));
        open.click("Save");
        assert_eq!(file(&fx), "# emptied\n");
        assert_eq!(open.labels_containing("no such rule").len(), 1);
    }

    #[test]
    fn the_list_switch_updates_the_open_rule_so_save_keeps_it() {
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = open_newsletters(&fx);
        open.click("Enable newsletters");
        let enabled = open.with(|live| live.app.rule_draft.clone().unwrap().enabled);
        assert!(!enabled);
        open.click("Save");
        let file = postvak_core::rules::load(&fx.paths.rules_file()).unwrap();
        assert!(!file.rules[0].enabled);
    }

    #[test]
    fn a_rule_switched_off_elsewhere_stays_off_when_saved() {
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = open_newsletters(&fx);
        let off = RULES.replace(
            "actions = { archive = true }",
            "enabled = false\nactions = { archive = true }",
        );
        std::fs::write(fx.paths.rules_file(), &off).unwrap();
        let file = std::fs::File::options()
            .write(true)
            .open(fx.paths.rules_file())
            .unwrap();
        file.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(5))
            .unwrap();
        open.advance(5.0);
        editor(&open, |e| e.invoke_value_edited(0, "letters@".into()));
        open.click("Save");
        let file = postvak_core::rules::load(&fx.paths.rules_file()).unwrap();
        assert!(!file.rules[0].enabled);
    }

    /// Writes rules.toml as another program would, with a time the window's next poll sees as new.
    fn write_elsewhere(open: &Open, fx: &Fixture, rules: &str) {
        std::fs::write(fx.paths.rules_file(), rules).unwrap();
        let file = std::fs::File::options()
            .write(true)
            .open(fx.paths.rules_file())
            .unwrap();
        file.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(5))
            .unwrap();
        open.advance(5.0);
    }

    /// Regression: a rule only looked at kept the conditions it was opened with, so Save undid a change made outside.
    #[test]
    fn a_rule_changed_elsewhere_while_open_shows_and_saves_the_change() {
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = open_newsletters(&fx);
        let changed = RULES.replace("news@", "digest@");
        write_elsewhere(&open, &fx, &changed);
        let value = open.get("Condition 1 value").accessible_value();
        assert_eq!(value.as_deref(), Some("digest@"));
        open.click("Save");
        assert_eq!(file(&fx), changed);
    }

    /// Regression: an edit saved over a change made outside while the rule was open, without a word.
    #[test]
    fn an_edit_does_not_silently_undo_a_change_made_elsewhere() {
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = open_newsletters(&fx);
        editor(&open, |e| e.invoke_value_edited(0, "letters@".into()));
        let changed = RULES.replace("archive = true", "flagged = true");
        write_elsewhere(&open, &fx, &changed);
        open.click("Save");
        assert_eq!(file(&fx), changed, "the first Save writes nothing");
        assert_eq!(
            open.labels_containing("changed in rules.toml while you edited it")
                .len(),
            1
        );
        open.click("Save");
        assert_eq!(file(&fx), RULES.replace("news@", "letters@"));
    }

    #[test]
    fn remove_asks_first_and_keeps_the_comment() {
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = open_newsletters(&fx);
        open.click("Remove rule");
        assert_eq!(file(&fx), RULES, "nothing removed before the confirmation");
        open.click("Remove from rules.toml");
        assert_eq!(file(&fx), "# keep me\n");
        assert!(draft_name(&open).is_none());
    }

    #[test]
    fn an_unsaved_rule_is_closed_without_asking() {
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = rules_view(&fx, RULES);
        open.click("New rule");
        open.click("Remove rule");
        assert!(draft_name(&open).is_none());
        assert_eq!(file(&fx), RULES);
    }

    #[test]
    fn test_counts_cached_mail_and_changes_nothing() {
        let fx = Fixture::new(&["work"]);
        let mut weekly = message("INBOX", 1, "weekly");
        weekly.from_addr = Some("News <news@example.com>".into());
        fx.add("work", weekly);
        fx.add("work", message("INBOX", 2, "lunch"));
        let (open, wires) = open_newsletters(&fx);
        open.click("Test");
        assert_eq!(open.find("would archive 1 cached message").len(), 1);
        assert_eq!(file(&fx), RULES);
        assert!(wires.sent().is_empty());
    }

    #[test]
    fn conditions_and_actions_are_added_and_removed() {
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = open_newsletters(&fx);
        open.click("+ Add condition");
        let fields = open.with(|live| {
            let draft = live.app.rule_draft.clone().unwrap();
            draft.conditions.iter().map(|c| c.field).collect::<Vec<_>>()
        });
        assert_eq!(fields, [crate::draft::Field::From, crate::draft::Field::To]);
        open.click("Remove condition 2");
        open.click("Remove archive");
        open.click("+ Add action");
        open.click("Delete");
        let actions = open.with(|live| live.app.rule_draft.clone().unwrap().actions);
        assert_eq!(actions, [Action::Delete]);
        assert_eq!(open.find("keeps a .eml backup").len(), 1);
    }

    #[test]
    fn cancel_keeps_the_edited_draft() {
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = leave_edited_newsletters(&fx);
        open.click("Cancel");
        assert!(open.find("This rule has unsaved changes.").is_empty());
        let opened = open.with(|live| live.app.rule_draft.as_ref()?.original.clone());
        assert_eq!(opened.as_deref(), Some("newsletters"));
        let value = open.with(|live| {
            live.app.rule_draft.clone().unwrap().conditions[0]
                .value
                .clone()
        });
        assert_eq!(value, "letters@");
        assert_eq!(file(&fx), two_rules());
    }

    #[test]
    fn discard_drops_the_edit_and_opens_the_other_rule() {
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = leave_edited_newsletters(&fx);
        open.click("Discard");
        assert_eq!(draft_name(&open).as_deref(), Some("receipts"));
        assert_eq!(file(&fx), two_rules());
    }

    #[test]
    fn save_writes_the_edit_then_opens_the_other_rule() {
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = leave_edited_newsletters(&fx);
        assert_eq!(open.find("Save").len(), 2, "the editor's and the prompt's");
        editor(&open, |e| e.invoke_unsaved_save());
        assert_eq!(file(&fx), two_rules().replace("news@", "letters@"));
        assert_eq!(draft_name(&open).as_deref(), Some("receipts"));
    }

    #[test]
    fn a_failed_save_stays_on_the_draft_and_shows_why() {
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = leave_edited_newsletters(&fx);
        std::fs::write(fx.paths.rules_file(), "# emptied\n").unwrap();
        editor(&open, |e| e.invoke_unsaved_save());
        let opened = open.with(|live| live.app.rule_draft.as_ref()?.original.clone());
        assert_eq!(opened.as_deref(), Some("newsletters"));
        assert!(open.find("This rule has unsaved changes.").is_empty());
        assert_eq!(open.labels_containing("no such rule").len(), 1);
        assert_eq!(file(&fx), "# emptied\n");
    }

    #[test]
    fn an_unchanged_draft_is_left_without_asking() {
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = rules_view(&fx, &two_rules());
        open.click("New rule");
        open.click("newsletters");
        open.click("Test");
        open.click("receipts");
        assert!(open.find("This rule has unsaved changes.").is_empty());
        assert_eq!(draft_name(&open).as_deref(), Some("receipts"));
    }
}
