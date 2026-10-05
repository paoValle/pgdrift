-- The same intent, written so that it does not stop traffic. Every statement here is either
-- SHARE UPDATE EXCLUSIVE or a no-op for other sessions.

-- the index is built online: it does not block reads or writes
CREATE INDEX CONCURRENTLY orders_created_at_idx ON orders (created_at);

-- the constraint is added without validating it, then validated: validation is SHARE UPDATE
-- EXCLUSIVE, so writes keep flowing while it scans
ALTER TABLE order_items ADD CONSTRAINT order_items_order_fk
    FOREIGN KEY (order_id) REFERENCES orders (id) NOT VALID;
ALTER TABLE order_items VALIDATE CONSTRAINT order_items_order_fk;

-- the new column and the backfill happen outside the lock window
ALTER TABLE orders ADD COLUMN note_v2 varchar(400);
