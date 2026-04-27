CREATE TABLE app_admin_keys (
    id TEXT PRIMARY KEY NOT NULL,
    app_id TEXT NOT NULL REFERENCES apps(id),
    key_hash TEXT NOT NULL,
    prefix TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    revoked_at TEXT
);
CREATE INDEX idx_app_admin_keys_key_hash ON app_admin_keys(key_hash);
CREATE INDEX idx_app_admin_keys_app_id ON app_admin_keys(app_id);

DROP TABLE jwt_blacklist;

ALTER TABLE apps DROP COLUMN allow_refresh_jwt;
