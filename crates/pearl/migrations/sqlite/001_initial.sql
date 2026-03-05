CREATE TABLE accounts (
    id TEXT PRIMARY KEY NOT NULL,
    due_date TEXT,
    min_sui_balance INTEGER NOT NULL DEFAULT 0,
    min_wal_balance INTEGER NOT NULL DEFAULT 0,
    top_up_target_sui INTEGER NOT NULL DEFAULT 0,
    top_up_target_wal INTEGER NOT NULL DEFAULT 0,
    address TEXT NOT NULL,
    private_key BLOB NOT NULL,
    credentials TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE UNIQUE INDEX idx_accounts_address ON accounts(address);
CREATE INDEX idx_accounts_credentials ON accounts(credentials);
