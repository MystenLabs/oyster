CREATE TABLE accounts (
    id TEXT PRIMARY KEY NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE api_keys (
    id TEXT PRIMARY KEY NOT NULL,
    account_id TEXT NOT NULL REFERENCES accounts(id),
    key_hash TEXT NOT NULL,
    prefix TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    revoked_at TEXT
);
CREATE INDEX idx_api_keys_key_hash ON api_keys(key_hash);
CREATE INDEX idx_api_keys_account_id ON api_keys(account_id);

CREATE TABLE buckets (
    id TEXT PRIMARY KEY NOT NULL,
    account_id TEXT NOT NULL REFERENCES accounts(id),
    name TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(account_id, name)
);
CREATE INDEX idx_buckets_account_id ON buckets(account_id);

CREATE TABLE blobs (
    object_id TEXT PRIMARY KEY NOT NULL,
    blob_id TEXT NOT NULL,
    bucket_id TEXT NOT NULL REFERENCES buckets(id),
    account_id TEXT NOT NULL REFERENCES accounts(id),
    content_type TEXT NOT NULL DEFAULT 'application/octet-stream',
    size INTEGER NOT NULL,
    auto_extend_duration TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    expires_at TEXT
);
CREATE INDEX idx_blobs_bucket_id ON blobs(bucket_id);
CREATE INDEX idx_blobs_blob_id ON blobs(blob_id);
CREATE INDEX idx_blobs_account_id ON blobs(account_id);
