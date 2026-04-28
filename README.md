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

Oyster is an Axum-based HTTP server with OpenAPI documentation served at `/api/docs`. The server
binary is `oysterd`, which supports two subcommands: `oysterd serve` (default) starts the HTTP
API, and `oysterd extend` runs the blob extension background worker.

### Data model

- **Account** -- Top-level identity. Has API keys and a Pearl-derived wallet (keyed by
  account ID).
- **API Key** -- Bearer token for authentication. Stored as a Blake2s-256 hash. Multiple keys
  per account.
- **Bucket** -- Named container scoped to an account. Bucket names are unique per account.
- **Blob** -- Content-addressed object stored in a bucket. Identified by `object_id` (unique
  per upload) and `blob_id` (content hash, shared across deduplicates). When stored on Walrus,
  has a `pooled_blob_object_id` pointing at the `PooledBlob` on-chain object registered under
  the account's `StoragePool`.
- **Blob tags** -- User-defined key/value metadata attached to a blob. Up to 10 tags per blob,
  key ≤128 B, value ≤256 B, total ≤2 KiB; charset `[A-Za-z0-9 +\-=._:/@]`. Cascaded on blob
  delete. Compatible with the S3 `x-amz-tagging` header and the `PutObjectTagging` /
  `GetObjectTagging` / `DeleteObjectTagging` operations.
- **Storage pool** -- One `StoragePool` Sui object per account, created lazily on the first
  blob write. All of the account's `PooledBlob`s reserve capacity from and share the same
  expiration epoch as this pool.

### API

**Admin routes** (admin-key authenticated, for managing accounts and credentials):

| Method | Path | Auth | Description |
|--------|------|------|-------------|
| `POST` | `/api/v1/accounts` | Admin Key | Create account |
| `POST` | `/api/v1/accounts/{account_id}/api-keys` | Admin Key | Generate API key for account |
| `DELETE` | `/api/v1/accounts/{account_id}/api-keys/{key_id}` | Admin Key | Revoke API key |
| `POST` | `/api/v1/accounts/{account_id}/access-keys` | Admin Key | Create S3 access key |
| `GET` | `/api/v1/accounts/{account_id}/access-keys` | Admin Key | List S3 access keys |
| `DELETE` | `/api/v1/accounts/{account_id}/access-keys/{access_key_id}` | Admin Key | Revoke S3 access key |

**Data routes** (API key-authenticated):

| Method | Path | Auth | Description |
|--------|------|------|-------------|
| `GET` | `/api/v1/account/wallet` | API Key | Get Pearl wallet address |
| `POST` | `/api/v1/buckets` | API Key | Create bucket |
| `GET` | `/api/v1/buckets` | API Key | List buckets (paginated) |
| `DELETE` | `/api/v1/buckets/{bucket_name}` | API Key | Delete empty bucket (409 if non-empty) |
| `PUT` | `/api/v1/buckets/{bucket_name}/blobs/{key}` | API Key | Upload blob |
| `GET` | `/api/v1/buckets/{bucket_name}/blobs` | API Key | List blobs in bucket (paginated) |
| `GET` | `/api/v1/buckets/{bucket_name}/blobs/{key}` | No | Read blob by bucket and key |
| `GET` | `/api/v1/blobs/by-blob-id/{blob_id}` | No | Read blob by content hash |
| `PATCH` | `/api/v1/buckets/{bucket_name}/blobs/{key}/metadata` | API Key | Update content type |
| `DELETE` | `/api/v1/buckets/{bucket_name}/blobs/{key}` | API Key | Delete blob |
| `GET` | `/api/v1/buckets/{bucket_name}/blobs/{key}/tags` | API Key | List blob tags |
| `PUT` | `/api/v1/buckets/{bucket_name}/blobs/{key}/tags` | API Key | Replace all blob tags |
| `PATCH` | `/api/v1/buckets/{bucket_name}/blobs/{key}/tags` | API Key | Merge blob tags |
| `DELETE` | `/api/v1/buckets/{bucket_name}/blobs/{key}/tags` | API Key | Clear all blob tags |
| `PUT` | `/api/v1/buckets/{bucket_name}/blobs/{key}/tags/{tag_key}` | API Key | Upsert a single tag |
| `DELETE` | `/api/v1/buckets/{bucket_name}/blobs/{key}/tags/{tag_key}` | API Key | Delete a single tag |
| `GET` | `/health` | No | Liveness probe |
| `GET` | `/ready` | No | Readiness probe (checks DB and Pearl connectivity) |
| `GET` | `/metrics` | No | Prometheus metrics |

