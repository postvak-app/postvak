//! Mail homes for the GUI tests and the GPUI prototype: a config and SQLite stores under a directory the caller owns.
//! Built with `testing` as well as in tests, so the prototype draws the same mailbox as the egui snapshots.
use std::path::Path;

use anyhow::Result;

use crate::paths::Paths;
use crate::store::{Folder, Message, Store};

/// Every fixture time: 2026-09-21 13:46 UTC, more than a week before any run, so the list shows the date.
pub const AT: i64 = 1_790_000_000;

/// A Postvak home: `config.toml` with the accounts, and one store per account.
pub struct Home {
    pub accounts: Vec<String>,
    pub paths: Paths,
}

impl Home {
    /// A config entry and an empty INBOX per account, under `root`.
    pub fn new(root: &Path, accounts: &[&str]) -> Result<Home> {
        let paths = Paths::under(root);
        std::fs::create_dir_all(&paths.config_dir)?;
        let mut config = String::from("# test accounts\n");
        for name in accounts {
            config.push_str(&format!(
                "[[accounts]]\nname = \"{name}\"\nhost = \"imap.example.com\"\nusername = \"{name}@example.com\"\npassword = {{ keyring = true }}\n\n"
            ));
        }
        std::fs::write(paths.config_file(), config)?;
        let home = Home {
            accounts: accounts.iter().map(|name| name.to_string()).collect(),
            paths,
        };
        for name in accounts {
            home.folder(name, "INBOX", None)?;
        }
        Ok(home)
    }

    pub fn append_config(&self, text: &str) -> Result<()> {
        let mut config = std::fs::read_to_string(self.paths.config_file())?;
        config.push_str(text);
        std::fs::write(self.paths.config_file(), config)?;
        Ok(())
    }

    pub fn store(&self, account: &str) -> Result<Store> {
        Ok(Store::open(&self.paths.mail_db(account))?)
    }

    pub fn folder(&self, account: &str, name: &str, special_use: Option<&str>) -> Result<()> {
        let folder = Folder {
            name: name.into(),
            uidvalidity: 1,
            last_uid: 0,
            special_use: special_use.map(str::to_string),
            delimiter: None,
        };
        self.store(account)?.upsert_folder(&folder)?;
        Ok(())
    }

    pub fn add(&self, account: &str, message: Message) -> Result<()> {
        self.store(account)?.insert_message(&message)?;
        Ok(())
    }
}

/// A read message with a stored body, alone in its thread; a higher uid is newer.
pub fn message(folder: &str, uid: u32, subject: &str) -> Message {
    let at = AT + i64::from(uid) * 60;
    Message {
        folder: folder.into(),
        uid,
        message_id: Some(format!("<{uid}@example.com>")),
        from_addr: Some(format!("Sender {uid} <sender{uid}@example.com>")),
        to_addr: Some("me@example.com".into()),
        thread_id: format!("<{uid}@example.com>"),
        subject: Some(subject.into()),
        date: Some(at),
        internaldate: at,
        flags: "\\Seen".into(),
        size: Some(100),
        headers: format!("Subject: {subject}\r\n\r\n").into_bytes(),
        body_text: Some(format!("Body of {uid}")),
        ..Default::default()
    }
}

/// A message in the snapshot inbox, dated `100 - uid` hours before `AT`.
pub fn mail(uid: u32, from: &str, subject: &str, flags: &str, body: &str) -> Message {
    let mut m = message("INBOX", uid, subject);
    let at = AT - i64::from(100 - uid) * 3_600;
    m.from_addr = Some(from.into());
    m.to_addr = Some("Robin Example <robin@example.com>".into());
    m.flags = flags.into();
    m.body_text = Some(body.into());
    m.date = Some(at);
    m.internaldate = at;
    m
}

