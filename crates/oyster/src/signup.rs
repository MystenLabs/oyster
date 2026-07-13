//! Self-serve web signup: Turnstile anti-bot verification, Google
//! OAuth, and the `/signup` pages. Enabled only when
//! [`crate::config::SignupConfig`] is present.

pub mod google;
pub mod routes;
pub mod turnstile;

/// Interval between expired-session sweeps.
const SESSION_SWEEP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(3600);

/// Periodically delete expired web sessions. Hygiene only — expiry is
/// enforced at lookup time regardless (`web_sessions::find_active_by_hash`),
/// and logins opportunistically clean their own user's expired rows;
/// this just keeps dead rows from accumulating forever. Safe with
/// multiple replicas: concurrent deletes of the same rows are harmless.
///
/// Runs forever; spawn it (`tokio::spawn`) alongside the server when
/// signup is enabled.
pub async fn run_session_sweep(db: crate::db::DbPool) {
    loop {
        match crate::db::web_sessions::delete_expired(&db).await {
            Ok(0) => {}
            Ok(n) => tracing::debug!(deleted = n, "swept expired web sessions"),
            Err(e) => tracing::error!(error = %e, "web session sweep failed"),
        }
        tokio::time::sleep(SESSION_SWEEP_INTERVAL).await;
    }
}
