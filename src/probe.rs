//! The measurement: what a statement does to a *live* database, instead of what the manual says.
//!
//! The static table is a claim. This is the check on the claim, and it is the reason `pgdrift`
//! exists as more than a linter: it creates a scratch table, holds a read lock with one session and
//! a write lock with another, and then tries the statement with `lock_timeout` set.
//!
//! What comes out is not "ACCESS EXCLUSIVE according to the documentation" but:
//!
//! - did it get blocked by a reader? (only `ACCESS EXCLUSIVE` and stronger do)
//! - did it get blocked by a writer? (everything from `SHARE` up does)
//! - how long did it wait before it got through?
//!
//! Two booleans and a stopwatch give the lock level, measured. When the measurement disagrees with
//! the table, the report says so, and the table is what is wrong.

// The probe schema is built by appending `format!` lines to one script: `write!` into a `String`
// cannot fail either, and `let _ =` noise would hide the SQL.
#![allow(clippy::format_push_string)]

use std::time::{Duration, Instant};

use postgres::{Client, NoTls};

use crate::rules::{classify, LockLevel};
use crate::sql::{referenced_table, target_table};

/// How long a probe waits for a lock before declaring that it would have been blocked.
const LOCK_TIMEOUT: Duration = Duration::from_millis(300);
/// Rows in the scratch table: enough to matter, small enough to set up in milliseconds.
const ROWS: i64 = 20_000;

/// The result of probing one statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    /// The statement, as written.
    pub statement: String,
    /// The table the probe used, if it could find one.
    pub table: Option<String>,
    /// Why it was skipped, when it was.
    pub skipped: Option<String>,
    /// What the static table predicted.
    pub predicted: LockLevel,
    /// The lock the statement asked PostgreSQL for, read from `pg_locks`.
    ///
    /// This is the measurement that decides whether the table is right. When the statement cannot
    /// run inside a transaction (`CREATE INDEX CONCURRENTLY`), there is nothing to read and the
    /// probe says so instead of guessing.
    pub catalog_level: Option<LockLevel>,
    /// The raw lock modes from `pg_locks`, for the evidence.
    pub catalog_modes: Vec<String>,
    /// What the two holder probes add up to, coarser than `catalog_level`.
    pub measured: Option<LockLevel>,
    /// Whether a reader holding `ACCESS SHARE` blocked it.
    pub blocked_by_reader: Option<bool>,
    /// Whether a writer holding `ROW EXCLUSIVE` blocked it.
    pub blocked_by_writer: Option<bool>,
    /// How long it waited when nothing was holding a lock, in milliseconds.
    pub acquired_ms: Option<u128>,
    /// How long it waited when a reader held one, in milliseconds.
    pub waited_for_reader_ms: Option<u128>,
    /// Human-readable evidence, one line per fact.
    pub evidence: Vec<String>,
}

impl Probe {
    /// Whether the measurement contradicted the table.
    ///
    /// The comparison is against the level PostgreSQL recorded in `pg_locks`, which is the exact
    /// claim the table makes. It is deliberately **not** against the two holder probes: those measure
    /// whether the statement had to *wait*, and waiting is not the same question as what a statement
    /// locks. `CREATE INDEX CONCURRENTLY` waits for open transactions on the table while taking a
    /// lock that conflicts with nothing, and a tool that called that "blocks writes" would be wrong
    /// about the one operation people reach for to avoid blocking writes.
    #[must_use]
    pub fn disagrees(&self) -> bool {
        match self.catalog_level {
            Some(level) => level != self.predicted,
            // no catalog reading (the statement cannot run inside a transaction): nothing to compare
            None => false,
        }
    }

    /// Whether a session holding a lock made the statement wait even though nothing conflicts.
    #[must_use]
    pub fn waits_without_conflicting(&self) -> bool {
        self.catalog_level == Some(LockLevel::ShareUpdateExclusive)
            && (self.blocked_by_reader == Some(true) || self.blocked_by_writer == Some(true))
    }

    /// What the holder probes add up to, coarser than the catalog reading.
    #[must_use]
    pub fn measured_label(&self) -> Option<&'static str> {
        self.measured.map(LockLevel::label)
    }
}

