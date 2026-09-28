use std::time::Duration;

use async_trait::async_trait;
use lettre::transport::smtp::Error as SmtpError;
use lettre::{AsyncSmtpTransport, Tokio1Executor};
use sqlx::PgPool;
use tokio::time::timeout;

use domain::errors::NotifyError;
use domain::ports::{HealthCheck, HealthStatus};

use crate::notifier::build_smtp_transport;

// ============================================================
// DatabaseHealthCheck
// ============================================================

pub struct DatabaseHealthCheck {
    pool: PgPool,
}

impl DatabaseHealthCheck {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl HealthCheck for DatabaseHealthCheck {
    fn component(&self) -> &str {
        "database"
    }

    async fn check(&self) -> HealthStatus {
        let start = std::time::Instant::now();
        match sqlx::query_scalar::<_, i32>("SELECT 1")
            .fetch_one(&self.pool)
            .await
        {
            Ok(_) => {
                let latency = start.elapsed().as_millis() as i32;
                HealthStatus {
                    status: if latency > 3000 { "degraded" } else { "ok" }.into(),
                    latency_ms: Some(latency),
                    details: serde_json::json!({}),
                }
            }
            Err(e) => HealthStatus {
                status: "down".into(),
                latency_ms: None,
                details: serde_json::json!({ "error": e.to_string() }),
            },
        }
    }
}

// ============================================================
// SyncPipelineFreshnessCheck
// ============================================================

/// Проверяет, что пайплайн синхронизации жив: последний цикл завершился
/// `success` или `skipped` не позднее `max_age` назад.
///
/// `skipped` — штатный исход: провайдер отдал байт-в-байт тот же контент,
/// дедуп отработал, БД приняла решение. Настоящие поломки (сеть, парсинг,
/// БД) дают `failed` или застревают в `running`.
///
/// Использует `finished_at`, а не `started_at`: если sync стартовал 3 часа
/// назад и завис (`finished_at IS NULL`), `MAX(finished_at)` его не увидит —
/// что и нужно.
pub struct SyncPipelineFreshnessCheck {
    pool: PgPool,
    provider_id: String,
    max_age: Duration,
}

impl SyncPipelineFreshnessCheck {
    pub fn new(pool: PgPool, provider_id: impl Into<String>, max_age: Duration) -> Self {
        Self {
            pool,
            provider_id: provider_id.into(),
            max_age,
        }
    }
}

#[async_trait]
impl HealthCheck for SyncPipelineFreshnessCheck {
    fn component(&self) -> &str {
        "sync:pipeline_freshness"
    }

    async fn check(&self) -> HealthStatus {
        let row: Result<Option<chrono::DateTime<chrono::Utc>>, _> = sqlx::query_scalar(
            "SELECT MAX(finished_at)
             FROM sync_runs
             WHERE cruise_provider_id = $1
               AND status IN ('success', 'skipped')",
        )
        .bind(&self.provider_id)
        .fetch_one(&self.pool)
        .await;

        match row {
            Ok(Some(last)) => {
                let age_secs = chrono::Utc::now()
                    .signed_duration_since(last)
                    .num_seconds()
                    .max(0) as u64;

                let status = if age_secs > self.max_age.as_secs() * 2 {
                    "down"
                } else if age_secs > self.max_age.as_secs() {
                    "degraded"
                } else {
                    "ok"
                };

                HealthStatus {
                    status: status.into(),
                    latency_ms: None,
                    details: serde_json::json!({
                        "last_pipeline_at": last.to_rfc3339(),
                        "age_secs": age_secs,
                        "max_age_secs": self.max_age.as_secs(),
                    }),
                }
            }
            Ok(None) => HealthStatus {
                status: "degraded".into(),
                latency_ms: None,
                details: serde_json::json!({ "reason": "no completed sync yet" }),
            },
            Err(e) => HealthStatus {
                status: "down".into(),
                latency_ms: None,
                details: serde_json::json!({ "error": e.to_string() }),
            },
        }
    }
}

// ============================================================
// ContentFreshnessCheck
// ============================================================

/// Отдельная проверка: когда провайдер последний раз присылал **новые**
/// данные. Долгий `ok` здесь — не алерт: если контент не меняется, это норма.
///
/// `degraded` (а не `down`) — сигнал к разбору, не авария.
pub struct ContentFreshnessCheck {
    pool: PgPool,
    provider_id: String,
    max_age: Duration,
}

impl ContentFreshnessCheck {
    pub fn new(pool: PgPool, provider_id: impl Into<String>, max_age: Duration) -> Self {
        Self {
            pool,
            provider_id: provider_id.into(),
            max_age,
        }
    }
}

#[async_trait]
impl HealthCheck for ContentFreshnessCheck {
    fn component(&self) -> &str {
        "sync:content_freshness"
    }

