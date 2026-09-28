//! Integration-тесты HTTP-обвязки API.
//!
//! Проверяют слои middleware (auth, rate limit, cache, request-id, timeout) и
//! роутинг. НЕ проверяют SQL — для этого есть integration-тесты в `infrastructure`.
//! Здесь используется mock `CruiseReadRepository`, возвращающий фикстуры.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{header, HeaderValue, Request, StatusCode};
use axum::middleware;
use axum::response::IntoResponse;
use axum::Router;
use chrono::NaiveDate;
use rust_decimal::Decimal;
use tower::ServiceExt;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};

use application::use_cases::get_cruise::GetCruiseUseCase;
use application::use_cases::list_cruises::ListCruisesUseCase;
use domain::errors::ReadError;
use domain::ports::CruiseReadRepository;
use domain::views::*;

use api::middleware::auth::require_token;
use api::middleware::cache::cache_headers;
use api::middleware::rate_limit::{rate_limit_mw, RateLimiter, TooManyRequests, TrustedProxies};
use api::middleware::timeout::{timeout_mw, TimeoutState};
use api::routes;
use api::state::{AppState, DbHealth};

const TEST_TOKEN: &str = "test-token-2026";

/// Дефолтный таймаут для тестов, которые не проверяют timeout.
/// Достаточно большой, чтобы никогда не срабатывал случайно.
const DEFAULT_TEST_TIMEOUT: Duration = Duration::from_secs(30);

/// id, для которого mock-репозиторий имитирует медленный ответ.
/// Используется в тесте `long_request_returns_504`.
const SLOW_CRUISE_ID: &str = "slow";

/// Насколько долго mock-репозиторий "висит" для `SLOW_CRUISE_ID`.
/// Должно быть заметно больше тестового таймаута.
const SLOW_DELAY: Duration = Duration::from_millis(200);

// ============================================================
// Mock repository
// ============================================================

struct MockRepo;

impl MockRepo {
    fn fixed_items() -> Vec<CruiseListItem> {
        (1..=3)
            .map(|i| CruiseListItem {
                cruise_id: format!("c{i}"),
                name: format!("Route {i}"),
                ship_name: Some("Ship Alpha".into()),
                begin_date: NaiveDate::from_ymd_opt(2026, 9, 10 + i as u32).unwrap(),
                end_date: NaiveDate::from_ymd_opt(2026, 9, 15).unwrap(),
                days: Some(5),
                route: None,
                departure_city: Some("Астрахань".into()),
                minimal_price: Some(Decimal::from(50000)),
                room_counts: 3,
                is_active: true,
            })
            .collect()
    }

    fn fixed_detail(id: &str) -> CruiseDetail {
        CruiseDetail {
            cruise_id: id.into(),
            name: "Perm-Samara".into(),
            ship_name: Some("Ship Alpha".into()),
            begin_date: NaiveDate::from_ymd_opt(2026, 9, 10).unwrap(),
            begin_time: None,
            end_date: NaiveDate::from_ymd_opt(2026, 9, 15).unwrap(),
            end_time: None,
            days: Some(5),
            route: None,
            departure_city: Some("Астрахань".into()),
            city_from: None,
            city_to: None,
            is_return: None,
            is_weekend: None,
            is_active: true,
            status: "active".into(),
            prices: vec![],
            rooms: vec![],
        }
    }
}

#[async_trait]
impl CruiseReadRepository for MockRepo {
    async fn list_cruises(
        &self,
        filter: &CruiseListFilter,
    ) -> Result<Vec<CruiseListItem>, ReadError> {
        let mut items = Self::fixed_items();

        if let Some(cursor) = &filter.cursor {
            items.retain(|i| (i.begin_date, &i.cruise_id) > (cursor.begin_date, &cursor.cruise_id));
        }

        let sql_limit = filter.limit + 1;
        items.truncate(sql_limit as usize);
        Ok(items)
    }

