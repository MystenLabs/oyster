use std::{fmt, str::FromStr};

use sqlx::Row;

use super::users::IdentityProvider;

/// Format used to encode `decided_at` when binding to TEXT columns;
/// matches the table's timestamp defaults.
const TIMESTAMP_FORMAT: &str = "%Y-%m-%d %H:%M:%S";

/// Review status of a signup request. Stored as lowercase TEXT in
/// `signup_requests.status`; kept a Rust enum for the same reason as
/// [`IdentityProvider`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignupRequestStatus {
    /// Awaiting an operator decision.
    Pending,
    /// Approved — the next sign-in completes signup.
    Approved,
    /// Rejected — sign-ins are shown a denial page.
    Rejected,
}

impl SignupRequestStatus {
    /// Canonical lowercase string form stored in the database.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
        }
    }
}

impl fmt::Display for SignupRequestStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error returned when parsing an unknown signup-request status string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownStatus(pub String);

impl fmt::Display for UnknownStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown signup request status: {}", self.0)
    }
}

impl std::error::Error for UnknownStatus {}

impl FromStr for SignupRequestStatus {
    type Err = UnknownStatus;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "approved" => Ok(Self::Approved),
            "rejected" => Ok(Self::Rejected),
            other => Err(UnknownStatus(other.to_string())),
        }
    }
}

/// A signup request awaiting or carrying an operator decision.
/// Internal to the signup gate and review CLI — not an API response
/// model (hence it lives here rather than `models`, like
/// `accounts::ExpiringPool`).
#[derive(Debug, Clone)]
pub struct SignupRequest {
    /// Unique identifier (UUID).
    pub id: String,
    /// Identity provider the requester authenticated with.
    pub provider: String,
    /// See `user_identities.provider_subject` — the requester's stable
    /// ID within the provider.
    pub provider_subject: String,
    /// Provider-attested email at request time.
    pub email: String,
    /// Display name from the provider, if any.
    pub display_name: Option<String>,
    /// Current review status.
    pub status: SignupRequestStatus,
    /// ISO 8601 request timestamp.
    pub requested_at: String,
    /// ISO 8601 decision timestamp, once decided.
    pub decided_at: Option<String>,
    /// Operator note recorded with the decision (e.g. CLI user), if any.
    pub decided_by: Option<String>,
}

fn row_to_request(r: &sqlx::any::AnyRow) -> Result<SignupRequest, sqlx::Error> {
    let status: String = r.get("status");
    let status = status
        .parse::<SignupRequestStatus>()
        .map_err(|e| sqlx::Error::Decode(Box::new(e)))?;
    Ok(SignupRequest {
        id: r.get("id"),
        provider: r.get("provider"),
        provider_subject: r.get("provider_subject"),
        email: r.get("email"),
        display_name: r.get("display_name"),
        status,
        requested_at: r.get("requested_at"),
        decided_at: r.get("decided_at"),
        decided_by: r.get("decided_by"),
    })
}

const SELECT_COLUMNS: &str = "id, provider, provider_subject, email, display_name, status, \
                              requested_at, decided_at, decided_by";

/// Record a pending signup request for an identity, or return the
/// existing request unchanged if one is already on file (any status).
/// The upsert makes concurrent callbacks for the same identity safe.
pub async fn create_or_get(
    pool: &super::DbPool,
    provider: IdentityProvider,
    provider_subject: &str,
    email: &str,
    display_name: Option<&str>,
) -> Result<SignupRequest, sqlx::Error> {
    let id = uuid::Uuid::new_v4().to_string();
    sqlx::query(&super::sql(
        "INSERT INTO signup_requests (id, provider, provider_subject, email, display_name) \
         VALUES (?, ?, ?, ?, ?) \
         ON CONFLICT (provider, provider_subject) DO NOTHING",
    ))
    .bind(&id)
    .bind(provider.as_str())
    .bind(provider_subject)
    .bind(email)
    .bind(display_name)
    .execute(pool)
    .await?;

    let row = sqlx::query(&super::sql(&format!(
        "SELECT {SELECT_COLUMNS} FROM signup_requests \
         WHERE provider = ? AND provider_subject = ?"
    )))
    .bind(provider.as_str())
    .bind(provider_subject)
    .fetch_one(pool)
    .await?;
    row_to_request(&row)
}

/// Look up the signup request for an identity, if any.
pub async fn find_by_identity(
    pool: &super::DbPool,
    provider: IdentityProvider,
    provider_subject: &str,
) -> Result<Option<SignupRequest>, sqlx::Error> {
    let row = sqlx::query(&super::sql(&format!(
        "SELECT {SELECT_COLUMNS} FROM signup_requests \
         WHERE provider = ? AND provider_subject = ?"
    )))
    .bind(provider.as_str())
    .bind(provider_subject)
    .fetch_optional(pool)
    .await?;
    row.as_ref().map(row_to_request).transpose()
}

