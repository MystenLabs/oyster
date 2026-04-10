# Changelog

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