    async fn get_cruise(
        &self,
        _cruise_provider_id: &str,
        cruise_provider_cruise_id: &str,
    ) -> Result<Option<CruiseDetail>, ReadError> {
        if cruise_provider_cruise_id == SLOW_CRUISE_ID {
            tokio::time::sleep(SLOW_DELAY).await;
        }
        if cruise_provider_cruise_id == "448" || cruise_provider_cruise_id == SLOW_CRUISE_ID {
            Ok(Some(Self::fixed_detail(cruise_provider_cruise_id)))
        } else {
            Ok(None)
        }
    }
}

// ============================================================
// Mock DbHealth
// ============================================================

struct MockDbHealth {
    ok: bool,
}

#[async_trait]
impl DbHealth for MockDbHealth {
    async fn ping(&self) -> Result<(), String> {
        if self.ok {
            Ok(())
        } else {
            Err("mock: db down".into())
        }
    }
}

// ============================================================
// Test app builder
// ============================================================

fn build_test_app() -> Router {
    build_test_app_full("1", true, DEFAULT_TEST_TIMEOUT)
}

fn build_test_app_with_default_provider(default_provider_id: &str) -> Router {
    build_test_app_full(default_provider_id, true, DEFAULT_TEST_TIMEOUT)
}

fn build_test_app_with_db_state(db_ok: bool) -> Router {
    build_test_app_full("1", db_ok, DEFAULT_TEST_TIMEOUT)
}

fn build_test_app_with_timeout(timeout: Duration) -> Router {
    build_test_app_full("1", true, timeout)
}

fn build_test_app_full(default_provider_id: &str, db_ok: bool, timeout: Duration) -> Router {
    let repo: Arc<dyn CruiseReadRepository> = Arc::new(MockRepo);

    let rate_limiter = Arc::new(RateLimiter::new(
        100,
        20.0,
        TrustedProxies::parse("").unwrap(),
    ));

    let db_health: Arc<dyn DbHealth> = Arc::new(MockDbHealth { ok: db_ok });

    let state = AppState::new(
        Arc::new(ListCruisesUseCase::new(repo.clone())),
        Arc::new(GetCruiseUseCase::new(repo)),
        TEST_TOKEN,
        rate_limiter,
        default_provider_id,
        db_health,
    );

    let timeout_state = TimeoutState::new(timeout);

    Router::new()
        .merge(routes::health_router())
        .merge(
            routes::cruises::router()
                .layer(middleware::from_fn_with_state(state.clone(), require_token)),
        )
        .layer(middleware::from_fn_with_state(timeout_state, timeout_mw))
        .layer(middleware::from_fn_with_state(state.clone(), rate_limit_mw))
        .layer(middleware::from_fn(cache_headers))
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
        .layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 12345))))
        .with_state(state)
}

fn get(uri: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .body(Body::empty())
        .unwrap()
}

fn get_auth(uri: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {TEST_TOKEN}"))
        .body(Body::empty())
        .unwrap()
}

