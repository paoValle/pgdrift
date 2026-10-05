//! The command line: four commands, and exit codes a CI can gate on.
//!
//! ```console
//! pgdrift plan  --migration migrations/0001.sql [--fail-on access-exclusive]
//! pgdrift prove --db-url postgres://... --migration migrations/0001.sql
//! pgdrift audit --db-url postgres://...
//! pgdrift report --db-url postgres://... --migration x.sql --safe y.sql --seed schema.sql --out reports/latest.md
//! ```
//!
//! Exit codes: `0` nothing to report, `1` something to look at (a lock level above the threshold, a
//! measurement that contradicts the table, or an audit finding), `2` usage or connection error.
//! A tool that returns `1` for a broken invocation is a tool people learn to ignore.

// The report builder is a wall of `format!` lines pushed into one string: `write!` would not be
// more correct (writing into a `String` cannot fail) and would bury the report's shape.
#![allow(clippy::format_push_string)]

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use pgdrift::cli::Args;
use pgdrift::{audit, classify, probe_statement, split, LockLevel, Probe};

const USAGE: &str = "\
pgdrift — would this migration block production?

  plan   --migration <file|dir> [--fail-on share|access-exclusive|unknown]
  prove  --db-url <url> --migration <file|dir>
  audit  --db-url <url>
  report --db-url <url> --migration <file> [--safe <file>] [--seed <schema.sql>] [--out reports/latest.md]

Exit codes: 0 nothing to report, 1 something to look at, 2 usage or connection error.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(code) => code,
        Err(message) => {
            eprintln!("pgdrift: {message}");
            ExitCode::from(2)
        }
    }
}

fn run(raw: &[String]) -> Result<ExitCode, String> {
    let args = Args::parse(raw)?;
    args.validate()?;
    if args.command.is_empty() || args.command == "help" {
        println!("{USAGE}");
        return Ok(ExitCode::from(if args.command.is_empty() { 2 } else { 0 }));
    }
    match args.command.as_str() {
        "plan" => plan(&args),
        "prove" => prove(&args),
        "audit" => audit_command(&args),
        "report" => report(&args),
        other => Err(format!("unknown command {other:?}\n\n{USAGE}")),
    }
}

/// The static verdict, per statement.
fn plan(args: &Args) -> Result<ExitCode, String> {
    let threshold = match args.one("fail-on").unwrap_or("access-exclusive") {
        "none" => LockLevel::Unknown.severity() + 1,
        "share" => LockLevel::Share.severity(),
        "access-exclusive" => LockLevel::AccessExclusive.severity(),
        "unknown" => LockLevel::Unknown.severity(),
        other => {
            return Err(format!(
                "--fail-on {other:?} is not one of none, share, access-exclusive, unknown"
            ))
        }
    };

    let files = sql_files(args.required("migration")?)?;
    let mut worst = LockLevel::None;
    let mut lines: Vec<String> = Vec::new();
    for file in &files {
        let statements = read_statements(file)?;
        for statement in &statements {
            let rule = classify(&statement.sql);
            if rule.lock.severity() > worst.severity() {
                worst = rule.lock;
            }
            lines.push(format!(
                "{}#{}  {:<42} {:<24} {}{}",
                file.file_name().map_or_else(
                    || "?".to_owned(),
                    |name| name.to_string_lossy().into_owned()
                ),
                statement.index,
                rule.kind,
                rule.lock.label(),
                match rule.rewrite {
                    Some(true) => "rewrites the table · ",
                    _ => "",
                },
                rule.advice
            ));
        }
    }

    for line in &lines {
        println!("{line}");
    }
    Ok(
        if worst.severity() >= threshold && threshold <= LockLevel::Unknown.severity() {
            ExitCode::from(1)
        } else {
            ExitCode::SUCCESS
        },
    )
}