/// Probes one statement against a live database.
///
/// `prior` is the rest of the migration **before** this statement: a migration is a sequence, and a
/// statement measured in an empty schema is a statement measured in a state it will never see. The
/// scratch schema is rebuilt for every probe, the previous statements are replayed into it, and only
/// then is this one tried.
pub fn probe_statement(url: &str, prior: &[String], statement: &str) -> Result<Probe, String> {
    let predicted = classify(statement);
    let mut probe = Probe {
        statement: statement.to_owned(),
        table: None,
        skipped: None,
        predicted: predicted.lock,
        catalog_level: None,
        catalog_modes: Vec::new(),
        measured: None,
        blocked_by_reader: None,
        blocked_by_writer: None,
        acquired_ms: None,
        waited_for_reader_ms: None,
        evidence: Vec::new(),
    };

    let Some(table) = target_table(statement) else {
        probe.skipped = Some("no single table could be identified for this statement".to_owned());
        return Ok(probe);
    };
    if predicted.lock == LockLevel::Unknown && !statement.to_uppercase().starts_with("ALTER TABLE")
    {
        // `prove` refuses to set up what it cannot understand rather than measuring something else
        probe.skipped =
            Some("statement not in the table, and not an ALTER: nothing to set up".to_owned());
        return Ok(probe);
    }
    probe.table = Some(table.clone());

    let mut admin = connect(url)?;

    // three attempts, and the scratch schema is rebuilt before each one: a statement that cannot be
    // rolled back (CREATE INDEX CONCURRENTLY) would otherwise fail on the second attempt because of
    // what the first attempt left behind
    for holder in [Holder::None, Holder::Reader, Holder::Writer] {
        reset(&mut admin, statement, prior, &mut probe)?;
        let attempt = run_once(url, statement, &table, holder)?;
        let (status, elapsed, detail) = (
            attempt.status.clone(),
            attempt.elapsed,
            attempt.detail.clone(),
        );
        match holder {
            Holder::None => {
                probe.acquired_ms = Some(elapsed.as_millis());
                if let Some(error) = &attempt.modes_error {
                    probe
                        .evidence
                        .push(format!("pg_locks could not be read: {error}"));
                } else if !attempt.modes.is_empty() {
                    probe.catalog_level = level_from_modes(&attempt.modes);
                    probe.catalog_modes.clone_from(&attempt.modes);
                    probe.evidence.push(format!(
                        "pg_locks says it asked for {}",
                        attempt.modes.join(", ")
                    ));
                }
                probe.evidence.push(match &status {
                    Status::Succeeded => format!(
                        "with no other session: acquired in {} ms",
                        elapsed.as_millis()
                    ),
                    Status::Blocked => format!(
                        "with no other session: blocked ({} ms) — unexpected",
                        elapsed.as_millis()
                    ),
                    Status::Failed(reason) => format!("with no other session: {reason}"),
                });
                if status != Status::Succeeded {
                    probe.skipped = Some(detail);
                    teardown(&mut admin);
                    return Ok(probe);
                }
            }
            Holder::Reader => {
                probe.waited_for_reader_ms = Some(elapsed.as_millis());
                probe.blocked_by_reader = Some(status == Status::Blocked);
                probe.evidence.push(match &status {
                    Status::Blocked => format!("with a reader holding a lock: blocked after {} ms (lock_timeout) — this blocks reads", elapsed.as_millis()),
                    Status::Succeeded => format!("with a reader holding a lock: acquired in {} ms — reads are never blocked", elapsed.as_millis()),
                    Status::Failed(reason) => format!("with a reader holding a lock: {reason}"),
                });
            }
            Holder::Writer => {
                probe.blocked_by_writer = Some(status == Status::Blocked);
                probe.evidence.push(match &status {
                    Status::Blocked => format!("with a writer holding a lock: blocked after {} ms (lock_timeout) — this blocks writes", elapsed.as_millis()),
                    Status::Succeeded => format!("with a writer holding a lock: acquired in {} ms — writes are never blocked", elapsed.as_millis()),
                    Status::Failed(reason) => format!("with a writer holding a lock: {reason}"),
                });
            }
        }
    }

    probe.measured = Some(match (probe.blocked_by_reader, probe.blocked_by_writer) {
        (Some(true), _) => LockLevel::AccessExclusive,
        (Some(false), Some(true)) => LockLevel::Share,
        (Some(false), Some(false)) => LockLevel::ShareUpdateExclusive,
        _ => LockLevel::Unknown,
    });
    teardown(&mut admin);
    Ok(probe)
}

