# Changelog

Format [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
versioning [SemVer](https://semver.org/).

## [Unreleased]

## [0.1.1] - 2026-10-07

### Changed
- `prove` adds a column to the scratch table when the statement it measures names one that nobody
  creates: `ALTER COLUMN x`, `FOREIGN KEY (x)` and an index column. The type comes from the column a
  foreign key references, or from the scratch shape's own name, and the added column carries a default
  so a `SET NOT NULL` behind it is measurable. Statements that only name columns the schema already
  has are measured exactly as before; the report lists what was added, and a column no statement names
  is still never invented.

## [0.1.0] - 2026-10-05

First version: a verdict per statement, and then the measurement that can contradict it.

### Added
- `plan`: the lock level each statement is expected to take, with the reason and the safer
  alternative. A statement that is not in the table is reported as unknown, never guessed.
- `prove`: the measurement against a live PostgreSQL — a scratch schema, the migration's earlier
  statements replayed into it, a reader and a writer holding locks with `lock_timeout` set, and the
  lock the statement actually asked for read out of `pg_locks`.
- `audit`: unindexed foreign keys, tables without a primary key, invalid indexes (what a failed
  `CREATE INDEX CONCURRENTLY` leaves behind) and `int4` primary keys.
- `report`: the artifact in `reports/latest.md`, generated from `examples/` by `make report`.
- A lexer for splitting migration files, because strings, comments and dollar-quoted function bodies
  all contain semicolons and `split(';')` is where a tool starts lying about a migration.

### Fixed
- The table said `ALTER TABLE ... ADD FOREIGN KEY` takes `ACCESS EXCLUSIVE`. Measured with `pg_locks`
  on PostgreSQL 16 it is `SHARE ROW EXCLUSIVE`, on both tables: the probe contradicted the table, and
  the table was wrong. `CREATE INDEX CONCURRENTLY` was described as blocking writers when its lock
  conflicts with nothing — it waits for open transactions to finish, which is a different fact.
- The argument parser accepted unknown flags and ignored them (`--fail-on-share`, a typo, looked
  exactly like a gate that passed), and the usage text advertised `--json` where there was none.
