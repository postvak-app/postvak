//! Mail homes for the GUI tests: a config and SQLite stores under a directory the caller owns.
use std::path::Path;

use postvak_core::paths::Paths;
use postvak_core::store::{Folder, Message, Store};

/// Every fixture time: 2026-09-21 13:46 UTC, more than a week before any run, so the list shows the date.
pub const AT: i64 = 1_790_000_000;

/// A Postvak home: `config.toml` with the accounts, and one store per account.
pub struct Home {
    pub accounts: Vec<String>,
    pub paths: Paths,
}

impl Home {
    /// A config entry and an empty INBOX per account, under `root`.
    pub fn new(root: &Path, accounts: &[&str]) -> Home {
        let paths = Paths::under(root);
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        let mut config = String::from("# test accounts\n");
        for name in accounts {
            config.push_str(&format!(
                "[[accounts]]\nname = \"{name}\"\nhost = \"imap.example.com\"\nusername = \"{name}@example.com\"\npassword = {{ keyring = true }}\n\n"
            ));
        }
        std::fs::write(paths.config_file(), config).unwrap();
        let home = Home {
            accounts: accounts.iter().map(|name| name.to_string()).collect(),
            paths,
        };
        for name in accounts {
            home.folder(name, "INBOX", None);
        }
        home
    }

    pub fn append_config(&self, text: &str) {
        let mut config = std::fs::read_to_string(self.paths.config_file()).unwrap();
        config.push_str(text);
        std::fs::write(self.paths.config_file(), config).unwrap();
    }

    pub fn store(&self, account: &str) -> Store {
        Store::open(&self.paths.mail_db(account)).unwrap()
    }

    pub fn folder(&self, account: &str, name: &str, special_use: Option<&str>) {
        let folder = Folder {
            name: name.into(),
            uidvalidity: 1,
            last_uid: 0,
            special_use: special_use.map(str::to_string),
            delimiter: None,
        };
        self.store(account).upsert_folder(&folder).unwrap();
    }

    /// Stores the message; one with body text also gets raw bytes, as a message the reader fetched has both.
    pub fn add(&self, account: &str, message: Message) {
        let store = self.store(account);
        store.insert_message(&message).unwrap();
        if let Some(text) = &message.body_text {
            let mut raw = message.headers.clone();
            raw.extend_from_slice(text.as_bytes());
            store
                .set_raw(&message.folder, message.uid, &raw, text)
                .unwrap();
        }
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
pub fn mailbox(root: &Path, theme: &str) -> Home {
    let fx = Home::new(root, &["home", "work"]);
    fx.append_config(&format!("[ui]\ntheme = \"{theme}\"\n"));
    for (name, special) in [
        ("Archive", Some("Archive")),
        ("Sent", Some("Sent")),
        ("Junk", Some("Junk")),
        ("Trash", Some("Trash")),
        ("Receipts", None),
    ] {
        fx.folder("work", name, special);
    }
    fx.folder("home", "Archive", Some("Archive"));
    for name in [
        "Projects/Postvak/Design/Mockups/Old explorations 2024",
        "Projects/Website",
    ] {
        let nested = Folder {
            name: name.into(),
            uidvalidity: 1,
            last_uid: 0,
            special_use: None,
            delimiter: Some("/".into()),
        };
        fx.store("work").upsert_folder(&nested).unwrap();
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
        fx.add("work", m);
    }
    let photos = mail(
        96,
        "Mum <mum@example.net>",
        "Photos from the weekend",
        "",
        "",
    );
    fx.add("home", photos);
    fx
}
