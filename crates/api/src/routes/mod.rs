pub mod cruises;
pub mod health;

use axum::routing::get;
use axum::Router;

use crate::state::AppState;

/// Публичный роутер health-проверок. Без авторизации —
/// балансировщик не передаёт `Authorization`.
pub fn health_router() -> Router<AppState> {
    Router::new()
        .route("/health", get(health::liveness))
        .route("/ready", get(health::readiness))
}
