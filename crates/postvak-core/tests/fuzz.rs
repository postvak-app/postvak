// Test code: unwrap, expect and panic are how a test fails.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Property tests: hostile input must never panic the parsers. Every property only asserts "no panic"; Ok and Err
//! are both fine. Inputs are arbitrary bytes and mutations of made-up messages, the rules.toml examples from the docs
//! and valid wire lines.
//!
//! The default case count keeps `cargo test` fast. A deeper search:
//!
//! ```sh
//! PROPTEST_CASES=20000 cargo test --release --test fuzz
//! ```
//!
//! Failures are not persisted; turn a finding into a named `#[test]` here (or in `tests/known_bugs.rs` while it is
//! unfixed) instead. The message properties tolerate only the upstream panic sites listed in `KNOWN_UPSTREAM_PANICS`,
//! each reproduced in `tests/known_bugs.rs`.

use std::cell::RefCell;
use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::Once;

use proptest::prelude::*;
use proptest::test_runner::Config;

use postvak_core::daemon::wire::{
    self, AccountStatus, ClientMessage, DaemonMessage, Outcome, Payload, Status,
};
use postvak_core::mail_ops::utf7;
use postvak_core::message::{
    attachments, attachments_and_html, auth, bare_addresses, body_text, clean, clean_for_agent,
    header_value, parse_headers, save_attachment, thread_id,
};
use postvak_core::rules::{self, Action, Match, OneOrMany, Rule, RuleFile, TextMatch};
use postvak_core::search::{self, Scope};
use postvak_core::store::{Folder, Message, Store};
use postvak_core::sync::{Activity, Command, Event};

/// Cases per property unless `PROPTEST_CASES` is set.
const DEFAULT_CASES: u32 = 64;

