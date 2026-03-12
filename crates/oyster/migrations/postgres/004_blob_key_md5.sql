DROP TABLE IF EXISTS blobs;

CREATE TABLE blobs (
    key TEXT NOT NULL,
    blob_id TEXT NOT NULL,
    bucket_name TEXT NOT NULL REFERENCES buckets(name),
    account_id TEXT NOT NULL REFERENCES accounts(id),
    content_type TEXT NOT NULL DEFAULT 'application/octet-stream',
    size BIGINT NOT NULL,
    md5 TEXT NOT NULL,
    sui_object_id TEXT,
    created_at TEXT NOT NULL DEFAULT to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI:SS'),
    expires_at TEXT,
    PRIMARY KEY (bucket_name, key)
);
CREATE INDEX idx_blobs_bucket_name ON blobs(bucket_name);
CREATE INDEX idx_blobs_blob_id ON blobs(blob_id);
CREATE INDEX idx_blobs_account_id ON blobs(account_id);
CREATE INDEX idx_blobs_expires_at ON blobs(expires_at);
