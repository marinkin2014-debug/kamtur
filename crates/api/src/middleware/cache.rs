use axum::body::{to_bytes, Body};
use axum::http::{header, HeaderValue, Method, Request, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use http_body::Body as _; // для size_hint()
use sha2::{Digest, Sha256};
use std::fmt::Write as _; // для write!() в String

/// Максимальный размер тела, которое готовы буферизовать для ETag.
const MAX_BODY: usize = 1024 * 1024; // 1 MiB

/// Cache-Control для авторизованных read-endpoint'ов.
///
/// `private` — критично: все `/cruises*` требуют `Authorization`.
/// Если поставить `public`, любой shared cache (корпоративный proxy,
/// CDN, Varnish) закэширует ответ и отдаст его клиенту без токена,
/// не доходя до нашего API. RFC 7234 §3.2.
///
/// `max-age=60` — клиентский кэш браузера на 60 секунд. После — revalidate
/// через If-None-Match, обычно 304.
const CACHE_CONTROL_VALUE: &str = "private, max-age=60";

/// Добавляет `Cache-Control` и `ETag` к GET-ответам на кэшируемых путях.
///
/// Кэшируются только публичные read-endpoint'ы (`/cruises*`). Служебные
/// `/health`, `/metrics` не кэшируются.
///
/// Проверка размера идёт через `Body::size_hint()`, а не через header
/// `Content-Length`. В axum 0.7 `Json<T>` не выставляет этот header —
/// он появляется только при сериализации в сокет (hyper). Чтение header'а
/// из Response дало бы всегда `None` и отключало ETag.
///
/// ## Про аллокации
///
/// На hot path middleware избегаем лишних аллокаций:
///
/// - ETag формируется за одну аллокацию (`etag_from_sha256`), без
///   промежуточной 64-символьной hex-строки.
/// - `bytes` тела уходит в `Body::from(bytes)` без копирования.
pub async fn cache_headers(req: Request<Body>, next: Next) -> Response {
    if req.method() != Method::GET {
        return next.run(req).await;
    }

    if !is_cacheable_path(req.uri().path()) {
        return next.run(req).await;
    }

    let if_none_match = req
        .headers()
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);

    let response = next.run(req).await;

    // ETag только для 200 OK.
    if response.status() != StatusCode::OK {
        return response;
    }

    // Проверяем размер тела до того, как что-либо буферизовать.
    // Берём либо header (если кто-то выше его выставил), либо size_hint body.
    let exact_len = response
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .or_else(|| response.body().size_hint().exact());

    match exact_len {
        Some(len) if len <= MAX_BODY as u64 => { /* ok, буферизуем */ }
        _ => {
            // Либо тело точно больше MAX_BODY, либо размер неизвестен —
            // не рискуем съесть поток и не восстановить.
            return response;
        }
    }

    let (mut parts, body) = response.into_parts();

    let bytes = match to_bytes(body, MAX_BODY).await {
        Ok(b) => b,
        Err(e) => {
            // size_hint обещал, что влезет, но что-то пошло не так (streaming).
            // Тело уже съедено — восстановить нельзя. Отдаём 500, чтобы клиент
            // не получил усечённый контент с валидным статусом.
            tracing::error!(error = %e, "cache middleware: failed to buffer body");
            let mut err = Response::new(Body::from(r#"{"error":"internal_error"}"#));
            *err.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
            err.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            return err;
        }
    };

    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let hash: [u8; 32] = hasher.finalize().into();
    let etag = etag_from_sha256(&hash);

    if let Some(client) = if_none_match {
        if etag_matches(&client, &etag) {
            let mut not_modified = Response::new(Body::empty());
            *not_modified.status_mut() = StatusCode::NOT_MODIFIED;
            if let Ok(value) = HeaderValue::from_str(&etag) {
                not_modified.headers_mut().insert(header::ETAG, value);
            }
            not_modified.headers_mut().insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static(CACHE_CONTROL_VALUE),
            );
            return not_modified;
        }
    }

    if let Ok(value) = HeaderValue::from_str(&etag) {
        parts.headers.insert(header::ETAG, value);
    }
    parts.headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(CACHE_CONTROL_VALUE),
    );

    Response::from_parts(parts, Body::from(bytes))
}

/// Формирует ETag из SHA256-хэша тела.
///
/// Формат: `"<16 hex-символов>"` — 8 первых байт хэша в lowercase hex,
/// обёрнутые в кавычки по RFC 7232. Итог всегда 18 символов:
/// кавычка (1) + 16 hex-символов + кавычка (1).
///
/// ## Почему не `hex::encode(hash)`
///
/// `hex::encode(&hash[..8])` вернул бы `String` на 16 символов, но
/// `hex::encode` работает с любым срезом и аллоцирует под весь вход.
/// Если бы мы взяли `hex::encode(hasher.finalize())` целиком (64 символа),
/// получили бы лишнюю аллокацию + копирование при `format!`.
///
/// Здесь — сразу пишем в целевой буфер фиксированной длины. Одна
/// аллокация, ноль промежуточных строк. `write!` в `String` не может
/// вернуть ошибку по контракту `fmt::Write` для `String`, `expect`
/// документирует инвариант.
#[inline]
fn etag_from_sha256(hash: &[u8; 32]) -> String {
    let mut etag = String::with_capacity(18);
    etag.push('"');
    for b in &hash[..8] {
        write!(&mut etag, "{b:02x}").expect("writing to String never fails");
    }
    etag.push('"');
    etag
}

