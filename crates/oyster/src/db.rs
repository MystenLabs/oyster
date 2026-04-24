/// S3 access key database operations.
pub mod access_keys;
/// Account database operations.
pub mod accounts;
/// API key database operations.
pub mod api_keys;
/// App database operations.
pub mod apps;
/// Blob tag database operations.
pub mod blob_tags;
/// Blob metadata database operations.
pub mod blobs;
/// Bucket database operations.
pub mod buckets;
/// JWT blacklist database operations.
pub mod jwt_blacklist;

use std::{borrow::Cow, fmt::Write, sync::OnceLock};

use sqlx::any::AnyPoolOptions;

/// Database connection pool type alias.
pub type DbPool = sqlx::AnyPool;

/// Whether the connected database is PostgreSQL (requires `$1`-style placeholders).
static IS_POSTGRES: OnceLock<bool> = OnceLock::new();

/// Translate `?` placeholders to `$1, $2, …` when connected to PostgreSQL.
///
/// SQLite uses `?` while PostgreSQL uses `$N`. The `sqlx::Any` driver does not
/// always translate automatically, so we handle it ourselves.
///
/// See https://github.com/launchbadge/sqlx/issues/875 for more details on a potential future fix.
pub fn sql(query: &str) -> Cow<'_, str> {
    if !*IS_POSTGRES.get().unwrap_or(&false) {
        return Cow::Borrowed(query);
    }
    let mut result = String::with_capacity(query.len() + 16);
    let mut n = 0u32;
    for c in query.chars() {
        if c == '?' {
            n += 1;
            write!(result, "${n}").unwrap();
        } else {
            result.push(c);
        }
    }
    Cow::Owned(result)
}

/// Create and migrate a database connection pool.
///
/// The backend (SQLite or PostgreSQL) is detected from the connection URL.
/// SQLite URLs start with `sqlite:`, PostgreSQL with `postgres://` or `postgresql://`.
pub async fn create_pool(database_url: &str) -> Result<DbPool, sqlx::Error> {
    sqlx::any::install_default_drivers();

    let is_sqlite = database_url.starts_with("sqlite");
    if is_sqlite {
        tracing::info!("using SQLite database: {database_url}");
    }
    IS_POSTGRES.set(!is_sqlite).ok();

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
