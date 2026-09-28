//! Retry-обёртка для DB-операций.
//!
//! Ретраит **только** `RepositoryError::Connection`. Не ретраит
//! `Transaction` — constraint violation или aborted transaction
//! повторно не пройдут, повтор только усугубит ситуацию.
//!
//! ## Стратегия backoff
//!
//! Exponential backoff с **full jitter** (AWS recommendation,
//! <https://aws.amazon.com/blogs/architecture/exponential-backoff-and-jitter/>).
//!
//! На каждой итерации вычисляется базовый delay `base = prev * multiplier`
//! (клампится к `max_delay`), фактическая задержка берётся как случайная
//! в `[0, base]`.
//!
//! ## Почему jitter критичен
//!
//! Без jitter'а несколько тасков при всплеске ошибок БД синхронизируются:
//! все ждут `T=1с`, потом все ждут `T=2с`, … — и одновременно бьют по БД,
//! вызывая новый всплеск («thundering herd»). Jitter разбивает cohort'ы
//! на равномерное распределение, снижая пиковую нагрузку пропорционально
//! числу параллельных клиентов.
//!
//! ## Источник случайности
//!
//! `SystemTime::now().subsec_nanos()` — не криптографически стойкий,
//! но нам это и не нужно. Задача — не предсказуемость, а разброс попыток
//! между независимыми вызывающими. Nano-precision даёт достаточно
//! энтропии; коллизии на десятках задач статистически несущественны.
//!
//! Зависимость на `rand` не тянем: лишние ~100 КБ бинарника ради
//! размытия retry-попыток — нерационально.

use std::time::Duration;

use tracing::warn;

use domain::errors::RepositoryError;

/// Политика retry: сколько попыток, с какими задержками и jitter'ом.
///
/// Конструируется через один из готовых конструкторов
/// (`quick`, `standard`, `patient`) либо вручную — все поля `pub`.
///
/// ## Контракт полей
///
/// - `max_attempts` — общее число попыток, включая первую. `0` клампится
///   в `1` при нормализации: минимум одна попытка обязательна (иначе
///   функция не вызвала бы `f()` ни разу и вернула синтетическую ошибку,
///   что вводит в заблуждение).
///
/// - `initial_delay` — базовая задержка перед **второй** попыткой
///   (после первой ошибки). Перед первой попыткой задержки нет.
///   Клампится к `max_delay`.
///
/// - `max_delay` — потолок для base delay. Защита от overflow'а и от
///   неадекватно длинных пауз при большом числе попыток.
///
/// - `multiplier` — множитель экспоненты. Клампится к `>= 1.0` (иначе
///   delay бы уменьшался, что противоречит смыслу backoff).
///
/// - `jitter` — если `true`, фактическая задержка = `random(0..=base)`.
///   Если `false` — `base` без разброса (используется в тестах для
///   проверки формулы экспоненты).
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub initial_delay: Duration,
    pub max_delay: Duration,
    pub multiplier: f64,
    pub jitter: bool,
}

impl RetryPolicy {
    /// Для быстрых операций: `has_snapshot`, `load_enrichment_rules`,
    /// `mark_sent`/`mark_failed`, `sweep_*`, `start_run`/`finish_run`.
    ///
    /// Sleeps: 50 мс + 100 мс = **150 мс** worst case без jitter, ~75 мс
    /// в среднем с jitter. Fail fast: если БД лежит дольше 150 мс, нет
    /// смысла ждать — sync-цикл повторится в следующем тике scheduler'а.
    pub const fn quick() -> Self {
        Self {
            max_attempts: 3,
            initial_delay: Duration::from_millis(50),
            max_delay: Duration::from_millis(500),
            multiplier: 2.0,
            jitter: true,
        }
    }

    /// Для тяжёлых операций: `apply_sync`, `cleanup_old_data`.
    ///
    /// Sleeps: 1 + 2 + 4 + 8 = **15 с** worst case без jitter. Даёт
    /// шанс пережить кратковременный failover Postgres (patroni
    /// switchover занимает 10–30 с).
    pub const fn standard() -> Self {
        Self {
            max_attempts: 5,
            initial_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(16),
            multiplier: 2.0,
            jitter: true,
        }
    }

