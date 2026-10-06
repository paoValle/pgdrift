//! What the schema already hides.
//!
//! Three checks that are **facts** from the catalog, and one that is a judgement and has to be asked
//! for. No guessing, no configuration file:
//!
//! - **an unindexed foreign key**: every `DELETE` or `UPDATE` on the referenced table has to scan
//!   the referencing one, under a lock, to enforce the constraint;
//! - **a table with no primary key**: it cannot be replicated safely, and it cannot be updated
//!   confidently by anything that needs to identify a row;
//! - **an invalid index**: what a failed `CREATE INDEX CONCURRENTLY` leaves behind — it is never
//!   used, and it is still maintained on every write;
//! - **an `int4` primary key**: a 2.1-billion ceiling that a busy table reaches faster than anyone
//!   expects, and changing it later is the migration this tool exists to warn about. It is the one
//!   that is a judgement rather than a fact — a lookup table with a thousand rows and a ceiling
//!   nobody will reach is not the same problem as a table adding a million rows a month — so it runs
//!   only when `--warn-int4-over <rows>` asks for it, and only above that many rows.

use postgres::{Client, NoTls};

/// One thing worth fixing, with where it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// The category, for grouping in the report.
    pub check: &'static str,
    /// What it is about, e.g. `order_items.order_id`.
    pub subject: String,
    /// Why it matters, in one line.
    pub why: String,
}

/// Runs every check.
///
/// `int4_over` is the row threshold for the `int4` primary key check, which is a judgement and not a
/// fact: without it that check does not run at all, and the report says so instead of claiming there
/// was nothing to find.
pub fn audit(url: &str, int4_over: Option<u64>) -> Result<Vec<Finding>, String> {
    let mut client = Client::connect(url, NoTls)
        .map_err(|error| format!("cannot connect to PostgreSQL: {error}"))?;
    let mut findings = Vec::new();
    findings.extend(unindexed_foreign_keys(&mut client)?);
    findings.extend(tables_without_primary_key(&mut client)?);
    findings.extend(invalid_indexes(&mut client)?);
    if let Some(threshold) = int4_over {
        findings.extend(int4_primary_keys(&mut client, threshold)?);
    }
    Ok(findings)
}

/// A condition that keeps the catalog's own tables out of the report.
///
/// `pg_toast_*` is not a detail to leave in: those tables belong to TOAST, they have `int4` primary
/// keys, and reporting forty of them would bury the one row about `sessions.id` that a human needs to
/// see. A tool that reports noise is a tool that gets ignored.
const NOT_USER_SCHEMAS: &str = "n.nspname NOT LIKE 'pg\\_%' AND n.nspname <> 'information_schema'";

fn unindexed_foreign_keys(client: &mut Client) -> Result<Vec<Finding>, String> {
    // the prefix comparison is the point: an index on (a, b) also serves a foreign key on (a)
    let sql = format!(
        "SELECT c.conrelid::regclass::text || '.' || array_to_string(
                    ARRAY(SELECT a.attname FROM unnest(c.conkey) AS k
                          JOIN pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = k), ', '),
                c.conname::text
         FROM pg_constraint c
         JOIN pg_class t ON t.oid = c.conrelid
         JOIN pg_namespace n ON n.oid = t.relnamespace
         WHERE c.contype = 'f' AND {NOT_USER_SCHEMAS}
           AND NOT EXISTS (
             SELECT 1 FROM pg_index i
             WHERE i.indrelid = c.conrelid AND i.indisvalid
               AND (string_to_array(i.indkey::text, ' ')::int[])[1:array_length(c.conkey, 1)]
                   = (c.conkey::int[])[1:array_length(c.conkey, 1)])
         ORDER BY 1"
    );
    let rows = client
        .query(&sql, &[])
        .map_err(|error| format!("unindexed foreign keys: {error}"))?;
    Ok(rows
        .iter()
        .map(|row| Finding {
            check: "unindexed foreign key",
            subject: format!("{} ({})", row.get::<_, String>(0), row.get::<_, String>(1)),
            why: "deleting or updating the referenced row scans the whole referencing table, under a lock"
                .to_owned(),
        })
        .collect())
}

fn tables_without_primary_key(client: &mut Client) -> Result<Vec<Finding>, String> {
    let sql = format!(
        "SELECT c.relname::text
         FROM pg_class c
         JOIN pg_namespace n ON n.oid = c.relnamespace
         WHERE c.relkind IN ('r', 'p') AND {NOT_USER_SCHEMAS}
           AND NOT EXISTS (SELECT 1 FROM pg_index i WHERE i.indrelid = c.oid AND i.indisprimary)
         ORDER BY 1"
    );
    let rows = client
        .query(&sql, &[])
        .map_err(|error| format!("tables without a primary key: {error}"))?;
    Ok(rows
        .iter()
        .map(|row| Finding {
            check: "table without a primary key",
            subject: row.get::<_, String>(0),
            why: "no row can be identified: logical replication and anything that updates a single row are out"
                .to_owned(),
        })
        .collect())
}

