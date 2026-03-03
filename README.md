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
wallet custody and transaction signing in isolation.

## Architecture

```
                         +---------------------+
                         |     Oyster (HTTP)    |
                         |    :3000 (Axum)      |
                         |                      |
                         |  Buckets / Blobs     |
                         |  API Keys / Auth     |
                         |  Extension Task      |
                         +----------+-----------+
                                    |
                  +---------+-------+--------+-----------+
                  |         |                |           |
                  v         v                v           v
            +---------+ +--------+    +------------+ +--------+
            | SQLite  | | Walrus |    |   Pearl    | |  Sui   |
            | oyster  | | Nodes  |    |  (gRPC)    | |  RPC   |
            | .db     | |        |    |   :50051   | |        |
            +---------+ +--------+    +-----+------+ +--------+
                                            |
                                      +-----+------+
                                      |   SQLite   |
                                      |  pearl.db  |
                                      +------------+
```

### Crates

| Crate | Type | Purpose |
|-------|------|---------|
| `crates/oyster` | HTTP server (lib + bin) | Object storage API, blob store abstraction, extension task |
| `crates/pearl` | gRPC server (lib + bin) | Wallet custody, transaction signing, balance tracking |
| `crates/oyster-cli` | CLI binary | Command-line client for the Oyster HTTP API |
| `crates/oyster-e2e-tests` | Test crate | Full-stack E2E tests (Sui + Walrus + Pearl + Oyster) |

### How the pieces fit together

1. **Users** interact with Oyster's HTTP API to store and retrieve blobs.
2. **Oyster** manages buckets, blob metadata, API keys, and content-addressed deduplication in
   its own SQLite database.
3. When configured for Walrus, Oyster encodes blob data with the Walrus SDK, builds Sui
   Programmable Transaction Blocks (PTBs) for `reserve_space`, `register_blob`, and
   `certify_blob`, then delegates signing to Pearl.
4. **Pearl** holds Ed25519 private keys in its own SQLite database, signs transactions on
   request, tracks cached on-chain balances, and manages a pending-transaction lifecycle for
   cost accounting.
5. A background **extension task** in Oyster monitors blob expiry and extends storage on-chain.
6. A background **reconciliation task** in Pearl periodically queries Sui RPC for actual
   on-chain balances and times out stale pending transactions.

---

## Oyster (HTTP API)

Oyster is an Axum-based HTTP server with OpenAPI documentation served at `/docs`.

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
| `GET` | `/account/wallets` | Yes | List Pearl wallet addresses |
| `POST` | `/buckets` | Yes | Create bucket |
| `GET` | `/buckets` | Yes | List buckets (paginated) |
| `DELETE` | `/buckets/{bucket_id}` | Yes | Delete bucket (cascades blobs) |
| `PUT` | `/buckets/{bucket_id}/blobs` | Yes | Upload blob |
| `GET` | `/buckets/{bucket_id}/blobs` | Yes | List blobs in bucket (paginated) |
| `GET` | `/blobs/{object_id}` | No | Read blob by object ID |
| `GET` | `/blobs/by-blob-id/{blob_id}` | No | Read blob by content hash |
| `PATCH` | `/blobs/{object_id}/metadata` | Yes | Update content type / auto-extend |
| `DELETE` | `/blobs/{object_id}` | Yes | Delete blob |

Pagination is cursor-based. Pass `?limit=N&cursor=TOKEN` to paginate; the response includes
`next_cursor` when more pages exist.

### Blob store implementations

Oyster selects a blob store at startup based on environment variables:

| Implementation | When selected | On-chain? | Signing? |
|---|---|---|---|
| `LocalBlobStore` | No Walrus config | No | No |
| `WalrusBlobStore` | `WALRUS_PUBLISHER_URL` + `WALRUS_AGGREGATOR_URL` | Publisher handles it | No |
| `DirectWalrusBlobStore` | Pearl + Sui RPC + Walrus system/staking objects | Yes (PTBs) | Via Pearl |

`DirectWalrusBlobStore` is the production path. It:

1. Encodes the blob with the Walrus encoding scheme.
2. Builds a PTB (`reserve_space` + `register_blob`) and submits via Pearl.
3. Uploads slivers to Walrus storage nodes and collects a certificate.
4. Builds a `certify_blob` PTB and submits via Pearl.
5. Returns both the content-addressed `blob_id` and the Sui `sui_object_id`.

### Extension task

When Walrus integration is active, a background task runs on a configurable interval:

1. Queries the Oyster database for blobs expiring within a lookahead window.
2. Checks the Pearl wallet balance -- skips the cycle if SUI or WAL is below the configured
   minimum.
3. For each expiring blob, builds an `extend_blob` PTB, signs via Pearl, and submits to Sui.
4. Updates the `expires_at` timestamp in the Oyster database.

### Database

SQLite with WAL journal mode. Migrations in `crates/oyster/migrations/`:

- `001_initial.sql` -- accounts, api_keys, buckets, blobs
- `002_add_sui_object_id.sql` -- adds `sui_object_id` and expiry tracking to blobs
- `003_add_pearl_account_id.sql` -- links Oyster accounts to Pearl accounts

### Configuration

All configuration is via environment variables.

| Variable | Default | Description |
|----------|---------|-------------|
| `BIND_ADDR` | `0.0.0.0:3000` | HTTP listen address |
| `DATABASE_URL` | `sqlite:oyster.db?mode=rwc` | SQLite connection string |
| `BLOB_STORE_PATH` | `blob_store` | Path for LocalBlobStore |
| `ENABLE_DEBUG` | `true` | Enable `/debug/*` endpoints |
| `PEARL_GRPC_URL` | -- | Pearl gRPC address (e.g. `http://127.0.0.1:50051`) |
| `PEARL_SERVICE_SECRET` | `dev-secret` | Shared secret for Pearl auth |
| `PEARL_ACCOUNT_ID` | -- | Default Pearl account for operator transactions |
| `WALRUS_PUBLISHER_URL` | -- | Walrus publisher HTTP URL |
| `WALRUS_AGGREGATOR_URL` | -- | Walrus aggregator HTTP URL |
| `WALRUS_DEFAULT_EPOCHS` | `5` | Storage epochs for new blobs |
| `SUI_RPC_URL` | -- | Sui RPC endpoint |
| `WALRUS_SYSTEM_OBJECT` | -- | Walrus system object ID on Sui |
| `WALRUS_STAKING_OBJECT` | -- | Walrus staking object ID on Sui |
| `BLOB_EXTEND_INTERVAL_SECS` | `3600` | Extension task check interval |
| `BLOB_EXTEND_LOOKAHEAD_DAYS` | `7` | How far ahead to look for expiring blobs |
| `BLOB_EXTEND_EPOCHS` | `5` | Epochs to extend by |

---

## Pearl (gRPC wallet service)

Pearl is a tonic-based gRPC service that manages Sui Ed25519 keypairs and signs transactions.
It is intentionally isolated from business logic -- it only knows about wallets, keys, and
balances.

### gRPC API

Defined in `crates/pearl/proto/pearl.proto`:

| RPC | Description |
|-----|-------------|
| `CreateAccount` | Generate keypair, store in DB, return account ID + Sui address |
| `GetAccountWallets` | Return wallet info (address, balance thresholds) for an account |
| `SignTransaction` | Sign BCS-encoded `TransactionData`, create pending transaction record |
| `GetBalance` | Return cached SUI/WAL balances and minimum thresholds |
| `ConfirmTransaction` | Report transaction outcome, adjust cached balance |

Authentication: all RPCs require a `Bearer {secret}` in the `authorization` gRPC metadata
header.

### Signing flow

```
  Oyster                         Pearl                         Sui RPC
    |                              |                              |
    |-- SignTransaction(tx_data) ->|                              |
    |   (estimated_sui_cost,       |                              |
    |    estimated_wal_cost)       |-- sign with Ed25519 key      |
    |                              |-- INSERT pending_tx           |
    |                              |-- UPDATE cached balance -= est|
    |<- signed_tx, pending_tx_id --|                              |
    |                              |                              |
    |-- execute_transaction_block -------------------------------->|
    |<- SuiTransactionBlockResponse -------------------------------|
    |                              |                              |
    |-- ConfirmTransaction ------->|                              |
    |   (pending_tx_id, digest,    |-- UPDATE pending_tx status   |
    |    success, actual costs)    |-- correct cached balance     |
    |<- updated cached balances ---|                              |
```

