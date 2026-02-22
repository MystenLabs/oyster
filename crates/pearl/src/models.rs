use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Account {
    pub id: String,
    pub due_date: Option<String>,
    pub min_sui_balance: i64,
    pub min_wal_balance: i64,
    pub top_up_target_sui: i64,
    pub top_up_target_wal: i64,
    pub address: String,
    pub credentials: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalletInfo {
    pub account_id: String,
    pub address: String,
    pub min_sui_balance: i64,
    pub min_wal_balance: i64,
    pub top_up_target_sui: i64,
    pub top_up_target_wal: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateAccountRequest {
    pub min_sui_balance: i64,
    pub min_wal_balance: i64,
    pub top_up_target_sui: i64,
    pub top_up_target_wal: i64,
}
