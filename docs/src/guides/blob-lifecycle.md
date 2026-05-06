# Blob Lifecycle

This guide explains what happens to blobs from upload to expiration, and
how Oyster's automatic extension service keeps your data alive.

## Upload and Expiration

Walrus storage is epoch-scoped, not time-scoped. When you upload a blob,
Oyster registers it under your account's `StoragePool` — a single
on-chain object whose `end_epoch` defines the lifetime of every blob it
holds. The first upload from an account lazily creates the pool with
`POOL_INITIAL_EPOCHS_AHEAD` of runway (default `5`). Subsequent uploads
share that same expiration.

The pool's `end_epoch` is surfaced on the account as
`pool_end_epoch`. To inspect remaining runway, compare it against
the network's current epoch. Blob responses do not carry a per-blob
expiration — every blob in the account shares the pool's lifetime.

## Automatic Extension

Oyster runs a **background extension worker** (`oysterd extend`) that
keeps every account's `StoragePool` ahead of expiration. As long as the
worker is running and the account's Pearl-derived wallet has WAL and
SUI to spend, your blobs persist indefinitely.

### What it guarantees

While the worker is running:

- Any account whose `pool_end_epoch` falls within
  `current_epoch + POOL_EXTEND_LOOKAHEAD_EPOCHS` is picked up and its
  pool extended by `POOL_EXTEND_EPOCHS` Walrus epochs, provided the
  Pearl-derived wallet has the WAL and SUI to cover it.
- Each pool is extended at most once per
  `EXTENSION_CLAIM_COOLDOWN_SECS` window, so retries and
  `account.funding_required` webhook deliveries are naturally rate-
  limited per account. The same cooldown doubles as webhook-spam
  suppression — a row that just emitted `account.funding_required`
  cannot re-emit until the cooldown expires.
- Latency between an account becoming eligible and its pool being
  extended is bounded above by `EXTENSION_IDLE_SLEEP_SECS` plus the
  RPC time of one extension.

### Horizontal scaling

The worker is safe to run as multiple replicas against the same
database — each pool is claimed by exactly one replica per cycle, so
two extenders never double-extend the same pool. The public Oyster
testnet runs 2 extender replicas behind a shared DB.

### Configuration

| Variable | Default | Description |
|----------|---------|-------------|
| `POOL_EXTEND_LOOKAHEAD_EPOCHS` | `7` | Claim any pool expiring within `current_epoch + this`. Leave default unless your network's epoch length is unusual. |
| `POOL_EXTEND_EPOCHS` | `5` | Walrus epochs each `extend_storage_pool` PTB extends by. Tune per network: testnet ≈ 1 day/epoch → `30`; mainnet ≈ 14 days/epoch → `4`. |
| `EXTENSION_IDLE_SLEEP_SECS` | `30` | Sleep when a cycle finds zero work. Leave default unless tuning latency vs. RPC load. |
| `EXTENSION_BUSY_SLEEP_MS` | `250` | Sleep between cycles while there's still work to drain. Leave default. |
| `EXTENSION_CLAIM_BATCH_SIZE` | `100` | Max pool rows claimed per cycle. Leave default unless DB round-trip latency dominates. |
| `EXTENSION_CLAIM_COOLDOWN_SECS` | `60` | Per-row claim TTL — also the webhook re-notify backoff for the same account. Leave default. |

### Insufficient funds

If the Pearl-derived wallet for an account is short on WAL or SUI, the
`extend_storage_pool` PTB fails with an insufficient-funds error.
Oyster:

1. Logs the failure.
2. POSTs an `account.funding_required` webhook to the owning app's
   configured receiver URL (if any).
3. Leaves the cooldown TTL stamped on the row so the same account
   does not re-trigger the webhook for `EXTENSION_CLAIM_COOLDOWN_SECS`.

The next cycle re-claims the row once the cooldown expires; if the
wallet is still underfunded, another webhook fires. See
[Webhooks](webhooks.md) for the full payload schema, retry policy,
circuit-breaker behavior, and receiver examples.

## Blob States

A blob's lifetime is bound to its account's `StoragePool`:

```
Upload → Active → Pool Approaching Expiry → Pool Extended → Active → ...
                                         ↘ (if wallet underfunded)
                                           Funding Required webhook
```

| State | Description |
|-------|-------------|
| **Active** | Blob is registered in a pool with `pool_end_epoch > current_epoch` |
| **Pool Approaching Expiry** | `pool_end_epoch < current_epoch + POOL_EXTEND_LOOKAHEAD_EPOCHS`; the worker will claim and extend |
| **Pool Extended** | `extend_storage_pool` PTB succeeded; `pool_end_epoch` advanced |
| **Funding Required** | PTB failed insufficient-funds; webhook fired; cooldown TTL active |

## Deletion

Blobs can be explicitly deleted at any time via the API:

- **JSON API:** `DELETE /api/v1/buckets/{bucket}/blobs/{key}`
- **S3 API:** `DeleteObject`
- **CLI:** `oyster delete <key> --bucket <bucket>`

Deletion is reference-counted at the content-addressed level: the
on-chain `delete_pooled_blob` PTB fires only when the last reference
to a given `blob_id` is removed from the account (see
[Content Addressing](content-addressing.md)).

## Local vs. On-Chain Storage

| Aspect | Local (filesystem) | On-chain (Walrus) |
|--------|-------------------|-------------------|
| Expiration tracked | Not applicable | `accounts.pool_end_epoch` (Walrus epochs) |
| Auto-renewal | Not applicable | Yes (extension worker, multi-instance safe) |
| `pooled_blob_object_id` | `null` | Sui object ID of the registered `PooledBlob` |
| Storage scope | Per-blob file on disk | Pool-scoped capacity reservation |
