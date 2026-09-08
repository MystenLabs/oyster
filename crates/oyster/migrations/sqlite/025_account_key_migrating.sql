-- Set while `oysterd keys migrate` is moving this account's on-chain
-- assets from its current key_version address to the target version's
-- address. Uploads, deletes and admin shrinks return 503 and the
-- extension worker skips the row until the tool clears it (on success
-- after bumping key_version, or on failure). NULL = not migrating. The
-- timestamp lets an operator spot a lock left behind by a crashed run
-- (`oysterd keys status`; clear with `keys migrate --break-lock`).
ALTER TABLE accounts ADD COLUMN key_migrating_since TEXT;
