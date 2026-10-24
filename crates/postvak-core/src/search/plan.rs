//! From a `Query` to the FTS5 expression and the SQL predicates. Every word and phrase becomes a quoted FTS5
//! string, so nothing the user typed is read as FTS5 syntax; a filter's value is bound as a parameter.

use crate::config::Identity;

use super::close::expanded_words;
use super::{Column, Filter, Plan, Query, Scope, Term, Vocab};

/// The columns a `Headers` search may decide on.
const HEADER_COLUMNS: &str = "{subject from_addr to_addr cc_addr delivered_to attachment_names}";

/// A shorter last word is not a prefix: `a*` would merge every term starting with that letter.
const PREFIX_MIN_CHARS: usize = 3;

/// Plans the query. With a vocabulary, `close` is the expression with each word that has close matches widened to
/// `(word OR match OR …)`; None when no word has any. With an identity, `from:me` and `to:me` are its address and
/// aliases; without one they are the word `me` in that column.
pub fn plan(
    query: &Query,
    scope: Scope,
    vocab: Option<&dyn Vocab>,
    identity: Option<&Identity>,
) -> Plan {
    let expanded = vocab.map_or_else(Vec::new, |vocab| expanded_words(query, vocab));
    let (exact, exclude) = expression(query, scope, &[], identity.is_some());
    let close = (!expanded.is_empty())
        .then(|| expression(query, scope, &expanded, identity.is_some()).0)
        .filter(|close| !close.is_empty());
    let mut plan = Plan {
        exact,
        close,
        exclude,
        sql: Vec::new(),
        params: Vec::new(),
    };
    for filter in &query.filters {
        predicate(filter, identity, &mut plan);
    }
    plan
}

/// The FTS5 expression, with the words in `expanded` widened to their close matches, and, when nothing positive
/// is left to match, the expression of what the query excludes. `with_identity` says whether `from:me` resolves to
/// the identity (as predicates) or stays the word.
fn expression(
    query: &Query,
    scope: Scope,
    expanded: &[(String, Vec<String>)],
    with_identity: bool,
) -> (String, Option<String>) {
    // The free text, which the `Headers` scope confines to the header columns as one group.
    let mut free_positive = Vec::new();
    let mut free_negative = Vec::new();
    let last_positive = query.terms.iter().rposition(|t| !matches!(t, Term::Not(_)));
    for (i, term) in query.terms.iter().enumerate() {
        match term {
            Term::Not(inner) => free_negative.push(fts_term(inner, false)),
            Term::Word(word) => {
                let own = fts_term(term, last_positive == Some(i));
                match expanded.iter().find(|(w, _)| w == word) {
                    Some((_, close)) => {
                        let close: Vec<String> = close.iter().map(|c| quote(c)).collect();
                        free_positive.push(format!("({own} OR {})", close.join(" OR ")));
                    }
                    None => free_positive.push(own),
                }
            }
            term => free_positive.push(fts_term(term, last_positive == Some(i))),
        }
    }
    // The column terms, already confined to one header column each.
    let mut column_positive = Vec::new();
    let mut column_negative = Vec::new();
    for filter in &query.filters {
        let (column, term) = match filter {
            Filter::Text(column, term) => (*column, term.clone()),
            Filter::Me { column, negated } if !with_identity => {
                let me = Term::Word("me".into());
                let term = if *negated {
                    Term::Not(Box::new(me))
                } else {
                    me
                };
                (*column, term)
            }
            _ => continue,
        };
        match term {
            Term::Not(inner) => {
                column_negative.push(format!(
                    "{} : {}",
                    fts_column(column),
                    fts_term(&inner, false)
                ));
            }
            term => column_positive.push(format!(
                "{} : {}",
                fts_column(column),
                fts_term(&term, false)
            )),
        }
    }
    let scoped = |expr: String| match scope {
        Scope::All => expr,
        Scope::Headers => format!("{HEADER_COLUMNS} : ({expr})"),
    };
    let mut parts = Vec::new();
    let mut negatives = Vec::new();
    if free_positive.is_empty() {
        negatives.extend(free_negative.into_iter().map(&scoped));
    } else {
        // The free negatives stay inside the scoped group, so a body word never excludes a headers hit.
        let mut free = group(free_positive);
        for n in free_negative {
            free = format!("{free} NOT {n}");
        }
        parts.push(scoped(free));
    }
    parts.extend(column_positive);
    negatives.extend(column_negative);
    if parts.is_empty() {
        let exclude = (!negatives.is_empty()).then(|| group_or(negatives));
        return (String::new(), exclude);
    }
    let mut expr = group(parts);
    for n in negatives {
        expr = format!("{expr} NOT {n}");
    }
    (expr, None)
}

/// The parts joined by AND, in parentheses when there are several.
fn group(mut parts: Vec<String>) -> String {
    match parts.len() {
        1 => parts.remove(0),
        _ => format!("({})", parts.join(" AND ")),
    }
}

