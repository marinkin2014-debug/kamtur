//! Criterion benchmarks для `VolgaParser::parse`.
//!
//! ## Что измеряем
//!
//! XML-фид Volga → `RawData`. Полный цикл парсинга через quick-xml:
//! обработка каждого `Event::Start`, `Event::End`, `Event::Empty`,
//! извлечение атрибутов, парсинг `Decimal` / `bool`, переключение
//! контекста (`<prices>`, `<spos>`, `<free>`).
//!
//! ## Что НЕ измеряем
//!
//! - `VolgaCanonicalTransformer::transform` — это B.2 (`bench transform`).
//! - `Fingerprint::of` — это B.3.
//! - Сетевую загрузку (`VolgaFetcher::fetch_raw`) — нестабильно на
//!   CI, внешний сервер. Отдельно в k6 (B.6).
//!
//! ## Запуск
//!
//! ```text
//! cargo bench --bench parser
//! cargo bench --bench parser -- 1mb        # только один кейс
//! ```
//!
//! Baseline (заполняется после прогона на конкретной машине):
//! см. docs/performance.md.

mod common;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use infrastructure::providers::volga_wolga::VolgaParser;

fn bench_volga_parser(c: &mut Criterion) {
    let mut group = c.benchmark_group("volga_parser");

    // 1 MiB = 1024 * 1024. Для 1mb убран множитель `1 *` --
    // clippy::identity_op справедливо ругается на `1 * x`.
    let cases: &[(&str, usize)] = &[
        ("1mb", 1024 * 1024),
        ("10mb", 10 * 1024 * 1024),
        ("50mb", 50 * 1024 * 1024),
    ];

    for (label, target) in cases {
        let xml = common::generate_volga_xml(*target);

        // Реальный размер, а не целевой: генератор может недобрать
        // или перебрать на 5-10%. Печатаем его — чтобы цифры в отчёте
        // были честными.
        eprintln!(
            "fixture {label}: target={} bytes, actual={} bytes",
            target,
            xml.len()
        );

        group.throughput(Throughput::Bytes(xml.len() as u64));

        group.bench_with_input(BenchmarkId::from_parameter(label), &xml, |b, xml| {
            b.iter(|| {
                // `xml` приходит через `bench_with_input` — компилятор
                // видит его как input и не сможет соптимизировать.
                // `expect("parse")`: сбой генератора → паника
                // бенчмарка. Намеренно: фикстура должна быть валидной.
                let data = VolgaParser.parse(xml).expect("parse");
                // `black_box` на результате: гарантия, что парсер
                // действительно выполнился и не был вырезан как
                // dead code.
                criterion::black_box(data);
            })
        });
    }

    group.finish();
}

criterion_group!(benches, bench_volga_parser);
criterion_main!(benches);
