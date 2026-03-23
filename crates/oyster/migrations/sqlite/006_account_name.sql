ALTER TABLE accounts ADD COLUMN name TEXT NOT NULL DEFAULT '';
UPDATE accounts SET name = id;
