-- A migration that a reviewer would want to stop, and one they would not.
-- Both are one file, because the interesting question is per statement, not per file.

-- 0001: adding a nullable column. Brief ACCESS EXCLUSIVE, no rewrite. Unavoidable, worth knowing.
ALTER TABLE orders ADD COLUMN note text;

-- 0002: the classic outage. A plain CREATE INDEX takes SHARE and blocks every write for the whole
-- build. On a table being written to right now, that is a queue of writes behind one index.
CREATE INDEX orders_created_at_idx ON orders (created_at);

-- 0003: an added foreign key, validated immediately: ACCESS EXCLUSIVE on both tables for as long
-- as the validation takes.
ALTER TABLE order_items ADD CONSTRAINT order_items_order_fk
    FOREIGN KEY (order_id) REFERENCES orders (id);

-- 0004: a type change, which is ACCESS EXCLUSIVE *and* a full table rewrite.
ALTER TABLE orders ALTER COLUMN note TYPE varchar(400);
