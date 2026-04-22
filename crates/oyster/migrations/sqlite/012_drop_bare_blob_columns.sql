DROP INDEX IF EXISTS idx_blobs_expires_at;
DROP TABLE IF EXISTS blobs;

CREATE TABLE blobs (
    key TEXT NOT NULL,
    blob_id TEXT NOT NULL,
    bucket_name TEXT NOT NULL REFERENCES buckets(name),
    account_id TEXT NOT NULL REFERENCES accounts(id),
    content_type TEXT NOT NULL DEFAULT 'application/octet-stream',
    size INTEGER NOT NULL,
    md5 TEXT NOT NULL,
    pooled_blob_object_id TEXT,
    encoded_size INTEGER,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (bucket_name, key)
);
CREATE INDEX idx_blobs_bucket_name ON blobs(bucket_name);
CREATE INDEX idx_blobs_blob_id ON blobs(blob_id);
CREATE INDEX idx_blobs_account_id ON blobs(account_id);
