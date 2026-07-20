# Contributing to Oyster

## Prerequisites

- **Rust** (edition 2024) -- install via [rustup](https://rustup.rs/)
- **protoc** -- `brew install protobuf` on macOS, `apt install protobuf-compiler` on Debian
- **SQLite3** -- typically pre-installed; the Rust `sqlx` crate bundles `libsqlite3-sys`

## Building

```bash
cargo build
```

The workspace compiles all four crates. Proto files are compiled by `build.rs` in both `pearl`
and `oyster` (using `tonic-prost-build`). You need `protoc` on your PATH.

## Checking your work

The project uses a custom `chk` script that runs formatting and linting:

```bash
chk
```

This runs `cargo fmt` (with project-specific formatting options) followed by
`cargo clippy --fix` and other checks. Always run `chk` before committing.

**Important:** Always use `chk` to verify the full workspace, since cross-crate changes are common.

## Running tests

```bash
# Pearl unit + integration tests
cargo test -p pearl

# Oyster unit + integration tests
cargo test -p oyster

# E2E tests (boots Sui + Walrus in-process, ~30s startup)
cargo test -p oyster-e2e-tests
```

Integration tests for both crates spin up in-process gRPC/HTTP servers on random ports with
in-memory SQLite databases. No external services are needed for `cargo test -p pearl` or
`cargo test -p oyster`.

## Code organization conventions

### File layout

Use `module.rs` and `module/submodule.rs`, **not** `mod.rs`. For example:

```
db.rs           # pub mod accounts; pub mod pending_transactions;
db/
  accounts.rs
  pending_transactions.rs
```

### Adding dependencies

Always use `cargo add` rather than manually editing `Cargo.toml`:

```bash
cargo add -p pearl some-crate --features foo
```

This ensures you get the latest compatible version. For workspace-level dependencies, add them
to `[workspace.dependencies]` in the root `Cargo.toml`.

### Commit style

Use [semantic commits](https://www.conventionalcommits.org/):

```
feat(pearl): add balance tracking
fix(oyster): handle empty bucket list
chore: update dependencies
test(pearl): add pending transaction timeout test
docs: update PLAN.md -- mark phase 12 complete
```

No attributions in commit messages.

## Adding a new Oyster HTTP endpoint

1. **Add the route handler** in the appropriate module under `crates/oyster/src/routes/`
   (`account.rs`, `blobs.rs`, or `buckets.rs`). Use utoipa annotations for OpenAPI docs:

   ```rust
   #[utoipa::path(
       get,
       path = "/my-endpoint",
       tag = "MyTag",
       responses(
           (status = 200, description = "Success"),
       ),
       security(("bearer" = []))
   )]
   pub async fn my_handler(/* ... */) -> Result<impl IntoResponse, AppError> {
       // ...
   }
   ```

2. **Register the route** in `crates/oyster/src/routes.rs`:

   ```rust
   .routes(routes!(my_module::my_handler))
   ```

3. **Add integration tests** in `crates/oyster/tests/integration.rs`. Tests use the
   `test_app()` helper which creates an in-memory database and `LocalBlobStore`.

## Adding a new Pearl gRPC RPC

1. **Define the RPC** in `crates/pearl/proto/pearl.proto` with request/response messages.
   Use additive field numbers to maintain backward compatibility.

2. **Add models** to `crates/pearl/src/models.rs` if new types are needed.

3. **Add database functions** in the appropriate `crates/pearl/src/db/` module. Use
   `sqlx::query` / `sqlx::query_as` with `?` bind parameters.

4. **Implement the handler** in `crates/pearl/src/grpc.rs` in the `impl Pearl for PearlService`
   block. Map errors via the `to_status` function.

5. **Update the client** in `crates/oyster/src/pearl_client.rs` -- add a method to
   `PearlConnection` that wraps the generated gRPC client call with authentication.

6. **Add tests** in both `crates/pearl/tests/integration.rs` (gRPC-level) and
   `crates/oyster/tests/integration.rs` (client-level).

## Adding a new blob store implementation

1. Implement the `BlobStore` trait from `crates/oyster/src/blob_store.rs`:

   ```rust
   pub trait BlobStore: Send + Sync + 'static {
       fn store(&self, data: &[u8], account_id: Option<&str>)
           -> BoxFuture<'_, Result<StoreResult, BlobStoreError>>;
       fn read(&self, blob_id: &BlobId)
           -> BoxFuture<'_, Result<Vec<u8>, BlobStoreError>>;
       fn delete(&self, blob_id: &BlobId, sui_object_id: Option<&str>,
                 account_id: Option<&str>)
           -> BoxFuture<'_, Result<(), BlobStoreError>>;
       fn exists(&self, blob_id: &BlobId)
           -> BoxFuture<'_, Result<bool, BlobStoreError>>;
   }
   ```

2. `StoreResult` returns `blob_id` (content hash) and optionally `sui_object_id`.
   The `account_id` parameter enables per-account signing for on-chain stores.

3. Add selection logic in `crates/oyster/src/main.rs` based on configuration.

## Database migrations

Both Oyster and Pearl use `sqlx::migrate!()` which auto-runs migrations from the `migrations/`
directory on startup. Migrations are embedded in the binary at compile time.

### Adding a migration

1. Create a new `.sql` file in the appropriate `migrations/` directory with the next sequence
   number:

   ```
   crates/pearl/migrations/003_my_change.sql
   crates/oyster/migrations/004_my_change.sql
   ```

2. Use `ALTER TABLE` for additive changes. SQLite has limited `ALTER TABLE` support --
   you can add columns (`ADD COLUMN`) and create tables/indexes, but cannot drop columns or
   rename tables without recreating them.

3. Always include `NOT NULL DEFAULT <value>` for new columns on existing tables so that
   existing rows are valid.

4. After adding a migration, run `chk` to verify it compiles (sqlx checks migrations at
   compile time).

## Testing patterns

### Pearl unit tests

Database tests use in-memory SQLite pools via a `test_pool()` helper:

```rust
async fn test_pool() -> DbPool {
    db::create_pool("sqlite::memory:").await.expect("in-memory pool")
}
```

### Pearl integration tests

Tests start an in-process gRPC server on a random port, connect a client, and exercise the
full RPC flow:

```rust
let mut client = start_server().await;
let resp = client.create_account(authenticated(proto::CreateAccountRequest { ... }))
    .await.unwrap().into_inner();
```

The `authenticated()` helper adds the Bearer token to requests. The `start_server()` helper
creates an in-memory database, binds to a random port, and returns a connected client.

### Oyster integration tests

Tests build a full Axum `Router` with in-memory SQLite and a `LocalBlobStore`:

```rust
let (app, _tmp, _pool) = test_app().await;
let (account_id, api_key) = create_test_account(&app).await;
```

Use `json_response()` and `raw_response()` helpers to send requests and inspect responses.

### E2E tests

E2E tests boot the full stack in-process (Sui test cluster, Walrus storage nodes, Pearl, and
Oyster) — no external Walrus testbed is needed. They take ~30 seconds to start.

```bash
cargo test -p oyster-e2e-tests
```

## Error handling

- **Oyster:** `AppError` enum maps to HTTP status codes in `crates/oyster/src/error.rs`.
  Database errors become 500, not-found becomes 404, etc.
- **Pearl:** `Error` enum maps to gRPC `Status` codes in `crates/pearl/src/grpc.rs` via the
  `to_status` function. Add new error variants to `crates/pearl/src/error.rs` and their status
  mappings to `to_status`.

## Authentication

### Oyster API keys

- **Generate:** 32 random bytes, hex-encoded (64-char string).
- **Store:** Blake2s-256 hash of the key. The first 8 characters are stored as a prefix for
  display/audit.
- **Verify:** Hash the incoming key, look up by hash in the database.

See `crates/oyster/src/auth.rs`.

### Pearl service secret

A shared secret passed as `Bearer {secret}` in the gRPC `authorization` metadata header.
Checked by a tonic interceptor in `crates/pearl/src/auth.rs`.

## Local development with Walrus

For manual testing against a running Walrus local testbed (separate from the e2e tests, which
boot their own in-process cluster), use `scripts/local-testbed.sh`:

```bash
# Start (requires a running Walrus testbed)
./scripts/local-testbed.sh --walrus-working-dir ~/src/walrus/working_dir

# Stop
./scripts/local-testbed.sh --stop
```

This:
1. Extracts Walrus config from the testbed's `client_config.yaml`.
2. Starts Pearl and Oyster in tmux sessions.
3. Creates an operator Pearl account.
4. Funds the operator wallet with SUI and WAL.
5. Prints connection details and a test API key.

## Sui / Walrus SDK versions

The workspace pins specific Sui SDK tags via git dependencies:

- `sui-types`, `sui-sdk`, `shared-crypto`, `fastcrypto` -- from `testnet-v1.66.1` (Pearl)
- `sui-sdk` -- from `testnet-v1.65.1` (Oyster, via walrus-sui)
- Walrus SDK crates -- from `testnet-v1.42.1`

Version conflicts between crates are bridged by Oyster re-exporting `sui_types`:
`pub use sui_types;` in `crates/oyster/src/lib.rs`.

## Transaction lifecycle

Understanding the full transaction lifecycle helps when debugging:

1. **Build:** Oyster constructs a `TransactionData` (BCS-serializable) using Walrus
   `WalrusPtbBuilder` for operations like `reserve_space`, `register_blob`, `extend_blob`,
   or `certify_blob`.

2. **Serialize:** The `TransactionData` is BCS-encoded into bytes.

3. **Sign (Pearl):**
   - Loads the Ed25519 private key from the database.
   - Wraps the `TransactionData` in an `IntentMessage` with `Intent::sui_transaction()`.
   - Signs with `Signature::new_secure()`.
   - Constructs a `Transaction` from the data + signature.
   - Returns BCS-encoded `Transaction` bytes.
   - Creates a `pending_transaction` record and deducts estimated cost from cached balance.

4. **Submit:** Oyster deserializes the signed `Transaction` and submits it to Sui via
   `execute_transaction_block`.

5. **Confirm:** Oyster calls Pearl's `ConfirmTransaction` with the tx digest and actual gas
   cost. Pearl corrects the cached balance.

6. **Reconcile:** Pearl's background task periodically queries Sui RPC for true on-chain
   balances and times out stale pending transactions.
