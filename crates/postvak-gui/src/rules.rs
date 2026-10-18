//! The Rules, Activity and Trash views: what replaces the list and body columns.
use std::collections::BTreeMap;
use std::path::Path;

use postvak_core::help;
use postvak_core::message::clean;
use postvak_core::paths::Paths;
use postvak_core::rules::{self, Rule, RuleFile};
use postvak_core::store::LogEntry;
use postvak_core::trash::{Trash, TrashEntry};

use postvak_core::time::date_time;

use super::draft;
use super::state::{Account, App};

/// Entries shown in the Activity view.
const LOG_LIMIT: u32 = 500;

pub(crate) struct RulesState {
    pub error: Option<String>,
    /// "Preview on cached mail" results by proposal name; a reload drops them.
    pub previews: BTreeMap<String, String>,
    pub rules: Vec<Rule>,
}

impl RulesState {
    /// The file's rules, or the previous ones when it does not parse. A rule that does not compile is reported but
    /// still listed, so it can be switched off.
    pub fn load(path: &Path, previous: Vec<Rule>) -> RulesState {
        let (error, rules) = match rules::load(path) {
            Ok(file) => (
                rules::compile(&file).err().map(|e| e.to_string()),
                file.rules,
            ),
            Err(e) => (Some(e.to_string()), previous),
        };
        RulesState {
            error,
            previews: BTreeMap::new(),
            rules,
        }
    }
}

/// A rule waiting for approval, as the Rules view shows it.
pub(crate) struct Proposal {
    pub name: String,
    pub label: String,
    /// "proposed by agent".
    pub by: String,
    pub summary: String,
    /// The TOML it adds, each line behind a "+".
    pub toml: String,
    /// What "Preview on cached mail" found, or empty.
    pub preview: String,
}

/// A rule in the Rules view's list.
pub(crate) struct RuleLine {
    pub name: String,
    pub label: String,
    pub enabled: bool,
    /// "work · INBOX", or "every account · INBOX".
    pub scope: String,
    /// "proposed by agent", or empty.
    pub by: String,
    pub summary: String,
}

/// "1 rule · 2 proposals", beside the Rules title.
pub(crate) fn counts(rules: usize, proposals: usize) -> String {
    let plural = |n: usize, word: &str| format!("{n} {word}{}", if n == 1 { "" } else { "s" });
    format!(
        "{} · {}",
        plural(rules, "rule"),
        plural(proposals, "proposal")
    )
}

/// Whether the list shows `rule` as a proposal rather than a rule.
pub(crate) fn is_proposal(rule: &Rule) -> bool {
    !rule.enabled && rule.proposed_by.is_some()
}

/// The line above the Rules view when rules.toml does not load.
pub(crate) fn banner(state: &RulesState) -> Option<String> {
    let error = state.error.as_deref()?;
    Some(format!(
        "{} — sync keeps the previous rules until this is fixed",
        clean(error, false)
    ))
}

pub(crate) fn proposals(state: &RulesState) -> Vec<Proposal> {
    state
        .rules
        .iter()
        .filter(|rule| is_proposal(rule))
        .map(|rule| {
            let toml = toml::to_string(&RuleFile {
                rules: vec![rule.clone()],
            })
            .unwrap_or_else(|e| e.to_string());
            let toml: Vec<String> = clean(&toml, true)
                .trim_end()
                .lines()
                .map(|line| format!("+ {line}").trim_end().to_owned())
                .collect();
            Proposal {
                name: rule.name.clone(),
                label: clean(&rule.name, false),
                by: format!(
                    "proposed by {}",
                    clean(rule.proposed_by.as_deref().unwrap_or_default(), false)
                ),
                summary: draft::summary(rule),
                toml: toml.join("\n"),
                preview: state.previews.get(&rule.name).cloned().unwrap_or_default(),
            }
        })
        .collect()
}