fn group_or(mut parts: Vec<String>) -> String {
    match parts.len() {
        1 => parts.remove(0),
        _ => format!("({})", parts.join(" OR ")),
    }
}

/// The FTS5 column filter for a `key:`.
fn fts_column(column: Column) -> &'static str {
    match column {
        Column::From => "from_addr",
        Column::To => "{to_addr cc_addr delivered_to}",
        Column::Cc => "cc_addr",
        Column::Subject => "subject",
        Column::Attachment => "attachment_names",
    }
}

/// The SQL predicate a filter adds, with its parameters; a text filter is in the FTS5 expression instead.
fn predicate(filter: &Filter, identity: Option<&Identity>, plan: &mut Plan) {
    match filter {
        Filter::Text(..) | Filter::Account(_) => {}
        Filter::Me { column, negated } => {
            let Some(identity) = identity else { return };
            let mut alternatives = Vec::new();
            for sql_column in sql_columns(*column) {
                let lowered = format!("lower(COALESCE({sql_column}, ''))");
                alternatives.push(format!("instr({lowered}, ?) > 0"));
                plan.params.push(identity.address().to_owned().into());
                for pattern in identity.alias_patterns() {
                    alternatives.push(format!("{lowered} LIKE ? ESCAPE '\\'"));
                    plan.params.push(like_pattern(pattern).into());
                }
            }
            let any = alternatives.join(" OR ");
            plan.sql.push(if *negated {
                format!("NOT ({any})")
            } else {
                any
            });
        }
        Filter::In(folder) => {
            plan.sql.push("folder = ?".into());
            plan.params.push(folder.clone().into());
        }
        Filter::HasAttachment => plan.sql.push("has_attachment".into()),
        Filter::Unread => plan.sql.push(crate::store::UNSEEN.into()),
        Filter::Read => plan.sql.push(format!("NOT ({})", crate::store::UNSEEN)),
        Filter::Flagged => plan.sql.push(crate::store::FLAGGED.into()),
        Filter::Before(at) => {
            plan.sql.push("internaldate < ?".into());
            plan.params.push((*at).into());
        }
        Filter::After(at) => {
            plan.sql.push("internaldate >= ?".into());
            plan.params.push((*at).into());
        }
    }
}

/// The `messages` columns `from:me`, `to:me` and `cc:me` look in.
fn sql_columns(column: Column) -> &'static [&'static str] {
    match column {
        Column::From => &["from_addr"],
        Column::To => &["to_addr", "cc_addr", "delivered_to"],
        Column::Cc => &["cc_addr"],
        Column::Subject => &["subject"],
        Column::Attachment => &["attachment_names"],
    }
}

