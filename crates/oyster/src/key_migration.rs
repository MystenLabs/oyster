//! Pearl master-seed rotation (SEC-F3b).
//!
//! Every account's wallet is an Ed25519 key derived from one of Pearl's
//! versioned master seeds (`accounts.key_version`). Rotating to a new
//! seed only helps if existing accounts can be moved off the old one,
//! which means moving their *on-chain assets* to the address the new
//! seed derives, then re-stamping the row. This module is that move.
//!
//! What an Oyster wallet owns on-chain is small and well defined: at most
//! one `StoragePool` (whose `PooledBlob`s live inside the pool's object
//! table and travel with it), SUI coins (gas), WAL coins (storage
//! payment), and occasionally a Walrus `Storage`/`Blob` object left by an
//! admin shrink. All of these have the `store` ability, so a plain
//! `TransferObjects` moves them. Nothing in the database caches the
//! derived address, so the version flip is the only state change.
//!
//! Per-account procedure (`migrate`):
//! 1. Take the rotation lock (`accounts.key_migrating_since`, CAS on the
//!    current version). While held, uploads/deletes/admin shrinks answer
//!    503 and the extension worker skips the row.
//! 2. Derive both addresses from Pearl; refuse early if Pearl has no seed
//!    for the target version.
//! 3. Repeatedly list the old address's owned objects and transfer them
//!    in chunks, signed by Pearl under the *old* version. The last chunk
//!    also transfers the gas coin itself (pay-all-SUI style), so nothing
//!    is left behind. Unknown object types are skipped and reported
//!    rather than risking an abort of the whole transaction.
//! 4. Verify the account's `StoragePool` (if the DB knows of one) is now
//!    owned by the new address.
//! 5. Re-stamp `key_version` and release the lock.
//!
//! Every step is idempotent: a re-run after a crash finds either nothing
//! left to move (and just flips), or a lock to break, or an address that
//! still holds objects.
//!
//! `sweep` is the same transfer without lock or flip, for funds that
//! land on an old-version address after the flip (integrators that
//! cached the funding address). Keep the old seed configured in Pearl,
//! inactive, for as long as sweeps are still finding anything.

use std::{fmt, time::Duration};

use chrono::Utc;
use sui_sdk::{
    SuiClient, SuiClientBuilder,
    rpc_types::{SuiObjectDataOptions, SuiObjectResponseQuery, SuiTransactionBlockEffectsAPI},
};
use sui_types::{
    TypeTag,
    base_types::{ObjectID, ObjectRef, ObjectType, SuiAddress},
    digests::TransactionDigest,
    object::Owner,
    programmable_transaction_builder::ProgrammableTransactionBuilder,
    transaction::{Argument, Command, ObjectArg, TransactionData},
};

use crate::{
    AccountId,
    db::{self, DbPool, accounts::KeyMigrationCandidate},
    pearl_client::PearlConnection,
    sui_transaction::{self, SignAndSubmitError},
};

/// Upper bound on objects moved per transaction. Well under Sui's
/// input-object and per-command argument limits, so a wallet with many
/// coin objects is moved in a few transactions rather than one that
/// risks a protocol-limit rejection.
pub const MAX_OBJECTS_PER_TX: usize = 256;

/// Guard against a runaway loop if an address keeps gaining objects
/// while we drain it (each iteration re-lists the address).
const MAX_TRANSFER_ROUNDS: usize = 64;

/// Floor for the gas budget; transfers are cheap, but the dry-run
/// estimate can be slightly below the real cost when storage rebates
/// shift between checkpoints.
const MIN_GAS_BUDGET: u64 = 2_000_000;

/// Budget used for the dry run that estimates the real budget. Must be
/// ≤ the gas coin balance or the dry run itself is rejected.
const DRY_RUN_BUDGET_CAP: u64 = 500_000_000;

