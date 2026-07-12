-- Self-serve web signup: users authenticated via external identity
-- providers (Google OAuth first; the provider/subject split keeps email
-- or other providers additive later), browser sessions, and a request
-- queue for gated signup modes. `apps.owner_user_id` links an app to
-- the web user who owns it; NULL for operator-created apps.

CREATE TABLE users (
    id TEXT PRIMARY KEY NOT NULL,
    email TEXT NOT NULL,
    display_name TEXT,
    created_at TEXT NOT NULL DEFAULT to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI:SS')
);

CREATE TABLE user_identities (
    id TEXT PRIMARY KEY NOT NULL,
    user_id TEXT NOT NULL REFERENCES users(id),
    provider TEXT NOT NULL,
    provider_subject TEXT NOT NULL,
    -- NULL for OAuth providers; reserved for a password hash if email
    -- signup is added later.
    credential TEXT,
    verified_at TEXT,
    created_at TEXT NOT NULL DEFAULT to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI:SS'),
    UNIQUE (provider, provider_subject)
);
CREATE INDEX idx_user_identities_user_id ON user_identities(user_id);

CREATE TABLE web_sessions (
    id TEXT PRIMARY KEY NOT NULL,
    user_id TEXT NOT NULL REFERENCES users(id),
    -- Blake2s-256 of the random cookie token; the raw token is never stored.
    token_hash TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL DEFAULT to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI:SS'),
    expires_at TEXT NOT NULL
);
CREATE INDEX idx_web_sessions_user_id ON web_sessions(user_id);
CREATE INDEX idx_web_sessions_expires_at ON web_sessions(expires_at);

CREATE TABLE signup_requests (
    id TEXT PRIMARY KEY NOT NULL,
    provider TEXT NOT NULL,
    provider_subject TEXT NOT NULL,
    email TEXT NOT NULL,
    display_name TEXT,
    -- pending | approved | rejected
    status TEXT NOT NULL DEFAULT 'pending',
    requested_at TEXT NOT NULL DEFAULT to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI:SS'),
    decided_at TEXT,
    decided_by TEXT,
    UNIQUE (provider, provider_subject)
);

ALTER TABLE apps ADD COLUMN owner_user_id TEXT REFERENCES users(id);
