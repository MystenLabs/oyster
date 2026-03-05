-- Remove key material from the database.
-- Keys are now derived on-the-fly from PEARL_MASTER_SEED + account_id.
DROP INDEX IF EXISTS idx_accounts_address;
ALTER TABLE accounts DROP COLUMN private_key;
ALTER TABLE accounts DROP COLUMN address;
