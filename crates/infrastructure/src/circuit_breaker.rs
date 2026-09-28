use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

use domain::errors::ProviderError;
use domain::ports::{CircuitState, MetricsRecorder};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Closed,
    Open,
    HalfOpen,
}

struct Inner {
    state: State,
    failures: u32,
    successes: u32,
    opened_at: Option<Instant>,
}

/// Circuit breaker с экспортом состояния в `MetricsRecorder`.
///
/// Каждый переход состояния (`Closed → Open`, `Open → HalfOpen`,
/// `HalfOpen → Closed`) обновляет gauge `kamtur_circuit_breaker_state`.
/// Без этого alert `CircuitBreakerOpen` не работает.
///
/// Gauge имеет один label — `provider`. Значение: 0=closed, 1=open,
/// 2=half_open. Не используем label `state`, потому что тогда серия
/// `{state="open"}` остаётся в TSDB навсегда после первого перехода,
/// и alert не резолвится.
pub struct CircuitBreaker {
    inner: Arc<Mutex<Inner>>,
    failure_threshold: u32,
    success_threshold: u32,
    timeout: Duration,
    provider_id: String,
    metrics: Arc<dyn MetricsRecorder>,
}

impl CircuitBreaker {
    pub fn new(
        failure_threshold: u32,
        success_threshold: u32,
        timeout: Duration,
        provider_id: String,
        metrics: Arc<dyn MetricsRecorder>,
    ) -> Self {
        let cb = Self {
            inner: Arc::new(Mutex::new(Inner {
                state: State::Closed,
                failures: 0,
                successes: 0,
                opened_at: None,
            })),
            failure_threshold,
            success_threshold,
            timeout,
            provider_id,
            metrics,
        };

        // Инициализируем gauge — чтобы на дашборде серия существовала
        // с первого scrape'а, ещё до первого перехода.
        cb.metrics
            .set_circuit_breaker_state(&cb.provider_id, CircuitState::Closed);

        cb
    }

