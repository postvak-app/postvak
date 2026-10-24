//! The query syntax: words, `"phrases"` and `-not`. Anything else is a word, so parsing cannot fail.

use super::{Query, Term};

/// Reads a query. Whitespace separates terms; a term with a `"` in it is a phrase that runs to the next `"` or the
/// end, spaces included; a leading `-` negates. A term without a letter or digit is dropped, since FTS5 would
/// tokenize it to nothing. Control characters count as whitespace: FTS5 reads the expression as a C string, so a
/// NUL would end it early.
pub fn parse(text: &str) -> Query {
    let mut terms = Vec::new();
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
        let term = if was_quoted {
            Term::Phrase(body.to_string())
        } else {
            Term::Word(body.to_string())
        };
        terms.push(if negated {
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
    Query {
        terms,
        filters: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(s: &str) -> Term {
        Term::Word(s.into())
    }

    fn phrase(s: &str) -> Term {
        Term::Phrase(s.into())
    }

    fn not(t: Term) -> Term {
        Term::Not(Box::new(t))
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
            assert_eq!(parse(text).terms, expected, "{text:?}");
        }
    }

    #[test]
    fn nothing_to_match_is_dropped() {
        for text in ["", "   ", "\"\"", "-", "--", "- -", "...", "\" \"", "-\"\""] {
            assert!(parse(text).is_empty(), "{text:?}");
        }
    }

    #[test]
    fn control_characters_separate_words_and_blank_inside_phrases() {
        assert_eq!(
            parse("a\0b\tc").terms,
            vec![word("a"), word("b"), word("c")]
        );
        assert_eq!(parse("\"a\0b\"").terms, vec![phrase("a b")]);
        assert!(parse("\0\x01").is_empty());
    }

    #[test]
    fn a_second_dash_stays_in_the_word() {
        assert_eq!(parse("--x").terms, vec![not(word("-x"))]);
    }
}
