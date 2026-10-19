//! From a `Query` to the FTS5 expression. Every word and phrase becomes a quoted FTS5 string, so nothing the user
//! typed is read as FTS5 syntax.

use super::{Plan, Query, Scope, Term};

/// The columns a `Headers` search may decide on.
const HEADER_COLUMNS: &str = "{subject from_addr to_addr cc_addr delivered_to attachment_names}";

/// A shorter last word is not a prefix: `a*` would merge every term starting with that letter.
const PREFIX_MIN_CHARS: usize = 3;

pub fn plan(query: &Query, scope: Scope) -> Plan {
    let mut positive = Vec::new();
    let mut negative = Vec::new();
    let last_positive = query
        .terms
        .iter()
        .rposition(|t| !matches!(t, Term::Not(_)));
    for (i, term) in query.terms.iter().enumerate() {
        match term {
            Term::Not(inner) => negative.push(fts_term(inner, false)),
            term => positive.push(fts_term(term, last_positive == Some(i))),
        }
    }
    let exact = if positive.is_empty() {
        String::new()
    } else {
        let mut expr = match positive.len() {
            1 => positive.remove(0),
            _ => format!("({})", positive.join(" AND ")),
        };
        for n in negative {
            expr = format!("{expr} NOT {n}");
        }
        match scope {
            Scope::All => expr,
            Scope::Headers => format!("{HEADER_COLUMNS} : ({expr})"),
        }
    };
    Plan {
        exact,
        close: None,
        sql: Vec::new(),
        params: Vec::new(),
    }
}

/// One term as FTS5 text; a `Word` that ends the query matches as a prefix when it is long enough.
fn fts_term(term: &Term, last: bool) -> String {
    match term {
        Term::Word(w) if last && w.chars().count() >= PREFIX_MIN_CHARS => {
            format!("{}*", quote(w))
        }
        Term::Word(w) | Term::Phrase(w) => quote(w),
        Term::Not(inner) => fts_term(inner, false),
    }
}

/// An FTS5 string: double quotes, with inner ones doubled, which FTS5 always parses as plain text.
fn quote(text: &str) -> String {
    format!("\"{}\"", text.replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::parse;

    fn exact(text: &str) -> String {
        plan(&parse(text), Scope::All).exact
    }

    #[test]
    fn the_last_word_is_a_prefix_and_phrases_and_negations_are_not() {
        assert_eq!(exact("invoice"), "\"invoice\"*");
        assert_eq!(exact("acme invoice"), "(\"acme\" AND \"invoice\"*)");
        assert_eq!(exact("\"status update\""), "\"status update\"");
        assert_eq!(exact("invoice -\"status update\""), "\"invoice\"* NOT \"status update\"");
        assert_eq!(exact("ab -cd efg -hi"), "(\"ab\" AND \"efg\"*) NOT \"cd\" NOT \"hi\"");
        assert_eq!(exact("invoice -acme"), "\"invoice\"* NOT \"acme\"", "the prefix stays on the last positive word");
    }

    #[test]
    fn short_last_words_are_not_prefixes() {
        assert_eq!(exact("invoice to"), "(\"invoice\" AND \"to\")");
        assert_eq!(exact("abc"), "\"abc\"*");
    }

    #[test]
    fn only_negatives_or_nothing_plan_to_an_empty_expression() {
        assert_eq!(exact("-spam"), "");
        assert_eq!(exact(""), "");
    }

    #[test]
    fn user_text_never_becomes_syntax() {
        // An opening quote runs to the end: the whole attempt is one phrase.
        assert_eq!(exact("x\") OR body_text:secret"), "\"x) OR body_text:secret\"");
        assert_eq!(exact("say \"hi\" there"), "(\"say\" AND \"hi\" AND \"there\"*)");
        assert_eq!(exact("NEAR(a b)"), "(\"NEAR(a\" AND \"b)\")");
    }

    #[test]
    fn the_headers_scope_wraps_everything() {
        assert_eq!(
            plan(&parse("invoice -spam"), Scope::Headers).exact,
            "{subject from_addr to_addr cc_addr delivered_to attachment_names} : (\"invoice\"* NOT \"spam\")"
        );
        assert_eq!(plan(&parse("-spam"), Scope::Headers).exact, "");
    }
}
