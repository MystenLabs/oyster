-- Add per-account `max_unencoded_bytes` storage cap, backfilled to
-- 5_000_000_000 (5 × 10⁹ bytes, decimal GB) for existing rows via
-- `NOT NULL DEFAULT`.
ALTER TABLE accounts ADD COLUMN max_unencoded_bytes BIGINT NOT NULL DEFAULT 5000000000;

-- Widen the existing pool accounting columns from INTEGER (4-byte
-- signed, max ~2.1×10⁹) to BIGINT. Migration 010 typed these as
-- INTEGER, but the Rust code binds/reads them as `i64`, and the new
-- 5 GB default cap implies encoded reservations well past 2 GB. Without
-- this widening, Postgres deployments would overflow once usage grows.
-- SQLite's INTEGER is dynamic up to i64, so the matching SQLite
-- migration is a no-op on these columns.
ALTER TABLE accounts ALTER COLUMN pool_end_epoch TYPE BIGINT;
ALTER TABLE accounts ALTER COLUMN pool_reserved_encoded_bytes TYPE BIGINT;
ALTER TABLE accounts ALTER COLUMN pool_used_encoded_bytes TYPE BIGINT;
