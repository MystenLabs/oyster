CREATE TABLE access_keys (
    id TEXT PRIMARY KEY NOT NULL,
    account_id TEXT NOT NULL REFERENCES accounts(id),
    access_key_id TEXT NOT NULL UNIQUE,
    secret_access_key TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    revoked_at TEXT
);
CREATE INDEX idx_access_keys_account_id ON access_keys(account_id);
CREATE INDEX idx_access_keys_access_key_id ON access_keys(access_key_id);
