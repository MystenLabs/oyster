pub mod accounts;
pub mod pending_transactions;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

pub type DbPool = sqlx::SqlitePool;

pub async fn create_pool(database_url: &str) -> Result<DbPool, sqlx::Error> {
    let options: SqliteConnectOptions = database_url
        .parse::<SqliteConnectOptions>()?
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .foreign_keys(true);

    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await?;

    sqlx::migrate!("./migrations").run(&pool).await?;

    Ok(pool)
}
