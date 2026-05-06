# Changelog

## [Unreleased]

## [0.7.1] - 2026-05-06

### Changed
- Documentation pass to strip pre-release backwards-facing language. Asides like "no longer carry `expires_at`", per-blob `expires_at` JSON snippets, "30-day default" wording, and the pre-0.3 `client.yaml` flat-schema breaking-change blockquote have been replaced with positive, present-tense statements about the current pool-scoped model. The `Blob Lifecycle` "How it works (continuous loop)" + "Multi-instance safe" walkthrough (with `UPDATE … RETURNING` and the `garbage_collector.rs` reference) is now framed as operational guarantees: latency bound, per-account cooldown rate-limit, and replica safety. The README's extension-worker section drops the `garbage_collector.rs` aside and the literal SQL phrasing while keeping the operational facts. No code or API changes

## [0.7.0] - 2026-05-05

### Breaking Changes
- Extension task pivoted to a continuous, idempotent loop modeled on Walrus's `garbage_collector.rs`. Removed env vars: `BLOB_EXTEND_INTERVAL_SECS`, `POOL_EXTEND_LOOKAHEAD_DAYS`. New env vars: `POOL_EXTEND_LOOKAHEAD_EPOCHS` (raw epochs, replaces the day-shaped lookahead), `EXTENSION_IDLE_SLEEP_SECS` (default 30), `EXTENSION_BUSY_SLEEP_MS` (default 250), `EXTENSION_CLAIM_BATCH_SIZE` (default 100), `EXTENSION_CLAIM_COOLDOWN_SECS` (default 60). The cron-style `check_interval` field on `ExtensionConfig` is gone; `run_extension_cycle_once` now returns the number of pool rows processed instead of `(extended, errors)`
- Webhook payload renamed and reshaped. Old: `InsufficientFundsPayload { account_id, address, error }` posted by `WebhookClient::notify_insufficient_funds`. New: `FundingRequiredPayload { event_id, type: "account.funding_required", account_id, pearl_address, amount: { wal_frost, sui_mist }, timestamp }` posted by `WebhookClient::notify_funding_required`. `event_id` is generated once per delivery and is stable across the internal retry loop, enabling receiver-side dedupe. Token amounts are decimal strings to avoid JSON-number precision loss for `u64`. The error string is no longer included
- Migration `016_extend_attempt_after.sql` (sqlite + postgres): adds `accounts.extend_attempt_after TEXT` plus the partial index `accounts_extend_due ON accounts (pool_end_epoch, extend_attempt_after) WHERE storage_pool_object_id IS NOT NULL`. The atomic `UPDATE … RETURNING` claim guarantees disjoint result sets across concurrent workers, so the same row cannot be re-claimed (or re-notified) for `EXTENSION_CLAIM_COOLDOWN_SECS` regardless of the attempt's outcome — single backoff knob covers both worker-coordination and webhook-spam suppression

### Added
- `extension_cost::compute_extension_cost` helper computes `(wal_frost, sui_mist)` for a given pool + extend epochs. WAL is computed from `walrus_sui::utils::price_for_encoded_length`; SUI is a fixed `SUI_GAS_PER_EXTENSION_BUFFER_MIST = 100_000_000` (≈0.1 SUI) buffer — Oyster does not dry-run gas
- `db::accounts::claim_pools_for_extension` (atomic `UPDATE … RETURNING` claim) and `db::accounts::fetch_webhook_urls_for_apps` follow-up keyed by the claim's app-ids

### Changed
- Dependency bumps: `sui` → `testnet-v1.71.0`, `walrus-*` → `main`. Dropped the `[patch.crates-io]` `s3s` fork (`wbbradley/s3s@oyster-chrono-relax`); upstream `s3s` is now compatible with the workspace's `chrono` pin

### Fixed
- Extension worker's `bump_pool_end_epoch` is now monotonic — never lowers `pool_end_epoch`, so a stale post-extend write cannot regress a row another worker has already advanced further
- `s3s::ops` log silencing is now unconditional in the default `RUST_LOG` filter, so the noisy per-request `s3s::ops` traces no longer leak through when callers set `RUST_LOG=info` or similar

### Recommended config per network

| Network | Epoch length | `POOL_EXTEND_LOOKAHEAD_EPOCHS` | `POOL_EXTEND_EPOCHS` |
|---------|--------------|--------------------------------|----------------------|
| testnet | ≈ 1 day      | 7                              | 30                   |
| mainnet | ≈ 14 days    | 1                              | 4                    |

## [0.6.0] - 2026-04-29

### Breaking Changes
- `oyster-cli` now refuses to read `client.yaml` on Unix if its mode allows any group/other access (`mode & 0o077 != 0`). Existing 0644-style configs from 0.5.0 will fail every CLI invocation until you run `chmod 600 <path>` (the error message embeds the exact command). Save opens the temp file with `O_CREAT | O_EXCL` and mode `0o600` set at file-creation time via `OpenOptions::mode`, so the yaml never lands on disk at a more permissive mode (closing the TOCTOU window between content-write and chmod). Each save uses a unique temp suffix to coexist with `O_EXCL` across crash-leftover temps. Windows behavior unchanged
- New per-account active-api-key cap of **3** on `POST /api/v1/accounts/{account_id}/api-keys`: returns `409` with `"limit"` in the message when exceeded; revoke a key to free a slot. CLI users are insulated by the `oyster app account use` revoke-on-cap flow; direct HTTP callers must handle the new `409`

