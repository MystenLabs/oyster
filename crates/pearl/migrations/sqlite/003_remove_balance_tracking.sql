-- Remove balance tracking columns and pending transactions table.
-- These responsibilities are moving to Oyster (Phase 15).
ALTER TABLE accounts DROP COLUMN cached_sui_balance;
ALTER TABLE accounts DROP COLUMN cached_wal_balance;
ALTER TABLE accounts DROP COLUMN balance_updated_at;
ALTER TABLE accounts DROP COLUMN min_sui_balance;
ALTER TABLE accounts DROP COLUMN min_wal_balance;
ALTER TABLE accounts DROP COLUMN top_up_target_sui;
ALTER TABLE accounts DROP COLUMN top_up_target_wal;
ALTER TABLE accounts DROP COLUMN due_date;
DROP TABLE IF EXISTS pending_transactions;
