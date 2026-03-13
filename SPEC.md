# Oyster + Pearl Service Specification

Status: Draft v1

Purpose: Define a two-service system that provides a Web2-friendly object storage API backed by
decentralized storage infrastructure (Walrus and Sui).

## 1. Problem Statement

Oyster is an HTTP object storage service that presents a familiar buckets-and-blobs API to
application developers while storing data on Walrus, a decentralized blob storage network, with
on-chain state managed on Sui. Pearl is a companion custodial wallet service that derives
cryptographic keys and signs Sui transactions on behalf of Oyster accounts.

The system solves four problems:

- It gives developers a conventional REST API (create buckets, upload blobs, list objects) without
  requiring them to understand blockchain transactions, gas fees, or decentralized storage protocols.
- It manages Sui wallet keys custodially so that each Oyster account has a deterministic on-chain
  identity without users holding private keys.
- It automatically extends blob storage leases before they expire, keeping data available without
  operator intervention.
- It abstracts the blob storage backend so the same API works against a local filesystem (for
  development) or Walrus (for production), with the backend selected by configuration.

Important boundaries:

- Oyster is a storage API server and background blob-lifecycle manager. It does not implement a
  billing system; accounts are pre-funded on-chain and Oyster communicates funding needs to an
  external billing layer via webhook.
- Pearl is a stateless signing service. It derives keys on demand from a master seed and never
  persists private key material.
- Blob reads are public and unauthenticated by design. Walrus data is publicly accessible on the
  decentralized network; Oyster mirrors this property at the API layer.

## 2. Goals and Non-Goals

### 2.1 Goals

- Provide S3-like object storage semantics (accounts, API keys, buckets, blobs) over HTTP.
- Support both hosted and self-hosted deployment models.
- Store blobs on Walrus with on-chain state on Sui, or on a local filesystem for development.
- Manage per-account Sui wallets custodially via deterministic key derivation (HKDF-SHA256).
- Automatically extend expiring blob storage leases in the background.
- Notify an external funding/billing layer via webhook when an account has insufficient on-chain
  funds for lease extension.
- Expose Prometheus metrics for HTTP requests, blob operations, gRPC calls, and extension cycles.
- Support SQLite (development) and PostgreSQL (production) as database backends, selected at runtime
  by connection URL.
- Generate OpenAPI documentation from route definitions.
- Content-address blobs for automatic deduplication.

### 2.2 Non-Goals

- Built-in billing, metering, or payment processing. Oyster assumes accounts are pre-funded on-chain
  and defers billing to an external system.
- Access control on blob reads. Blob data is public, consistent with Walrus's public data model.
- End-to-end encryption of blob data. Clients may encrypt before upload, but Oyster does not manage
  encryption keys.
- Multi-region replication or CDN. Walrus handles data distribution; Oyster is a single-instance API
  server.
- User-facing web UI or admin dashboard.
- General-purpose blockchain transaction builder. Pearl signs only Sui transactions for Oyster's blob
  storage operations.

## 3. System Overview

### 3.1 Services

1. **Oyster** (`oysterd`)
   - Axum HTTP server on port 3000 (default).
   - Manages accounts, API keys, buckets, and blobs in a relational database.
   - Delegates blob storage to a pluggable `BlobStore` backend (local filesystem or Walrus).
   - Calls Pearl gRPC to sign Sui transactions when using the Walrus backend.
   - Runs a background extension task to renew expiring blob leases.
   - Exposes OpenAPI docs at `/api/docs` and Prometheus metrics at `/metrics`.

2. **Pearl**
   - Tonic gRPC server on port 50051 (default).
   - Derives Ed25519 keypairs from a master seed via HKDF-SHA256.
   - Signs Sui `TransactionData` and returns signed `Transaction` bytes.
   - Exposes Prometheus metrics on port 50052 (default).
   - Supports optional TLS for gRPC transport.

3. **oyster-cli**
   - Command-line client for the Oyster HTTP API.
   - Configured via `client.yaml` or CLI flags.
   - Supports all CRUD operations on buckets, blobs, and API keys.

### 3.2 Abstraction Layers

1. **API Layer** (Oyster HTTP routes)
   - Request parsing, authentication, response serialization.
   - OpenAPI schema generation via `utoipa`.

2. **Storage Layer** (`BlobStore` trait)
   - Pluggable backend for blob persistence.
   - Two implementations: `LocalBlobStore` (filesystem) and `DirectWalrusBlobStore` (on-chain).

3. **Custody Layer** (Pearl gRPC)
   - Key derivation and transaction signing.
   - Isolated from application logic; speaks only gRPC.

4. **Persistence Layer** (SQLx with `Any` driver)
   - SQLite or PostgreSQL, selected by connection URL at runtime.
   - Migration-based schema management with separate migration sets per backend.

5. **Lifecycle Layer** (extension task)
   - Background loop that extends expiring blobs.
   - Webhook integration for insufficient-funds notification.

### 3.3 External Dependencies

- **Walrus network**: Decentralized blob storage (aggregator nodes, storage nodes).
- **Sui RPC**: On-chain transaction submission and object queries.
- **Database**: SQLite (local) or PostgreSQL (production).
- **External billing layer** (optional): Receives webhook notifications for funding shortfalls.

## 4. Core Domain Model

