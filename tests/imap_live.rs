//! Live IMAP tests against tests/dovecot/compose.yml, skipped unless POSTVAK_TEST_IMAP_HOST is set.

// Test code: unwrap, expect and panic are how a test fails.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use postvak_core::config::{self, AccountConfig, Config, PasswordSource};
use postvak_core::credentials::Secret;
use postvak_core::engine::{Engine, imap_connector};
use postvak_core::mail_ops::imap::ImapOps;
use postvak_core::mail_ops::{Envelope, IdleOutcome, MailOps};
use postvak_core::paths::Paths;
use postvak_core::rules::Action;
use postvak_core::store::Store;
use postvak_core::sync::{self, Activity, Command, Event};

const PASSWORD: &str = "postvak-test";
const PORT: u16 = 10993;
const PORT_WITHOUT_MOVE: u16 = 11993;

/// CI sets the variable to an empty string where no server runs; that counts as unset.
fn host() -> Option<String> {
    let host = std::env::var("POSTVAK_TEST_IMAP_HOST")
        .ok()
        .filter(|h| !h.is_empty());
    if host.is_none() {
        eprintln!("POSTVAK_TEST_IMAP_HOST unset; live IMAP test skipped");
    }
    host
}

fn all_envelopes(ops: &mut ImapOps) -> Vec<Envelope> {
    let uids = ops.search_uids(1).unwrap();
    match (uids.first(), uids.last()) {
        (Some(&first), Some(&last)) => ops.fetch_envelopes(first, last).unwrap(),
        _ => Vec::new(),
    }
}

/// A fresh user per call, so no two tests (or reruns) share a mailbox; Dovecot accepts any name.
fn account(host: &str, port: u16, test: &str) -> AccountConfig {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let password = PasswordSource::Command {
        command: format!("printf {PASSWORD}"),
    };
    let username = format!("{test}-{nanos}@example.com");
    AccountConfig {
        port,
        notify: false,
        ca_file: Some(PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/dovecot/certs/ca.pem"
        ))),
        ..AccountConfig::new("live", host, &username, password)
    }
}

fn connect(account: &AccountConfig) -> ImapOps {
    ImapOps::connect(account, &Secret::new(PASSWORD.into())).unwrap_or_else(|e| panic!("{e}"))
}

fn mail(subject: &str) -> Vec<u8> {
    format!(
        "From: Sender <sender@example.com>\r\nTo: user@example.com\r\nSubject: {subject}\r\n\
         Message-ID: <{}@example.com>\r\nDate: Tue, 06 Oct 2026 10:00:00 +0000\r\n\r\nBody of {subject}\r\n",
        subject.replace(' ', ".")
    )
    .into_bytes()
}

#[test]
fn lists_special_use_folders() {
    let Some(host) = host() else { return };
    let folders = connect(&account(&host, PORT, "special-use"))
        .list_folders()
        .unwrap();
    for role in ["Archive", "Drafts", "Junk", "Sent", "Trash"] {
        assert!(
            folders
                .iter()
                .any(|f| f.special_use.as_deref() == Some(role)),
            "{role}: {folders:?}"
        );
    }
}

#[test]
fn lists_the_hierarchy_delimiter() {
    let Some(host) = host() else { return };
    let folders = connect(&account(&host, PORT, "delimiter"))
        .list_folders()
        .unwrap();
    assert!(
        !folders.is_empty() && folders.iter().all(|f| f.delimiter.is_some()),
        "{folders:?}"
    );
}

#[test]
fn append_fetch_and_flags_round_trip() {
    let Some(host) = host() else { return };
    let mut ops = connect(&account(&host, PORT, "flags"));
    ops.append("INBOX", &mail("hello"), &["$PostvakRestored"])
        .unwrap();
    ops.select("INBOX").unwrap();
    let new = all_envelopes(&mut ops);
    assert_eq!(new.len(), 1);
    assert!(
        new[0].flags.iter().any(|f| f == "$PostvakRestored"),
        "{:?}",
        new[0].flags
    );
    let uid = new[0].uid;
    ops.add_flags(uid, &["\\Seen", "\\Flagged"]).unwrap();
    ops.remove_flags(uid, &["\\Flagged"]).unwrap();
    let flags = ops.fetch_flags(uid, None).unwrap().remove(0).flags;
    assert!(flags.iter().any(|f| f == "\\Seen"), "{flags:?}");
    assert!(!flags.iter().any(|f| f == "\\Flagged"), "{flags:?}");
    assert_eq!(ops.fetch_raw(uid).unwrap().unwrap(), mail("hello"));
}

/// Appends a fixture under `tests/fixtures/structure/`, then fetches its structure and the text of its parts.
fn structure_round_trip(port: u16, name: &str) -> (postvak_core::mail_ops::Structure, String) {
    let Some(host) = host() else {
        return (postvak_core::mail_ops::Structure::default(), String::new());
    };
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/structure")
        .join(format!("{name}.eml"));
    let raw = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut ops = connect(&account(&host, port, &format!("structure-{name}")));
    ops.append("INBOX", &raw, &[]).unwrap();
    ops.select("INBOX").unwrap();
    let uid = all_envelopes(&mut ops)[0].uid;
    let structure = ops
        .fetch_structures(&[uid])
        .unwrap()
        .remove(&uid)
        .expect("a structure");
    let parts: Vec<_> = structure
        .text_parts
        .iter()
        .map(|part| {
            let bytes = ops
                .fetch_part(uid, &part.section)
                .unwrap()
                .unwrap_or_else(|| panic!("no bytes for section {:?}", part.section));
            (part.clone(), bytes)
        })
        .collect();
    (structure, postvak_core::message::text_from_parts(&parts))
}

