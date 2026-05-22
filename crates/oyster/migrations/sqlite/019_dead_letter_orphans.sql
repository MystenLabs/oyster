-- Dead-letter table for on-chain `PooledBlob` orphans created when a
-- post-store DB write (in `store_blob` / S3 `put_object`) fails AND the
-- compensating on-chain delete also fails. A future reaper picks these
-- up and retries the delete. See `routes/blobs/compensation.rs`.
CREATE TABLE dead_letter_orphans (
    blob_id TEXT NOT NULL,
    account_id TEXT NOT NULL,
    pool_id TEXT,
    encoded_size BIGINT NOT NULL,
    original_db_error TEXT NOT NULL,
    compensation_error TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (blob_id, account_id)
);
