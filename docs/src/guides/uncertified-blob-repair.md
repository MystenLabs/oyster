# Repairing Uncertified Blobs

An upload is three steps in order: register the `PooledBlob` in the
account's `StoragePool`, push the slivers to the storage nodes, and
submit the certify transaction. Walrus serves a blob only once it is
certified. If the certify transaction fails after register landed, the
`PooledBlob` stays on chain with `certified_epoch: null` and the data
is unreadable.

Before 0.14.4 this state could become permanent. The failed upload
answered `502` with no `blobs` row. The client's retry re-registered,
which aborted with `EFieldAlreadyExists`, and the self-heal path
answered `201` with the existing `PooledBlob` without certifying it.
The row it wrote then short-circuited every later `PUT` of the same
content, so the client had no way to repair it, and had already
discarded the bytes. The upload path now finishes the certify on the
retry (see [Blob Lifecycle](blob-lifecycle.md#upload-failure-and-retry))
and on any later dedup hit (see
[Content Addressing](content-addressing.md#deduplication)). This guide
is the bulk pass for the blobs nobody re-uploads: `oysterd blobs
repair`.

## What it does

A candidate is one `(account, blob)` with its on-chain `PooledBlob`.
Candidates come from the database (`blobs` rows that reference a
`PooledBlob` but have no `certified_epoch`) and from `--ids-file`, a
list of `PooledBlob` object IDs, which also reaches orphans with no row
at all: a `502`'d upload the client never retried.

For each candidate:

1. Read the `PooledBlob` from chain. Gone: `MISSING-ON-CHAIN`.
2. Already certified: stamp `certified_epoch` on the rows (`stamped`).
   Rows from before migration 026 land here.
3. Otherwise ask the storage nodes for a confirmation certificate
   without uploading anything. The failed upload did push the slivers
   before its certify failed, and nodes keep uncertified slivers for a
   while. With a certificate, submit certify and stamp the rows
   (`certified`). The blob is readable again.
4. No certificate: the bytes are gone from Walrus and Oyster never kept
   a copy. By default this is reported as `UNRECOVERABLE`. With
   `--delete-unrecoverable` the tool deletes the `PooledBlob` (returning
   its capacity to the pool), removes the rows, and prints the keys so
   the customer can be told to re-upload (`deleted`).

Every step is idempotent. Re-running after a crash or a transient
failure picks up where it left off. `--dry-run` reads the chain and the
storage nodes and reports what each candidate would get (`would-stamp`,
`would-certify`, `would-delete`, `UNRECOVERABLE`) without submitting or
writing anything.

## Prerequisites

- Oyster ≥ 0.14.4 deployed, so new uploads stamp `certified_epoch` and
  the upload path no longer creates these blobs.
- The same environment as `oysterd serve`: `DATABASE_URL`,
  `PEARL_GRPC_URL`, `PEARL_SERVICE_SECRET`, `SUI_RPC_URL`,
  `WALRUS_SYSTEM_OBJECT`, `WALRUS_STAKING_OBJECT`. Certify and delete
  transactions are signed by Pearl under each account's own key and
  paid from the account's wallet (SUI gas only; no WAL).
- An account under `oysterd keys migrate` is skipped (`FAILED: signer:
  key migration in progress`); re-run after the rotation.

## Procedure

1. Dry run, scoped to one account first if you have a known case:

   ```bash
   oysterd blobs repair --dry-run
   oysterd blobs repair --dry-run --account <ACCOUNT_ID>
   ```

   Output is TSV: `ACCOUNT`, `BLOB_ID`, `POOLED_BLOB`, `REFERENCES`
   (`bucket/key,...`), `OUTCOME`. A summary line goes to stderr. The
   `would-certify` count is what the real run will recover.

2. Run it:

   ```bash
   oysterd blobs repair
   ```

   Exit code 1 means some candidates need attention. Read their
   `OUTCOME`:

   - `UNRECOVERABLE`: decide with the customer (step 3).
   - `MISSING-ON-CHAIN`: the rows point at a `PooledBlob` that no
     longer exists. The data is gone; treat like unrecoverable.
   - `FAILED: ...`: transient (RPC, a concurrent transaction on the
     account's pool). Re-run.

3. For the unrecoverable ones, once the affected customers know their
   keys will disappear:

   ```bash
   oysterd blobs repair --delete-unrecoverable
   ```

   Each `deleted` line lists the keys that were removed. Deleting is
   what lets a future `PUT` of the same content register afresh; until
   then the `PooledBlob` blocks re-registration and the rows would make
   the dedup path try (and fail) to certify it again.

4. With a list of object IDs from an incident (one per line, `#`
   comments allowed), include orphans that have no rows:

   ```bash
   oysterd blobs repair --ids-file pooled-blobs.txt
   oysterd blobs repair --ids-file pooled-blobs.txt --skip-db
   ```

   `--skip-db` repairs only the listed IDs. Entries whose
   `StoragePool` matches no account are reported as `UNKNOWN-POOL`.

## Verifying

- `oysterd blobs repair --dry-run` prints nothing (no candidates).
- `GET /api/v1/buckets/{bucket}/blobs/{key}` returns the data for a
  `certified` key.
- `oyster_dedup_certify_verify_total{certified="completed"}` and
  `oyster_register_dedup_self_heal_total{certified="completed"}` stay
  flat afterwards; a rising count means certify transactions are still
  failing on first attempt (see
  `oyster_certify_stale_version_retries_total`).
