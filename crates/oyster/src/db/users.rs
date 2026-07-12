use std::{fmt, str::FromStr};

use sqlx::Row;

use crate::{UserId, models::User};

/// Identity provider through which a web-signup user authenticates.
///
/// Stored as lowercase TEXT in `user_identities.provider`; kept a Rust
/// enum (rather than a DB-level CHECK) so adding a provider is a code
/// change, not a schema migration. See `FromStr`/`as_str` for the
/// canonical string forms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityProvider {
    /// Google OAuth (`provider_subject` is the id_token `sub` claim).
    Google,
}

impl IdentityProvider {
    /// Canonical lowercase string form stored in the database.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Google => "google",
        }
    }
}

impl fmt::Display for IdentityProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error returned when parsing an unknown identity-provider string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownProvider(pub String);

impl fmt::Display for UnknownProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown identity provider: {}", self.0)
    }
}

impl std::error::Error for UnknownProvider {}

impl FromStr for IdentityProvider {
    type Err = UnknownProvider;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "google" => Ok(Self::Google),
            other => Err(UnknownProvider(other.to_string())),
        }
    }
}

/// Insert a new user together with their first identity, in one
/// transaction. `verified_at` is set immediately: OAuth providers
/// attest the identity as part of the login flow.
///
/// `provider_subject` is the user's stable unique identifier *within*
/// `provider` — `(provider, provider_subject)` is the login key. For
/// [`IdentityProvider::Google`] it is the id_token `sub` claim, an
/// opaque numeric string that never changes for a given Google account
/// (unlike email, which can be changed or re-issued). `email` is
/// contact/display metadata only and plays no part in authentication.
pub async fn create_user_with_identity(
    pool: &super::DbPool,
    provider: IdentityProvider,
    provider_subject: &str,
    email: &str,
    display_name: Option<&str>,
) -> Result<User, sqlx::Error> {
    let user_id = UserId::new();
    let identity_id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

    let mut tx = pool.begin().await?;
    let row = sqlx::query(&super::sql(
        "INSERT INTO users (id, email, display_name) VALUES (?, ?, ?) \
         RETURNING id, email, display_name, created_at",
    ))
    .bind(&user_id)
    .bind(email)
    .bind(display_name)
    .fetch_one(&mut *tx)
    .await?;

    sqlx::query(&super::sql(
        "INSERT INTO user_identities (id, user_id, provider, provider_subject, verified_at) \
         VALUES (?, ?, ?, ?, ?)",
    ))
    .bind(&identity_id)
    .bind(&user_id)
    .bind(provider.as_str())
    .bind(provider_subject)
    .bind(&now)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(User {
        id: row.get("id"),
        email: row.get("email"),
        display_name: row.get("display_name"),
        created_at: row.get("created_at"),
    })
}

/// Look up a user by one of their identities, returning `None` when no
/// identity matches. See [`create_user_with_identity`] for
/// `provider_subject` semantics.
pub async fn find_user_by_identity(
    pool: &super::DbPool,
    provider: IdentityProvider,
    provider_subject: &str,
) -> Result<Option<User>, sqlx::Error> {
    let row = sqlx::query(&super::sql(
        "SELECT u.id, u.email, u.display_name, u.created_at \
         FROM users u \
         JOIN user_identities i ON i.user_id = u.id \
         WHERE i.provider = ? AND i.provider_subject = ?",
    ))
    .bind(provider.as_str())
    .bind(provider_subject)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|r| User {
        id: r.get("id"),
        email: r.get("email"),
        display_name: r.get("display_name"),
        created_at: r.get("created_at"),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    async fn test_pool() -> db::DbPool {
        db::create_pool("sqlite::memory:").await.unwrap()
    }

    #[tokio::test]
    async fn create_and_find_round_trip() {
        let pool = test_pool().await;
        let created = create_user_with_identity(
            &pool,
            IdentityProvider::Google,
            "sub-123",
            "alice@example.com",
            Some("Alice"),
        )
        .await
        .unwrap();
        assert_eq!(created.email, "alice@example.com");
        assert_eq!(created.display_name.as_deref(), Some("Alice"));

        let found = find_user_by_identity(&pool, IdentityProvider::Google, "sub-123")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.id, created.id);
        assert_eq!(found.email, created.email);
    }

    #[tokio::test]
    async fn unknown_identity_returns_none() {
        let pool = test_pool().await;
        let found = find_user_by_identity(&pool, IdentityProvider::Google, "no-such-sub")
            .await
            .unwrap();
        assert!(found.is_none());
    }

    #[tokio::test]
    async fn display_name_is_optional() {
        let pool = test_pool().await;
        let created = create_user_with_identity(
            &pool,
            IdentityProvider::Google,
            "sub-456",
            "bob@example.com",
            None,
        )
        .await
        .unwrap();
        assert!(created.display_name.is_none());

        let found = find_user_by_identity(&pool, IdentityProvider::Google, "sub-456")
            .await
            .unwrap()
            .unwrap();
        assert!(found.display_name.is_none());
    }

    #[tokio::test]
    async fn duplicate_identity_is_rejected() {
        let pool = test_pool().await;
        create_user_with_identity(
            &pool,
            IdentityProvider::Google,
            "sub-dup",
            "carol@example.com",
            None,
        )
        .await
        .unwrap();

        let dup = create_user_with_identity(
            &pool,
            IdentityProvider::Google,
            "sub-dup",
            "mallory@example.com",
            None,
        )
        .await;
        assert!(dup.is_err());

        // The failed transaction must not leave a partial user row behind.
        let found = find_user_by_identity(&pool, IdentityProvider::Google, "sub-dup")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.email, "carol@example.com");
    }

    #[test]
    fn provider_string_round_trip() {
        assert_eq!(IdentityProvider::Google.as_str(), "google");
        assert_eq!(
            "google".parse::<IdentityProvider>().unwrap(),
            IdentityProvider::Google
        );
        assert!("github".parse::<IdentityProvider>().is_err());
    }
}
