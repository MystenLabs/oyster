use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Account {
    pub id: String,
    pub address: String,
    pub credentials: String,
    pub created_at: String,
    pub updated_at: String,
}