### 4.1 Entities

#### 4.1.1 Account

Top-level identity that owns API keys, buckets, and blobs.

Fields:

- `id` (string, UUID v4)
  - Primary key. Also used as the account identifier for Pearl key derivation.
- `created_at` (string, UTC datetime)
- `updated_at` (string, UTC datetime)

#### 4.1.2 API Key

Bearer token credential scoped to one account.

Fields:

- `id` (string, UUID v4)
  - Primary key.
- `account_id` (string)
  - Foreign key to `accounts.id`.
- `key_hash` (string)
  - Blake2s-256 hash of the raw 32-byte key, hex-encoded.
- `prefix` (string)
  - First 8 characters of the raw hex-encoded key. Used for human identification; not sufficient for
    authentication.
- `created_at` (string, UTC datetime)
- `revoked_at` (string or null, UTC datetime)
  - Non-null indicates a soft-revoked key. Revoked keys fail authentication.

Indexes:

- `key_hash` (lookup during authentication)
- `account_id` (list keys for an account)

#### 4.1.3 Bucket

Named container for blobs, scoped to one account.

Fields:

- `id` (string, UUID v4)
  - Primary key.
- `account_id` (string)
  - Foreign key to `accounts.id`.
- `name` (string)
  - Human-readable name. Unique within an account (`UNIQUE(account_id, name)`).
- `created_at` (string, UTC datetime)

Index:

- `account_id` (list buckets for an account)

#### 4.1.4 Blob

A stored object within a bucket.

Fields:

- `object_id` (string, UUID v4)
  - Primary key. Unique per upload, even for identical content.
- `blob_id` (string)
  - Content-addressed identifier. For the local store, this is the Blake2s-256 hash of the data
    (hex-encoded). For the Walrus store, this is the Walrus-assigned blob identifier.
- `bucket_id` (string)
  - Foreign key to `buckets.id`.
- `account_id` (string)
  - Foreign key to `accounts.id`. Denormalized from bucket for query efficiency.
- `content_type` (string)
  - MIME type. Defaults to `application/octet-stream`.
- `size` (integer, 64-bit for PostgreSQL)
  - Blob size in bytes.
- `sui_object_id` (string or null)
  - Sui on-chain object identifier. Present only for Walrus-stored blobs.
- `created_at` (string, UTC datetime)
- `expires_at` (string or null, UTC datetime)
  - Expiration time for the blob's storage lease. Present only for Walrus-stored blobs.

Indexes:

- `bucket_id` (list blobs in a bucket)
- `blob_id` (content-addressed lookup)
- `account_id` (list blobs for an account)
- `expires_at` (extension worker queries for expiring blobs)

### 4.2 Content Deduplication

Multiple blob records may reference the same `blob_id` (content hash). The underlying data is stored
once. Deletion of a blob record only removes the underlying data if no other records reference the
same `blob_id`.

### 4.3 Identifiers and Normalization

- **Account ID**: UUID v4, generated at account creation.
- **API Key ID**: UUID v4, generated at key creation.
- **Bucket ID**: UUID v4, generated at bucket creation.
- **Object ID**: UUID v4, generated per blob upload.
- **Blob ID**: Content-addressed. Blake2s-256 hex digest (local store) or Walrus blob identifier
  (Walrus store).
- **Sui Object ID**: Hex-encoded Sui object address (e.g., `0x...`). Present only for
  Walrus-stored blobs.

## 5. Authentication and Authorization

### 5.1 Oyster API Key Authentication

Mechanism: Bearer token in the `Authorization` header.

```
Authorization: Bearer <raw_hex_key>
```

Key generation:

1. Generate 32 cryptographically random bytes.
2. Hex-encode to produce a 64-character string (the raw key).
3. Compute `key_hash = hex(blake2s_256(raw_key))`.
4. Store `key_hash` and `prefix = raw_key[0..8]` in the `api_keys` table.
5. Return the raw key to the caller exactly once. The raw key is never stored.

Authentication flow:

1. Extract `Bearer <token>` from the `Authorization` header.
2. Compute `candidate_hash = hex(blake2s_256(token))`.
3. Query `api_keys` for a row where `key_hash = candidate_hash` and `revoked_at IS NULL`.
4. If found, extract the associated `account_id`. If not found, return `401 Unauthorized`.

### 5.2 Pearl Service Secret Authentication

Mechanism: Shared secret in the gRPC `authorization` metadata header.

```
authorization: Bearer <service_secret>
```

Validation:

- Constant-time comparison (`subtle::ConstantTimeEq`) of the provided secret against the configured
  `PEARL_SERVICE_SECRET`.
- Applied as a Tonic interceptor to all RPC calls.
- Returns gRPC status `16 (Unauthenticated)` on mismatch.

### 5.3 Public Blob Reads

Blob read endpoints (`GET /blobs/{object_id}`, `GET /blobs/by-blob-id/{blob_id}`) do not require
authentication. This is intentional: Walrus data is publicly accessible on the decentralized
network, and Oyster mirrors this property.

### 5.4 Authorization Model

Authorization is identity-scoped: an authenticated API key grants access to all resources owned by
the associated account. There is no per-bucket or per-blob ACL. Specifically:

