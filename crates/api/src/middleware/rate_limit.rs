//! In-memory rate limit по IP (token bucket).
//!
//! Устройство:
//!   - N шардов (по hash от IP), каждый со своим `std::sync::Mutex<HashMap>`.
//!     Устраняет единый глобальный лок.
//!   - Ленивая очистка шарда при его разрастании + периодический sweep
//!     из фоновой задачи (`spawn_sweeper`).
//!   - X-Forwarded-For учитывается ТОЛЬКО если сам peer входит в
//!     TRUSTED_PROXIES (CIDR). Из XFF берётся правый недоверенный IP.
//!
//! Для мульти-инстансового прода — переехать на Redis.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{ConnectInfo, State};
use axum::http::{header, HeaderValue, Request};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use ipnet::IpNet;
use tracing::warn;

use crate::state::AppState;

const SHARDS: usize = 16;
const BUCKET_TTL: Duration = Duration::from_secs(600); // 10 минут простоя → выкидываем
const MAX_ENTRIES_PER_SHARD: usize = 10_000;
const SWEEP_INTERVAL: Duration = Duration::from_secs(60);

// ============================================================
// TrustedProxies
// ============================================================

/// Набор доверенных CIDR. Пустой — XFF полностью игнорируется.
#[derive(Debug)]
pub struct TrustedProxies {
    networks: Vec<IpNet>,
}

impl TrustedProxies {
    pub fn parse(spec: &str) -> Result<Self, String> {
        let networks: Vec<IpNet> = spec
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| IpNet::from_str(s).map_err(|e| format!("bad CIDR `{s}`: {e}")))
            .collect::<Result<_, _>>()?;
        Ok(Self { networks })
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        self.networks.iter().any(|net| net.contains(&ip))
    }

    pub fn is_empty(&self) -> bool {
        self.networks.is_empty()
    }
}

// ============================================================
// RateLimiter
// ============================================================

struct Bucket {
    tokens: f64,
    last_refill: Instant,
    last_seen: Instant,
}

struct Shard {
    map: Mutex<HashMap<IpAddr, Bucket>>,
}

pub struct RateLimiter {
    shards: Vec<Shard>,
    capacity: u32,
    refill_per_sec: f64,
    trusted_proxies: TrustedProxies,
}

impl RateLimiter {
    pub fn new(capacity: u32, refill_per_sec: f64, trusted_proxies: TrustedProxies) -> Self {
        let mut shards = Vec::with_capacity(SHARDS);
        for _ in 0..SHARDS {
            shards.push(Shard {
                map: Mutex::new(HashMap::new()),
            });
        }
        Self {
            shards,
            capacity,
            refill_per_sec,
            trusted_proxies,
        }
    }

    #[inline]
    fn shard_for(&self, ip: IpAddr) -> &Shard {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        ip.hash(&mut h);
        &self.shards[(h.finish() as usize) % SHARDS]
    }

    /// Проверить и списать один токен. Синхронный — блокировка шарда короткая.
    pub fn check(&self, ip: IpAddr) -> bool {
        let now = Instant::now();
        let shard = self.shard_for(ip);
        let mut map = shard.map.lock().unwrap_or_else(|e| e.into_inner());

        // Ленивая очистка: если шард разросся, выкидываем протухшие записи.
        if map.len() > MAX_ENTRIES_PER_SHARD {
            map.retain(|_, b| now.duration_since(b.last_seen) < BUCKET_TTL);
        }

        let capacity = self.capacity as f64;
        let bucket = map.entry(ip).or_insert_with(|| Bucket {
            tokens: capacity,
            last_refill: now,
            last_seen: now,
        });

        let elapsed = now.duration_since(bucket.last_refill).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * self.refill_per_sec).min(capacity);
        bucket.last_refill = now;
        bucket.last_seen = now;

        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// Пройтись по всем шардам и удалить протухшие записи.
    pub fn sweep(&self) {
        let now = Instant::now();
        for shard in &self.shards {
            let mut map = shard.map.lock().unwrap_or_else(|e| e.into_inner());
            map.retain(|_, b| now.duration_since(b.last_seen) < BUCKET_TTL);
        }
    }