/// Two accounts with a few folders and a filled inbox, in a new home under `root`.
pub fn mailbox(root: &Path, theme: &str) -> Result<Home> {
    let fx = Home::new(root, &["home", "work"])?;
    fx.append_config(&format!("[ui]\ntheme = \"{theme}\"\n"))?;
    for (name, special) in [
        ("Archive", Some("Archive")),
        ("Sent", Some("Sent")),
        ("Trash", Some("Trash")),
        ("Receipts", None),
    ] {
        fx.folder("work", name, special)?;
    }
    fx.folder("home", "Archive", Some("Archive"))?;
    for name in ["Projects/Postvak", "Projects/Website"] {
        let nested = Folder {
            name: name.into(),
            uidvalidity: 1,
            last_uid: 0,
            special_use: None,
            delimiter: Some("/".into()),
        };
        fx.store("work")?.upsert_folder(&nested)?;
    }
    let lunch = "Hi Robin,\n\nShall we try the new place on the corner this Friday at noon?\nThe menu is at https://example.com/menu and they take bookings.\n\nCheers,\nLinus";
    let inbox = [
        (
            90,
            "Ada Lovelace <ada@example.com>",
            "Notes on the analytical engine",
            "\\Seen",
            "",
        ),
        (
            91,
            "Example Shop <shop@example.com>",
            "Your receipt for order 1042",
            "\\Seen",
            "",
        ),
        (
            92,
            "Weekly News <news@example.org>",
            "This week: ten things about tea",
            "",
            "1. Green tea\n2. Black tea\n3. Mint tea",
        ),
        (
            93,
            "Grace Hopper <grace@example.com>",
            "Planning the offsite",
            "\\Seen",
            "Shall we meet in Utrecht on the 14th?",
        ),
        (
            94,
            "Alan Turing <alan@example.com>",
            "Re: Planning the offsite",
            "\\Seen \\Flagged",
            "The 14th works for me.",
        ),
        (
            95,
            "Linus Example <linus@example.com>",
            "Lunch on Friday?",
            "",
            lunch,
        ),
    ];
    for (uid, from, subject, flags, body) in inbox {
        let mut m = mail(uid, from, subject, flags, body);
        if uid == 93 || uid == 94 {
            m.thread_id = "<offsite@example.com>".into();
        }
        fx.add("work", m)?;
    }
    let photos = mail(
        96,
        "Mum <mum@example.net>",
        "Photos from the weekend",
        "",
        "",
    );
    fx.add("home", photos)?;
    Ok(fx)
}

/// One account, `work`, whose INBOX holds `count` messages in threads of one to eight, a third of them unread and a
/// tenth flagged; the same `count` always gives the same mailbox. For timing the list on a large folder.
pub fn generated(root: &Path, count: u32) -> Result<Home> {
    let home = Home::new(root, &["work"])?;
    let store = home.store("work")?;
    store.transaction(|| -> Result<()> {
        let mut uid = 1;
        let mut thread = 0;
        while uid <= count {
            let members = thread % 8 + 1;
            for _ in 0..members.min(count - uid + 1) {
                let mut m = message("INBOX", uid, &format!("Thread {thread}: weekly update"));
                m.thread_id = format!("<thread-{thread}@example.com>");
                let sender = uid % 97;
                m.from_addr = Some(format!("Sender {sender} <sender{sender}@example.com>"));
                m.flags = match (uid % 3, uid % 10) {
                    (0, 0) => "\\Flagged",
                    (0, _) => "",
                    (_, 0) => "\\Seen \\Flagged",
                    _ => "\\Seen",
                }
                .into();
                store.insert_message(&m)?;
                uid += 1;
            }
            thread += 1;
        }
        Ok(())
    })?;
    Ok(home)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_generated_inbox_is_threaded_and_always_the_same() {
        let dir = tempfile::tempdir().unwrap();
        let home = generated(dir.path(), 100).unwrap();
        let threads = home
            .store("work")
            .unwrap()
            .thread_summaries("INBOX", 1000)
            .unwrap();
        assert_eq!(threads.iter().map(|t| t.count).sum::<u32>(), 100);
        assert!(threads.iter().any(|t| t.count == 8));
        let again = tempfile::tempdir().unwrap();
        let again = generated(again.path(), 100).unwrap();
        let again = again.store("work").unwrap().thread_summaries("INBOX", 1000);
        assert_eq!(again.unwrap(), threads);
    }
}