If confirmation fails (Pearl unreachable), the reconciliation task will eventually correct the
balance by querying on-chain state and timing out stale pending transactions.

### Balance tracking

Pearl caches SUI and WAL balances per account with an optimistic deduction model:

- **On sign:** Estimated cost is deducted from the cached balance. A `pending_transaction`
  record is created.
- **On confirm (success):** The difference between estimated and actual cost is corrected.
- **On confirm (failure):** The full estimated cost is refunded.
- **On timeout:** Stale pending transactions (default 30 min) are refunded by the
  reconciliation task.
- **Reconciliation:** A background task periodically picks a random account, queries on-chain
  balances via Sui RPC, and overwrites the cache.

The cached balance can go negative (best-effort tracking). Oyster uses `GetBalance` to check
minimums before extension cycles but proceeds on failure.

### Reconciliation task

Enabled when `SUI_RPC_URL` is set. Runs on a configurable interval:

1. Pick a random account.
2. Query on-chain SUI balance (and WAL balance if `WAL_COIN_TYPE` is configured).
3. Update cached balance in the database.
4. Find and refund pending transactions older than the timeout threshold.

### Database

SQLite with WAL journal mode. Migrations in `crates/pearl/migrations/`:

- `001_initial.sql` -- accounts table with keypair storage
- `002_balance_tracking.sql` -- cached balances on accounts, pending_transactions table

### Configuration

| Variable | Default | Description |
|----------|---------|-------------|
| `PEARL_DATABASE_URL` | `sqlite:pearl.db?mode=rwc` | SQLite connection string |
| `PEARL_BIND_ADDR` | `0.0.0.0:50051` | gRPC listen address |
| `PEARL_SERVICE_SECRET` | `dev-secret` | Shared secret for service auth |
| `SUI_RPC_URL` | -- | Sui RPC endpoint (enables reconciliation) |
| `WAL_COIN_TYPE` | -- | Fully-qualified WAL coin type for balance queries |
| `PEARL_RECONCILIATION_INTERVAL_SECS` | `300` | Reconciliation loop interval |
| `PEARL_PENDING_TX_TIMEOUT_MINUTES` | `30` | Timeout for unconfirmed pending transactions |

---

## Oyster CLI

A command-line client for the Oyster HTTP API. Install with `cargo install --path crates/oyster-cli`.

```
oyster store photo.jpg --bucket media
oyster read <object_id> -o photo.jpg
oyster list-blobs --bucket media
oyster create-bucket backups
oyster list-buckets
oyster wallets
```

Configuration is read from `./client.yaml`, `$XDG_CONFIG_HOME/oyster/client.yaml`, or
`~/.config/oyster/client.yaml`, with CLI flags as overrides. Pass `--json` for
machine-readable output.

---

## Quick start

### Prerequisites

- Rust (edition 2024)
- `protoc` (`brew install protobuf` on macOS)
- SQLite3

### Development (local blob store, no Walrus)

```bash
# Terminal 1: start Pearl
cargo run -p pearl

# Terminal 2: start Oyster
PEARL_GRPC_URL=http://127.0.0.1:50051 cargo run -p oyster

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

### Full stack (with Walrus)

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

# E2E tests (boots Sui + Walrus in-process, ~30s startup)
cargo test -p oyster-e2e-tests -- --ignored
```

---

## Security model

| Layer | Mechanism | Scope |
|-------|-----------|-------|
| Oyster API | Bearer API key (Blake2s-256 hashed) | Per-account |
| Pearl gRPC | Shared service secret | Service-to-service |
| Blob reads | Unauthenticated | Public |
| Private keys | Stored as BLOB in Pearl's SQLite | At rest (plaintext in dev) |

Production hardening (not yet implemented):
- Private key encryption at rest (KMS or column-level encryption)
- mTLS or service mesh auth for Pearl gRPC
- Rate limiting and abuse prevention