/// What kind of on-chain object we found at the old address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectClass {
    /// `Coin<SUI>` — gas.
    SuiCoin,
    /// `Coin<WAL>` — storage payment.
    WalCoin,
    /// Any other coin type (transferable; reported separately).
    OtherCoin,
    /// `walrus::storage_pool::StoragePool` — the account's pool, with all
    /// its `PooledBlob`s inside.
    StoragePool,
    /// `walrus::storage_resource::Storage` — capacity extracted by an
    /// admin shrink and transferred back to the wallet.
    WalrusStorage,
    /// `walrus::blob::Blob` — a non-pooled blob object.
    WalrusBlob,
    /// Anything else. Not transferred (it may lack `store`, and one
    /// abort would fail the whole transaction); listed in the report.
    Unknown,
}

impl ObjectClass {
    /// Whether the tool moves objects of this class.
    pub fn is_transferable(self) -> bool {
        !matches!(self, ObjectClass::Unknown)
    }
}

/// One object owned by the address being drained.
#[derive(Debug, Clone)]
pub struct OwnedObject {
    /// `(id, version, digest)` as of the listing.
    pub object_ref: ObjectRef,
    /// Fully-qualified Move type, for the report.
    pub type_name: String,
    /// Classification driving transfer/skip.
    pub class: ObjectClass,
}

impl OwnedObject {
    /// Object ID.
    pub fn id(&self) -> ObjectID {
        self.object_ref.0
    }
}

/// Classify an object by its Move type. `wal_coin_type` is the network's
/// WAL coin type (from `SuiReadClient::wal_coin_type`), when known.
pub fn classify(object_type: &ObjectType, wal_coin_type: Option<&TypeTag>) -> ObjectClass {
    let ObjectType::Struct(move_type) = object_type else {
        return ObjectClass::Unknown;
    };
    if move_type.is_gas_coin() {
        return ObjectClass::SuiCoin;
    }
    if move_type.is_coin() {
        return match (move_type.coin_type_maybe(), wal_coin_type) {
            (Some(inner), Some(wal)) if &inner == wal => ObjectClass::WalCoin,
            _ => ObjectClass::OtherCoin,
        };
    }
    match (move_type.module().as_str(), move_type.name().as_str()) {
        ("storage_pool", "StoragePool") => ObjectClass::StoragePool,
        ("storage_resource", "Storage") => ObjectClass::WalrusStorage,
        ("blob", "Blob") => ObjectClass::WalrusBlob,
        _ => ObjectClass::Unknown,
    }
}

/// Result of draining one address into another.
#[derive(Debug, Clone, Default)]
pub struct TransferOutcome {
    /// Digests of the submitted transactions, in order.
    pub digests: Vec<TransactionDigest>,
    /// Objects moved, including the gas coin.
    pub moved: usize,
    /// Objects left behind because their type is not one we move.
    pub skipped: Vec<OwnedObject>,
}

/// Why an account could not be migrated.
#[derive(Debug, thiserror::Error)]
pub enum MigrationError {
    /// Pearl refused to derive the target version (seed not configured)
    /// or another Pearl call failed.
    #[error("pearl: {0}")]
    Pearl(String),
    /// The old address holds transferable objects but no SUI to pay for
    /// the move. Fund `address` with a little SUI and re-run (a gas
    /// sponsor is not supported yet).
    #[error("address {address} holds {objects} object(s) but no SUI for gas; fund it and re-run")]
    NeedsGas {
        /// The old-version address.
        address: SuiAddress,
        /// Number of transferable objects waiting there.
        objects: usize,
    },
    /// The DB's `StoragePool` for the account is owned by neither the
    /// old nor the new address. Refusing to flip the version.
    #[error(
        "storage pool {pool} owned by {owner:?}, expected {expected}; not flipping key_version"
    )]
    PoolOwnerMismatch {
        /// The pool object.
        pool: ObjectID,
        /// Its on-chain owner (None: shared/immutable/deleted).
        owner: Option<SuiAddress>,
        /// The address it should have ended up at.
        expected: SuiAddress,
    },
    /// The DB row changed under us (someone else flipped or locked it).
    #[error("account row changed during migration: {0}")]
    Raced(String),
    /// Sui RPC / transaction failure.
    #[error("sui: {0}")]
    Sui(String),
    /// Database failure.
    #[error("database: {0}")]
    Database(#[from] sqlx::Error),
}