    pub async fn call<F, Fut, T>(&self, f: F) -> Result<T, ProviderError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T, ProviderError>>,
    {
        // Фаза 1: проверяем состояние под локом. Если Open и timeout прошёл —
        // переводим в HalfOpen и запоминаем, что нужно обновить gauge.
        let mut transition: Option<CircuitState> = None;
        {
            let mut inner = self.inner.lock().await;
            if inner.state == State::Open {
                if let Some(opened) = inner.opened_at {
                    if opened.elapsed() >= self.timeout {
                        inner.state = State::HalfOpen;
                        inner.successes = 0;
                        transition = Some(CircuitState::HalfOpen);
                    } else {
                        return Err(ProviderError::CircuitOpen);
                    }
                }
            }
        }
        if let Some(state) = transition {
            self.metrics
                .set_circuit_breaker_state(&self.provider_id, state);
        }

        // Фаза 2: сам вызов, вне лока.
        let result = f().await;

        // Фаза 3: обновляем счётчики и состояние.
        let mut transition: Option<CircuitState> = None;
        {
            let mut inner = self.inner.lock().await;
            match result {
                Ok(ref _v) => {
                    if inner.state == State::HalfOpen {
                        inner.successes += 1;
                        if inner.successes >= self.success_threshold {
                            inner.state = State::Closed;
                            inner.failures = 0;
                            inner.successes = 0;
                            inner.opened_at = None;
                            transition = Some(CircuitState::Closed);
                        }
                    } else {
                        inner.failures = 0;
                    }
                }
                Err(_) => {
                    inner.failures += 1;
                    if inner.failures >= self.failure_threshold {
                        inner.state = State::Open;
                        inner.opened_at = Some(Instant::now());
                        transition = Some(CircuitState::Open);
                    }
                }
            }
        }
        if let Some(state) = transition {
            self.metrics
                .set_circuit_breaker_state(&self.provider_id, state);
        }

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::entities::{SyncPhase, SyncStage, SyncStatus, WriteTable};
    use domain::ports::CircuitState;
    use std::sync::Mutex as StdMutex;

    /// Тестовый `MetricsRecorder`, который запоминает последнее состояние CB.
    /// Остальные методы — noop (используются только для того, чтобы
    /// удовлетворить трейт).
    struct TestMetrics {
        last: StdMutex<Option<(String, CircuitState)>>,
    }

    impl TestMetrics {
        fn new() -> Self {
            Self {
                last: StdMutex::new(None),
            }
        }

        fn last(&self) -> Option<(String, CircuitState)> {
            self.last.lock().unwrap().clone()
        }
    }

    impl MetricsRecorder for TestMetrics {
        fn set_circuit_breaker_state(&self, provider: &str, state: CircuitState) {
            *self.last.lock().unwrap() = Some((provider.to_string(), state));
        }

        fn record_sync_phase(&self, _: &str, _: SyncPhase, _: Duration) {}
        fn increment_sync_cycle(&self, _: &str, _: SyncStatus) {}
        fn increment_sync_error(&self, _: &str, _: SyncStage) {}
        fn increment_sync_skipped(&self, _: &str) {}
        fn set_last_success_timestamp(&self, _: &str, _: i64) {}
        fn increment_rows_written(&self, _: &str, _: WriteTable, _: u64) {}
        fn increment_deactivated(&self, _: &str, _: u64) {}
        fn increment_fetch_bytes(&self, _: &str, _: u64) {}
        fn set_raw_snapshots_size(&self, _: u64) {}
        fn set_raw_snapshots_count(&self, _: i64) {}
        fn set_db_pool_available(&self, _: u32) {}
        fn set_db_pool_size(&self, _: u32) {}
        fn set_enrichment_rules_loaded(&self, _: &str, _: usize) {}
    }

    #[tokio::test]
    async fn initializes_metric_as_closed() {
        let m = Arc::new(TestMetrics::new());
        let _cb = CircuitBreaker::new(10, 2, Duration::from_secs(60), "1".into(), m.clone());

        let (provider, state) = m.last().expect("metric set on init");
        assert_eq!(provider, "1");
        assert_eq!(state, CircuitState::Closed);
    }

    #[tokio::test]
    async fn reports_open_after_threshold() {
        let m = Arc::new(TestMetrics::new());
        let cb = CircuitBreaker::new(3, 2, Duration::from_secs(60), "1".into(), m.clone());

        for _ in 0..3 {
            let _ = cb
                .call(|| async { Err::<(), _>(ProviderError::Network("test".into())) })
                .await;
        }

        let (_, state) = m.last().expect("metric set after failures");
        assert_eq!(state, CircuitState::Open);
    }

    #[tokio::test]
    async fn open_circuit_rejects_until_timeout() {
        let m = Arc::new(TestMetrics::new());
        let cb = CircuitBreaker::new(2, 1, Duration::from_secs(3600), "1".into(), m.clone());

        // Открываем
        for _ in 0..2 {
            let _ = cb
                .call(|| async { Err::<(), _>(ProviderError::Network("x".into())) })
                .await;
        }

        // Следующий вызов — сразу CircuitOpen, без выполнения f().
        let result = cb.call(|| async { Ok::<(), ProviderError>(()) }).await;
        assert!(matches!(result, Err(ProviderError::CircuitOpen)));
    }

    #[tokio::test]
    async fn half_open_recovers_to_closed() {
        let m = Arc::new(TestMetrics::new());
        let cb = CircuitBreaker::new(2, 2, Duration::from_millis(1), "1".into(), m.clone());

        // Открываем
        for _ in 0..2 {
            let _ = cb
                .call(|| async { Err::<(), _>(ProviderError::Network("x".into())) })
                .await;
        }
        assert_eq!(m.last().unwrap().1, CircuitState::Open);

        tokio::time::sleep(Duration::from_millis(10)).await;

        // Первый успешный — HalfOpen, ещё не Closed
        let r = cb.call(|| async { Ok::<(), ProviderError>(()) }).await;
        assert!(r.is_ok());

        // Второй успешный — Closed
        let r = cb.call(|| async { Ok::<(), ProviderError>(()) }).await;
        assert!(r.is_ok());
        assert_eq!(m.last().unwrap().1, CircuitState::Closed);
    }
}
