DROP INDEX IF EXISTS idx_blobs_expires_at;
ALTER TABLE blobs DROP COLUMN IF EXISTS sui_object_id;
ALTER TABLE blobs DROP COLUMN IF EXISTS expires_at;
