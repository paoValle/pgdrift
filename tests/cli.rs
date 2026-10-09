//! The verdict's exit codes, which are the contract the README documents and the CI gates on.
//!
//! The rest of the suite tests what the table and the lexer say. Nothing tested what the binary
//! *returns*, and `plan` is the command whose exit code is a gate: `0` nothing to report, `1`
//! something to look at, `2` a usage or connection error. These run the binary on the
//! repository's own examples, so no database is needed.

use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_pgdrift");

fn pgdrift(args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .output()
        .expect("run the pgdrift binary")
}

fn code(output: &Output) -> i32 {
    output
        .status
        .code()
        .expect("the binary must exit with a code, not a signal")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn the_verdict_exit_codes_are_the_ones_the_readme_documents() {
    let risky = pgdrift(&[
        "plan",
        "--migration",
        "examples/0001-risky.sql",
        "--fail-on",
        "share",
    ]);
    assert_eq!(
        code(&risky),
        1,
        "the risky migration is what the gate exists for: {}",
        stdout(&risky)
    );
    assert!(
        stdout(&risky).contains("ACCESS EXCLUSIVE"),
        "the verdict must name the lock it blocks on: {}",
        stdout(&risky)
    );

    let safe = pgdrift(&[
        "plan",
        "--migration",
        "examples/0002-safe.sql",
        "--fail-on",
        "unknown",
    ]);
    assert_eq!(
        code(&safe),
        0,
        "nothing to report is exit 0: {}",
        stdout(&safe)
    );

    // The safe migration is not free: adding a nullable column takes ACCESS EXCLUSIVE briefly, so
    // it fails `--fail-on share` too — which is why CI asks `--fail-on unknown` for it, and why a
    // threshold cannot tell one millisecond from forty minutes: that is what `prove` measures.
    let safe_against_share = pgdrift(&[
        "plan",
        "--migration",
        "examples/0002-safe.sql",
        "--fail-on",
        "share",
    ]);
    assert_eq!(
        code(&safe_against_share),
        1,
        "the safe migration is brief, not free: {}",
        stdout(&safe_against_share)
    );
}

#[test]
fn a_statement_outside_the_table_is_unknown_and_fails_the_unknown_gate() {
    // The README's promise: "A statement that is not in the table is reported as unknown: it never
    // guesses a lock level". Written out of tree on purpose — it is an example of an unknown
    // statement, not a migration this repository ships.
    let dir = std::env::temp_dir().join(format!("pgdrift-cli-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create the temp directory");
    let migration = dir.join("unknown.sql");
    std::fs::write(&migration, "ALTER SYSTEM SET work_mem = '64MB';\n")
        .expect("write the migration");

    let out = pgdrift(&[
        "plan",
        "--migration",
        migration.to_str().expect("the temp path is utf-8"),
        "--fail-on",
        "unknown",
    ]);
    assert_eq!(
        code(&out),
        1,
        "an unknown statement is reported: {}",
        stdout(&out)
    );
    assert!(
        stdout(&out).contains("unknown"),
        "and it is named as unknown rather than guessed: {}",
        stdout(&out)
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_usage_error_is_exit_2() {
    let cases: [&[&str]; 5] = [
        &["plan", "--fail-on", "share"],
        &[
            "plan",
            "--migration",
            "examples/0001-risky.sql",
            "--fail-on",
            "bogus",
        ],
        &[
            "plan",
            "--migration",
            "examples/0001-risky.sql",
            "--frobnicate",
        ],
        &["frobnicate"],
        &[],
    ];
    for args in cases {
        assert_eq!(
            code(&pgdrift(args)),
            2,
            "a usage error is exit 2, not a verdict: {args:?}"
        );
    }
}