/// An alias pattern as a LIKE pattern over the lower-cased column: `*` matches anything, and the column holds
/// names and other addresses around it, so it matches anywhere inside.
fn like_pattern(alias: &str) -> String {
    let mut out = String::from("%");
    for c in alias.to_ascii_lowercase().chars() {
        match c {
            '*' => out.push('%'),
            '%' | '_' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out.push('%');
    out
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
    use crate::search::parse::tests::NOW;

    use crate::search::close::tests::Fixed;

    fn planned(text: &str) -> Plan {
        plan(&parse(text, NOW), Scope::All, None, None)
    }

    fn exact(text: &str) -> String {
        planned(text).exact
    }

    fn identity() -> Identity {
        let mut account = crate::config::AccountConfig::new(
            "work",
            "imap.example.com",
            "Me@example.com",
            crate::config::PasswordSource::Command {
                command: "x".into(),
            },
        );
        account.aliases = vec!["me@example.org".into(), "*@shop.example.com".into()];
        account.identity().unwrap()
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
        assert!(planned("").is_empty());
        assert!(planned("-spam").is_empty(), "nothing to exclude from");
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
        assert_eq!(
            exact("from:\"x) OR body_text:secret"),
            "from_addr : \"x) OR body_text:secret\""
        );
    }

    #[test]
    fn the_headers_scope_wraps_the_free_text() {
        let headers = |text: &str| plan(&parse(text, NOW), Scope::Headers, None, None);
        assert_eq!(
            headers("invoice -spam").exact,
            "{subject from_addr to_addr cc_addr delivered_to attachment_names} : (\"invoice\"* NOT \"spam\")"
        );
        assert_eq!(headers("-spam").exact, "");
        assert_eq!(
            headers("from:acme invoice").exact,
            "({subject from_addr to_addr cc_addr delivered_to attachment_names} : (\"invoice\"*) AND from_addr : \"acme\")",
            "a column term is confined already"
        );
        let excluded = headers("-spam is:unread");
        assert_eq!(excluded.exact, "");
        assert_eq!(
            excluded.exclude.as_deref(),
            Some("{subject from_addr to_addr cc_addr delivered_to attachment_names} : (\"spam\")"),
            "a body word never excludes a headers hit"
        );
    }

    #[test]
    fn column_terms_are_confined_never_prefixes_and_negate_outside_the_group() {
        assert_eq!(exact("from:acme"), "from_addr : \"acme\"");
        assert_eq!(
            exact("to:alice subject:\"status update\" cc:lists attachment:pdf"),
            "({to_addr cc_addr delivered_to} : \"alice\" AND subject : \"status update\" AND cc_addr : \"lists\" AND attachment_names : \"pdf\")"
        );
        assert_eq!(
            exact("invoice from:acme -subject:draft"),
            "(\"invoice\"* AND from_addr : \"acme\") NOT subject : \"draft\""
        );
        let only_excluded = planned("-from:acme -spam");
        assert_eq!(only_excluded.exact, "");
        assert_eq!(
            only_excluded.exclude.as_deref(),
            Some("(\"spam\" OR from_addr : \"acme\")")
        );
    }

    #[test]
    fn close_matches_widen_a_word_and_keep_the_prefix_on_the_original() {
        let vocab = Fixed::of(&[("invoice", 40, true), ("acme", 9, true)]);
        let with = |text: &str, scope| plan(&parse(text, NOW), scope, Some(&vocab), None);
        let planned = with("acme invioce", Scope::All);
        assert_eq!(planned.exact, "(\"acme\" AND \"invioce\"*)");
        assert_eq!(
            planned.close.as_deref(),
            Some("(\"acme\" AND (\"invioce\"* OR \"invoice\"))")
        );
        let planned = with("invioce -\"status update\"", Scope::Headers);
        assert_eq!(
            planned.close.as_deref(),
            Some(
                "{subject from_addr to_addr cc_addr delivered_to attachment_names} : ((\"invioce\"* OR \"invoice\") NOT \"status update\")"
            )
        );
        assert_eq!(with("acme", Scope::All).close, None, "nothing to widen");
        assert_eq!(
            plan(&parse("acme invioce", NOW), Scope::All, None, None).close,
            None,
            "no vocabulary"
        );
        assert_eq!(
            with("from:invioce", Scope::All).close,
            None,
            "a filter value is never widened"
        );
    }

    #[test]
    fn filters_become_predicates_with_their_parameters_in_order() {
        let full = planned("invoice in:Archive has:attachment is:unread after:7d before:1d");
        assert_eq!(full.exact, "\"invoice\"*");
        assert_eq!(
            full.sql,
            [
                "folder = ?",
                "has_attachment",
                crate::store::UNSEEN,
                "internaldate >= ?",
                "internaldate < ?",
            ]
        );
        assert_eq!(
            full.params,
            [
                "Archive".to_string().into(),
                (NOW - 7 * 86_400).into(),
                (NOW - 86_400).into(),
            ]
        );
        assert_eq!(
            planned("is:read is:flagged").sql,
            [
                format!("NOT ({})", crate::store::UNSEEN),
                crate::store::FLAGGED.to_string()
            ]
        );
        let only_filters = planned("is:unread");
        assert_eq!(only_filters.exact, "");
        assert!(!only_filters.is_empty(), "the predicates run on their own");
        assert!(
            planned("account:work").is_empty(),
            "account: narrows nothing inside one store"
        );
    }

    #[test]
    fn me_is_the_identity_as_predicates_or_the_word_without_one() {
        let query = parse("from:me", NOW);
        let with = plan(&query, Scope::All, None, Some(&identity()));
        assert_eq!(with.exact, "");
        assert_eq!(
            with.sql,
            [
                "instr(lower(COALESCE(from_addr, '')), ?) > 0 OR lower(COALESCE(from_addr, '')) LIKE ? ESCAPE '\\' OR lower(COALESCE(from_addr, '')) LIKE ? ESCAPE '\\'"
            ]
        );
        assert_eq!(
            with.params,
            [
                "me@example.com".to_string().into(),
                "%me@example.org%".to_string().into(),
                "%%@shop.example.com%".to_string().into(),
            ]
        );
        let to = plan(&parse("-to:me", NOW), Scope::All, None, Some(&identity()));
        assert!(to.sql[0].starts_with("NOT (instr(lower(COALESCE(to_addr, '')), ?) > 0"));
        assert!(to.sql[0].contains("delivered_to"));
        assert_eq!(to.params.len(), 9);
        let without = plan(&query, Scope::All, None, None);
        assert_eq!(without.exact, "from_addr : \"me\"");
        assert!(without.sql.is_empty());
    }

    #[test]
    fn like_patterns_escape_what_like_reads() {
        assert_eq!(like_pattern("*@Shop.example.com"), "%%@shop.example.com%");
        assert_eq!(like_pattern("a_b%c\\d"), "%a\\_b\\%c\\\\d%");
    }
}
