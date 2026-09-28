use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, Transaction};

use domain::entities::ProviderId;
use domain::errors::RepositoryError;
use domain::fingerprint::Fingerprint;

use super::error::tx_err;

const ZSTD_LEVEL: i32 = 1;
const ZSTD_MIN_SIZE: usize = 1024 * 1024; // не сжимаем < 1 МБ

pub(super) fn encode_raw(
    raw: &[u8],
    compress: bool,
) -> Result<(Vec<u8>, &'static str), RepositoryError> {
    if !compress || raw.len() < ZSTD_MIN_SIZE {
        return Ok((raw.to_vec(), "none"));
    }
    let compressed = zstd::encode_all(raw, ZSTD_LEVEL)
        .map_err(|e| RepositoryError::Transaction(format!("zstd: {e}")))?;
    Ok((compressed, "zstd"))
}

pub(super) async fn raw_exists(
    pool: &PgPool,
    provider_id: &ProviderId,
    fingerprint: &Fingerprint,
) -> Result<bool, RepositoryError> {
    let exists: Option<i32> = sqlx::query_scalar(
        "SELECT 1 FROM raw_snapshots WHERE cruise_provider_id = $1 AND sha256 = $2",
    )
    .bind(&provider_id.0)
    .bind(fingerprint.as_str())
    .fetch_optional(pool)
    .await
    .map_err(|e| RepositoryError::Connection(e.to_string()))?;
    Ok(exists.is_some())
}

pub(super) async fn insert_snapshot(
    tx: &mut Transaction<'_, Postgres>,
    provider_id: &ProviderId,
    fingerprint: &Fingerprint,
    payload: &[u8],
    encoding: &str,
    observed_at: DateTime<Utc>,
) -> Result<bool, RepositoryError> {
    let inserted: Option<i64> = sqlx::query_scalar(
        "INSERT INTO raw_snapshots
             (cruise_provider_id, sha256, payload, payload_encoding, fetched_at)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (cruise_provider_id, sha256) DO NOTHING
         RETURNING id",
    )
    .bind(&provider_id.0)
    .bind(fingerprint.as_str())
    .bind(payload)
    .bind(encoding)
    .bind(observed_at)
    .fetch_optional(&mut **tx)
    .await
    .map_err(tx_err)?;
    Ok(inserted.is_some())
}