fn config() -> Config {
    let cases = std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(DEFAULT_CASES);
    Config {
        cases,
        failure_persistence: None,
        ..Config::default()
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Mutation

/// Fragments that tend to sit on parser edges: separators, encoded-word and MIME syntax, bad charsets, odd bytes.
const TOKENS: &[&[u8]] = &[
    b"\r\n",
    b"\n",
    b"\r\n\r\n",
    b"\r\n ",
    b"\r\n\t",
    b"\0",
    b"\xff\xfe",
    b"\xc3",
    b"\xe2\x82",
    b"\xf0\x9f\x98",
    b":",
    b";",
    b"\"",
    b"\\",
    b"<",
    b">",
    b"<>",
    b"(",
    b")",
    b",",
    b"@",
    b"=",
    b"=?",
    b"?=",
    b"=?utf-8?B?",
    b"=?utf-8?Q?",
    b"=?utf-8?B?////?=",
    b"=?utf-8?Q?=FF=E2=82?=",
    b"=?x-bogus-8?Q?caf=E9?=",
    b"=?iso-2022-jp?B?GyRCJDMbKEI=?=",
    b"=?utf-16?B?2D3e?=",
    b"=??B??=",
    b"=?utf-8*en?q?hi?=",
    b"charset=\"bogus-8\"",
    b"charset=utf-7",
    b"charset=\"utf-16\"",
    b"Content-Type: multipart/mixed; boundary=\"b\"\r\n",
    b"Content-Type: multipart/alternative; boundary=\r\n",
    b"Content-Type: message/rfc822\r\n",
    b"Content-Type: text/html; charset=\"x-unknown\"\r\n",
    b"Content-Transfer-Encoding: base64\r\n",
    b"Content-Transfer-Encoding: quoted-printable\r\n",
    b"Content-Disposition: attachment; filename*=utf-8''..%2F..%2F.bashrc\r\n",
    b"Content-Disposition: attachment; filename=\"../../\0x\"\r\n",
    b"--b\r\n",
    b"--b--\r\n",
    b"--\r\n",
    b"=\r\n",
    b"=F",
    b"=XY",
    b"====",
    b"References: <a@example.com> <\r\n",
    b"In-Reply-To: \r\n",
    b"Message-ID: <>\r\n",
    b"Date: Mon, 32 Foo 99999 25:61:61 +9999\r\n",
    b"Date: \r\n",
    b"From: \"\" <@>, ,,<\r\n",
    b"To: undisclosed-recipients:;\r\n",
    b"<html><body><p>&#x110000;&amp;&lt;<",
    b"<!--",
    b"<![CDATA[",
];

#[derive(Debug, Clone)]
enum Mutation {
    Flip { at: usize, byte: u8 },
    Truncate { at: usize },
    Remove { at: usize, len: usize },
    Insert { at: usize, token: usize },
    InsertBytes { at: usize, bytes: Vec<u8> },
    Splice { at: usize, from: usize, len: usize },
    DuplicateLine { at: usize, times: usize },
}

fn mutation() -> impl Strategy<Value = Mutation> {
    let at = any::<usize>();
    prop_oneof![
        (at, any::<u8>()).prop_map(|(at, byte)| Mutation::Flip { at, byte }),
        at.prop_map(|at| Mutation::Truncate { at }),
        (at, 1..64usize).prop_map(|(at, len)| Mutation::Remove { at, len }),
        (at, 0..TOKENS.len()).prop_map(|(at, token)| Mutation::Insert { at, token }),
        (at, proptest::collection::vec(any::<u8>(), 1..16))
            .prop_map(|(at, bytes)| Mutation::InsertBytes { at, bytes }),
        (at, any::<usize>(), 1..256usize).prop_map(|(at, from, len)| Mutation::Splice {
            at,
            from,
            len
        }),
        (at, 1..64usize).prop_map(|(at, times)| Mutation::DuplicateLine { at, times }),
    ]
}

/// Mutated input never grows past this many bytes.
const MAX_MUTATED_LEN: usize = 256 * 1024;

/// Applies `mutations` in order; positions wrap around the current length, `donor` feeds splices.
fn mutate(mut data: Vec<u8>, donor: &[u8], mutations: &[Mutation]) -> Vec<u8> {
    for m in mutations {
        // Duplicating a whole one-line input compounds; stop growing well before that costs real memory.
        if data.len() > MAX_MUTATED_LEN {
            break;
        }
        let pos = |at: usize, len: usize| if len == 0 { 0 } else { at % (len + 1) };
        match m {
            Mutation::Flip { at, byte } => {
                if !data.is_empty() {
                    let i = at % data.len();
                    data[i] = *byte;
                }
            }
            Mutation::Truncate { at } => data.truncate(pos(*at, data.len())),
            Mutation::Remove { at, len } => {
                let start = pos(*at, data.len());
                let end = (start + len).min(data.len());
                data.drain(start..end);
            }
            Mutation::Insert { at, token } => {
                let i = pos(*at, data.len());
                data.splice(i..i, TOKENS[*token].iter().copied());
            }
            Mutation::InsertBytes { at, bytes } => {
                let i = pos(*at, data.len());
                data.splice(i..i, bytes.iter().copied());
            }
            Mutation::Splice { at, from, len } => {
                if !donor.is_empty() {
                    let start = from % donor.len();
                    let end = (start + len).min(donor.len());
                    let i = pos(*at, data.len());
                    data.splice(i..i, donor[start..end].iter().copied());
                }
            }
            Mutation::DuplicateLine { at, times } => {
                let i = pos(*at, data.len());
                let start = data[..i]
                    .iter()
                    .rposition(|&b| b == b'\n')
                    .map_or(0, |p| p + 1);
                let end = data[i..]
                    .iter()
                    .position(|&b| b == b'\n')
                    .map_or(data.len(), |p| i + p + 1);
                let line = data[start..end].to_vec();
                for _ in 0..*times {
                    data.splice(end..end, line.iter().copied());
                }
            }
        }
    }
    data.truncate(MAX_MUTATED_LEN);
    data
}

fn mutations() -> impl Strategy<Value = Vec<Mutation>> {
    proptest::collection::vec(mutation(), 1..12)
}

// ---------------------------------------------------------------------------------------------------------------
// Messages

/// Made-up mail covering plain, encoded words, multipart, quoted-printable, base64, attachments and HTML.
const MESSAGES: &[&[u8]] = &[
    b"From: Alice <alice@example.com>\r\n\
To: Bob <bob@example.com>, carol@example.com\r\n\
Cc: dave@example.com\r\n\
Delivered-To: bob@example.com\r\n\
Subject: Re: lunch\r\n\
Date: Mon, 6 Oct 2026 12:00:00 +0200\r\n\
Message-ID: <m3@example.com>\r\n\
In-Reply-To: <m2@example.com>\r\n\
References: <m1@example.com> <m2@example.com>\r\n\
List-Id: Dev <dev.lists.example.com>\r\n\
\r\n\
Sounds good, see you at noon.\r\n",
    b"From: =?utf-8?B?w4lsaXNl?= <elise@example.org>\r\n\
To: =?iso-8859-1?Q?J=F6rg?= <jorg@example.net>\r\n\
Subject: =?utf-8?Q?Caf=C3=A9_menu?= =?utf-8?B?IOKCrA==?=\r\n\
Date: Tue, 7 Oct 2026 08:30:00 -0700\r\n\
Message-ID: <enc1@example.org>\r\n\
MIME-Version: 1.0\r\n\
Content-Type: text/plain; charset=\"iso-8859-1\"\r\n\
Content-Transfer-Encoding: quoted-printable\r\n\
\r\n\
Caf=E9 au lait =\r\n\
for everyone.\r\n",
    b"From: Shop <noreply@shop.example.com>\r\n\
To: me+shop@example.com\r\n\
Subject: Your order\r\n\
Date: Wed, 8 Oct 2026 10:00:00 +0000\r\n\
Message-ID: <order-77@shop.example.com>\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"outer\"\r\n\
\r\n\
preamble\r\n\
--outer\r\n\
Content-Type: multipart/alternative; boundary=\"inner\"\r\n\
\r\n\
--inner\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
Order 77 is on its way.\r\n\
--inner\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n\
<html><body><p>Order <b>77</b> is on its way.</p></body></html>\r\n\
--inner--\r\n\
--outer\r\n\
Content-Type: application/pdf; name=\"invoice.pdf\"\r\n\
Content-Disposition: attachment; filename=\"invoice.pdf\"\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
JVBERi0xLjQKJcOkw7zDtsOfCjIgMCBvYmoKPDwvTGVuZ3RoIDMgMCBSPj4Kc3RyZWFtCg==\r\n\
--outer\r\n\
Content-Type: message/rfc822\r\n\
\r\n\
From: inner@example.com\r\n\
Subject: forwarded\r\n\
\r\n\
inner body\r\n\
--outer--\r\n\
epilogue\r\n",
    b"From: list@lists.example.com\r\n\
To: dev@lists.example.com\r\n\
Subject: [dev] weekly digest\r\n\
List-Unsubscribe: <mailto:leave@lists.example.com>\r\n\
Content-Type: text/html; charset=\"windows-1252\"\r\n\
\r\n\
<html><head><style>p{}</style></head><body><p>Hello &amp; welcome &#8364;</p><br><div>bye</div></body></html>\r\n",
];

/// A bounded random MIME tree, rendered to bytes. Boundaries may collide with a parent's on purpose.
fn mime_part(depth: u32) -> BoxedStrategy<Vec<u8>> {
    let leaf = (
        prop::sample::select(vec![
            "text/plain",
            "text/html",
            "application/octet-stream",
            "image/png",
            "text/x-weird",
        ]),
        prop::sample::select(vec![
            "utf-8",
            "iso-8859-1",
            "windows-1252",
            "bogus-8",
            "utf-16",
            "",
            "\"\"",
        ]),
        prop::sample::select(vec![
            "7bit",
            "base64",
            "quoted-printable",
            "8bit",
            "x-uuencode",
            "nonsense",
        ]),
        proptest::collection::vec(any::<u8>(), 0..64),
        proptest::option::of("[ -~]{0,20}"),
    )
        .prop_map(|(ctype, charset, cte, body, name)| {
            let mut out = format!(
                "Content-Type: {ctype}; charset={charset}\r\nContent-Transfer-Encoding: {cte}\r\n"
            )
            .into_bytes();
            if let Some(name) = name {
                out.extend_from_slice(
                    format!("Content-Disposition: attachment; filename=\"{name}\"\r\n").as_bytes(),
                );
            }
            out.extend_from_slice(b"\r\n");
            out.extend_from_slice(&body);
            out
        })
        .boxed();
    leaf.prop_recursive(depth, 64, 4, |inner| {
        (
            prop::sample::select(vec!["mixed", "alternative", "related", "digest"]),
            prop::sample::select(vec!["b", "b1", "=_x", "", "outer"]),
            proptest::collection::vec(inner, 0..4),
            any::<bool>(),
        )
            .prop_map(|(sub, boundary, parts, close)| {
                let mut out =
                    format!("Content-Type: multipart/{sub}; boundary=\"{boundary}\"\r\n\r\n")
                        .into_bytes();
                for part in parts {
                    out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
                    out.extend_from_slice(&part);
                    out.extend_from_slice(b"\r\n");
                }
                if close {
                    out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
                }
                out
            })
    })
    .boxed()
}

/// Panics in mail-parser 0.11.9 that only fire with debug assertions; see `tests/known_bugs.rs`. Matched by file and
/// message, so a mail-parser upgrade that fixes or moves them makes them fail again here.
const KNOWN_UPSTREAM_PANICS: &[(&str, &str)] = &[
    (
        "src/decoders/quoted_printable.rs",
        "attempt to subtract with overflow",
    ),
    (
        "src/parsers/message.rs",
        "Invalid part ID, could not find multipart.",
    ),
];

thread_local! {
    static KNOWN_PANIC: RefCell<bool> = const { RefCell::new(false) };
}

/// Notes on the panicking thread whether a panic is a known upstream one, and keeps those out of the test output.
fn install_panic_hook() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let default = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            let file = info.location().map(|l| l.file()).unwrap_or("");
            let message = info
                .payload()
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| info.payload().downcast_ref::<String>().cloned())
                .unwrap_or_default();
            let known = file.contains("/mail-parser-")
                && KNOWN_UPSTREAM_PANICS
                    .iter()
                    .any(|(f, m)| file.ends_with(f) && message.contains(m));
            KNOWN_PANIC.with(|k| *k.borrow_mut() = known);
            if !known {
                default(info);
            }
        }));
    });
}

