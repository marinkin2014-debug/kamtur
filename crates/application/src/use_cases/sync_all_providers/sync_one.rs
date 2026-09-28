use std::sync::Arc;
use std::time::Instant;

use tracing::{error, info, instrument};

use domain::entities::{SyncOutcome, SyncPhase, SyncStatus, WriteTable};
use domain::fingerprint::Fingerprint;
use domain::ports::{Clock, CruiseProvider, CruiseRepository, MetricsRecorder};

use super::enrich::enrich_tours;
use super::errors::SyncError;

pub async fn sync_one(
    provider: Arc<dyn CruiseProvider>,
    repository: Arc<dyn CruiseRepository>,
    clock: Arc<dyn Clock>,
    metrics: Arc<dyn MetricsRecorder>,
) -> Result<SyncOutcome, SyncError> {
    let id = provider.id().clone();
    let started = Instant::now();

    // `record_sync_run_start` — работа с репозиторием до основного цикла.
    // Ближайшая по смыслу стадия — Persist.
    let run_id = repository
        .record_sync_run_start(&id)
        .await
        .map_err(SyncError::persist)?;

    // Все логи внутри sync_inner несут provider и run_id. По run_id
    // в логах можно найти запись в sync_runs.
    let span = tracing::info_span!("sync_cycle", provider = %id.0, run_id);
    let _enter = span.enter();

    let result = sync_inner(
        &id,
        run_id,
        provider,
        Arc::clone(&repository),
        clock.clone(),
        Arc::clone(&metrics),
    )
    .await;

    let duration_ms = started.elapsed().as_millis() as i32;

    match &result {
        Ok(outcome) => {
            // Согласованность: и в метриках, и в БД статус одинаковый.
            let run_status = if outcome.duplicate {
                "skipped"
            } else {
                "success"
            };

            if outcome.duplicate {
                metrics.increment_sync_cycle(&id.0, SyncStatus::Skipped);
                metrics.increment_sync_skipped(&id.0);
            } else {
                metrics.increment_sync_cycle(&id.0, SyncStatus::Success);
                metrics.set_last_success_timestamp(&id.0, clock.now().timestamp());
                record_write_metrics(&*metrics, &id.0, outcome);
            }

            let rows_written = outcome.objects_upserted
                + outcome.stages_upserted
                + outcome.classes_upserted
                + outcome.rooms_upserted
                + outcome.tours_upserted
                + outcome.prices_created
                + outcome.prices_updated
                + outcome.sales_created
                + outcome.sales_updated
                + outcome.availability_created
                + outcome.availability_updated;

            let rows_read =
                rows_written + outcome.prices_closed + outcome.sales_closed + outcome.deactivated;

            if let Err(err) = repository
                .record_sync_run_finish(
                    run_id,
                    run_status,
                    rows_read as i32,
                    rows_written as i32,
                    duration_ms,
                    None,
                )
                .await
            {
                error!(error = %err, run_id, "failed to record sync run finish");
            }
        }
        Err(e) => {
            // Единая точка инкремента. `stage()` знает точную стадию.
            metrics.increment_sync_cycle(&id.0, SyncStatus::Failed);
            metrics.increment_sync_error(&id.0, e.stage());

            let msg = e.to_string();
            if let Err(err) = repository
                .record_sync_run_finish(run_id, "failed", 0, 0, duration_ms, Some(&msg))
                .await
            {
                error!(error = %err, run_id, "failed to record sync run finish");
            }
            if let Err(err) = repository
                .record_error(
                    Some(&id),
                    e.stage().as_str(),
                    "error",
                    &msg,
                    serde_json::json!({ "duration_ms": duration_ms }),
                )
                .await
            {
                error!(error = %err, provider = %id.0, "failed to record sync error");
            }
        }
    }

    result
}