- Bucket and blob mutations require a valid API key whose account owns the target resource.
- Blob reads are public (Section 5.3).
- Account-scoped operations (create API key, revoke API key, view wallet) require a valid API key
  for that account.

## 6. HTTP API Specification

All request and response bodies use JSON unless otherwise noted. Error responses use the format:

```json
{
  "error": "human-readable error message"
}
```

### 6.1 Health and Readiness

#### `GET /health`

Liveness probe. Always returns `200 OK`.

#### `GET /ready`

Readiness probe. Checks database connectivity and Pearl gRPC reachability (if configured). Returns
`200 OK` if all checks pass, `503 Service Unavailable` otherwise.

### 6.2 Debug Endpoints

Available only when `ENABLE_DEBUG=true`.

#### `POST /debug/accounts`

Creates a new account with an initial API key. Intended for development and testing.

Response `201 Created`:

```json
{
  "account_id": "uuid",
  "api_key": "raw_hex_key"
}
```

### 6.3 API Key Management

All endpoints in this section require authentication.

#### `POST /api/keys`

Creates a new API key for the authenticated account.

Response `201 Created`:

```json
{
  "id": "uuid",
  "prefix": "first8chars",
  "api_key": "raw_hex_key",
  "created_at": "YYYY-MM-DD HH:MM:SS"
}
```

The `api_key` field contains the raw key and is returned exactly once.

#### `DELETE /api/keys/{key_id}`

Revokes an API key by setting `revoked_at` to the current time. The key must belong to the
authenticated account.

Response `204 No Content`.

### 6.4 Wallet

#### `GET /wallet`

Returns the Sui wallet address for the authenticated account's Pearl-derived wallet.

Requires Pearl integration. Returns `501 Not Implemented` if Pearl is not configured.

Response `200 OK`:

```json
{
  "address": "0x..."
}
```

### 6.5 Billing and Transfers (Stubs)

These endpoints are defined but not yet implemented. They return `501 Not Implemented`.

- `PATCH /billing` -- Update billing configuration.
- `GET /reports` -- Retrieve usage reports.
- `POST /transfer` -- Transfer resources between accounts.

### 6.6 Bucket Operations

All endpoints require authentication.

#### `POST /buckets`

Creates a new bucket.

Request:

```json
{
  "name": "my-bucket"
}
```

Response `201 Created`:

```json
{
  "id": "uuid",
  "name": "my-bucket",
  "created_at": "YYYY-MM-DD HH:MM:SS"
}
```

Returns `409 Conflict` if a bucket with the same name already exists for the account.

#### `GET /buckets`

Lists buckets for the authenticated account. Supports cursor-based pagination (Section 6.9).

Response `200 OK`:

```json
{
  "data": [
    {
      "id": "uuid",
      "name": "my-bucket",
      "created_at": "YYYY-MM-DD HH:MM:SS"
    }
  ],
  "next_cursor": "opaque_token_or_null"
}
```

#### `DELETE /buckets/{bucket_id}`

Deletes a bucket and all blobs within it. The bucket must belong to the authenticated account.
Underlying blob data is deleted if no other blob records reference the same `blob_id`.

Response `204 No Content`.

### 6.7 Blob Operations

#### `PUT /buckets/{bucket_id}/blobs` (authenticated)

Uploads a blob to a bucket.

Request:

- Body: Raw binary data.
- `Content-Type` header: Used as the blob's content type. Defaults to `application/octet-stream`.
- Maximum body size: 1 GB (1,073,741,824 bytes). Returns `413 Payload Too Large` if exceeded.

Response `201 Created`:

```json
{
  "object_id": "uuid",
  "blob_id": "content_hash",
  "size": 12345,
  "content_type": "application/octet-stream",
  "sui_object_id": "0x...",
  "created_at": "YYYY-MM-DD HH:MM:SS",
  "expires_at": "YYYY-MM-DD HH:MM:SS"
}
```

`sui_object_id` and `expires_at` are present only when using the Walrus backend. The default
expiration is 30 days from creation.

#### `GET /buckets/{bucket_id}/blobs` (authenticated)

Lists blobs in a bucket. Supports cursor-based pagination (Section 6.9).

Response `200 OK`:

```json
{
  "data": [
    {
      "object_id": "uuid",
      "blob_id": "content_hash",
      "size": 12345,
      "content_type": "application/octet-stream",
      "sui_object_id": "0x...",
      "created_at": "YYYY-MM-DD HH:MM:SS",
      "expires_at": "YYYY-MM-DD HH:MM:SS"
    }
  ],
  "next_cursor": "opaque_token_or_null"
}
```

#### `GET /blobs/{object_id}` (public)

Reads a blob by its object ID.

Response `200 OK`:

- Body: Raw binary data.
- `Content-Type` header: Set to the blob's stored content type.

Returns `404 Not Found` if the object ID does not exist.

#### `GET /blobs/by-blob-id/{blob_id}` (public)

Reads a blob by its content-addressed blob ID. If multiple blob records share the same `blob_id`,
any one may be used to resolve the content type.

Response `200 OK`: Same as `GET /blobs/{object_id}`.

#### `PATCH /blobs/{object_id}/metadata` (authenticated)

Updates a blob's content type.

Request:

```json
{
  "content_type": "image/png"
}
```

Response `200 OK`: Returns the updated blob record.

#### `DELETE /blobs/{object_id}` (authenticated)

