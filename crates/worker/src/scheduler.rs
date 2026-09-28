use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use cron::Schedule;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use application::use_cases::sync_all_providers::SyncAllProvidersUseCase;
use domain::entities::ProviderId;
use domain::ports::{CruiseRepository, HealthCheck};

#[derive(Clone)]
pub struct ProviderSchedule {
    pub provider_id: String,
    pub cron: String,
    pub run_on_start: bool,
}

pub struct Scheduler {
    schedules: Vec<ProviderSchedule>,
    sync_uc: Arc<SyncAllProvidersUseCase>,
    cancel: CancellationToken,
}

impl Scheduler {
    pub fn new(
        schedules: Vec<ProviderSchedule>,
        sync_uc: Arc<SyncAllProvidersUseCase>,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            schedules,
            sync_uc,
            cancel,
        }
    }

    pub async fn run(self) {
        let mut handles = Vec::with_capacity(self.schedules.len());

        for schedule in self.schedules {
            let uc = Arc::clone(&self.sync_uc);
            let cancel = self.cancel.clone();
            let provider_id = schedule.provider_id.clone();
            let cron_str = schedule.cron.clone();
            let run_on_start = schedule.run_on_start;

            let tick_lock = Arc::new(Mutex::new(()));

            handles.push(tokio::spawn(async move {
                let parsed = match Schedule::from_str(&cron_str) {
                    Ok(s) => s,
                    Err(e) => {
                        error!(provider_id, cron = cron_str, error = %e, "invalid cron");
                        return;
                    }
                };

                if run_on_start {
                    run_tick(&uc, &provider_id, &tick_lock, "startup").await;
                }

                loop {
                    let next = match parsed.upcoming(Utc).next() {
                        Some(n) => n,
                        None => {
                            error!(provider_id, "no upcoming tick");
                            return;
                        }
                    };
                    let now = Utc::now();
                    let wait = (next - now).to_std().unwrap_or(Duration::ZERO);

                    tokio::select! {
                        _ = tokio::time::sleep(wait) => { /* tick fires */ }
                        _ = cancel.cancelled() => {
                            info!(provider_id, "scheduler: shutdown");
                            return;
                        }
                    }

                    run_tick(&uc, &provider_id, &tick_lock, "scheduled").await;
                }
            }));
        }

        for h in handles {
            let _ = h.await;
        }
    }
}

async fn run_tick(
    uc: &Arc<SyncAllProvidersUseCase>,
    provider_id: &str,
    lock: &Arc<Mutex<()>>,
    kind: &str,
) {
    let permit = match lock.try_lock() {
        Ok(p) => p,
        Err(_) => {
            warn!(
                provider_id,
                kind, "previous sync still running, skipping tick"
            );
            return;
        }
    };

    info!(provider_id, kind, "tick");
    let pid = ProviderId(provider_id.to_string());
    let _ = uc.execute_provider(&pid).await;
    drop(permit);
}

pub async fn health_loop(
    checks: Vec<Arc<dyn HealthCheck>>,
    repository: Arc<dyn CruiseRepository>,
    interval: Duration,
    cancel: CancellationToken,
) {
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                for check in &checks {
                    let component = check.component().to_string();
                    let status = check.check().await;

                    if status.status != "ok" {
                        warn!(
                            component = %component,
                            status = %status.status,
                            details = ?status.details,
                            "health check not ok"
                        );
                    }

                    if let Err(e) = repository
                        .record_health_check(
                            &component,
                            &status.status,
                            status.latency_ms,
                            status.details.clone(),
                        )
                        .await
                    {
                        error!(component = %component, error = %e, "failed to record health check");
                    }
                }
            }
            _ = cancel.cancelled() => {
                info!("health loop: shutdown");
                return;
            }
        }
    }
}
