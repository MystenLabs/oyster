-- Walrus epoch in which Oyster observed this blob's on-chain `PooledBlob`
-- certified. NULL means "not known to be certified": rows from before
-- this migration, and rows written for a blob whose certify had not run
-- yet. The account-level content dedup on upload only short-circuits on
-- a non-NULL value; for a NULL it reads the `PooledBlob` from chain,
-- certifies it if needed, and stamps the epoch here (lazy backfill).
-- `oysterd blobs repair` does the same in bulk. The exact epoch is
-- informational; the contract is NULL vs. non-NULL.
ALTER TABLE blobs ADD COLUMN certified_epoch BIGINT;