/// Runs `f`, swallowing only the known upstream panics.
fn tolerating_known_upstream_panics(f: impl FnOnce()) {
    install_panic_hook();
    KNOWN_PANIC.with(|k| *k.borrow_mut() = false);
    if let Err(payload) = panic::catch_unwind(AssertUnwindSafe(f))
        && !KNOWN_PANIC.with(|k| *k.borrow())
    {
        panic::resume_unwind(payload);
    }
}

/// Every public entry point that takes server-supplied message bytes.
fn exercise_message(raw: &[u8], folder: &str, uid: u32) {
    tolerating_known_upstream_panics(|| exercise_message_inner(raw, folder, uid));
}

fn exercise_message_inner(raw: &[u8], folder: &str, uid: u32) {
    let parsed = parse_headers(raw);
    let _ = thread_id(&parsed, folder, uid);
    let fields = [
        &parsed.from,
        &parsed.to,
        &parsed.cc,
        &parsed.delivered_to,
        &parsed.subject,
    ];
    for value in fields.into_iter().flatten() {
        let _ = bare_addresses(value);
        let _ = clean(value, false);
    }
    for name in [
        "List-Id",
        "list-unsubscribe",
        "Received",
        "",
        ":",
        "Subject\0",
    ] {
        let _ = header_value(raw, name);
    }
    let body = body_text(raw);
    let _ = clean(&body, true);
    let _ = clean_for_agent(&body, true);
    let _ = bare_addresses(&String::from_utf8_lossy(raw));
    let _ = attachments(raw);
    let _ = attachments_and_html(raw);
    let _ = auth::all(raw);
    let from = parsed.from.as_deref().unwrap_or_default();
    let _ = auth::authenticated(raw, from, |_| true);
}