pub(crate) fn rule_lines(state: &RulesState) -> Vec<RuleLine> {
    let line = |rule: &Rule| {
        let account = rule
            .account
            .as_deref()
            .map_or("every account".into(), |a| clean(a, false));
        let folder = rule
            .folder
            .as_deref()
            .map_or("INBOX".into(), |f| clean(f, false));
        RuleLine {
            name: rule.name.clone(),
            label: clean(&rule.name, false),
            enabled: rule.enabled,
            scope: format!("{account} · {folder}"),
            by: rule
                .proposed_by
                .as_deref()
                .map(|by| format!("proposed by {}", clean(by, false)))
                .unwrap_or_default(),
            summary: draft::summary(rule),
        }
    };
    state
        .rules
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
pub(crate) fn sheet_order<const N: usize>(
    rows: &[[String; N]],
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

/// The Activity view's rows, cell by cell.
pub(crate) fn activity_cells(app: &App) -> Vec<[String; 6]> {
    let row = |(account, entry): &(String, LogEntry)| {
        [
            date_time(entry.at),
            account.clone(),
            clean(&entry.rule_name, false),
            clean(&entry.action, false),
            clean(&entry.folder, false),
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
            clean(&row.entry.folder, false),
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

/// Every account's `.eml` backups, newest first, with sender and subject from their headers.
pub(crate) fn trash_rows(accounts: &[Account], paths: &Paths) -> Vec<TrashRow> {
    let mut rows = Vec::new();
    for (index, account) in accounts.iter().enumerate() {
        match Trash::new(paths.trash_dir(&account.name)).list() {
            Ok(entries) => rows.extend(entries.into_iter().map(|entry| {
                let headers = entry.headers();
                TrashRow {
                    account: index,
                    entry,
                    from: headers.from.unwrap_or_default(),
                    subject: headers.subject.unwrap_or_default(),
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

    const RULES: &str = "# keep me\n[[rules]]\nname = \"newsletters\"\nmatch.from = { contains = \"news@\" }\nactions = [\"archive\"]\n\n[[rules]]\nname = \"codes\"\nenabled = false\nproposed_by = \"agent\"\nmatch.subject = { contains = \"code\" }\nactions = [\"delete\"]\n";

    fn touch(path: &Path) {
        let file = std::fs::File::options().write(true).open(path).unwrap();
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
    fn apply_runs_an_enabled_rule_on_its_accounts_and_shows_the_outcome() {
        let fx = Fixture::new(&["home", "work"]);
        let only_work = "\n[[rules]]\nname = \"only-work\"\naccount = \"work\"\nmatch.subject = { contains = \"x\" }\nactions = [\"flag\"]\n";
        std::fs::write(fx.paths.rules_file(), format!("{RULES}{only_work}")).unwrap();
        let (open, wires) = view(&fx, View::Rules);
        let apply = |nth: usize| {
            let buttons = open.find("Apply to existing mail");
            assert_eq!(
                buttons.len(),
                2,
                "one per rule, not for the proposal: newsletters, only-work"
            );
            buttons[nth].invoke_accessible_default_action();
        };
        apply(1);
        let only = |name: &str, account: &str| {
            (
                account.to_string(),
                Command::ApplyRule { name: name.into() },
            )
        };
        assert_eq!(wires.sent(), [only("only-work", "work")]);
        apply(0);
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
        wires.events.send(applied(Vec::new())).unwrap();
        open.tick();
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
        let gone = "[[rules]]\nname = \"old\"\naccount = \"gone\"\nmatch.subject = { contains = \"x\" }\nactions = [\"flag\"]\n";
        std::fs::write(fx.paths.rules_file(), gone).unwrap();
        let (open, wires) = view(&fx, View::Rules);
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
        wires.events.send(applied).unwrap();
        open.tick();
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
        assert_eq!(open.find("proposed by agent").len(), 1);
        assert_eq!(open.find("subject contains \"code\" → delete").len(), 1);
        assert_eq!(open.labels_containing("+ name = \"codes\"").len(), 1);
        open.click("Approve");
        assert!(enabled(&fx, "codes"));
        assert!(fx.store("work").rule_first_seen("codes", "", 0).unwrap() > 1);
        assert!(
            std::fs::read_to_string(fx.paths.rules_file())
                .unwrap()
                .starts_with("# keep me\n")
        );
        assert!(wires.sent().is_empty());
        assert!(open.find("Approve").is_empty());
    }

    #[test]
    fn the_header_counts_rules_and_proposals_and_rows_say_what_they_do() {
        assert_eq!(super::counts(1, 2), "1 rule · 2 proposals");
        assert_eq!(super::counts(0, 1), "0 rules · 1 proposal");
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = rules_view(&fx);
        assert_eq!(open.find("1 rule · 1 proposal").len(), 1);
        assert_eq!(open.find("every account · INBOX").len(), 1);
        assert_eq!(open.find("from contains \"news@\" → archive").len(), 1);
    }

    #[test]
    fn preview_counts_what_a_proposal_would_do_to_cached_mail() {
        let fx = Fixture::new(&["work"]);
        fx.add("work", message("INBOX", 1, "your code is 1234"));
        fx.add("work", message("INBOX", 2, "lunch"));
        let (open, wires) = rules_view(&fx);
        open.click("Preview on cached mail");
        assert_eq!(open.find("would delete 1 cached message").len(), 1);
        assert!(wires.sent().is_empty());
        assert!(!enabled(&fx, "codes"), "a preview changes nothing");
    }

    #[test]
    fn agent_text_is_drawn_without_control_characters() {
        let sneaky = "[[rules]]\nname = \"codes\"\nenabled = false\nproposed_by = \"age\\u001bnt\"\nmatch.subject = { contains = \"\\u001b[2Jcode\" }\nactions = [\"delete\"]\n";
        let fx = Fixture::new(&["work"]);
        std::fs::write(fx.paths.rules_file(), sneaky).unwrap();
        let (open, _wires) = view(&fx, View::Rules);
        assert_eq!(open.find("proposed by agent").len(), 1);
        assert_eq!(open.find("subject contains \"[2Jcode\" → delete").len(), 1);
        assert!(open.labels_containing("\u{1b}").is_empty());
    }

    #[test]
    fn reject_takes_the_proposal_out() {
        let fx = Fixture::new(&["work"]);
        let (open, _wires) = rules_view(&fx);
        open.click("Reject");
        let file = postvak_core::rules::load(&fx.paths.rules_file()).unwrap();
        assert!(file.rules.iter().all(|r| r.name != "codes"));
        assert!(open.find("Approve").is_empty());
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
            "{RULES}\n[[rules]]\nname = \"receipts\"\nmatch.subject = {{ contains = \"receipt\" }}\nactions = [{{ move = \"Receipts\" }}]\n"
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
        wires.events.send(restored).unwrap();
        open.tick();
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