Deletes a blob. The blob must belong to the authenticated account. If the blob is stored on Walrus,
a `delete_blob` transaction is built and submitted via Pearl. Underlying data is deleted only if no
other blob records reference the same `blob_id`.

Response `204 No Content`.

### 6.8 Metrics

#### `GET /metrics`

Returns Prometheus-format metrics. See Section 11 for the full metric catalog.

### 6.9 Pagination

List endpoints use cursor-based (keyset) pagination.

Query parameters:

- `cursor` (string, optional): Opaque pagination token from a previous response's `next_cursor`.
- `limit` (integer, optional): Maximum number of items to return. Default: 20. Maximum: 100.

Cursor encoding: Base64url of `<created_at>|<id>`. The cursor value is opaque to clients; its
internal format is an implementation detail.

Response shape:

```json
{
  "data": [...],
  "next_cursor": "opaque_token_or_null"
}
```

`next_cursor` is `null` when there are no more results.

## 7. Pearl gRPC API Specification

### 7.1 Service Definition

```protobuf
syntax = "proto3";
package pearl;

service Pearl {
  rpc GetAddress(GetAddressRequest) returns (GetAddressResponse);
  rpc SignTransaction(SignTransactionRequest) returns (SignTransactionResponse);
}

message GetAddressRequest {
  string account_id = 1;
}

message GetAddressResponse {
  string address = 1;
}

message SignTransactionRequest {
  bytes tx_data = 1;
  string account_id = 2;
}

message SignTransactionResponse {
  bytes signed_transaction = 1;
}
```

### 7.2 `GetAddress`

Derives the Ed25519 public key for the given `account_id` and returns its Sui address as a
hex-encoded string.

This is a pure derivation with no side effects. The same `account_id` always returns the same
address.

### 7.3 `SignTransaction`

Signs a Sui transaction on behalf of an account.

Input:

- `tx_data`: BCS-encoded `TransactionData`.
- `account_id`: Account whose derived key should sign.

Processing:

1. Derive the Ed25519 keypair for `account_id` (Section 8).
2. Deserialize `tx_data` as BCS-encoded `TransactionData`.
3. Wrap in `IntentMessage` with `Intent::sui_transaction()`.
4. Sign with `Signature::new_secure()`.
5. Construct `Transaction` containing the original data and signature.
6. Serialize to BCS bytes.

Output:

- `signed_transaction`: BCS-encoded signed `Transaction`.

Error codes:

- `3 (InvalidArgument)`: `tx_data` cannot be deserialized as valid `TransactionData`.
- `13 (Internal)`: Key derivation or signing failure.
- `16 (Unauthenticated)`: Invalid service secret.

### 7.4 Health Check

Pearl implements the standard gRPC Health Check Protocol. The health endpoint is unauthenticated.

## 8. Key Derivation (Pearl)

Pearl derives per-account Ed25519 keypairs deterministically from a master seed. No private key
material is ever written to disk or persisted in a database.

### 8.1 Master Seed

- Provided via `PEARL_MASTER_SEED` environment variable or `--pearl-master-seed-file` CLI flag.
- Hex-encoded byte string.
- Minimum length: 32 bytes (64 hex characters).
- Held in memory using `Zeroizing<Vec<u8>>` to clear on drop.

### 8.2 Derivation Algorithm

```
salt  = b"pearl-key-derivation-v1"
hkdf  = HKDF-SHA256(salt, master_seed)
okm   = hkdf.expand(account_id.as_bytes(), 32)
key   = Ed25519::from_bytes(okm)
```

Properties:

- **Deterministic**: Same `(master_seed, account_id)` always yields the same keypair.
- **Isolated**: Different account IDs produce unrelated keys.
- **Stateless**: No nonces, counters, or stored state.
- **Zeroized**: Output key material is wrapped in `Zeroizing` and cleared when dropped.

### 8.3 Address Derivation

The Sui address is derived from the Ed25519 public key using Sui's standard address derivation
(Blake2b-256 hash of the flag byte concatenated with the public key bytes).

## 9. Blob Storage Backends

The `BlobStore` trait defines the interface for blob persistence:

```rust
trait BlobStore: Send + Sync + 'static {
    fn store(&self, data: &[u8], account_id: Option<&str>)
        -> Result<StoreResult, BlobStoreError>;
    fn read(&self, blob_id: &BlobId) -> Result<Vec<u8>, BlobStoreError>;
    fn delete(&self, blob_id: &BlobId, sui_object_id: Option<&str>,
              account_id: Option<&str>) -> Result<(), BlobStoreError>;
    fn exists(&self, blob_id: &BlobId) -> Result<bool, BlobStoreError>;
}

struct StoreResult {
    blob_id: BlobId,
    sui_object_id: Option<String>,
}
```

The backend is selected at startup based on configuration: if `PEARL_GRPC_URL`, `SUI_RPC_URL`,
`WALRUS_AGGREGATOR_URL`, `WALRUS_SYSTEM_OBJECT`, and `WALRUS_STAKING_OBJECT` are all set, the
Walrus backend is used. Otherwise, the local backend is used.

### 9.1 Local Blob Store

For development. Stores blobs on the local filesystem.

Storage path: `<BLOB_STORE_PATH>/<prefix>/<blob_id>`, where `prefix` is the first 2 characters of
the blob ID (directory sharding).

