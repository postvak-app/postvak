//! Close matches: a query word expanded with the indexed terms within a small edit distance of it, so a typo still
//! finds the message. The terms come from the index's own vocabulary, so nothing is suggested that no message holds.

use super::{Query, Term};

/// Shorter words are not expanded: within one edit of `inv` sits half the vocabulary.
const MIN_CHARS: usize = 4;
/// Words this long allow two edits.
const TWO_EDITS_FROM: usize = 8;
/// The most terms one word expands to, by how many messages hold them.
const MAX_EXPANSIONS: usize = 5;
/// A body-only term must appear in this many messages to count: bodies are full of one-off tokens.
const MIN_BODY_DOCS: u64 = 3;

/// A term of the search index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub term: String,
    /// Messages holding the term, in any column.
    pub docs: u64,
    /// Whether a header column (subject, addresses, attachment names) holds it in some message.
    pub in_headers: bool,
}

/// The search index's vocabulary.
pub trait Vocab {
    /// `word` as the index's tokenizer stores it, when it is exactly one token: lower-cased, accents folded; None
    /// when it splits into several tokens or into none.
    fn fold(&self, word: &str) -> Option<String>;
    /// The indexed terms that start with `first` and are `min_len..=max_len` characters long.
    fn candidates(&self, first: char, min_len: usize, max_len: usize) -> Vec<Candidate>;
}

/// The terms `word` may have meant, best first: within one edit (two for a long word), held by a header column or
/// by several messages, at most `MAX_EXPANSIONS`.
pub fn expansions(word: &str, vocab: &dyn Vocab) -> Vec<String> {
    if word.chars().count() < MIN_CHARS {
        return Vec::new();
    }
    let Some(folded) = vocab.fold(word) else {
        return Vec::new();
    };
    let len = folded.chars().count();
    let Some(first) = folded.chars().next() else {
        return Vec::new();
    };
    if len < MIN_CHARS {
        return Vec::new();
    }
    let max_distance = if len < TWO_EDITS_FROM { 1 } else { 2 };
    let mut found: Vec<Candidate> = vocab
        .candidates(first, len - 1, len + 1)
        .into_iter()
        .filter(|c| c.term != folded && (c.in_headers || c.docs >= MIN_BODY_DOCS))
        .filter(|c| strsim::damerau_levenshtein(&folded, &c.term) <= max_distance)
        .collect();
    found.sort_by(|a, b| b.docs.cmp(&a.docs).then_with(|| a.term.cmp(&b.term)));
    found.truncate(MAX_EXPANSIONS);
    found.into_iter().map(|c| c.term).collect()
}

/// Every positive word of the query with its expansions, in query order; words with none are left out.
pub(super) fn expanded_words(query: &Query, vocab: &dyn Vocab) -> Vec<(String, Vec<String>)> {
    query
        .terms
        .iter()
        .filter_map(|term| match term {
            Term::Word(word) => {
                let found = expansions(word, vocab);
                (!found.is_empty()).then(|| (word.clone(), found))
            }
            Term::Phrase(_) | Term::Not(_) => None,
        })
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A fixed vocabulary: `fold` lower-cases, `candidates` filters the list.
    pub(crate) struct Fixed(pub Vec<Candidate>);

    impl Fixed {
        pub(crate) fn of(terms: &[(&str, u64, bool)]) -> Fixed {
            Fixed(
                terms
                    .iter()
                    .map(|(term, docs, in_headers)| Candidate {
                        term: (*term).to_string(),
                        docs: *docs,
                        in_headers: *in_headers,
                    })
                    .collect(),
            )
        }
    }

    impl Vocab for Fixed {
        fn fold(&self, word: &str) -> Option<String> {
            let folded = word.to_lowercase();
            (!folded.contains(' ')).then_some(folded)
        }

        fn candidates(&self, first: char, min_len: usize, max_len: usize) -> Vec<Candidate> {
            self.0
                .iter()
                .filter(|c| c.term.starts_with(first))
                .filter(|c| (min_len..=max_len).contains(&c.term.chars().count()))
                .cloned()
                .collect()
        }
    }

    #[test]
    fn a_typo_finds_the_term_within_one_edit() {
        let vocab = Fixed::of(&[
            ("invoice", 40, true),
            ("invoices", 12, true),
            ("invite", 3, true),
        ]);
        assert_eq!(expansions("invioce", &vocab), ["invoice"]);
        assert_eq!(expansions("Invoce", &vocab), ["invoice"], "case folds");
        assert_eq!(
            expansions("recieve", &Fixed::of(&[("receive", 9, false)])),
            ["receive"]
        );
    }

    #[test]
    fn short_words_and_exact_words_are_not_expanded_and_long_words_allow_two_edits() {
        let vocab = Fixed::of(&[("inv", 10, true), ("ink", 10, true), ("invoice", 40, true)]);
        assert!(expansions("inv", &vocab).is_empty());
        assert_eq!(
            expansions("invoice", &vocab),
            Vec::<String>::new(),
            "no neighbours within one edit"
        );
        let long = Fixed::of(&[("maintenance", 5, true)]);
        assert_eq!(
            expansions("maintanence", &long),
            ["maintenance"],
            "two edits for eleven letters"
        );
        assert!(
            expansions("maintenace", &Fixed::of(&[("maintenances", 5, true)])).is_empty(),
            "two characters longer is out of reach"
        );
    }

    #[test]
    fn body_only_terms_need_several_messages_and_the_best_five_are_kept() {
        let vocab = Fixed::of(&[
            ("recieve", 1, false),
            ("receive", 2, false),
            ("receives", 2, false),
        ]);
        assert_eq!(expansions("recieved", &vocab), Vec::<String>::new());
        let vocab = Fixed::of(&[("receives", 3, false)]);
        assert_eq!(
            expansions("recieved", &vocab),
            ["receives"],
            "three messages are enough"
        );
        let vocab = Fixed::of(&[("receive", 2, false), ("recieve", 2, true)]);
        assert_eq!(
            expansions("receives", &vocab),
            ["recieve"],
            "a header term counts with one message"
        );
        let many: Vec<(String, u64, bool)> =
            (0..8).map(|i| (format!("word{i}"), 10 - i, true)).collect();
        let refs: Vec<(&str, u64, bool)> =
            many.iter().map(|(t, d, h)| (t.as_str(), *d, *h)).collect();
        let found = expansions("wordx", &Fixed::of(&refs));
        assert_eq!(
            found,
            ["word0", "word1", "word2", "word3", "word4"],
            "by messages, then name"
        );
    }

    #[test]
    fn phrases_negations_and_words_that_split_are_never_expanded() {
        let vocab = Fixed::of(&[("invoice", 40, true)]);
        let query = crate::search::parse("\"invioce due\" -invioce invioce");
        assert_eq!(
            expanded_words(&query, &vocab),
            [("invioce".to_string(), vec!["invoice".to_string()])]
        );
        assert!(expansions("in voice", &vocab).is_empty(), "two tokens");
    }
}