/// Per-account result of a `migrate` or `sweep` pass.
#[derive(Debug)]
pub enum AccountOutcome {
    /// `migrate`: the account is already at (or above) the target version.
    AlreadyAtVersion,
    /// `sweep`: the account still derives from the version being swept;
    /// run `migrate` for it instead.
    StillOnVersion,
    /// `migrate`: the account is locked by another (or a crashed) run.
    /// Re-run with `break_lock` once you are sure nothing else is
    /// touching it.
    Locked {
        /// When the lock was taken.
        since: String,
    },
    /// Dry run: what would move.
    Planned {
        /// Old-version address.
        from: SuiAddress,
        /// New-version address.
        to: SuiAddress,
        /// Objects that would be transferred.
        movable: Vec<OwnedObject>,
        /// Objects that would be left behind.
        skipped: Vec<OwnedObject>,
    },
    /// The old address held nothing; the version was flipped (migrate)
    /// or there was nothing to sweep.
    NothingOnChain,
    /// Assets moved (and, for `migrate`, the version flipped).
    Moved(TransferOutcome),
    /// The account was not migrated; the lock (if taken) was released.
    Failed(MigrationError),
}

impl AccountOutcome {
    /// Whether the operator needs to act on this account.
    pub fn is_problem(&self) -> bool {
        matches!(
            self,
            AccountOutcome::Locked { .. } | AccountOutcome::Failed(_)
        )
    }
}

impl fmt::Display for AccountOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AccountOutcome::AlreadyAtVersion => write!(f, "already-at-version"),
            AccountOutcome::StillOnVersion => write!(f, "still-on-version (use migrate)"),
            AccountOutcome::Locked { since } => write!(f, "locked since {since}"),
            AccountOutcome::Planned {
                from,
                to,
                movable,
                skipped,
            } => {
                write!(f, "plan {from} -> {to}: move {} object(s)", movable.len())?;
                for o in movable {
                    write!(f, " [{:?} {}]", o.class, o.id())?;
                }
                if !skipped.is_empty() {
                    write!(f, "; skip {}", skipped.len())?;
                    for o in skipped {
                        write!(f, " [{} {}]", o.type_name, o.id())?;
                    }
                }
                Ok(())
            }
            AccountOutcome::NothingOnChain => write!(f, "nothing-on-chain"),
            AccountOutcome::Moved(t) => {
                write!(f, "moved {} object(s) in {} tx", t.moved, t.digests.len())?;
                for d in &t.digests {
                    write!(f, " {d}")?;
                }
                if !t.skipped.is_empty() {
                    write!(f, "; skipped {}", t.skipped.len())?;
                    for o in &t.skipped {
                        write!(f, " [{} {}]", o.type_name, o.id())?;
                    }
                }
                Ok(())
            }
            AccountOutcome::Failed(e) => write!(f, "FAILED: {e}"),
        }
    }
}

/// Everything the tool needs to talk to the database, Pearl, and Sui.
pub struct MigrationContext {
    /// Oyster database.
    pub db: DbPool,
    /// Pearl signer.
    pub pearl: PearlConnection,
    /// Sui JSON-RPC client (object listing, dry runs).
    pub sui: SuiClient,
    /// Sui RPC URL (gRPC execution via `sign_and_submit`).
    pub rpc_url: String,
    /// The network's WAL coin type, for classification only.
    pub wal_coin_type: Option<TypeTag>,
}

impl MigrationContext {
    /// Connect to Sui and resolve the WAL coin type from the Walrus
    /// system object.
    pub async fn new(
        db: DbPool,
        pearl: PearlConnection,
        rpc_url: &str,
        system_object: ObjectID,
        staking_object: ObjectID,
    ) -> Result<Self, MigrationError> {
        let sui = SuiClientBuilder::default()
            .build(rpc_url)
            .await
            .map_err(|e| MigrationError::Sui(format!("connect {rpc_url}: {e}")))?;
        let read_client =
            sui_transaction::build_sui_read_client(rpc_url, system_object, staking_object)
                .await
                .map_err(|e| MigrationError::Sui(format!("build read client: {e}")))?;
        let wal_coin_type = sui_types::parse_sui_type_tag(read_client.wal_coin_type()).ok();
        Ok(Self {
            db,
            pearl,
            sui,
            rpc_url: rpc_url.to_string(),
            wal_coin_type,
        })
    }