    async fn check(&self) -> HealthStatus {
        let row: Result<Option<chrono::DateTime<chrono::Utc>>, _> = sqlx::query_scalar(
            "SELECT MAX(finished_at)
             FROM sync_runs
             WHERE cruise_provider_id = $1 AND status = 'success'",
        )
        .bind(&self.provider_id)
        .fetch_one(&self.pool)
        .await;

        match row {
            Ok(Some(last)) => {
                let age_secs = chrono::Utc::now()
                    .signed_duration_since(last)
                    .num_seconds()
                    .max(0) as u64;

                // Мягче, чем pipeline: только degraded, без down.
                let status = if age_secs > self.max_age.as_secs() * 2 {
                    "degraded"
                } else {
                    "ok"
                };

                HealthStatus {
                    status: status.into(),
                    latency_ms: None,
                    details: serde_json::json!({
                        "last_content_change_at": last.to_rfc3339(),
                        "age_secs": age_secs,
                        "max_age_secs": self.max_age.as_secs(),
                    }),
                }
            }
            Ok(None) => HealthStatus {
                status: "degraded".into(),
                latency_ms: None,
                details: serde_json::json!({ "reason": "no content change yet" }),
            },
            Err(e) => HealthStatus {
                status: "down".into(),
                latency_ms: None,
                details: serde_json::json!({ "error": e.to_string() }),
            },
        }
    }
}

// ============================================================
// SmtpHealthCheck
// ============================================================

/// Проверка доступности SMTP-сервера **по протоколу**, а не по TCP.
///
/// ## Что проверяется
///
/// `AsyncSmtpTransport::test_connection()` (из `lettre::AsyncTransport`)
/// проходит полный SMTP-цикл:
///
/// 1. TCP connect.
/// 2. TLS handshake (для порта 465 — implicit TLS).
/// 3. Получение приветствия сервера (код 220).
/// 4. `EHLO <hostname>` — сервер отвечает 250 с capabilities.
/// 5. `STARTTLS` + повторный TLS handshake (для 587/25).
/// 6. Второй EHLO.
/// 7. `AUTH LOGIN` с реальными credentials — сервер отвечает 235.
/// 8. `QUIT`.
///
/// ## Что это ловит
///
/// - **TCP accept без SMTP** — DPI, rate-limit, firewall. TCP-connect
///   раньше говорил «ok», теперь — «down».
/// - **Невалидный TLS-сертификат** — раньше TCP-connect молчал, теперь
///   `test_connection` возвращает ошибку handshake.
/// - **Протухший пароль** — раньше health-check зелёный, письма не
///   уходят с `535 Auth failed`. Теперь сервер отдаст `535`, `check`
///   вернёт `down`.
/// - **Блокировка EHLO сервером** — редко, но бывает в shared hosting.
///
/// ## Стоимость
///
/// Полный цикл = ~200-500 мс на удалённый SMTP через TLS. Это заметно
/// дороже TCP-connect (~10 мс), но health-loop работает раз в 60 секунд,
/// так что нагрузка ничтожная. Таймаут 5 секунд — компромисс между
/// «успеть за разумное время» и «не фолсить 504 на медленных сетях».
///
/// ## Credentials
///
/// Health-check аутентифицируется **теми же** credentials, что
/// `SmtpNotifier`. Если пароль неверный — это ошибка конфигурации,
/// которую хочется видеть сразу, а не при первой отправке алерта.
pub struct SmtpHealthCheck {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    host: String,
    port: u16,
    timeout: Duration,
}

impl SmtpHealthCheck {
    /// Создаёт health-check. Возвращает `Err(NotifyError::Smtp)` при
    /// невозможности построить TLS-контекст — это ошибка конфигурации,
    /// падаем на bootstrap.
    pub fn new(host: &str, port: u16, user: &str, password: &str) -> Result<Self, NotifyError> {
        let transport = build_smtp_transport(host, port, user, password)?;
        Ok(Self {
            transport,
            host: host.to_string(),
            port,
            timeout: Duration::from_secs(5),
        })
    }
}

#[async_trait]
impl HealthCheck for SmtpHealthCheck {
    fn component(&self) -> &str {
        "smtp"
    }

    async fn check(&self) -> HealthStatus {
        let start = std::time::Instant::now();
        let addr = format!("{}:{}", self.host, self.port);

        // `test_connection` возвращает:
        //   Ok(true)  — полный handshake прошёл, включая AUTH.
        //   Ok(false) — сервер отклонил EHLO или предшествующую команду.
        //   Err(e)    — TCP/TLS/SMTP ошибка с диагностикой.
        let result = timeout(self.timeout, self.transport.test_connection()).await;

        match result {
            Ok(Ok(true)) => HealthStatus {
                status: "ok".into(),
                latency_ms: Some(start.elapsed().as_millis() as i32),
                details: serde_json::json!({ "addr": addr }),
            },
            Ok(Ok(false)) => HealthStatus {
                status: "down".into(),
                latency_ms: None,
                details: serde_json::json!({
                    "addr": addr,
                    "error": "server rejected SMTP handshake (EHLO/STARTTLS/AUTH)",
                }),
            },
            Ok(Err(e)) => HealthStatus {
                status: "down".into(),
                latency_ms: None,
                details: serde_json::json!({
                    "addr": addr,
                    "error": format_smtp_error(&e),
                }),
            },
            Err(_) => HealthStatus {
                status: "down".into(),
                latency_ms: None,
                details: serde_json::json!({
                    "addr": addr,
                    "error": format!("timeout after {}s", self.timeout.as_secs()),
                }),
            },
        }
    }
}

/// Обёртка для `Display` у `lettre::transport::smtp::Error`. Явно
/// ограничиваем тип ошибки, чтобы `check` не зависел от точной сигнатуры
/// `test_connection` при обновлении lettre — если тип изменится,
/// увидим ошибку компиляции в одном месте, а не по всему хендлеру.
#[inline]
fn format_smtp_error(e: &SmtpError) -> String {
    e.to_string()
}
