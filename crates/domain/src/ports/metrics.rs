//! Порт метрик.
//!
//! Реализации:
//!   - `PrometheusMetrics` (в infrastructure) — реальные метрики.
//!   - `NoopMetrics` (в infrastructure) — заглушка для тестов.
//!
//! Методы не возвращают Result: метрики — fire-and-forget.
//! Падение recorder'а не должно ломать бизнес-логику.

use std::time::Duration;

use crate::entities::{SyncPhase, SyncStage, SyncStatus, WriteTable};

pub trait MetricsRecorder: Send + Sync {
    // ----- Sync phases -----
    fn record_sync_phase(&self, provider: &str, phase: SyncPhase, duration: Duration);

    // ----- Sync lifecycle -----
    fn increment_sync_cycle(&self, provider: &str, status: SyncStatus);
    fn increment_sync_error(&self, provider: &str, stage: SyncStage);
    fn increment_sync_skipped(&self, provider: &str);
    fn set_last_success_timestamp(&self, provider: &str, unix_seconds: i64);

    // ----- Rows -----
    fn increment_rows_written(&self, provider: &str, table: WriteTable, count: u64);
    fn increment_deactivated(&self, provider: &str, count: u64);

    // ----- Fetch -----
    fn increment_fetch_bytes(&self, provider: &str, bytes: u64);

    // ----- DB / system -----
    fn set_raw_snapshots_size(&self, bytes: u64);
    fn set_raw_snapshots_count(&self, count: i64);
    fn set_db_pool_available(&self, available: u32);
    fn set_db_pool_size(&self, size: u32);

    // ----- Enrichment -----
    fn set_enrichment_rules_loaded(&self, provider: &str, count: usize);

    // ----- Circuit breaker -----
    fn set_circuit_breaker_state(&self, provider: &str, state: CircuitState);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CircuitState {
    Closed,
    Open,
    HalfOpen,
}

impl CircuitState {
    #[inline]
    pub fn as_str(&self) -> &'static str {
        match self {
            CircuitState::Closed => "closed",
            CircuitState::Open => "open",
            CircuitState::HalfOpen => "half_open",
        }
    }
}