Blob ID: Blake2s-256 hash of the blob data, hex-encoded.

Content-addressed: Identical data produces the same blob ID and is stored once.

`sui_object_id`: Always `None`.

### 9.2 Walrus Blob Store

For production. Stores blobs on the Walrus decentralized storage network with on-chain state on Sui.

#### Store Flow

1. Encode blob data using Walrus RS2 erasure coding.
2. Build a Sui Programmable Transaction Block (PTB):
   - `reserve_space` on the Walrus system object.
   - `register_blob` with the encoded metadata.
3. Sign the PTB via Pearl gRPC (`SignTransaction`).
4. Submit the signed transaction to Sui RPC.
5. Upload encoded slivers to Walrus storage nodes.
6. Collect a storage certificate from the network.
7. Build a second PTB: `certify_blob` with the certificate.
8. Sign and submit the certification transaction via Pearl.

Result: `blob_id` (Walrus-assigned content identifier) and `sui_object_id` (on-chain object
address).

Default storage duration: `WALRUS_DEFAULT_EPOCHS` epochs (default 5).

#### Read Flow

Fetch blob data from the Walrus aggregator HTTP endpoint using the blob ID.

#### Delete Flow

Build and submit a `delete_blob` PTB via Pearl to remove the on-chain object. Only executed if no
other blob records reference the same `blob_id`.

### 9.3 Blob Store Errors

```rust
enum BlobStoreError {
    NotFound(String),
    Io(std::io::Error),
    Http(String),
}
```

`BlobStoreError` is mapped to `AppError::BlobStore` and returns `500 Internal Server Error` in the
HTTP response, except `NotFound` which returns `404`.

## 10. Extension Task (Blob Lease Renewal)

The extension task is a background loop that automatically renews expiring blob storage leases on
Walrus.

### 10.1 Configuration

| Parameter | Default | Environment Variable |
|-----------|---------|----------------------|
| Check interval | 3600s (1 hour) | `BLOB_EXTEND_INTERVAL_SECS` |
| Lookahead window | 7 days | `BLOB_EXTEND_LOOKAHEAD_DAYS` |
| Extension amount | 5 epochs | `BLOB_EXTEND_EPOCHS` |
| Metrics bind address | `0.0.0.0:50053` | `OYSTER_EXTENSION_METRICS_BIND_ADDR` |
| Webhook URL | None (disabled) | `FUND_MANAGER_WEBHOOK_URL` |

### 10.2 Execution

Invoked via `oysterd extend`. Requires all Walrus integration environment variables.

### 10.3 Cycle Algorithm

Each cycle:

1. Query the database for all blobs where `expires_at` is within the lookahead window and
   `sui_object_id IS NOT NULL`.
2. For each qualifying blob:
   a. Resolve the account ID to a Sui address via Pearl `GetAddress` (cached per cycle).
   b. Build an `extend_blob` PTB on the Walrus system object.
   c. Sign the PTB via Pearl gRPC.
   d. Submit the signed transaction to Sui RPC.
   e. Update `expires_at` in the database to reflect the new lease end.
3. Record metrics (blobs processed, errors by stage, cycle duration).
4. If any error indicates insufficient on-chain funds, invoke the fund manager webhook.
5. Sleep for `check_interval` and repeat.

### 10.4 Insufficient Funds Webhook

Triggered when a transaction error message contains `"insufficientgas"` or
`"insufficientcoinbalance"` (case-insensitive).

Payload (`POST` to `FUND_MANAGER_WEBHOOK_URL`):

```json
{
  "account_id": "account_id",
  "address": "0x...",
  "error": "original error message"
}
```

Retry policy: Up to 3 attempts with exponential backoff.

Circuit breaker: Opens after 5 consecutive failures. Resets after 60 seconds of cooldown. While
open, webhook calls are skipped.

## 11. Observability

### 11.1 Oyster HTTP Metrics

| Metric | Type | Labels |
|--------|------|--------|
| `oyster_http_requests_total` | counter | method, path, status |
| `oyster_http_request_duration_seconds` | histogram | method, path |

### 11.2 Blob Store Metrics

| Metric | Type | Labels |
|--------|------|--------|
| `oyster_blob_store_operations_total` | counter | operation, result |

### 11.3 Pearl gRPC Client Metrics (from Oyster)

| Metric | Type | Labels |
|--------|------|--------|
| `oyster_pearl_grpc_calls_total` | counter | method, result |
| `oyster_pearl_grpc_latency_seconds` | histogram | method |

### 11.4 Active Resource Gauges

| Metric | Type | Labels |
|--------|------|--------|
| `oyster_active_accounts` | gauge | (none) |
| `oyster_active_blobs` | gauge | (none) |

Refreshed from the database on each Prometheus scrape.

### 11.5 Extension Worker Metrics

| Metric | Type | Labels |
|--------|------|--------|
| `oyster_extension_cycles_total` | counter | (none) |
| `oyster_extension_blobs_extended_total` | counter | (none) |
| `oyster_extension_errors_total` | counter | stage |
| `oyster_extension_cycle_duration_seconds` | histogram | (none) |
| `oyster_extension_blobs_expiring` | gauge | (none) |
| `oyster_extension_cycle_blobs_processed` | gauge | (none) |

