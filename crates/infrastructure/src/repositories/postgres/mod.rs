#[cfg(test)]
pub mod test_support;

mod availability;
mod deactivation;
mod error;
mod errors;
mod health_checks;
mod maintenance;
mod objects;
mod prices;
mod read;
mod retry;
mod rules;
mod sales;
mod snapshot;
mod sync_runs;
mod tours;

use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use bytes::Bytes;
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use tracing::info;

use domain::entities::*;
use domain::errors::RepositoryError;
use domain::fingerprint::Fingerprint;
use domain::ports::{
    EnrichmentRuleRepository, ErrorNotificationRepository, ErrorRepository, HealthRepository,
    MaintenanceRepository, MetricsRecorder, SnapshotRepository, SyncRepository,
};
use domain::rules::EnrichmentRule;

use self::availability::apply_availability_scd2;
use self::deactivation::deactivate_missing;
use self::errors::record_error_row;
use self::health_checks::record_health_row;
use self::maintenance::{
    claim_batch, cleanup_old_data, cleanup_orphan_runs, mark_failed, mark_sent, sweep_dead_claims,
};
use self::objects::{upsert_classes, upsert_objects, upsert_rooms, upsert_stages};
use self::prices::apply_prices_scd2;
use self::retry::{with_retry, RetryPolicy};
use self::rules::load_rules;
use self::sales::apply_sales_scd2;
use self::snapshot::{encode_raw, insert_snapshot, raw_exists};
use self::sync_runs::{finish_run, start_run};
use self::tours::upsert_tours;

pub use maintenance::raw_snapshots_stats;
pub use read::PostgresCruiseReadRepository;

pub struct PostgresCruiseRepository {
    pool: PgPool,
    compress_raw: bool,
    batch_size: usize,
    metrics: Arc<dyn MetricsRecorder>,
}

impl PostgresCruiseRepository {
    pub fn new(
        pool: PgPool,
        compress_raw: bool,
        batch_size: usize,
        metrics: Arc<dyn MetricsRecorder>,
    ) -> Self {
        Self {
            pool,
            compress_raw,
            batch_size,
            metrics,
        }
    }
}

// ============================================================
// SnapshotRepository
// ============================================================

#[async_trait]
impl SnapshotRepository for PostgresCruiseRepository {
    async fn has_snapshot(
        &self,
        provider_id: &ProviderId,
        fingerprint: &Fingerprint,
    ) -> Result<bool, RepositoryError> {
        // Быстрый read-only. Fail fast: если БД недоступна дольше
        // ~150 мс, sync-цикл сам повторится в следующем тике scheduler'а.
        with_retry("has_snapshot", RetryPolicy::quick(), || {
            raw_exists(&self.pool, provider_id, fingerprint)
        })
        .await
    }
}

// ============================================================
// EnrichmentRuleRepository
// ============================================================

#[async_trait]
impl EnrichmentRuleRepository for PostgresCruiseRepository {
    async fn load_enrichment_rules(
        &self,
        provider_id: &ProviderId,
    ) -> Result<Vec<EnrichmentRule>, RepositoryError> {
        with_retry("load_enrichment_rules", RetryPolicy::quick(), || {
            load_rules(&self.pool, provider_id)
        })
        .await
    }
}

// ============================================================
// SyncRepository
// ============================================================

#[async_trait]
impl SyncRepository for PostgresCruiseRepository {
    async fn apply_sync(
        &self,
        provider_id: &ProviderId,
        raw: &Bytes,
        fingerprint: &Fingerprint,
        canonical: &CanonicalData,
        enriched_tours: &[EnrichedTour],
        observed_at: DateTime<Utc>,
    ) -> Result<SyncOutcome, RepositoryError> {
        // Тяжёлая транзакция. Standard: 5 попыток, ~15 с worst case.
        // Хватает на кратковременный failover Postgres.
        with_retry("apply_sync", RetryPolicy::standard(), || {
            self.apply_sync_once(
                provider_id,
                raw,
                fingerprint,
                canonical,
                enriched_tours,
                observed_at,
            )
        })
        .await
    }
}

