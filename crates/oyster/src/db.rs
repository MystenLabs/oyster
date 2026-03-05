/// Account database operations.
pub mod accounts;
/// API key database operations.
pub mod api_keys;
/// Blob metadata database operations.
pub mod blobs;
/// Bucket database operations.
pub mod buckets;

use sqlx::any::AnyPoolOptions;

/// Database connection pool type alias.
pub type DbPool = sqlx::AnyPool;

/// Create and migrate a database connection pool.
///
/// The backend (SQLite or PostgreSQL) is detected from the connection URL.
/// SQLite URLs start with `sqlite:`, PostgreSQL with `postgres://` or `postgresql://`.
pub async fn create_pool(database_url: &str) -> Result<DbPool, sqlx::Error> {
    sqlx::any::install_default_drivers();

    let is_sqlite = database_url.starts_with("sqlite");

    // In-memory SQLite databases are per-connection, so we must limit to a
    // single connection so that migrations and queries share the same database.
    let is_memory = is_sqlite && database_url.contains(":memory:");
    let max_conn = if is_memory { 1 } else { 5 };
    let mut options = AnyPoolOptions::new().max_connections(max_conn);

    if is_sqlite {
        options = options.after_connect(|conn, _meta| {
            Box::pin(async move {
                sqlx::query("PRAGMA journal_mode=WAL")
                    .execute(&mut *conn)
                    .await?;
                sqlx::query("PRAGMA foreign_keys=ON")
                    .execute(&mut *conn)
                    .await?;
                Ok(())
            })
        });
    }

    let pool = options.connect(database_url).await?;

    let migrator = if is_sqlite {
        sqlx::migrate!("./migrations/sqlite")
    } else {
        sqlx::migrate!("./migrations/postgres")
    };
    migrator.run(&pool).await?;

    Ok(pool)
}
