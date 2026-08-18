# Changelog

## [0.14.0] - 2026-08-17

### Security
- Closed a Turnstile anti-bot bypass in the Google OAuth signup flow.
  `/signup/start` ran the Turnstile check but stored the OAuth
  `state`/`nonce`/PKCE-verifier only in the caller-controlled
  `oyster_oauth` cookie, and `/signup/callback` validated the query
  `state` against that same cookie — so a client could fabricate a
  self-consistent cookie + `state` and reach the Google code exchange
  without ever solving Turnstile, and replay one solved challenge
  indefinitely (cookie expiry/deletion are browser-side only). The
  attempt secrets now live in a server-side `oauth_attempts` record
  (migration 023) created only after Turnstile passes; the cookie
  carries only an opaque, hashed token. The callback atomically consumes
  the record (single-use, via `DELETE … RETURNING`) and enforces expiry
  server-side, so fabricated cookies and replays are rejected before any
  Google exchange. A periodic sweep prunes expired attempts.
- Blob reads no longer let a caller-supplied `Content-Type` execute in a
  browser (stored XSS). Reads are public and echoed the stored MIME type
  verbatim, so a `text/html` (or `image/svg+xml`) blob rendered as an
  active page on the Oyster origin; `X-Content-Type-Options: nosniff`
  alone does not stop an *explicit* active type from rendering. All read
  paths — the public JSON `GET .../blobs/{key}` and `.../by-blob-id/…`,
  plus S3 `GetObject`/`HeadObject` — now return `Content-Disposition:
  attachment` and `Content-Security-Policy: default-src 'none'; sandbox`
  alongside the existing `nosniff`. Direct top-level navigation to a blob
  downloads instead of rendering; embedding as a subresource (`<img>`,
  `<video>`, `<script src>`, `fetch`) is unaffected, and the stored
  Content-Type is still returned verbatim for correct download/embed
  handling.
- Hardened the self-serve signup surface, which mints admin keys and
  shares an origin with public blob content, as defense-in-depth behind
  the blob-read fix. The state-changing POSTs (`/signup/keys/issue`,
  `/signup/keys/revoke`, `/signup/logout`) were authenticated only by the
  session cookie the browser attaches automatically; they now require the
  request `Origin` (falling back to `Referer`) to match the signup
  origin, an application-level CSRF guard that does not depend on the
  browser's `SameSite` default. All signup responses also carry a strict
  `Content-Security-Policy` (script-free for the dashboard/message pages;
  a per-response nonce for the reveal page's clipboard script; a
  Turnstile allowlist for the landing page), plus `X-Content-Type-Options:
  nosniff`, `X-Frame-Options: DENY`, and `Referrer-Policy: same-origin`,
  to shrink the blast radius of any future same-origin XSS.

### Fixed
- The `EInsufficientCapacity` register-retry path now re-runs the
  per-account storage-cap check against the refreshed on-chain usage
  before buying new pool capacity. That abort fires precisely when a
  concurrent upload consumed capacity between this request's cap check
  and its register, so the original verdict is stale — previously the
  retry recomputed `grow_by` from the refreshed pool state and grew the
  pool straight through the cap. When the refreshed pool already has
  enough reserved capacity (`retry_grow_by == 0`), the retry proceeds
  without re-checking: reserved capacity is already paid for, and
  consuming it is allowed even past the cap.
- S3 `PutObject` now enforces the same 1 GiB `MAX_BLOB_SIZE` cap as the
  JSON upload route. The S3 surface is mounted as a raw-request
  fallback, so the JSON route's axum `DefaultBodyLimit` never applied
  to it: a client could stream an arbitrarily large body and the
  server would buffer all of it into memory. Oversized uploads are now
  rejected from the declared `Content-Length` before the body is read,
  and again while draining (for clients that lie about or omit the
  length), returning S3 `EntityTooLarge` and incrementing
  `oyster_payload_too_large_responses_total{reason="body_limit"}`.

