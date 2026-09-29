//! Criterion benchmarks для `Fingerprint::of`.
//!
//! ## Что измеряем
//!
//! `sha2::Sha256` + `hex::encode` — полный цикл построения fingerprint.
//! Используется в `sync_one::sync_inner` (шаг Hash) для дедупликации:
//! если fingerprint уже в `raw_snapshots`, sync пропускается.
//!
//! ## Что сравниваем
//!
//! Четыре варианта на каждом размере входных данных:
//!
//! - `full` — текущая реализация `Fingerprint::of` (sha256 + hex::encode).
//! - `sha256_only` — только `Sha256::finalize()` в `[u8; 32]`, без hex.
//!   Разница `full − sha256_only` ≈ стоимость hex-кодирования 32 байт.
//! - `hex_crate` — изолированно `hex::encode(&hash)` (32 байта → 64 hex).
//! - `hex_manual` — изолированно ручное hex-кодирование через lookup-таблицу.
//!   Показывает потолок оптимизации, если бы мы убрали `hex`-крейт.
//!
//! ## Зачем это нужно (техдолг C-1)
//!
//! У `Fingerprint` нет `as_bytes()`. Гипотеза: держать `[u8; 32]` и кодировать
//! в hex лениво — быстрее, чем кодировать сразу на каждом `of()`.
//! Проверяем:
//!
//! - Если `sha256_only` ≈ `full` (hex < 5% от total) → оптимизация не нужна,
//!   оставляем `String` и закрываем C-1 как «не имеет смысла».
//! - Если `full` >> `sha256_only` → внедряем `as_bytes()` + ленивое кодирование.
//!
//! Для домена, где `Fingerprint::of` вызывается один раз за sync (раз в час
//! по расписанию), даже 10% на hex — статистический шум. Но проверить стоит:
//! `of()` также будет вызываться в бенчмарках SCD2/keyset и в CI.
//!
//! ## Запуск
//!
//! ```text
//! cargo bench -p domain --bench fingerprint
//! cargo bench -p domain --bench fingerprint -- 1mb   # только один размер
//! ```

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use sha2::{Digest, Sha256};

use domain::fingerprint::Fingerprint;

/// Lookup-таблица lowercase hex. Используется в `manual_hex`.
const HEX_LUT: &[u8; 16] = b"0123456789abcdef";

/// Ручное hex-кодирование без `hex`-крейта.
///
/// Пишем в `String::with_capacity(2 * len)` — фиксированная аллокация, без
/// амортизированного роста. `push(char)` для ASCII-символа компилируется в
/// один байтовый write — LLVM видит, что `HEX_LUT[i] as char` всегда < 128.
fn manual_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX_LUT[(b >> 4) as usize] as char);
        s.push(HEX_LUT[(b & 0x0f) as usize] as char);
    }
    s
}

fn bench_fingerprint(c: &mut Criterion) {
    let mut group = c.benchmark_group("fingerprint");

    // Размеры: 1 KB — краевой случай для SHA256 (init/finalize доминируют),
    // 10 MB — верхняя граница реального фида Volga.
    let cases: &[(&str, usize)] = &[
        ("1kb", 1024),
        ("100kb", 100 * 1024),
        ("1mb", 1024 * 1024),
        ("10mb", 10 * 1024 * 1024),
    ];

    for (label, size) in cases {
        // Данные — повторяющийся байт. Не криптостойкий паттерн, но для
        // измерения throughput SHA256 это не важно: алгоритм не
        // data-dependent по ветвлениям, throughput стабилен.
        let data = vec![0x42u8; *size];

        group.throughput(Throughput::Bytes(*size as u64));

        // --- full: текущая реализация ---
        group.bench_with_input(BenchmarkId::new("full", label), &data, |b, data| {
            b.iter(|| criterion::black_box(Fingerprint::of(criterion::black_box(data))));
        });

        // --- sha256_only: тот же input, без hex ---
        group.bench_with_input(BenchmarkId::new("sha256_only", label), &data, |b, data| {
            b.iter(|| {
                let mut h = Sha256::new();
                h.update(criterion::black_box(data));
                criterion::black_box(h.finalize());
            });
        });

        // --- hex: изолированно, на фиксированных 32 байтах ---
        // Hash отличается от `data` — но hex::encode uniform по входу,
        // содержимое на производительность не влияет.
        let hash: [u8; 32] = {
            let mut h = Sha256::new();
            h.update(&data);
            h.finalize().into()
        };

        group.bench_with_input(BenchmarkId::new("hex_crate", label), &hash, |b, hash| {
            b.iter(|| criterion::black_box(hex::encode(criterion::black_box(hash))));
        });

        group.bench_with_input(BenchmarkId::new("hex_manual", label), &hash, |b, hash| {
            b.iter(|| criterion::black_box(manual_hex(criterion::black_box(hash))));
        });
    }

    group.finish();
}

criterion_group!(benches, bench_fingerprint);
criterion_main!(benches);
