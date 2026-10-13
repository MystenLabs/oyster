//! Repair of `PooledBlob`s that are registered but not certified
//! (`oysterd blobs repair`).
//!
//! An upload is register → sliver upload → certify, and Walrus serves a
//! blob only once it is certified. Before 0.14.4, a `PUT` whose certify
//! transaction failed left the `PooledBlob` registered on chain and
//! answered `502`; the client's retry then hit the register
//! `EFieldAlreadyExists` self-heal, which returned `201` without
//! certifying and wrote a `blobs` row. Those blobs are unreadable, their
//! rows short-circuited every later `PUT` of the same content, and the
//! client had long since discarded the bytes. The upload path now
//! repairs such a blob the next time the same content is uploaded; this
//! module is the bulk pass for the ones nobody re-uploads.
//!
//! A candidate is one `(account, Walrus blob id)` with its on-chain
//! `PooledBlob`. Candidates come from the database (`blobs` rows that
//! reference a `PooledBlob` but have no `certified_epoch`) and/or from a
//! list of `PooledBlob` object IDs (`--ids-file`), which also covers
//! orphans with no row at all: a `502`'d upload the client never
//! retried.
//!
//! Per candidate:
//! 1. Read the `PooledBlob`. Gone from chain → [`Outcome::MissingOnChain`].
//! 2. Already certified → stamp the rows → [`Outcome::Stamped`].
//! 3. Otherwise ask the storage nodes for a confirmation certificate
//!    *without* re-uploading: the first attempt did push the slivers
//!    before its certify failed, and nodes keep uncertified slivers for
//!    a while. Certificate in hand → certify → stamp →
//!    [`Outcome::Certified`].
//! 4. No certificate → the bytes are gone from Walrus and Oyster never
//!    kept a copy. With `delete_unrecoverable`: delete the `PooledBlob`
//!    (frees pool capacity and lets a future `PUT` of the same content
//!    register afresh), drop the rows, and report the keys so the
//!    customer can be told → [`Outcome::Deleted`]. Otherwise
//!    [`Outcome::Unrecoverable`].
//!
//! Every step is idempotent; re-running after a crash picks up where it
//! left off. A dry run reads the chain and the storage nodes and reports
//! what each candidate would get, submitting nothing.

use std::{collections::BTreeMap, fmt, time::Duration};

use sui_types::base_types::ObjectID;

use crate::{
    AccountId,
    blob_store::{BlobId, BlobStore as _, BlobStoreError},
    db::{self, DbPool},
    direct_walrus_store::{CertifyOutcome, DirectWalrusBlobStore},
    pearl_client::PearlConnection,
};

/// Upper bound on one certificate collection. The storage-node client
/// retries each node with backoff; a blob whose slivers are gone fails
/// every node, and this keeps a large unrecoverable batch from taking
/// hours.
const CERTIFICATE_TIMEOUT: Duration = Duration::from_secs(120);

/// Pause between candidates so a large batch does not hammer the RPC.
const BETWEEN_CANDIDATES: Duration = Duration::from_millis(100);

