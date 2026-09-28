mod enrich;
mod errors;
mod sync_one;

pub use errors::SyncError;
pub use sync_one::sync_one;

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tracing::{error, info};

use domain::entities::{ProviderId, SyncOutcome};
use domain::ports::{Clock, CruiseProvider, CruiseRepository, MetricsRecorder};

pub struct SyncAllProvidersUseCase {
    providers: Vec<Arc<dyn CruiseProvider>>,
    repository: Arc<dyn CruiseRepository>,
    clock: Arc<dyn Clock>,
    metrics: Arc<dyn MetricsRecorder>,
    max_concurrency: usize,
}

#[derive(Debug)]
pub struct SyncReport {
    pub succeeded: Vec<(ProviderId, SyncOutcome)>,
    pub failed: Vec<(ProviderId, String)>,
    pub duration: Duration,
}

impl SyncAllProvidersUseCase {
    pub fn new(
        providers: Vec<Arc<dyn CruiseProvider>>,
        repository: Arc<dyn CruiseRepository>,
        clock: Arc<dyn Clock>,
        metrics: Arc<dyn MetricsRecorder>,
        max_concurrency: usize,
    ) -> Self {
        Self {
            providers,
            repository,
            clock,
            metrics,
            max_concurrency,
        }
    }

    /// Прогнать sync для всех провайдеров параллельно (с ограничением
    /// `max_concurrency`).
    ///
    /// ## Механика параллельности
    ///
    /// Создаётся локальный `Semaphore` на `max_concurrency` permits.
    /// Цикл по провайдерам делает `acquire → spawn`:
    ///
    /// 1. **Асинхронно берёт permit** (блокируется, если все заняты).
    /// 2. **Спавнит `sync_one` в `JoinSet`**, передавая permit внутрь
    ///    как `_permit` — он дропается вместе с таском, освобождая
    ///    permit для следующей итерации.
    ///
    /// Порядок `acquire → spawn` (а не `spawn → acquire`) даёт
    /// **backpressure**: `JoinSet` не накапливает больше
    /// `max_concurrency` задач одновременно, даже если провайдеров
    /// десятки или сотни. Память под futures остаётся ограниченной.
    ///
    /// ## Долгие провайдеры
    ///
    /// Если `sync_one` одного провайдера висит (медленный upstream),
    /// цикл `for` блокируется на `acquire_owned()`, но уже спавненные
    /// таски продолжают работать. Когда висящий таск завершится
    /// (по таймауту или ошибкой), permit освободится, цикл двинется
    /// дальше.
    pub async fn execute(&self) -> SyncReport {
        let start = Instant::now();
        let semaphore = Arc::new(Semaphore::new(self.max_concurrency));
        let mut join_set: JoinSet<(ProviderId, Result<SyncOutcome, SyncError>)> = JoinSet::new();

        for provider in &self.providers {
            let provider = Arc::clone(provider);
            let repository = Arc::clone(&self.repository);
            let clock = Arc::clone(&self.clock);
            let metrics = Arc::clone(&self.metrics);

            // ## Почему `expect` здесь безопасен
            //
            // `Semaphore::acquire_owned` возвращает `Err(AcquireError)`
            // **только** если семафор закрыт (`Semaphore::close()`).
            //
            // В этой функции `close()` не вызывается ни разу. Спавненные
            // таски его тоже не могут вызвать: они получают
            // `OwnedSemaphorePermit`, а не `Arc<Semaphore>` — drop
            // permit'а освобождает слот, но не закрывает семафор.
            //
            // Единственный способ получить `AcquireError` — дропнуть
            // **все** `Arc<Semaphore>` handles. Пока мы держим `Arc`
            // в этой функции, инвариант невозможно нарушить.
            //
            // Если инвариант когда-нибудь сломается (например, кто-то
            // добавит `close()` при shutdown), мы хотим узнать об этом
            // **громко** — паникой с осмысленным сообщением, а не
            // молчаливой потерей провайдеров из sync-цикла.
            //
            // Замена на `unwrap_or_else(|_| continue)` была бы хуже:
            // при закрытом семафоре цикл терял бы **все** оставшиеся
            // провайдеры без видимого сигнала — что противоречит
            // контракту `SyncReport { failed }`.
            let permit = Arc::clone(&semaphore)
                .acquire_owned()
                .await
                .expect("semaphore is never closed: this Arc is the only handle");

            join_set.spawn(async move {
                let _permit = permit;
                let id = provider.id().clone();
                let result = sync_one(provider, repository, clock, metrics).await;
                (id, result)
            });
        }

        let mut report = SyncReport {
            succeeded: Vec::new(),
            failed: Vec::new(),
            duration: Duration::ZERO,
        };

        while let Some(joined) = join_set.join_next().await {
            match joined {
                Ok((id, Ok(outcome))) => report.succeeded.push((id, outcome)),
                Ok((id, Err(e))) => {
                    error!(provider = %id.0, error = %e, "provider sync failed");
                    report.failed.push((id, e.to_string()));
                }
                Err(join_err) => {
                    error!(error = %join_err, "task panicked");
                    report
                        .failed
                        .push((ProviderId("unknown".into()), join_err.to_string()));
                }
            }
        }

        report.duration = start.elapsed();
        info!(
            ok = report.succeeded.len(),
            failed = report.failed.len(),
            elapsed_ms = report.duration.as_millis() as u64,
            "sync cycle completed"
        );
        report
    }

    /// Прогнать sync **только для одного провайдера**. Используется scheduler'ом,
    /// чтобы тик одного расписания не тянул за собой всех остальных.
    pub async fn execute_provider(&self, provider_id: &ProviderId) -> SyncReport {
        let start = Instant::now();
        let mut report = SyncReport {
            succeeded: Vec::new(),
            failed: Vec::new(),
            duration: Duration::ZERO,
        };

        let Some(provider) = self
            .providers
            .iter()
            .find(|p| p.id() == provider_id)
            .cloned()
        else {
            warn_unknown(provider_id);
            report
                .failed
                .push((provider_id.clone(), "unknown provider".into()));
            report.duration = start.elapsed();
            return report;
        };

        let result = sync_one(
            provider,
            Arc::clone(&self.repository),
            Arc::clone(&self.clock),
            Arc::clone(&self.metrics),
        )
        .await;

        match result {
            Ok(outcome) => report.succeeded.push((provider_id.clone(), outcome)),
            Err(e) => {
                error!(provider = %provider_id.0, error = %e, "provider sync failed");
                report.failed.push((provider_id.clone(), e.to_string()));
            }
        }

        report.duration = start.elapsed();
        info!(
            provider = %provider_id.0,
            ok = report.succeeded.len(),
            failed = report.failed.len(),
            elapsed_ms = report.duration.as_millis() as u64,
            "provider sync completed"
        );
        report
    }
}

#[inline]
fn warn_unknown(provider_id: &ProviderId) {
    tracing::warn!(provider = %provider_id.0, "requested provider not registered");
}