#[test]
fn structure_of_an_alternative_message_yields_its_plain_text() {
    for port in [PORT, PORT_WITHOUT_MOVE] {
        let (structure, text) = structure_round_trip(port, "alternative");
        if host().is_none() {
            return;
        }
        let sections: Vec<_> = structure
            .text_parts
            .iter()
            .map(|p| (p.section.as_str(), p.html, p.alternative))
            .collect();
        assert_eq!(sections, [("1", false, Some(1)), ("2", true, Some(1))]);
        assert_eq!(text.trim(), "plain pineapple");
        assert!(structure.attachment_names.is_empty());
    }
}

#[test]
fn structure_of_a_mixed_message_names_its_attachment() {
    let (structure, text) = structure_round_trip(PORT, "mixed");
    if host().is_none() {
        return;
    }
    assert_eq!(structure.attachment_names, ["Q3 report.pdf"]);
    assert_eq!(structure.text_parts.len(), 1);
    assert_eq!(text.trim(), "see attached");
}

#[test]
fn structure_of_a_single_part_decodes_its_charset_and_encoding() {
    let (structure, text) = structure_round_trip(PORT, "latin1");
    if host().is_none() {
        return;
    }
    assert_eq!(structure.text_parts[0].section, "");
    assert_eq!(
        structure.text_parts[0].charset.as_deref(),
        Some("iso-8859-1")
    );
    assert_eq!(text.trim(), "café");
}

#[test]
fn structure_of_a_message_without_text_has_only_names() {
    let (structure, text) = structure_round_trip(PORT, "pdf-only");
    if host().is_none() {
        return;
    }
    assert!(structure.text_parts.is_empty());
    assert_eq!(structure.attachment_names, ["a.pdf"]);
    assert_eq!(text, "");
}

#[test]
fn search_text_needs_every_word_in_headers_or_body() {
    let Some(host) = host() else { return };
    let mut ops = connect(&account(&host, PORT, "search-text"));
    for subject in ["apple pie", "apple tart", "café crème"] {
        ops.append("INBOX", &mail(subject), &[]).unwrap();
    }
    ops.select("INBOX").unwrap();
    let uids: Vec<u32> = all_envelopes(&mut ops).iter().map(|e| e.uid).collect();
    let search = |ops: &mut ImapOps, words: &[&str]| {
        let words: Vec<String> = words.iter().map(|w| w.to_string()).collect();
        ops.search_text(&words).unwrap()
    };
    assert_eq!(search(&mut ops, &["APPLE"]), uids[..2]);
    assert_eq!(search(&mut ops, &["apple", "Body", "tart"]), [uids[1]]);
    assert_eq!(search(&mut ops, &["crème"]), [uids[2]]);
    assert_eq!(search(&mut ops, &["say \"hi\""]), Vec::<u32>::new());
}

#[test]
fn condstore_fetches_only_flags_changed_since_a_modseq() {
    let Some(host) = host() else { return };
    let mut ops = connect(&account(&host, PORT, "condstore"));
    ops.append("INBOX", &mail("one"), &[]).unwrap();
    ops.append("INBOX", &mail("two"), &[]).unwrap();
    let before = ops.select("INBOX").unwrap();
    assert_eq!(before.exists, 2);
    let since = before.highest_modseq.expect("Dovecot advertises CONDSTORE");
    let uids: Vec<u32> = all_envelopes(&mut ops).iter().map(|e| e.uid).collect();
    ops.add_flags(uids[1], &["\\Seen"]).unwrap();
    let changed = ops.fetch_flags(uids[1], Some(since)).unwrap();
    assert_eq!(changed.len(), 1, "{changed:?}");
    assert_eq!(changed[0].uid, uids[1]);
    assert!(ops.select("INBOX").unwrap().highest_modseq > Some(since));
}

#[test]
fn list_status_and_status_report_what_select_does() {
    let Some(host) = host() else { return };
    for (port, list_status) in [(PORT, true), (PORT_WITHOUT_MOVE, false)] {
        let mut ops = connect(&account(&host, port, "status"));
        assert_eq!(ops.has_list_status(), list_status, "port {port}");
        ops.create_folder("Lists").unwrap();
        ops.append("Lists", &mail("one"), &[]).unwrap();
        ops.append("Lists", &mail("two"), &[]).unwrap();
        ops.select("INBOX").unwrap();
        let listed = ops.list_folders_status().unwrap();
        let (_, from_list) = listed
            .iter()
            .find(|(f, _)| f.name == "Lists")
            .unwrap_or_else(|| panic!("{listed:?}"));
        assert_eq!(from_list.is_some(), list_status, "port {port}: {listed:?}");
        let status = ops.status("Lists").unwrap();
        if let Some(from_list) = from_list {
            assert_eq!(from_list, &status);
        }
        let selected = ops.select("Lists").unwrap();
        assert_eq!(ops.selected(), Some("Lists"));
        assert_eq!(status.messages, selected.exists);
        assert_eq!(status.uidvalidity, selected.uidvalidity);
        assert_eq!(Some(status.uidnext), selected.uidnext);
        assert_eq!(status.highest_modseq, selected.highest_modseq);
        assert!(status.highest_modseq.is_some(), "Dovecot has CONDSTORE");
        assert!(ops.select("Nope").is_err());
        assert_eq!(ops.selected(), None, "a failed SELECT selects nothing");
    }
}