    /// Для очень долгих операций: `cleanup_old_data` при больших
    /// объёмах, миграции.
    ///
    /// Sleeps: 1 + 2 + 4 + 8 + 16 + 30 + 30 = **91 с** worst case.
    /// Использовать осознанно — затягивает graceful shutdown worker'а
    /// (по умолчанию `SHUTDOWN_GRACE_PERIOD_SECS=60`, retry может
    /// превысить бюджет).
    #[allow(dead_code)]
    pub const fn patient() -> Self {
        Self {
            max_attempts: 8,
            initial_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(30),
            multiplier: 2.0,
            jitter: true,
        }
    }

    /// Клампит поля к допустимым значениям. Вызывается внутри
    /// `with_retry` — защита от некорректных пользовательских значений
    /// (например, `max_attempts: 0`).
    fn normalized(self) -> Self {
        Self {
            max_attempts: self.max_attempts.max(1),
            initial_delay: self.initial_delay.min(self.max_delay),
            max_delay: self.max_delay,
            multiplier: self.multiplier.max(1.0),
            jitter: self.jitter,
        }
    }
}

/// Retry с exponential backoff + full jitter. См. документацию модуля.
///
/// ## Семантика
///
/// - Ретраит **только** `RepositoryError::Connection`.
/// - `RepositoryError::Transaction` возвращается немедленно: это
///   constraint violation или aborted transaction; повтор не поможет.
/// - Возвращает `Ok(T)` при первой успешной попытке.
/// - Возвращает `Err(Connection)` последней попытки при исчерпании.
pub async fn with_retry<F, Fut, T>(
    op: &str,
    policy: RetryPolicy,
    mut f: F,
) -> Result<T, RepositoryError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, RepositoryError>>,
{
    let policy = policy.normalized();
    let mut base_delay = policy.initial_delay;

    for attempt in 1..=policy.max_attempts {
        match f().await {
            Ok(v) => return Ok(v),
            Err(RepositoryError::Connection(msg)) if attempt < policy.max_attempts => {
                let actual_delay = if policy.jitter {
                    jittered(base_delay)
                } else {
                    base_delay
                };

                warn!(
                    operation = op,
                    attempt,
                    max_attempts = policy.max_attempts,
                    base_delay_ms = base_delay.as_millis() as u64,
                    actual_delay_ms = actual_delay.as_millis() as u64,
                    error = %msg,
                    "db connection error, retrying"
                );

                tokio::time::sleep(actual_delay).await;
                base_delay = next_base_delay(base_delay, policy.multiplier, policy.max_delay);
            }
            Err(e) => return Err(e),
        }
    }

    // Достижимо только если `1..=max_attempts` пуст, т.е.
    // `max_attempts = 0`. Клампится в `normalized()` до `1`, поэтому
    // эта ветка не исполняется. Оставлена для типовой корректности —
    // компилятор требует вернуть `Result` из всех путей.
    Err(RepositoryError::Connection(
        "retry: no attempts made (max_attempts clamped to >= 1)".into(),
    ))
}

/// Full jitter: `random(0..=base)`.
///
/// `Duration::try_from_secs_f64` возвращает `Err` на NaN/inf/negative —
/// мы подстраховываемся fallback'ом на `base`. Значения `base` в
/// нормальной работе всегда конечные, но defensive coding дешевле
/// паники в hot path.
#[inline]
fn jittered(base: Duration) -> Duration {
    if base.is_zero() {
        return Duration::ZERO;
    }
    let factor = jitter_factor();
    let secs = base.as_secs_f64() * factor;
    Duration::try_from_secs_f64(secs).unwrap_or(base)
}

/// Дробная часть секунды системных часов в `[0, 1)`.
#[inline]
fn jitter_factor() -> f64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    f64::from(nanos) / 1_000_000_000.0
}