    async fn derive_address(
        &self,
        account_id: &AccountId,
        version: u32,
    ) -> Result<SuiAddress, MigrationError> {
        sui_transaction::resolve_sender_address(&self.pearl, account_id, version)
            .await
            .map_err(|e| MigrationError::Pearl(format!("derive version {version}: {e}")))
    }

    /// Every object owned by `address`, classified.
    pub async fn list_owned(
        &self,
        address: SuiAddress,
    ) -> Result<Vec<OwnedObject>, MigrationError> {
        let mut out = Vec::new();
        let mut cursor = None;
        loop {
            let page = self
                .sui
                .read_api()
                .get_owned_objects(
                    address,
                    Some(SuiObjectResponseQuery::new_with_options(
                        SuiObjectDataOptions::new().with_type(),
                    )),
                    cursor,
                    None,
                )
                .await
                .map_err(|e| MigrationError::Sui(format!("get_owned_objects {address}: {e}")))?;
            for resp in page.data {
                let Some(data) = resp.data else { continue };
                let Some(object_type) = data.type_.as_ref() else {
                    continue;
                };
                out.push(OwnedObject {
                    object_ref: data.object_ref(),
                    type_name: object_type.to_string(),
                    class: classify(object_type, self.wal_coin_type.as_ref()),
                });
            }
            if !page.has_next_page {
                break;
            }
            cursor = page.next_cursor;
        }
        Ok(out)
    }

    /// Current balance of a coin object, or `None` if it no longer
    /// exists.
    async fn coin_balance(
        &self,
        address: SuiAddress,
        id: ObjectID,
    ) -> Result<Option<u64>, MigrationError> {
        // `get_coins` is the only JSON-RPC surface that returns balances
        // without parsing Move contents; SUI coins per address are few.
        let mut cursor = None;
        loop {
            let page = self
                .sui
                .coin_read_api()
                .get_coins(address, Some("0x2::sui::SUI".into()), cursor, None)
                .await
                .map_err(|e| MigrationError::Sui(format!("get_coins {address}: {e}")))?;
            if let Some(c) = page.data.iter().find(|c| c.coin_object_id == id) {
                return Ok(Some(c.balance));
            }
            if !page.has_next_page {
                return Ok(None);
            }
            cursor = page.next_cursor;
        }
    }

    /// Address-owner of `id`, or `None` when shared, immutable, wrapped,
    /// or deleted.
    pub async fn object_owner(&self, id: ObjectID) -> Result<Option<SuiAddress>, MigrationError> {
        let resp = self
            .sui
            .read_api()
            .get_object_with_options(id, SuiObjectDataOptions::new().with_owner())
            .await
            .map_err(|e| MigrationError::Sui(format!("get_object {id}: {e}")))?;
        Ok(resp.data.and_then(|d| d.owner).and_then(|o| match o {
            Owner::AddressOwner(a) => Some(a),
            _ => None,
        }))
    }