impl PostgresCruiseRepository {
    async fn apply_sync_once(
        &self,
        provider_id: &ProviderId,
        raw: &Bytes,
        fingerprint: &Fingerprint,
        canonical: &CanonicalData,
        enriched_tours: &[EnrichedTour],
        observed_at: DateTime<Utc>,
    ) -> Result<SyncOutcome, RepositoryError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| RepositoryError::Connection(e.to_string()))?;

        // Локально для этой транзакции — быстрее COMMIT.
        sqlx::query("SET LOCAL synchronous_commit = off")
            .execute(&mut *tx)
            .await
            .map_err(|e| RepositoryError::Transaction(e.to_string()))?;

        // Сериализуем sync одного провайдера. Разные — параллельно.
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1)::bigint)")
            .bind(&provider_id.0)
            .execute(&mut *tx)
            .await
            .map_err(|e| RepositoryError::Transaction(e.to_string()))?;

        let (payload, encoding) = encode_raw(raw, self.compress_raw)?;
        let inserted = insert_snapshot(
            &mut tx,
            provider_id,
            fingerprint,
            &payload,
            encoding,
            observed_at,
        )
        .await?;

        if !inserted {
            tx.rollback().await.ok();
            return Ok(SyncOutcome {
                duplicate: true,
                ..Default::default()
            });
        }

        let mut timings = Timings::default();

        let t = Instant::now();
        upsert_objects(
            &mut tx,
            provider_id,
            &canonical.objects,
            self.batch_size,
            observed_at,
        )
        .await?;
        timings.objects_ms = t.elapsed().as_millis() as u64;

        let t = Instant::now();
        upsert_stages(
            &mut tx,
            provider_id,
            &canonical.stages,
            self.batch_size,
            observed_at,
        )
        .await?;
        timings.stages_ms = t.elapsed().as_millis() as u64;

        let t = Instant::now();
        upsert_classes(
            &mut tx,
            provider_id,
            &canonical.classes,
            self.batch_size,
            observed_at,
        )
        .await?;
        timings.classes_ms = t.elapsed().as_millis() as u64;

        let t = Instant::now();
        upsert_rooms(
            &mut tx,
            provider_id,
            &canonical.rooms,
            self.batch_size,
            observed_at,
        )
        .await?;
        timings.rooms_ms = t.elapsed().as_millis() as u64;

        let t = Instant::now();
        upsert_tours(
            &mut tx,
            provider_id,
            &canonical.tours,
            enriched_tours,
            self.batch_size,
            observed_at,
        )
        .await?;
        timings.tours_ms = t.elapsed().as_millis() as u64;

        let t = Instant::now();
        let prices = apply_prices_scd2(
            &mut tx,
            provider_id,
            &canonical.prices,
            self.batch_size,
            observed_at,
        )
        .await?;
        timings.prices_ms = t.elapsed().as_millis() as u64;

        let t = Instant::now();
        let sales = apply_sales_scd2(
            &mut tx,
            provider_id,
            &canonical.sales,
            self.batch_size,
            observed_at,
        )
        .await?;
        timings.sales_ms = t.elapsed().as_millis() as u64;

        let t = Instant::now();
        let avail = apply_availability_scd2(
            &mut tx,
            provider_id,
            &canonical.availability,
            self.batch_size,
            observed_at,
        )
        .await?;
        timings.avail_ms = t.elapsed().as_millis() as u64;

        let t = Instant::now();
        let deactivated = deactivate_missing(
            &mut tx,
            provider_id,
            canonical,
            self.batch_size,
            observed_at,
        )
        .await?;
        timings.deact_ms = t.elapsed().as_millis() as u64;

        let t = Instant::now();
        tx.commit()
            .await
            .map_err(|e| RepositoryError::Transaction(e.to_string()))?;
        timings.commit_ms = t.elapsed().as_millis() as u64;

        info!(
            objects_ms = timings.objects_ms,
            stages_ms = timings.stages_ms,
            classes_ms = timings.classes_ms,
            rooms_ms = timings.rooms_ms,
            tours_ms = timings.tours_ms,
            prices_ms = timings.prices_ms,
            sales_ms = timings.sales_ms,
            availability_ms = timings.avail_ms,
            deactivate_ms = timings.deact_ms,
            commit_ms = timings.commit_ms,
            "persist breakdown"
        );

        Ok(SyncOutcome {
            duplicate: false,
            objects_upserted: canonical.objects.len(),
            stages_upserted: canonical.stages.len(),
            classes_upserted: canonical.classes.len(),
            rooms_upserted: canonical.rooms.len(),
            tours_upserted: canonical.tours.len(),
            prices_created: prices.created,
            prices_updated: prices.updated,
            prices_closed: prices.closed,
            sales_created: sales.created,
            sales_updated: sales.updated,
            sales_closed: sales.closed,
            availability_created: avail.created,
            availability_updated: avail.updated,
            availability_closed: avail.closed,
            deactivated,
        })
    }
}

// ============================================================
// ErrorRepository
// ============================================================

