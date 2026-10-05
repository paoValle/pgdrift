//! What a statement will do to a table, according to the manual, and what to do instead.
//!
//! This module is a table, not an algorithm. Lock levels are documented behaviour of PostgreSQL, so
//! the honest implementation is a curated list with a reason for every entry and **no guessing**:
//! a statement that is not in the list is reported as `Unknown`, and `pgdrift` says it will not
//! invent a lock level for it. A linter that guesses confidently about `ACCESS EXCLUSIVE` is how
//! people learn to ignore it.
//!
//! The levels here are PostgreSQL's, from weakest to strongest:
//! `ACCESS SHARE` (a plain `SELECT`) → `ROW SHARE` → `ROW EXCLUSIVE` (`INSERT`/`UPDATE`) →
//! `SHARE UPDATE EXCLUSIVE` (online DDL) → `SHARE` → `SHARE ROW EXCLUSIVE` → `EXCLUSIVE` →
//! `ACCESS EXCLUSIVE`.

/// PostgreSQL's table lock levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LockLevel {
    /// No table lock worth mentioning.
    None,
    /// `INSERT`, `UPDATE`, `DELETE`: blocks other writers' DDL, not readers.
    RowExclusive,
    /// `VACUUM`, `CREATE INDEX CONCURRENTLY`, `VALIDATE CONSTRAINT`: does not block reads or writes.
    ShareUpdateExclusive,
    /// `CREATE INDEX` (without `CONCURRENTLY`): blocks writes for the whole build, allows reads.
    Share,
    /// `CREATE TRIGGER`: blocks writes, allows reads.
    ShareRowExclusive,
    /// `ALTER TABLE`: blocks reads and writes.
    AccessExclusive,
    /// Not in the table, so `pgdrift` will not guess.
    Unknown,
}

impl LockLevel {
    /// A label for reports.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "no table lock",
            Self::RowExclusive => "ROW EXCLUSIVE",
            Self::ShareUpdateExclusive => "SHARE UPDATE EXCLUSIVE",
            Self::Share => "SHARE",
            Self::ShareRowExclusive => "SHARE ROW EXCLUSIVE",
            Self::AccessExclusive => "ACCESS EXCLUSIVE",
            Self::Unknown => "unknown",
        }
    }

    /// Whether a plain `SELECT` in another transaction would be blocked.
    #[must_use]
    pub fn blocks_readers(self) -> bool {
        matches!(self, Self::AccessExclusive | Self::Unknown)
    }

    /// Whether an `UPDATE` in another transaction would be blocked.
    #[must_use]
    pub fn blocks_writers(self) -> bool {
        matches!(
            self,
            Self::AccessExclusive | Self::ShareRowExclusive | Self::Share | Self::Unknown
        )
    }

    /// How much it matters, for sorting and for `--fail-on`.
    #[must_use]
    pub fn severity(self) -> u8 {
        match self {
            Self::None => 0,
            Self::RowExclusive => 1,
            Self::ShareUpdateExclusive => 2,
            Self::Share => 3,
            Self::ShareRowExclusive => 4,
            Self::AccessExclusive => 5,
            Self::Unknown => 6,
        }
    }
}

/// What the manual says about one statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    /// A short name for the statement kind, e.g. `alter table add column`.
    pub kind: String,
    /// The lock level.
    pub lock: LockLevel,
    /// Whether the table is rewritten (a full copy), when that is known.
    pub rewrite: Option<bool>,
    /// The advice, in one line: what to do instead when there is a better way.
    pub advice: String,
}

impl Rule {
    fn new(kind: &str, lock: LockLevel, rewrite: Option<bool>, advice: &str) -> Self {
        Self {
            kind: kind.to_owned(),
            lock,
            rewrite,
            advice: advice.to_owned(),
        }
    }
}

/// Classifies a statement.
#[must_use]
pub fn classify(statement: &str) -> Rule {
    let sql = statement.to_uppercase();

    if sql.starts_with("CREATE INDEX CONCURRENTLY")
        || sql.starts_with("CREATE UNIQUE INDEX CONCURRENTLY")
        || sql.starts_with("DROP INDEX CONCURRENTLY")
    {
        return Rule::new(
            "create index concurrently",
            LockLevel::ShareUpdateExclusive,
            Some(false),
            "online: it does not block reads or writes, but it takes longer and can leave an invalid index if it fails",
        );
    }
    if sql.starts_with("CREATE INDEX") || sql.starts_with("CREATE UNIQUE INDEX") {
        return Rule::new(
            "create index",
            LockLevel::Share,
            Some(false),
            "blocks writes for the whole build: CREATE INDEX CONCURRENTLY instead, outside a transaction",
        );
    }
    if sql.starts_with("REINDEX") {
        return Rule::new(
            "reindex",
            LockLevel::AccessExclusive,
            Some(false),
            "REINDEX takes ACCESS EXCLUSIVE unless CONCURRENTLY is used",
        );
    }
    if sql.starts_with("VACUUM") || sql.starts_with("ANALYZE") {
        return Rule::new(
            "vacuum",
            LockLevel::ShareUpdateExclusive,
            Some(false),
            "safe: it does not block reads or writes",
        );
    }
    if sql.starts_with("CREATE TRIGGER") {
        return Rule::new(
            "create trigger",
            LockLevel::ShareRowExclusive,
            Some(false),
            "blocks writes while it is applied",
        );
    }
    if sql.starts_with("TRUNCATE") {
        return Rule::new(
            "truncate",
            LockLevel::AccessExclusive,
            Some(false),
            "blocks everything and is not transactional across replicas: it is never a migration step",
        );
    }
    if sql.starts_with("DROP TABLE") {
        return Rule::new(
            "drop table",
            LockLevel::AccessExclusive,
            Some(false),
            "brief, but it blocks reads and writes: do it in a low-traffic window",
        );
    }
    if sql.starts_with("CREATE TABLE") || sql.starts_with("CREATE SCHEMA") {
        return Rule::new(
            "create table",
            LockLevel::None,
            Some(false),
            "a new object locks nothing: the risk starts with the first ALTER on an existing one",
        );
    }
    if sql.starts_with("LOCK TABLE") {
        return Rule::new(
            "lock table",
            LockLevel::AccessExclusive,
            Some(false),
            "an explicit lock is a deliberate outage: say why in the commit message",
        );
    }
    if sql.starts_with("COMMENT ON") || sql.starts_with("GRANT ") || sql.starts_with("REVOKE ") {
        return Rule::new(
            "no table lock",
            LockLevel::None,
            Some(false),
            "metadata only",
        );
    }

    if sql.starts_with("ALTER TABLE") {
        return classify_alter(&sql);
    }

    Rule::new(
        "unknown",
        LockLevel::Unknown,
        None,
        "this statement is not in pgdrift's table: it will not guess a lock level. Run `pgdrift prove` against a real database, or add the rule",
    )
}

