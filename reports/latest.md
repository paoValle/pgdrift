# pgdrift — what the next migration would do

Measured against a live PostgreSQL, not estimated. Migration: `examples/0001-risky.sql`.

## The verdict, per statement

| statement | kind | lock level | rewrite | advice |
|---|---|---|---|---|
| `ALTER TABLE orders ADD COLUMN note text` | alter table add column | **ACCESS EXCLUSIVE** | no | brief ACCESS EXCLUSIVE: it queues behind open transactions, so a long-running query makes it long |
| `CREATE INDEX orders_created_at_idx ON orders (created_at)` | create index | **SHARE** | no | blocks writes for the whole build: CREATE INDEX CONCURRENTLY instead, outside a transaction |
| `ALTER TABLE order_items ADD CONSTRAINT order_items_order_fk F...` | alter table add foreign key | **SHARE ROW EXCLUSIVE** | no | blocks writes, not reads: SHARE ROW EXCLUSIVE on both tables. ADD CONSTRAINT ... NOT VALID first, then VALIDATE CONSTRAINT in a separate step, so the scan is not part of the same lock |
| `ALTER TABLE orders ALTER COLUMN note TYPE varchar(400)` | alter table change column type | **ACCESS EXCLUSIVE** | yes | ACCESS EXCLUSIVE *and* a full rewrite: add a new column, backfill in batches, swap, then drop old |

## What the database actually does

Each statement is tried against a live PostgreSQL while another session holds a read lock, and while another holds a write lock, with `lock_timeout` set to 300 ms. What gets blocked is the lock level, measured.

| statement | predicted | measured | blocked by a reader | blocked by a writer | waits when unopposed |
|---|---|---|---|---|---|
| `ALTER TABLE orders ADD COLUMN note text` | ACCESS EXCLUSIVE | ACCESS EXCLUSIVE | **yes** | **yes** | 1 ms |
| `CREATE INDEX orders_created_at_idx ON orders (created_at)` | SHARE | SHARE | no | **yes** | 6 ms |
| `ALTER TABLE order_items ADD CONSTRAINT order_items_order_fk F...` | SHARE ROW EXCLUSIVE | SHARE ROW EXCLUSIVE | no | **yes** | 4 ms |
| `ALTER TABLE orders ALTER COLUMN note TYPE varchar(400)` | ACCESS EXCLUSIVE | ACCESS EXCLUSIVE | **yes** | **yes** | 27 ms |

0 statement(s) where the measurement and the table disagree.

## The same intent, written so it does not stop traffic

| statement | lock level | advice |
|---|---|---|
| `CREATE INDEX CONCURRENTLY orders_created_at_idx ON orders (cr...` | SHARE UPDATE EXCLUSIVE | online: it does not block reads or writes, but it takes longer and can leave an invalid index if it fails |
| `ALTER TABLE order_items ADD CONSTRAINT order_items_order_fk F...` | SHARE ROW EXCLUSIVE | blocks writes, not reads: SHARE ROW EXCLUSIVE on both tables. ADD CONSTRAINT ... NOT VALID first, then VALIDATE CONSTRAINT in a separate step, so the scan is not part of the same lock |
| `ALTER TABLE order_items VALIDATE CONSTRAINT order_items_order_fk` | SHARE UPDATE EXCLUSIVE | safe: it does not block reads or writes, it only takes time |
| `ALTER TABLE orders ADD COLUMN note_v2 varchar(400)` | ACCESS EXCLUSIVE | brief ACCESS EXCLUSIVE: it queues behind open transactions, so a long-running query makes it long |

## What the schema already hides

| check | subject | why it matters |
|---|---|---|
| unindexed foreign key | `order_items.order_id (order_items_order_id_fkey)` | deleting or updating the referenced row scans the whole referencing table, under a lock |
| table without a primary key | `events` | no row can be identified: logical replication and anything that updates a single row are out |
| invalid index | `orders_created_at_idx on orders` | what a failed CREATE INDEX CONCURRENTLY leaves: never used by the planner, still written to on every insert |
| int4 primary key | `sessions.id` | a ceiling of 2.1 billion rows, and widening it later is an ACCESS EXCLUSIVE rewrite |