    /// Move everything transferable at `from` to `to`, signing as
    /// `account_id` under `sign_version` (the version `from` derives
    /// from). Loops until `from` holds nothing we move.
    pub async fn drain_address(
        &self,
        account_id: &AccountId,
        sign_version: u32,
        from: SuiAddress,
        to: SuiAddress,
    ) -> Result<TransferOutcome, MigrationError> {
        let mut outcome = TransferOutcome::default();
        for _round in 0..MAX_TRANSFER_ROUNDS {
            let owned = self.list_owned(from).await?;
            let (movable, skipped): (Vec<_>, Vec<_>) =
                owned.into_iter().partition(|o| o.class.is_transferable());
            outcome.skipped = skipped;
            if movable.is_empty() {
                return Ok(outcome);
            }

            // Largest SUI coin pays; it is transferred last, with the
            // final chunk.
            let mut best: Option<(ObjectRef, u64)> = None;
            for o in movable.iter().filter(|o| o.class == ObjectClass::SuiCoin) {
                let bal = self.coin_balance(from, o.id()).await?.unwrap_or(0);
                if best.as_ref().is_none_or(|(_, b)| bal > *b) {
                    best = Some((o.object_ref, bal));
                }
            }
            let Some((gas_ref, gas_balance)) = best else {
                return Err(MigrationError::NeedsGas {
                    address: from,
                    objects: movable.len(),
                });
            };

            let others: Vec<ObjectRef> = movable
                .iter()
                .filter(|o| o.id() != gas_ref.0)
                .map(|o| o.object_ref)
                .collect();
            let chunk: Vec<ObjectRef> = others.iter().take(MAX_OBJECTS_PER_TX).copied().collect();
            let is_last = chunk.len() == others.len();

            let tx = self
                .build_transfer_tx(from, to, gas_ref, gas_balance, &chunk, is_last)
                .await?;
            let digest = self.submit(account_id, sign_version, tx).await?;
            outcome.digests.push(digest);
            outcome.moved += chunk.len() + usize::from(is_last);
            if is_last {
                // One more listing confirms nothing new arrived meanwhile.
                continue;
            }
        }
        Err(MigrationError::Sui(format!(
            "address {from} still holds objects after {MAX_TRANSFER_ROUNDS} transfer rounds"
        )))
    }

    /// One `TransferObjects` transaction, gas-estimated by dry run. When
    /// `transfer_gas_coin` is set the gas coin itself goes to `to` as
    /// well (its balance minus the fee).
    async fn build_transfer_tx(
        &self,
        from: SuiAddress,
        to: SuiAddress,
        gas_ref: ObjectRef,
        gas_balance: u64,
        objects: &[ObjectRef],
        transfer_gas_coin: bool,
    ) -> Result<TransactionData, MigrationError> {
        let build = |budget: u64, price: u64| -> Result<TransactionData, MigrationError> {
            let mut b = ProgrammableTransactionBuilder::new();
            let mut args = Vec::with_capacity(objects.len() + 1);
            for r in objects {
                args.push(
                    b.obj(ObjectArg::ImmOrOwnedObject(*r))
                        .map_err(|e| MigrationError::Sui(format!("obj arg: {e}")))?,
                );
            }
            if transfer_gas_coin {
                args.push(Argument::GasCoin);
            }
            let recipient = b
                .pure(to)
                .map_err(|e| MigrationError::Sui(format!("recipient arg: {e}")))?;
            b.command(Command::TransferObjects(args, recipient));
            Ok(TransactionData::new_programmable(
                from,
                vec![gas_ref],
                b.finish(),
                budget,
                price,
            ))
        };

        // A coin below the budget floor cannot even pay for the dry run;
        // say so plainly instead of surfacing an RPC error.
        if gas_balance < MIN_GAS_BUDGET {
            return Err(MigrationError::NeedsGas {
                address: from,
                objects: objects.len() + usize::from(transfer_gas_coin),
            });
        }

        let gas_price = self
            .sui
            .read_api()
            .get_reference_gas_price()
            .await
            .map_err(|e| MigrationError::Sui(format!("gas price: {e}")))?;

        let probe = build(gas_balance.min(DRY_RUN_BUDGET_CAP), gas_price)?;
        let dry = self
            .sui
            .read_api()
            .dry_run_transaction_block(probe)
            .await
            .map_err(|e| MigrationError::Sui(format!("dry run: {e}")))?;
        if let sui_sdk::rpc_types::SuiExecutionStatus::Failure { error } = dry.effects.status() {
            return Err(MigrationError::Sui(format!("dry run failed: {error}")));
        }
        let cost = dry.effects.gas_cost_summary();
        let needed = cost.computation_cost.saturating_add(cost.storage_cost);
        let budget = needed.saturating_mul(3) / 2;
        let budget = budget.max(MIN_GAS_BUDGET);
        if gas_balance < needed.max(MIN_GAS_BUDGET) {
            return Err(MigrationError::NeedsGas {
                address: from,
                objects: objects.len() + usize::from(transfer_gas_coin),
            });
        }
        build(budget.min(gas_balance), gas_price)
    }

