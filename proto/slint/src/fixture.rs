//! The mail homes the prototype opens, the same as the GPUI prototype's: the snapshot mailbox with the newsletter, or
//! a generated INBOX; and the model over an in-memory daemon client.
use std::path::Path;
use std::sync::mpsc::Receiver;

use postvak::daemon::Client;
use postvak::gui::fixture::{self, Home};
use postvak::sync::Command;

use crate::model::Model;

/// `tests/fixtures/newsletter.html`, the HTML the egui snapshots render.
const NEWSLETTER: &str = include_str!("../../../tests/fixtures/newsletter.html");

/// A 1×1 PNG for the newsletter's `cid:` image.
const DOT: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFBQIAX8jx0gAAAABJRU5ErkJggg==";

/// The newsletter as a raw message: its HTML with a `cid:dot@example.com` image beside it.
fn newsletter_raw() -> String {
    let parts = [
        "From: news@example.org",
        "Subject: news",
        "MIME-Version: 1.0",
        "Content-Type: multipart/related; boundary=\"b\"",
        "",
        "--b",
        "Content-Type: text/html; charset=utf-8",
        "",
        NEWSLETTER,
        "--b",
        "Content-Type: image/png",
        "Content-ID: <dot@example.com>",
        "Content-Transfer-Encoding: base64",
        "",
        DOT,
        "--b--",
        "",
    ];
    parts.join("\r\n")
}

/// Adds the newsletter to work's INBOX as its newest message, unread.
fn add_newsletter(home: &Home) -> anyhow::Result<()> {
    let text = "This week in the reading room";
    let mut m = fixture::mail(97, "The Weekly Reader <news@example.org>", text, "", text);
    m.date = Some(fixture::AT);
    m.internaldate = fixture::AT;
    home.add("work", m)?;
    home.store("work")?
        .set_raw("INBOX", 97, newsletter_raw().as_bytes(), text)?;
    Ok(())
}

/// The snapshot mailbox's `work` INBOX with the newsletter, or with `count` a generated one, in a new home under
/// `dir`, or the home an earlier `--home` run left there; and the indices of the accounts the list shows.
pub fn open_home(dir: &Path, count: Option<u32>) -> anyhow::Result<(Home, Vec<usize>)> {
    let paths = postvak::paths::Paths::under(dir);
    if paths.config_file().exists() {
        let config = postvak::config::Config::load(&paths.config_file())?;
        let accounts: Vec<String> = config.accounts.into_iter().map(|a| a.name).collect();
        let shown = vec![accounts.iter().position(|a| a == "work").unwrap_or(0)];
        return Ok((Home { accounts, paths }, shown));
    }
    Ok(match count {
        Some(count) => (fixture::generated(dir, count)?, vec![0]),
        None => {
            let home = fixture::mailbox(dir, "light")?;
            add_newsletter(&home)?;
            (home, vec![1])
        }
    })
}

/// A model over a client with no daemon behind it, and the receiver of what it sends; dropping the receiver makes
/// every send fail.
pub fn open_model(
    home: &Home,
    shown: Vec<usize>,
) -> anyhow::Result<(Model, Receiver<(String, Command)>)> {
    let names: Vec<&str> = home.accounts.iter().map(String::as_str).collect();
    let (client, commands, _) = Client::in_memory(&names);
    Ok((Model::open(home, shown, client)?, commands))
}

/// The snapshot inbox, with or without the newsletter, for tests.
#[cfg(test)]
#[allow(clippy::unwrap_used)]
pub fn test_model(newsletter: bool) -> (tempfile::TempDir, Model, Receiver<(String, Command)>) {
    let dir = tempfile::tempdir().unwrap();
    let home = fixture::mailbox(dir.path(), "light").unwrap();
    if newsletter {
        add_newsletter(&home).unwrap();
    }
    let (model, commands) = open_model(&home, vec![1]).unwrap();
    (dir, model, commands)
}
