use super::DbPool;
use crate::{
    error::Error,
    models::{Account, CreateAccountRequest, WalletInfo},
};

/// Generate a stub keypair. Returns (address_hex, private_key_bytes).
///
/// This is a placeholder — real Sui keypair generation will be added in Phase 5
/// when we integrate the Sui SDK.
fn generate_stub_keypair() -> (String, Vec<u8>) {
    let private_key: [u8; 32] = rand::random();
    // Derive a fake "address" by hashing the private key bytes.
    let address = hex::encode(&private_key[..20]);
    (format!("0x{address}"), private_key.to_vec())
}

pub async fn create_account(
    pool: &DbPool,
    req: &CreateAccountRequest,
    credentials: &str,
) -> Result<Account, Error> {
    let id = uuid::Uuid::new_v4().to_string();
    let (address, private_key) = generate_stub_keypair();

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
        "SELECT id, due_date, min_sui_balance, min_wal_balance, top_up_target_sui, top_up_target_wal, address, credentials, created_at, updated_at
         FROM accounts WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?
    .ok_or(Error::AccountNotFound)
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