Error `stage` values: `resolve_address`, `extend_blob`, `db_update`.

Extension metrics are served on a separate HTTP endpoint at `OYSTER_EXTENSION_METRICS_BIND_ADDR`
(default `:50053`).

### 11.6 Webhook Metrics

| Metric | Type | Labels |
|--------|------|--------|
| `oyster_webhook_attempts_total` | counter | (none) |
| `oyster_webhook_successes_total` | counter | (none) |
| `oyster_webhook_failures_total` | counter | (none) |
| `oyster_webhook_circuit_open_total` | counter | (none) |

### 11.7 Pearl Server Metrics

| Metric | Type | Labels |
|--------|------|--------|
| `pearl_grpc_requests_total` | counter | method, status |
| `pearl_grpc_request_duration_seconds` | histogram | method |
| `pearl_sign_transactions_total` | counter | result |

Served at `PEARL_METRICS_BIND_ADDR` (default `:50052`).

### 11.8 Metric Endpoints Summary

| Service | Endpoint | Default Address |
|---------|----------|-----------------|
| Oyster HTTP | `/metrics` | `:3000` |
| Extension worker | `/metrics` | `:50053` |
| Pearl | `/metrics` | `:50052` |

### 11.9 Structured Logging

Both services emit structured logs via `tracing`. Log output uses the default `tracing_subscriber`
format. Key events logged:

- Server startup and bind address.
- Each HTTP request (method, path, status, duration).
- Pearl gRPC calls (method, result, latency).
- Extension cycle start/end, per-blob extension results.
- Webhook attempts and circuit breaker state transitions.
- Authentication failures (without leaking key material).

## 12. Error Taxonomy

### 12.1 Oyster HTTP Errors

```rust
enum AppError {
    NotFound,                  // 404 Not Found
    Unauthorized,              // 401 Unauthorized
    BadRequest(String),        // 400 Bad Request
    Conflict(String),          // 409 Conflict
    NotImplemented,            // 501 Not Implemented
    PayloadTooLarge,           // 413 Payload Too Large
    Internal(String),          // 500 Internal Server Error
    Database(sqlx::Error),     // 500 Internal Server Error
    BlobStore(BlobStoreError), // 500 (or 404 for BlobStoreError::NotFound)
}
```

All error responses use the JSON format `{"error": "message"}`. Internal error details (database
errors, stack traces) are not exposed to clients; a generic message is returned and details are
logged server-side.

### 12.2 Pearl gRPC Errors

```rust
enum Error {
    InvalidPrivateKey(String),
    InvalidTransactionData(String),
    SigningError(String),
    DerivationError(String),
}
```

Mapping to gRPC status codes:

| Error Variant | gRPC Code |
|---------------|-----------|
| `InvalidTransactionData` | `3 (InvalidArgument)` |
| `InvalidPrivateKey` | `13 (Internal)` |
| `SigningError` | `13 (Internal)` |
| `DerivationError` | `13 (Internal)` |

Authentication failures (invalid service secret) return `16 (Unauthenticated)` before reaching
the service implementation.

### 12.3 Blob Store Errors

| Variant | Description | HTTP Mapping |
|---------|-------------|--------------|
| `NotFound(String)` | Blob data not found in backend | 404 |
| `Io(std::io::Error)` | Filesystem I/O error (local store) | 500 |
| `Http(String)` | HTTP error from Walrus aggregator or Sui RPC | 500 |

## 13. Configuration Specification

### 13.1 Oyster Configuration

All configuration is via environment variables. Secrets may alternatively be loaded from files via
CLI flags.

| Variable | Type | Default | Required | Description |
|----------|------|---------|----------|-------------|
| `BIND_ADDR` | string | `0.0.0.0:3000` | No | HTTP listen address |
| `DATABASE_URL` | string | `sqlite:oyster.db?mode=rwc` | No | SQLite or PostgreSQL URL |
| `BLOB_STORE_PATH` | path | `blob_store` | No | Local blob store directory |
| `ENABLE_DEBUG` | bool | `false` | No | Enable `/debug/*` endpoints |
| `PEARL_GRPC_URL` | string | (none) | For Walrus | Pearl gRPC endpoint URL |
| `PEARL_SERVICE_SECRET` | string | (none) | Yes | Shared secret for Pearl auth |
| `WALRUS_AGGREGATOR_URL` | string | (none) | For Walrus | Walrus aggregator HTTP URL |
| `WALRUS_DEFAULT_EPOCHS` | u32 | `5` | No | Default storage duration in epochs |
| `SUI_RPC_URL` | string | (none) | For Walrus | Sui RPC endpoint |
| `WALRUS_SYSTEM_OBJECT` | string | (none) | For Walrus | Walrus system Sui object ID |
| `WALRUS_STAKING_OBJECT` | string | (none) | For Walrus | Walrus staking Sui object ID |
| `BLOB_EXTEND_INTERVAL_SECS` | u64 | `3600` | No | Extension check interval |
| `BLOB_EXTEND_LOOKAHEAD_DAYS` | u32 | `7` | No | Expiry lookahead window |
| `BLOB_EXTEND_EPOCHS` | u32 | `5` | No | Epochs to extend per renewal |
| `OYSTER_EXTENSION_METRICS_BIND_ADDR` | string | `0.0.0.0:50053` | No | Extension metrics port |
| `FUND_MANAGER_WEBHOOK_URL` | string | (none) | No | Webhook for insufficient funds |

