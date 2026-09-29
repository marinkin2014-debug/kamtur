//! Criterion benchmarks для `etag_from_sha256`.
//!
//! ## Что измеряем
//!
//! Формирование `ETag` из SHA256-хэша тела: `"<16 hex>"` (18 символов).
//! Вызывается в `cache_headers` middleware на каждый `GET /cruises*`
//! — hot path read-API.
//!
//! ## Что сравниваем
//!
//! Три варианта на фиксированных 32 байтах:
//!
//! - `optimized` — текущая реализация из `cache.rs`: один
//!   `String::with_capacity(18)` + `write!` в цикле по 8 байтам.
//! - `naive` — гипотетическая альтернатива: `hex::encode(&hash[..8])`
//!   (аллокация №1) + `format!("\"{}\"", ...)` (аллокация №2).
//! - `naive_full` — совсем наивная: `hex::encode(hash)` (64-символьная
//!   строка) + `format!` с обрезкой до 16 символов. Две аллокации +
//!   лишнее кодирование 24 неиспользуемых байт.
//!
//! ## Зачем
//!
//! Docstring `etag_from_sha256` в `cache.rs` утверждает: «Одна аллокация,
//! ноль промежуточных строк». Бенчмарк подтверждает или опровергает это.
//!
//! Если `optimized` значимо быстрее `naive` — оставляем как есть.
//! Если одинаково — можно упростить код до `naive` (меньше строк — меньше
//! поддерживать). Если `optimized` медленнее — баг в реализации.
//!
//! ## Запуск
//!
//! ```text
//! cargo bench -p api --bench etag
//! ```

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use sha2::{Digest, Sha256};

use api::middleware::cache::etag_from_sha256;

/// Наивная реализация: `hex::encode(&hash[..8])` + `format!` с кавычками.
///
/// Две аллокации: одна под `String` от `hex::encode` (16 символов), вторая —
/// от `format!` (18 символов с кавычками). Docstring `cache.rs` утверждает,
/// что наша реализация избегает именно этой пары.
#[inline]
fn etag_naive(hash: &[u8; 32]) -> String {
    format!("\"{}\"", hex::encode(&hash[..8]))
}

/// Совсем наивная: кодируем весь 64-символьный хэш, потом обрезаем.
///
/// 24 неиспользуемых байта кодируются впустую + обрезка создаёт новую
/// строку через `format!`. Показывает верхнюю границу «глупости» — сколько
/// мы экономим, кодируя только 8 байт.
#[inline]
fn etag_naive_full(hash: &[u8; 32]) -> String {
    let hex = hex::encode(hash);
    format!("\"{}\"", &hex[..16])
}

fn bench_etag(c: &mut Criterion) {
    let mut group = c.benchmark_group("etag_from_sha256");

    // Одна «порция» — 32 байта на вызов. Throughput в элементах/сек.
    group.throughput(Throughput::Elements(1));

    // Реалистичный хэш: SHA256 от строки, похожей на типичный ответ API.
    // Значение не важно — важно, что все 32 байта задействованы, иначе
    // компилятор мог бы заоптимизировать какие-то ветки.
    let hash: [u8; 32] = {
        let mut h = Sha256::new();
        h.update(b"typical-api-response-body-payload");
        h.finalize().into()
    };

    group.bench_function("optimized", |b| {
        b.iter(|| criterion::black_box(etag_from_sha256(criterion::black_box(&hash))));
    });

    group.bench_function("naive", |b| {
        b.iter(|| criterion::black_box(etag_naive(criterion::black_box(&hash))));
    });

    group.bench_function("naive_full", |b| {
        b.iter(|| criterion::black_box(etag_naive_full(criterion::black_box(&hash))));
    });

    group.finish();
}

criterion_group!(benches, bench_etag);
criterion_main!(benches);
