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
//! - `Fingerprint::of` — это B.3.
//! - `VolgaCanonicalTransformer::transform` — это B.2.
//! - Сетевую загрузку (`VolgaFetcher::fetch_raw`) — нестабильно на
//!   CI, внешний сервер. Отдельно в k6 (B.6).
//!
//! ## Почему синтетические фикстуры
//!
//! Реальный фид Volga — 15-20 МБ, но:
//!
//! - он внешний, меняется по расписанию провайдера → невоспроизводимо;
//! - содержит бизнес-данные (названия, цены) — не место в публичном
//!   репозитории;
//! - 50 МБ бинаря в git — антипаттерн (даже через LFS).
//!
//! Генератор создаёт XML того же формата с теми же пропорциями тэгов.
//! Размер (1/10/50 МБ) покрывает три порядка сложности: smoke,
//! реалистичный прод, стресс.
//!
//! ## Throughput
//!
//! `Throughput::Bytes` включает MB/s в отчёт. Это позволяет:
//!
//! - сравнивать прогоны на разном hardware по одной метрике;
//! - видеть, линейна ли пропускная способность (не должно быть
//!   деградации на 50 МБ из-за аллокаций или кэш-промахов).
//!
//! ## Запуск
//!
//! ```text
//! cargo bench --bench parser
//! cargo bench --bench parser -- 1mb        # только один кейс
//! cargo bench --bench parser -- --quick    # быстрый прогон для CI
//! ```
//!
//! Baseline (заполняется после прогона на конкретной машине):
//! см. docs/performance.md.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use infrastructure::providers::volga_wolga::VolgaParser;

// ============================================================
// Synthetic fixture generator
// ============================================================

/// Эмпирический средний размер сгенерированного XML-элемента, в байтах.
///
/// **Отличается** от `parser.rs::AVG_ELEMENT_SIZE_BYTES` (128): там —
/// оценка сверху для capacity hint, сознательно завышенная. Генератору
/// нужна точечная оценка, чтобы попасть в целевой размер.
///
/// Измерено на текущей схеме:
///
/// ```text
///   1 MB  → ~83 B/elem
///   10 MB → ~86 B/elem
///   50 MB → ~89 B/elem
/// ```
///
/// Средний размер растёт с объёмом: ID получают разряды (ship #2048 —
/// 4 символа, ship #4 — 1). Одна константа даёт ±5% от цели во всём
/// диапазоне — достаточно для bench-фикстуры.
///
/// Обновлять при изменении схемы XML. Проверка — `eprintln!` в
/// `bench_volga_parser`.
const AVG_GENERATED_ELEMENT_BYTES: usize = 86;

struct Proportions {
    ships: usize,
    decks: usize,
    classes: usize,
    cabins: usize,
    cruises: usize,
    prices: usize,
    spos: usize,
}

impl Proportions {
    fn for_target(target_bytes: usize) -> Self {
        let elements = (target_bytes / AVG_GENERATED_ELEMENT_BYTES).max(64);
        Self {
            ships: elements / 200,
            decks: elements / 50,
            classes: elements / 20,
            cabins: elements / 2,
            cruises: elements / 50,
            prices: elements * 3 / 10,
            spos: elements / 20,
        }
    }
}

