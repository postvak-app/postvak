//! The query syntax: words, `"phrases"`, `-not` and `key:value` operators. Anything else is a word, so parsing
//! cannot fail.

use super::{Column, Filter, Query, Term};

/// Reads a query. Whitespace separates tokens; a token with a `"` in it is a phrase that runs to the next `"` or the
/// end, spaces included; a leading `-` negates. A token `key:value` with a known key is a filter (`in:"Projects/A"`
/// quotes a value with spaces); with an unknown key, or a value the key cannot read, it stays a word. A token
/// without a letter or digit is dropped, since FTS5 would tokenize it to nothing. Control characters count as
/// whitespace: FTS5 reads the expression as a C string, so a NUL would end it early. `now` resolves relative dates
/// such as `after:7d`.
pub fn parse(text: &str, now: i64) -> Query {
    let mut query = Query::default();
    let mut token = String::new();
    let mut quoted = false;
    let mut in_quote = false;
    let mut flush = |token: &mut String, quoted: &mut bool| {
        let raw = std::mem::take(token);
        let was_quoted = std::mem::take(quoted);
        let (negated, body) = match raw.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, raw.as_str()),
        };
        if !body.chars().any(char::is_alphanumeric) {
            return;
        }
        if let Some(filter) = filter(body, was_quoted, negated, now) {
            query.filters.push(filter);
            return;
        }
        let term = if was_quoted {
            Term::Phrase(body.to_string())
        } else {
            Term::Word(body.to_string())
        };
        query.terms.push(if negated {
            Term::Not(Box::new(term))
        } else {
            term
        });
    };
    for c in text.chars() {
        match c {
            '"' => {
                in_quote = !in_quote;
                quoted = true;
            }
            c if (c.is_whitespace() || c.is_control()) && !in_quote => {
                flush(&mut token, &mut quoted);
            }
            c if c.is_control() => token.push(' '),
            c => token.push(c),
        }
    }
    flush(&mut token, &mut quoted);
    query
}

/// The filter a `key:value` token means, if its key is known and its value readable. A `-` before a text key
/// excludes the column term; before any other key it is ignored, since `-is:read` has no clear meaning.
fn filter(body: &str, quoted: bool, negated: bool, now: i64) -> Option<Filter> {
    let (key, value) = body.split_once(':')?;
    if value.is_empty() {
        return None;
    }
    let column = match key.to_ascii_lowercase().as_str() {
        "from" => Column::From,
        "to" => Column::To,
        "cc" => Column::Cc,
        "subject" => Column::Subject,
        "attachment" => Column::Attachment,
        "in" => return Some(Filter::In(value.to_string())),
        "account" => return Some(Filter::Account(value.to_string())),
        "has" => {
            return matches!(
                value.to_ascii_lowercase().as_str(),
                "attachment" | "attachments"
            )
            .then_some(Filter::HasAttachment);
        }
        "is" => {
            return match value.to_ascii_lowercase().as_str() {
                "unread" => Some(Filter::Unread),
                "read" => Some(Filter::Read),
                "flagged" => Some(Filter::Flagged),
                _ => None,
            };
        }
        "before" => return date(value, now).map(Filter::Before),
        "after" => return date(value, now).map(Filter::After),
        _ => return None,
    };
    if !quoted
        && matches!(column, Column::From | Column::To | Column::Cc)
        && value.eq_ignore_ascii_case("me")
    {
        return Some(Filter::Me { column, negated });
    }
    let term = if quoted {
        Term::Phrase(value.to_string())
    } else {
        Term::Word(value.to_string())
    };
    Some(Filter::Text(
        column,
        if negated {
            Term::Not(Box::new(term))
        } else {
            term
        },
    ))
}

