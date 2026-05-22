-- Add per-account `max_unencoded_bytes` storage cap, backfilled to
-- 5_000_000_000 (5 × 10⁹ bytes, decimal GB) for existing rows via
-- `NOT NULL DEFAULT`. SQLite's `INTEGER` storage class is dynamic up to
-- 64 bits, so no widening of the `pool_*` columns is required here —
-- the matching Postgres migration widens those to BIGINT.
ALTER TABLE accounts ADD COLUMN max_unencoded_bytes BIGINT NOT NULL DEFAULT 5000000000;
