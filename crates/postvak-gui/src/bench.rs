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
use super::reader;
use super::state::{BODY_BUDGET, LIST_BUDGET, UiAction, View};
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

/// A 5 MB message: a 200 kB HTML newsletter and a 4.8 MB attachment, as mail with a scan or a slide deck is.
fn large_message() -> Vec<u8> {
    let row = "<tr><td><a href=\"https://example.com/item\">An item</a></td><td>A line of newsletter text</td></tr>\n";
    let html = format!(
        "<html><body><table>{}</table></body></html>",
        row.repeat(200_000 / row.len())
    );
    let bytes: Vec<u8> = (0..3_600_000u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8)
        .collect();
    let encoded = base64(&bytes);
    let lines: Vec<&str> = encoded
        .as_bytes()
        .chunks(76)
        .map(|line| std::str::from_utf8(line).unwrap())
        .collect();
    format!(
        "From: Sender <sender@example.com>\r\nTo: me@example.com\r\nSubject: large\r\nMIME-Version: 1.0\r\n\
         Content-Type: multipart/mixed; boundary=\"b\"\r\n\r\n--b\r\nContent-Type: text/html; charset=utf-8\r\n\r\n\
         {html}\r\n--b\r\nContent-Type: application/pdf; name=\"scan.pdf\"\r\n\
         Content-Disposition: attachment; filename=\"scan.pdf\"\r\nContent-Transfer-Encoding: base64\r\n\r\n\
         {}\r\n--b--\r\n",
        lines.join("\r\n")
    )
    .into_bytes()
}

/// Base64 without padding concerns: `bytes` is a multiple of three long.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len() / 3 * 4);
    for chunk in bytes.chunks(3) {
        let n = u32::from(chunk[0]) << 16 | u32::from(chunk[1]) << 8 | u32::from(chunk[2]);
        for shift in [18, 12, 6, 0] {
            out.push(char::from(ALPHABET[(n >> shift & 63) as usize]));
        }
    }
    out
}

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

/// A fake server per account with `scale` messages over all of them, and the generator for more mail.
fn servers(scale: u32) -> (Vec<RecordingOps>, Rng) {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let per_account = scale / ACCOUNTS.len() as u32;
    let servers = ACCOUNTS
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
    (servers, rng)
}

