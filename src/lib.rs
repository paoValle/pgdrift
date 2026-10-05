//! `pgdrift` — would this migration block production? A static verdict, and then a measurement.
//!
//! Two questions, and the tool answers them in that order because the second is what makes the first
//! trustworthy:
//!
//! 1. **[`rules`]**: what PostgreSQL's documented behaviour says about every statement in a
//!    migration, with the reason and the safer alternative. A statement that is not in the table is
//!    reported as unknown — `pgdrift` never guesses a lock level;
//! 2. **[`probe`]**: what the database *actually does*, measured against a live PostgreSQL by
//!    holding a read lock and a write lock in other sessions and trying the statement with
//!    `lock_timeout` set. When the measurement disagrees with the table, the report says so.
//!
//! And a third, because a migration is not the only way a schema hurts: [`catalog`] lists what the
//! schema already hides (unindexed foreign keys, tables without a primary key, invalid indexes,
//! `int4` primary keys).
//!
//! The lexer in [`sql`] is small on purpose: splitting a migration file on `;` is how a tool starts
//! lying about what a migration does.

pub mod catalog;
pub mod cli;
pub mod probe;
pub mod rules;
pub mod sql;

pub use catalog::{audit, Finding};
pub use probe::{probe_statement, Probe};
pub use rules::{classify, LockLevel, Rule};
pub use sql::{split, ParseError, Statement};