/// What one attempt saw.
#[derive(Debug)]
struct Attempt {
    status: Status,
    elapsed: Duration,
    detail: String,
    /// The lock modes read from `pg_locks` while the statement was inside its transaction.
    modes: Vec<String>,
    /// Why those modes could not be read, when they could not.
    modes_error: Option<String>,
}

/// What another session holds while the statement is tried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Holder {
    /// Nobody.
    None,
    /// `SELECT` in an open transaction: `ACCESS SHARE`.
    Reader,
    /// `UPDATE` in an open transaction: `ROW EXCLUSIVE` plus a row lock.
    Writer,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Status {
    Succeeded,
    Blocked,
    Failed(String),
}

/// The message a human needs. `postgres::Error`'s own `Display` is literally "db error", which is
/// useless exactly when something went wrong, so the server's message and SQLSTATE are extracted.
fn describe(error: &postgres::Error) -> String {
    error.as_db_error().map_or_else(
        || error.to_string(),
        |db| {
            format!(
                "{} (SQLSTATE {}, severity {})",
                db.message(),
                db.code().code(),
                db.severity()
            )
        },
    )
}

fn connect(url: &str) -> Result<Client, String> {
    Client::connect(url, NoTls).map_err(|error| format!("cannot connect to PostgreSQL: {error}"))
}

/// Rebuilds the scratch schema, and replays the migration's earlier statements into it.
///
/// Every table the migration mentions — the current statement's target, the tables it references,
/// and the ones the earlier statements touched — is created with the same shape, because a replay
/// that fails on a missing table would measure this statement in a state it will never see.
fn reset(
    admin: &mut Client,
    statement: &str,
    prior: &[String],
    probe: &mut Probe,
) -> Result<(), String> {
    let mut names: Vec<String> = Vec::new();
    for sql in prior.iter().chain(std::iter::once(&statement.to_owned())) {
        for name in [target_table(sql), referenced_table(sql)]
            .into_iter()
            .flatten()
        {
            if !names.contains(&name) {
                names.push(name);
            }
        }
    }

    let mut script = String::from(
        "DROP SCHEMA IF EXISTS pgdrift_probe CASCADE; \
         CREATE SCHEMA pgdrift_probe; SET search_path TO pgdrift_probe;",
    );
    for name in &names {
        script.push_str(&format!(
            "CREATE TABLE IF NOT EXISTS {name} (id bigint PRIMARY KEY, v text, n int, order_id bigint, created_at timestamptz DEFAULT now()); \
             INSERT INTO {name} (id, v, n) SELECT g, 'v' || g, g FROM generate_series(1, {ROWS}) AS g;"
        ));
    }
    admin
        .batch_execute(&script)
        .map_err(|error| format!("cannot set up the probe schema: {}", describe(&error)))?;

    for (position, earlier) in prior.iter().enumerate() {
        if let Err(error) = admin.batch_execute(earlier) {
            probe.evidence.push(format!(
                "statement {} of the migration was not replayed into the probe schema: {}",
                position + 1,
                describe(&error)
            ));
        }
    }
    Ok(())
}

fn teardown(admin: &mut Client) {
    let _ = admin.batch_execute("DROP SCHEMA IF EXISTS pgdrift_probe CASCADE;");
}

