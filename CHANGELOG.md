# Changelog

## [0.3.0] - 2026-04-21

### Breaking Changes
- `oyster-cli` `client.yaml` schema rewritten to named contexts — top-level `url`/`api_key` no longer parse (`deny_unknown_fields`). Migrate by wrapping existing values in `contexts.<name>: { url, api_key }` and optionally setting `active_context: <name>`.

### Added
- `oyster-cli`: `--context <name>` global flag and `OYSTER_CONTEXT` env var select the active context (precedence: flag > env > `active_context` field; auto-selects when exactly one context is defined)
- `oyster-cli`: new `app` subcommand — `app import <name>` reads a JWT with hidden tty input (or a stdin line when piped) and stores it, `app refresh-jwt <name>` calls `POST /apps/token-refresh` with the stored JWT, writes the rotated token back atomically, prints the new JWT on stdout with status on stderr
- `oyster-cli` `client.yaml`: per-context `apps.<name>: { jwt, jwt_expiry }` map for persisted app JWTs; `jwt_expiry` is populated by decoding the JWT `exp` claim without signature verification
- `oyster-cli` `app refresh-jwt` surfaces 401 as "ask admin to re-issue via `oysterd app jwt <APP_ID>`" and 403 as "refresh not allowed for this app (`allow_refresh_jwt=false`)"

### Changed
- Upstream Walrus aggregator connect failures, DNS failures, and request timeouts now return HTTP 502 Bad Gateway (both JSON API and S3 paths) instead of 500 Internal Server Error
- The root-level S3 fallback no longer shadows unmatched `/api/*` URLs — those now return a clean `{"error":"not found"}` 404 instead of a confusing S3 "invalid authorization header" 400
- `/apps/token-refresh` and JWT grace-verification emit tracing logs (warn on reject reasons, info on successful refresh with `app_id`/`jti`/`expired_ago_secs`) to make production failures diagnosable
- Docs: README and `docs/src/guides/cli.md` updated with the multi-context schema, precedence rules, and `app import` / `app refresh-jwt` usage

### Fixed
- Chain-refreshing a JWT (refreshing a token itself obtained from a prior refresh) now works correctly for both fresh and expired-within-grace tokens

## [0.2.2] - 2026-04-10

### Added
- Every HTTP response now carries an `X-Oyster-Version` header exposing the running server's crate version, so clients can identify which Oyster build they are talking to

## [0.2.1] - 2026-04-10

### Fixed
- Upstream Walrus aggregator failures on blob GET/HEAD now map to HTTP 502 Bad Gateway (previously 500 Internal Server Error), and upstream 4xx responses other than 404 are passed through with their original status code instead of being masked as 500
- `oysterd` and `pearl` now write tracing output to stderr instead of stdout, so stdout can be used cleanly by callers and process supervisors
- Local testbed now starts the Walrus aggregator daemon and uses correct `RUST_LOG` syntax, so blob reads work out of the box
- `scripts/manual-test.py` repaired against the current API surface: uses the JWT-authenticated admin API for account setup, drops the `Content-Type` header on empty-body account creation, and exercises both malformed and well-formed-but-missing blob IDs

### Changed
- Default `RUST_LOG` filter silences the noisy `s3s::ops` module
- Removed obsolete `procman.yaml`; `oyster.pman` is now the sole procman configuration for local dev

## [0.2.0] - 2026-04-08

### Breaking Changes
- Self-service key management endpoints removed; all account and key management now requires JWT-authenticated admin routes
- API key creation response field renamed from `secret` to `bearer_token`
- `FUND_MANAGER_WEBHOOK_URL` global environment variable removed; webhook URLs are now configured per-app
- Bucket deletion returns `409 Conflict` when the bucket contains blobs, matching AWS S3 `BucketNotEmpty` behavior

### Added
- Web2-friendly object storage API backed by Walrus and Sui
- Axum HTTP server (Oyster) with bucket and blob CRUD at `/api/v1`
- S3-compatible API supporting AWS SDKs and standard S3 tooling
- Pearl custodial wallet gRPC service with HKDF-SHA256 key derivation and Sui transaction signing
- oyster-cli command-line client for the Oyster HTTP API
- JWT-authenticated admin API for account, API key, and S3 access key management
- App management via `oysterd app` CLI commands (create, list, issue/revoke JWTs)
- Optional `name` field on the create-account endpoint
- `If-Match` / `If-None-Match` conditional request headers for blob operations
- Content-addressed blob storage with SHA-256 blob IDs
- Background extension worker for auto-renewing expiring blobs on Walrus
- Per-app webhook URL configuration for fund-manager notifications
- Blake2s-256 hashed Bearer token authentication
- `X-Content-Type-Options: nosniff` header on blob read responses
- OpenAPI documentation via Scalar at `/api/docs`
- Health, readiness, and Prometheus metrics endpoints
- SQLite and PostgreSQL support via SQLx `Any` driver
- Full E2E test suite booting Sui + Walrus + Pearl + Oyster in-process
- mdbook documentation site

### Fixed
- Duplicate bucket name returns `409` instead of `500`
- Nonexistent blob lookup returns `404` instead of `500`
- Listing blobs in a nonexistent bucket returns `404` instead of empty `200`
- Pagination with `limit <= 0` returns `400` instead of silently clamping
- `BlobStoreError::InsufficientBalance` returns `402`, `NotFound` returns `404`
- PostgreSQL `BOOLEAN` column decoding for `allow_refresh_jwt`
- `jsonwebtoken` CryptoProvider conflict resolved by explicit provider installation
- E2E tests serialized via global mutex to prevent Sui cluster races
