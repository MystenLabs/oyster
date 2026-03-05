ALTER TABLE accounts ADD COLUMN cached_sui_balance INTEGER NOT NULL DEFAULT 0;
ALTER TABLE accounts ADD COLUMN cached_wal_balance INTEGER NOT NULL DEFAULT 0;
ALTER TABLE accounts ADD COLUMN balance_updated_at TEXT;

CREATE TABLE pending_transactions (
    id TEXT PRIMARY KEY NOT NULL,
    account_id TEXT NOT NULL REFERENCES accounts(id),
    tx_digest TEXT,
    estimated_sui_cost INTEGER NOT NULL DEFAULT 0,
    estimated_wal_cost INTEGER NOT NULL DEFAULT 0,
    actual_sui_cost INTEGER,
    actual_wal_cost INTEGER,
    status TEXT NOT NULL DEFAULT 'pending',
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    resolved_at TEXT
);
CREATE INDEX idx_pending_tx_account ON pending_transactions(account_id);
CREATE INDEX idx_pending_tx_status ON pending_transactions(status);
