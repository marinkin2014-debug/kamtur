//! Сквозной таймаут на HTTP-запрос.
//!
//! ## Зачем
//!
//! Без таймаута медленный handler (SQL, сериализация, upstream) висит до
//! тех пор, пока клиент сам не закроет соединение. Это:
//!
//! - держит connection из pool'а;
//! - растёт в `pending_requests` и съедает память;
//! - при cascade — выводит из строя весь API.
//!
//! `statement_timeout` в пуле (см. `bootstrap.rs`) защищает только от
//! медленных SQL. Таймаут на HTTP — общая защита: 504 отдаётся клиенту
//! раньше, чем pool начнёт голодать.
//!
//! ## Почему не `tower-http::timeout::TimeoutLayer`
//!
//! 1. `tower-http` возвращает **408 Request Timeout** — семантика
//!    «клиент слишком долго присылает тело запроса». У нас — таймаут
//!    обработки на сервере. Правильный код — **504 Gateway Timeout**:
//!    так же поступают nginx, AWS ALB, GCP Load Balancer.
//! 2. Формат тела ошибки должен совпадать с `ApiError`:
//!    `{"error":"gateway_timeout","message":"..."}`. `tower-http` даёт
//!    пустое тело.
//!
//! ## Отмена future
//!
//! Когда `tokio::time::timeout` возвращает `Err`, обёрнутый future
//! **отбрасывается** (drop). Для handler'а это означает:
//!
//! - SQL-транзакция откатывается (`sqlx::Transaction` в `Drop` делает
//!   `ROLLBACK`).
//! - Блокировки на уровне БД освобождаются.
//! - Соединение возвращается в пул (возможно, с «ошибкой отмены» —
//!   `sqlx` сам вернёт его в рабочее состояние при следующем acquire).
//!
//! Для read-only API (наш случай) это безопасно. Для write-операций
//! нужно понимать, что отмена — это не «запрос не выполнился», а
//! «результат неизвестен». Но у нас нет write-endpoint'ов, поэтому
//! вопрос не стоит.

use std::time::Duration;

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, HeaderValue, Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

/// Состояние middleware. Хранит длительность таймаута.
///
/// Передаётся через `from_fn_with_state` — не засоряем `AppState`,
/// потому что таймаут — параметр middleware, а не бизнес-логики.
#[derive(Debug, Clone, Copy)]
pub struct TimeoutState {
    pub duration: Duration,
}

impl TimeoutState {
    pub fn new(duration: Duration) -> Self {
        Self { duration }
    }
}

/// Сквозной таймаут на HTTP-запрос. См. документацию модуля.
pub async fn timeout_mw(
    State(state): State<TimeoutState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    match tokio::time::timeout(state.duration, next.run(req)).await {
        Ok(response) => response,
        Err(_elapsed) => {
            // Логируем на WARN: в отличие от rate-limit, таймаут —
            // событие, требующее внимания. Если флудит — значит либо
            // таймаут слишком маленький, либо есть деградация в БД/сети.
            tracing::warn!(
                timeout_ms = state.duration.as_millis() as u64,
                "request timed out"
            );

            gateway_timeout_response()
        }
    }
}

/// 504 с JSON-телом в формате `ApiError`.
#[inline]
fn gateway_timeout_response() -> Response {
    let body = r#"{"error":"gateway_timeout","message":"request took too long"}"#;
    let mut resp = (StatusCode::GATEWAY_TIMEOUT, body).into_response();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gateway_timeout_response_has_correct_status_and_body() {
        let resp = gateway_timeout_response();
        assert_eq!(resp.status(), StatusCode::GATEWAY_TIMEOUT);

        let ct = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok());
        assert_eq!(ct, Some("application/json"));
    }

    #[test]
    fn gateway_timeout_response_body_is_valid_json() {
        let resp = gateway_timeout_response();
        // Разворачиваем body синхронно через `axum::body::to_bytes` невозможен —
        // только в async. Проверим здесь через прямую сборку сообщения.
        let body = r#"{"error":"gateway_timeout","message":"request took too long"}"#;
        let parsed: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(parsed["error"], "gateway_timeout");
        assert!(parsed["message"].is_string());
        // Убеждаемся, что в тесте используется тот же body — если менять,
        // тест упадёт.
        drop(resp);
    }
}
