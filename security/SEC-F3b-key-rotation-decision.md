# SEC-F3b: Pearl master-seed rotation — decision record

Status: DRAFT, pending owner sign-off and Security re-check.

| Field | Value |
|---|---|
| Finding | F-3(b): master-seed rotation not possible in practice; leaked seed unremediable for existing accounts |
| Related | F-3(a) (seed moved to a dedicated Pearl identity), closed |
| Accountable owner | _name, role_ |
| Decision date | _YYYY-MM-DD_ |
| Security reviewer | Reginaldo Silva |
| Revisit by | _date_, or earlier on any trigger below |

## Decision

Oyster ships a rotation and migration path rather than accepting the
risk. `oysterd keys migrate` moves every account's on-chain assets
(`StoragePool`, SUI, WAL) from the address derived under the current
seed version to the address derived under a new one, signed with the
old key, then re-stamps the account. `oysterd keys sweep` recovers funds
that reach a retired address afterwards. The runbook is
`docs/src/guides/key-rotation.md`.

## Evidence

- Code: `crates/oyster/src/key_migration.rs`, CLI in `oysterd keys`.
- Automated exercise: `crates/oyster-e2e-tests/tests/key_rotation_e2e.rs`
  creates an account under seed version 1 with a funded wallet, a
  `StoragePool` and a stored blob, migrates it to version 2, and verifies
  reads, uploads, extension, idempotent re-runs, sweeps, the rotation
  lock, and refusal of an unconfigured target version.
- Testnet exercise against accounts created before this change:
  _date, operator, `keys status` before/after, tx digests_.

## Scope covered

- Rotation of the Pearl master seed for all existing and future
  accounts, including accounts holding on-chain assets.
- Recovery of funds sent to a retired address during the sweep window.

## Explicitly deferred: per-user key isolation

All accounts derive from one seed per version; a leak of that seed
exposes every wallet of that version until rotation completes. Per-user
isolation (independent key material per account, e.g. envelope-encrypted
in a KMS) is deferred because:

- rotation now bounds the damage window to the time it takes to run
  `keys migrate` across the fleet, which is minutes at current scale;
- the closed beta's custody value is small relative to the engineering
  cost of a per-account key store and its own backup/restore story;
- the migration primitive built here is exactly what a per-account
  scheme would use to onboard existing accounts, so the work is not lost.

Revisit triggers (any one):

- custody value across Pearl wallets exceeds _threshold_;
- general availability;
- a `keys migrate` fleet run takes longer than _N minutes_, i.e. the
  damage window is no longer small;
- any Pearl seed exposure incident.

## Residual risks accepted until then

- A seed leak is a race between the attacker and the operator; the
  runbook's "leaked seed" section is the mitigation, and rotation must be
  rehearsed on testnet so it can be run without hesitation.
- Accounts whose old address holds a pool but no SUI cannot be moved
  until funded (no gas sponsor). `keys migrate` reports them.
- Integrators that cached a funding address keep paying the retired one
  until they read it from the webhook; the sweep window covers this.

## Sign-off

- Owner: _signature/date_
- Security re-check: _Reginaldo Silva, date, what was verified_