CLI secret flags:

- `--pearl-service-secret-file <path>`: Load `PEARL_SERVICE_SECRET` from a file.

### 13.2 Pearl Configuration

| Variable | Type | Default | Required | Description |
|----------|------|---------|----------|-------------|
| `PEARL_BIND_ADDR` | string | `0.0.0.0:50051` | No | gRPC listen address |
| `PEARL_SERVICE_SECRET` | string | (none) | Yes | Shared auth secret |
| `PEARL_MASTER_SEED` | hex string | (none) | Yes | Master seed (>=32 bytes) |
| `PEARL_TLS_CERT_PATH` | path | (none) | No | TLS certificate (PEM) |
| `PEARL_TLS_KEY_PATH` | path | (none) | No | TLS private key (PEM) |
| `PEARL_METRICS_BIND_ADDR` | string | `0.0.0.0:50052` | No | Prometheus metrics port |

CLI secret flags:

- `--pearl-service-secret-file <path>`: Load `PEARL_SERVICE_SECRET` from a file.
- `--pearl-master-seed-file <path>`: Load `PEARL_MASTER_SEED` from a file.

TLS constraint: `PEARL_TLS_CERT_PATH` and `PEARL_TLS_KEY_PATH` must both be set or both be unset.
Setting only one is a startup error.

### 13.3 Database Backend Detection

The database backend is determined at runtime from the connection URL:

- URLs starting with `sqlite:` use SQLite.
- URLs starting with `postgres://` or `postgresql://` use PostgreSQL.

SQLite pragmas applied at connection:

- `PRAGMA journal_mode=WAL` (write-ahead logging for concurrent reads).
- `PRAGMA foreign_keys=ON` (enforce foreign key constraints).

Migrations are stored separately:

- SQLite: `migrations/sqlite/`
- PostgreSQL: `migrations/postgres/`

### 13.4 Walrus Backend Activation

The Walrus blob store is activated when all of the following are set: `PEARL_GRPC_URL`,
`SUI_RPC_URL`, `WALRUS_AGGREGATOR_URL`, `WALRUS_SYSTEM_OBJECT`, `WALRUS_STAKING_OBJECT`. If any is
missing, the local filesystem blob store is used.

## 14. CLI Specification

### 14.1 Commands

| Command | Description |
|---------|-------------|
| `oyster store <file> --bucket <name> [--content-type <type>]` | Upload a file |
| `oyster read <object_id> [-o <output_file>]` | Download a blob |
| `oyster delete <object_id>` | Delete a blob |
| `oyster list-blobs --bucket <name> [--limit N]` | List blobs in a bucket |
| `oyster create-bucket <name>` | Create a bucket |
| `oyster list-buckets [--limit N]` | List buckets |
| `oyster delete-bucket <name>` | Delete a bucket |
| `oyster create-api-key` | Create a new API key |
| `oyster revoke-api-key <key_id>` | Revoke an API key |
| `oyster wallet` | Show wallet address |
| `oyster info` | Show server and account info |

### 14.2 Global Flags

| Flag | Description |
|------|-------------|
| `--config <path>` | Config file path |
| `--url <url>` | Oyster server URL |
| `--api-key <key>` | API key (overrides config file) |
| `--json` | Machine-readable JSON output |

### 14.3 Configuration File

Searched in order:

1. Path specified by `--config` flag.
2. `$XDG_CONFIG_HOME/oyster/client.yaml`
3. `~/.config/oyster/client.yaml`
4. `./client.yaml`

Format: YAML with `url` and `api_key` fields.

### 14.4 Content Type Detection

When `--content-type` is not specified for `store`, the CLI infers the MIME type from the file
extension (e.g., `.jpg` -> `image/jpeg`, `.txt` -> `text/plain`). Falls back to
`application/octet-stream` for unknown extensions.

## 15. Security Model

### 15.1 Secrets at Rest

- **API keys**: Stored as Blake2s-256 hashes. Raw keys exist only in memory during generation and
  are returned to the caller once.
- **Pearl service secret**: Loaded from environment variable or file. Held in memory; never written
  to the database.
- **Pearl master seed**: Loaded from environment variable or file. Held in `Zeroizing<Vec<u8>>`;
  cleared from memory on drop. Never written to the database.

### 15.2 Secrets in Transit

- **Oyster API keys**: Sent as Bearer tokens over HTTP. TLS termination is expected to be handled by
  a reverse proxy in production.
- **Pearl service secret**: Sent as gRPC metadata. Pearl supports native TLS via
  `PEARL_TLS_CERT_PATH` and `PEARL_TLS_KEY_PATH`.
- **Derived private keys**: Never leave the Pearl process. Only signed transaction bytes are returned
  over gRPC.

### 15.3 Cryptographic Primitives

| Purpose | Algorithm | Library |
|---------|-----------|---------|
| API key hashing | Blake2s-256 | `blake2` |
| Key derivation | HKDF-SHA256 | `hkdf`, `sha2` |
| Transaction signing | Ed25519 | `fastcrypto` (via `sui-types`) |
| Secret comparison | Constant-time equality | `subtle` |
| Memory zeroization | Zeroize on drop | `zeroize` |

