use std::time::Duration;

use anyhow::Context;

pub struct Config {
    pub database_url: String,

    /// Опциональный read replica для чтения. Если не задан — используется
    /// `database_url`.
    pub database_url_replica: Option<String>,

    pub bind_addr: String,
    pub api_token: String,

    /// Список доверенных CIDR через запятую: `10.0.0.0/8,127.0.0.1/32`.
    /// Пусто — XFF игнорируется, используется IP пира.
    pub trusted_proxies: String,

    /// `cruise_provider_id` по умолчанию для `GET /cruises` и
    /// `GET /cruises/:id`.
    pub default_provider_id: String,

    /// `statement_timeout` для read-пула (в секундах).
    pub read_statement_timeout_secs: u64,

    /// Сквозной таймаут HTTP-запроса (в секундах).
    ///
    /// Должен быть:
    ///
    /// - **больше** `read_statement_timeout_secs` — иначе 504 отдаст
    ///   HTTP-слой раньше, чем БД применит свой лимит, и мы потеряем
    ///   диагностически ценный `SQLSTATE 57014` из Postgres;
    /// - **меньше** таймаута балансировщика (nginx по умолчанию 60 сек);
    /// - с запасом на сериализацию ответа и сетевой jitter.
    ///
    /// 30 сек — компромисс: `5s statement_timeout + 25s` на оверхед.
    pub request_timeout_secs: u64,

    /// Включён ли rate limit middleware.
    ///
    /// - `true` (default) — middleware устанавливается, лимиты берутся из
    ///   `rate_limit_capacity` / `rate_limit_refill_per_sec`.
    /// - `false` — middleware не устанавливается. Никаких 429, никакой
    ///   траты токенов. Используется для load-тестов (k6, B.6): мы хотим
    ///   измерить latency самого API и БД, а не алгоритм token bucket.
    ///
    /// В проде **обязательно** `true`: без лимита один клиент может
    /// уложить SQL пул и задушить остальных. Настройка для bench/CI.
    pub rate_limit_enabled: bool,

    /// Ёмкость token bucket на один IP.
    ///
    /// 100 (default) = короткий burst 100 запросов, затем refill.
    /// Для load-теста с одного IP (127.0.0.1) — либо `rate_limit_enabled =
    /// false`, либо capacity >= ожидаемого burst'а.
    pub rate_limit_capacity: u32,

    /// Скорость refill token bucket (токенов/сек на IP).
    ///
    /// 20.0 (default) — устойчиво 20 rps, короткие bursts до capacity.
    /// 100 rps нагрузка с одного IP требует либо `rate_limit_enabled =
    /// false`, либо refill >= 100.
    pub rate_limit_refill_per_sec: f64,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let database_url = std::env::var("DATABASE_URL").context("DATABASE_URL")?;
        let database_url_replica = std::env::var("DATABASE_URL_REPLICA").ok();

        Ok(Self {
            database_url,
            database_url_replica,
            bind_addr: std::env::var("API_BIND").unwrap_or_else(|_| "0.0.0.0:8080".into()),
            api_token: std::env::var("API_TOKEN").context("API_TOKEN")?,
            trusted_proxies: std::env::var("TRUSTED_PROXIES").unwrap_or_default(),
            default_provider_id: std::env::var("DEFAULT_PROVIDER_ID")
                .unwrap_or_else(|_| "1".into()),
            read_statement_timeout_secs: std::env::var("READ_STATEMENT_TIMEOUT_SECS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(5),
            request_timeout_secs: std::env::var("REQUEST_TIMEOUT_SECS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(30),

            // Парсинг bool: `"false"`, `"0"`, `"no"` → false, всё
            // остальное (включая `"true"`, `"1"`, `"yes"`, отсутствие
            // переменной) → true. Отсутствие = дефолт = защита включена.
            rate_limit_enabled: std::env::var("RATE_LIMIT_ENABLED")
                .map(|v| !matches!(v.trim().to_ascii_lowercase().as_str(), "false" | "0" | "no"))
                .unwrap_or(true),

            rate_limit_capacity: std::env::var("RATE_LIMIT_CAPACITY")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(100),

            rate_limit_refill_per_sec: std::env::var("RATE_LIMIT_REFILL_PER_SEC")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(20.0),
        })
    }

    /// URL для read-операций: реплика, если задана; иначе — основная БД.
    pub fn read_url(&self) -> &str {
        self.database_url_replica
            .as_deref()
            .unwrap_or(&self.database_url)
    }

    /// Длительность сквозного таймаута для передачи в middleware.
    pub fn request_timeout(&self) -> Duration {
        Duration::from_secs(self.request_timeout_secs)
    }
}
