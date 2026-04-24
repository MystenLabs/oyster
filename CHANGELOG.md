# Changelog

## [0.4.0] - 2026-04-24

### Breaking Changes
- On-chain storage model migrated from per-blob `Blob` objects to pool-scoped `StoragePool` + `PooledBlob` objects. Oyster lazily creates one `StoragePool` per account on the first blob write and registers, extends, and deletes blobs at the pool level. Migration `009_storage_pools.sql` **truncates the `blobs` table**; existing on-chain `Blob` objects from v0.3.0 deployments become orphaned
- Database schema: `blobs.sui_object_id`, `blobs.expires_at`, and `idx_blobs_expires_at` removed. New columns: `accounts.storage_pool_object_id`, `accounts.pool_end_epoch`, `accounts.pool_reserved_encoded_bytes`, `accounts.pool_used_encoded_bytes`, `blobs.pooled_blob_object_id`, `blobs.encoded_size`. New table: `blob_tags`
- Config renamed/removed: `BLOB_EXTEND_LOOKAHEAD_DAYS` → `POOL_EXTEND_LOOKAHEAD_DAYS`, `BLOB_EXTEND_EPOCHS` → `POOL_EXTEND_EPOCHS`; `WALRUS_DEFAULT_EPOCHS` removed (superseded by `POOL_INITIAL_EPOCHS_AHEAD`). New: `POOL_INITIAL_EPOCHS_AHEAD` (default 5), `POOL_INITIAL_ENCODED_CAPACITY_BYTES` (default 1 MiB — Walrus `BYTES_PER_UNIT_SIZE`). `BLOB_EXTEND_INTERVAL_SECS` retained as the pool-extension cycle cadence
- HTTP response fields renamed/removed: `BlobMetadata.sui_object_id` → `pooled_blob_object_id`; `StoreBlobResponse.sui_object_id` → `pooled_blob_object_id`. `expires_at` removed from both (expiration is now pool-scoped; surfaced as `pool_end_epoch` on the account). `BlobMetadata.encoded_size` added
- CLI JSON output: `oyster store` / `oyster get-metadata` now emit `pooled_blob_object_id` instead of `sui_object_id`, and no longer emit `expires_at`
- `DELETE /api/v1/buckets/{name}` status ordering fix: ownership is now checked before emptiness, so unauthorized callers see `404` instead of probing `409 Conflict` on a non-empty bucket owned by someone else
- `BlobStore::delete` trait signature takes `pool_id: Option<&str>` and `encoded_size` in place of `sui_object_id`; out-of-tree implementations must update

### Added
- First-class blob tags (user-defined metadata): `GET/PUT/PATCH/DELETE /api/v1/buckets/{b}/blobs/{k}/tags` and `PUT/DELETE .../tags/{tag_key}`. `PUT /blobs/{k}` accepts repeated `x-oyster-tag: k=v` headers as the initial tag set. Limits: max 10 tags, key ≤128 B, value ≤256 B, total ≤2 KiB, charset `[A-Za-z0-9 +\-=._:/@]`, no duplicates. Cascaded on blob delete
- S3 tagging compatibility: `PutObjectTagging`, `GetObjectTagging`, `DeleteObjectTagging`; `PutObject` parses `x-amz-tagging` (URL-form); `GetObject` populates `tag_count`
- `oyster-cli`: `tags {list,set,rm,clear,replace,merge}` subcommand and repeatable `--tag k=v` on `store`
- Per-pool background extension task: one `extend_storage_pool` PTB per account-pool per cycle. Metrics renamed to `EXTENSION_POOLS_*`. Public `run_extension_cycle_once` exposed for synchronous test drivers
- E2E coverage for the full storage-pool lifecycle: lazy pool creation on first upload, multi-upload accounting (DB vs. on-chain agreement), background-task pool extension advancing `end_epoch`, and reference-counted deletes firing `delete_pooled_blob` and freeing on-chain capacity
- `scripts/manual-test.py` coverage for the blob-tags REST surface (validation, auth, cross-account isolation); `scripts/testbed-setup.sh` now emits the admin JWT in setup output
- `BlobStoreError::PoolCreationFailed` (HTTP 502) and `BlobStoreError::Database` variants

### Changed
- Pool capacity growth rounds up to Walrus's 1 MiB `BYTES_PER_UNIT_SIZE` quantum, so small uploads amortize the `increase_storage_pool_capacity` cost across many registrations instead of paying a fresh PTB per blob
- Dependency bumps: `walrus-*` → `main` (for `pooled_blob_ops`); `sui-sdk` / `sui-types` / `shared-crypto` → `testnet-v1.70.1`; `fastcrypto` pinned to sui 1.70.1's rev; `chrono` pinned to `=0.4.39`. Added `[patch.crates-io]` for `s3s` → `wbbradley/s3s@oyster-chrono-relax` to relax its chrono requirement

### Fixed
- `DELETE /api/v1/buckets/{name}` information leak: the non-empty 409 check ran before the ownership check, letting an unauthorized caller distinguish "exists but non-empty" from "does not exist for me". Now checks ownership (→ 404) first, then emptiness (→ 409). Pinned by a new integration test
- Dedup short-circuit on upload now populates `encoded_size` on the dedup'd blob row, so `pool_used_encoded_bytes` correctly decrements when the last reference to dedup'd content is deleted (previously it was `NULL` on dedup rows, silently skipping the decrement and causing DB/on-chain drift)

### Removed
- Per-blob expiration model: `ExpiringBlob`, `get_expiring_blobs*`, `update_blob_expires_at`, and the `DEFAULT_DURATION_DAYS` per-blob expiry computation in `routes/blobs.rs::store_blob` and the S3 `put_object` path
- `WALRUS_DEFAULT_EPOCHS` and `Config::walrus_default_epochs` (superseded by `POOL_INITIAL_EPOCHS_AHEAD`)

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
