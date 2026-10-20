//! Search queries: `parse` reads what the user typed and never fails; `plan` turns the result into the one FTS5
//! expression the store runs. Nothing else in Postvak builds one, so user text reaches SQLite only as quoted FTS5
//! strings and bound parameters (`tests/architecture.rs` checks the table name stays here and in `store.rs`).

mod close;
mod parse;
mod plan;

pub use close::{Candidate, Vocab, expansions};
pub use parse::parse;
pub use plan::plan;

/// What the user asked for, as `parse` read it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Query {
    pub terms: Vec<Term>,
    pub filters: Vec<Filter>,
}

impl Query {
    /// A query with nothing to match: the store returns no rows for it.
    pub fn is_empty(&self) -> bool {
        self.terms.is_empty() && self.filters.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Term {
    /// Matches as a word; as a prefix too when it ends the query.
    Word(String),
    /// Matches the words in order, never as a prefix.
    Phrase(String),
    /// Excludes what the term matches.
    Not(Box<Term>),
}

/// A condition on a message that is not free text. None yet: the operators arrive with their own change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Filter {}

/// Which columns a search may decide on. `Headers` is what MCP gets without `read:bodies`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    All,
    Headers,
}

/// What the store runs: the FTS5 expression, the predicates on `messages` after it, and their parameters.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Plan {
    /// The FTS5 expression; empty when the query has nothing positive to match.
    pub exact: String,
    /// The expression with close matches, when any word gained one.
    pub close: Option<String>,
    /// Each an SQL predicate on the `messages` row, such as `internaldate >= ?`.
    pub sql: Vec<String>,
    pub params: Vec<rusqlite::types::Value>,
}