/// A folder a rule creates is subscribed, so clients that list only subscribed folders show it.
#[test]
fn a_created_folder_can_be_subscribed() {
    let Some(host) = host() else { return };
    for port in [PORT, PORT_WITHOUT_MOVE] {
        let mut ops = connect(&account(&host, port, "subscribe"));
        ops.create_folder("Lists").unwrap();
        ops.subscribe_folder("Lists")
            .unwrap_or_else(|e| panic!("port {port}: {e}"));
    }
}

/// Mail another client adds to a folder the pass skips by its status still shows up, with or without LIST-STATUS.
#[test]
fn a_pass_finds_changes_another_client_made_in_a_skipped_folder() {
    let Some(host) = host() else { return };
    for port in [PORT, PORT_WITHOUT_MOVE] {
        let account = account(&host, port, "skip");
        let mut ops = connect(&account);
        let mut other = connect(&account);
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("mail.db")).unwrap();
        ops.create_folder("Lists").unwrap();
        ops.append("Lists", &mail("first"), &[]).unwrap();
        sync::sync_all(&mut ops, &store).unwrap();
        ops.select("INBOX").unwrap();
        let uid_next = store.uid_next("Lists").unwrap();
        assert_ne!(uid_next, 0, "port {port}");

        let (_, errors) = sync::sync_all(&mut ops, &store).unwrap();
        assert!(errors.is_empty(), "port {port}: {errors:?}");
        assert_eq!(store.uid_next("Lists").unwrap(), uid_next, "port {port}");

        other.append("Lists", &mail("second"), &[]).unwrap();
        other.select("Lists").unwrap();
        let first = all_envelopes(&mut other)[0].uid;
        other.add_flags(first, &["\\Seen"]).unwrap();
        ops.select("INBOX").unwrap();
        let (new, errors) = sync::sync_all(&mut ops, &store).unwrap();
        assert!(errors.is_empty(), "port {port}: {errors:?}");
        assert_eq!(new, 1, "port {port}");
        assert_eq!(store.message_count("Lists").unwrap(), 2);
        assert!(store.message("Lists", first).unwrap().unwrap().is_seen());
    }
}

fn move_round_trip(port: u16, test: &str, expect_move: bool) {
    let Some(host) = host() else { return };
    let mut ops = connect(&account(&host, port, test));
    assert_eq!(
        ops.has_move(),
        expect_move,
        "server capabilities differ from compose.yml"
    );
    ops.append("INBOX", &mail("move me"), &[]).unwrap();
    ops.select("INBOX").unwrap();
    let uid = all_envelopes(&mut ops)[0].uid;
    ops.move_message(uid, "Archive").unwrap();
    assert!(
        all_envelopes(&mut ops).is_empty(),
        "message is still in INBOX"
    );
    ops.select("Archive").unwrap();
    assert_eq!(all_envelopes(&mut ops).len(), 1);
}

#[test]
fn move_uses_move_when_advertised() {
    move_round_trip(PORT, "move", true);
}

#[test]
fn move_falls_back_to_copy_without_move() {
    move_round_trip(PORT_WITHOUT_MOVE, "copy", false);
}

#[test]
fn many_messages_move_in_one_command() {
    let Some(host) = host() else { return };
    for (port, test) in [(PORT, "batch-move"), (PORT_WITHOUT_MOVE, "batch-copy")] {
        let mut ops = connect(&account(&host, port, test));
        for subject in ["one", "two", "three"] {
            ops.append("INBOX", &mail(subject), &[]).unwrap();
        }
        ops.select("INBOX").unwrap();
        let uids: Vec<u32> = all_envelopes(&mut ops).iter().map(|e| e.uid).collect();
        ops.move_messages(&uids[..2], "Archive").unwrap();
        let left: Vec<u32> = all_envelopes(&mut ops).iter().map(|e| e.uid).collect();
        assert_eq!(left, vec![uids[2]], "{test}");
        ops.select("Archive").unwrap();
        assert_eq!(all_envelopes(&mut ops).len(), 2, "{test}");
    }
}

#[test]
fn many_messages_take_flags_and_expunge_in_one_command() {
    let Some(host) = host() else { return };
    for (port, test) in [(PORT, "batch-uidplus"), (PORT_WITHOUT_MOVE, "batch-plain")] {
        let mut ops = connect(&account(&host, port, test));
        for subject in ["one", "two", "three"] {
            ops.append("INBOX", &mail(subject), &[]).unwrap();
        }
        ops.select("INBOX").unwrap();
        let uids: Vec<u32> = all_envelopes(&mut ops).iter().map(|e| e.uid).collect();
        ops.add_flags_to(&uids, &["\\Seen"]).unwrap();
        ops.remove_flags_from(&uids[..1], &["\\Seen"]).unwrap();
        let seen: Vec<bool> = all_envelopes(&mut ops)
            .iter()
            .map(|e| e.flags.iter().any(|f| f == "\\Seen"))
            .collect();
        assert_eq!(seen, [false, true, true], "{test}");
        ops.add_flags_to(&uids[1..], &["\\Deleted"]).unwrap();
        ops.expunge_all(&uids[1..]).unwrap();
        let left: Vec<u32> = all_envelopes(&mut ops).iter().map(|e| e.uid).collect();
        assert_eq!(left, vec![uids[0]], "{test}");
    }
}

