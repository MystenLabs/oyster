ALTER TABLE accounts ADD COLUMN extend_attempt_after TEXT;
CREATE INDEX accounts_extend_due
    ON accounts (pool_end_epoch, extend_attempt_after)
    WHERE storage_pool_object_id IS NOT NULL;