fn invalid_indexes(client: &mut Client) -> Result<Vec<Finding>, String> {
    let sql = format!(
        "SELECT ci.relname::text, i.indrelid::regclass::text
         FROM pg_index i
         JOIN pg_class ci ON ci.oid = i.indexrelid
         JOIN pg_namespace n ON n.oid = ci.relnamespace
         WHERE NOT i.indisvalid AND {NOT_USER_SCHEMAS}
         ORDER BY 2, 1"
    );
    let rows = client
        .query(&sql, &[])
        .map_err(|error| format!("invalid indexes: {error}"))?;
    Ok(rows
        .iter()
        .map(|row| Finding {
            check: "invalid index",
            subject: format!("{} on {}", row.get::<_, String>(0), row.get::<_, String>(1)),
            why: "what a failed CREATE INDEX CONCURRENTLY leaves: never used by the planner, still written to on every insert"
                .to_owned(),
        })
        .collect())
}

/// Whether an `int4` primary key is worth a line at this size.
///
/// `estimated_rows` is the planner's estimate (`pg_class.reltuples`), not a count: it is what the
/// planner knows without reading the table, and the report says so. A negative value means the
/// table has never been analyzed and the estimate does not exist — which is reported rather than
/// treated as zero, because "I do not know how big this is" is not the same as "this is small".
#[must_use]
fn int4_is_worth_reporting(estimated_rows: f64, threshold: u64) -> bool {
    estimated_rows < 0.0 || estimated_rows >= threshold as f64
}

fn int4_primary_keys(client: &mut Client, threshold: u64) -> Result<Vec<Finding>, String> {
    let sql = format!(
        "SELECT c.relname::text, a.attname::text, c.reltuples::float8
         FROM pg_index i
         JOIN pg_class c ON c.oid = i.indrelid
         JOIN pg_namespace n ON n.oid = c.relnamespace
         JOIN pg_attribute a ON a.attrelid = c.oid AND a.attnum = ANY (i.indkey)
         WHERE i.indisprimary AND a.atttypid = 'int4'::regtype AND {NOT_USER_SCHEMAS}
         ORDER BY 1, 2"
    );
    let rows = client
        .query(&sql, &[])
        .map_err(|error| format!("int4 primary keys: {error}"))?;
    Ok(rows
        .iter()
        .filter(|row| int4_is_worth_reporting(row.get::<_, f64>(2), threshold))
        .map(|row| Finding {
            check: "int4 primary key",
            subject: format!("{}.{}", row.get::<_, String>(0), row.get::<_, String>(1)),
            why: format!(
                "a ceiling of 2.1 billion rows, {}",
                estimate_clause(row.get::<_, f64>(2))
            ),
        })
        .collect())
}

/// How to describe the planner's estimate, without pretending it is a count.
fn estimate_clause(estimated_rows: f64) -> String {
    if estimated_rows < 0.0 {
        return "and no estimate for this table yet (run ANALYZE) — which is not the same as small"
            .to_owned();
    }
    format!(
        "the planner estimates {} rows — and widening the key later is an ACCESS EXCLUSIVE rewrite",
        estimated_rows.round() as i64
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_int4_key_is_reported_from_the_threshold_up() {
        assert!(int4_is_worth_reporting(1_000_000.0, 1_000_000));
        assert!(int4_is_worth_reporting(2_500_000.0, 1_000_000));
        assert!(!int4_is_worth_reporting(999_999.0, 1_000_000));
        assert!(!int4_is_worth_reporting(0.0, 1_000_000));
    }

    #[test]
    fn a_table_that_was_never_analyzed_is_reported_and_says_so() {
        // reltuples is -1 until ANALYZE or autovacuum has run: treating that as small would hide
        // exactly the table the check exists for
        assert!(int4_is_worth_reporting(-1.0, 1_000_000));
        assert!(estimate_clause(-1.0).contains("no estimate"));
        assert!(estimate_clause(-1.0).contains("not the same as small"));
    }

    #[test]
    fn the_estimate_is_reported_as_an_estimate() {
        let why = estimate_clause(1_234_567.4);
        assert!(why.contains("estimates 1234567 rows"), "{why}");
        assert!(!why.contains("counts"), "{why}");
    }
}