    /// Запустить фоновый sweeper.
    pub fn spawn_sweeper(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(SWEEP_INTERVAL);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            ticker.tick().await; // пропускаем первый tick
            loop {
                ticker.tick().await;
                self.sweep();
            }
        })
    }
}

// ============================================================
// Middleware
// ============================================================

/// Возвращаем `Result<Response, Response>`, а не `Result<Response, StatusCode>`,
/// потому что на 429 нужен кастомный `Retry-After`. `Response` реализует
/// `IntoResponse`, axum принимает оба типа.
pub async fn rate_limit_mw(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    req: Request<Body>,
    next: Next,
) -> Result<Response, Response> {
    let ip = client_ip(&state.rate_limiter, &req, addr);
    if state.rate_limiter.check(ip) {
        Ok(next.run(req).await)
    } else {
        warn!(ip = %ip, "rate limit exceeded");
        Err(too_many_requests())
    }
}

/// 429 с корректным Retry-After.
#[inline]
fn too_many_requests() -> Response {
    let mut resp = axum::http::StatusCode::TOO_MANY_REQUESTS.into_response();
    resp.headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
    resp
}

/// Определяет IP клиента.
///
/// Если `TRUSTED_PROXIES` пуст или peer не входит в доверенные —
/// возвращает IP peer'а, XFF полностью игнорируется.
///
/// Если peer доверенный — идём по XFF справа налево и возвращаем
/// первый IP, не входящий в доверенные CIDR (это клиент). Если все
/// доверенные — отдаём peer.
#[inline]
fn client_ip(limiter: &RateLimiter, req: &Request<Body>, peer: SocketAddr) -> IpAddr {
    let trusted = &limiter.trusted_proxies;

    if trusted.is_empty() || !trusted.contains(peer.ip()) {
        return peer.ip();
    }

    let Some(xff) = req
        .headers()
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
    else {
        return peer.ip();
    };

    for candidate in xff.split(',').rev().map(str::trim) {
        if let Ok(ip) = candidate.parse::<IpAddr>() {
            if !trusted.contains(ip) {
                return ip;
            }
        }
    }

    peer.ip()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limiter(capacity: u32, refill: f64) -> RateLimiter {
        RateLimiter::new(capacity, refill, TrustedProxies::parse("").unwrap())
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn allows_up_to_capacity() {
        let rl = limiter(5, 0.0); // без refill
        let client = ip("1.2.3.4");
        for i in 0..5 {
            assert!(rl.check(client), "request {} should pass", i + 1);
        }
        assert!(!rl.check(client), "6th request should be denied");
    }

    #[test]
    fn independent_ips_have_independent_buckets() {
        let rl = limiter(2, 0.0);
        let a = ip("1.1.1.1");
        let b = ip("2.2.2.2");
        assert!(rl.check(a));
        assert!(rl.check(a));
        assert!(!rl.check(a));
        // b не пострадал
        assert!(rl.check(b));
        assert!(rl.check(b));
        assert!(!rl.check(b));
    }

    #[test]
    fn refill_adds_tokens() {
        let rl = limiter(2, 1000.0); // refill 1000/sec
        let client = ip("3.3.3.3");
        assert!(rl.check(client));
        assert!(rl.check(client));
        // за 1ms refill даёт 1 токен
        std::thread::sleep(std::time::Duration::from_millis(2));
        assert!(rl.check(client));
    }

    #[test]
    fn too_many_requests_has_status_and_retry_after() {
        let resp = too_many_requests();
        assert_eq!(resp.status(), axum::http::StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            resp.headers().get(header::RETRY_AFTER).unwrap(),
            HeaderValue::from_static("1"),
        );
    }

    #[test]
    fn trusted_proxies_parse_empty() {
        let tp = TrustedProxies::parse("").unwrap();
        assert!(tp.is_empty());
    }

    #[test]
    fn trusted_proxies_parse_multiple() {
        let tp = TrustedProxies::parse("127.0.0.1/32, 10.0.0.0/8").unwrap();
        assert!(tp.contains(ip("127.0.0.1")));
        assert!(tp.contains(ip("10.1.2.3")));
        assert!(!tp.contains(ip("192.168.1.1")));
    }

    #[test]
    fn trusted_proxies_rejects_invalid_cidr() {
        let err = TrustedProxies::parse("not-a-cidr").unwrap_err();
        assert!(err.contains("bad CIDR"));
    }
}
