//! HTTP-сервер метрик Prometheus на отдельном порту.
//! Не требует авторизации — предполагается, что порт 9090 доступен только внутри сети.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context;
use axum::extract::State;
use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use metrics_exporter_prometheus::PrometheusHandle;
use tokio_util::sync::CancellationToken;
use tracing::info;

#[derive(Clone)]
struct MetricsState {
    handle: PrometheusHandle,
}

pub async fn run_metrics_server(
    bind: SocketAddr,
    handle: PrometheusHandle,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    let state = Arc::new(MetricsState { handle });

    let app = Router::new()
        .route("/metrics", get(metrics_handler))
        .route("/health", get(health_handler))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("bind metrics server {bind}"))?;

    info!(addr = %bind, "metrics server started");

    axum::serve(listener, app)
        .with_graceful_shutdown(async move { cancel.cancelled().await })
        .await
        .context("serve metrics")?;

    info!("metrics server stopped");
    Ok(())
}

async fn metrics_handler(State(state): State<Arc<MetricsState>>) -> impl IntoResponse {
    let body = state.handle.render();
    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        body,
    )
}

async fn health_handler() -> &'static str {
    "ok"
}
