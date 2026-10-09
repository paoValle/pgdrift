# pgdrift

> **Would this migration block production?** A static verdict, and then a measurement against a real
> PostgreSQL.

Every linter tells you what the manual says. This one also tells you what your database did:

```
$ pgdrift prove --db-url postgres://... --migration examples/0001-risky.sql
ALTER TABLE orders ADD COLUMN note text
  predicted: ACCESS EXCLUSIVE
  measured:  ACCESS EXCLUSIVE (pg_locks: ACCESS EXCLUSIVE)
  · with no other session: acquired in 1 ms
  · with a reader holding a lock: blocked after 305 ms (lock_timeout) — this blocks reads
  · with a writer holding a lock: blocked after 304 ms (lock_timeout) — this blocks writes
```

The lock level is not inferred from the SQL and not taken on faith from a table: it is read out of
`pg_locks` while the statement runs, and the effect is measured by holding a read lock with one
session and a write lock with another, with `lock_timeout` set. When the measurement disagrees with
the table, the report says so — and the table is what is wrong.

## What it found in itself

`pgdrift`'s own table said `ALTER TABLE ... ADD FOREIGN KEY` takes `ACCESS EXCLUSIVE`. PostgreSQL 16
disagrees:

```console
$ pgdrift prove --db-url ... --migration examples/0001-risky.sql | grep -A1 'FOREIGN KEY'
  predicted: SHARE ROW EXCLUSIVE
  measured:  SHARE ROW EXCLUSIVE (pg_locks: SHARE ROW EXCLUSIVE)
```

The first version of this tool was wrong about the one thing it exists to be right about, and the
measurement caught it. That is why the table is not the product: the probe is.

A second finding, from the same mechanism: `CREATE INDEX CONCURRENTLY` showed up as "blocked by a
writer" while its lock (`SHARE UPDATE EXCLUSIVE`) conflicts with nothing. It does not block writes —
it **waits for open transactions that touched the table to finish**, which with a `lock_timeout` set
is a failure and in production is a delay. The tool now says that explicitly instead of calling it a
write block.

## What it does

- **`plan`** — the verdict per statement, from a curated table with a reason and the safer
  alternative for each form. A statement that is not in the table is reported as **unknown**: it
  never guesses a lock level, because a confident wrong answer about `ACCESS EXCLUSIVE` is how people
  learn to ignore a tool.
- **`prove`** — the measurement, against a live database: a scratch schema, the migration's earlier
  statements replayed into it, a read-lock holder, a write-lock holder, `lock_timeout`, and the
  actual lock read from `pg_locks`.
- **`audit`** — what the schema already hides, from the catalog, with no configuration file:
  unindexed foreign keys, tables without a primary key, and invalid indexes (the wreckage of a failed
  `CREATE INDEX CONCURRENTLY`) are facts and always run. The fourth, an `int4` primary key, is a
  judgement about how big a table will get: it runs only when asked, with
  `--warn-int4-over <rows>`, and it names the planner's estimate — or says the table has none.
- **`report`** — the artifact: verdict, measurement, the same migration written safely, and the
  audit. The committed [`reports/latest.md`](reports/latest.md) is the output of exactly that run.

Exit codes: `0` nothing to report, `1` something to look at, `2` usage or connection error.

## Usage

```bash
make setup     # cargo fetch
make db-up     # a throwaway PostgreSQL 16 in Docker, on port 55432
make plan      # the verdict: the risky migration fails, the safe one passes
make prove     # the measurement against that database
make audit     # what the seeded schema already hides
make report    # writes reports/latest.md
make ci        # fmt + clippy -D warnings + 22 tests (no database needed)
make db-down
```

CI runs all of it against a PostgreSQL 16 service container, and asserts the gate: the risky
migration must be flagged, the safe one must not, and the audit must find all four planted problems
in `examples/schema.sql` once it is asked for the fourth with `--warn-int4-over`.

## How it works

```
migrations/*.sql ──► sql.rs (a lexer, not split(';')) ──► rules.rs (the table, with reasons)
                                                              │
                                                    probe.rs ──┴──► a live PostgreSQL
                                                    (holders + pg_locks + lock_timeout)
                                                              │
                                                  catalog.rs ──┴──► pg_catalog
```

The lexer is about ninety lines and has the most tests in the repository: splitting a migration file
on `;` is where a tool starts lying about what a migration does, because strings, comments and
dollar-quoted function bodies all contain semicolons.

## Scope, declared

Not here, and not claimed:

- **a scratch schema, not your schema.** `prove` creates and drops a schema named `pgdrift_probe` and
  measures there. The table shape is a generic one; when a statement names a column nobody creates —
  an `ALTER COLUMN x`, a `FOREIGN KEY (x)`, an index on `x` — that column is added to the probe table,
  typed from what a foreign key references or from the scratch shape's own name, with a default so the
  statement can run, and the report says which columns it added. A column no statement names is still
  never invented: measuring against a shape the statement does not expect is the failure this tool
  exists to avoid. **Do not point it at production**: it mutates a schema, even if it is its own.
- **PostgreSQL 16 is what was measured.** The table is a claim about documented behaviour, and the
  probe is the check on it; on another major version the probe is still right and the table may need
  the same correction this one got.
- **lock conflicts, not durations.** A 40-second `ACCESS EXCLUSIVE` on a small table is quick and on a
  100 GB table is an outage: `pgdrift` reports the lock, and the time depends on your data.
- **one database, no migration history.** It does not know what has already been applied, and it is
  not a migration runner.
- **no rules beyond the four audit checks**, and no configuration file to add more: a check that needs
  a config file is a check somebody has to maintain. The one that is a judgement is a flag, not a
  config file.

## What I would do differently

- The audit checks are the least interesting part and the easiest to grow into a checklist nobody
  reads. The `int4` threshold is now a flag instead of a finding with the same weight as an invalid
  index; what is still missing is an alert that fires *before* the key runs out, which needs history
  this tool does not keep.
- The probe runs three attempts per statement, which means the setup cost is paid three times. With a
  hundred-statement migration that is minutes; a smarter version would reuse the schema for the two
  holder probes and only rebuild when a statement cannot be rolled back.
- `prove` cannot audit a statement's *plan* — `SET NOT NULL` scans the table, `ALTER COLUMN TYPE`
  rewrites it — and "the scan takes 40 minutes on your data" is the question that decides whether a
  migration is safe. That needs the real table, not a scratch one, and it is the honest next step.

## Development

```bash
git clone git@github.com:paoValle/pgdrift.git
cd pgdrift
make db-up && make plan && make prove && make ci && make db-down
```

## License

MIT © Paolo Valletta
