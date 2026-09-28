use async_trait::async_trait;
use bytes::Bytes;

use crate::entities::{
    CanonicalData, ClaimedErrorBatch, EnrichedTour, ObservedAt, ProviderId, SyncOutcome,
};
use crate::errors::RepositoryError;
use crate::fingerprint::Fingerprint;
use crate::rules::EnrichmentRule;

// ============================================================
// SnapshotRepository
// ============================================================

#[async_trait]
pub trait SnapshotRepository: Send + Sync {
    async fn has_snapshot(
        &self,
        provider_id: &ProviderId,
        fingerprint: &Fingerprint,
    ) -> Result<bool, RepositoryError>;
}

// ============================================================
// EnrichmentRuleRepository
// ============================================================

#[async_trait]
pub trait EnrichmentRuleRepository: Send + Sync {
    async fn load_enrichment_rules(
        &self,
        provider_id: &ProviderId,
    ) -> Result<Vec<EnrichmentRule>, RepositoryError>;
}

// ============================================================
// SyncRepository
// ============================================================

#[async_trait]
pub trait SyncRepository: Send + Sync {
    async fn apply_sync(
        &self,
        provider_id: &ProviderId,
        raw: &Bytes,
        fingerprint: &Fingerprint,
        canonical: &CanonicalData,
        enriched_tours: &[EnrichedTour],
        observed_at: ObservedAt,
    ) -> Result<SyncOutcome, RepositoryError>;
}

// ============================================================
// ErrorRepository
// ============================================================

#[async_trait]
pub trait ErrorRepository: Send + Sync {
    async fn record_error(
        &self,
        provider_id: Option<&ProviderId>,
        stage: &str,
        severity: &str,
        message: &str,
        context: serde_json::Value,
    ) -> Result<(), RepositoryError>;

    async fn record_sync_run_start(&self, provider_id: &ProviderId)
        -> Result<i64, RepositoryError>;

    async fn record_sync_run_finish(
        &self,
        run_id: i64,
        status: &str,
        rows_read: i32,
        rows_written: i32,
        duration_ms: i32,
        error_message: Option<&str>,
    ) -> Result<(), RepositoryError>;
}

// ============================================================
// ErrorNotificationRepository — two-phase доставка дайджеста
// ============================================================

/// Two-phase доставка error-дайджестов.
///
/// Контракт:
///   1. `claim_batch` — атомарно забирает N ошибок, помечает `claimed`,
///      инкрементит `notification_attempts`, ставит lease.
///   2. Отправка email — вне транзакции.
///   3. `mark_sent(ids)` — при успехе. Переводит в `sent`, ставит `notified_at`.
///      Идемпотентно.
///   4. `mark_failed(ids, max_attempts)` — при ошибке. Возвращает в `pending`
///      для повтора ИЛИ переводит в `dead`, если попытки исчерпаны.
///
/// Если worker упал между claim и ack/nack — запись останется `claimed`
/// с истёкшим lease. Следующий `claim_batch` подхватит её.
#[async_trait]
pub trait ErrorNotificationRepository: Send + Sync {
    async fn claim_batch(
        &self,
        window_secs: i64,
        max_attempts: i32,
        lease_secs: i64,
        limit: i64,
    ) -> Result<ClaimedErrorBatch, RepositoryError>;

    /// Помечает указанные id как `sent`. Идемпотентно.
    async fn mark_sent(&self, ids: &[i64]) -> Result<(), RepositoryError>;

    /// Помечает указанные id как `pending` (retry) или `dead` (исчерпаны).
    async fn mark_failed(&self, ids: &[i64], max_attempts: i32) -> Result<(), RepositoryError>;

    /// Переводит зависшие `claimed` с исчерпанными попытками в `dead`.
    async fn sweep_dead_claims(
        &self,
        max_attempts: i32,
        lease_secs: i64,
    ) -> Result<u64, RepositoryError>;
}

// ============================================================
// MaintenanceRepository
// ============================================================

#[async_trait]
pub trait MaintenanceRepository: Send + Sync {
    async fn cleanup_old_data(
        &self,
        raw_snapshots_days: i32,
        health_checks_days: i32,
        resolved_errors_days: i32,
    ) -> Result<crate::entities::RetentionOutcome, RepositoryError>;

    async fn cleanup_orphan_sync_runs(&self, stale_after_secs: i64)
        -> Result<u64, RepositoryError>;
}

// ============================================================
// Композитный супертрейт для удобства use cases
// ============================================================

pub trait CruiseRepository:
    SnapshotRepository
    + EnrichmentRuleRepository
    + SyncRepository
    + ErrorRepository
    + ErrorNotificationRepository
    + MaintenanceRepository
    + crate::ports::HealthRepository
{
}

impl<T> CruiseRepository for T where
    T: SnapshotRepository
        + EnrichmentRuleRepository
        + SyncRepository
        + ErrorRepository
        + ErrorNotificationRepository
        + MaintenanceRepository
        + crate::ports::HealthRepository
{
}
