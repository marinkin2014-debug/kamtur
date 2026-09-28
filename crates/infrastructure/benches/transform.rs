//! Criterion benchmarks для `VolgaCanonicalTransformer::transform`.
//!
//! ## Что измеряем
//!
//! `RawData` → `CanonicalData`. Полный цикл нормализации:
//! `object_id_from_class_id` (нарезка строк + аллокация), парсинг дат
//! через `chrono::NaiveDate::parse_from_str`, парсинг времени через
//! `chrono::NaiveTime::from_str`, `HashMap`-lookup по `cruise_id` для
//! `child_price` / `extra_seat`, `Vec::push` по восьми коллекциям.
//!
//! ## Отношение к `bench parser`
//!
//! `VolgaProvider::parse(xml)` = `VolgaParser::parse(xml)` +
//! `transform(raw)`. Оба бенчмарка вместе дают полное время
//! блокирующей фазы sync-цикла (см. `sync_one.rs`, шаг Parse).
//!
//! `bench transform` изолирует второй шаг. Разница с `bench parser`
//! показывает, во что обходится нормализация относительно собственно
//! XML-парсинга.
//!
//! ## Setup vs измерение
//!
//! `transform` принимает `RawData` по значению — в проде raw
//! потребляется один раз, копировать его нет смысла. Но бенчмарк
//! вызывает transform много раз, значит нужна свежая копия `RawData`
//! на каждую итерацию.
//!
//! Наивный `b.iter(|| transform(raw.clone()))` включал бы клонирование
//! `RawData` в замер. На 50 MiB raw — это ~2M `RawCabin`, ~10M `String`
//! — клонирование заняло бы больше времени, чем сам transform.
//!
//! `b.iter_batched()` разделяет setup (не измеряется) и routine
//! (измеряется). `BatchSize::LargeInput` говорит criterion, что
//! входные данные большие: он не будет держать много копий в памяти,
//! batch = 1 итерация.
//!
//! ## Запуск
//!
//! ```text
//! cargo bench --bench transform
//! cargo bench --bench transform -- 1mb
//! ```
//!
//! Baseline: см. docs/performance.md.

mod common;

use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput};
use infrastructure::providers::volga_wolga::{VolgaCanonicalTransformer, VolgaParser};

fn bench_volga_transform(c: &mut Criterion) {
    let mut group = c.benchmark_group("volga_transform");

    let cases: &[(&str, usize)] = &[
        ("1mb", 1024 * 1024),
        ("10mb", 10 * 1024 * 1024),
        ("50mb", 50 * 1024 * 1024),
    ];

    for (label, target) in cases {
        let xml = common::generate_volga_xml(*target);
        let raw = VolgaParser.parse(&xml).expect("parse fixture");

        // Throughput считаем по размеру XML, не по размеру raw. Это
        // позволяет сравнивать с `bench parser` напрямую: «58 MiB/s
        // парсим XML» vs «X MiB/s нормализуем тот же XML». Единая
        // база — исходный фид.
        group.throughput(Throughput::Bytes(xml.len() as u64));

        eprintln!(
            "fixture {label}: xml={} bytes, ships={}, decks={}, classes={}, \
             cabins={}, cruises={}, prices={}, spos={}, free={}",
            xml.len(),
            raw.ships.len(),
            raw.decks.len(),
            raw.classes.len(),
            raw.cabins.len(),
            raw.cruises.len(),
            raw.prices.len(),
            raw.spos.len(),
            raw.free.len(),
        );

        let transformer = VolgaCanonicalTransformer::new(1);

        group.bench_with_input(BenchmarkId::from_parameter(label), &raw, |b, raw| {
            b.iter_batched(
                // Setup: свежая копия на каждую итерацию. НЕ измеряется.
                || raw.clone(),
                // Routine: чистый transform. Измеряется.
                |raw_owned| {
                    let data = transformer.transform(raw_owned).expect("transform");
                    // black_box на выходе: transform не должен быть вырезан.
                    criterion::black_box(data);
                },
                // LargeInput: criterion не будет держать много копий
                // raw в памяти между итерациями. Batch = 1.
                BatchSize::LargeInput,
            )
        });
    }

    group.finish();
}

criterion_group!(benches, bench_volga_transform);
criterion_main!(benches);
