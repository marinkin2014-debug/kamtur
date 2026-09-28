//! Health-проверки API.
//!
//! ## Liveness vs Readiness
//!
//! - `/health` — **liveness**: процесс жив, event loop крутится, роутер
//!   отвечает. Не трогает зависимости. Если liveness-проба падает —
//!   оркестратор убивает pod/контейнер и перезапускает.
//!
//! - `/ready` — **readiness**: сервис готов принимать трафик. Проверяет
//!   доступность БД (`SELECT 1` с таймаутом). Если readiness-проба падает —
//!   оркестратор выводит инстанс из балансировки, но **не убивает** его:
//!   БД может восстановиться, инстанс сам вернётся в строй.
//!
//! Разделение критично. Одна общая `/health`, которая пингует БД,
//! приводит к **каскадному рестарту** всех API при кратковременной
//! недоступности Postgres. Это антипаттерн; см. Google SRE Book,
//! глава про distributed health checks.

use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde_json::{json, Value};

use crate::state::AppState;

/// Таймаут ping БД для `/ready`.
///
/// Значение выбрано так, чтобы:
///
/// - быть короче таймаута HTTP-клиента балансировщика (обычно 5-10 с);
/// - быть длиннее нормального `SELECT 1` (десятки мс даже под нагрузкой);
/// - не флудить ложными 503 при кратковременных GC-паузах в Postgres
///   или сетевых jitter'ах.
const READY_DB_TIMEOUT: Duration = Duration::from_secs(2);

/// Liveness probe. Всегда 200, если процесс отвечает.
pub async fn liveness() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

/// Readiness probe. 200 при доступной БД, 503 — иначе.
///
/// Логируем на `debug`, а не `warn`: балансировщик опрашивает этот
/// эндпоинт каждые 5-10 секунд, и `warn` в логах будет флудить при
/// длительной недоступности БД. Видимость обеспечивается через метрики
/// (`http_requests_total{path="/ready",status="503"}`).
pub async fn readiness(State(state): State<AppState>) -> impl IntoResponse {
    match tokio::time::timeout(READY_DB_TIMEOUT, state.db_health.ping()).await {
        Ok(Ok(())) => (StatusCode::OK, Json(json!({ "status": "ok" }))).into_response(),
        Ok(Err(e)) => {
            tracing::debug!(error = %e, "readiness: db ping failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "status": "unavailable", "reason": "database" })),
            )
                .into_response()
        }
        Err(_elapsed) => {
            tracing::debug!("readiness: db ping timed out");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "status": "unavailable", "reason": "timeout" })),
            )
                .into_response()
        }
    }
}