#[test]
fn expunge_removes_only_the_deleted_message() {
    let Some(host) = host() else { return };
    for (port, test) in [
        (PORT, "expunge-uidplus"),
        (PORT_WITHOUT_MOVE, "expunge-plain"),
    ] {
        let mut ops = connect(&account(&host, port, test));
        ops.append("INBOX", &mail("first"), &[]).unwrap();
        ops.append("INBOX", &mail("second"), &[]).unwrap();
        ops.select("INBOX").unwrap();
        let uids: Vec<u32> = all_envelopes(&mut ops).iter().map(|e| e.uid).collect();
        ops.add_flags(uids[0], &["\\Deleted"]).unwrap();
        ops.expunge(uids[0]).unwrap();
        let left: Vec<u32> = all_envelopes(&mut ops).iter().map(|e| e.uid).collect();
        assert_eq!(left, vec![uids[1]], "{test}");
    }
}

#[test]
fn plain_expunge_refuses_while_another_message_is_marked_deleted() {
    let Some(host) = host() else { return };
    let mut ops = connect(&account(&host, PORT_WITHOUT_MOVE, "expunge-foreign"));
    ops.append("INBOX", &mail("flagged elsewhere"), &["\\Deleted"])
        .unwrap();
    ops.append("INBOX", &mail("ours"), &[]).unwrap();
    ops.select("INBOX").unwrap();
    let uids: Vec<u32> = all_envelopes(&mut ops).iter().map(|e| e.uid).collect();
    ops.add_flags(uids[1], &["\\Deleted"]).unwrap();
    assert!(ops.expunge(uids[1]).is_err());
    assert!(ops.move_message(uids[1], "Archive").is_err());
    let left: Vec<u32> = all_envelopes(&mut ops).iter().map(|e| e.uid).collect();
    assert_eq!(
        left, uids,
        "a message another client marked deleted was expunged"
    );
    ops.select("Archive").unwrap();
    assert!(
        all_envelopes(&mut ops).is_empty(),
        "the refused move left a copy in Archive"
    );
}

#[test]
fn a_refused_move_without_move_or_uidplus_changes_nothing() {
    let Some(host) = host() else { return };
    let mut ops = connect(&account(&host, PORT_WITHOUT_MOVE, "move-refused"));
    ops.append("INBOX", &mail("flagged elsewhere"), &["\\Deleted"])
        .unwrap();
    ops.append("INBOX", &mail("ours"), &[]).unwrap();
    ops.select("INBOX").unwrap();
    let ours = all_envelopes(&mut ops)[1].uid;
    assert!(ops.move_message(ours, "Archive").is_err());
    let flags: Vec<String> = ops
        .fetch_flags(ours, None)
        .unwrap()
        .into_iter()
        .find(|update| update.uid == ours)
        .map(|update| update.flags)
        .unwrap_or_default();
    assert!(
        !flags.iter().any(|flag| flag == "\\Deleted"),
        "the refused move left the source marked deleted: {flags:?}"
    );
    ops.select("Archive").unwrap();
    assert!(
        all_envelopes(&mut ops).is_empty(),
        "the refused move left a copy in Archive"
    );
}

#[test]
fn idle_wakes_when_mail_arrives() {
    let Some(host) = host() else { return };
    let account = account(&host, PORT, "idle");
    let mut ops = connect(&account);
    ops.select("INBOX").unwrap();
    let sender = account.clone();
    let delivery = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(1));
        connect(&sender)
            .append("INBOX", &mail("ping"), &[])
            .unwrap();
    });
    let started = Instant::now();
    let outcome = ops
        .idle(Duration::from_secs(60), &AtomicBool::new(false))
        .unwrap();
    delivery.join().unwrap();
    assert_eq!(outcome, IdleOutcome::NewMail);
    assert!(started.elapsed() < Duration::from_secs(30));
}

/// Mail newer than a rule's first pass is what the rule acts on; INTERNALDATE has one-second resolution.
fn wait_past_the_rule_clock() {
    std::thread::sleep(Duration::from_millis(1100));
}

fn postvak(home: &Path, args: &[&str]) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_postvak"))
        .args(args)
        .env("POSTVAK_HOME", home)
        .env("POSTVAK_IDLE_EXIT_SECS", "1")
        .env("RUST_LOG", "error")
        .output()
        .unwrap()
}

fn home_with(account: &AccountConfig, rules: &str) -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    let paths = Paths::under(home.path());
    config::save_account(&paths.config_file(), account).unwrap();
    if !rules.is_empty() {
        std::fs::write(paths.rules_file(), rules).unwrap();
    }
    home
}

#[test]
fn sync_resets_a_folder_whose_uidvalidity_changed() {
    let Some(host) = host() else { return };
    let mut ops = connect(&account(&host, PORT, "uidvalidity"));
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("mail.db")).unwrap();
    ops.create_folder("Lists").unwrap();
    ops.append("Lists", &mail("before"), &[]).unwrap();
    sync::sync_all(&mut ops, &store).unwrap();
    let before = store.folder("Lists").unwrap().unwrap().uidvalidity;

    ops.select("INBOX").unwrap();
    ops.delete_folder("Lists").unwrap();
    // Dovecot derives UIDVALIDITY from the clock.
    std::thread::sleep(Duration::from_millis(1100));
    ops.create_folder("Lists").unwrap();
    ops.append("Lists", &mail("after"), &[]).unwrap();
    let (_, errors) = sync::sync_all(&mut ops, &store).unwrap();
    assert!(errors.is_empty(), "{errors:?}");

    assert_ne!(store.folder("Lists").unwrap().unwrap().uidvalidity, before);
    let subjects: Vec<Option<String>> = store
        .messages_in_folder("Lists")
        .unwrap()
        .into_iter()
        .map(|m| m.subject)
        .collect();
    assert_eq!(subjects, vec![Some("after".to_string())]);
}