/// Errors that abort the whole run (per-candidate failures are
/// reported as [`Outcome::Failed`] instead).
#[derive(Debug, thiserror::Error)]
pub enum RepairError {
    /// Database error.
    #[error("database: {0}")]
    Db(#[from] sqlx::Error),
    /// Could not build the Walrus/Sui clients.
    #[error("init: {0}")]
    Init(String),
}

/// Connections the repair needs.
pub struct RepairContext {
    /// Oyster database.
    pub db: DbPool,
    /// The direct Walrus store: chain reads, certificate collection,
    /// certify and delete transactions signed via Pearl.
    pub store: DirectWalrusBlobStore,
}

impl RepairContext {
    /// Connect to Sui and the Walrus storage nodes.
    pub async fn new(
        db: DbPool,
        pearl: PearlConnection,
        rpc_url: &str,
        system_object: ObjectID,
        staking_object: ObjectID,
    ) -> Result<Self, RepairError> {
        // The pool-creation parameters are only used when an upload
        // lazy-creates a pool, which this tool never does.
        let store = DirectWalrusBlobStore::new(
            rpc_url.to_string(),
            system_object,
            staking_object,
            pearl,
            db.clone(),
            0,
            0,
        )
        .await
        .map_err(|e| RepairError::Init(e.to_string()))?;
        Ok(Self { db, store })
    }
}

/// What to repair and how far to go.
#[derive(Debug, Clone, Default)]
pub struct RepairOptions {
    /// Only this account's rows (does not filter `ids`).
    pub account: Option<AccountId>,
    /// `PooledBlob` object IDs to repair in addition to (or, with
    /// `skip_db`, instead of) what the database finds.
    pub ids: Vec<ObjectID>,
    /// Do not scan the database for candidates; only `ids`.
    pub skip_db: bool,
    /// Read and report only; submit nothing and write nothing.
    pub dry_run: bool,
    /// When no certificate can be collected, delete the `PooledBlob` and
    /// the rows instead of just reporting.
    pub delete_unrecoverable: bool,
}

/// One `(account, blob)` to repair.
#[derive(Debug, Clone)]
pub struct Candidate {
    /// Owning account.
    pub account_id: AccountId,
    /// Walrus blob ID (URL-safe base64), as stored in `blobs.blob_id`.
    pub blob_id: String,
    /// The on-chain `PooledBlob`.
    pub pooled_blob_object_id: ObjectID,
    /// `(bucket, key)` rows referencing it; empty for a chain-only orphan.
    pub references: Vec<(String, String)>,
    /// Walrus-encoded size, from the rows or computed from the
    /// `PooledBlob`, for pool accounting on delete.
    pub encoded_size: Option<u64>,
}

/// What happened to one candidate.
#[derive(Debug, Clone)]
pub enum Outcome {
    /// The `PooledBlob` was already certified; the rows were stamped.
    Stamped {
        /// Its on-chain certified epoch.
        epoch: u32,
        /// Rows stamped by this run (0 if a previous run did it).
        rows: u64,
    },
    /// The storage nodes still held the slivers: certified and stamped.
    Certified {
        /// The on-chain certified epoch.
        epoch: u32,
    },
    /// Dry run: certified on chain, rows would be stamped.
    WouldStamp {
        /// Its on-chain certified epoch.
        epoch: u32,
    },
    /// Dry run: a certificate was collected; the run would certify.
    WouldCertify,
    /// Dry run with `delete_unrecoverable`: no certificate; the run
    /// would delete the `PooledBlob` and the rows.
    WouldDelete {
        /// Why no certificate could be collected.
        reason: String,
    },
    /// No certificate could be collected and deletion was not enabled.
    /// The blob is unreadable; the customer must re-upload.
    Unrecoverable {
        /// Why no certificate could be collected.
        reason: String,
    },
    /// No certificate; the `PooledBlob` and the rows were deleted.
    Deleted {
        /// Rows removed; their keys are in the candidate's references.
        rows: u64,
        /// Encoded bytes returned to the pool.
        freed: u64,
    },
    /// The `PooledBlob` no longer exists on chain. If rows reference
    /// it they are dangling; they are left alone and reported.
    MissingOnChain,
    /// `--ids-file` entry whose `StoragePool` matches no account.
    UnknownPool {
        /// The pool the `PooledBlob` belongs to.
        pool: ObjectID,
    },
    /// Something else went wrong; re-run.
    Failed(String),
}

impl Outcome {
    /// Whether the operator needs to act on this candidate.
    pub fn is_problem(&self) -> bool {
        matches!(
            self,
            Outcome::Unrecoverable { .. }
                | Outcome::MissingOnChain
                | Outcome::UnknownPool { .. }
                | Outcome::Failed(_)
        )
    }