Admin keys are issued via the `oysterd app` CLI commands (see below). Pagination is cursor-based
-- pass `?limit=N&cursor=TOKEN` to paginate; the response includes `next_cursor` when more pages
exist. Blob endpoints support `If-Match` / `If-None-Match` conditional headers for cache
validation and safe concurrent writes.

### Blob store implementations

Oyster selects a blob store at startup based on environment variables:

| Implementation | When selected | On-chain? | Signing? |
|---|---|---|---|
| `LocalBlobStore` | No Walrus config | No | No |
| `DirectWalrusBlobStore` | Pearl + Sui RPC + Walrus system/staking objects | Yes (PTBs) | Via Pearl |

`DirectWalrusBlobStore` is the production path. It:

1. Encodes the blob with the Walrus encoding scheme.
2. On the account's first blob write, bundles `create_storage_pool` into the upload PTB and
   persists the resulting `StoragePool` `ObjectID` on the account row (lazy, first-writer wins).
3. Builds the upload PTB — optionally prepending `increase_storage_pool_capacity` (rounded up
   to Walrus's 1 MiB `BYTES_PER_UNIT_SIZE`) when the new blob would exceed the pool's remaining
   capacity — and calls `register_pooled_blobs`. Submits via Pearl.
4. Uploads slivers to Walrus storage nodes and collects a certificate.
5. Builds a `certify_pooled_blobs` PTB and submits via Pearl.
6. Returns the content-addressed `blob_id` and the `pooled_blob_object_id` of the registered
   `PooledBlob` Sui object.

Deletes are reference-counted: the on-chain `delete_pooled_blob` call fires only when the last
reference to a given `blob_id` is removed from the account.

### Extension worker

Run as a separate process with `oysterd extend`. When Walrus integration is active, it runs on
a configurable interval:

1. Queries the Oyster database for `StoragePool`s whose `end_epoch` falls within
   `POOL_EXTEND_LOOKAHEAD_DAYS` of the current epoch.
2. For each expiring pool, builds a single `extend_storage_pool` PTB (extending by
   `POOL_EXTEND_EPOCHS`), signs via Pearl, and submits to Sui.
3. Updates the cached `pool_end_epoch` on the account row.

When extension failures indicate insufficient funds and the blob's owning app has a
`webhook_url` configured, Oyster notifies that URL with the account ID, wallet address, and
error details. The webhook client uses a circuit breaker to avoid repeated calls to a failing
endpoint.

### Database

Supports SQLite and PostgreSQL via the SQLx Any driver; the backend is determined at runtime by
the `DATABASE_URL` connection string. Migrations are per backend under `crates/oyster/migrations/`:

- `migrations/sqlite/001_initial.sql`
- `migrations/postgres/001_initial.sql`

Tables: `accounts`, `api_keys`, `s3_access_keys`, `apps`, `buckets`, `blobs`, `blob_tags`.

### Configuration

All configuration is via environment variables. Secrets can alternatively be loaded from files
using CLI flags (useful for Kubernetes secrets, Docker Swarm, or secret managers that mount
files to `/run/secrets/`).

| Variable | Default | Description |
|----------|---------|-------------|
| `BIND_ADDR` | `0.0.0.0:3000` | HTTP listen address |
| `DATABASE_URL` | `sqlite:oyster.db?mode=rwc` | Database connection string (SQLite or PostgreSQL) |
| `BLOB_STORE_PATH` | `blob_store` | Path for LocalBlobStore |
| `ENABLE_DEBUG` | `false` | Enable `/debug/*` endpoints |
| `PEARL_GRPC_URL` | -- | Pearl gRPC address (e.g. `http://127.0.0.1:50051`) |
| `PEARL_SERVICE_SECRET` | -- | Shared secret for Pearl auth (**required**) |
| `WALRUS_AGGREGATOR_URL` | -- | Walrus aggregator HTTP URL |
| `SUI_RPC_URL` | -- | Sui RPC endpoint |
| `WALRUS_SYSTEM_OBJECT` | -- | Walrus system object ID on Sui |
| `WALRUS_STAKING_OBJECT` | -- | Walrus staking object ID on Sui |
| `POOL_INITIAL_EPOCHS_AHEAD` | `5` | Epochs ahead when creating a new `StoragePool` |
| `POOL_INITIAL_ENCODED_CAPACITY_BYTES` | `1048576` | Initial reserved capacity for a new pool (1 MiB, Walrus `BYTES_PER_UNIT_SIZE`) |
| `POOL_EXTEND_EPOCHS` | `5` | Epochs to extend a `StoragePool` by |
| `POOL_EXTEND_LOOKAHEAD_DAYS` | `7` | How far ahead of pool expiry to trigger an extension |
| `BLOB_EXTEND_INTERVAL_SECS` | `3600` | Extension worker cycle cadence |
| `OYSTER_EXTENSION_METRICS_BIND_ADDR` | `0.0.0.0:50053` | Metrics endpoint for the extension worker |

| CLI flag | Description |
|----------|-------------|
| `--pearl-service-secret-file PATH` | Read `PEARL_SERVICE_SECRET` from a file |

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

| CLI flag | Description |
|----------|-------------|
| `--pearl-service-secret-file PATH` | Read `PEARL_SERVICE_SECRET` from a file |
| `--pearl-master-seed-file PATH` | Read `PEARL_MASTER_SEED` from a file |

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
`~/.config/oyster/client.yaml`. The file holds a map of named **contexts** (each
with its own `url`, `api_key`, and managed app admin keys); pick one with
`--context <name>`, `OYSTER_CONTEXT`, or the file's `active_context` field. CLI
flags (`--url`, `--api-key`) override the selected context. Pass `--json` for
machine-readable output.

### Setting up the CLI from scratch

Step-by-step walkthrough for a fresh machine, using the public testnet
deployment.

#### 1. Create a minimal config file

The CLI has no config file by default. Create
`~/.config/oyster/client.yaml` with just an active context and a URL:

```bash
mkdir -p ~/.config/oyster
cat > ~/.config/oyster/client.yaml <<'YAML'
active_context: testnet

contexts:
  testnet:
    url: https://oyster.testnet.mystenlabs.com/api/v1
YAML
```

Verify the CLI picks it up:

```bash
oyster info
# config: /Users/you/.config/oyster/client.yaml
# url:    https://oyster.testnet.mystenlabs.com/api/v1
# key:    (not set)
```

Notes on the schema:
- `url` must include `/api/v1`.
- `active_context` selects which context's settings are used. You can override
  per invocation with `--context <name>` or `OYSTER_CONTEXT=<name>`. With a
  single context in the file, that one is auto-selected even without
  `active_context`.
- The schema is strict (`deny_unknown_fields`) — typos in keys fail to parse.

#### 2. (Optional) Add an `api_key` for data-plane calls

`api_key` authenticates the per-account data routes (`buckets`, `blobs`,
`wallet`, …). Skip this step if you only need to manage app admin keys.
Otherwise, have an admin issue a key for your account and add it to the
context:

```yaml
contexts:
  testnet:
    url: https://oyster.testnet.mystenlabs.com/api/v1
    api_key: 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef
```

The value is the raw 64-char hex Bearer token returned by
`POST /api/v1/accounts/{account_id}/api-keys`.

#### 3. Get an admin key from an operator

Admin keys are separate from `api_key` — they authenticate admin
app-management calls (creating accounts, issuing API keys / S3 access keys).
Each admin key is a long-lived 64-char hex Bearer token; rotation is
operator-driven via issue-then-revoke. Ask an Oyster operator with access
to the target deployment to run:

```bash
# operator runs this and shares the output with you
oysterd app issue-admin-key <app_id>
# prints: <64-char hex admin key>
```

If your app doesn't exist yet, the operator creates it first — `oysterd app
new` auto-issues a first admin key by default:

```bash
oysterd app new --name my-app --contact-email me@example.com
# prints: <app_id>
# prints: <admin_key>     <- the first admin key for this app
```

#### 4. Import the admin key into your config

Pick a local nickname for the app — it's just a key under `apps:` in your
config file, so use anything you'll remember (it does not have to match the
server-side app name). Then paste the admin key at the prompt:

```bash
oyster app import my-app
# Admin key for my-app:    <- prompt; characters are hidden when stdin is a tty
# imported my-app into context testnet
```

The CLI writes the entry into the active context and replaces the config
file atomically. Your `client.yaml` now looks like:

```yaml
active_context: testnet
contexts:
  testnet:
    url: https://oyster.testnet.mystenlabs.com/api/v1
    apps:
      my-app:
        admin_key: 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef
```

For non-interactive use (CI, scripts), pipe the key on stdin instead of
typing it:

```bash
echo "$ADMIN_KEY" | oyster app import my-app
```

#### 5. Rotate the admin key

Admin keys are long-lived and do not expire — rotation is voluntary. The
recommended pattern is AWS-style two-key overlap: issue a fresh key, swap
it in, then revoke the old one once you're sure nothing is still using it.

```bash
# operator (issues a new key alongside the old one)
oysterd app issue-admin-key <app_id>
# prints: <new admin_key>
# prints: <new key id>     # to stderr — needed later to revoke

# you (replace the local entry with the new key)
oyster app import my-app

# operator (after verifying no in-flight callers depend on the old key)
oysterd app revoke-admin-key <OLD_KEY_ID>
```

`oysterd app list-admin-keys <app_id>` shows all issued keys (including
revoked ones) so an operator can confirm what is live before revoking.

#### 6. (Optional) Talk to multiple deployments

Add more contexts to the same file and switch between them with `--context`
or `OYSTER_CONTEXT`:

```yaml
active_context: testnet
contexts:
  testnet:
    url: https://oyster.testnet.mystenlabs.com/api/v1
    apps:
      my-app:
        admin_key: ...
  devnet:
    url: https://oyster.devnet.mystenlabs.com/api/v1
```

```bash
oyster --context devnet info
OYSTER_CONTEXT=devnet oyster info
```

---

## Docker

Dockerfiles are provided for both services:

- `docker/Dockerfile.oysterd` -- Builds the `oysterd` binary. Exposes port 3000 (HTTP API + S3).
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

# Or load secrets from files (useful for Kubernetes / Docker Swarm):
# cargo run -p pearl -- \
#   --pearl-master-seed-file /run/secrets/pearl_master_seed \
#   --pearl-service-secret-file /run/secrets/pearl_service_secret

# Terminal 2: start Oyster
PEARL_GRPC_URL=http://127.0.0.1:50051 \
PEARL_SERVICE_SECRET=<shared-secret> \
cargo run -p oyster  # runs `oysterd serve` by default

# Terminal 3: create an app (auto-issues a first admin key), then use the admin API
cargo run -p oyster -- app new --name dev --contact-email dev@example.com
# Prints: <app_id>
# Prints: <admin_key>     <- first admin key, save it

# Create an account and API key via the admin API
curl -X POST http://localhost:3000/api/v1/accounts \
  -H "Authorization: Bearer <admin_key>"
# Returns { "account_id": "...", ... }

curl -X POST http://localhost:3000/api/v1/accounts/<account_id>/api-keys \
  -H "Authorization: Bearer <admin_key>"
# Returns { "bearer_token": "...", ... }

# Use the API key for data operations
curl -X POST http://localhost:3000/api/v1/buckets \
  -H "Authorization: Bearer <api_key>" \
  -H "Content-Type: application/json" \
  -d '{"name": "my-bucket"}'

curl -X PUT http://localhost:3000/api/v1/buckets/my-bucket/blobs/hello.txt \
  -H "Authorization: Bearer <api_key>" \
  -H "Content-Type: text/plain" \
  -d 'hello world'

curl http://localhost:3000/api/v1/buckets/my-bucket/blobs/hello.txt
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
