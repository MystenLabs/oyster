ALTER TABLE apps ADD COLUMN webhook_public_key TEXT;
ALTER TABLE apps ADD COLUMN webhook_private_key TEXT;
UPDATE apps SET webhook_url = NULL;

CREATE TABLE audit_events (
    id TEXT PRIMARY KEY NOT NULL,
    app_id TEXT NOT NULL REFERENCES apps(id),
    actor_admin_key_id TEXT,
    event_type TEXT NOT NULL,
    event_data TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI:SS')
);
CREATE INDEX audit_events_app_idx ON audit_events(app_id, created_at);