    async fn submit(
        &self,
        account_id: &AccountId,
        sign_version: u32,
        tx: TransactionData,
    ) -> Result<TransactionDigest, MigrationError> {
        match sui_transaction::sign_and_submit(
            &self.pearl,
            account_id,
            sign_version,
            &self.rpc_url,
            tx,
        )
        .await
        {
            Ok(outcome) => Ok(outcome.digest),
            Err(SignAndSubmitError::ExecutionFailure(f)) => Err(MigrationError::Sui(f.to_string())),
            Err(SignAndSubmitError::Other(e)) => Err(MigrationError::Sui(e.to_string())),
        }
    }

    /// Migrate one account to `to_version`. See the module docs for the
    /// procedure. `dry_run` lists what would move without locking or
    /// submitting; `break_lock` clears a lock left by a crashed run.
    pub async fn migrate_account(
        &self,
        cand: &KeyMigrationCandidate,
        to_version: u32,
        dry_run: bool,
        break_lock: bool,
    ) -> AccountOutcome {
        if cand.key_version >= to_version {
            return AccountOutcome::AlreadyAtVersion;
        }
        if let Some(since) = &cand.key_migrating_since {
            if !break_lock {
                return AccountOutcome::Locked {
                    since: since.clone(),
                };
            }
            if !dry_run
                && let Err(e) =
                    db::accounts::clear_key_migration_lock(&self.db, &cand.account_id).await
            {
                return AccountOutcome::Failed(e.into());
            }
        }

        // Resolve both addresses first: an unconfigured target seed must
        // fail before anything is locked or moved.
        let (from, to) = match self
            .resolve_pair(&cand.account_id, cand.key_version, to_version)
            .await
        {
            Ok(pair) => pair,
            Err(e) => return AccountOutcome::Failed(e),
        };

        if dry_run {
            return match self.list_owned(from).await {
                Ok(owned) => {
                    let (movable, skipped) =
                        owned.into_iter().partition(|o| o.class.is_transferable());
                    AccountOutcome::Planned {
                        from,
                        to,
                        movable,
                        skipped,
                    }
                }
                Err(e) => AccountOutcome::Failed(e),
            };
        }

        match db::accounts::begin_key_migration(
            &self.db,
            &cand.account_id,
            cand.key_version,
            Utc::now(),
        )
        .await
        {
            Ok(true) => {}
            Ok(false) => {
                return AccountOutcome::Failed(MigrationError::Raced(
                    "could not take rotation lock (row changed or locked concurrently)".into(),
                ));
            }
            Err(e) => return AccountOutcome::Failed(e.into()),
        }

        let result = self.move_and_verify(cand, from, to).await;
        match result {
            Ok(transfer) => {
                match db::accounts::finish_key_migration(
                    &self.db,
                    &cand.account_id,
                    cand.key_version,
                    to_version,
                )
                .await
                {
                    Ok(true) => {}
                    Ok(false) => {
                        // Assets are at `to`; the row must be repaired by hand.
                        return AccountOutcome::Failed(MigrationError::Raced(format!(
                            "assets moved to {to} but key_version flip matched no row \
                             (expected version {}); set key_version = {to_version} manually",
                            cand.key_version
                        )));
                    }
                    Err(e) => return AccountOutcome::Failed(e.into()),
                }
                if transfer.moved == 0 && transfer.digests.is_empty() {
                    AccountOutcome::NothingOnChain
                } else {
                    AccountOutcome::Moved(transfer)
                }
            }
            Err(e) => {
                if let Err(unlock) =
                    db::accounts::clear_key_migration_lock(&self.db, &cand.account_id).await
                {
                    tracing::error!(account = %cand.account_id, error = %unlock, "could not release rotation lock");
                }
                AccountOutcome::Failed(e)
            }
        }
    }

    async fn resolve_pair(
        &self,
        account_id: &AccountId,
        from_version: u32,
        to_version: u32,
    ) -> Result<(SuiAddress, SuiAddress), MigrationError> {
        let to = self.derive_address(account_id, to_version).await?;
        let from = self.derive_address(account_id, from_version).await?;
        if from == to {
            return Err(MigrationError::Pearl(format!(
                "versions {from_version} and {to_version} derive the same address {from}; \
                 are both seeds configured and distinct?"
            )));
        }
        Ok((from, to))
    }

