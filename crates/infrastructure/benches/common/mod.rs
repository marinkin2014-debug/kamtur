//! Shared fixtures and helpers for kamtur benchmarks.
//!
//! Cargo treats this directory as a module, not a separate bench.
//! Bench files do `mod common;` and pull items in. `dead_code` is
//! allowed because different benches use different subsets of this
//! module -- without the allow, clippy in `--all-targets` mode would
//! flag every item that happens to be unused in a given compile unit.
#![allow(dead_code)]

// ============================================================
// Synthetic fixture generator
// ============================================================
//
// See crates/infrastructure/benches/parser.rs for the full rationale
// on why fixtures are generated rather than committed as binary data.

/// Пропорции тэгов Volga. Источник — комментарий в `parser.rs`
/// (`estimated_elements`). Синхронизировать при изменении реального
/// формата.
struct Proportions {
    ships: usize,
    decks: usize,
    classes: usize,
    cabins: usize,
    cruises: usize,
    prices: usize,
    spos: usize,
}

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
pub fn generate_volga_xml(target_bytes: usize) -> Vec<u8> {
    let p = Proportions::for_target(target_bytes);

    // Оценка сверху: 200 байт на элемент. Реальные пропорции
    // сгенерируют меньше, но Vec переаллоцируется максимум один раз.
    // Меньше аллокаций в setup = чище baseline.
    let mut xml = Vec::with_capacity(target_bytes + target_bytes / 8);

    xml.extend_from_slice(b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<root>\n");

    // ---- ships ----
    xml.extend_from_slice(b"<ships>\n");
    for i in 0..p.ships {
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
    // Для каждой ship генерируем classes_per_ship classes.
    xml.extend_from_slice(b"<classes>\n");
    let classes_per_ship = (p.classes / p.ships.max(1)).max(1);
    for ship in 1..=p.ships {
        for cls in 0..classes_per_ship {
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