/// The measurement, per statement.
fn prove(args: &Args) -> Result<ExitCode, String> {
    let url = args.required("db-url")?;
    let files = sql_files(args.required("migration")?)?;
    let mut failing = false;

    for file in &files {
        let statements = read_statements(file)?;
        let all: Vec<String> = statements
            .iter()
            .map(|statement| statement.sql.clone())
            .collect();
        for (index, statement) in statements.iter().enumerate() {
            let probe = probe_statement(url, &all[..index], &statement.sql)?;
            print_probe(file, &probe);
            if probe.disagrees() {
                failing = true;
            }
            if probe.blocked_by_reader == Some(true)
                && probe.predicted != LockLevel::AccessExclusive
            {
                failing = true;
            }
        }
    }
    Ok(if failing {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

fn print_probe(file: &Path, probe: &Probe) {
    let name = file
        .file_name()
        .map_or_else(|| "?".to_owned(), |n| n.to_string_lossy().into_owned());
    println!("{name}  {}", probe.statement);
    println!("  predicted: {}", probe.predicted.label());
    match (&probe.measured, &probe.skipped) {
        (Some(measured), _) => {
            match probe.catalog_level {
                Some(level) => println!("  measured:  {} (pg_locks: {})", measured.label(), level.label()),
                None => println!("  measured:  {} (from the holder probes: the statement cannot run inside a transaction, so there is no pg_locks reading)", measured.label()),
            }
            if probe.waits_without_conflicting() {
                println!("  note:      it does not conflict with those locks: it waits for the open transaction to finish, which is how a long query turns an online operation into a delay");
            }
            if probe.disagrees() {
                println!(
                    "  DISAGREES: the table predicts reads {} / writes {}, the database measured reads {} / writes {}",
                    blocked(probe.predicted.blocks_readers()),
                    blocked(probe.predicted.blocks_writers()),
                    blocked(probe.blocked_by_reader == Some(true)),
                    blocked(probe.blocked_by_writer == Some(true))
                );
            }
        }
        (None, Some(reason)) => println!("  skipped:   {reason}"),
        (None, None) => println!("  measured:  (nothing to measure)"),
    }
    for line in &probe.evidence {
        println!("  · {line}");
    }
}

/// What the schema already hides.
fn audit_command(args: &Args) -> Result<ExitCode, String> {
    let url = args.required("db-url")?;
    let findings = audit(url)?;
    for finding in &findings {
        println!(
            "{:<28} {:<48} {}",
            finding.check, finding.subject, finding.why
        );
    }
    println!("{} finding(s)", findings.len());
    Ok(if findings.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

/// Writes the artifact: the plan, the measurement, and the audit of a seeded schema.
fn report(args: &Args) -> Result<ExitCode, String> {
    let url = args.required("db-url")?;
    let migration = PathBuf::from(args.required("migration")?);

    if let Some(seed) = args.one("seed") {
        seed_schema(url, seed)?;
    }

    let mut markdown = String::from("# pgdrift — what the next migration would do\n\n");
    markdown.push_str(&format!(
        "Measured against a live PostgreSQL, not estimated. Migration: `{}`.\n\n",
        migration.display()
    ));

    // 1. the static table
    markdown.push_str("## The verdict, per statement\n\n| statement | kind | lock level | rewrite | advice |\n|---|---|---|---|---|\n");
    let statements = read_statements(&migration)?;
    for statement in &statements {
        let rule = classify(&statement.sql);
        markdown.push_str(&format!(
            "| `{}` | {} | **{}** | {} | {} |\n",
            shorten(&statement.sql),
            rule.kind,
            rule.lock.label(),
            match rule.rewrite {
                Some(true) => "yes",
                Some(false) => "no",
                None => "unknown",
            },
            rule.advice
        ));
    }

    // 2. the measurement
    markdown.push_str("\n## What the database actually does\n\n");
    markdown.push_str("Each statement is tried against a live PostgreSQL while another session holds a read lock, and while another holds a write lock, with `lock_timeout` set to 300 ms. What gets blocked is the lock level, measured.\n\n");
    markdown.push_str("| statement | predicted | measured | blocked by a reader | blocked by a writer | waits when unopposed |\n|---|---|---|---|---|---|\n");
    let mut mismatches = 0;
    let all: Vec<String> = statements
        .iter()
        .map(|statement| statement.sql.clone())
        .collect();
    for (index, statement) in statements.iter().enumerate() {
        let probe = probe_statement(url, &all[..index], &statement.sql)?;
        if probe.disagrees() {
            mismatches += 1;
        }
        markdown.push_str(&format!(
            "| `{}` | {} | {} | {} | {} | {} ms |\n",
            shorten(&statement.sql),
            probe.predicted.label(),
            // the catalog reading is the exact level; the two holder probes only add up to a bucket
            probe.catalog_level.or(probe.measured).map_or_else(
                || probe
                    .skipped
                    .as_deref()
                    .unwrap_or("(not measured)")
                    .to_owned(),
                |level| level.label().to_owned(),
            ),
            yes_no(probe.blocked_by_reader),
            yes_no(probe.blocked_by_writer),
            probe
                .acquired_ms
                .map_or_else(|| "-".to_owned(), |ms| ms.to_string())
        ));
    }
    markdown.push_str(&format!(
        "\n{mismatches} statement(s) where the measurement and the table disagree.\n"
    ));

    // 3. the same intent, done safely
    if let Some(safe) = args.one("safe") {
        markdown.push_str("\n## The same intent, written so it does not stop traffic\n\n| statement | lock level | advice |\n|---|---|---|\n");
        for statement in &read_statements(Path::new(safe))? {
            let rule = classify(&statement.sql);
            markdown.push_str(&format!(
                "| `{}` | {} | {} |\n",
                shorten(&statement.sql),
                rule.lock.label(),
                rule.advice
            ));
        }
    }

    // 4. what the schema already hides
    markdown.push_str("\n## What the schema already hides\n\n");
    let findings = audit(url)?;
    if findings.is_empty() {
        markdown.push_str("Nothing: no unindexed foreign key, no table without a primary key, no invalid index, no `int4` primary key.\n");
    } else {
        markdown.push_str("| check | subject | why it matters |\n|---|---|---|\n");
        for finding in &findings {
            markdown.push_str(&format!(
                "| {} | `{}` | {} |\n",
                finding.check, finding.subject, finding.why
            ));
        }
    }

    let out = args.one("out").unwrap_or("reports/latest.md");
    if let Some(parent) = Path::new(out).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
    }
    std::fs::write(out, &markdown).map_err(|error| format!("{out}: {error}"))?;
    println!("wrote {out}");
    print!("{markdown}");
    Ok(ExitCode::SUCCESS)
}

fn blocked(value: bool) -> &'static str {
    if value {
        "blocked"
    } else {
        "not blocked"
    }
}

fn yes_no(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "**yes**",
        Some(false) => "no",
        None => "-",
    }
}

fn shorten(sql: &str) -> String {
    let one_line = sql.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= 64 {
        return one_line;
    }
    let cut: String = one_line.chars().take(61).collect();
    format!("{cut}...")
}

/// Applies a seed schema, from a fresh schema, so the report is reproducible from one command.
fn seed_schema(url: &str, seed: &str) -> Result<(), String> {
    let mut client = postgres::Client::connect(url, postgres::NoTls)
        .map_err(|error| format!("cannot connect to PostgreSQL: {error}"))?;
    client
        .batch_execute("DROP SCHEMA IF EXISTS public CASCADE; CREATE SCHEMA public;")
        .map_err(|error| format!("cannot reset the public schema: {error}"))?;
    let script = std::fs::read_to_string(seed).map_err(|error| format!("{seed}: {error}"))?;
    client
        .batch_execute(&script)
        .map_err(|error| format!("{seed}: {error}"))
}

fn sql_files(path: &str) -> Result<Vec<PathBuf>, String> {
    let path = PathBuf::from(path);
    if path.is_file() {
        return Ok(vec![path]);
    }
    if !path.is_dir() {
        return Err(format!(
            "{} is neither a file nor a directory",
            path.display()
        ));
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(&path)
        .map_err(|error| format!("{}: {error}", path.display()))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|entry| {
            entry
                .extension()
                .is_some_and(|extension| extension == "sql")
        })
        .collect();
    files.sort();
    if files.is_empty() {
        return Err(format!("no .sql file in {}", path.display()));
    }
    Ok(files)
}

fn read_statements(file: &Path) -> Result<Vec<pgdrift::Statement>, String> {
    let text =
        std::fs::read_to_string(file).map_err(|error| format!("{}: {error}", file.display()))?;
    split(&text).map_err(|error| format!("{}: {}", file.display(), error.detail))
}