#[async_trait]
impl ErrorRepository for PostgresCruiseRepository {
    async fn record_error(
        &self,
        provider_id: Option<&ProviderId>,
        stage: &str,
        severity: &str,
        message: &str,
        context: serde_json::Value,
    ) -> Result<(), RepositoryError> {
        record_error_row(&self.pool, provider_id, stage, severity, message, context).await
    }

    async fn record_sync_run_start(
        &self,
        provider_id: &ProviderId,
    ) -> Result<i64, RepositoryError> {
        with_retry("record_sync_run_start", RetryPolicy::quick(), || {
            start_run(&self.pool, provider_id)
        })
        .await
    }

    async fn record_sync_run_finish(
        &self,
        run_id: i64,
        status: &str,
        rows_read: i32,
        rows_written: i32,
        duration_ms: i32,
        error_message: Option<&str>,
    ) -> Result<(), RepositoryError> {
        with_retry("record_sync_run_finish", RetryPolicy::quick(), || {
            finish_run(
                &self.pool,
                run_id,
                status,
                rows_read,
                rows_written,
                duration_ms,
                error_message,
            )
        })
        .await
    }
}

// ============================================================
// ErrorNotificationRepository
// ============================================================

#[async_trait]
impl ErrorNotificationRepository for PostgresCruiseRepository {
    async fn claim_batch(
        &self,
        window_secs: i64,
        max_attempts: i32,
        lease_secs: i64,
        limit: i64,
    ) -> Result<ClaimedErrorBatch, RepositoryError> {
        with_retry("claim_batch", RetryPolicy::quick(), || {
            claim_batch(&self.pool, window_secs, max_attempts, lease_secs, limit)
        })
        .await
    }

    async fn mark_sent(&self, ids: &[i64]) -> Result<(), RepositoryError> {
        with_retry("mark_sent", RetryPolicy::quick(), || {
            mark_sent(&self.pool, ids)
        })
        .await
    }

    async fn mark_failed(&self, ids: &[i64], max_attempts: i32) -> Result<(), RepositoryError> {
        with_retry("mark_failed", RetryPolicy::quick(), || {
            mark_failed(&self.pool, ids, max_attempts)
        })
        .await
    }

    async fn sweep_dead_claims(
        &self,
        max_attempts: i32,
        lease_secs: i64,
    ) -> Result<u64, RepositoryError> {
        with_retry("sweep_dead_claims", RetryPolicy::quick(), || {
            sweep_dead_claims(&self.pool, max_attempts, lease_secs)
        })
        .await
    }
}

// ============================================================
// HealthRepository
// ============================================================

#[async_trait]
impl HealthRepository for PostgresCruiseRepository {
    async fn record_health_check(
        &self,
        component: &str,
        status: &str,
        latency_ms: Option<i32>,
        details: serde_json::Value,
    ) -> Result<(), RepositoryError> {
        record_health_row(&self.pool, component, status, latency_ms, details).await
    }
}

// ============================================================
// MaintenanceRepository
// ============================================================

#[async_trait]
impl MaintenanceRepository for PostgresCruiseRepository {
    async fn cleanup_old_data(
        &self,
        raw_snapshots_days: i32,
        health_checks_days: i32,
        resolved_errors_days: i32,
    ) -> Result<RetentionOutcome, RepositoryError> {
        // Тяжёлая операция с несколькими DELETE. Standard: даёт шанс
        // пережить кратковременный failover. Patient (91 с) не
        // используем — 90 с могут превысить бюджет graceful shutdown
        // retention loop'а при остановке worker'а.
        let outcome = with_retry("cleanup_old_data", RetryPolicy::standard(), || {
            cleanup_old_data(
                &self.pool,
                raw_snapshots_days,
                health_checks_days,
                resolved_errors_days,
            )
        })
        .await?;

        if let Ok((size, count)) = raw_snapshots_stats(&self.pool).await {
            self.metrics.set_raw_snapshots_size(size);
            self.metrics.set_raw_snapshots_count(count);
        }

        Ok(outcome)
    }

    async fn cleanup_orphan_sync_runs(
        &self,
        stale_after_secs: i64,
    ) -> Result<u64, RepositoryError> {
        with_retry("cleanup_orphan_sync_runs", RetryPolicy::quick(), || {
            cleanup_orphan_runs(&self.pool, stale_after_secs)
        })
        .await
    }
}

// ============================================================
// Внутренние тайминги
// ============================================================

#[derive(Debug, Default)]
struct Timings {
    objects_ms: u64,
    stages_ms: u64,
    classes_ms: u64,
    rooms_ms: u64,
    tours_ms: u64,
    prices_ms: u64,
    sales_ms: u64,
    avail_ms: u64,
    deact_ms: u64,
    commit_ms: u64,
}
