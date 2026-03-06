# Oyster

> *"The time has come," the Walrus said,*
> *"To talk of many things:*
> *Of shoes -- and ships -- and sealing-wax --*
> *Of cabbages -- and kings --*
> *And why the sea is boiling hot --*
> *And whether pigs have wings."*

Decentralized object storage built on [Walrus](https://walrus.xyz/) and [Sui](https://sui.io/).

Oyster provides a Web2-friendly HTTP API (buckets, blobs, API keys) backed by Walrus for
decentralized blob storage and Sui for on-chain state. A companion service, **Pearl**, handles
deterministic key derivation and transaction signing in isolation.

## Architecture

```
                         +---------------------+
                         |     Oyster (HTTP)    |
                         |    :3000 (Axum)      |
                         |                      |
                         |  Buckets / Blobs     |
                         |  API Keys / Auth     |
                         |  Extension Worker    |
                         +----------+-----------+
                                    |
                  +---------+-------+--------+-----------+
                  |         |                |           |
                  v         v                v           v
            +---------+ +--------+    +------------+ +--------+
            |Database | | Walrus |    |   Pearl    | |  Sui   |
            |(SQLite  | | Nodes  |    |  (gRPC)    | |  RPC   |
            | or PG)  | |        |    |   :50051   | |        |
            +---------+ +--------+    +------------+ +--------+
```

### Crates

| Crate | Type | Purpose |
|-------|------|---------|
| `crates/oyster` | HTTP server (lib + bin) | Object storage API, blob store abstraction, extension worker |
| `crates/pearl` | gRPC server (lib + bin) | Wallet custody, transaction signing, deterministic key derivation |
| `crates/oyster-cli` | CLI binary | Command-line client for the Oyster HTTP API |
| `crates/oyster-e2e-tests` | Test crate | Full-stack E2E tests (Sui + Walrus + Pearl + Oyster) |

### How the pieces fit together

1. **Users** interact with Oyster's HTTP API to store and retrieve blobs.
2. **Oyster** manages buckets, blob metadata, API keys, and content-addressed deduplication in
   its database.
3. When configured for Walrus, Oyster encodes blob data with the Walrus SDK, builds Sui
   Programmable Transaction Blocks (PTBs) for `reserve_space`, `register_blob`, and
   `certify_blob`, then delegates signing to Pearl.
4. **Pearl** derives Ed25519 keys deterministically from a master seed via HKDF-SHA256 and signs
   transactions on request. It is fully stateless -- no database, no balance tracking.
5. A background **extension worker** (`oysterd extend`) monitors blob expiry and extends storage
   on-chain.

---

## Oyster (HTTP API)

Oyster is an Axum-based HTTP server with OpenAPI documentation served at `/docs`. The server
binary is `oysterd`, which supports two subcommands: `oysterd serve` (default) starts the HTTP
API, and `oysterd extend` runs the blob extension background worker.

### Data model

- **Account** -- Top-level identity. Has API keys and optionally a Pearl wallet
  (`pearl_account_id`).
- **API Key** -- Bearer token for authentication. Stored as a Blake2s-256 hash. Multiple keys
  per account.
- **Bucket** -- Named container scoped to an account. Bucket names are unique per account.
- **Blob** -- Content-addressed object stored in a bucket. Identified by `object_id` (unique
  per upload) and `blob_id` (content hash, shared across deduplicates). Optionally has a
  `sui_object_id` when stored on Walrus.

### API

| Method | Path | Auth | Description |
|--------|------|------|-------------|
| `POST` | `/debug/create-account` | No | Create account + first API key (dev only) |
| `POST` | `/account/api-keys` | Yes | Generate new API key |
| `DELETE` | `/account/api-keys/{key_id}` | Yes | Revoke API key |
| `GET` | `/account/wallet` | Yes | Get Pearl wallet address |
| `POST` | `/buckets` | Yes | Create bucket |
| `GET` | `/buckets` | Yes | List buckets (paginated) |
| `DELETE` | `/buckets/{bucket_id}` | Yes | Delete bucket (cascades blobs) |
| `PUT` | `/buckets/{bucket_id}/blobs` | Yes | Upload blob |
| `GET` | `/buckets/{bucket_id}/blobs` | Yes | List blobs in bucket (paginated) |
| `GET` | `/blobs/{object_id}` | No | Read blob by object ID |
| `GET` | `/blobs/by-blob-id/{blob_id}` | No | Read blob by content hash |
| `PATCH` | `/blobs/{object_id}/metadata` | Yes | Update content type |
| `DELETE` | `/blobs/{object_id}` | Yes | Delete blob |
| `GET` | `/health` | No | Liveness probe |
| `GET` | `/ready` | No | Readiness probe (checks DB and Pearl connectivity) |
| `GET` | `/metrics` | No | Prometheus metrics |

Pagination is cursor-based. Pass `?limit=N&cursor=TOKEN` to paginate; the response includes
`next_cursor` when more pages exist.

### Blob store implementations

Oyster selects a blob store at startup based on environment variables:

| Implementation | When selected | On-chain? | Signing? |
|---|---|---|---|
| `LocalBlobStore` | No Walrus config | No | No |
| `DirectWalrusBlobStore` | Pearl + Sui RPC + Walrus system/staking objects | Yes (PTBs) | Via Pearl |

`DirectWalrusBlobStore` is the production path. It:

1. Encodes the blob with the Walrus encoding scheme.
2. Builds a PTB (`reserve_space` + `register_blob`) and submits via Pearl.
3. Uploads slivers to Walrus storage nodes and collects a certificate.
4. Builds a `certify_blob` PTB and submits via Pearl.
5. Returns both the content-addressed `blob_id` and the Sui `sui_object_id`.

### Extension worker

Run as a separate process with `oysterd extend`. When Walrus integration is active, it runs on
a configurable interval:

1. Queries the Oyster database for blobs expiring within a lookahead window.
2. For each expiring blob, builds an `extend_blob` PTB, signs via Pearl, and submits to Sui.
3. Updates the `expires_at` timestamp in the Oyster database.

When extension failures indicate insufficient funds, an optional fund manager webhook
(`FUND_MANAGER_WEBHOOK_URL`) is notified with the account ID, wallet address, and error details.
The webhook client uses a circuit breaker to avoid repeated calls to a failing endpoint.

### Database

Supports SQLite and PostgreSQL via the SQLx Any driver; the backend is determined at runtime by
the `DATABASE_URL` connection string. Migrations are per backend under `crates/oyster/migrations/`:

- `migrations/sqlite/001_initial.sql`
- `migrations/postgres/001_initial.sql`

Tables: `accounts`, `api_keys`, `buckets`, `blobs`.

### Configuration

All configuration is via environment variables.

| Variable | Default | Description |
|----------|---------|-------------|
| `BIND_ADDR` | `0.0.0.0:3000` | HTTP listen address |
| `DATABASE_URL` | `sqlite:oyster.db?mode=rwc` | Database connection string (SQLite or PostgreSQL) |
| `BLOB_STORE_PATH` | `blob_store` | Path for LocalBlobStore |
| `ENABLE_DEBUG` | `false` | Enable `/debug/*` endpoints |
| `PEARL_GRPC_URL` | -- | Pearl gRPC address (e.g. `http://127.0.0.1:50051`) |
| `PEARL_SERVICE_SECRET` | -- | Shared secret for Pearl auth (**required**) |
| `WALRUS_AGGREGATOR_URL` | -- | Walrus aggregator HTTP URL |
| `WALRUS_DEFAULT_EPOCHS` | `5` | Storage epochs for new blobs |
| `SUI_RPC_URL` | -- | Sui RPC endpoint |
| `WALRUS_SYSTEM_OBJECT` | -- | Walrus system object ID on Sui |
| `WALRUS_STAKING_OBJECT` | -- | Walrus staking object ID on Sui |
| `BLOB_EXTEND_INTERVAL_SECS` | `3600` | Extension worker check interval |
| `BLOB_EXTEND_LOOKAHEAD_DAYS` | `7` | How far ahead to look for expiring blobs |
| `BLOB_EXTEND_EPOCHS` | `5` | Epochs to extend by |
| `OYSTER_EXTENSION_METRICS_BIND_ADDR` | `0.0.0.0:50053` | Metrics endpoint for the extension worker |
| `FUND_MANAGER_WEBHOOK_URL` | -- | Optional webhook URL for insufficient-funds notifications |

---

## Pearl (gRPC wallet service)

Pearl is a tonic-based gRPC service that derives Ed25519 keypairs deterministically from a
master seed and signs Sui transactions. It is fully stateless -- no database, no balance
tracking. It is intentionally isolated from business logic; it only knows about key derivation
and signing.

### gRPC API

Defined in `crates/pearl/proto/pearl.proto`:

| RPC | Description |
|-----|-------------|
| `GetAddress` | Derive the Sui address for an account ID |
| `SignTransaction` | Sign BCS-encoded `TransactionData`, return signed bytes |

Authentication: all RPCs require a `Bearer {secret}` in the `authorization` gRPC metadata
header.

### Signing flow

```
  Oyster                         Pearl                         Sui RPC
    |                              |                              |
    |-- SignTransaction(tx_data,   |                              |
    |      account_id) ---------->|                              |
    |                              |-- derive Ed25519 key         |
    |                              |   (HKDF-SHA256 from seed)   |
    |                              |-- sign tx_data               |
    |<-- signed_transaction ------|                              |
    |                              |                              |
    |-- execute_transaction_block -------------------------------->|
    |<-- SuiTransactionBlockResponse -------------------------------|
```

### Configuration

| Variable | Default | Description |
|----------|---------|-------------|
| `PEARL_BIND_ADDR` | `0.0.0.0:50051` | gRPC listen address |
| `PEARL_SERVICE_SECRET` | -- | Shared secret for service auth (**required**) |
| `PEARL_MASTER_SEED` | -- | Hex-encoded master seed for key derivation (**required**, >= 32 bytes) |
| `PEARL_METRICS_BIND_ADDR` | `0.0.0.0:50052` | Prometheus metrics endpoint |
| `PEARL_TLS_CERT_PATH` | -- | TLS certificate path (optional; must pair with key) |
| `PEARL_TLS_KEY_PATH` | -- | TLS private key path (optional; must pair with cert) |

---

## Oyster CLI

A command-line client for the Oyster HTTP API. Install with `cargo install --path crates/oyster-cli`.

```
oyster store photo.jpg --bucket media
oyster read <object_id> -o photo.jpg
oyster list-blobs --bucket media
oyster create-bucket backups
oyster list-buckets
oyster wallet
```

Configuration is read from `./client.yaml`, `$XDG_CONFIG_HOME/oyster/client.yaml`, or
`~/.config/oyster/client.yaml`, with CLI flags as overrides. Pass `--json` for
machine-readable output.

---

## Docker

Dockerfiles are provided for both services:

- `docker/Dockerfile.oyster` -- Builds the `oysterd` binary. Exposes port 3000.
- `docker/Dockerfile.pearl` -- Builds the `pearl` binary. Exposes ports 50051 (gRPC) and 50052 (metrics).

---

## Quick start

### Prerequisites

- Rust (edition 2024)
- `protoc` (`brew install protobuf` on macOS)

### Development (local blob store, no Walrus)

```bash
# Terminal 1: start Pearl
PEARL_MASTER_SEED=<hex-encoded-seed> \
PEARL_SERVICE_SECRET=<shared-secret> \
cargo run -p pearl

# Terminal 2: start Oyster
PEARL_GRPC_URL=http://127.0.0.1:50051 \
PEARL_SERVICE_SECRET=<shared-secret> \
cargo run -p oyster  # runs `oysterd serve` by default

# Terminal 3: use the API
curl -X POST http://localhost:3000/debug/create-account
# Returns { "account_id": "...", "api_key": { "secret": "..." } }

curl -X POST http://localhost:3000/buckets \
  -H "Authorization: Bearer <api_key>" \
  -H "Content-Type: application/json" \
  -d '{"name": "my-bucket"}'

curl -X PUT http://localhost:3000/buckets/<bucket_id>/blobs \
  -H "Authorization: Bearer <api_key>" \
  -H "Content-Type: text/plain" \
  -d 'hello world'

curl http://localhost:3000/blobs/<object_id>
```

To run the extension worker separately:

```bash
cargo run -p oyster -- extend
# or: oysterd extend
```

### Full stack (with an external Walrus network)

For manual full-stack development, use `scripts/local-testbed.sh` against an already-running
Walrus local testbed. This starts Pearl and Oyster in tmux sessions, creates and funds a test
account, and prints connection details.

### Running tests

```bash
# Format + lint (project-specific alias)
chk

# Unit and integration tests
cargo test -p pearl
cargo test -p oyster

# E2E tests (boots Sui + Walrus in-process, ~30s startup). These do not require an external testbed
# as they stand up their own in-process Sui and Walrus clusters.
cargo test -p oyster-e2e-tests

# When changing anything, it's always safe/best-practice to just test everything.
cargo test
```

---

## Security model

| Layer | Mechanism | Scope |
|-------|-----------|-------|
| Oyster API | Bearer API key (Blake2s-256 hashed) | Per-account |
| Pearl gRPC | Shared service secret | Service-to-service |
| Blob reads | Unauthenticated | Public |
| Private keys | Derived from master seed (HKDF-SHA256), in-memory only | Never stored at rest |

Production hardening:
- Secure `PEARL_MASTER_SEED` via a secret manager (e.g. AWS Secrets Manager, HashiCorp Vault)
- TLS for Pearl gRPC (supported via `PEARL_TLS_CERT_PATH` / `PEARL_TLS_KEY_PATH`)
- mTLS or service mesh auth for additional Pearl isolation
- Rate limiting and abuse prevention
