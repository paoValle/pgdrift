//! The lexer and the table, tested without a database.
//!
//! Everything here runs in milliseconds and needs no PostgreSQL, which matters: the parser and the
//! rules are what the tool says about a migration *before* anything is measured, and a wrong
//! classification is worse than no classification — it is a confident wrong answer.

use pgdrift::rules::LockLevel;
use pgdrift::{classify, split, sql};

#[test]
fn a_semicolon_inside_a_string_is_not_a_statement_boundary() {
    let statements = split("INSERT INTO t VALUES ('a;b'); SELECT 1;").expect("parses");
    assert_eq!(statements.len(), 2, "{statements:?}");
    assert_eq!(statements[0].sql, "INSERT INTO t VALUES ('a;b')");
    assert_eq!(statements[1].sql, "SELECT 1");
}

#[test]
fn a_semicolon_inside_a_comment_or_a_dollar_quote_is_not_a_statement_boundary() {
    let sql = "-- a comment; with a semicolon\nCREATE FUNCTION f() RETURNS int AS $$ BEGIN RETURN 1; END; $$ LANGUAGE plpgsql;\n/* another ; comment */ SELECT 2;";
    let statements = split(sql).expect("parses");
    assert_eq!(statements.len(), 2, "{statements:?}");
    assert!(statements[0].sql.contains("END;"), "{}", statements[0].sql);
    assert_eq!(statements[1].sql, "SELECT 2");
}

#[test]
fn an_unterminated_dollar_quote_is_an_error_not_a_guess() {
    let error = split("CREATE FUNCTION f() AS $$ SELECT 1;").expect_err("unterminated");
    assert!(error.detail.contains("never closed"), "{error:?}");
}

#[test]
fn escaped_quotes_do_not_end_a_string() {
    let statements = split("SELECT 'it''s; fine'; SELECT 2;").expect("parses");
    assert_eq!(statements.len(), 2, "{statements:?}");
}

#[test]
fn the_target_table_is_found_in_the_common_shapes() {
    for (statement, expected) in [
        ("ALTER TABLE orders ADD COLUMN note text", Some("orders")),
        (
            "ALTER TABLE ONLY public.orders DROP COLUMN note",
            Some("public.orders"),
        ),
        (
            "CREATE INDEX CONCURRENTLY IF NOT EXISTS idx ON orders (created_at)",
            Some("orders"),
        ),
        (
            "CREATE UNIQUE INDEX idx ON order_items (order_id)",
            Some("order_items"),
        ),
        ("LOCK TABLE orders IN ACCESS EXCLUSIVE MODE", Some("orders")),
        ("TRUNCATE orders", Some("orders")),
        ("SELECT 1", None),
    ] {
        assert_eq!(
            sql::target_table(statement).as_deref(),
            expected,
            "{statement}"
        );
    }
}

#[test]
fn the_referenced_table_is_found_for_foreign_keys() {
    assert_eq!(
        sql::referenced_table(
            "ALTER TABLE a ADD CONSTRAINT fk FOREIGN KEY (x) REFERENCES orders (id)"
        )
        .as_deref(),
        Some("orders")
    );
    assert_eq!(
        sql::referenced_table("ALTER TABLE a ADD COLUMN b int"),
        None
    );
}

#[test]
fn the_table_classifies_the_lock_each_statement_takes() {
    for (statement, expected, rewrite) in [
        (
            "CREATE INDEX CONCURRENTLY i ON t (a)",
            LockLevel::ShareUpdateExclusive,
            false,
        ),
        ("CREATE INDEX i ON t (a)", LockLevel::Share, false),
        ("CREATE UNIQUE INDEX i ON t (a)", LockLevel::Share, false),
        (
            "ALTER TABLE t ADD COLUMN c int",
            LockLevel::AccessExclusive,
            false,
        ),
        (
            "ALTER TABLE t ADD COLUMN c int NOT NULL DEFAULT 0",
            LockLevel::AccessExclusive,
            false,
        ),
        (
            "ALTER TABLE t DROP COLUMN c",
            LockLevel::AccessExclusive,
            false,
        ),
        (
            "ALTER TABLE t ALTER COLUMN c TYPE text",
            LockLevel::AccessExclusive,
            true,
        ),
        (
            "ALTER TABLE t ALTER COLUMN c SET NOT NULL",
            LockLevel::AccessExclusive,
            true,
        ),
        // measured with pg_locks on PostgreSQL 16: SHARE ROW EXCLUSIVE, not ACCESS EXCLUSIVE.
        // This test said ACCESS EXCLUSIVE until `pgdrift prove` contradicted it.
        (
            "ALTER TABLE t ADD CONSTRAINT fk FOREIGN KEY (a) REFERENCES o (id)",
            LockLevel::ShareRowExclusive,
            false,
        ),
        (
            "ALTER TABLE t VALIDATE CONSTRAINT fk",
            LockLevel::ShareUpdateExclusive,
            false,
        ),
        (
            "ALTER TABLE t ADD CONSTRAINT uq UNIQUE (a)",
            LockLevel::AccessExclusive,
            false,
        ),
        ("VACUUM t", LockLevel::ShareUpdateExclusive, false),
        (
            "CREATE TRIGGER tr AFTER INSERT ON t FOR EACH ROW EXECUTE FUNCTION f()",
            LockLevel::ShareRowExclusive,
            false,
        ),
        ("DROP TABLE t", LockLevel::AccessExclusive, false),
        ("TRUNCATE t", LockLevel::AccessExclusive, false),
        ("CREATE TABLE t (id int)", LockLevel::None, false),
        ("COMMENT ON TABLE t IS 'x'", LockLevel::None, false),
    ] {
        let rule = classify(statement);
        assert_eq!(rule.lock, expected, "{statement} → {:?}", rule.lock);
        assert_eq!(rule.rewrite, Some(rewrite), "{statement} rewrite");
    }
}

#[test]
fn a_statement_that_is_not_in_the_table_is_reported_as_unknown() {
    // the whole point: pgdrift does not guess a lock level it has not been taught
    let rule = classify("ALTER TABLE t SET (fillfactor = 70)");
    assert_eq!(rule.lock, LockLevel::Unknown);
    assert!(rule.advice.contains("will not guess"), "{}", rule.advice);

    let rule = classify("CLUSTER t USING i");
    assert_eq!(rule.lock, LockLevel::Unknown);
}

#[test]
fn the_lock_levels_report_what_they_block() {
    // measured behaviour, in the table that predicts it: these two booleans are what the probe checks
    assert!(LockLevel::AccessExclusive.blocks_readers());
    assert!(LockLevel::AccessExclusive.blocks_writers());
    assert!(!LockLevel::Share.blocks_readers());
    assert!(LockLevel::Share.blocks_writers());
    assert!(!LockLevel::ShareUpdateExclusive.blocks_readers());
    assert!(!LockLevel::ShareUpdateExclusive.blocks_writers());
}
