use std::sync::Arc;

use application::use_cases::get_cruise::GetCruiseUseCase;
use application::use_cases::list_cruises::ListCruisesUseCase;
use async_trait::async_trait;
use sha2::{Digest, Sha256};
use sqlx::PgPool;

use crate::middleware::rate_limit::RateLimiter;

/// Проба доступности БД для `/ready`.
///
/// Абстракция над пулом: в проде — обёртка над `PgPool`, в тестах —
/// mock. Зачем не давать `PgPool` в `AppState` напрямую:
///
/// - Не тащим `sqlx`-тип в публичный API state'а. `PgPool` — это
///   деталь реализации БД-слоя, а не контракт HTTP-слоя.
/// - Хендлеры не получают случайный доступ к пулу минуя репозитории.
///   Все запросы идут через use case'ы; `/ready` — исключение, для него
///   и есть отдельный трейт.
/// - Возможно заменить источник (read replica, health-check через
///   отдельный endpoint, mock в тестах) без правок `AppState`.
#[async_trait]
pub trait DbHealth: Send + Sync {
    /// Ping БД. Возвращает `Err`, если БД недоступна.
    async fn ping(&self) -> Result<(), String>;
}

#[async_trait]
impl DbHealth for PgPool {
    async fn ping(&self) -> Result<(), String> {
        // `SELECT 1` — минимальный round-trip:
        //
        // - Нет чтения таблиц, нет MVCC-снапшота.
        // - Не берёт тяжёлых блокировок.
        // - Проверяет именно соединение: если connection в пуле сдох,
        //   `fetch_one` вернёт ошибку и заменит connection.
        sqlx::query_scalar::<_, i32>("SELECT 1")
            .fetch_one(self)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

#[derive(Clone)]
pub struct AppState {
    pub list_cruises: Arc<ListCruisesUseCase>,
    pub get_cruise: Arc<GetCruiseUseCase>,

    /// SHA256 ожидаемого токена. Plaintext в state не хранится.
    pub api_token_hash: Arc<[u8; 32]>,

    pub rate_limiter: Arc<RateLimiter>,

    /// `cruise_provider_id` по умолчанию для `GET /cruises*`, если
    /// query-параметр не задан.
    pub default_provider_id: Arc<str>,

    /// Проба доступности БД для `/ready`.
    pub db_health: Arc<dyn DbHealth>,
}

impl AppState {
    pub fn new(
        list_cruises: Arc<ListCruisesUseCase>,
        get_cruise: Arc<GetCruiseUseCase>,
        api_token: &str,
        rate_limiter: Arc<RateLimiter>,
        default_provider_id: impl Into<String>,
        db_health: Arc<dyn DbHealth>,
    ) -> Self {
        let mut h = Sha256::new();
        h.update(api_token.as_bytes());
        let api_token_hash: [u8; 32] = h.finalize().into();

        Self {
            list_cruises,
            get_cruise,
            api_token_hash: Arc::new(api_token_hash),
            rate_limiter,
            default_provider_id: Arc::from(default_provider_id.into()),
            db_health,
        }
    }
}
