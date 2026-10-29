//! Search queries: `parse` reads what the user typed and never fails; `plan` turns the result into the one FTS5
//! expression the store runs, and the SQL predicates that follow it. Nothing else in Postvak builds one, so user text reaches SQLite only as quoted FTS5
//! strings and bound parameters (`tests/architecture.rs` checks the table name stays here and in `store.rs`).

mod close;
mod parse;
mod plan;

pub use close::{Candidate, Vocab, expansions};
pub use parse::{date, parse};
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

impl Query {
    /// The account `account:` names, on surfaces that search several.
    pub fn account(&self) -> Option<&str> {
        self.filters.iter().find_map(|filter| match filter {
            Filter::Account(name) => Some(name.as_str()),
            _ => None,
        })
    }
}

/// The one account to search, from a surface's own account argument and the query's `account:`; an error when
/// both are given and disagree.
pub fn named_account<'a>(
    argument: Option<&'a str>,
    query: &'a Query,
) -> Result<Option<&'a str>, AccountMismatch> {
    match (argument, query.account()) {
        (Some(a), Some(b)) if a != b => Err(AccountMismatch {
            argument: a.to_string(),
            query: b.to_string(),
        }),
        (argument, named) => Ok(argument.or(named)),
    }
}

/// `account:` in the query names another account than the one asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountMismatch {
    pub argument: String,
    pub query: String,
}

impl std::fmt::Display for AccountMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the query names account '{}' but '{}' was asked for",
            self.query, self.argument
        )
    }
}

impl std::error::Error for AccountMismatch {}

/// A condition on a message beyond its free text, written `key:value`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Filter {
    /// The term in one column: `from:acme`, `subject:"status update"`, `-cc:lists`; `Not` excludes it.
    Text(Column, Term),
    /// `from:me`, `to:me` or `cc:me`: the identity's address or an alias in that column; `negated` for `-from:me`.
    Me {
        column: Column,
        negated: bool,
    },
    /// `in:Archive`: one folder.
    In(String),
    /// `has:attachment`.
    HasAttachment,
    /// `is:unread`, `is:read`, `is:flagged`.
    Unread,
    Read,
    Flagged,
    /// `before:2026-01-15` or `before:7d`: `internaldate` before this timestamp, exclusive.
    Before(i64),
    /// `after:2026-01-01` or `after:7d`: `internaldate` at or after this timestamp.
    After(i64),
    /// `account:work`: one account, on surfaces that search several.
    Account(String),
}

/// The header columns a `key:` can name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Column {
    From,
    /// To, Cc and Delivered-To together.
    To,
    Cc,
    Subject,
    Attachment,
}

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
    /// With `exact` empty: the FTS5 expression of the excluded terms, which the predicates alone then honour.
    pub exclude: Option<String>,
    /// Each an SQL predicate on the `messages` row, such as `internaldate >= ?`; their `?` placeholders are
    /// numbered after the store's fixed ones, in order.
    pub sql: Vec<String>,
    pub params: Vec<rusqlite::types::Value>,
}

impl Plan {
    /// Whether running the plan can find anything: a positive expression, or predicates on their own.
    pub fn is_empty(&self) -> bool {
        self.exact.is_empty() && self.sql.is_empty()
    }
}
