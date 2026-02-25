use fastcrypto::traits::ToFromBytes;
use sui_types::{
    base_types::SuiAddress,
    crypto::{AccountKeyPair, get_account_key_pair},
};

use super::DbPool;
use crate::{
    error::Error,
    models::{Account, CachedBalance, CreateAccountRequest, WalletInfo},
};

fn generate_sui_keypair() -> (String, Vec<u8>) {
    let (address, ed25519_kp): (SuiAddress, AccountKeyPair) = get_account_key_pair();
    let private_key = ed25519_kp.as_bytes().to_vec();
    (address.to_string(), private_key)
}

pub async fn create_account(
    pool: &DbPool,
    req: &CreateAccountRequest,
    credentials: &str,
) -> Result<Account, Error> {
    let id = uuid::Uuid::new_v4().to_string();
    let (address, private_key) = generate_sui_keypair();

    sqlx::query(
        "INSERT INTO accounts (id, min_sui_balance, min_wal_balance, top_up_target_sui, top_up_target_wal, address, private_key, credentials)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(req.min_sui_balance)
    .bind(req.min_wal_balance)
    .bind(req.top_up_target_sui)
    .bind(req.top_up_target_wal)
    .bind(&address)
    .bind(&private_key)
    .bind(credentials)
    .execute(pool)
    .await?;

    get_account(pool, &id).await
}

pub async fn get_account(pool: &DbPool, id: &str) -> Result<Account, Error> {
    sqlx::query_as::<_, Account>(
        "SELECT id, due_date, min_sui_balance, min_wal_balance, top_up_target_sui, top_up_target_wal, address, credentials, cached_sui_balance, cached_wal_balance, balance_updated_at, created_at, updated_at
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

pub async fn get_account_wallets(pool: &DbPool, id: &str) -> Result<Vec<WalletInfo>, Error> {
    // Currently one wallet per account. When multi-wallet support is added,
    // this will return multiple rows.
    let account = get_account(pool, id).await?;
    Ok(vec![WalletInfo {
        account_id: account.id,
        address: account.address,
        min_sui_balance: account.min_sui_balance,
        min_wal_balance: account.min_wal_balance,
        top_up_target_sui: account.top_up_target_sui,
        top_up_target_wal: account.top_up_target_wal,
    }])
}

pub async fn get_balance(pool: &DbPool, account_id: &str) -> Result<CachedBalance, Error> {
    let account = get_account(pool, account_id).await?;
    Ok(CachedBalance {
        cached_sui_balance: account.cached_sui_balance,
        cached_wal_balance: account.cached_wal_balance,
        min_sui_balance: account.min_sui_balance,
        min_wal_balance: account.min_wal_balance,
        balance_updated_at: account.balance_updated_at,
    })
}

pub async fn set_cached_balance(
    pool: &DbPool,
    account_id: &str,
    sui_balance: i64,
    wal_balance: i64,
) -> Result<(), Error> {
    let rows = sqlx::query(
        "UPDATE accounts SET cached_sui_balance = ?, cached_wal_balance = ?, balance_updated_at = datetime('now') WHERE id = ?",
    )
    .bind(sui_balance)
    .bind(wal_balance)
    .bind(account_id)
    .execute(pool)
    .await?
    .rows_affected();

    if rows == 0 {
        return Err(Error::AccountNotFound);
    }
    Ok(())
}

pub async fn get_random_account_id(pool: &DbPool) -> Result<Option<String>, Error> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT id FROM accounts ORDER BY RANDOM() LIMIT 1")
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|(id,)| id))
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
        let req = CreateAccountRequest {
            min_sui_balance: 100,
            min_wal_balance: 200,
            top_up_target_sui: 500,
            top_up_target_wal: 1000,
        };

        let account = create_account(&pool, &req, "cred-abc").await.unwrap();

        assert!(!account.id.is_empty());
        assert!(account.address.starts_with("0x"));
        // Real Sui address: 0x + 64 hex chars = 66 chars total.
        assert_eq!(account.address.len(), 66);
        assert_eq!(account.min_sui_balance, 100);
        assert_eq!(account.min_wal_balance, 200);
        assert_eq!(account.top_up_target_sui, 500);
        assert_eq!(account.top_up_target_wal, 1000);
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
    async fn get_account_wallets_returns_matching_wallet() {
        let pool = test_pool().await;
        let req = CreateAccountRequest {
            min_sui_balance: 10,
            min_wal_balance: 20,
            top_up_target_sui: 30,
            top_up_target_wal: 40,
        };

        let account = create_account(&pool, &req, "cred").await.unwrap();
        let wallets = get_account_wallets(&pool, &account.id).await.unwrap();

        assert_eq!(wallets.len(), 1);
        let w = &wallets[0];
        assert_eq!(w.account_id, account.id);
        assert_eq!(w.address, account.address);
        assert_eq!(w.min_sui_balance, 10);
        assert_eq!(w.min_wal_balance, 20);
        assert_eq!(w.top_up_target_sui, 30);
        assert_eq!(w.top_up_target_wal, 40);
    }

    #[tokio::test]
    async fn get_private_key_returns_valid_bytes() {
        let pool = test_pool().await;
        let req = CreateAccountRequest {
            min_sui_balance: 0,
            min_wal_balance: 0,
            top_up_target_sui: 0,
            top_up_target_wal: 0,
        };

        let account = create_account(&pool, &req, "cred").await.unwrap();
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
        let req = CreateAccountRequest {
            min_sui_balance: 0,
            min_wal_balance: 0,
            top_up_target_sui: 0,
            top_up_target_wal: 0,
        };

        let mut addrs = std::collections::HashSet::new();
        for _ in 0..50 {
            let account = create_account(&pool, &req, "cred").await.unwrap();
            assert!(addrs.insert(account.address), "duplicate address generated");
        }
    }
}