/// Generates `scale` messages over every account and times each phase `runs` times.
fn run(scale: u32, runs: usize) {
    let fx = Fixture::new(&ACCOUNTS);
    let (mut servers, mut rng) = servers(scale);
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

    // Opening a large message: what reading and parsing it costs on the reader thread, and how long the frame that
    // chose it waits now. The top two rows get the same 5 MB message, and each run steps between them.
    let large = large_message();
    let keys: Vec<_> = driver.app.list.rows[..2]
        .iter()
        .map(|row| (row.folder.clone(), row.uid))
        .collect();
    for (folder, uid) in &keys {
        stores[0].set_raw(folder, *uid, &large, "large").unwrap();
    }
    report(
        "open_message",
        scale,
        (0..runs)
            .map(|_| timed(|| drop(reader::read(&stores[0], &keys[0]))))
            .collect(),
    );
    driver.app.body_budget = BODY_BUDGET;
    let frames = (0..runs).map(|run| {
        let ms = timed(|| driver.press(if run % 2 == 0 { "j" } else { "k" }));
        while driver
            .app
            .body
            .as_ref()
            .is_some_and(|b| b.reading.is_some())
        {
            std::thread::sleep(std::time::Duration::from_millis(1));
            driver.tick();
        }
        ms
    });
    report("open_message_frame", scale, frames.collect());
    driver.app.body_budget = std::time::Duration::from_secs(60);
    assert!(driver.app.body.as_ref().is_some_and(|b| b.html.is_some()));

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

    // A keystroke in the search field with the window's own budget: how long the frame that takes it waits. The
    // search that outlasts the budget finishes before the next run.
    driver.app.list_budget = LIST_BUDGET;
    let keystrokes = (0..runs).map(|_| {
        let ms = timed(|| driver.apply(UiAction::SearchFor("invoice".into())));
        while driver.app.list_loading.is_some() {
            std::thread::sleep(std::time::Duration::from_millis(1));
            driver.tick();
        }
        ms
    });
    report("search_keystroke", scale, keystrokes.collect());
    driver.app.list_budget = std::time::Duration::from_secs(60);
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

    // What the rules pass reads of INBOX on every pass, once the indexer stored each body's text: the generated mail has
    // no bodies, so each message gets 4 kB of text, about a plain newsletter's. With the text, as before for a folder
    // whose rules test no body, and without it, as now.
    let text =
        "A line of newsletter text with a link to https://example.com/item.\n".repeat(4096 / 64);
    let inbox: Vec<u32> = stores[0]
        .messages_without_body("INBOX", 0)
        .unwrap()
        .iter()
        .map(|m| m.uid)
        .collect();
    for uid in inbox {
        stores[0]
            .set_indexed("INBOX", uid, Some(&text), None, T0)
            .unwrap();
    }
    report(
        "rules_read_with_body",
        scale,
        (0..runs)
            .map(|_| timed(|| drop(stores[0].messages_in_folder("INBOX").unwrap())))
            .collect(),
    );
    report(
        "rules_read",
        scale,
        (0..runs)
            .map(|_| timed(|| drop(stores[0].messages_without_body("INBOX", 0).unwrap())))
            .collect(),
    );
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

/// The uids of the two messages `home` adds to the `home` account's INBOX, which `scripts/startup.py` opens.
const NEWSLETTER_UID: u32 = 1_000_000;
const PLAIN_UID: u32 = 1_000_001;

/// A mail home under `root` for `scripts/startup.py`: the benchmark's mail synced into the stores, an HTML newsletter
/// with an inline image and a plain message in `home`'s INBOX. The accounts reach no server: the host is this
/// machine's port 1 and the password command fails, so the daemon only ever retries.
fn write_home(root: &std::path::Path, scale: u32) {
    let home = super::fixture::Home::new(root, &ACCOUNTS);
    let config: String = ACCOUNTS
        .iter()
        .map(|name| {
            format!(
                "[[accounts]]\nname = \"{name}\"\nhost = \"127.0.0.1\"\nport = 1\nusername = \"{name}@example.com\"\n\
                 password = {{ command = \"false\" }}\n\n"
            )
        })
        .collect();
    std::fs::write(home.paths.config_file(), config).unwrap();
    let (mut servers, _) = servers(scale);
    for (ops, account) in servers.iter_mut().zip(ACCOUNTS) {
        sync_all(ops, &home.store(account)).unwrap();
    }
    let store = home.store("home");
    for (uid, subject, raw, text) in [
        (NEWSLETTER_UID, "The weekly newsletter", newsletter(), "newsletter"),
        (
            PLAIN_UID,
            "A plain message",
            b"Subject: A plain message\r\nContent-Type: text/plain\r\n\r\nA few lines of plain text.\r\n".to_vec(),
            "A few lines of plain text.",
        ),
    ] {
        let message = super::fixture::message("INBOX", uid, subject);
        store.insert_message(&message).unwrap();
        store.set_raw("INBOX", uid, &raw, text).unwrap();
    }
}

/// An HTML newsletter: a style sheet, a 600 × 200 banner as a `cid:` image, and 30 sections laid out in tables, as
/// mailing tools write them. Short enough to stay under the 32,767 physical pixels at which the software renderer
/// panics (#321) even at 2x: 150 sections were over it at 1x.
fn newsletter() -> Vec<u8> {
    let section = |i: usize| {
        format!(
            "<table width=\"600\" cellpadding=\"12\"><tr><td class=\"card\">\
             <h2>{i}. {} and {}</h2><p>{}</p><p>{}</p>\
             <a class=\"button\" href=\"https://example.com/story/{i}\">Read more</a></td></tr></table>\n",
            WORDS[i % WORDS.len()],
            WORDS[(i * 7 + 3) % WORDS.len()],
            "A paragraph of newsletter text, long enough to wrap over a few lines in the reader. ".repeat(6),
            "<b>Bold</b>, <i>italic</i> and <a href=\"https://example.com/link\">a link</a> in a second paragraph. "
                .repeat(4),
        )
    };
    let html = format!(
        "<html><head><style>body {{ font-family: sans-serif; background: #f4f4f4; }} \
         .card {{ background: #fff; border-radius: 8px; }} h2 {{ color: #234; }} \
         .button {{ background: #2a6; color: #fff; padding: 8px 16px; text-decoration: none; }}</style></head>\
         <body><center><img src=\"cid:banner\" width=\"600\" height=\"200\" alt=\"Banner\">{}</center></body></html>",
        (1..=30).map(section).collect::<String>()
    );
    let mut png = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png, 600, 200);
        encoder.set_color(png::ColorType::Rgb);
        let mut writer = encoder.write_header().unwrap();
        let pixels: Vec<u8> = (0..200u32)
            .flat_map(|y| {
                (0..600u32).flat_map(move |x| [(x * 255 / 600) as u8, (y * 255 / 200) as u8, 160])
            })
            .collect();
        writer.write_image_data(&pixels).unwrap();
    }
    // The base64 helper takes whole groups of three bytes.
    png.resize(png.len().div_ceil(3) * 3, 0);
    let image = base64(&png);
    let lines: Vec<&str> = image
        .as_bytes()
        .chunks(76)
        .map(|line| std::str::from_utf8(line).unwrap())
        .collect();
    format!(
        "From: News <news@example.com>\r\nTo: me@example.com\r\nSubject: The weekly newsletter\r\nMIME-Version: 1.0\r\n\
         Content-Type: multipart/related; boundary=\"b\"\r\n\r\n--b\r\nContent-Type: text/html; charset=utf-8\r\n\r\n\
         {html}\r\n--b\r\nContent-Type: image/png\r\nContent-ID: <banner>\r\nContent-Transfer-Encoding: base64\r\n\r\n\
         {}\r\n--b--\r\n",
        lines.join("\r\n")
    )
    .into_bytes()
}

#[test]
fn a_written_home_has_the_mail_and_the_two_messages() {
    let dir = tempfile::tempdir().unwrap();
    write_home(dir.path(), 300);
    let paths = postvak_core::paths::Paths::under(dir.path());
    let config = postvak_core::config::Config::load(&paths.config_file()).unwrap();
    assert_eq!(config.accounts.len(), ACCOUNTS.len());
    let store = Store::open(&paths.mail_db("home")).unwrap();
    assert!(store.message_count("INBOX").unwrap() > 2);
    let newsletter = reader::read(&store, &("INBOX".to_string(), NEWSLETTER_UID))
        .html
        .unwrap();
    assert!(newsletter.html.len() > 20_000);
    assert_eq!(newsletter.inline.len(), 1, "the banner");
    let plain = reader::read(&store, &("INBOX".to_string(), PLAIN_UID));
    assert!(plain.message.is_some() && plain.html.is_none());
}

#[test]
#[ignore = "writes a mail home for scripts/startup.py into POSTVAK_PERF_HOME"]
fn home() {
    let root =
        std::env::var("POSTVAK_PERF_HOME").expect("POSTVAK_PERF_HOME names the directory to write");
    let scale = std::env::var("POSTVAK_BENCH_SCALE").map_or(10_000, |scale| scale.parse().unwrap());
    write_home(std::path::Path::new(&root), scale);
}
