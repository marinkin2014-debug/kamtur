//! Security-регрессионные тесты HTTP-обвязки API.
//!
//! Отделены от `api_tests.rs` — там функциональные тесты, здесь только
//! проверки инвариантов безопасности. Если регрессия в безопасности
//! ломает прод, ищем в первую очередь здесь.
//!
//! ## Что здесь тестируется
//!
//! - Маскировка `Authorization` в `Debug`-представлении заголовков
//!   (`SetSensitiveRequestHeadersLayer`).
//!
//! ## Что здесь НЕ тестируется (и где искать)
//!
//! - Constant-time сравнение токена — `api::middleware::auth` unit-тесты.
//! - Cache-Control: private — `api::middleware::cache` unit-тесты.
//! - Rate limiting — `api::middleware::rate_limit` unit-тесты.
//! - TLS — инфраструктурный уровень, тестируется в integration-тестах
//!   с реальным сервером, вне scope этого файла.

use std::convert::Infallible;
use std::iter::once;

use axum::body::Body;
use axum::http::{header, Request, Response};
use tower::{ServiceBuilder, ServiceExt};
use tower_http::sensitive_headers::SetSensitiveRequestHeadersLayer;

// ============================================================
// Authorization masking
// ============================================================

/// Регрессия: `SetSensitiveRequestHeadersLayer` маскирует `Authorization`
/// в `Debug`-представлении заголовков.
///
/// ## Почему это важно
///
/// `bootstrap.rs` вешает `SetSensitiveRequestHeadersLayer` **снаружи**
/// `TraceLayer`. `TraceLayer` при логировании запроса вызывает
/// `Debug` для `HeaderMap` через `make_span_with`. Без маскировки
/// `Authorization: Bearer <token>` попадает в каждую лог-строку HTTP-
/// запроса — токен утекает в Loki/Elasticsearch, доступный всей команде
/// DevOps.
///
/// ## Что проверяет тест
///
/// 1. Значение `Bearer secret-token-xyz` **не появляется** в `Debug`.
/// 2. Имя заголовка `authorization` **остаётся** (замаскировано как
///    `Sensitive`) — иначе не отличить «маскировано» от «удалено».
/// 3. Любые другие заголовки не маскируются (проверяем, что мы не
///    переусердствовали с `once(header::AUTHORIZATION)`).
///
/// ## Технический подход
///
/// Используем `tower::service_fn`, возвращающий `Debug`-строку заголовков
/// в body. Оборачиваем через `ServiceBuilder::layer(SetSensitiveRequestHeadersLayer)`.
/// Это **не** тестирует порядок слоёв в `bootstrap.rs` (компилятор
/// гарантирует, что слой есть), но фиксирует контракт `tower-http`:
/// если зависимость обновится и маскировка сломается — тест упадёт.
///
/// Альтернатива через `tracing_subscriber::MakeWriter` — не работает
/// надёжно в параллельных тестах: `set_global_default` паникует при
/// повторной установке, `set_default` использует thread-local, который
/// теряется при миграции tokio-задач между потоками.
#[tokio::test]
async fn sensitive_headers_layer_masks_authorization_in_debug() {
    // Внутренний service: возвращает Debug заголовков в body.
    // `Infallible` — тип ошибки, которая никогда не возникает.
    let inner = tower::service_fn(|req: Request<Body>| async move {
        let debug = format!("{:?}", req.headers());
        Ok::<_, Infallible>(Response::new(Body::from(debug)))
    });

    // Слой маскировки: те же аргументы, что в `bootstrap.rs`.
    let service = ServiceBuilder::new()
        .layer(SetSensitiveRequestHeadersLayer::new(once(
            header::AUTHORIZATION,
        )))
        .service(inner);

    let req = Request::builder()
        .uri("/")
        .header(header::AUTHORIZATION, "Bearer secret-token-xyz")
        .header("x-request-id", "abc-123")
        .body(Body::empty())
        .unwrap();

    let resp = service.oneshot(req).await.unwrap();
    let bytes = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
    let debug = String::from_utf8(bytes.to_vec()).unwrap();

    // 1. Значение токена не должно светиться в Debug.
    assert!(
        !debug.contains("secret-token-xyz"),
        "Authorization value leaked into Debug output: {debug}",
    );
    assert!(
        !debug.contains("Bearer"),
        "`Bearer` prefix leaked into Debug output: {debug}",
    );

    // 2. Имя заголовка осталось — замаскировано как `Sensitive`.
    assert!(
        debug.contains("authorization"),
        "header name must remain visible (masked), got: {debug}",
    );
    assert!(
        debug.contains("Sensitive"),
        "masked header must display as `Sensitive`, got: {debug}",
    );

    // 3. Обычные заголовки не маскируются — значение `x-request-id`
    //    видно как есть. Иначе мы бы прятали больше, чем нужно, и
    //    ломали отладку.
    assert!(
        debug.contains("x-request-id"),
        "x-request-id header missing from Debug: {debug}",
    );
    assert!(
        debug.contains("abc-123"),
        "x-request-id value must NOT be masked, got: {debug}",
    );
}

/// Регрессия: без `SetSensitiveRequestHeadersLayer` токен **светится**.
///
/// Этот тест — «контрольный эксперимент»: он фиксирует, что наша
/// зависимость (`tower-http`) реально делает разницу. Если однажды
/// окажется, что `SetSensitiveRequestHeadersLayer` — no-op (или
/// `HeaderMap::Debug` сам маскирует `Authorization`), тест 1 перестанет
/// ловить регрессию в `bootstrap.rs`. Этот тест это выявит.
///
/// **Тест 2** (без слоя) должен всегда проходить. Если он когда-нибудь
/// упадёт — значит `tower-http` изменил семантику `HeaderMap::Debug`,
/// и нужно пересмотреть подход к маскировке.
#[tokio::test]
async fn without_sensitive_layer_authorization_is_visible() {
    let inner = tower::service_fn(|req: Request<Body>| async move {
        let debug = format!("{:?}", req.headers());
        Ok::<_, Infallible>(Response::new(Body::from(debug)))
    });

    let req = Request::builder()
        .uri("/")
        .header(header::AUTHORIZATION, "Bearer secret-token-xyz")
        .body(Body::empty())
        .unwrap();

    let resp = inner.oneshot(req).await.unwrap();
    let bytes = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
    let debug = String::from_utf8(bytes.to_vec()).unwrap();

    // Без слоя токен виден — это контраст к первому тесту.
    assert!(
        debug.contains("secret-token-xyz"),
        "sanity check failed: without layer, Debug should contain the token. \
         If this fails, `tower-http` changed semantics and \
         `sensitive_headers_layer_masks_authorization_in_debug` no longer \
         verifies anything. Got: {debug}",
    );
}