fn classify_alter(sql: &str) -> Rule {
    if contains(sql, "ADD COLUMN") && contains(sql, "DEFAULT") && contains(sql, "NOT NULL") {
        return Rule::new(
            "alter table add not null column with default",
            LockLevel::AccessExclusive,
            // since PostgreSQL 11 the default is stored as metadata, so there is no rewrite; the
            // lock is still ACCESS EXCLUSIVE, which is the fact this tool exists to make visible
            Some(false),
            "the default no longer rewrites the table (PG11+), but the ACCESS EXCLUSIVE lock is real and queues behind every open transaction",
        );
    }
    if contains(sql, "ADD COLUMN") {
        return Rule::new(
            "alter table add column",
            LockLevel::AccessExclusive,
            Some(false),
            "brief ACCESS EXCLUSIVE: it queues behind open transactions, so a long-running query makes it long",
        );
    }
    if contains(sql, "DROP COLUMN") {
        return Rule::new(
            "alter table drop column",
            LockLevel::AccessExclusive,
            Some(false),
            "brief, and it removes the data: the column is gone for every client the moment it commits",
        );
    }
    if contains(sql, "ALTER COLUMN") && contains(sql, "TYPE") {
        return Rule::new(
            "alter table change column type",
            LockLevel::AccessExclusive,
            Some(true),
            "ACCESS EXCLUSIVE *and* a full rewrite: add a new column, backfill in batches, swap, then drop old",
        );
    }
    if contains(sql, "SET NOT NULL") {
        return Rule::new(
            "alter table set not null",
            LockLevel::AccessExclusive,
            Some(true),
            "scans the table under ACCESS EXCLUSIVE: add a NOT VALID CHECK first, then validate the check, then set NOT NULL",
        );
    }
    if contains(sql, "ADD CONSTRAINT") && contains(sql, "FOREIGN KEY") {
        // Measured with pg_locks on PostgreSQL 16: SHARE ROW EXCLUSIVE on *both* tables, not
        // ACCESS EXCLUSIVE. Worth stating because the manual's wording makes people expect the
        // worse one, and because this entry was wrong here until `pgdrift prove` contradicted it.
        return Rule::new(
            "alter table add foreign key",
            LockLevel::ShareRowExclusive,
            Some(false),
            "blocks writes, not reads: SHARE ROW EXCLUSIVE on both tables. ADD CONSTRAINT ... NOT VALID first, then VALIDATE CONSTRAINT in a separate step, so the scan is not part of the same lock",
        );
    }
    if contains(sql, "VALIDATE CONSTRAINT") {
        return Rule::new(
            "alter table validate constraint",
            LockLevel::ShareUpdateExclusive,
            Some(false),
            "safe: it does not block reads or writes, it only takes time",
        );
    }
    if contains(sql, "ADD CONSTRAINT") && (contains(sql, "PRIMARY KEY") || contains(sql, "UNIQUE"))
    {
        return Rule::new(
            "alter table add unique constraint",
            LockLevel::AccessExclusive,
            Some(false),
            "build the unique index with CREATE UNIQUE INDEX CONCURRENTLY first, then ADD CONSTRAINT ... USING INDEX",
        );
    }
    if contains(sql, "DROP CONSTRAINT") || contains(sql, "RENAME") || contains(sql, "SET SCHEMA") {
        return Rule::new(
            "alter table metadata change",
            LockLevel::AccessExclusive,
            Some(false),
            "brief ACCESS EXCLUSIVE for a metadata change",
        );
    }

    Rule::new(
        "alter table (unclassified form)",
        LockLevel::Unknown,
        None,
        "this ALTER form is not in pgdrift's table: it will not guess. Run `pgdrift prove` against a real database",
    )
}

fn contains(sql: &str, needle: &str) -> bool {
    sql.contains(needle)
}