### Changed
- Blob uploads no longer hold multiple full copies of the payload in
  memory. `BlobStore::store` takes ownership of the buffer, and the
  Walrus backend feeds it straight into the RS2 encoder instead of
  cloning it twice along the way — peak per-upload memory drops by up
  to two payload-sized allocations (2 GiB for a max-size blob). The
  Walrus per-network encoder ceiling is also checked *before* encoding,
  so an over-ceiling payload is rejected without first materializing
  the ~4.5x sliver expansion.

## [0.13.1] - 2026-07-22

### Added
- `POST /account/extend`: user-triggered storage-pool extension retry.
  After funding the wallet (see `/account/wallet` or the
  `account.funding_required` webhook), calling this clears the
  extension worker's retry backoff so the pool is re-attempted on the
  next worker cycle instead of waiting out the backoff cap. Issues no
  chain transactions itself; returns 202 with the current pool end
  epoch, or 404 when the account has no pool.

### Changed
- The extension worker now retries failed pool extensions with
  exponential backoff (`EXTENSION_CLAIM_COOLDOWN_SECS * 2^failures`,
  capped by the new `EXTENSION_BACKOFF_CAP_SECS`, default 3600) instead
  of a flat 60s cadence, and pre-checks the wallet's WAL balance
  against the exact extension cost before each retry — one read RPC
  instead of the full PTB-build + sign + execute chain while the wallet
  remains unfunded. The `account.funding_required` webhook follows the
  same (slower) schedule. Migration 022 adds
  `accounts.extend_failure_count` (additive, defaulted — old pods keep
  working against the new schema during rollout).

### Fixed
- The extension worker no longer retries pools whose end epoch has
  already passed — expired Walrus storage can never be extended, and
  each doomed attempt burned the full RPC chain every cooldown,
  contributing to fullnode rate limiting. Claimed pools with a past DB
  end epoch are now reconciled against the chain: if an extension
  landed outside Oyster, the stale DB epoch is repaired from the
  on-chain value; if the chain confirms expiry, the account is reset
  for lazy re-create (pool columns cleared, unrecoverable blob rows
  deleted, an `account.pool_expired` audit event recorded). Existing
  expired rows drain through this path automatically after deploy.