/// Здесь перечислены только публичные read-endpoint'ы.
#[inline]
fn is_cacheable_path(path: &str) -> bool {
    path.starts_with("/cruises")
}

/// Поддержка списков в `If-None-Match` (`"a", "b"`), weak-валидаторов (`W/"a"`)
/// и `*`.
#[inline]
fn etag_matches(header_value: &str, etag: &str) -> bool {
    header_value.split(',').map(str::trim).any(|candidate| {
        let c = candidate.strip_prefix("W/").unwrap_or(candidate);
        c == etag || c == "*"
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ============================================================
    // etag_matches
    // ============================================================

    #[test]
    fn etag_exact_match() {
        assert!(etag_matches("\"abc\"", "\"abc\""));
    }

    #[test]
    fn etag_weak_match() {
        assert!(etag_matches("W/\"abc\"", "\"abc\""));
    }

    #[test]
    fn etag_wildcard() {
        assert!(etag_matches("*", "\"anything\""));
    }

    #[test]
    fn etag_list_contains_match() {
        assert!(etag_matches("\"x\", \"abc\", \"y\"", "\"abc\""));
        assert!(etag_matches("\"abc\", \"x\"", "\"abc\""));
    }

    #[test]
    fn etag_no_match() {
        assert!(!etag_matches("\"xyz\"", "\"abc\""));
        assert!(!etag_matches("\"x\", \"y\"", "\"abc\""));
    }

    #[test]
    fn etag_handles_spaces() {
        assert!(etag_matches("  \"abc\"  ", "\"abc\""));
        assert!(etag_matches("\"x\" , \"abc\"", "\"abc\""));
    }

    // ============================================================
    // is_cacheable_path
    // ============================================================

    #[test]
    fn cacheable_paths() {
        assert!(is_cacheable_path("/cruises"));
        assert!(is_cacheable_path("/cruises/448"));
        assert!(!is_cacheable_path("/health"));
        assert!(!is_cacheable_path("/metrics"));
        assert!(!is_cacheable_path("/"));
    }

    /// Регрессия: Cache-Control не должен быть `public`.
    /// `public` на авторизованных endpoint'ах позволяет shared cache
    /// отдавать данные клиентам без токена.
    #[test]
    fn cache_control_is_private_not_public() {
        assert!(
            CACHE_CONTROL_VALUE.contains("private"),
            "must be private: {CACHE_CONTROL_VALUE}"
        );
        assert!(
            !CACHE_CONTROL_VALUE.contains("public"),
            "must NOT be public: {CACHE_CONTROL_VALUE}"
        );
    }

    // ============================================================
    // etag_from_sha256
    // ============================================================

    /// Формат ETag: `"<16 hex>"`, всегда 18 символов, lowercase hex.
    #[test]
    fn etag_format_is_quoted_16_hex() {
        let mut h = [0u8; 32];
        h[..8].copy_from_slice(&[0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89]);

        let etag = etag_from_sha256(&h);

        assert_eq!(etag.len(), 18, "кавычка + 16 hex + кавычка = 18");
        assert!(etag.starts_with('"'), "должен начинаться с кавычки");
        assert!(etag.ends_with('"'), "должен заканчиваться кавычкой");
        assert_eq!(etag, "\"abcdef0123456789\"");
    }

    /// ETag использует только первые 8 байт SHA256. Два хэша с одинаковым
    /// префиксом и разным хвостом дают одинаковый ETag.
    #[test]
    fn etag_uses_only_first_8_bytes() {
        let mut h1 = [0u8; 32];
        let mut h2 = [0u8; 32];
        h1[..8].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        h2[..8].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        h1[8] = 0xff;
        h2[8] = 0x00;

        assert_eq!(etag_from_sha256(&h1), etag_from_sha256(&h2));
    }

    /// Детерминированность: одинаковый вход → одинаковый ETag.
    /// Разный вход → разный ETag (в пределах коллизий SHA256).
    #[test]
    fn etag_is_deterministic_and_distinguishes() {
        let a = [0x00; 32];
        let b = [0xff; 32];

        assert_eq!(etag_from_sha256(&a), etag_from_sha256(&a));
        assert_ne!(etag_from_sha256(&a), etag_from_sha256(&b));
    }

    /// Кавычки не экранируются, hex — только [0-9a-f].
    /// Проверка для всех 256 возможных байт на позициях 0..8.
    #[test]
    fn etag_is_always_ascii_hex_or_quote() {
        for byte in 0u8..=255 {
            let mut h = [0u8; 32];
            h[0] = byte;
            let etag = etag_from_sha256(&h);
            for ch in etag.chars() {
                assert!(
                    ch == '"' || ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase(),
                    "недопустимый символ {ch:?} в ETag для byte={byte:#04x}",
                );
            }
        }
    }
}