/// Runs the statement once, optionally with another session holding a lock.
fn run_once(url: &str, statement: &str, table: &str, holder: Holder) -> Result<Attempt, String> {
    // the holder keeps its transaction open until the very end: that is what holds the lock
    let mut holding = match holder {
        Holder::None => None,
        _ => Some(connect(url)?),
    };
    if let Some(client) = holding.as_mut() {
        client
            .batch_execute("SET search_path TO pgdrift_probe; BEGIN;")
            .map_err(|error| {
                format!("cannot start the holder transaction: {}", describe(&error))
            })?;
        let sql = match holder {
            Holder::Reader => format!("SELECT count(*) FROM {table};"),
            _ => format!("UPDATE {table} SET v = v WHERE id = 1;"),
        };
        client
            .batch_execute(&sql)
            .map_err(|error| format!("the holder could not take its lock: {}", describe(&error)))?;
    }

    let mut prober = connect(url)?;
    let timeout_ms = LOCK_TIMEOUT.as_millis();
    let can_run_in_transaction = !needs_own_transaction(statement);
    let mut prelude =
        format!("SET search_path TO pgdrift_probe; SET lock_timeout = '{timeout_ms}ms';");
    if can_run_in_transaction {
        // the statement is rolled back either way: a probe must not change the database it measures
        prelude.push_str(" BEGIN;");
    }
    prober
        .batch_execute(&prelude)
        .map_err(|error| format!("cannot prepare the probe session: {}", describe(&error)))?;

    let started = Instant::now();
    let outcome = prober.batch_execute(statement);
    let elapsed = started.elapsed();

    let status = match &outcome {
        Ok(()) => Status::Succeeded,
        Err(error) => {
            let code = error.code().map(|code| code.code().to_owned());
            match code.as_deref() {
                // 55P03 lock_not_available (what lock_timeout raises), 40P01 deadlock_detected
                Some("55P03" | "40P01") => Status::Blocked,
                _ => Status::Failed(describe(error)),
            }
        }
    };
    let detail = outcome
        .err()
        .map_or_else(|| "no error".to_owned(), |error| describe(&error));

    // the lock is read before the transaction ends: after a commit there is nothing to read
    let mut modes = Vec::new();
    let mut modes_error: Option<String> = None;
    if can_run_in_transaction && status == Status::Succeeded {
        // a failure here is reported, never swallowed: a missing measurement that looks like "no
        // measurement was needed" is how a tool stops being trustworthy
        // the parameter must be a String: `&[&table]` would pass a double reference, which is not
        // serializable, and the failure would look like "no measurement was needed"
        let table_name = table.to_owned();
        match prober.query(
            "SELECT mode::text FROM pg_locks WHERE pid = pg_backend_pid() AND relation = to_regclass($1)",
            &[&table_name],
        ) {
            Ok(rows) => modes = rows.iter().map(|row| row.get::<_, String>(0)).collect(),
            Err(error) => modes_error = Some(describe(&error)),
        }
    }
    if can_run_in_transaction {
        let _ = prober.batch_execute("ROLLBACK;");
    }
    // a statement that ran outside a transaction may have created something: the scratch schema is
    // rebuilt for every statement, so there is nothing else to undo here
    if let Some(client) = holding.as_mut() {
        let _ = client.batch_execute("ROLLBACK;");
    }
    Ok(Attempt {
        status,
        elapsed,
        detail,
        modes,
        modes_error,
    })
}

/// The strongest lock a statement asked for, out of the modes `pg_locks` reported.
///
/// `AccessShareLock` appears in nearly every reading — it is the lock taken just to resolve the table
/// name — and `RowShareLock` is weaker than anything this tool reports, so neither can be the answer
/// on its own: the statement's own lock is the strongest one in the list.
fn level_from_modes(modes: &[String]) -> Option<LockLevel> {
    modes
        .iter()
        .filter_map(|mode| match mode.as_str() {
            "AccessExclusiveLock" => Some(LockLevel::AccessExclusive),
            "ShareRowExclusiveLock" => Some(LockLevel::ShareRowExclusive),
            "ShareLock" => Some(LockLevel::Share),
            "ShareUpdateExclusiveLock" => Some(LockLevel::ShareUpdateExclusive),
            "RowExclusiveLock" => Some(LockLevel::RowExclusive),
            _ => None,
        })
        .max_by_key(|level| level.severity())
}

/// Whether PostgreSQL refuses to run this statement inside a transaction block.
fn needs_own_transaction(statement: &str) -> bool {
    let sql = statement.to_uppercase();
    sql.starts_with("CREATE INDEX CONCURRENTLY")
        || sql.starts_with("CREATE UNIQUE INDEX CONCURRENTLY")
        || sql.starts_with("DROP INDEX CONCURRENTLY")
        || sql.starts_with("VACUUM")
        || sql.starts_with("REINDEX")
        || sql.starts_with("ALTER SYSTEM")
        || sql.starts_with("CREATE DATABASE")
}
