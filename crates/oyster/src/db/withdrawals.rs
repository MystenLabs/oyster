//! Admin fund-withdrawal persistence: the pre-registered destination
//! address per account (with its cooldown) and the request/approve
//! ledger. See migration 027 for the threat model these tables encode.

use chrono::{DateTime, Utc};
use sqlx::Row;
use uuid::Uuid;

use crate::{AccountId, AppId};

/// Timestamp format shared with the `accounts` table (`YYYY-MM-DD
/// HH:MM:SS`, UTC); lexicographic order equals chronological order.
const TIMESTAMP_FORMAT: &str = "%Y-%m-%d %H:%M:%S";

/// Encode a timestamp for a TEXT column.
pub fn ts(t: DateTime<Utc>) -> String {
    t.format(TIMESTAMP_FORMAT).to_string()
}

/// Parse a TEXT timestamp written by [`ts`]. `None` on malformed input.
pub fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    chrono::NaiveDateTime::parse_from_str(s, TIMESTAMP_FORMAT)
        .ok()
        .map(|naive| naive.and_utc())
}

/// The registered withdrawal destination for an account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WithdrawalAddress {
    /// Owning account.
    pub account_id: AccountId,
    /// Normalized Sui address (`0x` + 64 hex).
    pub address: String,
    /// Admin key that registered it (audit trail).
    pub registered_by_admin_key_id: String,
    /// When it was registered.
    pub registered_at: String,
    /// Earliest time a withdrawal to it may be approved.
    pub usable_at: String,
}

/// Lifecycle of a withdrawal request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WithdrawalStatus {
    /// Requested; waiting for approval by a second admin key.
    Pending,
    /// Approved; the on-chain transfer is being submitted.
    Executing,
    /// Transfer landed on-chain (`tx_digest` set).
    Completed,
    /// Transfer failed (`error` set). A new request is needed.
    Failed,
    /// Withdrawn by an admin before approval.
    Cancelled,
}

impl WithdrawalStatus {
    /// Column value.
    pub fn as_str(self) -> &'static str {
        match self {
            WithdrawalStatus::Pending => "pending",
            WithdrawalStatus::Executing => "executing",
            WithdrawalStatus::Completed => "completed",
            WithdrawalStatus::Failed => "failed",
            WithdrawalStatus::Cancelled => "cancelled",
        }
    }

    /// Parse a column value.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "pending" => WithdrawalStatus::Pending,
            "executing" => WithdrawalStatus::Executing,
            "completed" => WithdrawalStatus::Completed,
            "failed" => WithdrawalStatus::Failed,
            "cancelled" => WithdrawalStatus::Cancelled,
            _ => return None,
        })
    }
}

/// One withdrawal request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Withdrawal {
    /// Request id (UUID).
    pub id: String,
    /// Account whose wallet is debited.
    pub account_id: AccountId,
    /// App that owns the account (denormalized for listing/auth).
    pub app_id: AppId,
    /// Destination address, snapshotted at request time.
    pub destination: String,
    /// SUI to send, in MIST. `None` when not withdrawing SUI.
    pub sui_mist: Option<i64>,
    /// WAL to send, in FROST. `None` when not withdrawing WAL.
    pub wal_frost: Option<i64>,
    /// Move every SUI and WAL coin instead of fixed amounts.
    pub drain: bool,
    /// Current state.
    pub status: WithdrawalStatus,
    /// Admin key that created the request.
    pub requested_by_admin_key_id: String,
    /// Admin key that approved it (must differ from the requester).
    pub approved_by_admin_key_id: Option<String>,
    /// Sui transaction digest once completed.
    pub tx_digest: Option<String>,
    /// Failure reason once failed.
    pub error: Option<String>,
    /// Creation time.
    pub created_at: String,
    /// Time after which a pending request can no longer be approved.
    pub expires_at: String,
    /// Last state change.
    pub updated_at: String,
}