    /// Drain `from` into `to`, then insist the DB's pool (if any) sits at
    /// `to`. A pool that is already at `to` (previous partial run) is
    /// fine; anywhere else is an error.
    async fn move_and_verify(
        &self,
        cand: &KeyMigrationCandidate,
        from: SuiAddress,
        to: SuiAddress,
    ) -> Result<TransferOutcome, MigrationError> {
        let transfer = self
            .drain_address(&cand.account_id, cand.key_version, from, to)
            .await?;
        if let Some(pool) = &cand.storage_pool_object_id {
            let pool: ObjectID = pool
                .parse()
                .map_err(|e| MigrationError::Sui(format!("invalid pool id {pool}: {e}")))?;
            let owner = self.object_owner(pool).await?;
            if owner != Some(to) {
                return Err(MigrationError::PoolOwnerMismatch {
                    pool,
                    owner,
                    expected: to,
                });
            }
        }
        Ok(transfer)
    }

    /// Sweep residual assets from the `from_version` address of an
    /// account that has already been migrated past it, into its current
    /// address. No lock, no version change.
    pub async fn sweep_account(
        &self,
        cand: &KeyMigrationCandidate,
        from_version: u32,
        dry_run: bool,
    ) -> AccountOutcome {
        if cand.key_version <= from_version {
            return AccountOutcome::StillOnVersion;
        }
        let (from, to) = match self
            .resolve_pair(&cand.account_id, from_version, cand.key_version)
            .await
        {
            Ok(pair) => pair,
            Err(e) => return AccountOutcome::Failed(e),
        };
        if dry_run {
            return match self.list_owned(from).await {
                Ok(owned) => {
                    let (movable, skipped) =
                        owned.into_iter().partition(|o| o.class.is_transferable());
                    AccountOutcome::Planned {
                        from,
                        to,
                        movable,
                        skipped,
                    }
                }
                Err(e) => AccountOutcome::Failed(e),
            };
        }
        match self
            .drain_address(&cand.account_id, from_version, from, to)
            .await
        {
            Ok(t) if t.digests.is_empty() => AccountOutcome::NothingOnChain,
            Ok(t) => AccountOutcome::Moved(t),
            Err(e) => AccountOutcome::Failed(e),
        }
    }
}

/// A `migrate`/`sweep` pass over several accounts.
#[derive(Debug, Default)]
pub struct Report {
    /// Per-account outcomes in processing order.
    pub outcomes: Vec<(KeyMigrationCandidate, AccountOutcome)>,
}

impl Report {
    /// Number of accounts the operator must look at.
    pub fn problems(&self) -> usize {
        self.outcomes.iter().filter(|(_, o)| o.is_problem()).count()
    }

    /// Number of accounts whose assets moved.
    pub fn moved(&self) -> usize {
        self.outcomes
            .iter()
            .filter(|(_, o)| matches!(o, AccountOutcome::Moved(_)))
            .count()
    }
}

