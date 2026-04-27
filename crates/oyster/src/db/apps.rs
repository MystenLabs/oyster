use sqlx::Row;

use crate::{AppId, models::App};

/// Insert a new app.
pub async fn create_app(
    pool: &super::DbPool,
    name: &str,
    contact_email: &str,
    webhook_url: Option<&str>,
) -> Result<App, sqlx::Error> {
    let id = AppId::new();
    let row = sqlx::query(&super::sql(
        "INSERT INTO apps (id, name, contact_email, webhook_url) VALUES (?, ?, ?, ?) RETURNING id, name, contact_email, webhook_url, created_at",
    ))
    .bind(&id)
    .bind(name)
    .bind(contact_email)
    .bind(webhook_url)
    .fetch_one(pool)
    .await?;

    Ok(App {
        id: row.get("id"),
        name: row.get("name"),
        contact_email: row.get("contact_email"),
        webhook_url: row.get("webhook_url"),
        created_at: row.get("created_at"),
    })
}

/// Fetch an app by ID, returning `None` if it does not exist.
pub async fn get_app(pool: &super::DbPool, id: &AppId) -> Result<Option<App>, sqlx::Error> {
    let row = sqlx::query(&super::sql(
        "SELECT id, name, contact_email, webhook_url, created_at FROM apps WHERE id = ?",
    ))
    .bind(id)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|r| App {
        id: r.get("id"),
        name: r.get("name"),
        contact_email: r.get("contact_email"),
        webhook_url: r.get("webhook_url"),
        created_at: r.get("created_at"),
    }))
}

/// List all apps.
pub async fn list_apps(pool: &super::DbPool) -> Result<Vec<App>, sqlx::Error> {
    let rows = sqlx::query(&super::sql(
        "SELECT id, name, contact_email, webhook_url, created_at FROM apps ORDER BY created_at",
    ))
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| App {
            id: r.get("id"),
            name: r.get("name"),
            contact_email: r.get("contact_email"),
            webhook_url: r.get("webhook_url"),
            created_at: r.get("created_at"),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    async fn test_pool() -> super::super::DbPool {
        db::create_pool("sqlite::memory:").await.unwrap()
    }

    #[tokio::test]
    async fn create_app_works() {
        let pool = test_pool().await;
        let app = create_app(&pool, "test-app", "test@example.com", None)
            .await
            .unwrap();
        assert_eq!(app.name, "test-app");
        assert_eq!(app.contact_email, "test@example.com");
        assert!(app.webhook_url.is_none());
    }

    #[tokio::test]
    async fn create_app_with_webhook_url() {
        let pool = test_pool().await;
        let app = create_app(
            &pool,
            "webhook-app",
            "wh@example.com",
            Some("https://example.com/hook"),
        )
        .await
        .unwrap();
        assert_eq!(app.webhook_url.as_deref(), Some("https://example.com/hook"));

        let fetched = get_app(&pool, &app.id).await.unwrap().unwrap();
        assert_eq!(
            fetched.webhook_url.as_deref(),
            Some("https://example.com/hook")
        );
    }

    #[tokio::test]
    async fn get_app_returns_created() {
        let pool = test_pool().await;
        let app = create_app(&pool, "test-app", "test@example.com", None)
            .await
            .unwrap();
        let fetched = get_app(&pool, &app.id).await.unwrap().unwrap();
        assert_eq!(fetched.id, app.id);
        assert_eq!(fetched.name, "test-app");
    }

    #[tokio::test]
    async fn list_apps_works() {
        let pool = test_pool().await;
        // The "internal" app is seeded by the migration.
        let apps = list_apps(&pool).await.unwrap();
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].name, "internal");

        create_app(&pool, "app-2", "a@b.com", None).await.unwrap();
        let apps = list_apps(&pool).await.unwrap();
        assert_eq!(apps.len(), 2);
    }

    #[tokio::test]
    async fn duplicate_name_fails() {
        let pool = test_pool().await;
        create_app(&pool, "dup", "a@b.com", None).await.unwrap();
        let result = create_app(&pool, "dup", "c@d.com", None).await;
        assert!(result.is_err());
    }
}
