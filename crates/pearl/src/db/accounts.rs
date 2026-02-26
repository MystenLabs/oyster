use fastcrypto::traits::ToFromBytes;
use sui_types::{
    base_types::SuiAddress,
    crypto::{AccountKeyPair, get_account_key_pair},
};

use super::DbPool;
use crate::{error::Error, models::Account};

fn generate_sui_keypair() -> (String, Vec<u8>) {
    let (address, ed25519_kp): (SuiAddress, AccountKeyPair) = get_account_key_pair();
    let private_key = ed25519_kp.as_bytes().to_vec();
    (address.to_string(), private_key)
}

pub async fn create_account(pool: &DbPool, credentials: &str) -> Result<Account, Error> {
    let id = uuid::Uuid::new_v4().to_string();
    let (address, private_key) = generate_sui_keypair();

    sqlx::query(
        "INSERT INTO accounts (id, address, private_key, credentials)
         VALUES (?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&address)
    .bind(&private_key)
    .bind(credentials)
    .execute(pool)
    .await?;

    get_account(pool, &id).await
}

pub async fn get_account(pool: &DbPool, id: &str) -> Result<Account, Error> {
    sqlx::query_as::<_, Account>(
        "SELECT id, address, credentials, created_at, updated_at
         FROM accounts WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?
    .ok_or(Error::AccountNotFound)
}

pub async fn get_private_key(pool: &DbPool, account_id: &str) -> Result<Vec<u8>, Error> {
    let row: Option<(Vec<u8>,)> = sqlx::query_as("SELECT private_key FROM accounts WHERE id = ?")
        .bind(account_id)
        .fetch_optional(pool)
        .await?;
    row.map(|(pk,)| pk).ok_or(Error::AccountNotFound)
}

pub async fn get_address(pool: &DbPool, account_id: &str) -> Result<String, Error> {
    let row: Option<(String,)> = sqlx::query_as("SELECT address FROM accounts WHERE id = ?")
        .bind(account_id)
        .fetch_optional(pool)
        .await?;
    row.map(|(addr,)| addr).ok_or(Error::AccountNotFound)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    async fn test_pool() -> DbPool {
        db::create_pool("sqlite::memory:")
            .await
            .expect("in-memory pool")
    }

    #[tokio::test]
    async fn create_account_returns_well_formed_account() {
        let pool = test_pool().await;

        let account = create_account(&pool, "cred-abc").await.unwrap();

        assert!(!account.id.is_empty());
        assert!(account.address.starts_with("0x"));
        // Real Sui address: 0x + 64 hex chars = 66 chars total.
        assert_eq!(account.address.len(), 66);
        assert_eq!(account.credentials, "cred-abc");
        assert!(!account.created_at.is_empty());
        assert!(!account.updated_at.is_empty());
    }

    #[tokio::test]
    async fn get_account_not_found() {
        let pool = test_pool().await;

        let err = get_account(&pool, "nonexistent-id").await.unwrap_err();
        assert!(
            matches!(err, Error::AccountNotFound),
            "expected AccountNotFound, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn get_private_key_returns_valid_bytes() {
        let pool = test_pool().await;

        let account = create_account(&pool, "cred").await.unwrap();
        let pk = get_private_key(&pool, &account.id).await.unwrap();

        // Ed25519 private key is 32 bytes.
        assert_eq!(pk.len(), 32);

        // The key should reconstruct into a valid Ed25519KeyPair.
        use fastcrypto::ed25519::Ed25519KeyPair;
        Ed25519KeyPair::from_bytes(&pk).expect("valid Ed25519 private key");
    }

    #[tokio::test]
    async fn get_private_key_not_found() {
        let pool = test_pool().await;

        let err = get_private_key(&pool, "nonexistent-id").await.unwrap_err();
        assert!(
            matches!(err, Error::AccountNotFound),
            "expected AccountNotFound, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn unique_addresses_across_many_accounts() {
        let pool = test_pool().await;

        let mut addrs = std::collections::HashSet::new();
        for _ in 0..50 {
            let account = create_account(&pool, "cred").await.unwrap();
            assert!(addrs.insert(account.address), "duplicate address generated");
        }
    }

    #[tokio::test]
    async fn get_address_returns_correct_address() {
        let pool = test_pool().await;

        let account = create_account(&pool, "cred").await.unwrap();
        let address = get_address(&pool, &account.id).await.unwrap();
        assert_eq!(address, account.address);
    }

    #[tokio::test]
    async fn get_address_not_found() {
        let pool = test_pool().await;

        let err = get_address(&pool, "nonexistent-id").await.unwrap_err();
        assert!(
            matches!(err, Error::AccountNotFound),
            "expected AccountNotFound, got: {err:?}"
        );
    }
}
