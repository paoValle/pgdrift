-- A schema with four things pgdrift's audit reports, so the report has something real to show.
-- orders has a primary key, order_items references it without an index on the referencing side,
-- events has no primary key at all, sessions has an int4 primary key (a 2.1-billion ceiling), and
-- orders_created_at_idx is left invalid, which is what a failed CREATE INDEX CONCURRENTLY leaves
-- behind.
CREATE TABLE orders (
    id bigserial PRIMARY KEY,
    created_at timestamptz NOT NULL DEFAULT now(),
    note text
);

CREATE TABLE order_items (
    id bigserial PRIMARY KEY,
    order_id bigint NOT NULL REFERENCES orders (id),
    sku text NOT NULL
);

CREATE TABLE events (
    payload text,
    received_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE sessions (
    id serial PRIMARY KEY,
    token text NOT NULL
);

CREATE INDEX orders_created_at_idx ON orders (created_at);
UPDATE pg_index SET indisvalid = false
 WHERE indexrelid = 'orders_created_at_idx'::regclass;