// Синтаксис `fields(provider = %id.0, run_id = run_id)`:
// левая часть — имя поля span'а, правая — выражение, которое его даёт.
// Без `= run_id` макрос не считает переменную использованной,
// и компилятор выдаёт `unused variable`.
#[instrument(skip_all, fields(provider = %id.0, run_id = run_id))]
async fn sync_inner(
    id: &domain::entities::ProviderId,
    run_id: i64,
    provider: Arc<dyn CruiseProvider>,
    repository: Arc<dyn CruiseRepository>,
    clock: Arc<dyn Clock>,
    metrics: Arc<dyn MetricsRecorder>,
) -> Result<SyncOutcome, SyncError> {
    // ---- 1. Fetch ----
    let t0 = Instant::now();
    let raw = provider.fetch_raw().await.map_err(SyncError::fetch)?;
    let fetch_ms = t0.elapsed().as_millis() as u64;
    metrics.record_sync_phase(&id.0, SyncPhase::Fetch, t0.elapsed());
    metrics.increment_fetch_bytes(&id.0, raw.len() as u64);

    // ---- 2. Hash ----
    let t1 = Instant::now();
    let fingerprint = Fingerprint::of(&raw);
    let hash_ms = t1.elapsed().as_millis() as u64;
    metrics.record_sync_phase(&id.0, SyncPhase::Hash, t1.elapsed());

    // ---- 3. Dedup ----
    let t_dedup = Instant::now();
    let is_dup = repository
        .has_snapshot(id, &fingerprint)
        .await
        .map_err(SyncError::dedup)?;
    metrics.record_sync_phase(&id.0, SyncPhase::Dedup, t_dedup.elapsed());
    if is_dup {
        info!(provider = %id.0, fetch_ms, hash_ms, "sync skipped (duplicate)");
        return Ok(SyncOutcome {
            duplicate: true,
            ..Default::default()
        });
    }

    // ---- 4. Parse ----
    let t2 = Instant::now();
    let p = Arc::clone(&provider);
    let raw_for_parse = raw.clone();
    let canonical = tokio::task::spawn_blocking(move || p.parse(&raw_for_parse))
        .await
        .map_err(|e| SyncError::Parse(format!("parse task join: {e}")))?
        .map_err(SyncError::parse)?;
    let parse_ms = t2.elapsed().as_millis() as u64;
    metrics.record_sync_phase(&id.0, SyncPhase::Parse, t2.elapsed());

    // Guard от парсерного бага.
    if canonical.objects.is_empty() && canonical.tours.is_empty() {
        return Err(SyncError::Parse(
            "canonical data is empty (no objects, no tours); refusing to persist \
             and deactivate everything — likely a parser/provider failure"
                .into(),
        ));
    }

    // ---- 5. Load rules ----
    let rules = repository
        .load_enrichment_rules(id)
        .await
        .map_err(SyncError::enrich)?;
    metrics.set_enrichment_rules_loaded(&id.0, rules.len());

    // ---- 6. Enrich ----
    let t3 = Instant::now();
    let enriched_tours = enrich_tours(&canonical, &rules, provider.provider_config());
    let enrich_ms = t3.elapsed().as_millis() as u64;
    metrics.record_sync_phase(&id.0, SyncPhase::Enrich, t3.elapsed());

    // ---- 7. Persist ----
    let t4 = Instant::now();
    let outcome = repository
        .apply_sync(
            id,
            &raw,
            &fingerprint,
            &canonical,
            &enriched_tours,
            clock.now(),
        )
        .await
        .map_err(SyncError::persist)?;
    let persist_ms = t4.elapsed().as_millis() as u64;
    metrics.record_sync_phase(&id.0, SyncPhase::Persist, t4.elapsed());

    info!(
        provider = %id.0,
        fetch_ms, hash_ms, parse_ms, enrich_ms, persist_ms,
        objects = canonical.objects.len(),
        tours = canonical.tours.len(),
        prices = canonical.prices.len(),
        sales = canonical.sales.len(),
        availability = canonical.availability.len(),
        avail_created = outcome.availability_created,
        avail_updated = outcome.availability_updated,
        avail_closed = outcome.availability_closed,
        deactivated = outcome.deactivated,
        "sync done"
    );

    Ok(outcome)
}

