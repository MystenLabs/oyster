ALTER TABLE api_keys ADD COLUMN note TEXT NOT NULL DEFAULT 'api';
CREATE INDEX idx_api_keys_account_revoked ON api_keys(account_id, revoked_at);