/// A date value: `YYYY-MM-DD` is local midnight of that day; a `time::Span` such as `7d`, `3w`, `6mo` or `1y` counts
/// back from `now`. What `before:` and `after:` read, and the `--before` and `--after` of sender statistics.
pub fn date(value: &str, now: i64) -> Option<i64> {
    if let Ok(span) = crate::time::Span::parse(value) {
        return span.before(now);
    }
    crate::time::local_date(value)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A fixed `now` for the tests: 2026-10-24 12:00 UTC.
    pub(crate) const NOW: i64 = 1_792_843_200;

    fn word(s: &str) -> Term {
        Term::Word(s.into())
    }

    fn phrase(s: &str) -> Term {
        Term::Phrase(s.into())
    }

    fn not(t: Term) -> Term {
        Term::Not(Box::new(t))
    }

    fn terms(text: &str) -> Vec<Term> {
        parse(text, NOW).terms
    }

    fn filters(text: &str) -> Vec<Filter> {
        parse(text, NOW).filters
    }

    #[test]
    fn words_phrases_and_negation() {
        let cases: [(&str, Vec<Term>); 9] = [
            ("invoice", vec![word("invoice")]),
            ("  two   words ", vec![word("two"), word("words")]),
            ("\"status update\"", vec![phrase("status update")]),
            ("-newsletter", vec![not(word("newsletter"))]),
            ("-\"b c\"", vec![not(phrase("b c"))]),
            ("\"unbalanced", vec![phrase("unbalanced")]),
            ("re: lunch", vec![word("re:"), word("lunch")]),
            (
                "\"a\" -\"b c\" d",
                vec![phrase("a"), not(phrase("b c")), word("d")],
            ),
            ("alice@x.nl", vec![word("alice@x.nl")]),
        ];
        for (text, expected) in cases {
            assert_eq!(terms(text), expected, "{text:?}");
        }
    }

    #[test]
    fn nothing_to_match_is_dropped() {
        for text in ["", "   ", "\"\"", "-", "--", "- -", "...", "\" \"", "-\"\""] {
            assert!(parse(text, NOW).is_empty(), "{text:?}");
        }
    }

    #[test]
    fn control_characters_separate_words_and_blank_inside_phrases() {
        assert_eq!(terms("a\0b\tc"), vec![word("a"), word("b"), word("c")]);
        assert_eq!(terms("\"a\0b\""), vec![phrase("a b")]);
        assert!(parse("\0\x01", NOW).is_empty());
    }

    #[test]
    fn a_second_dash_stays_in_the_word() {
        assert_eq!(terms("--x"), vec![not(word("-x"))]);
    }

    #[test]
    fn column_operators_take_words_phrases_and_negation() {
        use Column::*;
        let cases: [(&str, Filter); 8] = [
            ("from:acme", Filter::Text(From, word("acme"))),
            ("FROM:acme", Filter::Text(From, word("acme"))),
            ("to:alice@x.nl", Filter::Text(To, word("alice@x.nl"))),
            ("cc:lists", Filter::Text(Cc, word("lists"))),
            (
                "subject:\"status update\"",
                Filter::Text(Subject, phrase("status update")),
            ),
            ("attachment:pdf", Filter::Text(Attachment, word("pdf"))),
            ("-from:acme", Filter::Text(From, not(word("acme")))),
            (
                "-subject:\"status update",
                Filter::Text(Subject, not(phrase("status update"))),
            ),
        ];
        for (text, expected) in cases {
            let query = parse(text, NOW);
            assert_eq!(query.filters, [expected], "{text:?}");
            assert!(query.terms.is_empty(), "{text:?}");
        }
    }

    #[test]
    fn me_is_the_identity_in_the_address_columns_only() {
        use Column::*;
        for (text, column, negated) in [
            ("from:me", From, false),
            ("to:ME", To, false),
            ("cc:me", Cc, false),
            ("-from:me", From, true),
        ] {
            assert_eq!(filters(text), [Filter::Me { column, negated }], "{text:?}");
        }
        assert_eq!(
            filters("subject:me"),
            [Filter::Text(Subject, word("me"))],
            "a subject can be about me"
        );
        assert_eq!(
            filters("from:\"me\""),
            [Filter::Text(From, phrase("me"))],
            "quoted, it is the word"
        );
    }

    #[test]
    fn folder_flag_and_account_operators() {
        let cases: [(&str, Filter); 9] = [
            ("in:Archive", Filter::In("Archive".into())),
            (
                "in:\"Projects/Old explorations\"",
                Filter::In("Projects/Old explorations".into()),
            ),
            ("-in:Archive", Filter::In("Archive".into())),
            ("has:attachment", Filter::HasAttachment),
            ("has:Attachments", Filter::HasAttachment),
            ("is:unread", Filter::Unread),
            ("is:READ", Filter::Read),
            ("is:flagged", Filter::Flagged),
            ("account:work", Filter::Account("work".into())),
        ];
        for (text, expected) in cases {
            assert_eq!(filters(text), [expected], "{text:?}");
        }
        assert_eq!(parse("account:work", NOW).account(), Some("work"));
        assert_eq!(parse("work", NOW).account(), None);
    }

    #[test]
    fn dates_are_local_midnights_and_relative_ones_count_back_from_now() {
        let jan15 = crate::time::local_date("2026-01-15").unwrap();
        assert_eq!(filters("before:2026-01-15"), [Filter::Before(jan15)]);
        assert_eq!(filters("after:2026-01-15"), [Filter::After(jan15)]);
        assert_eq!(filters("after:7d"), [Filter::After(NOW - 7 * 86_400)]);
        assert_eq!(filters("before:3w"), [Filter::Before(NOW - 21 * 86_400)]);
        let six_months = crate::time::months_back(NOW, 6).unwrap();
        assert_eq!(filters("after:6mo"), [Filter::After(six_months)]);
        assert_eq!(
            filters("after:1y"),
            [Filter::After(crate::time::months_back(NOW, 12).unwrap())]
        );
        assert!(NOW - six_months > 180 * 86_400 && NOW - six_months < 186 * 86_400);
    }

    #[test]
    fn unknown_keys_and_unreadable_values_stay_words() {
        for text in [
            "size:10",
            "from:",
            "has:photo",
            "is:starred",
            "before:yesterday",
            "after:2026-13-01",
            "after:7x",
            "after:6m",
            "after:d",
            "re:lunch",
        ] {
            let query = parse(text, NOW);
            assert_eq!(query.terms, [word(text)], "{text:?}");
            assert!(query.filters.is_empty(), "{text:?}");
        }
    }

    #[test]
    fn operators_and_words_mix_in_any_order() {
        let query = parse("from:acme invoice after:7d -spam", NOW);
        assert_eq!(query.terms, [word("invoice"), not(word("spam"))]);
        assert_eq!(
            query.filters,
            [
                Filter::Text(Column::From, word("acme")),
                Filter::After(NOW - 7 * 86_400)
            ]
        );
    }
}