impl Withdrawal {
    /// Whether a pending request has passed its `expires_at`.
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        self.status == WithdrawalStatus::Pending
            && parse_ts(&self.expires_at).is_some_and(|exp| exp <= now)
    }
}

/// Register (or replace) the withdrawal address for an account. A
/// replacement restarts the cooldown: `usable_at` is always the value
/// passed here.
pub async fn set_withdrawal_address(
    pool: &super::DbPool,
    account_id: &AccountId,
    address: &str,
    admin_key_id: &str,
    now: DateTime<Utc>,
    usable_at: DateTime<Utc>,
) -> Result<WithdrawalAddress, sqlx::Error> {
    let row = sqlx::query(&super::sql(
        "INSERT INTO withdrawal_addresses \
             (account_id, address, registered_by_admin_key_id, registered_at, usable_at) \
         VALUES (?, ?, ?, ?, ?) \
         ON CONFLICT(account_id) DO UPDATE SET \
             address = excluded.address, \
             registered_by_admin_key_id = excluded.registered_by_admin_key_id, \
             registered_at = excluded.registered_at, \
             usable_at = excluded.usable_at \
         RETURNING account_id, address, registered_by_admin_key_id, registered_at, usable_at",
    ))
    .bind(account_id)
    .bind(address)
    .bind(admin_key_id)
    .bind(ts(now))
    .bind(ts(usable_at))
    .fetch_one(pool)
    .await?;
    Ok(row_to_address(row))
}

/// The registered withdrawal address, if any.
pub async fn get_withdrawal_address(
    pool: &super::DbPool,
    account_id: &AccountId,
) -> Result<Option<WithdrawalAddress>, sqlx::Error> {
    let row = sqlx::query(&super::sql(
        "SELECT account_id, address, registered_by_admin_key_id, registered_at, usable_at \
         FROM withdrawal_addresses WHERE account_id = ?",
    ))
    .bind(account_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(row_to_address))
}

