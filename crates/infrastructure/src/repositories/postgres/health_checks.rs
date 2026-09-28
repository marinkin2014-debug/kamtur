use domain::errors::RepositoryError;
use sqlx::PgPool;

pub(super) async fn record_health_row(
    pool: &PgPool,
    component: &str,
    status: &str,
    latency_ms: Option<i32>,
    details: serde_json::Value,
) -> Result<(), RepositoryError> {
    sqlx::query(
        "INSERT INTO health_checks (component, status, latency_ms, details)
         VALUES ($1, $2, $3, $4)",
    )
    .bind(component)
    .bind(status)
    .bind(latency_ms)
    .bind(details)
    .execute(pool)
    .await
    .map_err(|e| RepositoryError::Connection(e.to_string()))?;
    Ok(())
}