fn exercise_save(raw: &[u8]) {
    let dir = tempfile::tempdir().unwrap();
    tolerating_known_upstream_panics(|| {
        for index in 0..=attachments(raw).len() + 1 {
            let _ = save_attachment(raw, index, dir.path());
        }
    });
}

proptest! {
    #![proptest_config(config())]

    #[test]
    fn message_arbitrary_bytes(raw in proptest::collection::vec(any::<u8>(), 0..2048), folder in ".{0,16}", uid in any::<u32>()) {
        exercise_message(&raw, &folder, uid);
    }

    #[test]
    fn message_mutated(seed in 0..MESSAGES.len(), donor in 0..MESSAGES.len(), ms in mutations()) {
        let raw = mutate(MESSAGES[seed].to_vec(), MESSAGES[donor], &ms);
        exercise_message(&raw, "INBOX", 1);
        exercise_save(&raw);
    }

    #[test]
    fn message_random_mime_tree(head in 0..MESSAGES.len(), tree in mime_part(6), ms in proptest::collection::vec(mutation(), 0..4)) {
        let header_end = MESSAGES[head].windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let mut raw: Vec<u8> = MESSAGES[head][..header_end + 2]
            .split_inclusive(|&b| b == b'\n')
            .filter(|line| !line.to_ascii_lowercase().starts_with(b"content-"))
            .flatten()
            .copied()
            .collect();
        raw.extend_from_slice(b"MIME-Version: 1.0\r\n");
        raw.extend_from_slice(&tree);
        let raw = mutate(raw, &tree, &ms);
        exercise_message(&raw, "INBOX", 1);
        exercise_save(&raw);
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Rules

/// The rules.toml examples from the docs, one string per ```toml block.
fn rule_examples() -> Vec<String> {
    let doc = include_str!("../../../docs/src/rules.md");
    doc.split("```toml\n")
        .skip(1)
        .filter_map(|block| block.split("```").next())
        .map(str::to_string)
        .collect()
}

/// Parses and, when that works, validates; neither may panic.
fn exercise_rules(text: &str) {
    if let Ok(file) = rules::parse(text) {
        let _ = rules::compile(&file);
    }
}

/// Text mixing plain letters with tag, bidi, zero-width and other invisible characters, and emoji joined by U+200D.
fn invisible_text() -> BoxedStrategy<String> {
    let piece = prop_oneof![
        "[a-z <>/_]{0,4}",
        (0xE0000u32..=0xE007F)
            .prop_map(|c| char::from_u32(c).map(String::from).unwrap_or_default()),
        prop::sample::select(vec![
            "\u{202A}",
            "\u{202E}",
            "\u{2066}",
            "\u{2069}",
            "\u{200B}",
            "\u{200C}",
            "\u{200D}",
            "\u{200E}",
            "\u{2060}",
            "\u{2064}",
            "\u{FEFF}",
            "\u{AD}",
            "\u{61C}",
            "\u{1D173}",
            "\u{1F468}",
            "\u{FE0F}",
        ])
        .prop_map(str::to_string),
    ];
    proptest::collection::vec(piece, 0..32)
        .prop_map(|pieces| pieces.concat())
        .boxed()
}

proptest! {
    #![proptest_config(config())]

    /// No hidden character survives, the visible ones and U+200D all do, and a second pass changes nothing.
    #[test]
    fn clean_for_agent_leaves_nothing_hidden(text in invisible_text(), layout in any::<bool>()) {
        let cleaned = clean_for_agent(&text, layout);
        let hidden = |c: char| {
            ('\u{E0000}'..='\u{E007F}').contains(&c)
                || ('\u{202A}'..='\u{202E}').contains(&c)
                || ('\u{2066}'..='\u{2069}').contains(&c)
                || matches!(c, '\u{200B}' | '\u{2060}' | '\u{FEFF}')
        };
        prop_assert!(!cleaned.chars().any(hidden), "{:?}", cleaned);
        let visible: String = text.chars().filter(|&c| c.is_ascii() || matches!(c, '\u{200D}' | '\u{1F468}' | '\u{FE0F}')).collect();
        prop_assert_eq!(&cleaned, &visible);
        prop_assert_eq!(clean_for_agent(&cleaned, layout), cleaned);
    }
}

/// A store with three messages, so a search runs against real rows.
fn search_store() -> Store {
    let store = Store::open_in_memory().unwrap();
    store
        .upsert_folder(&Folder {
            name: "INBOX".into(),
            uidvalidity: 1,
            last_uid: 3,
            special_use: None,
            delimiter: None,
        })
        .unwrap();
    for (uid, subject, body) in [
        (1, "Café invoice", Some("pineapple pie")),
        (2, "Re: lunch \"quoted\"", None),
        (3, "NEAR(a b) OR * -x", Some("")),
    ] {
        store
            .insert_message(&Message {
                folder: "INBOX".into(),
                uid,
                thread_id: format!("t{uid}"),
                subject: Some(subject.into()),
                from_addr: Some("Alice <alice@example.org>".into()),
                internaldate: i64::from(uid),
                headers: b"From: alice@example.org\r\n\r\n".to_vec(),
                body_text: body.map(str::to_string),
                ..Default::default()
            })
            .unwrap();
    }
    store
}

proptest! {
    #![proptest_config(config())]

    /// Whatever is typed, the planned expression is one FTS5 accepts: an `Err` here is a syntax error.
    #[test]
    fn any_text_parses_and_plans_into_an_expression_fts5_accepts(text in prop_oneof![".{0,128}", hostile_text()]) {
        let store = search_store();
        let query = search::parse(&text, 1_792_843_200);
        let identity = identity();
        for scope in [Scope::All, Scope::Headers] {
            for identity in [None, Some(&identity)] {
                let result = store.search(&query, scope, None, 10, true, identity);
                prop_assert!(result.is_ok(), "{text:?}: {:?}", result.err());
            }
        }
    }
}

/// Folder names a server could list: modified UTF-7 sections, sloppy ones, raw UTF-8 and `&`/`-` in odd places.
fn wire_folder_name() -> BoxedStrategy<String> {
    prop_oneof![
        ".{0,64}",
        "[A-Za-z0-9+,&/. -]{0,64}",
        proptest::collection::vec(
            prop_oneof![
                Just("&".to_string()),
                Just("-".to_string()),
                Just("&-".to_string()),
                "&[A-Za-z0-9+,]{0,12}-",
                "[A-Za-z ./]{0,4}",
                ".{0,2}",
            ],
            0..12,
        )
        .prop_map(|parts| parts.concat()),
    ]
    .boxed()
}

proptest! {
    #![proptest_config(config())]

    /// Whatever name a server lists, decoding it does not panic, and a name that decodes encodes back to what the
    /// server sent, so the server gets back the name it listed.
    #[test]
    fn modified_utf7_decodes_any_listed_name_or_refuses_it(wire in wire_folder_name()) {
        if let Some(name) = utf7::decode(&wire) {
            prop_assert_eq!(utf7::encode(&name), wire);
        }
    }

    /// Every UTF-8 name survives the trip to the wire and back.
    #[test]
    fn modified_utf7_round_trips_any_name(name in ".{0,64}") {
        prop_assert_eq!(utf7::decode(&utf7::encode(&name)), Some(name));
    }
}

/// An identity with a wildcard alias, so `from:me` plans its LIKE predicates.
fn identity() -> postvak_core::config::Identity {
    let mut account = postvak_core::config::AccountConfig::new(
        "work",
        "imap.example.com",
        "me@example.com",
        postvak_core::config::PasswordSource::Command {
            command: "x".into(),
        },
    );
    account.aliases = vec!["*@shop.example.com".into(), "a_b%c".into()];
    account.identity().unwrap()
}

/// Strings aimed at regex, duration and glob syntax as well as plain junk.
fn hostile_text() -> BoxedStrategy<String> {
    prop_oneof![
        ".{0,24}",
        "[\\[\\](){}*?+|^$\\\\.0-9a-z,!-]{0,24}",
        "[0-9]{0,30}[a-z ]{0,10}",
        prop::sample::select(vec![
            "",
            " ",
            "\0",
            "a{99999}",
            "(?i)",
            "(",
            "[",
            "\\",
            "\\p{Bogus}",
            "(?P<n>",
            "x{2,1}",
            "**",
            "[!]",
            "{a,{b,c}",
            "[a-]",
            "\\u{110000}",
            "18446744073709551616s",
            "99999999999999999999years",
            "1h 2",
            "-1h",
            "1e9d",
            "0",
            "1.5h",
            "\u{202e}",
            "{",
            "}",
            "a/**/",
            "[z-a]",
        ])
        .prop_map(str::to_string),
    ]
    .boxed()
}

fn values() -> BoxedStrategy<OneOrMany<String>> {
    prop_oneof![
        hostile_text().prop_map(OneOrMany::One),
        proptest::collection::vec(hostile_text(), 0..3).prop_map(OneOrMany::Many),
    ]
    .boxed()
}

fn text_match() -> BoxedStrategy<TextMatch> {
    (
        proptest::option::of(values()),
        proptest::option::of(values()),
        proptest::option::of(hostile_text()),
    )
        .prop_map(|(contains, equals, regex)| TextMatch {
            contains,
            equals,
            regex,
        })
        .boxed()
}

fn action() -> BoxedStrategy<Action> {
    prop_oneof![
        Just(Action::Delete),
        Just(Action::MarkRead),
        Just(Action::MarkUnread),
        Just(Action::Unflag),
        Just(Action::Flag),
        Just(Action::Archive),
        Just(Action::Notify),
        Just(Action::Silent),
        Just(Action::Stop),
        hostile_text().prop_map(Action::Move),
        hostile_text().prop_map(Action::Tag),
    ]
    .boxed()
}

fn match_leaf() -> BoxedStrategy<Match> {
    let texts = (
        proptest::option::of(text_match()),
        proptest::option::of(text_match()),
        proptest::option::of(text_match()),
        proptest::option::of(text_match()),
        proptest::option::of(text_match()),
    )
        .boxed();
    let domains = (
        proptest::option::of(values()),
        proptest::option::of(values()),
        proptest::option::of(any::<bool>()),
    )
        .boxed();
    let others = (
        proptest::option::of(proptest::collection::btree_map(
            hostile_text(),
            text_match(),
            0..3,
        )),
        proptest::option::of(hostile_text()),
        proptest::option::of(any::<bool>()),
        proptest::option::of(any::<bool>()),
        proptest::option::of(hostile_text()),
        proptest::option::of(hostile_text()),
        proptest::option::of(hostile_text()),
        proptest::option::of(any::<bool>()),
    )
        .boxed();
    (texts, others, domains)
        .prop_map(
            |(
                (from, to, cc, subject, body),
                (header, older_than, read, to_me, alias, tag, larger_than, flagged),
                (from_domain, to_domain, authenticated),
            )| {
                Match {
                    from,
                    to,
                    cc,
                    subject,
                    body,
                    header,
                    from_domain,
                    to_domain,
                    authenticated,
                    older_than,
                    larger_than,
                    read,
                    flagged,
                    to_me,
                    alias,
                    tag,
                    none: None,
                    any: None,
                }
            },
        )
        .boxed()
}

/// Leaves wrapped in `none` and `any`, a few levels deep.
fn match_tree() -> BoxedStrategy<Match> {
    match_leaf()
        .prop_recursive(3, 16, 3, |inner| {
            (
                match_leaf(),
                proptest::option::of(proptest::collection::vec(inner.clone(), 0..3)),
                proptest::option::of(proptest::collection::vec(inner, 0..3)),
            )
                .prop_map(|(mut leaf, none, any)| {
                    leaf.none = none;
                    leaf.any = any;
                    leaf
                })
        })
        .boxed()
}

fn rule() -> BoxedStrategy<Rule> {
    let matches = match_tree();
    (
        hostile_text(),
        proptest::option::of(hostile_text()),
        proptest::option::of(hostile_text()),
        any::<bool>(),
        proptest::option::of(hostile_text()),
        matches,
        proptest::collection::vec(action(), 0..4),
    )
        .prop_map(
            |(name, account, folder, enabled, proposed_by, matches, actions)| Rule {
                name,
                account,
                folder,
                enabled,
                proposed_by,
                matches,
                actions,
            },
        )
        .boxed()
}

proptest! {
    #![proptest_config(config())]

    #[test]
    fn rules_arbitrary_text(text in ".{0,512}") {
        exercise_rules(&text);
    }

    #[test]
    fn rules_mutated_examples(seed in any::<prop::sample::Index>(), donor in any::<prop::sample::Index>(), ms in mutations()) {
        let examples = rule_examples();
        let seed = seed.get(&examples);
        let donor = donor.get(&examples);
        let bytes = mutate(seed.as_bytes().to_vec(), donor.as_bytes(), &ms);
        exercise_rules(&String::from_utf8_lossy(&bytes));
    }

    #[test]
    fn rules_concatenated_examples(picks in proptest::collection::vec(any::<prop::sample::Index>(), 1..6)) {
        let examples = rule_examples();
        let text: String = picks.iter().map(|i| i.get(&examples).as_str()).collect();
        exercise_rules(&text);
    }

    #[test]
    fn rules_structured(rules in proptest::collection::vec(rule(), 0..4)) {
        let file = RuleFile { rules };
        let _ = rules::compile(&file);
        if let Ok(text) = toml::to_string(&file) {
            exercise_rules(&text);
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Authentication-Results

/// Header values as servers write them, comments, quotes and versions included.
const AUTH_RESULTS: &[&str] = &[
    "mx.soverin.net; dkim=pass (2048-bit key; unprotected) header.d=crates.io header.i=@crates.io header.b=\"AbC=\"; spf=pass smtp.mailfrom=bounces.crates.io; dmarc=pass (p=none dis=none) header.from=crates.io",
    "mx.google.com;\r\n       dkim=pass header.i=@paypal.com header.s=pp-dkim1 header.b=x;\r\n       spf=pass (google.com: domain of x@paypal.com designates 1.2.3.4 as permitted sender) smtp.mailfrom=x@paypal.com;\r\n       dmarc=pass (p=REJECT sp=REJECT dis=NONE) header.from=paypal.com",
    "\"quoted.example\" 1; dkim/1 = pass reason=\"a; b\" header.d = example.org",
    "example.net; none",
    "(comment (nested) \\) still) mx.example; dmarc=fail",
];

/// Every parse of a value, and the verdict on it for a few From domains.
fn exercise_auth(value: &str) {
    if let Some(header) = auth::parse(value) {
        for domain in ["paypal.com", "", ".", "co.uk", "a.b.c.d.e"] {
            let _ = header.authenticates(domain);
        }
    }
    let raw = format!("Authentication-Results: {value}\r\nFrom: a@paypal.com\r\n\r\n");
    let _ = auth::authenticated(raw.as_bytes(), "a@paypal.com", |_| true);
    for domain in [value, "", "."] {
        let _ = auth::org_domain(domain);
        let _ = auth::in_domain(domain, "paypal.com");
        let _ = auth::address_domain(domain);
    }
}

/// Fragments that sit on the grammar's edges: separators, comments, quotes and escapes.
fn auth_piece() -> BoxedStrategy<String> {
    prop_oneof![
        "[a-z0-9.@/-]{0,8}",
        prop::sample::select(vec![
            ";",
            "=",
            "(",
            ")",
            "\"",
            "\\",
            " ",
            "\r\n ",
            "dkim=pass",
            "dmarc=pass",
            "header.d=",
            "header.i=@",
            "header.from=",
            "\u{0}",
            "\u{202e}",
            "ü",
        ])
        .prop_map(str::to_string),
    ]
    .boxed()
}

proptest! {
    #![proptest_config(config())]

    #[test]
    fn auth_results_arbitrary_text(value in ".{0,256}") {
        exercise_auth(&value);
    }

    #[test]
    fn auth_results_from_pieces(pieces in proptest::collection::vec(auth_piece(), 0..48)) {
        exercise_auth(&pieces.concat());
    }

    #[test]
    fn auth_results_mutated(seed in 0..AUTH_RESULTS.len(), donor in 0..AUTH_RESULTS.len(), ms in mutations()) {
        let bytes = mutate(AUTH_RESULTS[seed].as_bytes().to_vec(), AUTH_RESULTS[donor].as_bytes(), &ms);
        exercise_auth(&String::from_utf8_lossy(&bytes));
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Wire

fn wire_samples() -> Vec<String> {
    let event = Event::Activity {
        account: "work".into(),
        activity: Activity::FetchingHeaders {
            folder: "INBOX".into(),
            done: 1200,
            total: 5000,
        },
    };
    let clients = [
        ClientMessage::Hello {
            protocol: wire::PROTOCOL,
            version: "0.1.0".into(),
        },
        ClientMessage::Subscribe { id: 1 },
        ClientMessage::Status { id: 2 },
        ClientMessage::Shutdown { id: 3 },
        ClientMessage::Command {
            id: 4,
            account: "work".into(),
            command: Command::Apply {
                folder: "INBOX".into(),
                uids: vec![1, 2, 3],
                action: Action::Move("Lists/GitHub".into()),
                by: "gui".into(),
            },
        },
        ClientMessage::Command {
            id: 5,
            account: "home".into(),
            command: Command::Restore {
                file: PathBuf::from("/tmp/trash/1.eml"),
            },
        },
        ClientMessage::Command {
            id: 6,
            account: "home".into(),
            command: Command::ApplyRule { name: "r".into() },
        },
        ClientMessage::Command {
            id: 7,
            account: "home".into(),
            command: Command::SyncNow,
        },
    ];
    let daemons = [
        DaemonMessage::Hello {
            protocol: wire::PROTOCOL,
            version: "0.1.0".into(),
            pid: 42,
        },
        DaemonMessage::Reply {
            id: 2,
            outcome: Outcome::Ok(Payload::Status(Status {
                pid: 42,
                version: "0.1.0".into(),
                uptime_secs: 10,
                clients: 1,
                accounts: vec![AccountStatus {
                    name: "work".into(),
                    activity: Some(Activity::Offline {
                        reason: "dns".into(),
                        retry_at: 1_791_280_800,
                    }),
                    uidplus: Some(true),
                }],
            })),
        },
        DaemonMessage::Reply {
            id: 4,
            outcome: Outcome::Error("no such account".into()),
        },
        DaemonMessage::Event(Event::Synced {
            account: "work".into(),
            new_messages: 3,
            actions: 1,
            requests: vec![4, 5],
            errors: vec!["x".into()],
        }),
        DaemonMessage::Event(Event::NewMail {
            account: "work".into(),
            folder: "INBOX".into(),
            uid: 9,
            from: "Alice <alice@example.com>".into(),
            subject: "hi".into(),
        }),
        DaemonMessage::Reply {
            id: 5,
            outcome: Outcome::Ok(Payload::Event(event.clone())),
        },
        DaemonMessage::Event(event),
    ];
    let mut lines = Vec::new();
    for message in clients {
        let mut line = Vec::new();
        wire::write_line(&mut line, &message).unwrap();
        lines.push(String::from_utf8(line).unwrap());
    }
    for message in daemons {
        let mut line = Vec::new();
        wire::write_line(&mut line, &message).unwrap();
        lines.push(String::from_utf8(line).unwrap());
    }
    lines
}

/// Decodes the line as either side would, then re-encodes and renders whatever decoded.
fn exercise_wire(line: &str) {
    if let Ok(message) = serde_json::from_str::<ClientMessage>(line) {
        wire::write_line(&mut Vec::new(), &message).unwrap();
    }
    if let Ok(message) = serde_json::from_str::<DaemonMessage>(line) {
        wire::write_line(&mut Vec::new(), &message).unwrap();
        let activity = match &message {
            DaemonMessage::Event(Event::Activity { activity, .. }) => Some(activity.clone()),
            DaemonMessage::Reply {
                outcome: Outcome::Ok(Payload::Status(status)),
                ..
            } => status.accounts.iter().find_map(|a| a.activity.clone()),
            _ => None,
        };
        if let Some(activity) = activity {
            let _ = activity.to_string();
        }
        if let DaemonMessage::Event(event) = &message {
            let _ = event.request_ids();
        }
    }
}

fn activity() -> impl Strategy<Value = Activity> {
    prop_oneof![
        Just(Activity::Connecting),
        Just(Activity::ListingFolders),
        (".{0,8}", any::<usize>(), any::<usize>())
            .prop_map(|(folder, index, of)| Activity::SyncingFolder { folder, index, of }),
        (".{0,8}", any::<usize>(), any::<usize>()).prop_map(|(folder, done, total)| {
            Activity::FetchingHeaders {
                folder,
                done,
                total,
            }
        }),
        (".{0,8}", any::<usize>(), any::<usize>()).prop_map(|(folder, done, total)| {
            Activity::FetchingBodies {
                folder,
                done,
                total,
            }
        }),
        ".{0,8}".prop_map(|folder| Activity::RunningRules { folder }),
        ".{0,8}".prop_map(|what| Activity::RunningCommand { what }),
        any::<i64>().prop_map(|since| Activity::Idle { since }),
        (".{0,8}", any::<i64>())
            .prop_map(|(reason, retry_at)| Activity::Offline { reason, retry_at }),
        ".{0,8}".prop_map(|reason| Activity::NotRunning { reason }),
    ]
}

proptest! {
    #![proptest_config(config())]

    #[test]
    fn wire_arbitrary_line(line in ".{0,256}") {
        exercise_wire(&line);
    }

    #[test]
    fn wire_json_shaped_line(line in "[{}\\[\\]\":,0-9a-z_ .eE+-]{0,128}") {
        exercise_wire(&line);
    }

    #[test]
    fn wire_mutated_line(seed in any::<prop::sample::Index>(), donor in any::<prop::sample::Index>(), ms in mutations()) {
        let samples = wire_samples();
        let bytes = mutate(seed.get(&samples).as_bytes().to_vec(), donor.get(&samples).as_bytes(), &ms);
        exercise_wire(&String::from_utf8_lossy(&bytes));
    }

    #[test]
    fn wire_activity_with_any_numbers(account in ".{0,8}", activity in activity()) {
        let line = wire::write_line(&mut Vec::new(), &DaemonMessage::Event(Event::Activity { account, activity: activity.clone() }));
        line.unwrap();
        let _ = activity.to_string();
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Named cases

#[test]
fn deeply_nested_multipart_does_not_panic() {
    let depth = 2000;
    let mut raw = b"From: a@example.com\r\nSubject: deep\r\nMIME-Version: 1.0\r\n".to_vec();
    for level in 0..depth {
        raw.extend_from_slice(
            format!("Content-Type: multipart/mixed; boundary=\"b{level}\"\r\n\r\n--b{level}\r\n")
                .as_bytes(),
        );
    }
    raw.extend_from_slice(b"Content-Type: text/plain\r\n\r\nbottom\r\n");
    exercise_message(&raw, "INBOX", 1);
}

#[test]
fn deeply_nested_rfc822_does_not_panic() {
    let mut raw = Vec::new();
    for _ in 0..2000 {
        raw.extend_from_slice(b"From: a@example.com\r\nContent-Type: message/rfc822\r\n\r\n");
    }
    raw.extend_from_slice(b"Subject: bottom\r\n\r\nbottom\r\n");
    exercise_message(&raw, "INBOX", 1);
}

#[test]
fn deeply_nested_json_line_does_not_panic() {
    let line = format!("{}{}", "[".repeat(100_000), "]".repeat(100_000));
    exercise_wire(&line);
    let line = format!("{}{}", "{\"hello\":".repeat(10_000), "}".repeat(10_000));
    exercise_wire(&line);
}

#[test]
fn deeply_nested_toml_does_not_panic() {
    exercise_rules(&format!(
        "x = {}{}",
        "[".repeat(100_000),
        "]".repeat(100_000)
    ));
    exercise_rules(&format!(
        "x = {}1{}",
        "{a=".repeat(10_000),
        "}".repeat(10_000)
    ));
}

#[test]
fn rule_docs_examples_are_found() {
    assert!(rule_examples().len() >= 4);
}

#[test]
fn known_upstream_panics_are_tolerated() {
    exercise_message(
        b"Content-Type: multipart/mixed; boundary=\"b\"\r\n\r\n--b\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\n=\r\n--b--\r\n",
        "INBOX",
        1,
    );
    exercise_message(
        b"Content-Type: multipart/mixed; boundary=outer\n\n--outerContent-Type: message/rfc822\nC\n\nContent-Type: message/rfc822\n\n\n--outer--",
        "INBOX",
        1,
    );
}

#[test]
#[should_panic(expected = "not a known upstream panic")]
fn other_panics_still_fail() {
    tolerating_known_upstream_panics(|| panic!("not a known upstream panic"));
}
