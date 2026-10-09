-- A subscription plan.
CREATE TABLE plans (
    id        BIGSERIAL PRIMARY KEY,
    name      TEXT NOT NULL UNIQUE,
    -- Exact money: NUMERIC becomes rust_decimal's Decimal (feature `decimal`).
    price     NUMERIC(10, 2) NOT NULL,
    -- How often it bills: INTERVAL becomes PgInterval.
    period    INTERVAL NOT NULL DEFAULT '1 month',
    trial     INTERVAL
);

-- name: create_plan :one!
INSERT INTO plans (name, price, period) VALUES ($1, $2, $3) RETURNING *;

-- name: cheaper_than :many
SELECT * FROM plans WHERE price < $1 ORDER BY price, id;