fn header_str(resp: &axum::response::Response, name: header::HeaderName) -> Option<String> {
    resp.headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(resp.into_body(), 10 * 1024 * 1024)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

// ============================================================
// Liveness / Readiness
// ============================================================

#[tokio::test]
async fn liveness_returns_ok() {
    let resp = build_test_app().oneshot(get("/health")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn liveness_ignores_db_state() {
    let app = build_test_app_with_db_state(false);
    let resp = app.oneshot(get("/health")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn liveness_has_no_etag_or_cache_control() {
    let resp = build_test_app().oneshot(get("/health")).await.unwrap();
    assert!(
        resp.headers().get(header::ETAG).is_none(),
        "/health не должен иметь ETag"
    );
    assert!(
        resp.headers().get(header::CACHE_CONTROL).is_none(),
        "/health не должен иметь Cache-Control"
    );
}

#[tokio::test]
async fn readiness_returns_200_when_db_ok() {
    let resp = build_test_app().oneshot(get("/ready")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn readiness_returns_503_when_db_down() {
    let app = build_test_app_with_db_state(false);
    let resp = app.oneshot(get("/ready")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);

    let body = body_json(resp).await;
    assert_eq!(body["status"], "unavailable");
    assert_eq!(body["reason"], "database");
}

#[tokio::test]
async fn health_and_ready_do_not_require_auth() {
    let resp = build_test_app().oneshot(get("/health")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "/health без токена");

    let resp = build_test_app().oneshot(get("/ready")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "/ready без токена");
}

// ============================================================
// Timeout
// ============================================================

/// Медленный handler (> timeout) → 504 Gateway Timeout.
#[tokio::test]
async fn slow_handler_returns_504() {
    let app = build_test_app_with_timeout(Duration::from_millis(50));

    let uri = format!("/cruises/{SLOW_CRUISE_ID}");
    let resp = app.oneshot(get_auth(&uri)).await.unwrap();

    assert_eq!(resp.status(), StatusCode::GATEWAY_TIMEOUT);
}

/// Тело 504 — валидный JSON в формате `ApiError`.
#[tokio::test]
async fn slow_handler_504_body_is_json() {
    let app = build_test_app_with_timeout(Duration::from_millis(50));

    let uri = format!("/cruises/{SLOW_CRUISE_ID}");
    let resp = app.oneshot(get_auth(&uri)).await.unwrap();

    assert_eq!(resp.status(), StatusCode::GATEWAY_TIMEOUT);

    let ct = header_str(&resp, header::CONTENT_TYPE);
    assert_eq!(ct.as_deref(), Some("application/json"));

    let body = body_json(resp).await;
    assert_eq!(body["error"], "gateway_timeout");
    assert!(body["message"].is_string());
}

/// Быстрый handler (< timeout) → 200, таймаут не срабатывает.
/// Проверяем, что middleware не портит нормальные запросы.
#[tokio::test]
async fn fast_handler_is_not_affected_by_timeout() {
    let app = build_test_app_with_timeout(Duration::from_secs(5));

    let resp = app.oneshot(get_auth("/cruises/448")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

// ============================================================
// Request-Id
// ============================================================

#[tokio::test]
async fn response_has_x_request_id() {
    let resp = build_test_app().oneshot(get("/health")).await.unwrap();

    let id = header_str(&resp, header::HeaderName::from_static("x-request-id"));
    assert!(id.is_some(), "должен быть X-Request-Id");
    assert!(id.unwrap().len() > 10, "UUID длиной > 10 символов");
}

#[tokio::test]
async fn client_supplied_request_id_is_preserved() {
    let req = Request::builder()
        .uri("/health")
        .header("x-request-id", "my-test-123")
        .body(Body::empty())
        .unwrap();

    let resp = build_test_app().oneshot(req).await.unwrap();
    let id = header_str(&resp, header::HeaderName::from_static("x-request-id"));
    assert_eq!(id.as_deref(), Some("my-test-123"));
}

// ============================================================
// Auth
// ============================================================

#[tokio::test]
async fn list_without_token_returns_401() {
    let resp = build_test_app().oneshot(get("/cruises")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn list_with_wrong_token_returns_401() {
    let req = Request::builder()
        .uri("/cruises")
        .header(header::AUTHORIZATION, "Bearer wrong-token")
        .body(Body::empty())
        .unwrap();

    let resp = build_test_app().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn list_with_valid_token_returns_200() {
    let resp = build_test_app()
        .oneshot(get_auth("/cruises"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    assert_eq!(body["items"].as_array().unwrap().len(), 3);
    assert_eq!(body["has_more"], false);
    assert!(
        body.get("next_cursor").is_none(),
        "next_cursor должен отсутствовать"
    );
}

// ============================================================
// Pagination (cursor)
// ============================================================

#[tokio::test]
async fn list_limit_clamped_to_100() {
    let resp = build_test_app()
        .oneshot(get_auth("/cruises?limit=100000"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    assert_eq!(body["limit"], 100, "limit должен быть обрезан до 100");
}

#[tokio::test]
async fn list_limit_clamped_to_min_1() {
    let resp = build_test_app()
        .oneshot(get_auth("/cruises?limit=0"))
        .await
        .unwrap();
    let body = body_json(resp).await;
    assert_eq!(body["limit"], 1);
}

#[tokio::test]
async fn list_returns_has_more_and_next_cursor() {
    let resp = build_test_app()
        .oneshot(get_auth("/cruises?limit=2"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    assert_eq!(body["items"].as_array().unwrap().len(), 2);
    assert_eq!(body["has_more"], true);
    assert!(
        body["next_cursor"].is_string(),
        "next_cursor должен быть строкой: {body}"
    );
}

#[tokio::test]
async fn list_cursor_pagination_roundtrip() {
    let resp1 = build_test_app()
        .oneshot(get_auth("/cruises?limit=2"))
        .await
        .unwrap();
    assert_eq!(resp1.status(), StatusCode::OK);
    let body1 = body_json(resp1).await;
    let cursor = body1["next_cursor"].as_str().unwrap().to_string();
    let c1_id = body1["items"][0]["cruise_id"].as_str().unwrap().to_string();
    let c2_id = body1["items"][1]["cruise_id"].as_str().unwrap().to_string();

    let uri = format!("/cruises?limit=2&cursor={cursor}");
    let resp2 = build_test_app().oneshot(get_auth(&uri)).await.unwrap();
    assert_eq!(resp2.status(), StatusCode::OK);
    let body2 = body_json(resp2).await;
    let c3_id = body2["items"][0]["cruise_id"].as_str().unwrap().to_string();

    assert_ne!(c1_id, c2_id);
    assert_ne!(c2_id, c3_id);
    assert_ne!(c1_id, c3_id);
    assert_eq!(body2["has_more"], false);
}

#[tokio::test]
async fn list_invalid_cursor_returns_400() {
    let resp = build_test_app()
        .oneshot(get_auth("/cruises?cursor=zzzz-not-hex"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn list_empty_cursor_param_is_ignored() {
    let resp = build_test_app()
        .oneshot(get_auth("/cruises?cursor="))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

// ============================================================
// Validation
// ============================================================

#[tokio::test]
async fn invalid_date_returns_400() {
    let resp = build_test_app()
        .oneshot(get_auth("/cruises?begin_from=not-a-date"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

// ============================================================
// get_one
// ============================================================

#[tokio::test]
async fn get_existing_cruise_returns_detail() {
    let resp = build_test_app()
        .oneshot(get_auth("/cruises/448"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    assert_eq!(body["cruise_id"], "448");
}

#[tokio::test]
async fn get_missing_cruise_returns_404() {
    let resp = build_test_app()
        .oneshot(get_auth("/cruises/999"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ============================================================
// ETag / Cache-Control
// ============================================================

#[tokio::test]
async fn list_response_has_etag_and_cache_control() {
    let resp = build_test_app()
        .oneshot(get_auth("/cruises"))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let etag = header_str(&resp, header::ETAG);
    assert!(etag.is_some(), "должен быть ETag");
    assert!(etag.unwrap().starts_with('"'), "ETag в кавычках");

    let cc = header_str(&resp, header::CACHE_CONTROL);
    assert_eq!(cc.as_deref(), Some("private, max-age=60"));
}

#[tokio::test]
async fn etag_exact_match_returns_304() {
    let resp1 = build_test_app()
        .oneshot(get_auth("/cruises"))
        .await
        .unwrap();
    let etag = header_str(&resp1, header::ETAG).expect("ETag on first request");

    let req = Request::builder()
        .uri("/cruises")
        .header(header::AUTHORIZATION, format!("Bearer {TEST_TOKEN}"))
        .header(header::IF_NONE_MATCH, &etag)
        .body(Body::empty())
        .unwrap();

    let resp2 = build_test_app().oneshot(req).await.unwrap();
    assert_eq!(resp2.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(
        header_str(&resp2, header::ETAG).as_deref(),
        Some(etag.as_str())
    );
}

#[tokio::test]
async fn etag_wildcard_returns_304() {
    let req = Request::builder()
        .uri("/cruises")
        .header(header::AUTHORIZATION, format!("Bearer {TEST_TOKEN}"))
        .header(header::IF_NONE_MATCH, "*")
        .body(Body::empty())
        .unwrap();

    let resp = build_test_app().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_MODIFIED);
}

#[tokio::test]
async fn etag_weak_match_returns_304() {
    let resp1 = build_test_app()
        .oneshot(get_auth("/cruises"))
        .await
        .unwrap();
    let etag = header_str(&resp1, header::ETAG).expect("ETag on first request");

    let req = Request::builder()
        .uri("/cruises")
        .header(header::AUTHORIZATION, format!("Bearer {TEST_TOKEN}"))
        .header(header::IF_NONE_MATCH, format!("W/{etag}"))
        .body(Body::empty())
        .unwrap();

    let resp2 = build_test_app().oneshot(req).await.unwrap();
    assert_eq!(resp2.status(), StatusCode::NOT_MODIFIED);
}

#[tokio::test]
async fn etag_mismatch_returns_200_with_body() {
    let req = Request::builder()
        .uri("/cruises")
        .header(header::AUTHORIZATION, format!("Bearer {TEST_TOKEN}"))
        .header(header::IF_NONE_MATCH, "\"something-else\"")
        .body(Body::empty())
        .unwrap();

    let resp = build_test_app().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn etag_not_set_on_404() {
    let resp = build_test_app()
        .oneshot(get_auth("/cruises/nonexistent"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert!(resp.headers().get(header::ETAG).is_none());
}

#[tokio::test]
async fn etag_not_set_on_401() {
    let resp = build_test_app().oneshot(get("/cruises")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert!(resp.headers().get(header::ETAG).is_none());
}

// ============================================================
// Method
// ============================================================

#[tokio::test]
async fn post_on_cruises_returns_405() {
    let req = Request::builder()
        .method("POST")
        .uri("/cruises")
        .header(header::AUTHORIZATION, format!("Bearer {TEST_TOKEN}"))
        .body(Body::empty())
        .unwrap();

    let resp = build_test_app().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn list_response_cache_control_is_private_not_public() {
    let resp = build_test_app()
        .oneshot(get_auth("/cruises"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let cc = header_str(&resp, header::CACHE_CONTROL).expect("Cache-Control header");
    assert!(
        cc.contains("private"),
        "Cache-Control must be private, got: {cc}"
    );
    assert!(
        !cc.contains("public"),
        "Cache-Control must NOT be public (leak via shared cache), got: {cc}"
    );
}

#[tokio::test]
async fn not_modified_cache_control_is_private_too() {
    let resp1 = build_test_app()
        .oneshot(get_auth("/cruises"))
        .await
        .unwrap();
    let etag = header_str(&resp1, header::ETAG).expect("ETag");

    let req = Request::builder()
        .uri("/cruises")
        .header(header::AUTHORIZATION, format!("Bearer {TEST_TOKEN}"))
        .header(header::IF_NONE_MATCH, &etag)
        .body(Body::empty())
        .unwrap();

    let resp2 = build_test_app().oneshot(req).await.unwrap();
    assert_eq!(resp2.status(), StatusCode::NOT_MODIFIED);

    let cc = header_str(&resp2, header::CACHE_CONTROL).expect("Cache-Control");
    assert!(cc.contains("private"), "304 must be private, got: {cc}");
    assert!(!cc.contains("public"), "304 must NOT be public, got: {cc}");
}

#[tokio::test]
async fn list_uses_default_provider_id_from_state() {
    let app = build_test_app_with_default_provider("custom-42");

    let resp = app.oneshot(get_auth("/cruises")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    assert_eq!(body["items"].as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn too_many_requests_has_status_and_retry_after() {
    let resp = TooManyRequests.into_response();
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        resp.headers().get(header::RETRY_AFTER).unwrap(),
        HeaderValue::from_static("1"),
    );
}
