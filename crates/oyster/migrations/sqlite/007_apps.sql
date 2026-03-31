CREATE TABLE apps (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT UNIQUE NOT NULL,
    contact_email TEXT NOT NULL,
    allow_refresh_jwt INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE jwt_blacklist (
    jti TEXT PRIMARY KEY NOT NULL,
    not_after TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

INSERT INTO apps (id, name, contact_email)
VALUES ('00000000-0000-0000-0000-000000000000', 'internal', 'internal@oyster.local');

ALTER TABLE accounts ADD COLUMN app_id TEXT NOT NULL
    DEFAULT '00000000-0000-0000-0000-000000000000' REFERENCES apps(id);

UPDATE accounts SET app_id = '00000000-0000-0000-0000-000000000000' WHERE app_id IS NULL;