/// Migrate every account below `to_version` (or just `only`), pausing
/// briefly between accounts so a large fleet does not hammer the RPC.
pub async fn migrate(
    ctx: &MigrationContext,
    to_version: u32,
    only: Option<&AccountId>,
    dry_run: bool,
    break_lock: bool,
) -> Result<Report, MigrationError> {
    let candidates = match only {
        Some(id) => db::accounts::get_key_migration_candidate(&ctx.db, id)
            .await?
            .into_iter()
            .collect(),
        None => {
            db::accounts::list_key_migration_candidates(
                &ctx.db,
                db::accounts::KeyVersionFilter::Below(to_version),
            )
            .await?
        }
    };
    let mut report = Report::default();
    for cand in candidates {
        let outcome = ctx
            .migrate_account(&cand, to_version, dry_run, break_lock)
            .await;
        tracing::info!(account = %cand.account_id, name = %cand.name, from = cand.key_version, to = to_version, %outcome, "keys migrate");
        report.outcomes.push((cand, outcome));
        if !dry_run {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    Ok(report)
}

/// Sweep every account already past `from_version` (or just `only`).
pub async fn sweep(
    ctx: &MigrationContext,
    from_version: u32,
    only: Option<&AccountId>,
    dry_run: bool,
) -> Result<Report, MigrationError> {
    let candidates = match only {
        Some(id) => db::accounts::get_key_migration_candidate(&ctx.db, id)
            .await?
            .into_iter()
            .collect(),
        None => {
            db::accounts::list_key_migration_candidates(
                &ctx.db,
                db::accounts::KeyVersionFilter::Above(from_version),
            )
            .await?
        }
    };
    let mut report = Report::default();
    for cand in candidates {
        let outcome = ctx.sweep_account(&cand, from_version, dry_run).await;
        tracing::info!(account = %cand.account_id, name = %cand.name, from = from_version, to = cand.key_version, %outcome, "keys sweep");
        report.outcomes.push((cand, outcome));
        if !dry_run {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use sui_types::base_types::MoveObjectType;

    use super::*;

    fn object_type(s: &str) -> ObjectType {
        ObjectType::Struct(MoveObjectType::from(
            sui_types::parse_sui_struct_tag(s).unwrap(),
        ))
    }

    const WAL: &str =
        "0x9f992cc2430a1f442ca7a5ca7638169f5d5c00e0ebc3977a65e9ac6e497fe5ef::wal::WAL";
    const POOL: &str = "0x795ddbc26b8cfff2551f45e198b87fc19473f2df50f995376b924ac80e56f88b::storage_pool::StoragePool";

    #[test]
    fn classifies_gas_and_wal_coins() {
        let wal = sui_types::parse_sui_type_tag(WAL).unwrap();
        assert_eq!(
            classify(&object_type("0x2::coin::Coin<0x2::sui::SUI>"), Some(&wal)),
            ObjectClass::SuiCoin
        );
        assert_eq!(
            classify(&object_type(&format!("0x2::coin::Coin<{WAL}>")), Some(&wal)),
            ObjectClass::WalCoin
        );
        // Without a known WAL type, WAL is still a transferable coin.
        assert_eq!(
            classify(&object_type(&format!("0x2::coin::Coin<{WAL}>")), None),
            ObjectClass::OtherCoin
        );
        assert_eq!(
            classify(
                &object_type("0x2::coin::Coin<0xabc::usdc::USDC>"),
                Some(&wal)
            ),
            ObjectClass::OtherCoin
        );
    }

    #[test]
    fn classifies_walrus_objects_by_module_and_name() {
        assert_eq!(classify(&object_type(POOL), None), ObjectClass::StoragePool);
        assert_eq!(
            classify(&object_type("0x795d::storage_resource::Storage"), None),
            ObjectClass::WalrusStorage
        );
        assert_eq!(
            classify(&object_type("0x795d::blob::Blob"), None),
            ObjectClass::WalrusBlob
        );
    }

    #[test]
    fn unknown_types_are_not_transferable() {
        let c = classify(&object_type("0xdead::kiosk::KioskOwnerCap"), None);
        assert_eq!(c, ObjectClass::Unknown);
        assert!(!c.is_transferable());
        assert!(!classify(&ObjectType::Package, None).is_transferable());
        for c in [
            ObjectClass::SuiCoin,
            ObjectClass::WalCoin,
            ObjectClass::OtherCoin,
            ObjectClass::StoragePool,
            ObjectClass::WalrusStorage,
            ObjectClass::WalrusBlob,
        ] {
            assert!(c.is_transferable(), "{c:?}");
        }
    }

    #[test]
    fn outcome_problem_flags() {
        assert!(AccountOutcome::Locked { since: "t".into() }.is_problem());
        assert!(AccountOutcome::Failed(MigrationError::Pearl("x".into())).is_problem());
        assert!(!AccountOutcome::AlreadyAtVersion.is_problem());
        assert!(!AccountOutcome::NothingOnChain.is_problem());
        assert!(!AccountOutcome::Moved(TransferOutcome::default()).is_problem());
    }
}
