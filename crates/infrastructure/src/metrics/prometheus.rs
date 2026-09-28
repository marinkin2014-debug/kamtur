//! Реализация `MetricsRecorder` поверх крейта `metrics` (экспорт в Prometheus).

use std::time::Duration;

use metrics::{counter, describe_counter, describe_gauge, describe_histogram, gauge, histogram};

use domain::entities::{SyncPhase, SyncStage, SyncStatus, WriteTable};
use domain::ports::{CircuitState, MetricsRecorder};

pub struct PrometheusMetrics;

impl PrometheusMetrics {
    pub fn new() -> Self {
        Self::describe_all();
        Self
    }

    /// Инициализирует нулевые серии для всех известных лейблов провайдера.
    ///
    /// Зачем: Prometheus не знает о серии, пока она не появится в `/metrics`
    /// хотя бы один раз. Без инициализации дашборд покажет «No data» до
    /// первого реального инкремента — иногда часы. `init_zeros` вызывается
    /// один раз на старте воркера для каждого зарегистрированного провайдера.
    ///
    /// Список стадий должен совпадать с `SyncStage::as_str()`.
    pub fn init_zeros(provider: &str) {
        // Счётчики циклов — все статусы
        counter!("kamtur_sync_cycles_total",
                 "provider" => provider.to_owned(),
                 "status" => "success")
        .increment(0);
        counter!("kamtur_sync_cycles_total",
                 "provider" => provider.to_owned(),
                 "status" => "failed")
        .increment(0);
        counter!("kamtur_sync_cycles_total",
                 "provider" => provider.to_owned(),
                 "status" => "skipped")
        .increment(0);

        // Ошибки — все стадии. Порядок совпадает с SyncStage::as_str().
        for stage in &[
            "fetch", "dedup", "parse", "enrich", "persist", "notify", "unknown",
        ] {
            counter!("kamtur_sync_errors_total",
                     "provider" => provider.to_owned(),
                     "stage" => *stage)
            .increment(0);
        }

        // Deactivated
        counter!("kamtur_deactivated_total",
                 "provider" => provider.to_owned())
        .increment(0);

        // Skipped
        counter!("kamtur_sync_skipped_total",
                 "provider" => provider.to_owned())
        .increment(0);
    }

    fn describe_all() {
        // ---- sync phases ----
        describe_histogram!(
            "kamtur_sync_phase_duration_seconds",
            "Duration of sync phases in seconds"
        );
        describe_counter!(
            "kamtur_sync_cycles_total",
            "Total number of sync cycles by status"
        );
        describe_counter!(
            "kamtur_sync_errors_total",
            "Total number of sync errors by stage"
        );
        describe_counter!(
            "kamtur_sync_skipped_total",
            "Total number of sync cycles skipped due to duplicate content"
        );
        describe_gauge!(
            "kamtur_sync_last_success_timestamp_seconds",
            "Unix timestamp of last successful sync"
        );

        // ---- rows ----
        describe_counter!("kamtur_rows_written_total", "Total rows written by table");
        describe_counter!("kamtur_deactivated_total", "Total deactivated entities");

        // ---- fetch ----
        describe_counter!(
            "kamtur_fetch_bytes_total",
            "Total bytes fetched from provider"
        );

        // ---- db ----
        describe_gauge!(
            "kamtur_raw_snapshots_size_bytes",
            "Approximate total size of raw_snapshots table"
        );
        describe_gauge!(
            "kamtur_raw_snapshots_count",
            "Number of rows in raw_snapshots table"
        );
        describe_gauge!(
            "kamtur_db_pool_available",
            "Available connections in DB pool"
        );
        describe_gauge!("kamtur_db_pool_size", "Current size of DB pool");

        // ---- enrichment ----
        describe_gauge!(
            "kamtur_enrichment_rules_loaded",
            "Number of enrichment rules loaded per provider"
        );

        // ---- circuit breaker ----
        describe_gauge!(
            "kamtur_circuit_breaker_state",
            "Circuit breaker state: 0=closed, 1=open, 2=half_open"
        );
    }
}

