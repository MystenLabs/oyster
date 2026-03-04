-- Remove vestigial credentials column and index.
DROP INDEX IF EXISTS idx_accounts_credentials;
ALTER TABLE accounts DROP COLUMN credentials;