### Added — Server
- `GET /api/v1/accounts` — list accounts owned by the authenticated app; returns `[AccountSummary]` with `active_api_key_count` per row
- `GET /api/v1/accounts/{account_id}/api-keys` — list api-key metadata for an account (never returns the bearer secret)
- Optional `note` field on `POST /api/v1/accounts` and `POST /api/v1/accounts/{account_id}/api-keys` request bodies; defaults to `'api'` when omitted
- Migration `015_api_keys_note.sql` (sqlite + postgres): adds `api_keys.note TEXT NOT NULL DEFAULT 'api'` plus a compound `(account_id, revoked_at)` index for the cap-count query

### Added — CLI
- `oyster app account list` — table over accounts (id, name, created_at, active_api_key_count)
- `oyster app account create [--name NAME] [--note NOTE] [--activate]` — mints account + initial api-key; `--activate` saves the bearer to `context.api_key` atomically
- `oyster app account use <id-or-name> [--revoke <key_id> | --revoke-oldest]` — mints a fresh api-key (note `oyster-cli: activate <id-or-name>`) and replaces `context.api_key` atomically; on `409` in a TTY, prompts via `inquire` (inline, never alt-screen) and retries
- `oyster app account select` — TTY-only `inquire` picker over accounts; dispatches to `use`
- `oyster app account keys <id-or-name>` — list api-key metadata for an account
- Global `--app <name>` flag on `oyster app account` to disambiguate when the active context has multiple apps; auto-picked when there's exactly one

### Dependencies
- `oyster-cli` adds `inquire = 0.9.4` for the inline TUI revoke picker

## [0.5.0] - 2026-04-28

### Breaking Changes
- Admin authentication migrated from short-lived JWTs to long-lived per-app **admin keys**. Both tiers continue to use `Authorization: Bearer <hex>` on the wire; the route prefix selects which credential table the bearer is looked up in (`api_keys` for data routes, `app_admin_keys` for admin routes). Existing JWTs are rejected immediately on upgrade
- Removed: `OYSTER_JWT_SECRET` env var, `--oyster-jwt-secret-file` CLI flag, `POST /api/v1/apps/token-refresh`, `oysterd app jwt`, `oysterd app revoke-jwt`, `apps.allow_refresh_jwt` column, `jwt_blacklist` table
- Added: `oysterd app issue-admin-key <APP_ID>`, `oysterd app list-admin-keys <APP_ID>`, `oysterd app revoke-admin-key <KEY_ID>`. `oysterd app new` auto-issues a first admin key by default; pass `--no-key` to opt out
- `oyster-cli` `client.yaml` schema: `apps.<name>: { admin_key }` replaces `apps.<name>: { jwt, jwt_expiry }`. Old configs fail to parse under `deny_unknown_fields`. `oyster app refresh-jwt` removed (rotation is operator-driven via issue + revoke); `oyster app import` now prompts for an admin key
- Migration `014_app_admin_keys.sql` (sqlite + postgres): creates `app_admin_keys`, drops `jwt_blacklist`, drops `apps.allow_refresh_jwt`. Operators must issue at least one admin key per app before its admin routes become reachable; the migration does not auto-issue
- Supported SQLite floor raised to ≥ 3.35 (for `ALTER TABLE … DROP COLUMN`)

### Added
- AWS-style two-key rotation for admin auth: multiple admin keys per app are supported with no cap. Issue a new key, switch callers, then revoke the old key; revocation is immediate
- E2E harness lock-poison recovery: `crates/oyster-e2e-tests/src/lib.rs::run_e2e` now recovers from `PoisonError` when acquiring `E2E_LOCK`, so a transient panic in one test (e.g., walrus's `git ls-remote` failure during Move-dep fetch) no longer cascade-fails every subsequent test in the suite

### Removed
- `crates/oyster/src/app_auth.rs` (entire file: `AppClaims`, `sign_jwt`, `verify_jwt`, `verify_jwt_with_grace`, `JWT_GRACE_SECS`, `install_crypto_provider`)
- `crates/oyster/src/db/jwt_blacklist.rs`
- `crates/oyster/src/routes/admin.rs::token_refresh` handler and OpenAPI wiring
- `models::TokenRefreshResponse`, `models::App.allow_refresh_jwt` field, `decode_allow_refresh_jwt` helper
- `Config::jwt_secret`, `SecretOverrides::oyster_jwt_secret`, `Cli::oyster_jwt_secret_file`
- `jsonwebtoken` workspace dep (still pulled in transitively via walrus-sdk / walrus-service)
- `base64` and `chrono` direct deps from `oyster-cli` (`decode_exp` and the JWT `exp` decoder are gone)

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