/// Следующий base delay с клампингом к `max_delay`. Защищает от
/// overflow'а через `min(max_delay_secs)`.
#[inline]
fn next_base_delay(current: Duration, multiplier: f64, max: Duration) -> Duration {
    let next_secs = (current.as_secs_f64() * multiplier).min(max.as_secs_f64());
    Duration::try_from_secs_f64(next_secs).unwrap_or(max)
}

// ============================================================
// Tests
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    // ============================================================
    // Нормализация полей
    // ============================================================

    #[test]
    fn normalized_clamps_zero_attempts_to_one() {
        let p = RetryPolicy {
            max_attempts: 0,
            ..RetryPolicy::standard()
        }
        .normalized();
        assert_eq!(p.max_attempts, 1);
    }

    #[test]
    fn normalized_clamps_initial_delay_above_max() {
        let p = RetryPolicy {
            initial_delay: Duration::from_secs(60),
            max_delay: Duration::from_secs(10),
            ..RetryPolicy::standard()
        }
        .normalized();
        assert_eq!(p.initial_delay, Duration::from_secs(10));
    }

    #[test]
    fn normalized_clamps_multiplier_below_one() {
        let p = RetryPolicy {
            multiplier: 0.5,
            ..RetryPolicy::standard()
        }
        .normalized();
        assert_eq!(p.multiplier, 1.0);
    }

    #[test]
    fn normalized_preserves_valid_values() {
        let original = RetryPolicy::standard();
        let normalized = original.normalized();
        assert_eq!(normalized.max_attempts, original.max_attempts);
        assert_eq!(normalized.initial_delay, original.initial_delay);
        assert_eq!(normalized.max_delay, original.max_delay);
        assert_eq!(normalized.multiplier, original.multiplier);
        assert_eq!(normalized.jitter, original.jitter);
    }

    // ============================================================
    // next_base_delay
    // ============================================================

    #[test]
    fn next_base_delay_doubles() {
        let d = next_base_delay(Duration::from_secs(1), 2.0, Duration::from_secs(100));
        assert_eq!(d, Duration::from_secs(2));
    }

    #[test]
    fn next_base_delay_caps_at_max() {
        let d = next_base_delay(Duration::from_secs(10), 2.0, Duration::from_secs(16));
        assert_eq!(d, Duration::from_secs(16));
    }

    #[test]
    fn next_base_delay_does_not_overflow() {
        // Огромный current не должен паниковать.
        let d = next_base_delay(
            Duration::from_secs(u64::MAX / 4),
            2.0,
            Duration::from_secs(30),
        );
        assert_eq!(d, Duration::from_secs(30));
    }

    // ============================================================
    // jitter
    // ============================================================

    #[test]
    fn jittered_zero_returns_zero() {
        assert_eq!(jittered(Duration::ZERO), Duration::ZERO);
    }

    #[test]
    fn jittered_within_range() {
        let base = Duration::from_millis(1000);
        for _ in 0..100 {
            let j = jittered(base);
            assert!(j <= base, "jittered {j:?} must be <= base {base:?}");
        }
    }

    #[test]
    fn jittered_produces_varied_values() {
        // 100 итераций должны дать много разных значений.
        // Ловит случай, когда jitter случайно реализован как константа.
        let base = Duration::from_millis(1_000_000);
        let mut seen = std::collections::HashSet::new();
        for _ in 0..100 {
            seen.insert(jittered(base).as_nanos());
        }
        assert!(
            seen.len() > 50,
            "jitter produced only {} distinct values in 100 iterations",
            seen.len()
        );
    }

    // ============================================================
    // with_retry — интеграция
    // ============================================================

    #[tokio::test(start_paused = true)]
    async fn succeeds_on_first_attempt() {
        let counter = Arc::new(AtomicU32::new(0));
        let c = Arc::clone(&counter);

        let result = with_retry("test", RetryPolicy::quick(), || {
            let c = Arc::clone(&c);
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Ok::<_, RepositoryError>(42)
            }
        })
        .await;

        assert_eq!(result.unwrap(), 42);
        assert_eq!(counter.load(Ordering::SeqCst), 1, "ровно одна попытка");
    }

    #[tokio::test(start_paused = true)]
    async fn succeeds_after_retries() {
        let counter = Arc::new(AtomicU32::new(0));
        let c = Arc::clone(&counter);

        let result = with_retry("test", RetryPolicy::quick(), || {
            let c = Arc::clone(&c);
            async move {
                let attempt = c.fetch_add(1, Ordering::SeqCst) + 1;
                if attempt < 3 {
                    Err(RepositoryError::Connection(format!("fail {attempt}")))
                } else {
                    Ok(attempt)
                }
            }
        })
        .await;

        assert_eq!(result.unwrap(), 3);
        assert_eq!(counter.load(Ordering::SeqCst), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn exhausts_attempts_returns_last_connection_error() {
        let counter = Arc::new(AtomicU32::new(0));
        let c = Arc::clone(&counter);

        let result = with_retry("test", RetryPolicy::quick(), || {
            let c = Arc::clone(&c);
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Err::<u32, _>(RepositoryError::Connection("always fail".into()))
            }
        })
        .await;

        let err = result.unwrap_err();
        assert!(matches!(err, RepositoryError::Connection(_)));
        assert_eq!(counter.load(Ordering::SeqCst), 3, "3 попытки по quick()");
    }

    #[tokio::test(start_paused = true)]
    async fn does_not_retry_transaction_error() {
        let counter = Arc::new(AtomicU32::new(0));
        let c = Arc::clone(&counter);

        let result = with_retry("test", RetryPolicy::standard(), || {
            let c = Arc::clone(&c);
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Err::<u32, _>(RepositoryError::Transaction("constraint".into()))
            }
        })
        .await;

        assert!(matches!(result, Err(RepositoryError::Transaction(_))));
        assert_eq!(
            counter.load(Ordering::SeqCst),
            1,
            "Transaction не ретраится — только одна попытка"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn zero_attempts_clamps_to_one() {
        let counter = Arc::new(AtomicU32::new(0));
        let c = Arc::clone(&counter);

        let policy = RetryPolicy {
            max_attempts: 0,
            jitter: false,
            ..RetryPolicy::quick()
        };

        let result = with_retry("test", policy, || {
            let c = Arc::clone(&c);
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Err::<u32, _>(RepositoryError::Connection("fail".into()))
            }
        })
        .await;

        assert!(result.is_err());
        assert_eq!(counter.load(Ordering::SeqCst), 1, "клампнуто до 1");
    }

    /// С `start_paused = true` `tokio::time::sleep` продвигает
    /// виртуальное время мгновенно. Проверяем точную формулу
    /// экспоненты без jitter'а: 1 + 2 + 4 = 7 с.
    #[tokio::test(start_paused = true)]
    async fn without_jitter_delay_is_exponential() {
        let policy = RetryPolicy {
            max_attempts: 4,
            initial_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(60),
            multiplier: 2.0,
            jitter: false,
        };

        let start = tokio::time::Instant::now();

        let result = with_retry("test", policy, || async {
            Err::<u32, _>(RepositoryError::Connection("fail".into()))
        })
        .await;

        assert!(result.is_err());
        // 4 попытки → 3 sleep'а: base = 1, 2, 4 → сумма 7 с.
        assert_eq!(start.elapsed(), Duration::from_secs(7));
    }

    /// С jitter'ом фактическая задержка всегда ≤ base, значит
    /// суммарная ≤ суммы base'ов (без jitter).
    #[tokio::test(start_paused = true)]
    async fn with_jitter_delay_does_not_exceed_no_jitter() {
        let policy = RetryPolicy {
            max_attempts: 4,
            initial_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(60),
            multiplier: 2.0,
            jitter: true,
        };

        let start = tokio::time::Instant::now();
        let _ = with_retry("test", policy, || async {
            Err::<u32, _>(RepositoryError::Connection("fail".into()))
        })
        .await;

        assert!(
            start.elapsed() <= Duration::from_secs(7),
            "with jitter total {:?} must be <= 7s",
            start.elapsed()
        );
    }
}