    /// Whether this run made the blob readable or reclaimed its slot.
    pub fn is_repaired(&self) -> bool {
        matches!(self, Outcome::Certified { .. } | Outcome::Deleted { .. })
    }
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Outcome::Stamped { epoch, rows } => {
                write!(f, "stamped epoch {epoch} on {rows} row(s)")
            }
            Outcome::Certified { epoch } => write!(f, "certified epoch {epoch}"),
            Outcome::WouldStamp { epoch } => write!(f, "would-stamp epoch {epoch}"),
            Outcome::WouldCertify => write!(f, "would-certify"),
            Outcome::WouldDelete { reason } => write!(f, "would-delete: {reason}"),
            Outcome::Unrecoverable { reason } => write!(f, "UNRECOVERABLE: {reason}"),
            Outcome::Deleted { rows, freed } => {
                write!(f, "deleted ({rows} row(s), {freed} encoded bytes freed)")
            }
            Outcome::MissingOnChain => write!(f, "MISSING-ON-CHAIN"),
            Outcome::UnknownPool { pool } => write!(f, "UNKNOWN-POOL {pool}"),
            Outcome::Failed(e) => write!(f, "FAILED: {e}"),
        }
    }
}

/// Per-candidate outcomes in processing order.
pub struct Report {
    /// Candidates and what happened to each.
    pub outcomes: Vec<(Candidate, Outcome)>,
}

impl Report {
    /// Number of candidates the operator must look at.
    pub fn problems(&self) -> usize {
        self.outcomes.iter().filter(|(_, o)| o.is_problem()).count()
    }

    /// Number of candidates this run certified or deleted.
    pub fn repaired(&self) -> usize {
        self.outcomes
            .iter()
            .filter(|(_, o)| o.is_repaired())
            .count()
    }
}

/// Collect candidates from the database and `opts.ids`, then repair each.
pub async fn repair(ctx: &RepairContext, opts: &RepairOptions) -> Result<Report, RepairError> {
    let mut outcomes = Vec::new();
    let mut seen: BTreeMap<(String, String), ()> = BTreeMap::new();
    let mut candidates: Vec<Candidate> = Vec::new();

    if !opts.skip_db {
        let rows = db::blobs::list_uncertified_references(&ctx.db, opts.account.as_ref()).await?;
        for row in rows {
            let key = (row.account_id.to_string(), row.blob_id.clone());
            if let Some(cand) = candidates
                .iter_mut()
                .find(|c| c.account_id == row.account_id && c.blob_id == row.blob_id)
            {
                cand.references.push((row.bucket_name, row.key));
                if cand.encoded_size.is_none() {
                    cand.encoded_size = row.encoded_size.map(|e| e as u64);
                }
                continue;
            }
            seen.insert(key, ());
            let pooled_blob_object_id: ObjectID = match row.pooled_blob_object_id.parse() {
                Ok(id) => id,
                Err(e) => {
                    outcomes.push((
                        Candidate {
                            account_id: row.account_id,
                            blob_id: row.blob_id,
                            pooled_blob_object_id: ObjectID::ZERO,
                            references: vec![(row.bucket_name, row.key)],
                            encoded_size: row.encoded_size.map(|e| e as u64),
                        },
                        Outcome::Failed(format!(
                            "invalid pooled_blob_object_id {}: {e}",
                            row.pooled_blob_object_id
                        )),
                    ));
                    continue;
                }
            };
            candidates.push(Candidate {
                account_id: row.account_id,
                blob_id: row.blob_id,
                pooled_blob_object_id,
                references: vec![(row.bucket_name, row.key)],
                encoded_size: row.encoded_size.map(|e| e as u64),
            });
        }
    }

    for id in &opts.ids {
        match candidate_from_object_id(ctx, *id).await {
            Ok(Some(cand)) => {
                let key = (cand.account_id.to_string(), cand.blob_id.clone());
                if seen.contains_key(&key) {
                    continue;
                }
                seen.insert(key, ());
                candidates.push(cand);
            }
            Ok(None) => outcomes.push((
                Candidate {
                    account_id: AccountId::NIL,
                    blob_id: String::new(),
                    pooled_blob_object_id: *id,
                    references: Vec::new(),
                    encoded_size: None,
                },
                Outcome::MissingOnChain,
            )),
            Err(outcome) => outcomes.push((
                Candidate {
                    account_id: AccountId::NIL,
                    blob_id: String::new(),
                    pooled_blob_object_id: *id,
                    references: Vec::new(),
                    encoded_size: None,
                },
                outcome,
            )),
        }
    }

    for cand in candidates {
        let outcome = repair_one(ctx, opts, &cand).await;
        tracing::info!(
            account_id = %cand.account_id,
            blob_id = %cand.blob_id,
            pooled_blob_object_id = %cand.pooled_blob_object_id,
            outcome = %outcome,
            "blob repair",
        );
        outcomes.push((cand, outcome));
        tokio::time::sleep(BETWEEN_CANDIDATES).await;
    }

    Ok(Report { outcomes })
}