#[test]
fn rule_delete_keeps_a_backup_then_expunges() {
    let Some(host) = host() else { return };
    let account = account(&host, PORT, "rule-delete");
    let home = home_with(
        &account,
        "[[rules]]\nname = \"codes\"\nmatch.subject = { contains = \"sign-in code\" }\nactions = { delete = true }\n",
    );
    let paths = Paths::under(home.path());
    let (events, received) = std::sync::mpsc::channel();
    sync::run_once(&account, &paths, &events).unwrap();
    wait_past_the_rule_clock();
    connect(&account)
        .append("INBOX", &mail("Your sign-in code"), &[])
        .unwrap();
    sync::run_once(&account, &paths, &events).unwrap();
    drop(events);
    let errors: Vec<Event> = received
        .iter()
        .filter(|e| matches!(e, Event::Error { .. }))
        .collect();
    assert!(errors.is_empty(), "{errors:?}");

    let backups = std::fs::read_dir(paths.trash_dir(&account.name))
        .unwrap()
        .count();
    assert_eq!(backups, 1, "expected one .eml backup");
    let mut ops = connect(&account);
    for folder in ["INBOX", "Trash"] {
        ops.select(folder).unwrap();
        assert!(all_envelopes(&mut ops).is_empty(), "{folder} is not empty");
    }
}

/// A non-ASCII name goes out as modified UTF-7, which Dovecot insists on, and comes back as UTF-8.
#[test]
fn a_non_ascii_folder_is_created_listed_and_selected_by_its_utf8_name() {
    let Some(host) = host() else { return };
    let name = "Caf\u{e9} & \u{91cd}\u{8981}";
    let mut ops = connect(&account(&host, PORT, "utf7"));
    ops.create_folder(name).unwrap();
    ops.append(name, &mail("accented"), &[]).unwrap();
    let names: Vec<String> = ops
        .list_folders()
        .unwrap()
        .into_iter()
        .map(|f| f.name)
        .collect();
    assert!(names.iter().any(|n| n == name), "{names:?}");
    ops.select(name).unwrap();
    assert_eq!(all_envelopes(&mut ops).len(), 1);
}

/// A rule moving to a new folder written in NFD creates it in NFC, and the next sync stores it under its UTF-8 name.
#[test]
fn a_rule_moves_to_a_new_non_ascii_folder() {
    let Some(host) = host() else { return };
    let account = account(&host, PORT, "rule-utf7");
    let home = home_with(
        &account,
        "[[rules]]\nname = \"archive\"\nmatch.subject = { contains = \"receipt\" }\n\
         actions.move = \"Archief/E\u{301}e\u{301}n\"\n",
    );
    let paths = Paths::under(home.path());
    let (events, received) = std::sync::mpsc::channel();
    sync::run_once(&account, &paths, &events).unwrap();
    wait_past_the_rule_clock();
    connect(&account)
        .append("INBOX", &mail("Your receipt"), &[])
        .unwrap();
    sync::run_once(&account, &paths, &events).unwrap();
    sync::run_once(&account, &paths, &events).unwrap();
    drop(events);
    let errors: Vec<Event> = received
        .iter()
        .filter(|e| matches!(e, Event::Error { .. }))
        .collect();
    assert!(errors.is_empty(), "{errors:?}");

    let nfc = "Archief/\u{c9}\u{e9}n";
    let mut ops = connect(&account);
    ops.select(nfc).unwrap();
    assert_eq!(all_envelopes(&mut ops).len(), 1);
    let store = Store::open(&paths.mail_db(&account.name)).unwrap();
    let folder = store.folder(nfc).unwrap().expect("the folder is stored");
    assert_ne!(folder.uidvalidity, 0, "synced, not a placeholder");
    assert_eq!(store.message_count(nfc).unwrap(), 1);
}

