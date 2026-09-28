use domain::entities::ProviderId;
use domain::errors::RepositoryError;
use sqlx::PgPool;

pub(super) async fn start_run(
    pool: &PgPool,
    provider_id: &ProviderId,
) -> Result<i64, RepositoryError> {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO sync_runs (cruise_provider_id, status)
         VALUES ($1, 'running') RETURNING id",
    )
    .bind(&provider_id.0)
    .fetch_one(pool)
    .await
    .map_err(|e| RepositoryError::Connection(e.to_string()))?;
    Ok(id)
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn finish_run(
    pool: &PgPool,
    run_id: i64,
    status: &str,
    rows_read: i32,
    rows_written: i32,
    duration_ms: i32,
    error_message: Option<&str>,
) -> Result<(), RepositoryError> {
    sqlx::query(
        "UPDATE sync_runs SET finished_at = now(), status = $2,
             rows_read = $3, rows_written = $4, duration_ms = $5, error_message = $6
         WHERE id = $1",
    )
    .bind(run_id)
    .bind(status)
    .bind(rows_read)
    .bind(rows_written)
    .bind(duration_ms)
    .bind(error_message)
    .execute(pool)
    .await
    .map_err(|e| RepositoryError::Connection(e.to_string()))?;
    Ok(())
}
