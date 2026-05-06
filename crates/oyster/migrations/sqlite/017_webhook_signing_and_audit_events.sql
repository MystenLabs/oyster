ALTER TABLE apps ADD COLUMN webhook_public_key TEXT;
ALTER TABLE apps ADD COLUMN webhook_private_key TEXT;
UPDATE apps SET webhook_url = NULL;

CREATE TABLE audit_events (
    id TEXT PRIMARY KEY NOT NULL,
    app_id TEXT NOT NULL,
    actor_admin_key_id TEXT,
    event_type TEXT NOT NULL,
    event_data TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY (app_id) REFERENCES apps(id)
);
CREATE INDEX audit_events_app_idx ON audit_events(app_id, created_at);
