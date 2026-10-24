//! A benchmark over generated stores: accounts × folders × messages, with threads and flags, synced into SQLite
//! through `RecordingOps`. No user data is read. Each phase prints one JSON line; see docs/src/perf.md.
//!
//! ```sh
//! POSTVAK_BENCH_SCALE=10000 cargo test --release -p postvak-gui --lib bench::bench -- --ignored --nocapture
//! ```
use std::time::Instant;

use postvak_core::mail_ops::RecordingOps;
use postvak_core::store::{Filter, Store};
use postvak_core::sync::{IndexOutcome, index_slice, sync_all};

use super::list::THREAD_LIMIT;
use super::state::{LIST_BUDGET, UiAction, View};
use super::test_support::Fixture;

const ACCOUNTS: [&str; 3] = ["home", "lists", "work"];
/// Each folder's share of an account's mail, in sixteenths.
const FOLDERS: [(&str, Option<&str>, u32); 6] = [
    ("Archive", Some("Archive"), 4),
    ("INBOX", None, 8),
    ("Junk", Some("Junk"), 1),
    ("Lists", None, 1),
    ("Sent", Some("Sent"), 1),
    ("Trash", Some("Trash"), 1),
];
const WORDS: [&str; 12] = [
    "budget", "contract", "dinner", "holiday", "invoice", "meeting", "photos", "release", "report",
    "school", "ticket", "travel",
];
const RUNS: usize = 5;
/// Per sync pass: messages that arrive in INBOX, and existing ones whose flags change.
const ARRIVING: u32 = 200;
const RETAGGED: usize = 500;
const T0: i64 = 1_700_000_000;

/// xorshift64: the same mail on every run without a dependency.
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % n
    }
}

/// Mostly single messages, some short threads, a few long ones.
fn thread_length(rng: &mut Rng) -> u32 {
    match rng.below(100) {
        0..60 => 1,
        60..85 => 2 + rng.below(3) as u32,
        85..97 => 5 + rng.below(8) as u32,
        _ => 13 + rng.below(48) as u32,
    }
}

/// About 10% unread and 3% flagged.
fn flags(rng: &mut Rng) -> Vec<String> {
    let mut flags = Vec::new();
    if rng.below(100) >= 10 {
        flags.push("\\Seen".to_string());
    }
    if rng.below(100) < 3 {
        flags.push("\\Flagged".to_string());
    }
    flags
}

/// Appends `count` messages to `folder`, in threads; replies carry In-Reply-To and References like a mail client's.
fn add_mail(ops: &mut RecordingOps, rng: &mut Rng, account: &str, folder: &str, count: u32) {
    let first = ops.mail.get(folder).map_or(0, |mail| mail.len() as u32) + 1;
    let end = first + count;
    let mut uid = first;
    while uid < end {
        let subject = format!(
            "{} {}",
            WORDS[rng.below(WORDS.len() as u64) as usize],
            WORDS[rng.below(WORDS.len() as u64) as usize]
        );
        let mut ids: Vec<String> = Vec::new();
        for _ in 0..thread_length(rng).min(end - uid) {
            let id = format!("<{account}.{folder}.{uid}@bench.example>");
            let date = T0 + i64::from(uid) * 600;
            let sender = rng.below(500);
            let replying = match ids.as_slice() {
                [] => String::new(),
                [.., parent] => {
                    format!("In-Reply-To: {parent}\r\nReferences: {}\r\n", ids.join(" "))
                }
            };
            let headers = format!(
                "Message-ID: {id}\r\nFrom: Sender {sender} <s{sender}@bench.example>\r\nTo: me@{account}.example\r\n\
                 Subject: {}{subject}\r\nDate: {}\r\n{replying}\r\n",
                if ids.is_empty() { "" } else { "Re: " },
                chrono::DateTime::from_timestamp(date, 0)
                    .unwrap()
                    .to_rfc2822(),
            );
            ops.add_mail(folder, uid, date, &headers, None);
            ops.mail.get_mut(folder).unwrap().last_mut().unwrap().flags = flags(rng);
            ids.push(id);
            uid += 1;
        }
    }
}

fn timed(work: impl FnOnce()) -> f64 {
    let start = Instant::now();
    work();
    start.elapsed().as_secs_f64() * 1000.0
}

fn report(phase: &str, scale: u32, mut ms: Vec<f64>) {
    ms.sort_by(f64::total_cmp);
    let line = serde_json::json!({
        "phase": phase,
        "messages": scale,
        "runs": ms.len(),
        "median_ms": ms[ms.len() / 2],
        "min_ms": ms[0],
        "max_ms": ms[ms.len() - 1],
    });
    println!("{line}");
}