/// Set the status of a request by its ID, recording the decision time
/// and `decided_by` note. Returns `false` when no such request exists.
/// Deliberately allows overriding a prior decision (operator change of
/// mind), not just pending → decided transitions.
pub async fn set_status_by_id(
    pool: &super::DbPool,
    id: &str,
    status: SignupRequestStatus,
    decided_by: &str,
) -> Result<bool, sqlx::Error> {
    let now = chrono::Utc::now().format(TIMESTAMP_FORMAT).to_string();
    let result = sqlx::query(&super::sql(
        "UPDATE signup_requests SET status = ?, decided_at = ?, decided_by = ? WHERE id = ?",
    ))
    .bind(status.as_str())
    .bind(&now)
    .bind(decided_by)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Set the status of all requests matching an email (an email may
/// appear under multiple providers). Returns the number of requests
/// updated.
pub async fn set_status_by_email(
    pool: &super::DbPool,
    email: &str,
    status: SignupRequestStatus,
    decided_by: &str,
) -> Result<u64, sqlx::Error> {
    let now = chrono::Utc::now().format(TIMESTAMP_FORMAT).to_string();
    let result = sqlx::query(&super::sql(
        "UPDATE signup_requests SET status = ?, decided_at = ?, decided_by = ? WHERE email = ?",
    ))
    .bind(status.as_str())
    .bind(&now)
    .bind(decided_by)
    .bind(email)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// List requests with the given status, oldest first; `None` lists all.
pub async fn list(
    pool: &super::DbPool,
    status: Option<SignupRequestStatus>,
) -> Result<Vec<SignupRequest>, sqlx::Error> {
    let rows = match status {
        Some(s) => {
            sqlx::query(&super::sql(&format!(
                "SELECT {SELECT_COLUMNS} FROM signup_requests \
                 WHERE status = ? ORDER BY requested_at, id"
            )))
            .bind(s.as_str())
            .fetch_all(pool)
            .await?
        }
        None => {
            sqlx::query(&super::sql(&format!(
                "SELECT {SELECT_COLUMNS} FROM signup_requests ORDER BY requested_at, id"
            )))
            .fetch_all(pool)
            .await?
        }
    };
    rows.iter().map(row_to_request).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    async fn test_pool() -> db::DbPool {
        db::create_pool("sqlite::memory:").await.unwrap()
    }

    #[tokio::test]
    async fn create_or_get_is_idempotent() {
        let pool = test_pool().await;
        let first = create_or_get(
            &pool,
            IdentityProvider::Google,
            "req-1",
            "alice@example.com",
            Some("Alice"),
        )
        .await
        .unwrap();
        assert_eq!(first.status, SignupRequestStatus::Pending);

        // Second call returns the existing row — same id, and the
        // original email is preserved even if the caller passes new
        // metadata.
        let second = create_or_get(
            &pool,
            IdentityProvider::Google,
            "req-1",
            "other@example.com",
            None,
        )
        .await
        .unwrap();
        assert_eq!(second.id, first.id);
        assert_eq!(second.email, "alice@example.com");
    }

    #[tokio::test]
    async fn create_or_get_preserves_decision() {
        let pool = test_pool().await;
        let req = create_or_get(
            &pool,
            IdentityProvider::Google,
            "req-2",
            "bob@example.com",
            None,
        )
        .await
        .unwrap();
        set_status_by_id(&pool, &req.id, SignupRequestStatus::Approved, "test-op")
            .await
            .unwrap();

        // A repeat sign-in before the user row exists must not reset
        // the approval back to pending.
        let again = create_or_get(
            &pool,
            IdentityProvider::Google,
            "req-2",
            "bob@example.com",
            None,
        )
        .await
        .unwrap();
        assert_eq!(again.status, SignupRequestStatus::Approved);
    }

    #[tokio::test]
    async fn find_returns_none_for_unknown_identity() {
        let pool = test_pool().await;
        let found = find_by_identity(&pool, IdentityProvider::Google, "nope")
            .await
            .unwrap();
        assert!(found.is_none());
    }

    #[tokio::test]
    async fn set_status_by_id_records_decision() {
        let pool = test_pool().await;
        let req = create_or_get(
            &pool,
            IdentityProvider::Google,
            "req-3",
            "carol@example.com",
            None,
        )
        .await
        .unwrap();
        assert!(req.decided_at.is_none());

        let updated = set_status_by_id(&pool, &req.id, SignupRequestStatus::Rejected, "cli:zhe")
            .await
            .unwrap();
        assert!(updated);

        let found = find_by_identity(&pool, IdentityProvider::Google, "req-3")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.status, SignupRequestStatus::Rejected);
        assert!(found.decided_at.is_some());
        assert_eq!(found.decided_by.as_deref(), Some("cli:zhe"));

        assert!(
            !set_status_by_id(&pool, "no-such-id", SignupRequestStatus::Approved, "x")
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn set_status_by_email_updates_matching() {
        let pool = test_pool().await;
        create_or_get(
            &pool,
            IdentityProvider::Google,
            "req-4",
            "dave@example.com",
            None,
        )
        .await
        .unwrap();

        let n = set_status_by_email(
            &pool,
            "dave@example.com",
            SignupRequestStatus::Approved,
            "cli",
        )
        .await
        .unwrap();
        assert_eq!(n, 1);

        let n = set_status_by_email(
            &pool,
            "unknown@example.com",
            SignupRequestStatus::Approved,
            "cli",
        )
        .await
        .unwrap();
        assert_eq!(n, 0);
    }

    #[tokio::test]
    async fn list_filters_by_status() {
        let pool = test_pool().await;
        let a = create_or_get(&pool, IdentityProvider::Google, "req-5", "e1@x.com", None)
            .await
            .unwrap();
        create_or_get(&pool, IdentityProvider::Google, "req-6", "e2@x.com", None)
            .await
            .unwrap();
        set_status_by_id(&pool, &a.id, SignupRequestStatus::Approved, "cli")
            .await
            .unwrap();

        let pending = list(&pool, Some(SignupRequestStatus::Pending))
            .await
            .unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].provider_subject, "req-6");

        let all = list(&pool, None).await.unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn status_string_round_trip() {
        for s in [
            SignupRequestStatus::Pending,
            SignupRequestStatus::Approved,
            SignupRequestStatus::Rejected,
        ] {
            assert_eq!(s.as_str().parse::<SignupRequestStatus>().unwrap(), s);
        }
        assert!("consumed".parse::<SignupRequestStatus>().is_err());
    }
}
