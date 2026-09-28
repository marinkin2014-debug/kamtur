use std::sync::Arc;
use std::time::Duration;

use sqlx::PgPool;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use domain::ports::{CruiseRepository, MetricsRecorder};
use infrastructure::repositories::postgres::raw_snapshots_stats;

const RAW_SNAPSHOTS_DAYS: i32 = 90;
const HEALTH_CHECKS_DAYS: i32 = 30;
const RESOLVED_ERRORS_DAYS: i32 = 90;

pub async fn run_retention_loop(
    repository: Arc<dyn CruiseRepository>,
    metrics: Arc<dyn MetricsRecorder>,
    pool: PgPool,
    orphan_stale_secs: i64,
    orphan_check_interval: Duration,
    cancel: CancellationToken,
) {
    // Gauge-метрики размера raw_snapshots — каждые 5 минут.
    let mut size_ticker = tokio::time::interval(Duration::from_secs(300));
    size_ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

    // Retention старых данных — раз в сутки.
    let mut retention_ticker = tokio::time::interval(Duration::from_secs(86400));
    retention_ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    retention_ticker.tick().await; // пропускаем первый

    // Orphan sync_runs — периодически, не на старте.
    let mut orphan_ticker = tokio::time::interval(orphan_check_interval);
    orphan_ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    orphan_ticker.tick().await;

    loop {
        tokio::select! {
            _ = size_ticker.tick() => {
                match raw_snapshots_stats(&pool).await {
                    Ok((size, count)) => {
                        metrics.set_raw_snapshots_size(size);
                        metrics.set_raw_snapshots_count(count);
                    }
                    Err(e) => error!(error = %e, "failed to get raw_snapshots stats"),
                }
            }
            _ = retention_ticker.tick() => {
                match repository
                    .cleanup_old_data(
                        RAW_SNAPSHOTS_DAYS,
                        HEALTH_CHECKS_DAYS,
                        RESOLVED_ERRORS_DAYS,
                    )
                    .await
                {
                    Ok(outcome) => {
                        info!(
                            raw_snapshots_deleted = outcome.raw_snapshots_deleted,
                            health_checks_deleted = outcome.health_checks_deleted,
                            errors_deleted = outcome.errors_deleted,
                            "retention cycle completed"
                        );
                        if let Ok((size, count)) = raw_snapshots_stats(&pool).await {
                            metrics.set_raw_snapshots_size(size);
                            metrics.set_raw_snapshots_count(count);
                        }
                    }
                    Err(e) => error!(error = %e, "retention failed"),
                }
            }
            _ = orphan_ticker.tick() => {
                match repository.cleanup_orphan_sync_runs(orphan_stale_secs).await {
                    Ok(0) => {}
                    Ok(n) => {
                        warn!(
                            orphans = n,
                            stale_secs = orphan_stale_secs,
                            "marked orphan sync_runs as failed"
                        );
                    }
                    Err(e) => error!(error = %e, "orphan sync_runs cleanup failed"),
                }
            }
            _ = cancel.cancelled() => {
                info!("retention loop: shutdown");
                return;
            }
        }
    }
}
