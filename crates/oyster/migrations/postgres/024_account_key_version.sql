-- Pearl master-seed version this account's wallet key derives from.
-- Stamped at account creation with Pearl's active key version; a future
-- key rotation migrates an account's on-chain assets to its next-version
-- address and then bumps this value. All pre-024 rows derive from the
-- original seed, hence DEFAULT 1. Additive with a default so old code
-- can keep running against the new schema during rollout.
ALTER TABLE accounts ADD COLUMN key_version BIGINT NOT NULL DEFAULT 1;