/// Generates `scale` messages over every account and times each phase `runs` times.
fn run(scale: u32, runs: usize) {
    let fx = Fixture::new(&ACCOUNTS);
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let per_account = scale / ACCOUNTS.len() as u32;
    let mut servers: Vec<RecordingOps> = ACCOUNTS
        .iter()
        .map(|account| {
            let mut ops = FOLDERS
                .iter()
                .fold(RecordingOps::default(), |ops, (name, role, _)| {
                    ops.with_folder(name, *role)
                });
            for (name, _, share) in FOLDERS {
                add_mail(&mut ops, &mut rng, account, name, per_account * share / 16);
            }
            ops
        })
        .collect();
    let stores: Vec<Store> = ACCOUNTS.iter().map(|account| fx.store(account)).collect();
    report(
        "first_sync",
        scale,
        vec![timed(|| {
            for (ops, store) in servers.iter_mut().zip(&stores) {
                sync_all(ops, store).unwrap();
            }
        })],
    );

    // The indexer's throughput against the fake: every message of the first account in one unlimited slice. The
    // store keeps the text, so each run indexes the same mail again after a reset.
    let indexed = (0..runs).map(|_| {
        stores[0].forget_indexing().unwrap();
        let (events, _) = std::sync::mpsc::channel();
        let account = postvak_core::config::AccountConfig::new(
            "home",
            "h",
            "u",
            postvak_core::config::PasswordSource::Keyring { keyring: true },
        );
        timed(|| {
            while index_slice(&mut servers[0], &stores[0], &account, &events).unwrap()
                == IndexOutcome::Progressed
            {}
        })
    });
    report("index_all", scale, indexed.collect());

    report(
        "open_store",
        scale,
        (0..runs)
            .map(|_| timed(|| drop(fx.store("home"))))
            .collect(),
    );
    // What migration 011 cost once per store: the one rebuild of the search index, here over every account's.
    report(
        "rebuild_index",
        scale,
        (0..runs)
            .map(|_| {
                timed(|| {
                    stores
                        .iter()
                        .for_each(|s| s.rebuild_search_index().unwrap())
                })
            })
            .collect(),
    );
    report(
        "open_window",
        scale,
        (0..runs).map(|_| timed(|| drop(fx.driver()))).collect(),
    );

    let summaries = (0..runs).map(|_| {
        timed(|| {
            let threads = stores[0]
                .thread_summaries("INBOX", THREAD_LIMIT, Filter::default())
                .unwrap();
            assert!(
                threads.len() < stores[0].message_count("INBOX").unwrap() as usize,
                "threads group mail"
            );
        })
    });
    report("thread_grouping", scale, summaries.collect());

    let (mut driver, _wires) = fx.driver();
    driver.apply(UiAction::SelectView(View::Folder {
        account: 0,
        folder: "INBOX".into(),
    }));
    report(
        "folder_list",
        scale,
        (0..runs)
            .map(|_| timed(|| driver.app.reload_view()))
            .collect(),
    );
    assert!(!driver.app.list.rows.is_empty());

    driver.apply(UiAction::SelectView(View::AllInboxes));
    report(
        "all_inboxes",
        scale,
        (0..runs)
            .map(|_| timed(|| driver.app.reload_view()))
            .collect(),
    );
    let accounts: std::collections::HashSet<usize> =
        driver.app.list.rows.iter().map(|row| row.account).collect();
    assert_eq!(accounts.len(), ACCOUNTS.len());

    // A sync event in All inboxes with the window's own budget: how long the frame that takes it waits. The reload
    // that outlasts the budget finishes before the next run.
    driver.app.list_budget = LIST_BUDGET;
    let frames = (0..runs).map(|_| {
        driver.app.view_dirty = true;
        let ms = timed(|| driver.tick());
        while driver.app.list_loading.is_some() {
            std::thread::sleep(std::time::Duration::from_millis(1));
            driver.tick();
        }
        ms
    });
    report("sync_event", scale, frames.collect());
    driver.app.list_budget = std::time::Duration::from_secs(60);

    // What every sync event of an account also does on the window thread: its folders with their unread counts.
    report(
        "folder_counts",
        scale,
        (0..runs)
            .map(|_| timed(|| driver.app.accounts[0].reload_folders()))
            .collect(),
    );

    driver.press("/");
    driver.apply(UiAction::SearchFor("invoice".into()));
    report(
        "search",
        scale,
        (0..runs)
            .map(|_| timed(|| driver.app.reload_view()))
            .collect(),
    );
    assert!(!driver.app.list.rows.is_empty());

    // A misspelled word: the exact tier is empty, so the vocabulary is consulted and the close tier runs.
    driver.apply(UiAction::SearchFor("invioce".into()));
    report(
        "search_close",
        scale,
        (0..runs)
            .map(|_| timed(|| driver.app.reload_view()))
            .collect(),
    );
    assert!(driver.app.list.rows.iter().all(|row| row.close));
    assert!(!driver.app.list.rows.is_empty());

    let ops = &mut servers[0];
    let passes = (0..runs).map(|_| {
        add_mail(ops, &mut rng, "home", "INBOX", ARRIVING);
        for _ in 0..RETAGGED {
            let (name, _, _) = FOLDERS[rng.below(FOLDERS.len() as u64) as usize];
            let mail = ops.mail.get_mut(name).unwrap();
            let index = rng.below(mail.len() as u64) as usize;
            mail[index].flags = flags(&mut rng);
        }
        timed(|| drop(sync_all(ops, &stores[0]).unwrap()))
    });
    report("sync_pass", scale, passes.collect());
}

#[test]
fn bench_runs_at_a_small_scale() {
    run(600, 1);
}

#[test]
#[ignore = "a benchmark: run with --release, see docs/src/perf.md"]
fn bench() {
    let scale = std::env::var("POSTVAK_BENCH_SCALE").map_or(10_000, |scale| scale.parse().unwrap());
    run(scale, RUNS);
}
