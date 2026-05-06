use sqlx::Row;

use crate::{AppId, models::App};

/// Insert a new app.
pub async fn create_app(
    pool: &super::DbPool,
    name: &str,
    contact_email: &str,
) -> Result<App, sqlx::Error> {
    let id = AppId::new();
    let row = sqlx::query(&super::sql(
        "INSERT INTO apps (id, name, contact_email) VALUES (?, ?, ?) \
         RETURNING id, name, contact_email, webhook_url, webhook_public_key, created_at",
    ))
    .bind(&id)
    .bind(name)
    .bind(contact_email)
    .fetch_one(pool)
    .await?;

    Ok(App {
        id: row.get("id"),
        name: row.get("name"),
        contact_email: row.get("contact_email"),
        webhook_url: row.get("webhook_url"),
        webhook_public_key: row.get("webhook_public_key"),
        created_at: row.get("created_at"),
    })
}

/// Fetch an app by ID, returning `None` if it does not exist.
pub async fn get_app(pool: &super::DbPool, id: &AppId) -> Result<Option<App>, sqlx::Error> {
    let row = sqlx::query(&super::sql(
        "SELECT id, name, contact_email, webhook_url, webhook_public_key, created_at \
         FROM apps WHERE id = ?",
    ))
    .bind(id)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|r| App {
        id: r.get("id"),
        name: r.get("name"),
        contact_email: r.get("contact_email"),
        webhook_url: r.get("webhook_url"),
        webhook_public_key: r.get("webhook_public_key"),
        created_at: r.get("created_at"),
    }))
}

/// List all apps.
pub async fn list_apps(pool: &super::DbPool) -> Result<Vec<App>, sqlx::Error> {
    let rows = sqlx::query(&super::sql(
        "SELECT id, name, contact_email, webhook_url, webhook_public_key, created_at \
         FROM apps ORDER BY created_at",
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
            webhook_public_key: r.get("webhook_public_key"),
            created_at: r.get("created_at"),
        })
        .collect())
}

/// Set or replace the webhook URL and Ed25519 keypair for an app, returning
/// the updated row. Each call is a fresh-keypair rotation.
pub async fn set_app_webhook(
    pool: &super::DbPool,
    app_id: &AppId,
    webhook_url: &str,
    public_key_b64: &str,
    private_key_b64: &str,
) -> Result<App, sqlx::Error> {
    let row = sqlx::query(&super::sql(
        "UPDATE apps SET webhook_url = ?, webhook_public_key = ?, webhook_private_key = ? \
         WHERE id = ? \
         RETURNING id, name, contact_email, webhook_url, webhook_public_key, created_at",
    ))
    .bind(webhook_url)
    .bind(public_key_b64)
    .bind(private_key_b64)
    .bind(app_id)
    .fetch_one(pool)
    .await?;

    Ok(App {
        id: row.get("id"),
        name: row.get("name"),
        contact_email: row.get("contact_email"),
        webhook_url: row.get("webhook_url"),
        webhook_public_key: row.get("webhook_public_key"),
        created_at: row.get("created_at"),
    })
}

/// Clear the webhook URL and keypair for an app, returning the updated row.
pub async fn clear_app_webhook(pool: &super::DbPool, app_id: &AppId) -> Result<App, sqlx::Error> {
    let row = sqlx::query(&super::sql(
        "UPDATE apps SET webhook_url = NULL, webhook_public_key = NULL, webhook_private_key = NULL \
         WHERE id = ? \
         RETURNING id, name, contact_email, webhook_url, webhook_public_key, created_at",
    ))
    .bind(app_id)
    .fetch_one(pool)
    .await?;

    Ok(App {
        id: row.get("id"),
        name: row.get("name"),
        contact_email: row.get("contact_email"),
        webhook_url: row.get("webhook_url"),
        webhook_public_key: row.get("webhook_public_key"),
        created_at: row.get("created_at"),
    })
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
        let app = create_app(&pool, "test-app", "test@example.com")
            .await
            .unwrap();
        assert_eq!(app.name, "test-app");
        assert_eq!(app.contact_email, "test@example.com");
        assert!(app.webhook_url.is_none());
        assert!(app.webhook_public_key.is_none());
    }

    #[tokio::test]
    async fn get_app_returns_created() {
        let pool = test_pool().await;
        let app = create_app(&pool, "test-app", "test@example.com")
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

        create_app(&pool, "app-2", "a@b.com").await.unwrap();
        let apps = list_apps(&pool).await.unwrap();
        assert_eq!(apps.len(), 2);
    }

    #[tokio::test]
    async fn duplicate_name_fails() {
        let pool = test_pool().await;
        create_app(&pool, "dup", "a@b.com").await.unwrap();
        let result = create_app(&pool, "dup", "c@d.com").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn set_app_webhook_writes_url_and_keys() {
        let pool = test_pool().await;
        let app = create_app(&pool, "wh-app", "wh@example.com").await.unwrap();
        let updated = set_app_webhook(
            &pool,
            &app.id,
            "https://example.com/hook",
            "pubkey-b64",
            "privkey-b64",
        )
        .await
        .unwrap();
        assert_eq!(
            updated.webhook_url.as_deref(),
            Some("https://example.com/hook")
        );
        assert_eq!(updated.webhook_public_key.as_deref(), Some("pubkey-b64"));

        let fetched = get_app(&pool, &app.id).await.unwrap().unwrap();
        assert_eq!(
            fetched.webhook_url.as_deref(),
            Some("https://example.com/hook")
        );
        assert_eq!(fetched.webhook_public_key.as_deref(), Some("pubkey-b64"));
    }

    #[tokio::test]
    async fn set_app_webhook_overwrites_existing_keypair() {
        let pool = test_pool().await;
        let app = create_app(&pool, "wh-app", "wh@example.com").await.unwrap();
        set_app_webhook(&pool, &app.id, "https://example.com/hook", "pub1", "priv1")
            .await
            .unwrap();
        let after = set_app_webhook(&pool, &app.id, "https://example.com/hook", "pub2", "priv2")
            .await
            .unwrap();
        assert_eq!(after.webhook_public_key.as_deref(), Some("pub2"));
    }

    #[tokio::test]
    async fn clear_app_webhook_nulls_all_three() {
        let pool = test_pool().await;
        let app = create_app(&pool, "wh-app", "wh@example.com").await.unwrap();
        set_app_webhook(&pool, &app.id, "https://example.com/hook", "pub1", "priv1")
            .await
            .unwrap();

        let cleared = clear_app_webhook(&pool, &app.id).await.unwrap();
        assert!(cleared.webhook_url.is_none());
        assert!(cleared.webhook_public_key.is_none());

        let row = sqlx::query(&db::sql(
            "SELECT webhook_url, webhook_public_key, webhook_private_key FROM apps WHERE id = ?",
        ))
        .bind(&app.id)
        .fetch_one(&pool)
        .await
        .unwrap();
        let url: Option<String> = row.get("webhook_url");
        let pubk: Option<String> = row.get("webhook_public_key");
        let privk: Option<String> = row.get("webhook_private_key");
        assert!(url.is_none());
        assert!(pubk.is_none());
        assert!(privk.is_none());
    }
}