impl Default for PrometheusMetrics {
    fn default() -> Self {
        Self::new()
    }
}

#[inline]
fn cb_state_value(state: CircuitState) -> f64 {
    match state {
        CircuitState::Closed => 0.0,
        CircuitState::Open => 1.0,
        CircuitState::HalfOpen => 2.0,
    }
}

impl MetricsRecorder for PrometheusMetrics {
    #[inline]
    fn record_sync_phase(&self, provider: &str, phase: SyncPhase, duration: Duration) {
        histogram!(
            "kamtur_sync_phase_duration_seconds",
            "provider" => provider.to_owned(),
            "phase" => phase.as_str(),
        )
        .record(duration.as_secs_f64());
    }

    #[inline]
    fn increment_sync_cycle(&self, provider: &str, status: SyncStatus) {
        counter!(
            "kamtur_sync_cycles_total",
            "provider" => provider.to_owned(),
            "status" => status.as_str(),
        )
        .increment(1);
    }

    #[inline]
    fn increment_sync_error(&self, provider: &str, stage: SyncStage) {
        counter!(
            "kamtur_sync_errors_total",
            "provider" => provider.to_owned(),
            "stage" => stage.as_str(),
        )
        .increment(1);
    }

    #[inline]
    fn increment_sync_skipped(&self, provider: &str) {
        counter!(
            "kamtur_sync_skipped_total",
            "provider" => provider.to_owned(),
        )
        .increment(1);
    }

    #[inline]
    fn set_last_success_timestamp(&self, provider: &str, unix_seconds: i64) {
        gauge!(
            "kamtur_sync_last_success_timestamp_seconds",
            "provider" => provider.to_owned(),
        )
        .set(unix_seconds as f64);
    }

    #[inline]
    fn increment_rows_written(&self, provider: &str, table: WriteTable, count: u64) {
        if count == 0 {
            return;
        }
        counter!(
            "kamtur_rows_written_total",
            "provider" => provider.to_owned(),
            "table" => table.as_str(),
        )
        .increment(count);
    }

    #[inline]
    fn increment_deactivated(&self, provider: &str, count: u64) {
        if count == 0 {
            return;
        }
        counter!(
            "kamtur_deactivated_total",
            "provider" => provider.to_owned(),
        )
        .increment(count);
    }

    #[inline]
    fn increment_fetch_bytes(&self, provider: &str, bytes: u64) {
        counter!(
            "kamtur_fetch_bytes_total",
            "provider" => provider.to_owned(),
        )
        .increment(bytes);
    }

    #[inline]
    fn set_raw_snapshots_size(&self, bytes: u64) {
        gauge!("kamtur_raw_snapshots_size_bytes").set(bytes as f64);
    }

    #[inline]
    fn set_raw_snapshots_count(&self, count: i64) {
        gauge!("kamtur_raw_snapshots_count").set(count as f64);
    }

    #[inline]
    fn set_db_pool_available(&self, available: u32) {
        gauge!("kamtur_db_pool_available").set(available as f64);
    }

    #[inline]
    fn set_db_pool_size(&self, size: u32) {
        gauge!("kamtur_db_pool_size").set(size as f64);
    }

    #[inline]
    fn set_enrichment_rules_loaded(&self, provider: &str, count: usize) {
        gauge!(
            "kamtur_enrichment_rules_loaded",
            "provider" => provider.to_owned(),
        )
        .set(count as f64);
    }

    #[inline]
    fn set_circuit_breaker_state(&self, provider: &str, state: CircuitState) {
        // Single gauge без label `state`: иначе серия `{state="open"}` остаётся
        // навсегда после первого перехода, и alert не резолвится.
        gauge!(
            "kamtur_circuit_breaker_state",
            "provider" => provider.to_owned(),
        )
        .set(cb_state_value(state));
    }
}
