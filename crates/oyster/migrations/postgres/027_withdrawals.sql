-- Admin-initiated fund withdrawals from an account's Pearl wallet.
--
-- Egress of SUI/WAL is the most sensitive admin operation, so it is
-- gated by two controls a single leaked admin key cannot satisfy alone:
--
-- 1. The destination must be registered in `withdrawal_addresses` ahead
--    of time and is only usable after a cooldown (`usable_at`). Every
--    registration is audited and pushed to the app's webhook, so the
--    legitimate operator sees a hostile registration while there is
--    still time to revoke the compromised key.
-- 2. A withdrawal is a two-step request/approve flow recorded in
--    `withdrawals`; the approving admin key must differ from the
--    requesting one. Admin keys cannot mint admin keys through the API,
--    so a thief with one key cannot self-approve.
CREATE TABLE withdrawal_addresses (
    account_id TEXT PRIMARY KEY NOT NULL REFERENCES accounts(id),
    -- Sui address (0x + 64 hex), normalized.
    address TEXT NOT NULL,
    registered_by_admin_key_id TEXT NOT NULL,
    registered_at TEXT NOT NULL,
    -- Earliest time a withdrawal to `address` may be approved.
    usable_at TEXT NOT NULL
);

CREATE TABLE withdrawals (
    id TEXT PRIMARY KEY NOT NULL,
    account_id TEXT NOT NULL REFERENCES accounts(id),
    app_id TEXT NOT NULL REFERENCES apps(id),
    -- Snapshot of the registered address at request time; re-checked
    -- against `withdrawal_addresses` at approval.
    destination TEXT NOT NULL,
    -- NULL when not withdrawing that token; ignored when drain = 1.
    sui_mist BIGINT,
    wal_frost BIGINT,
    -- 1: move every SUI and WAL coin, leaving the wallet empty.
    drain BIGINT NOT NULL DEFAULT 0,
    -- pending | executing | completed | failed | cancelled
    status TEXT NOT NULL,
    requested_by_admin_key_id TEXT NOT NULL,
    approved_by_admin_key_id TEXT,
    tx_digest TEXT,
    error TEXT,
    created_at TEXT NOT NULL,
    -- A pending request that is not approved by this time is dead.
    expires_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX idx_withdrawals_account_created ON withdrawals(account_id, created_at);
CREATE INDEX idx_withdrawals_status ON withdrawals(status);