/// Resolve an `--ids-file` entry to a candidate: read the `PooledBlob`,
/// map its pool to an account, and gather the rows (if any).
/// `Ok(None)` when the object is gone from chain.
async fn candidate_from_object_id(
    ctx: &RepairContext,
    id: ObjectID,
) -> Result<Option<Candidate>, Outcome> {
    let pooled_blob = match ctx.store.read_pooled_blob(id).await {
        Ok(b) => b,
        Err(e) if is_missing_object(&e) => return Ok(None),
        Err(e) => return Err(Outcome::Failed(e.to_string())),
    };
    let pool = pooled_blob.storage_pool_id;
    let account_id = db::accounts::find_account_by_storage_pool(&ctx.db, &pool.to_string())
        .await
        .map_err(|e| Outcome::Failed(e.to_string()))?
        .ok_or(Outcome::UnknownPool { pool })?;
    let blob_id = pooled_blob.blob_id.to_string();
    let rows = db::blobs::list_references(&ctx.db, &account_id, &blob_id)
        .await
        .map_err(|e| Outcome::Failed(e.to_string()))?;
    let encoded_size = rows
        .iter()
        .find_map(|r| r.encoded_size.map(|e| e as u64))
        .or_else(|| {
            ctx.store
                .encoded_size_for(pooled_blob.unencoded_size, pooled_blob.encoding_type)
        });
    Ok(Some(Candidate {
        account_id,
        blob_id,
        pooled_blob_object_id: id,
        references: rows.into_iter().map(|r| (r.bucket_name, r.key)).collect(),
        encoded_size,
    }))
}

