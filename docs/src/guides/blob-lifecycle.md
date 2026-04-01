# Blob Lifecycle

This guide explains what happens to blobs from upload to expiration, and
how Oyster's automatic extension service keeps your data alive.

## Upload and Expiration

When you upload a blob, Oyster sets a default expiration of **30 days**:

```json
{
  "key": "hello.txt",
  "blob_id": "2cf24dba...",
  "created_at": "2025-01-15T10:31:00Z",
  "expires_at": "2025-02-14T10:31:00Z"
}
```

The `expires_at` field reflects when the blob's on-chain storage allocation
(on Walrus) would naturally expire. For blobs stored locally (filesystem
backend), this timestamp is tracked in the database but not enforced.

## Automatic Extension

Oyster runs a **background extension service** that automatically renews
blobs before they expire. In practice, this means your data persists
indefinitely as long as:

1. The extension service is running
2. The account has sufficient on-chain funds (SUI/WAL)

### How It Works

The extension service runs on a periodic loop:

1. **Scan** — every check interval (default: 1 hour), query the database
   for blobs expiring within the lookahead window (default: 7 days)
2. **Extend** — for each expiring blob, submit a Walrus transaction to
   extend the storage allocation by a set number of epochs (default: 5)
3. **Update** — record the new expiration timestamp in the database

This means a blob uploaded today will be automatically renewed ~23 days
later (30 days expiration minus 7 days lookahead), and then again every
cycle as needed.

### Extension Configuration

Oyster administrators can tune the extension service with these environment
variables:

| Variable | Default | Description |
|----------|---------|-------------|
| `BLOB_EXTEND_INTERVAL_SECS` | `3600` | How often to check for expiring blobs (seconds) |
| `BLOB_EXTEND_LOOKAHEAD_DAYS` | `7` | How far ahead to look for upcoming expirations |
| `BLOB_EXTEND_EPOCHS` | `5` | Number of Walrus epochs to extend by |

### Insufficient Funds

If an account doesn't have enough on-chain funds to extend a blob, the
extension service:

1. Logs the error
2. Posts a webhook notification to the owning app's `webhook_url` (if
   configured) so administrators can top up the account
3. Continues processing other blobs

The webhook payload includes the account ID, wallet address, and error
details.

## Blob States

A blob goes through these states during its lifetime:

```
Upload → Active → Approaching Expiry → Extended → Active → ...
                                    ↘ (if funds insufficient)
                                      Expired
```

| State | Description |
|-------|-------------|
| **Active** | Blob is stored and accessible; expiration is in the future |
| **Approaching Expiry** | Within the lookahead window; extension service will renew it |
| **Extended** | Successfully renewed; `expires_at` updated with new date |
| **Expired** | Not renewed (insufficient funds or service down); data may be lost on-chain |

## Deletion

Blobs can be explicitly deleted at any time via the API:

- **JSON API:** `DELETE /api/v1/buckets/{bucket}/blobs/{key}`
- **S3 API:** `DeleteObject`
- **CLI:** `oyster delete <key> --bucket <bucket>`

Deletion is immediate and removes the blob metadata. The underlying data
is only removed from storage when no other keys reference the same content
(see [Content Addressing](content-addressing.md)).

## Local vs. On-Chain Storage

| Aspect | Local (filesystem) | On-chain (Walrus) |
|--------|-------------------|-------------------|
| Expiration tracked | In database | On-chain + database |
| Auto-renewal | Not applicable | Yes (extension service) |
| `sui_object_id` | `null` | Sui object ID |
| `expires_at` | Set but not enforced | Enforced by Walrus network |