- Web signup now sends `prompt=select_account` on every Google sign-in,
  forcing the account chooser. Previously, signing out of Oyster and
  back in silently reused the browser's active Google session — Oyster's
  logout only clears its own session cookie, and without a `prompt`
  parameter Google skips the chooser whenever the app was previously
  approved, so there was no way to switch accounts. (#7)
- `oyster info` no longer reports `config: (none)` when a config file
  exists but cannot be loaded. All load errors — including the
  insecure-permissions refusal (anything looser than `chmod 600`) and
  YAML parse errors — were silently swallowed, making a refused config
  indistinguishable from an absent one. `info` remains best-effort
  (exit 0, `--json` stdout stays parseable) but now prints the load
  error as a `warning:` on stderr. (#8)

## [0.13.0] - 2026-07-16

### Fixed
- Restored on-chain operation against Sui fullnodes running `sui-node`
  ≥ 1.75, which have removed the JSON-RPC interface. `walrus-sui`
  `testnet-v1.48.1` defaulted to JSON-RPC and built a JSON-RPC client
  unconditionally, so `SuiReadClient` construction — and every read
  behind it, including the `current_epoch` lookup on the blob-store
  write path — failed with HTTP 404 against
  `https://fullnode.testnet.sui.io:443`. The Walrus bump below defaults
  to a gRPC migration level that skips the JSON-RPC client entirely.

### Changed
- Walrus dependencies bumped from tag `testnet-v1.48.1` to
  `testnet-v1.52.0`, and the Sui ecosystem pinned to the revisions that
  release targets: `sui-sdk`/`sui-types`/`shared-crypto` →
  `testnet-v1.75.1`, `sui-rpc` → rev `43c5bc13`, `fastcrypto` → rev
  `c6010b90`. Embedders pinning the same revs must update.
- Upstream `RetriableSuiClient::get_object_owner_address` now returns
  `Option<SuiAddress>` rather than erroring for objects with no address
  owner. The `StoragePool` owner check in `admin_storage_pool` treats a
  missing owner as a mismatch, so a shared or immutable pool is refused
  rather than submitted as a malformed `ObjectArg`.

## [0.12.1] - 2026-06-16

### Changed
- Bumped `rand`, `rustls-webpki`, and `rpassword` to pull in upstream
  security-advisory fixes.

## [0.12.0] - 2026-06-16

### Added
- Per-account `avg_blob_size` knob (unencoded bytes) that turns
  `max_unencoded_bytes` into a *lower* bound on storable capacity for
  blobs of that size. When set, the storage-cap admission ceiling is
  inflated by the per-blob expansion factor `f(s)/s`, guaranteeing at
  least `max_unencoded_bytes` unencoded bytes are storable when blobs
  average ≥ `s`. Settable at account creation and via
  `PUT /api/v1/accounts/{account_id}/max-storage` (both echo the value;
  the orphan/shrink threshold uses the same inflated budget). `0` is the
  no-inflation sentinel (the historical upper-bound behavior). New
  accounts default to `OYSTER_DEFAULT_AVG_BLOB_SIZE` (10 MB); existing
  accounts backfill to `0`. Adds migration `020_avg_blob_size`
  (`ALTER TABLE accounts ADD COLUMN avg_blob_size BIGINT NOT NULL
  DEFAULT 0`) for both SQLite and Postgres, and the
  `OYSTER_DEFAULT_AVG_BLOB_SIZE` environment variable.
- Documentation for the blob/object tagging feature, which had shipped
  without docs: the six JSON-API tag endpoints (`GET/PUT/PATCH/DELETE`
  `.../blobs/{key}/tags` plus single-tag `PUT/DELETE .../tags/{tag_key}`),
  the S3 object-tagging operations, the `oyster tags` CLI command group,
  the repeatable `x-oyster-tag` store header, `oyster store --tag`, and
  the `oyster app webhook` CLI commands. Also documents `oysterd serve` /
  `oysterd extend` and the `--pearl-service-secret-file` flag.
- Operator analysis scripts: Walrus storage-efficiency plots
  (`walrus_storage_efficiency.py`, `walrus_storage_efficiency_ratio.py`)
  and Walrus account-cost / capacity-shortfall plots
  (`walrus_account_cost.py`).

### Fixed
- Documentation corrections: removed the dead `ENABLE_DEBUG` environment
  variable from the README, corrected the stale `oysterd app
  issue-admin-key` and `list-admin-keys` output examples, and dropped the
  no-longer-read `WALRUS_AGGREGATOR_URL` from internal docs.

## [0.11.1] - 2026-06-04

### Added
- `BlobStoreError::variant_name()` returns the variant identifier as a
  `&'static str` (e.g. `"Upstream"`).

### Changed
- Masked blob-store 5xx/500 error responses now suffix the message with
  `[BlobStoreError::<Variant>]` so callers can disambiguate variants that
  otherwise collapse to the same generic text (e.g. `Upstream` vs.
  `UpstreamStatus`, or `Internal`/`Io`/`Database`). The suffix is
  informational, not a stable machine-parseable contract; raw upstream and
  internal error text remains in server logs only. Passthrough 4xx
  responses are unchanged and not tagged.

## [0.11.0] - 2026-05-22

### Breaking Changes
- `PUT /api/v1/buckets/{bucket}/blobs/{key}` (and S3 `PutObject`)
  now returns **413 Payload Too Large** with a structured
  `payload_too_large` block (`unencoded_size_bytes`, `n_shards`,
  `max_unencoded_bytes_for_network`) when the upload exceeds the
  Walrus encoder's per-network ceiling. v0.10.2 returned an
  opaque 500. Clients that retried on 5xx must add 413 to their
  handled set.
- `DELETE /api/v1/buckets/{bucket}/blobs/{key}` (and S3
  `DeleteObject`) now performs the on-chain `delete_pooled_blob`
  **before** the DB row delete. On
  `BlobStoreError::InsufficientBalance` the route returns
  **402 Payment Required** with a `funding_required` body and
  leaves the DB row intact for retry; v0.10.2 swallowed the
  on-chain error and returned 204. Other on-chain delete errors
  are still swallowed to preserve idempotent-DELETE semantics
  (now counted in `oyster_delete_db_only_total{reason=…}`).
- `PUT /api/v1/buckets/{bucket}/blobs/{key}` now returns
  **409 Conflict** (S3: `NoSuchBucket`) when
  `DELETE /buckets/{bucket}` races with an in-flight upload;
  v0.10.2 returned 500. The just-registered `PooledBlob` is
  best-effort compensated before the 409 is returned (see
  `oyster_post_store_compensation_total`).
- Postgres migration **018_max_unencoded_bytes** widens
  `accounts.pool_end_epoch`, `pool_reserved_encoded_bytes`, and
  `pool_used_encoded_bytes` from `INTEGER` to `BIGINT`. Required —
  Rust binds these as `i64` and overflows `INTEGER` past ~2 GB.
  SQLite is dynamic-typed; the matching SQLite migration only
  adds the new column. Run all migrations before resuming
  traffic.

### Added
- Per-account `max_unencoded_bytes` storage cap (default
  `5 × 10⁹` bytes). Enforced before any on-chain work on
  `PUT /buckets/{bucket}/blobs/{key}` and S3 `PutObject`;
  over-cap uploads return **400** with a structured
  `cap_exceeded` block (`max_unencoded_bytes`,
  `used_encoded_bytes`, `new_unencoded_bytes`, `admin_endpoint`).
  S3 surface mirrors the message as `EntityTooLarge` (400).
  Migration `018_max_unencoded_bytes` adds the column with a
  `NOT NULL DEFAULT 5_000_000_000` backfill.
- `POST /api/v1/accounts` accepts optional `max_unencoded_bytes`
  (rejected with 400 when `≤ 0`).
- `PUT /api/v1/accounts/{account_id}/max-storage` admin endpoint.
  When no pool exists yet, only the DB cap is updated. When a
  pool exists, Oyster reads on-chain `reserved_encoded` /
  `used_encoded`, rejects 400 with a `would_orphan` block if
  lowering would orphan stored data, and otherwise submits a
  Pearl-signed `decrease_storage_pool_capacity_by_size` PTB. A
  concurrent-upload race surfaces as 400 with a
  `shrink_aborted` block carrying the chain's MoveAbort
  description. Returns the post-shrink on-chain pool snapshot
  and the shrink tx digest. New `account.max_storage_updated`
  audit event records old/new cap + the shrink digest.
- Auto-grow + one-time retry on `register_pooled_blobs` PTB
  aborts with `storage_pool::add_blob` code 6
  (`EInsufficientCapacity`). Reconciles DB pool counters to
  on-chain `StoragePoolInnerV1` (read via gRPC
  `StateService.ListDynamicFields`) before recomputing `grow_by`
  and resubmitting once. Handles cross-replica drift without
  process-local locking.
- Self-heal for `register_pooled_blobs` PTB aborts with
  `dynamic_field::add` code 0 (`EFieldAlreadyExists`): Oyster
  recovers the existing `PooledBlob` ObjectID off-chain and
  returns success instead of 502. Covers TOCTOU dedup races and
  recovers on-chain orphans left behind by failed delete txs
  whose DB rows were dropped to preserve idempotent DELETE. New
  metric
  `oyster_register_dedup_self_heal_total{cause=db_miss|orphan_recovered}`.
- Post-store DB-failure compensation: if `insert_blob` or
  `replace_all_tags` fails after the on-chain register PTB has
  already landed, run a bounded compensating on-chain delete (3
  attempts, 100 ms + 250 ms back-off). Failed compensations land
  in the new `dead_letter_orphans` table (migration `019`) for a
  future reaper. New metric
  `oyster_post_store_compensation_total{outcome=ok|failed}`.
- Move-abort visibility on failed Sui transactions: a new
  `SignAndSubmitError::ExecutionFailure(TxExecutionFailure)`
  carries the digest, proto `ExecutionErrorKind`, and (when
  present) the typed `MoveAbort` with module/function and abort
  code, plus an `is_move_abort(module, function, code)`
  predicate for callers to dispatch on. A failing register PTB
  now logs a tracing warning naming `storage_pool::add_blob` and
  the abort code instead of falling through to the misleading
  "no PooledBlobRegistered event" branch.
- New Prometheus metrics:
  `oyster_payload_too_large_responses_total{reason=body_limit|encoder_ceiling}`,
  `oyster_register_dedup_self_heal_total{cause=…}`,
  `oyster_post_store_compensation_total{outcome=…}`,
  `oyster_delete_db_only_total{reason=upstream_error|internal_error|other}`.
- New `BlobStoreError` variants: `CapExceeded` (400),
  `PayloadTooLarge` (413). New `AppError` variants:
  `MaxStorageWouldOrphan` (400), `MaxStorageShrinkAborted`
  (400). All four carry structured response blocks documented in
  OpenAPI.
- `BlobStoreError::InsufficientBalance` now propagates out of
  `delete_blob` (JSON) and `delete_object` (S3) as 402 with the
  `funding_required` body; the DB row is left intact so the
  caller can fund and retry.
- Manual-test scenario in `scripts/manual-test.py` exercising
  the per-account storage cap (rejection → admin raises cap →
  re-upload succeeds → restore default).

### Changed
- Sui transaction execution migrated from JSON-RPC
  `quorum_driver_api` to gRPC
  `sui_rpc::Client::execute_transaction_and_wait_for_checkpoint`.
  Same `SUI_RPC_URL` drives both protocols (Mysten fullnodes and
  the in-process test cluster serve both on the same endpoint).
  Reads still go over JSON-RPC. No behaviour change on the
  success path. Workspace adds `sui-rpc` and `prost-types` deps.
- Account JSON now includes `max_unencoded_bytes`.

### Fixed
- Walrus encoder `DataTooLargeError` no longer surfaces as an
  opaque 500. Both JSON and S3 paths now return 413 with
  structured detail so clients can distinguish mis-sized traffic
  from encoder regressions.
- `DELETE /buckets/{bucket}` racing with an in-flight upload no
  longer leaks an on-chain `PooledBlob` (compensated before the
  409 is returned).
- A `register_pooled_blobs` PTB that aborts with
  `EFieldAlreadyExists` no longer returns 502; the existing
  on-chain `PooledBlob` is recovered.
- A `register_pooled_blobs` PTB that aborts with
  `EInsufficientCapacity` due to cross-replica DB-counter drift
  no longer requires operator intervention; Oyster reconciles
  and retries once.

### Database
- New migrations on both SQLite and Postgres:
  `018_max_unencoded_bytes` (adds `accounts.max_unencoded_bytes
  BIGINT NOT NULL DEFAULT 5000000000`; Postgres also widens
  three `pool_*` columns to `BIGINT`) and
  `019_dead_letter_orphans` (creates `dead_letter_orphans` table
  for orphan-cleanup bookkeeping).

## [0.10.2] - 2026-05-21

### Fixed
- `PUT /api/v1/buckets/{bucket}/blobs/{key}` no longer rejects bodies
  larger than ~2 MiB with axum-core's `LengthLimitError`. The blob
  upload route now applies `DefaultBodyLimit::max(MAX_BLOB_SIZE)` (1
  GiB), restoring the documented contract and letting the in-handler
  size check run as intended. Other routes keep the safe 2 MiB default.

## [0.10.1] - 2026-05-20

### Added
- `oyster_funding_required_webhooks_total{outcome=success|failure}`
  Prometheus counter, incremented exactly once per
  `WebhookClient::notify_funding_required` call at each of the three
  terminal points (2xx success, 4xx non-retryable, retries exhausted).
- `oyster_insufficient_funds_responses_total{operation=…}` Prometheus
  counter, emitted from the `InsufficientBalance` short-circuit arm of
  `AppError::into_response` so every Axum 402 funding response is
  counted regardless of route. Today's reachable label values:
  `store_blob`, `unknown`.
- `BlobStoreError::with_operation(&'static str)` helper that re-tags an
  `InsufficientBalance` error with the API surface that produced it;
  no-op on other variants.

### Changed
- `BlobStoreError::InsufficientBalance` gained an `operation: &'static
  str` field tagging the API surface that produced the 402. Backend
  constructors in `direct_walrus_store` default to `"unknown"`; the
  `store_blob` route re-tags via `with_operation("store_blob")` before
  propagation.

## [0.10.0] - 2026-05-20

### Breaking Changes
- `BlobStoreError::Http(String)` split into `Upstream(String)` (502, for
  Sui/Walrus/sliver-upload failures) and `Internal(String)` (500, for
  server-internal invariant violations). The previous
  `Upstream { status, message }` struct variant is renamed to
  `UpstreamStatus { status, message }` to free the `Upstream` name.
- `BlobStoreError::InsufficientBalance(String)` reshaped to struct
  variant `{ message: String, funding_required: Option<FundingAmount> }`.
- `webhook::FundingAmount` removed; use `oyster::FundingAmount` (re-exported
  from the new `oyster::funding` module). Fields are now `u64` (`wal_frost`,
  `sui_mist`) rather than `String`; on-the-wire JSON shape is preserved by a
  custom `Serialize` impl that emits decimal strings.
- `extension_cost::ExtensionCost` removed; `compute_extension_cost` now
  returns `oyster::FundingAmount`.
- `models::FundingRequiredResponse` removed;
  `InsufficientBalanceErrorResponse.funding_required` is now
  `Option<FundingAmount>`.
- PTB-build balance shortfalls (e.g. under-funded `create_storage_pool`)
  now surface as **402 Payment Required** instead of 502 Bad Gateway.
  Clients that retried on 502 must treat 402 as the funding signal.
- Walrus dependencies bumped from rev `7ecb6720` to tag
  `testnet-v1.48.1`. Embedders pinning the same revs must update.
  Upstream walrus PR #3332 dropped `checkpoint_wait_timeout` from
  `SuiReadClient::new_for_rpc_urls`; the corresponding argument is
  removed from `oyster::sui_transaction::build_sui_read_client`.
- All workspace crates marked `publish = false` to prevent accidental
  `cargo publish` uploads.

### Added
- 402 Payment Required responses now include a `funding_required:
  { wal_frost, sui_mist }` block (decimal strings) so callers can top
  up the Pearl-derived wallet without a round-trip to
  `/account/wallet`.
- New `oyster::FundingAmount` type shared between the synchronous 402
  body and the asynchronous `account.funding_required` webhook payload.
- `InsufficientBalanceErrorResponse` schema documented in OpenAPI; the
  `store_blob` route annotated with a 402 response.
- S3 `InsufficientBalance` error message renders the `funding_required`
  hint inline (`funding_required={wal_frost:N,sui_mist:M}`).

### Changed
- `AppError::into_response` documented with a per-variant status-code
  mapping table.
- Balance-shortfall classification consolidated into
  `classify_create_pool_error` / `classify_upstream_error` at every
  PTB build and submit site in `direct_walrus_store`.
- "No `StoragePool` object in `create_storage_pool` response" now
  classified as `BlobStoreError::Internal` (500) rather than
  `PoolCreationFailed` (502) — it is a server-internal invariant
  violation, not an upstream failure.
- Pearl resolve failures, DB-stored ObjectID parse failures, and local
  encoding failures classified as `Internal` (500); `current_epoch`,
  sliver upload, and PTB submit failures classified as `Upstream` (502).

### Fixed
- Staging incident "502 Bad Gateway pool creation failed" on
  under-funded accounts: the `create_storage_pool` balance-shortfall
  path now correctly returns 402 with a top-up hint.

## [0.9.0] - 2026-05-13

### Breaking Changes
- `DirectWalrusBlobStore` now reads blobs and checks existence directly
  against Walrus storage nodes via the `walrus-sdk` `WalrusNodeClient`,
  removing the Walrus aggregator dependency entirely. The
  `WALRUS_AGGREGATOR_URL` environment variable, the
  `walrus_aggregator_url` field on `oyster::config::Config`, and the
  `aggregator_url` argument to `DirectWalrusBlobStore::new` are all
  removed. Operators must drop `WALRUS_AGGREGATOR_URL` from their
  deployment environments; embedders must update calls to
  `DirectWalrusBlobStore::new` and stop setting
  `Config::walrus_aggregator_url`. The e2e test harness's
  `.with_aggregator()` builder method is also gone, and
  `scripts/local-testbed.sh` no longer starts or wires an aggregator.

### Changed
- Blob reads and existence checks no longer require a running Walrus
  aggregator. `exists()` mirrors the aggregator's previous HEAD
  semantics by treating a blob as present only when its
  `initial_certified_epoch` is set (i.e. certified, not deleted, not
  invalid).

### Fixed
- Malformed `blob_id` path parameters on direct Walrus reads now return
  HTTP 400 (with a dedicated `BlobStoreError::InvalidBlobId` mapping)
  instead of 500, matching the previous aggregator behavior on both
  the JSON and S3 surfaces.
- Audit events are now timestamped with microsecond precision,
  eliminating tied `created_at` values that caused non-deterministic
  ordering when multiple events were recorded within the same second.

## [0.8.0] - 2026-05-06

### Breaking Changes
- `apps.webhook_url` is reset on upgrade. App builders must re-register
  their webhook URL via `PUT /api/v1/admin/app/webhook` (or
  `oyster app webhook set <URL>`) before deliveries resume. Existing rows
  pre-date the per-app keypair, so the migration nulls the URL to force a
  re-register through the new endpoint.
- `oysterd app new --webhook_url` flag removed. Webhook URLs are now
  exclusively self-service via the new admin endpoints below.

### Added
- `PUT /api/v1/admin/app/webhook`, `DELETE /api/v1/admin/app/webhook`,
  `GET /api/v1/admin/app` for self-service webhook configuration. Each
  `PUT` generates a fresh Ed25519 keypair and returns the public key
  (base64). `PUT` always rotates: the prior keypair is discarded, so
  receivers must update their stored public key after every call
- Webhook deliveries are signed with the per-app Ed25519 keypair.
  Receivers verify with `X-Oyster-Signature` (`ed25519=<base64(sig)>`)
  computed over the exact body bytes plus
  `X-Oyster-Public-Key-Fingerprint` (hex of the first 8 bytes of the
  public key, for rotation detection). Express + Flask receiver
  examples in `docs/src/guides/webhooks.md` show the verification
  flow with `tweetnacl` and `pynacl`
- `audit_events` table for security-relevant admin actions; webhook
  URL set/clear are recorded with the actor's admin-key id, the host,
  and the public-key fingerprint (never the full URL — path/query may
  contain secrets)
- `oyster app webhook {show,set,clear}` CLI commands wrapping the new
  admin endpoints
- Counter `oyster_webhook_skipped_unsigned_total` — should be zero in
  normal operation; non-zero indicates a stored key failed to decode

### Migration `017_webhook_signing_and_audit_events.sql` (sqlite + postgres)
Adds `apps.webhook_public_key` and `apps.webhook_private_key`, both
TEXT (base64-encoded 32-byte values) — TEXT is portable across the
`sqlx::Any` driver in a way that BLOB/BYTEA is not. Resets
`apps.webhook_url` to NULL. Creates the `audit_events` table with an
`(app_id, created_at)` index.

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
