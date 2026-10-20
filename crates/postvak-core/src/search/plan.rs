//! From a `Query` to the FTS5 expression. Every word and phrase becomes a quoted FTS5 string, so nothing the user
//! typed is read as FTS5 syntax.

use super::close::expanded_words;
use super::{Plan, Query, Scope, Term, Vocab};

/// The columns a `Headers` search may decide on.
const HEADER_COLUMNS: &str = "{subject from_addr to_addr cc_addr delivered_to attachment_names}";

/// A shorter last word is not a prefix: `a*` would merge every term starting with that letter.
const PREFIX_MIN_CHARS: usize = 3;

/// Plans the query. With a vocabulary, `close` is the expression with each word that has close matches widened to
/// `(word OR match OR …)`; None when no word has any.
pub fn plan(query: &Query, scope: Scope, vocab: Option<&dyn Vocab>) -> Plan {
    let expanded = vocab.map_or_else(Vec::new, |vocab| expanded_words(query, vocab));
    let exact = expression(query, scope, &[]);
    let close = (!expanded.is_empty()).then(|| expression(query, scope, &expanded));
    Plan {
        exact,
        close,
        sql: Vec::new(),
        params: Vec::new(),
    }
}

/// The FTS5 expression, with the words in `expanded` widened to their close matches.
fn expression(query: &Query, scope: Scope, expanded: &[(String, Vec<String>)]) -> String {
    let mut positive = Vec::new();
    let mut negative = Vec::new();
    let last_positive = query.terms.iter().rposition(|t| !matches!(t, Term::Not(_)));
    for (i, term) in query.terms.iter().enumerate() {
        match term {
            Term::Not(inner) => negative.push(fts_term(inner, false)),
            Term::Word(word) => {
                let own = fts_term(term, last_positive == Some(i));
                match expanded.iter().find(|(w, _)| w == word) {
                    Some((_, close)) => {
                        let close: Vec<String> = close.iter().map(|c| quote(c)).collect();
                        positive.push(format!("({own} OR {})", close.join(" OR ")));
                    }
                    None => positive.push(own),
                }
            }
            term => positive.push(fts_term(term, last_positive == Some(i))),
        }
    }
    if positive.is_empty() {
        return String::new();
    }
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

    use crate::search::close::tests::Fixed;

    fn exact(text: &str) -> String {
        plan(&parse(text), Scope::All, None).exact
    }

    #[test]
    fn the_last_word_is_a_prefix_and_phrases_and_negations_are_not() {
        assert_eq!(exact("invoice"), "\"invoice\"*");
        assert_eq!(exact("acme invoice"), "(\"acme\" AND \"invoice\"*)");
        assert_eq!(exact("\"status update\""), "\"status update\"");
        assert_eq!(
            exact("invoice -\"status update\""),
            "\"invoice\"* NOT \"status update\""
        );
        assert_eq!(
            exact("ab -cd efg -hi"),
            "(\"ab\" AND \"efg\"*) NOT \"cd\" NOT \"hi\""
        );
        assert_eq!(
            exact("invoice -acme"),
            "\"invoice\"* NOT \"acme\"",
            "the prefix stays on the last positive word"
        );
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
        assert_eq!(
            exact("x\") OR body_text:secret"),
            "\"x) OR body_text:secret\""
        );
        assert_eq!(
            exact("say \"hi\" there"),
            "(\"say\" AND \"hi\" AND \"there\"*)"
        );
        assert_eq!(exact("NEAR(a b)"), "(\"NEAR(a\" AND \"b)\")");
    }

    #[test]
    fn the_headers_scope_wraps_everything() {
        assert_eq!(
            plan(&parse("invoice -spam"), Scope::Headers, None).exact,
            "{subject from_addr to_addr cc_addr delivered_to attachment_names} : (\"invoice\"* NOT \"spam\")"
        );
        assert_eq!(plan(&parse("-spam"), Scope::Headers, None).exact, "");
    }

    #[test]
    fn close_matches_widen_a_word_and_keep_the_prefix_on_the_original() {
        let vocab = Fixed::of(&[("invoice", 40, true), ("acme", 9, true)]);
        let planned = plan(&parse("acme invioce"), Scope::All, Some(&vocab));
        assert_eq!(planned.exact, "(\"acme\" AND \"invioce\"*)");
        assert_eq!(
            planned.close.as_deref(),
            Some("(\"acme\" AND (\"invioce\"* OR \"invoice\"))")
        );
        let planned = plan(
            &parse("invioce -\"status update\""),
            Scope::Headers,
            Some(&vocab),
        );
        assert_eq!(
            planned.close.as_deref(),
            Some(
                "{subject from_addr to_addr cc_addr delivered_to attachment_names} : ((\"invioce\"* OR \"invoice\") NOT \"status update\")"
            )
        );
        assert_eq!(
            plan(&parse("acme"), Scope::All, Some(&vocab)).close,
            None,
            "nothing to widen"
        );
        assert_eq!(
            plan(&parse("acme invioce"), Scope::All, None).close,
            None,
            "no vocabulary"
        );
    }
}