/// Пробрасывает все счётчики `SyncOutcome` в `MetricsRecorder`.
///
/// Покрытие: 14 из 14 полей-счётчиков `SyncOutcome` (кроме флага `duplicate`).
/// В частности — `*_closed`, которые отражают закрытие SCD2-версий: они
/// не входят в «rows written» по логике «новая строка», но реально пишут
/// UPDATE и должны быть видны на дашборде.
#[inline]
fn record_write_metrics(metrics: &dyn MetricsRecorder, provider: &str, outcome: &SyncOutcome) {
    metrics.increment_rows_written(
        provider,
        WriteTable::Objects,
        outcome.objects_upserted as u64,
    );
    metrics.increment_rows_written(provider, WriteTable::Stages, outcome.stages_upserted as u64);
    metrics.increment_rows_written(
        provider,
        WriteTable::Classes,
        outcome.classes_upserted as u64,
    );
    metrics.increment_rows_written(provider, WriteTable::Rooms, outcome.rooms_upserted as u64);
    metrics.increment_rows_written(provider, WriteTable::Tours, outcome.tours_upserted as u64);

    metrics.increment_rows_written(
        provider,
        WriteTable::PricesCreated,
        outcome.prices_created as u64,
    );
    metrics.increment_rows_written(
        provider,
        WriteTable::PricesUpdated,
        outcome.prices_updated as u64,
    );
    metrics.increment_rows_written(
        provider,
        WriteTable::PricesClosed,
        outcome.prices_closed as u64,
    );

    metrics.increment_rows_written(
        provider,
        WriteTable::SalesCreated,
        outcome.sales_created as u64,
    );
    metrics.increment_rows_written(
        provider,
        WriteTable::SalesUpdated,
        outcome.sales_updated as u64,
    );
    metrics.increment_rows_written(
        provider,
        WriteTable::SalesClosed,
        outcome.sales_closed as u64,
    );

    metrics.increment_rows_written(
        provider,
        WriteTable::AvailabilityCreated,
        outcome.availability_created as u64,
    );
    metrics.increment_rows_written(
        provider,
        WriteTable::AvailabilityUpdated,
        outcome.availability_updated as u64,
    );
    metrics.increment_rows_written(
        provider,
        WriteTable::AvailabilityClosed,
        outcome.availability_closed as u64,
    );

    metrics.increment_deactivated(provider, outcome.deactivated as u64);
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::entities::SyncStage;
    use domain::ports::CircuitState;
    use std::sync::Mutex;
    use std::time::Duration;

    /// Тестовый `MetricsRecorder`, записывающий все вызовы. Позволяет
    /// проверить, что `record_write_metrics` покрывает все поля `SyncOutcome`
    /// без пропусков.
    #[derive(Default)]
    struct RecordingMetrics {
        rows: Mutex<Vec<(String, WriteTable, u64)>>,
        deactivated: Mutex<Vec<(String, u64)>>,
    }

    impl MetricsRecorder for RecordingMetrics {
        fn increment_rows_written(&self, provider: &str, table: WriteTable, count: u64) {
            self.rows
                .lock()
                .unwrap()
                .push((provider.to_string(), table, count));
        }

        fn increment_deactivated(&self, provider: &str, count: u64) {
            self.deactivated
                .lock()
                .unwrap()
                .push((provider.to_string(), count));
        }

        // ---- noop для остального трейта ----
        fn record_sync_phase(&self, _: &str, _: SyncPhase, _: Duration) {}
        fn increment_sync_cycle(&self, _: &str, _: SyncStatus) {}
        fn increment_sync_error(&self, _: &str, _: SyncStage) {}
        fn increment_sync_skipped(&self, _: &str) {}
        fn set_last_success_timestamp(&self, _: &str, _: i64) {}
        fn increment_fetch_bytes(&self, _: &str, _: u64) {}
        fn set_raw_snapshots_size(&self, _: u64) {}
        fn set_raw_snapshots_count(&self, _: i64) {}
        fn set_db_pool_available(&self, _: u32) {}
        fn set_db_pool_size(&self, _: u32) {}
        fn set_enrichment_rules_loaded(&self, _: &str, _: usize) {}
        fn set_circuit_breaker_state(&self, _: &str, _: CircuitState) {}
    }

    fn full_outcome() -> SyncOutcome {
        SyncOutcome {
            duplicate: false,
            objects_upserted: 1,
            stages_upserted: 2,
            classes_upserted: 3,
            rooms_upserted: 4,
            tours_upserted: 5,
            prices_created: 6,
            prices_updated: 7,
            prices_closed: 8,
            sales_created: 9,
            sales_updated: 10,
            sales_closed: 11,
            availability_created: 12,
            availability_updated: 13,
            availability_closed: 14,
            deactivated: 15,
        }
    }

    /// Регрессия: `record_write_metrics` должен покрывать все 14 полей
    /// `SyncOutcome`. До фикса не передавались `prices_closed`,
    /// `sales_closed`, `availability_closed` — дашборд «Rows written»
    /// недооценивал работу воркера.
    #[test]
    fn record_write_metrics_covers_all_tables() {
        let m = RecordingMetrics::default();
        let outcome = full_outcome();

        record_write_metrics(&m, "test-provider", &outcome);

        let rows = m.rows.lock().unwrap();
        assert_eq!(
            rows.len(),
            14,
            "ожидается 14 вызовов increment_rows_written (по одному на WriteTable)"
        );

        // Все варианты уникальны.
        let mut seen = std::collections::HashSet::new();
        for (provider, table, _) in rows.iter() {
            assert_eq!(provider, "test-provider");
            assert!(seen.insert(*table), "дубликат WriteTable: {:?}", table);
        }

        // Явно проверяем новые Closed-варианты.
        assert!(
            rows.iter().any(|(_, t, _)| *t == WriteTable::PricesClosed),
            "prices_closed не передан в MetricsRecorder"
        );
        assert!(
            rows.iter().any(|(_, t, _)| *t == WriteTable::SalesClosed),
            "sales_closed не передан в MetricsRecorder"
        );
        assert!(
            rows.iter()
                .any(|(_, t, _)| *t == WriteTable::AvailabilityClosed),
            "availability_closed не передан в MetricsRecorder"
        );

        // Значения совпадают с полями outcome.
        let get = |t: WriteTable| -> u64 {
            rows.iter()
                .find(|(_, table, _)| *table == t)
                .map(|(_, _, count)| *count)
                .unwrap_or_else(|| panic!("{:?} не передан", t))
        };
        assert_eq!(get(WriteTable::PricesClosed), 8);
        assert_eq!(get(WriteTable::SalesClosed), 11);
        assert_eq!(get(WriteTable::AvailabilityClosed), 14);

        // Deactivated — отдельная метрика, не через increment_rows_written.
        let d = m.deactivated.lock().unwrap();
        assert_eq!(d.len(), 1, "ровно один вызов increment_deactivated");
        assert_eq!(d[0], ("test-provider".to_string(), 15));
    }

    /// Duplicate outcome (все счётчики 0) не должен ничего писать.
    /// `record_write_metrics` вызывается только при `!duplicate`, но
    /// проверим, что нули дают нулевые вызовы — это сигнал, что нет
    /// пропущенных нулевых инкрементов.
    #[test]
    fn record_write_metrics_zero_outcome_writes_only_zeros() {
        let m = RecordingMetrics::default();
        let outcome = SyncOutcome::default();

        record_write_metrics(&m, "p", &outcome);

        let rows = m.rows.lock().unwrap();
        assert_eq!(rows.len(), 14, "все 14 вызовов сделаны");
        for (_, _, count) in rows.iter() {
            assert_eq!(*count, 0, "все значения нулевые");
        }

        let d = m.deactivated.lock().unwrap();
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].1, 0);
    }
}