/// Remove the registered address. Returns `true` when one existed.
pub async fn clear_withdrawal_address(
    pool: &super::DbPool,
    account_id: &AccountId,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(&super::sql(
        "DELETE FROM withdrawal_addresses WHERE account_id = ?",
    ))
    .bind(account_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

fn row_to_address(r: sqlx::any::AnyRow) -> WithdrawalAddress {
    WithdrawalAddress {
        account_id: r.get("account_id"),
        address: r.get("address"),
        registered_by_admin_key_id: r.get("registered_by_admin_key_id"),
        registered_at: r.get("registered_at"),
        usable_at: r.get("usable_at"),
    }
}

const WITHDRAWAL_COLUMNS: &str = "id, account_id, app_id, destination, sui_mist, wal_frost, drain, \
     status, requested_by_admin_key_id, approved_by_admin_key_id, tx_digest, error, \
     created_at, expires_at, updated_at";

fn row_to_withdrawal(r: sqlx::any::AnyRow) -> Withdrawal {
    let status: String = r.get("status");
    Withdrawal {
        id: r.get("id"),
        account_id: r.get("account_id"),
        app_id: r.get("app_id"),
        destination: r.get("destination"),
        sui_mist: r.get("sui_mist"),
        wal_frost: r.get("wal_frost"),
        drain: r.get::<i64, _>("drain") != 0,
        status: WithdrawalStatus::parse(&status)
            .unwrap_or_else(|| panic!("unknown withdrawal status {status:?} in database")),
        requested_by_admin_key_id: r.get("requested_by_admin_key_id"),
        approved_by_admin_key_id: r.get("approved_by_admin_key_id"),
        tx_digest: r.get("tx_digest"),
        error: r.get("error"),
        created_at: r.get("created_at"),
        expires_at: r.get("expires_at"),
        updated_at: r.get("updated_at"),
    }
}

/// Create a pending withdrawal request.
#[allow(clippy::too_many_arguments)]
pub async fn create_withdrawal(
    pool: &super::DbPool,
    account_id: &AccountId,
    app_id: &AppId,
    destination: &str,
    sui_mist: Option<i64>,
    wal_frost: Option<i64>,
    drain: bool,
    requested_by_admin_key_id: &str,
    now: DateTime<Utc>,
    expires_at: DateTime<Utc>,
) -> Result<Withdrawal, sqlx::Error> {
    let id = Uuid::new_v4().to_string();
    let row = sqlx::query(&super::sql(&format!(
        "INSERT INTO withdrawals \
             (id, account_id, app_id, destination, sui_mist, wal_frost, drain, status, \
              requested_by_admin_key_id, created_at, expires_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, 'pending', ?, ?, ?, ?) \
         RETURNING {WITHDRAWAL_COLUMNS}"
    )))
    .bind(&id)
    .bind(account_id)
    .bind(app_id)
    .bind(destination)
    .bind(sui_mist)
    .bind(wal_frost)
    .bind(i64::from(drain))
    .bind(requested_by_admin_key_id)
    .bind(ts(now))
    .bind(ts(expires_at))
    .bind(ts(now))
    .fetch_one(pool)
    .await?;
    Ok(row_to_withdrawal(row))
}

/// Fetch a withdrawal by id.
pub async fn get_withdrawal(
    pool: &super::DbPool,
    id: &str,
) -> Result<Option<Withdrawal>, sqlx::Error> {
    let row = sqlx::query(&super::sql(&format!(
        "SELECT {WITHDRAWAL_COLUMNS} FROM withdrawals WHERE id = ?"
    )))
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(row_to_withdrawal))
}

/// All withdrawals for an account, newest first.
pub async fn list_withdrawals(
    pool: &super::DbPool,
    account_id: &AccountId,
) -> Result<Vec<Withdrawal>, sqlx::Error> {
    let rows = sqlx::query(&super::sql(&format!(
        "SELECT {WITHDRAWAL_COLUMNS} FROM withdrawals WHERE account_id = ? \
         ORDER BY created_at DESC, id"
    )))
    .bind(account_id)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(row_to_withdrawal).collect())
}

/// The account's live pending request whose `expires_at` is still in
/// the future, if any. At most one such request is allowed per account.
pub async fn get_live_pending_withdrawal(
    pool: &super::DbPool,
    account_id: &AccountId,
    now: DateTime<Utc>,
) -> Result<Option<Withdrawal>, sqlx::Error> {
    let row = sqlx::query(&super::sql(&format!(
        "SELECT {WITHDRAWAL_COLUMNS} FROM withdrawals \
         WHERE account_id = ? AND status = 'pending' AND expires_at > ? \
         ORDER BY created_at DESC LIMIT 1"
    )))
    .bind(account_id)
    .bind(ts(now))
    .fetch_optional(pool)
    .await?;
    Ok(row.map(row_to_withdrawal))
}

#[allow(clippy::too_many_arguments)]
/// Compare-and-set state transition. Succeeds only if the row is
/// currently in `from`, so two concurrent approvals cannot both submit.
/// `approved_by`, `tx_digest` and `error` are written when given.
pub async fn transition_withdrawal(
    pool: &super::DbPool,
    id: &str,
    from: WithdrawalStatus,
    to: WithdrawalStatus,
    approved_by: Option<&str>,
    tx_digest: Option<&str>,
    error: Option<&str>,
    now: DateTime<Utc>,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(&super::sql(
        "UPDATE withdrawals SET status = ?, \
             approved_by_admin_key_id = COALESCE(?, approved_by_admin_key_id), \
             tx_digest = COALESCE(?, tx_digest), \
             error = COALESCE(?, error), \
             updated_at = ? \
         WHERE id = ? AND status = ?",
    ))
    .bind(to.as_str())
    .bind(approved_by)
    .bind(tx_digest)
    .bind(error)
    .bind(ts(now))
    .bind(id)
    .bind(from.as_str())
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}
