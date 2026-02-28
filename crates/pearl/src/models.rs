use serde::{Deserialize, Serialize};

/// A Pearl account record.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Account {
    /// Unique identifier.
    pub id: String,
    /// Opaque credentials string.
    pub credentials: String,
    /// ISO 8601 creation timestamp.
    pub created_at: String,
    /// ISO 8601 last-update timestamp.
    pub updated_at: String,
}
