CREATE TABLE apps (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT UNIQUE NOT NULL,
    contact_email TEXT NOT NULL,
    allow_refresh_jwt BOOLEAN NOT NULL DEFAULT false,
    created_at TEXT NOT NULL DEFAULT to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI:SS')
);

CREATE TABLE jwt_blacklist (
    jti TEXT PRIMARY KEY NOT NULL,
    not_after TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI:SS')
);

INSERT INTO apps (id, name, contact_email)
VALUES ('00000000-0000-0000-0000-000000000000', 'internal', 'internal@oyster.local');

ALTER TABLE accounts ADD COLUMN app_id TEXT NOT NULL
    DEFAULT '00000000-0000-0000-0000-000000000000';
UPDATE accounts SET app_id = '00000000-0000-0000-0000-000000000000' WHERE app_id IS NULL;
ALTER TABLE accounts ALTER COLUMN app_id DROP DEFAULT;
ALTER TABLE accounts ADD CONSTRAINT fk_accounts_app_id FOREIGN KEY (app_id) REFERENCES apps(id);
