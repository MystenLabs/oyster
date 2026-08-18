-- Server-side record for one in-flight Google OAuth login attempt.
--
-- Before this table, `/signup/start` (which runs the Turnstile anti-bot
-- gate) minted `state`, `nonce`, and the PKCE verifier and stored them
-- ONLY in the `oyster_oauth` cookie; `/signup/callback` then validated
-- the query `state` against that same cookie. Because the cookie is
-- fully caller-controlled, a client could fabricate a matching
-- state/cookie pair and reach the Google code exchange without ever
-- solving Turnstile, and replay a single solved challenge indefinitely
-- (cookie deletion/expiry are browser-side only).
--
-- Now `/signup/start` persists the attempt here — created only after
-- Turnstile passes — and the cookie carries only an opaque, hashed
-- token. The callback atomically consumes the row (single-use) and reads
-- the secrets from the server, enforcing expiry server-side.
CREATE TABLE oauth_attempts (
    id TEXT PRIMARY KEY NOT NULL,
    -- Blake2s-256 of the random cookie token; the raw token is never
    -- stored (it lives only in the browser cookie), mirroring
    -- web_sessions and API keys.
    token_hash TEXT NOT NULL UNIQUE,
    -- OAuth CSRF token echoed back by Google in `?state=`.
    state TEXT NOT NULL,
    -- Nonce bound into the id_token by Google.
    nonce TEXT NOT NULL,
    -- PKCE code verifier; its S256 digest went into the auth URL.
    pkce_verifier TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI:SS'),
    expires_at TEXT NOT NULL
);
CREATE INDEX idx_oauth_attempts_expires_at ON oauth_attempts(expires_at);
