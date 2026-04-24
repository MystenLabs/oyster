CREATE TABLE blob_tags (
    account_id  TEXT NOT NULL,
    bucket_name TEXT NOT NULL,
    key         TEXT NOT NULL,
    tag_key     TEXT NOT NULL,
    tag_value   TEXT NOT NULL,
    PRIMARY KEY (bucket_name, key, tag_key),
    FOREIGN KEY (bucket_name, key) REFERENCES blobs(bucket_name, key) ON DELETE CASCADE
);
CREATE INDEX idx_blob_tags_blob ON blob_tags(bucket_name, key);
CREATE INDEX idx_blob_tags_account ON blob_tags(account_id);