/// Steps 1–4 for one candidate. Never returns an error: anything that
/// goes wrong is an [`Outcome::Failed`] so the batch keeps going.
async fn repair_one(ctx: &RepairContext, opts: &RepairOptions, cand: &Candidate) -> Outcome {
    // 1. Chain state.
    let pooled_blob = match ctx.store.read_pooled_blob(cand.pooled_blob_object_id).await {
        Ok(b) => b,
        Err(e) if is_missing_object(&e) => return Outcome::MissingOnChain,
        Err(e) => return Outcome::Failed(e.to_string()),
    };

    // 2. Already certified: only the rows need the stamp.
    if let Some(epoch) = pooled_blob.certified_epoch {
        if opts.dry_run {
            return Outcome::WouldStamp { epoch };
        }
        return match db::blobs::set_certified_epoch(
            &ctx.db,
            &cand.account_id,
            &cand.blob_id,
            i64::from(epoch),
        )
        .await
        {
            Ok(rows) => Outcome::Stamped { epoch, rows },
            Err(e) => Outcome::Failed(format!("stamp rows: {e}")),
        };
    }

    // 3. Ask the storage nodes for a certificate; no bytes needed.
    let certificate = match tokio::time::timeout(
        CERTIFICATE_TIMEOUT,
        ctx.store.collect_certificate_standalone(&pooled_blob),
    )
    .await
    {
        Ok(Ok(cert)) => Some(cert),
        Ok(Err(e)) => {
            tracing::warn!(
                pooled_blob_object_id = %cand.pooled_blob_object_id,
                error = %e,
                "no confirmation certificate from storage nodes",
            );
            None
        }
        Err(_) => {
            tracing::warn!(
                pooled_blob_object_id = %cand.pooled_blob_object_id,
                "certificate collection timed out",
            );
            None
        }
    };

    let Some(certificate) = certificate else {
        let reason = "storage nodes hold no slivers for this blob; the bytes must be re-uploaded"
            .to_string();
        if !opts.delete_unrecoverable {
            return Outcome::Unrecoverable { reason };
        }
        if opts.dry_run {
            return Outcome::WouldDelete { reason };
        }
        // 4. Reclaim the slot. `delete` submits `delete_pooled_blob` and
        // decrements the DB pool counter when `encoded_size > 0`.
        let encoded_size = cand.encoded_size.unwrap_or(0);
        if let Err(e) = ctx
            .store
            .delete(
                &BlobId(cand.blob_id.clone()),
                Some(&pooled_blob.storage_pool_id.to_string()),
                encoded_size,
                &cand.account_id,
            )
            .await
        {
            return Outcome::Failed(format!("delete PooledBlob: {e}"));
        }
        return match db::blobs::delete_references_for_blob(&ctx.db, &cand.account_id, &cand.blob_id)
            .await
        {
            Ok(rows) => Outcome::Deleted {
                rows,
                freed: encoded_size,
            },
            Err(e) => Outcome::Failed(format!(
                "PooledBlob deleted on chain but rows not removed: {e}; re-run"
            )),
        };
    };

    if opts.dry_run {
        return Outcome::WouldCertify;
    }

    let signer = match ctx.store.signer_for_account(&cand.account_id).await {
        Ok(s) => s,
        Err(e) => return Outcome::Failed(format!("signer: {e}")),
    };
    match ctx
        .store
        .submit_certify(&signer, &pooled_blob, certificate)
        .await
    {
        Ok(CertifyOutcome::Certified | CertifyOutcome::AlreadyCertified) => {}
        Err(e) => return Outcome::Failed(format!("certify: {e}")),
    }
    let epoch = match ctx.store.read_pooled_blob(cand.pooled_blob_object_id).await {
        Ok(b) => match b.certified_epoch {
            Some(e) => e,
            None => {
                return Outcome::Failed("still uncertified after certify tx; re-run".into());
            }
        },
        Err(e) => return Outcome::Failed(format!("re-read after certify: {e}")),
    };
    match db::blobs::set_certified_epoch(&ctx.db, &cand.account_id, &cand.blob_id, i64::from(epoch))
        .await
    {
        Ok(_) => Outcome::Certified { epoch },
        Err(e) => Outcome::Failed(format!(
            "certified on chain but rows not stamped: {e}; re-run"
        )),
    }
}

/// Whether a `read_pooled_blob` failure means the object no longer
/// exists (deleted `PooledBlob`), as opposed to a transport problem.
fn is_missing_object(e: &BlobStoreError) -> bool {
    let lower = e.to_string().to_lowercase();
    lower.contains("missing bcs data")
        || lower.contains("not found")
        || lower.contains("notexists")
        || lower.contains("does not exist")
        || lower.contains("deleted")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_object_classification() {
        assert!(is_missing_object(&BlobStoreError::Upstream(
            "read PooledBlob 0x1: missing bcs data on object".into()
        )));
        assert!(is_missing_object(&BlobStoreError::Upstream(
            "object 0x1 was deleted at version 3".into()
        )));
        assert!(!is_missing_object(&BlobStoreError::Upstream(
            "connection refused".into()
        )));
    }

    #[test]
    fn problems_and_repaired_counts() {
        let cand = Candidate {
            account_id: AccountId::NIL,
            blob_id: "b".into(),
            pooled_blob_object_id: ObjectID::ZERO,
            references: vec![],
            encoded_size: None,
        };
        let report = Report {
            outcomes: vec![
                (cand.clone(), Outcome::Certified { epoch: 3 }),
                (cand.clone(), Outcome::Stamped { epoch: 3, rows: 1 }),
                (cand.clone(), Outcome::Deleted { rows: 2, freed: 10 }),
                (cand.clone(), Outcome::Unrecoverable { reason: "x".into() }),
                (cand.clone(), Outcome::MissingOnChain),
                (cand, Outcome::WouldCertify),
            ],
        };
        assert_eq!(report.repaired(), 2);
        assert_eq!(report.problems(), 2);
    }
}
