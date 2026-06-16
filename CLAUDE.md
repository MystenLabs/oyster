# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

- **Check/Lint/Format**: `chk` — runs `cargo fmt`, `cargo clippy --fix`, and other checks. Always use this; never run `cargo check -p <crate>` individually.
- **E2E tests**: `cargo test -p oyster-e2e-tests` (boots Sui + Walrus in-process; no external testbed needed)
- **Local full-stack dev**: `./scripts/local-testbed.sh` (for manual testing against an already-running Walrus local testbed)

## Architecture

Oyster is a Web2-friendly object storage API backed by [Walrus](https://walrus.xyz/) (decentralized blob storage) and [Sui](https://sui.io/) (on-chain state). Two services communicate via gRPC:

- **Oyster** (`crates/oyster`): Axum HTTP server on `:3000`. Manages accounts, API keys, buckets, and blobs. Has two blob store backends: `LocalBlobStore` (filesystem) and `DirectWalrusBlobStore` (on-chain via Programmable Transaction Blocks). Runs a background extension task to auto-renew expiring blobs.
- **Pearl** (`crates/pearl`): Tonic gRPC server on `:50051`. Custodial wallet service — derives Ed25519 keys via HKDF-SHA256, signs Sui transaction blocks. Uses a shared-secret Bearer token for auth.
- **oyster-cli** (`crates/oyster-cli`): CLI client for the Oyster HTTP API. Config via `client.yaml`.
- **oyster-e2e-tests** (`crates/oyster-e2e-tests`): Full-stack tests booting Sui cluster + Walrus + Pearl + Oyster in-process.

### Key Data Flow

API request → Oyster auth (Blake2s-256 hashed Bearer token) → route handler → SQLite (oyster.db) + blob store backend. When storing on-chain: Oyster calls Pearl gRPC to sign a Sui PTB, then submits it.

### Database

Oyster uses SQLx with the `Any` driver, supporting SQLite (default for local dev) and PostgreSQL (production). The backend is determined at runtime by the connection URL. SQLite uses WAL journal mode. Migration-based schema management with separate migration sets under `crates/oyster/migrations/sqlite/` and `crates/oyster/migrations/postgres/`. Tables: `accounts`, `api_keys`, `buckets`, `blobs`, `apps`. Pearl is stateless — keys are derived on-the-fly from `PEARL_MASTER_SEED` via HKDF-SHA256.

### Proto

`crates/pearl/proto/pearl.proto` defines the Pearl gRPC API (`CreateAccount`, `GetAddress`, `SignTransaction`). Both `build.rs` scripts compile protos via `tonic-prost-build`.

## Code Conventions

- **File layout**: `module.rs` + `module/submodule.rs` — never `mod.rs`
- **Lint enforcement**: `missing_docs = "deny"` workspace-wide; all public APIs must be documented
- **Error handling**: `AppError` (Oyster) and `Error` (Pearl) enums map to HTTP/gRPC status codes
- **Auth**: Oyster uses Blake2s-256 hashed 32-byte random Bearer tokens for end-user API auth; long-lived per-app admin-key Bearer auth (also Blake2s-256 hashed) for app admin routes; Pearl uses a shared service secret
- **Adding dependencies**: always use `cargo add`, never edit `Cargo.toml` manually

## Testing Patterns

- **Pearl integration tests**: `start_server()` spins up an in-process gRPC server on a random port; `authenticated()` injects the Bearer token
- **Oyster integration tests**: `test_app()` creates a full Axum router with in-memory SQLite; `SpyBlobStore` records store/delete calls
- **E2E tests**: boot Sui + Walrus + Pearl + Oyster in-process (~10–30s startup); no external testbed needed

## Configuration

Oyster and Pearl are configured via environment variables (see `crates/oyster/src/config.rs` and `crates/pearl/src/config.rs`). Key vars:
- Oyster: `BIND_ADDR`, `DATABASE_URL`, `PEARL_GRPC_URL`, `PEARL_SERVICE_SECRET`, `SUI_RPC_URL`. Supported SQLite floor is ≥ 3.35 (for `ALTER TABLE … DROP COLUMN`).
- Pearl: `PEARL_BIND_ADDR`, `PEARL_SERVICE_SECRET`, `PEARL_MASTER_SEED`, `PEARL_METRICS_BIND_ADDR`, optional TLS via `PEARL_TLS_CERT_PATH`/`PEARL_TLS_KEY_PATH`

## OpenAPI Docs

Oyster exposes Swagger/Scalar docs at `/api/docs` (via `utoipa` + `utoipa-axum` + `utoipa-scalar`).

### Gotchas

Occasionally you will encounter something that makes you think, "this is/was a pre-existing issue."
EVERY TIME that happens, stop your chain of thought and address the pre-existing issue. Start by
launching a sub-agent to validate that this issue existed prior to your ongoing work. Consider
cloning the entire repo and attempting to reproduce the issue if need be.