/// Генерирует синтетический XML-фид Volga, стремясь к `target_bytes`.
///
/// Точный размер может немного отличаться (±10%) из-за целочисленного
/// деления пропорций. Это нормально: бенчмарк работает с реальным
/// размером `bytes.len()`, который печатает в `Throughput`.
///
/// Гарантии:
///
/// - Валидный XML по схеме Volga (все обязательные атрибуты).
/// - Детерминирован: одинаковый `target_bytes` → одинаковые байты.
/// - Все `class_id` соответствуют `<object_id><2-значный_номер>`,
///   как требует `object_id_from_class_id`.
/// - Внутри `<free>` только `cruise_id`-блоки с существующими cabin'ами.
fn generate_volga_xml(target_bytes: usize) -> Vec<u8> {
    let p = Proportions::for_target(target_bytes);

    // Оценка сверху: 200 байт на элемент. Реальные пропорции
    // сгенерируют меньше, но Vec переаллоцируется максимум один раз.
    // Меньше аллокаций в setup = чище baseline.
    let mut xml = Vec::with_capacity(target_bytes + target_bytes / 8);

    xml.extend_from_slice(b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<root>\n");

    // ---- ships ----
    xml.extend_from_slice(b"<ships>\n");
    for i in 0..p.ships {
        // id="1", id="2", ...
        xml.extend_from_slice(b"  <ship id=\"");
        push_usize(&mut xml, i + 1);
        xml.extend_from_slice(b"\" name=\"Ship ");
        push_usize(&mut xml, i + 1);
        xml.extend_from_slice(b"\"/>\n");
    }
    xml.extend_from_slice(b"</ships>\n");

    // ---- decks ----
    xml.extend_from_slice(b"<decks>\n");
    for i in 0..p.decks {
        xml.extend_from_slice(b"  <deck id=\"d");
        push_usize(&mut xml, i + 1);
        xml.extend_from_slice(b"\" name=\"Deck ");
        push_usize(&mut xml, i + 1);
        xml.extend_from_slice(b"\"/>\n");
    }
    xml.extend_from_slice(b"</decks>\n");

    // ---- classes ----
    // class_id = <object_id><2-digit suffix>. object_id — 1..=ships.
    // Для каждой ship генерируем classes/2 classes.
    xml.extend_from_slice(b"<classes>\n");
    let classes_per_ship = (p.classes / p.ships.max(1)).max(1);
    for ship in 1..=p.ships {
        for cls in 0..classes_per_ship {
            // suffix: 01, 02, ... (2 цифры).
            let suffix = (cls % 99) + 1;
            xml.extend_from_slice(b"  <class id=\"");
            push_usize(&mut xml, ship);
            if suffix < 10 {
                xml.push(b'0');
            }
            push_usize(&mut xml, suffix);
            xml.extend_from_slice(b"\" name=\"Class ");
            push_usize(&mut xml, suffix);
            xml.extend_from_slice(b"\" m_count=\"2\" no_full=\"0\"/>\n");
        }
    }
    xml.extend_from_slice(b"</classes>\n");

    // ---- cabins ----
    // cabin id="cabN", ship="1..=ships", class_id="<ship><suffix>",
    // deck="d1".
    xml.extend_from_slice(b"<cabins>\n");
    for i in 0..p.cabins {
        let ship = (i % p.ships.max(1)) + 1;
        let cls_suffix = ((i / p.ships.max(1)) % classes_per_ship) + 1;
        xml.extend_from_slice(b"  <cabin id=\"cab");
        push_usize(&mut xml, i + 1);
        xml.extend_from_slice(b"\" ship=\"");
        push_usize(&mut xml, ship);
        xml.extend_from_slice(b"\" number=\"");
        push_usize(&mut xml, 100 + i);
        xml.extend_from_slice(b"\" class_id=\"");
        push_usize(&mut xml, ship);
        if cls_suffix < 10 {
            xml.push(b'0');
        }
        push_usize(&mut xml, cls_suffix);
        xml.extend_from_slice(b"\" deck=\"d1\"/>\n");
    }
    xml.extend_from_slice(b"</cabins>\n");

    // ---- cruises ----
    xml.extend_from_slice(b"<cruises>\n");
    for i in 0..p.cruises {
        let ship = (i % p.ships.max(1)) + 1;
        xml.extend_from_slice(b"  <cruise id=\"c");
        push_usize(&mut xml, i + 1);
        xml.extend_from_slice(b"\" ship_id=\"");
        push_usize(&mut xml, ship);
        xml.extend_from_slice(b"\" begin_date=\"01.09.2026\" begin_time=\"10:00\"");
        xml.extend_from_slice(b" end_date=\"05.09.2026\" end_time=\"18:00\"");
        xml.extend_from_slice(b" route=\"Perm-Samara\"");
        xml.extend_from_slice(b" child_price=\"5000\" dop_price=\"1000\"/>\n");
    }
    xml.extend_from_slice(b"</cruises>\n");

    // ---- prices ----
    // price привязан к cruise_id и class_id. class_id — существующий.
    xml.extend_from_slice(b"<prices>\n");
    for i in 0..p.prices {
        let cruise = (i % p.cruises.max(1)) + 1;
        let ship = (i % p.ships.max(1)) + 1;
        let cls_suffix = (i % classes_per_ship) + 1;
        xml.extend_from_slice(b"  <price cruise_id=\"c");
        push_usize(&mut xml, cruise);
        xml.extend_from_slice(b"\" class_id=\"");
        push_usize(&mut xml, ship);
        if cls_suffix < 10 {
            xml.push(b'0');
        }
        push_usize(&mut xml, cls_suffix);
        xml.extend_from_slice(b"\" price=\"50000.00\" nofull=\"0\"/>\n");
    }
    xml.extend_from_slice(b"</prices>\n");

    // ---- spos ----
    xml.extend_from_slice(b"<spos>\n");
    for i in 0..p.spos {
        let cruise = (i % p.cruises.max(1)) + 1;
        let ship = (i % p.ships.max(1)) + 1;
        let cls_suffix = (i % classes_per_ship) + 1;
        let cabin = (i % p.cabins.max(1)) + 1;
        xml.extend_from_slice(b"  <spo cruise_id=\"c");
        push_usize(&mut xml, cruise);
        xml.extend_from_slice(b"\" class_id=\"");
        push_usize(&mut xml, ship);
        if cls_suffix < 10 {
            xml.push(b'0');
        }
        push_usize(&mut xml, cls_suffix);
        xml.extend_from_slice(b"\" cabin_id=\"cab");
        push_usize(&mut xml, cabin);
        xml.extend_from_slice(b"\" spo=\"48000.00\" nofull=\"0\"/>\n");
    }
    xml.extend_from_slice(b"</spos>\n");

    // ---- free ----
    // Для каждого cruise — блок с частью cabin'ов, привязанных к нему.
    xml.extend_from_slice(b"<free>\n");
    let free_per_cruise = (p.cabins / p.cruises.max(1)).max(1);
    for cruise in 1..=p.cruises {
        xml.extend_from_slice(b"  <cruise id=\"c");
        push_usize(&mut xml, cruise);
        xml.extend_from_slice(b"\">\n");
        for j in 0..free_per_cruise {
            let cabin = ((cruise - 1) * free_per_cruise + j) % p.cabins.max(1) + 1;
            xml.extend_from_slice(b"    <cabin id=\"cab");
            push_usize(&mut xml, cabin);
            xml.extend_from_slice(b"\"/>\n");
        }
        xml.extend_from_slice(b"  </cruise>\n");
    }
    xml.extend_from_slice(b"</free>\n");

    xml.extend_from_slice(b"</root>\n");
    xml
}

/// Push `n` as ASCII digits. Ровно так, как это делает `write!(..., "{}")`,
/// но без `format!`-аллокаций: генератор вызывается один раз на bench,
/// но чистота кода важнее микрооптимизации здесь — это bench-setup,
/// не измеряемый путь.
#[inline]
fn push_usize(buf: &mut Vec<u8>, n: usize) {
    use std::io::Write;
    // Запись в Vec<u8> не возвращает io::Error — контракт impl Write
    // для Vec<u8> гарантирует отсутствие ошибок. `expect` документирует
    // инвариант (и удовлетворяет clippy, который требует обработки Result).
    write!(buf, "{}", n).expect("write to Vec<u8> never fails");
}

// ============================================================
// Benchmarks
// ============================================================

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
        let xml = generate_volga_xml(*target);

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
                // `criterion::black_box` не нужен на самом входе — `xml`
                // приходит через `bench_with_input`, компилятор видит
                // его как input и не сможет соптимизировать.
                //
                // На выходе — `expect("parse")`: любой сбой генератора
                // (несоответствие схеме) → паника бенчмарка. Это
                // намеренно: фикстура должна быть валидной.
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
