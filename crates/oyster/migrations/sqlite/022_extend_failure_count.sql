-- Consecutive extension-attempt failures for the account's StoragePool.
-- Drives the extension task's exponential retry backoff: the next
-- extend_attempt_after stamp grows with this count (capped by
-- EXTENSION_BACKOFF_CAP_SECS). Reset to 0 on a successful extension.
-- Additive with a default so rows written by pre-022 code are unaffected
-- and old code can keep running against the new schema during rollout.
ALTER TABLE accounts ADD COLUMN extend_failure_count BIGINT NOT NULL DEFAULT 0;
