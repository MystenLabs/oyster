ALTER TABLE blobs ADD COLUMN sui_object_id TEXT;
CREATE INDEX idx_blobs_expires_at ON blobs(expires_at);
