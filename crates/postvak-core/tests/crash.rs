// Test code: unwrap, expect and panic are how a test fails.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Crash reports. Its own test binary, since the panic hook is process-wide.

use postvak_core::crash;
use postvak_core::sync::Command;

const ADDRESS: &str = "jane.doe@example.org";
const BODY: &str = "the secret quarterly figures";

#[test]
fn a_report_names_the_command_but_holds_no_mail_data() {
    let dir = tempfile::tempdir().unwrap();
    crash::install(dir.path().to_path_buf(), "daemon");
    crash::note(&Command::SearchServer {
        folder: Some(format!("Clients/{ADDRESS}")),
        query: BODY.into(),
    });
    let crashed = std::thread::Builder::new()
        .name(format!("sync-{ADDRESS}"))
        .spawn(|| panic!("could not parse mail from {ADDRESS}: {BODY}"))
        .unwrap()
        .join();
    assert!(crashed.is_err());

    let ids = crash::list(dir.path()).unwrap();
    assert_eq!(ids.len(), 1);
    assert!(ids[0].ends_with("-daemon"), "{}", ids[0]);
    let report = crash::show(dir.path(), &ids[0]).unwrap();
    for needle in [ADDRESS, "jane", "example.org", BODY, "quarterly", "Clients"] {
        assert!(!report.contains(needle), "{needle} leaked into:\n{report}");
    }
    for expected in [
        "process: daemon",
        "thread: sync",
        "last action: SearchServer",
        "tests/crash.rs",
        env!("CARGO_PKG_VERSION"),
    ] {
        assert!(report.contains(expected), "no {expected} in:\n{report}");
    }

    // A literal message is compiled into the binary, so it can hold no mail data and is kept.
    let _ = std::thread::spawn(|| panic!("a literal message")).join();
    let ids = crash::list(dir.path()).unwrap();
    assert!(
        crash::show(dir.path(), &ids[0])
            .unwrap()
            .contains("a literal message")
    );

    assert!(crash::show(dir.path(), "../escape").is_err());
}