#[test]
fn cli_delete_moves_to_the_trash_folder() {
    let Some(host) = host() else { return };
    let account = account(&host, PORT, "cli-delete");
    let home = home_with(&account, "");
    connect(&account)
        .append("INBOX", &mail("old newsletter"), &[])
        .unwrap();
    for args in [&["sync"][..], &["delete", "1"][..]] {
        let out = postvak(home.path(), args);
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let mut ops = connect(&account);
    ops.select("INBOX").unwrap();
    assert!(all_envelopes(&mut ops).is_empty(), "still in INBOX");
    ops.select("Trash").unwrap();
    assert_eq!(all_envelopes(&mut ops).len(), 1);
}

#[test]
fn cli_actions_are_logged_as_cli_through_the_daemon() {
    let Some(host) = host() else { return };
    let account = account(&host, PORT, "cli-logged");
    let home = home_with(&account, "");
    connect(&account)
        .append("INBOX", &mail("file me"), &[])
        .unwrap();
    for args in [&["sync"][..], &["archive", "1"][..]] {
        let out = postvak(home.path(), args);
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let paths = Paths::under(home.path());
    let store = Store::open(&paths.mail_db(&account.name)).unwrap();
    assert_eq!(store.log(1).unwrap()[0].rule_name, "cli");
    let pid = std::fs::read_to_string(paths.daemon_lock()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while paths.daemon_socket().exists() || alive(pid.trim()) {
        assert!(
            Instant::now() < deadline,
            "the auto-started daemon (pid {pid}) did not idle out"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn alive(pid: &str) -> bool {
    std::process::Command::new("kill")
        .args(["-0", pid])
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .success()
}

#[test]
fn cli_applies_a_rule_to_existing_mail_and_restores_it_through_the_daemon() {
    let Some(host) = host() else { return };
    let account = account(&host, PORT, "cli-restore");
    let home = home_with(
        &account,
        "[[rules]]\nname = \"codes\"\nmatch.subject = { contains = \"sign-in code\" }\nactions = { delete = true }\n",
    );
    connect(&account)
        .append("INBOX", &mail("Your sign-in code"), &[])
        .unwrap();
    wait_past_the_rule_clock();
    let success = |args: &[&str]| {
        let out = postvak(home.path(), args);
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    };
    success(&["sync"]);
    // The body may not be indexed yet right after the first sync; the server search finds it either way.
    assert!(success(&["search", "--server", "Body"]).contains("INBOX/1"));
    assert!(success(&["show", "1", "--raw"]).contains("Body of Your sign-in code"));
    assert!(success(&["rules", "apply-existing", "codes", "--yes"]).contains("live: 1 actions on"));

    let trash_dir = Paths::under(home.path()).trash_dir(&account.name);
    let backup = std::fs::read_dir(&trash_dir)
        .unwrap()
        .next()
        .unwrap()
        .unwrap();
    let restored = std::process::Command::new(env!("CARGO_BIN_EXE_postvak"))
        .args(["trash", "restore"])
        .arg(backup.file_name())
        .current_dir(&trash_dir)
        .env("POSTVAK_HOME", home.path())
        .env("POSTVAK_IDLE_EXIT_SECS", "1")
        .output()
        .unwrap();
    assert!(
        restored.status.success(),
        "{}",
        String::from_utf8_lossy(&restored.stderr)
    );
    let mut ops = connect(&account);
    ops.select("INBOX").unwrap();
    assert_eq!(all_envelopes(&mut ops).len(), 1);
}

#[test]
fn sync_exits_nonzero_when_a_rule_fails_on_a_message() {
    let Some(host) = host() else { return };
    let account = account(&host, PORT, "cli-sync-error");
    // Rules allow 255 characters, but these are 510 bytes: Dovecot refuses the mailbox name, so the move fails for
    // this message only.
    let target = "é".repeat(255);
    let home = home_with(
        &account,
        &format!(
            "[[rules]]\nname = \"impossible\"\nmatch.subject = {{ contains = \"move me\" }}\nactions = {{ move = \"{target}\" }}\n"
        ),
    );
    let first = postvak(home.path(), &["sync"]);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    wait_past_the_rule_clock();
    connect(&account)
        .append("INBOX", &mail("please move me"), &[])
        .unwrap();
    let out = postvak(home.path(), &["sync"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{stderr}");
    assert!(
        stderr.contains("sync failed for at least one account or folder"),
        "{stderr}"
    );
}

#[test]
fn account_add_accepts_a_private_ca_file() {
    use std::io::Write;
    use std::process::Stdio;

    let Some(host) = host() else { return };
    let template = account(&host, PORT, "account-add");
    let ca_file = template.ca_file.as_ref().unwrap().display().to_string();
    let home = tempfile::tempdir().unwrap();
    let answers = format!(
        "live\n{host}\n{PORT}\n{ca_file}\n{}\n\nc\nprintf {PASSWORD}\n",
        template.username
    );
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_postvak"))
        .args(["account", "add"])
        .env("POSTVAK_HOME", home.path())
        .env("RUST_LOG", "error")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(answers.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let saved = Config::load(&Paths::under(home.path()).config_file()).unwrap();
    assert_eq!(saved.accounts[0].ca_file, template.ca_file);
}

#[test]
fn first_sync_of_a_large_folder_runs_in_chunks() {
    let Some(host) = host() else { return };
    let account = account(&host, PORT, "chunks");
    let mut ops = connect(&account);
    for i in 0..12 {
        ops.append("INBOX", &mail(&format!("bulk {i}")), &[])
            .unwrap();
    }
    let home = tempfile::tempdir().unwrap();
    let paths = Paths::under(home.path());
    let (tx, rx) = std::sync::mpsc::channel();
    sync::CHUNK_OVERRIDE.set(Some(5));
    sync::run_once(&account, &paths, &tx).unwrap();
    drop(tx);
    let progress: Vec<usize> = rx
        .iter()
        .filter_map(|e| match e {
            Event::Activity {
                activity:
                    Activity::FetchingHeaders {
                        folder,
                        done,
                        total,
                    },
                ..
            } if folder == "INBOX" => {
                assert_eq!(total, 12);
                Some(done)
            }
            _ => None,
        })
        .collect();
    assert_eq!(progress, [0, 5, 10, 12]);
    let store = Store::open(&paths.mail_db(&account.name)).unwrap();
    assert_eq!(store.message_count("INBOX").unwrap(), 12);
}

#[test]
fn a_command_wakes_idle_and_runs_within_two_seconds() {
    let Some(host) = host() else { return };
    let account = account(&host, PORT, "wake");
    connect(&account)
        .append("INBOX", &mail("wake me"), &[])
        .unwrap();
    let home = tempfile::tempdir().unwrap();
    let config = Config {
        accounts: vec![account.clone()],
        ..Default::default()
    };
    let (engine, events) = Engine::start(&config, &Paths::under(home.path()), imap_connector());
    let wait = |wanted: &dyn Fn(&Event) -> bool| {
        loop {
            let event = events
                .recv_timeout(Duration::from_secs(30))
                .expect("event within 30 s");
            if wanted(&event) {
                return event;
            }
        }
    };
    wait(&|e| {
        matches!(
            e,
            Event::Activity {
                activity: Activity::Idle { .. },
                ..
            }
        )
    });
    let sent = Instant::now();
    assert!(engine.send(
        &account.name,
        1,
        Command::Apply {
            folder: "INBOX".into(),
            uids: vec![1],
            action: Action::MarkRead,
            by: "gui".into(),
        }
    ));
    let done = wait(&|e| matches!(e, Event::ActionDone { .. }));
    assert!(
        sent.elapsed() < Duration::from_secs(2),
        "{:?}",
        sent.elapsed()
    );
    assert!(matches!(done, Event::ActionDone { results, .. } if results == [(1, Ok(1))]));
    drop(engine);
}

/// The binary behind a script that sets the idle exit to 1 s; tests cannot set environment variables without unsafe.
#[cfg(feature = "mcp")]
fn idling_exe() -> &'static Path {
    use std::sync::OnceLock;
    static SCRIPT: OnceLock<(tempfile::TempDir, PathBuf)> = OnceLock::new();
    &SCRIPT
        .get_or_init(|| {
            use std::os::unix::fs::PermissionsExt;
            let dir = tempfile::tempdir().unwrap();
            let script = dir.path().join("postvak");
            let text = format!(
                "#!/bin/sh\nPOSTVAK_IDLE_EXIT_SECS=1 exec '{}' \"$@\"\n",
                env!("CARGO_BIN_EXE_postvak")
            );
            std::fs::write(&script, text).unwrap();
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
            (dir, script)
        })
        .1
}

/// A backend on a daemon this test starts, which exits a second after the backend is dropped.
#[cfg(feature = "mcp")]
fn backend(home: &Path) -> postvak_mcp::Backend {
    let paths = Paths::under(home);
    let config = Config::load(&paths.config_file()).unwrap();
    let client = postvak_core::daemon::Client::connect_or_start_with(
        &paths,
        idling_exe(),
        postvak_core::daemon::wire::VERSION,
    )
    .unwrap();
    postvak_mcp::Backend::with_client(&config, &paths, &[], client).unwrap()
}

#[cfg(feature = "mcp")]
#[test]
fn mcp_archive_moves_the_message() {
    let Some(host) = host() else { return };
    let account = account(&host, PORT, "mcp-archive");
    let home = home_with(&account, "");
    connect(&account)
        .append("INBOX", &mail("to archive"), &[])
        .unwrap();
    let backend = backend(home.path());
    backend.sync(None).unwrap();
    let report = backend
        .act(None, "INBOX", &[1], Action::Archive, false, "mcp:test-host")
        .unwrap();
    assert_eq!(report["done"], 1, "{report}");
    let store = Store::open(&Paths::under(home.path()).mail_db(&account.name)).unwrap();
    assert_eq!(store.log(1).unwrap()[0].rule_name, "mcp:test-host");
    let mut ops = connect(&account);
    ops.select("INBOX").unwrap();
    assert!(all_envelopes(&mut ops).is_empty());
    ops.select("Archive").unwrap();
    assert_eq!(all_envelopes(&mut ops).len(), 1);
}

#[cfg(feature = "mcp")]
#[test]
fn mcp_delete_inside_trash_expunges_and_keeps_the_eml() {
    let Some(host) = host() else { return };
    let account = account(&host, PORT_WITHOUT_MOVE, "mcp-delete");
    let home = home_with(&account, "");
    let mut ops = connect(&account);
    ops.append("Trash", &mail("already trashed"), &[]).unwrap();
    let backend = backend(home.path());
    backend.sync(None).unwrap();
    let report = backend
        .act(None, "Trash", &[1], Action::Trash, false, "mcp")
        .unwrap();
    assert_eq!(report["done"], 1, "{report}");
    let backups = std::fs::read_dir(Paths::under(home.path()).trash_dir(&account.name))
        .unwrap()
        .count();
    assert_eq!(backups, 1);
    ops.select("Trash").unwrap();
    assert!(all_envelopes(&mut ops).is_empty());
}

/// The server runs tools on a blocking-pool thread, where ImapOps' own runtime must still start.
#[cfg(feature = "mcp")]
#[tokio::test]
async fn mcp_actions_run_from_a_blocking_pool_thread() {
    let Some(host) = host() else { return };
    let account = account(&host, PORT, "mcp-pool");
    let home = home_with(&account, "");
    let backend = backend(home.path());
    let report = tokio::task::spawn_blocking(move || {
        connect(&account)
            .append("INBOX", &mail("from the pool"), &[])
            .unwrap();
        backend.sync(None).unwrap();
        backend
            .act(None, "INBOX", &[1], Action::MarkRead, false, "mcp")
            .unwrap()
    })
    .await
    .unwrap();
    assert_eq!(report["done"], 1, "{report}");
}

/// `postvak account add` with a password command, answering the wizard in prompt order.
fn account_add(home: &Path, host: &str, username: &str, password: &str) -> std::process::Output {
    use std::io::Write;
    let ca = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/dovecot/certs/ca.pem");
    let answers = format!("live\n{host}\n{PORT}\n{ca}\n{username}\n\nc\nprintf {password}\n");
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_postvak"))
        .args(["account", "add"])
        .env("POSTVAK_HOME", home)
        .env("POSTVAK_IDLE_EXIT_SECS", "1")
        .env("RUST_LOG", "error")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(answers.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn account_add_tests_the_login_before_it_saves() {
    let Some(host) = host() else { return };
    let username = account(&host, PORT, "add-tested").username;
    let home = tempfile::tempdir().unwrap();
    let config = Paths::under(home.path()).config_file();
    let refused = account_add(home.path(), &host, &username, "wrong");
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(!refused.status.success(), "{stderr}");
    assert!(
        stderr.contains("the login failed, so nothing was saved"),
        "{stderr}"
    );
    assert!(!config.exists(), "nothing saved");
    let added = account_add(home.path(), &host, &username, PASSWORD);
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );
    let saved = Config::load(&config).unwrap();
    assert_eq!(saved.accounts[0].username, username);
}

/// A newsletter with a `cid:` logo and an attachment of about `mb` MB as sent.
fn large_newsletter(mb: usize) -> Vec<u8> {
    // Base64 in lines of 76, as mail clients send it.
    let pdf = format!("{}\r\n", "QUFB".repeat(19)).repeat(mb * 1024 * 1024 / 78);
    format!(
        "From: Sender <sender@example.com>\r\nTo: user@example.com\r\nSubject: scan\r\n\
         Message-ID: <scan@example.com>\r\nDate: Tue, 06 Oct 2026 10:00:00 +0000\r\nMIME-Version: 1.0\r\n\
         Content-Type: multipart/mixed; boundary=m\r\n\r\n--m\r\n\
         Content-Type: multipart/alternative; boundary=a\r\n\r\n--a\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\r\nthe two lines\r\n--a\r\n\
         Content-Type: multipart/related; boundary=r\r\n\r\n--r\r\n\
         Content-Type: text/html; charset=utf-8\r\n\r\n<p>the two lines</p><img src=\"cid:logo@example.com\">\r\n--r\r\n\
         Content-Type: image/png\r\nContent-ID: <logo@example.com>\r\nContent-Transfer-Encoding: base64\r\n\r\nUE5H\r\n\
         --r--\r\n--a--\r\n--m\r\n\
         Content-Type: application/pdf; name=\"scan.pdf\"\r\nContent-Disposition: attachment; filename=\"scan.pdf\"\r\n\
         Content-Transfer-Encoding: base64\r\n\r\n{pdf}--m--\r\n"
    )
    .into_bytes()
}

#[test]
fn a_large_message_opens_with_its_text_before_the_attachment_arrives() {
    let Some(host) = host() else { return };
    let mut ops = connect(&account(&host, PORT, "preview"));
    let raw = large_newsletter(2);
    ops.append("INBOX", &raw, &[]).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("mail.db")).unwrap();
    sync::sync_all(&mut ops, &store).unwrap();
    let uid = store.messages_in_folder("INBOX").unwrap()[0].uid;
    let mut previews = Vec::new();
    let mut show = |preview| {
        let msg = store.message("INBOX", uid).unwrap().unwrap();
        previews.push((preview, msg.body_text, store.has_raw("INBOX", uid).unwrap()));
    };
    postvak_core::actions::fetch_body(&mut ops, &store, "INBOX", uid, Some(&mut show)).unwrap();
    let [(preview, text, had_raw)] = previews.as_slice() else {
        panic!("{} previews", previews.len());
    };
    assert!(!had_raw, "the preview came before the whole message");
    assert_eq!(text.as_deref().map(str::trim), Some("the two lines"));
    let html = preview.html.as_ref().expect("its HTML");
    assert!(html.html.contains("cid:logo@example.com"), "{}", html.html);
    assert_eq!(html.inline.len(), 1);
    assert_eq!(html.inline[0].bytes, b"PNG");
    let stored = store
        .raw("INBOX", uid)
        .unwrap()
        .expect("then the whole message");
    assert_eq!(stored, raw);
    let whole = postvak_core::message::attachments(&stored);
    let listed: Vec<_> = preview
        .attachments
        .iter()
        .map(|a| (a.index, a.name.clone(), a.content_type.clone()))
        .collect();
    let expected: Vec<_> = whole
        .iter()
        .map(|a| (a.index, a.name.clone(), a.content_type.clone()))
        .collect();
    assert_eq!(listed, expected);
    let estimated = preview.attachments[1].size as f64;
    let actual = whole[1].size as f64;
    assert!(
        (estimated / actual - 1.0).abs() < 0.05,
        "{estimated} vs {actual}"
    );
}

#[test]
fn a_message_too_large_to_download_unasked_fetches_one_attachment_when_asked() {
    let Some(host) = host() else { return };
    let mut ops = connect(&account(&host, PORT, "on-demand"));
    let raw = large_newsletter(10);
    ops.append("INBOX", &raw, &[]).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("mail.db")).unwrap();
    sync::sync_all(&mut ops, &store).unwrap();
    let uid = store.messages_in_folder("INBOX").unwrap()[0].uid;
    let mut previews = Vec::new();
    let mut show = |preview| previews.push(preview);
    postvak_core::actions::fetch_body(&mut ops, &store, "INBOX", uid, Some(&mut show)).unwrap();
    let [preview] = previews.as_slice() else {
        panic!("{} previews", previews.len());
    };
    assert!(!preview.whole);
    assert!(!store.has_raw("INBOX", uid).unwrap(), "left on the server");
    let (name, contents) =
        postvak_core::actions::fetch_attachment(&mut ops, &store, "INBOX", uid, 2).unwrap();
    assert_eq!(name.as_deref(), Some("scan.pdf"));
    postvak_core::actions::fetch_body(&mut ops, &store, "INBOX", uid, None).unwrap();
    let stored = store.raw("INBOX", uid).unwrap().expect("the whole message");
    assert_eq!(stored, raw);
    assert_eq!(
        postvak_core::message::attachment(&stored, 2).unwrap(),
        ("scan.pdf".to_string(), contents)
    );
}
