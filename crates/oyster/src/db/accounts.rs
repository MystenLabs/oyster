use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::models::Account;

pub async fn create_account(pool: &SqlitePool) -> Result<Account, sqlx::Error> {
    let id = Uuid::new_v4().to_string();
    let row =
        sqlx::query("INSERT INTO accounts (id) VALUES (?) RETURNING id, created_at, updated_at")
            .bind(&id)
            .fetch_one(pool)
            .await?;

    Ok(Account {
        id: row.get("id"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    })
}

#[allow(dead_code)]
pub async fn get_account(pool: &SqlitePool, id: &str) -> Result<Option<Account>, sqlx::Error> {
    let row = sqlx::query("SELECT id, created_at, updated_at FROM accounts WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?;

    Ok(row.map(|r| Account {
        id: r.get("id"),
        created_at: r.get("created_at"),
        updated_at: r.get("updated_at"),
    }))
}
