use std::iter::once;
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use axum::http::header;
use axum::middleware;
use axum::routing::get;
use axum::Router;
use axum_prometheus::PrometheusMetricLayer;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use tokio::signal;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::sensitive_headers::SetSensitiveRequestHeadersLayer;
use tower_http::trace::{DefaultOnFailure, DefaultOnResponse, TraceLayer};
use tracing::{info, Level};

use application::use_cases::get_cruise::GetCruiseUseCase;
use application::use_cases::list_cruises::ListCruisesUseCase;
use infrastructure::repositories::postgres::PostgresCruiseReadRepository;

use crate::config::Config;
use crate::middleware::auth::require_token;
use crate::middleware::cache::cache_headers;
use crate::middleware::rate_limit::{rate_limit_mw, RateLimiter, TrustedProxies};
use crate::middleware::timeout::{timeout_mw, TimeoutState};
use crate::routes;
use crate::state::{AppState, DbHealth};

pub async fn run(config: Config) -> anyhow::Result<()> {
    let read_url = config.read_url().to_string();
    let using_replica = config.database_url_replica.is_some();

    info!(
        using_replica,
        url = %mask_url(&read_url),
        statement_timeout_secs = config.read_statement_timeout_secs,
        request_timeout_secs = config.request_timeout_secs,
        "connecting to database for read"
    );

    // Парсим URL и выставляем statement_timeout через параметры сессии.
    let connect_opts = PgConnectOptions::from_str(&read_url)
        .with_context(|| format!("parse DATABASE_URL: {}", mask_url(&read_url)))?
        .options([(
            "statement_timeout",
            format!("{}", config.read_statement_timeout_secs * 1000),
        )]);

    let pool = PgPoolOptions::new()
        .max_connections(50)
        .min_connections(5)
        .acquire_timeout(Duration::from_secs(5))
        .idle_timeout(Duration::from_secs(600))
        .connect_with(connect_opts)
        .await
        .context("connect postgres for read")?;

    let repo = Arc::new(PostgresCruiseReadRepository::new(pool.clone()));

    let db_health: Arc<dyn DbHealth> = Arc::new(pool);

    let trusted_proxies = TrustedProxies::parse(&config.trusted_proxies)
        .map_err(|e| anyhow::anyhow!("TRUSTED_PROXIES: {e}"))?;

    if trusted_proxies.is_empty() {
        info!("TRUSTED_PROXIES not set — X-Forwarded-For ignored, using peer IP");
    } else {
        info!(spec = %config.trusted_proxies, "trusted proxies configured");
    }

    let rate_limiter = Arc::new(RateLimiter::new(
        config.rate_limit_capacity,
        config.rate_limit_refill_per_sec,
        trusted_proxies,
    ));
    let rate_limiter_sweeper = rate_limiter.clone().spawn_sweeper();

    let state = AppState::new(
        Arc::new(ListCruisesUseCase::new(repo.clone())),
        Arc::new(GetCruiseUseCase::new(repo)),
        &config.api_token,
        rate_limiter,
        config.default_provider_id.clone(),
        db_health,
    );

    let (prometheus_layer, metric_handle) = PrometheusMetricLayer::pair();

    let metrics_route = Router::new().route(
        "/metrics",
        get({
            let handle = metric_handle.clone();
            move || {
                let handle = handle.clone();
                async move { handle.render() }
            }
        }),
    );

    let timeout_state = TimeoutState::new(config.request_timeout());

    // Слои применяются в порядке снизу вверх: последний .layer()
    // выполняется первым на входящем запросе.
    //
    // ## Порядок timeout_mw и prometheus_layer
    //
    // `timeout_mw` стоит **внутри** `prometheus_layer` (в коде — раньше,
    // значит оборачивается prometheus'ом снаружи). Это критично:
    //
    // - prometheus_layer начинает отсчёт → timeout_mw запускает таймер →
    //   handler. Если таймер сработал, timeout_mw возвращает 504 →
    //   prometheus_layer фиксирует 504 в `http_requests_total`.
    //
    // - Если поменять местами (timeout снаружи prometheus), при
    //   срабатывании таймера prometheus-future отменяется через drop,
    //   метрика не записывается, 504 не видно на дашборде.
    //
    // ## Порядок timeout_mw и rate_limit_mw
    //
    // `rate_limit_mw` **снаружи** timeout: in-memory token bucket
    // выполняется за наносекунды, таймаутить его бессмысленно. Кроме
    // того, rate-limit должен срабатывать до того, как запрос займёт
    // слот в timeout'е — иначе flood битых запросов будет ждать таймаут
    // вместо мгновенного 429.
    // Rate limit условный: `RATE_LIMIT_ENABLED=false` отключает middleware
    // целиком (для load-тестов). Порядок слоёв вокруг остаётся прежним:
    // rate_limit — снаружи timeout, внутри cache (см. docstring выше).
    let app = Router::new()
        .merge(routes::health_router())
        .merge(metrics_route)
        .merge(
            routes::cruises::router()
                .layer(middleware::from_fn_with_state(state.clone(), require_token)),
        )
        .layer(middleware::from_fn_with_state(timeout_state, timeout_mw))
        .layer(prometheus_layer);

    let app = if config.rate_limit_enabled {
        app.layer(middleware::from_fn_with_state(state.clone(), rate_limit_mw))
    } else {
        info!("rate limit middleware disabled (RATE_LIMIT_ENABLED=false)");
        app
    };

    let app = app
        .layer(middleware::from_fn(cache_headers))
        // --- Observability (в порядке добавления — от внутреннего к внешнему) ---
        .layer(SetSensitiveRequestHeadersLayer::new(once(
            header::AUTHORIZATION,
        )))
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(|req: &axum::http::Request<_>| {
                    let request_id = req
                        .headers()
                        .get("x-request-id")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("-");

                    tracing::info_span!(
                        "http_request",
                        method = %req.method(),
                        uri = %req.uri().path(),
                        version = ?req.version(),
                        request_id = %request_id,
                    )
                })
                .on_response(DefaultOnResponse::new().level(Level::INFO))
                .on_failure(DefaultOnFailure::new().level(Level::ERROR)),
        )
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&config.bind_addr)
        .await
        .with_context(|| format!("bind {}", config.bind_addr))?;

    info!(addr = %config.bind_addr, "api started");

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        let _ = signal::ctrl_c().await;
        info!("shutdown signal received");
    })
    .await
    .context("serve")?;

    rate_limiter_sweeper.abort();

    info!("api shutdown complete");
    Ok(())
}

/// Скрывает пароль в логах.
fn mask_url(url: &str) -> String {
    if let Some(at) = url.find('@') {
        if let Some(scheme_end) = url.find("://") {
            let scheme = &url[..scheme_end + 3];
            return format!("{}***{}", scheme, &url[at..]);
        }
    }
    url.to_string()
}