### 15.4 Production Hardening Recommendations

- Terminate TLS at a reverse proxy in front of Oyster.
- Enable TLS on Pearl gRPC (`PEARL_TLS_CERT_PATH`, `PEARL_TLS_KEY_PATH`).
- Load secrets from a secrets manager (AWS Secrets Manager, HashiCorp Vault, Kubernetes Secrets)
  using the `--*-file` CLI flags.
- Run Oyster and Pearl in a private network; do not expose Pearl's gRPC port publicly.
- Use PostgreSQL in production for durability and concurrent access.

## 16. Database Schema

### 16.1 Oyster Schema

```sql
CREATE TABLE accounts (
    id TEXT PRIMARY KEY NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE api_keys (
    id TEXT PRIMARY KEY NOT NULL,
    account_id TEXT NOT NULL REFERENCES accounts(id),
    key_hash TEXT NOT NULL,
    prefix TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    revoked_at TEXT
);
CREATE INDEX idx_api_keys_key_hash ON api_keys(key_hash);
CREATE INDEX idx_api_keys_account_id ON api_keys(account_id);

CREATE TABLE buckets (
    id TEXT PRIMARY KEY NOT NULL,
    account_id TEXT NOT NULL REFERENCES accounts(id),
    name TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(account_id, name)
);
CREATE INDEX idx_buckets_account_id ON buckets(account_id);

CREATE TABLE blobs (
    object_id TEXT PRIMARY KEY NOT NULL,
    blob_id TEXT NOT NULL,
    bucket_id TEXT NOT NULL REFERENCES buckets(id),
    account_id TEXT NOT NULL REFERENCES accounts(id),
    content_type TEXT NOT NULL DEFAULT 'application/octet-stream',
    size INTEGER NOT NULL,
    sui_object_id TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    expires_at TEXT
);
CREATE INDEX idx_blobs_bucket_id ON blobs(bucket_id);
CREATE INDEX idx_blobs_blob_id ON blobs(blob_id);
CREATE INDEX idx_blobs_account_id ON blobs(account_id);
CREATE INDEX idx_blobs_expires_at ON blobs(expires_at);
```

PostgreSQL differences:

- `blobs.size` is `BIGINT` instead of `INTEGER`.
- Default timestamps use `to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI:SS')`.

### 16.2 Pearl Schema

Pearl has no database tables. All state is derived from the master seed and request parameters at
call time.

## 17. Deployment

### 17.1 Docker Images

#### Oyster (`Dockerfile.oysterd`)

- Build: Multi-stage from `rust:1.87-bookworm`.
- Runtime: `debian:bookworm-slim`.
- Binary: `/usr/local/bin/oysterd`.
- User: `oyster` (UID 10001).
- Port: 3000.
- Default entrypoint: `oysterd serve`.

#### Pearl (`Dockerfile.pearl`)

- Build: Multi-stage from `rust:1.87-bookworm`.
- Runtime: `debian:bookworm-slim`.
- Binary: `/usr/local/bin/pearl`.
- User: `pearl` (UID 10002).
- Ports: 50051 (gRPC), 50052 (metrics).
- Default entrypoint: `pearl`.

### 17.2 Service Commands

Oyster uses subcommands:

- `oysterd serve` -- Start the HTTP server.
- `oysterd extend` -- Start the extension worker (blob lease renewal loop).

Pearl has a single mode: start the gRPC server.

### 17.3 Local Development

- `chk` -- Format (`cargo fmt`) and lint (`cargo clippy --fix`).
- `cargo test -p pearl` -- Pearl unit and integration tests.
- `cargo test -p oyster` -- Oyster unit and integration tests.
- `cargo test -p oyster-e2e-tests` -- Full-stack E2E tests (boots Sui + Walrus in-process).
- `./scripts/local-testbed.sh` -- Start a local Walrus testbed for manual testing.

## 18. Testing

### 18.1 Unit Tests

In-module `#[cfg(test)]` blocks covering:

- API key hashing and prefix extraction.
- Key derivation determinism and isolation.
- Pagination cursor encoding and decoding.
- Webhook circuit breaker state transitions.
- Database CRUD operations (via `test_pool()` in-memory SQLite helper).

### 18.2 Integration Tests

- **Pearl**: `start_server()` spins up an in-process gRPC server on a random port.
  `authenticated()` injects the Bearer token for test requests.
- **Oyster**: `test_app()` creates a full Axum router backed by in-memory SQLite and a
  `SpyBlobStore` that records store/delete calls without persisting data.

### 18.3 End-to-End Tests

The `oyster-e2e-tests` crate boots the full stack in-process:

1. Start a local Sui cluster.
2. Start a local Walrus network.
3. Start Pearl gRPC server.
4. Start Oyster HTTP server.
5. Run end-to-end workflows (account creation, blob upload/read/delete, bucket operations).

Startup time: approximately 10-30 seconds. No external testbed required.

## 19. Build Requirements

- **Rust edition**: 2024.
- **Minimum Rust version**: 1.87+.
- **Protocol compiler**: `protoc` (for proto3 compilation).
- **Crypto provider**: `aws-lc-rs`.
- **Workspace lints**: `missing_docs = "deny"` (all public APIs must have doc comments).
