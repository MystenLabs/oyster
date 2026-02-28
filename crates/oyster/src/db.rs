/// Account database operations.
pub mod accounts;
/// API key database operations.
pub mod api_keys;
/// Blob metadata database operations.
pub mod blobs;
/// Bucket database operations.
pub mod buckets;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

/// SQLite connection pool type alias.
pub type DbPool = sqlx::SqlitePool;

/// Create and migrate a SQLite connection pool.
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
