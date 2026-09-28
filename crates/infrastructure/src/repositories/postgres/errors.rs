use domain::entities::ProviderId;
use domain::errors::RepositoryError;
use sqlx::PgPool;

pub(super) async fn record_error_row(
    pool: &PgPool,
    provider_id: Option<&ProviderId>,
    stage: &str,
    severity: &str,
    message: &str,
    context: serde_json::Value,
) -> Result<(), RepositoryError> {
    sqlx::query(
        "INSERT INTO errors (cruise_provider_id, stage, severity, message, context)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(provider_id.map(|p| p.0.as_str()))
    .bind(stage)
    .bind(severity)
    .bind(message)
    .bind(context)
    .execute(pool)
    .await
    .map_err(|e| RepositoryError::Connection(e.to_string()))?;
    Ok(())
}
