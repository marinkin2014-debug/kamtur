//! No-op реализация для тестов.
//! Не требует установки Prometheus recorder'а.

use std::time::Duration;

use domain::entities::{SyncPhase, SyncStage, SyncStatus, WriteTable};
use domain::ports::{CircuitState, MetricsRecorder};

pub struct NoopMetrics;

impl MetricsRecorder for NoopMetrics {
    #[inline]
    fn record_sync_phase(&self, _provider: &str, _phase: SyncPhase, _duration: Duration) {}

    #[inline]
    fn increment_sync_cycle(&self, _provider: &str, _status: SyncStatus) {}

    #[inline]
    fn increment_sync_error(&self, _provider: &str, _stage: SyncStage) {}

    #[inline]
    fn increment_sync_skipped(&self, _provider: &str) {}

    #[inline]
    fn set_last_success_timestamp(&self, _provider: &str, _unix_seconds: i64) {}

    #[inline]
    fn increment_rows_written(&self, _provider: &str, _table: WriteTable, _count: u64) {}

    #[inline]
    fn increment_deactivated(&self, _provider: &str, _count: u64) {}

    #[inline]
    fn increment_fetch_bytes(&self, _provider: &str, _bytes: u64) {}

    #[inline]
    fn set_raw_snapshots_size(&self, _bytes: u64) {}

    #[inline]
    fn set_raw_snapshots_count(&self, _count: i64) {}

    #[inline]
    fn set_db_pool_available(&self, _available: u32) {}

    #[inline]
    fn set_db_pool_size(&self, _size: u32) {}

    #[inline]
    fn set_enrichment_rules_loaded(&self, _provider: &str, _count: usize) {}

    #[inline]
    fn set_circuit_breaker_state(&self, _provider: &str, _state: CircuitState) {}
}
