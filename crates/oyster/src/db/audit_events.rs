use uuid::Uuid;

use crate::AppId;

/// Insert an audit-event row.
///
/// `event_data` is serialized to JSON text and stored verbatim. The DB layer
/// does not interpret it; queries are typically filtered by `app_id` +
/// `event_type` and the JSON body is read application-side.
pub async fn record_audit_event(
    pool: &super::DbPool,
    app_id: &AppId,
    actor_admin_key_id: Option<&str>,
    event_type: &str,
    event_data: serde_json::Value,
) -> Result<(), sqlx::Error> {
    let id = Uuid::new_v4().to_string();
    let event_data = serde_json::to_string(&event_data).expect("serialize audit event_data");
    sqlx::query(&super::sql(
        "INSERT INTO audit_events (id, app_id, actor_admin_key_id, event_type, event_data) \
         VALUES (?, ?, ?, ?, ?)",
    ))
    .bind(&id)
    .bind(app_id)
    .bind(actor_admin_key_id)
    .bind(event_type)
    .bind(&event_data)
    .execute(pool)
    .await?;
    Ok(())
}

/// One audit event row, as returned by `list_audit_events_by_app`.
#[derive(Debug, Clone)]
pub struct AuditEvent {
    /// Unique identifier.
    pub id: String,
    /// Owning app.
    pub app_id: AppId,
    /// Admin key id that performed the action, if any.
    pub actor_admin_key_id: Option<String>,
    /// Event-type discriminator, e.g. `"webhook.url_set"`.
    pub event_type: String,
    /// JSON-encoded structured event payload.
    pub event_data: String,
    /// ISO 8601 creation timestamp.
    pub created_at: String,
}

/// List every audit event for an app, oldest first. Not exposed over the
/// HTTP API; used by tests and operator queries.
pub async fn list_audit_events_by_app(
    pool: &super::DbPool,
    app_id: &AppId,
) -> Result<Vec<AuditEvent>, sqlx::Error> {
    use sqlx::Row;
    let rows = sqlx::query(&super::sql(
        "SELECT id, app_id, actor_admin_key_id, event_type, event_data, created_at \
         FROM audit_events WHERE app_id = ? ORDER BY created_at, id",
    ))
    .bind(app_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| AuditEvent {
            id: r.get("id"),
            app_id: r.get("app_id"),
            actor_admin_key_id: r.get("actor_admin_key_id"),
            event_type: r.get("event_type"),
            event_data: r.get("event_data"),
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
    async fn record_audit_event_writes_row() {
        let pool = test_pool().await;
        let app = db::apps::create_app(&pool, "audit-app", "a@example.com")
            .await
            .unwrap();

        record_audit_event(
            &pool,
            &app.id,
            Some("admin-key-1"),
            "webhook.url_set",
            serde_json::json!({ "host": "example.com", "public_key_fingerprint": "deadbeef" }),
        )
        .await
        .unwrap();

        let events = list_audit_events_by_app(&pool, &app.id).await.unwrap();
        assert_eq!(events.len(), 1);
        let row = &events[0];
        assert_eq!(row.app_id, app.id);
        assert_eq!(row.actor_admin_key_id.as_deref(), Some("admin-key-1"));
        assert_eq!(row.event_type, "webhook.url_set");
        let parsed: serde_json::Value = serde_json::from_str(&row.event_data).unwrap();
        assert_eq!(parsed["host"], "example.com");
        assert_eq!(parsed["public_key_fingerprint"], "deadbeef");
    }

    #[tokio::test]
    async fn record_audit_event_allows_null_actor() {
        let pool = test_pool().await;
        let app = db::apps::create_app(&pool, "audit-app", "a@example.com")
            .await
            .unwrap();

        record_audit_event(
            &pool,
            &app.id,
            None,
            "webhook.url_cleared",
            serde_json::json!({}),
        )
        .await
        .unwrap();

        let events = list_audit_events_by_app(&pool, &app.id).await.unwrap();
        assert_eq!(events.len(), 1);
        assert!(events[0].actor_admin_key_id.is_none());
    }
}
