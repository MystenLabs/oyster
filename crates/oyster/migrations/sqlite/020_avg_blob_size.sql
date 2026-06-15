-- Add per-account `avg_blob_size` knob (unencoded bytes). Drives the
-- storage-cap inflation that turns `max_unencoded_bytes` into a *lower*
-- bound on storable unencoded capacity for blobs averaging this size.
-- Backfills to `0` for existing rows: 0 is the no-inflation sentinel,
-- so existing accounts keep today's exact upper-bound behavior. The
-- 10 MB global default (`OYSTER_DEFAULT_AVG_BLOB_SIZE`) applies only to
-- *new* accounts via `create_account`, not via this column DEFAULT.
ALTER TABLE accounts ADD COLUMN avg_blob_size BIGINT NOT NULL DEFAULT 0;
