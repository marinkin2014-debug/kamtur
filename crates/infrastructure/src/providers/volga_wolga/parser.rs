use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use quick_xml::XmlVersion;
use rust_decimal::Decimal;
use std::str::FromStr;
use tracing::warn;

use super::raw::*;
use domain::errors::ProviderError;

// ============================================================
// Capacity-эвристика
// ============================================================

/// Средний размер одного XML-элемента (start-tag с атрибутами), в байтах.
///
/// Оценка по реальным фидам Volga:
///
/// ```text
///   <cabin id="cab1" ship="100" number="101" class_id="c10" deck="d1"/>   ~70 B
///   <price cruise_id="448" class_id="c10" price="50000.00" nofull="0"/>   ~70 B
///   <cruise id="448" ship_id="100" begin_date="01.09.2026"
///           begin_time="10:00" end_date="05.09.2026" end_time="18:00"
///           route="Perm-Samara" child_price="5000" dop_price="1000"/>     ~200 B
///   <ship id="100" name="Ship A"/>                                        ~30 B
///   <spo cruise_id="448" class_id="c10" cabin_id="cab1"
///        spo="45000.00" nofull="0"/>                                      ~80 B
/// ```
///
/// Средневзвешенное (cabins и prices доминируют по количеству) — 100–150
/// байт. Округляем вниз до 128: безопасная оценка сверху, в бинарных
/// системах удобно, переоценка ×1.5 не критична.
///
/// ## Зачем вообще считать
///
/// `Vec::with_capacity` без hint даёт амортизированный рост с
/// логарифмическим числом перевыделений и ~2N копированиями элементов.
/// Для 50 МБ XML с ~200k cabins это ~10–15 перевыделений и копирование
/// десятков МБ памяти. Hint убирает перевыделения для типичных фидов.
///
/// ## Почему hint не должен быть точным
///
/// Точные доли элементов меняются от провайдера к провайдеру, и
/// «правильные» ratios устаревают. Hint — защита от фрагментации
/// аллокаций, не точная оценка. Переоценка capacity ×2 даёт лишние
/// мегабайты на 50 МБ фиде; недооценка даёт пару перевыделений.
/// Первое безопаснее второго, поэтому ratios — с запасом сверху.
const AVG_ELEMENT_SIZE_BYTES: usize = 128;

/// Минимальная начальная hint, чтобы крошечный XML (тестовая фикстура
/// на 1–2 КБ) не аллоцировал capacity меньше реального количества.
const MIN_ELEMENT_HINT: usize = 64;

/// Возвращает оценку общего числа элементов в документе.
///
/// Значение — надёжная верхняя граница: если XML плоский и средний
/// элемент ≤128 байт, реальное число ≤ hint. Не используется как точное.
#[inline]
fn estimated_elements(bytes_len: usize) -> usize {
    (bytes_len / AVG_ELEMENT_SIZE_BYTES).max(MIN_ELEMENT_HINT)
}

// ============================================================
// Parser
// ============================================================

#[derive(Clone, Default)]
pub struct VolgaParser;

#[derive(PartialEq)]
enum Ctx {
    None,
    Prices,
    Spos,
    Free,
}

impl VolgaParser {
    /// Парсит XML Volga в `RawData`.
    ///
    /// ## Стратегия capacity
    ///
    /// `hint` — грубая оценка числа элементов (см. `AVG_ELEMENT_SIZE_BYTES`).
    /// Ratios для каждого типа Vec:
    ///
    /// ```text
    ///   ships   : hint / 200   (~0.5% от общего числа)  — 1 корабль на 200 тегов
    ///   decks   : hint / 50    (~2%)
    ///   classes : hint / 20    (~5%)
    ///   cabins  : hint / 2     (~50%)
    ///   cruises : hint / 50    (~2%)
    ///   prices  : hint * 3 / 10 (~30%)
    ///   spos    : hint / 20    (~5%)
    ///   free    : hint / 2     (~50%, пересекается с cabins)
    /// ```
    ///
    /// Ratios — эмпирика по Volga. Для маленького фида (≤ `MIN_ELEMENT_HINT`)
    /// hint одинаков, и все Vec'ы аллоцируют 2–32 элемента — это копейки.
    ///
    /// ## Парсинг
    ///
    /// Состояние (`ctx`) переключается на `<prices>`, `<spos>`, `<free>`.
    /// Внутри `<free>` дополнительно трекаем `current_free_cruise` — id
    /// круиза, к которому относятся вложенные `<cabin>`.
    pub fn parse(&self, bytes: &[u8]) -> Result<RawData, ProviderError> {
        let hint = estimated_elements(bytes.len());

        let mut raw = RawData {
            ships: Vec::with_capacity(hint / 200),
            decks: Vec::with_capacity(hint / 50),
            classes: Vec::with_capacity(hint / 20),
            cabins: Vec::with_capacity(hint / 2),
            cruises: Vec::with_capacity(hint / 50),
            prices: Vec::with_capacity(hint * 3 / 10),
            spos: Vec::with_capacity(hint / 20),
            free: Vec::with_capacity(hint / 2),
        };

        let mut reader = Reader::from_reader(bytes);
        reader.config_mut().trim_text(true);
        let mut buf = Vec::with_capacity(8192);

        let mut ctx = Ctx::None;
        let mut current_free_cruise: Option<String> = None;

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(e)) => match e.name().as_ref() {
                    b"prices" => ctx = Ctx::Prices,
                    b"spos" => ctx = Ctx::Spos,
                    b"free" => ctx = Ctx::Free,
                    // <cruise id="448"> внутри <free> — начало блока кают
                    b"cruise" if ctx == Ctx::Free => {
                        current_free_cruise = attr(&e, b"id");
                    }
                    _ => {}
                },
                Ok(Event::End(e)) => match e.name().as_ref() {
                    b"prices" | b"spos" | b"free" => {
                        ctx = Ctx::None;
                        current_free_cruise = None;
                    }
                    b"cruise" if ctx == Ctx::Free => {
                        current_free_cruise = None;
                    }
                    _ => {}
                },
                Ok(Event::Empty(e)) => {
                    let name = e.name();
                    match (ctx == Ctx::Free, name.as_ref()) {
                        // ============ Обычные теги (вне <free>) ============
                        (false, b"ship") => {
                            if let Some(id) = attr(&e, b"id") {
                                raw.ships.push(RawShip {
                                    id,
                                    name: attr(&e, b"name").unwrap_or_default(),
                                });
                            }
                        }
                        (false, b"deck") => {
                            if let Some(id) = attr(&e, b"id") {
                                raw.decks.push(RawDeck {
                                    id,
                                    name: attr(&e, b"name").unwrap_or_default(),
                                });
                            }
                        }
                        (false, b"class") => {
                            if let Some(id) = attr(&e, b"id") {
                                raw.classes.push(RawClass {
                                    id,
                                    name: attr(&e, b"name").unwrap_or_default(),
                                    comment: attr(&e, b"comment"),
                                    m_count: attr(&e, b"m_count").and_then(|s| s.parse().ok()),
                                    r_count: attr(&e, b"r_count").and_then(|s| s.parse().ok()),
                                    no_full: parse_bool(attr(&e, b"no_full").as_deref()),
                                });
                            }
                        }
                        (false, b"cabin") => {
                            if let Some(id) = attr(&e, b"id") {
                                raw.cabins.push(RawCabin {
                                    id,
                                    ship: attr(&e, b"ship").unwrap_or_default(),
                                    number: attr(&e, b"number").unwrap_or_default(),
                                    class_id: attr(&e, b"class_id").unwrap_or_default(),
                                    deck: attr(&e, b"deck").unwrap_or_default(),
                                });
                            }
                        }
                        (false, b"cruise") if ctx == Ctx::None => {
                            if let Some(id) = attr(&e, b"id") {
                                raw.cruises.push(RawCruise {
                                    id,
                                    ship_id: attr(&e, b"ship_id").unwrap_or_default(),
                                    begin_date: attr(&e, b"begin_date").unwrap_or_default(),
                                    begin_time: attr(&e, b"begin_time"),
                                    end_date: attr(&e, b"end_date").unwrap_or_default(),
                                    end_time: attr(&e, b"end_time"),
                                    route: attr(&e, b"route").unwrap_or_default(),
                                    child_price: parse_decimal(attr(&e, b"child_price").as_deref()),
                                    dop_price: parse_decimal(attr(&e, b"dop_price").as_deref()),
                                });
                            }
                        }
                        (false, b"price") if ctx == Ctx::Prices => {
                            if let (Some(cruise_id), Some(class_id), Some(price_s)) = (
                                attr(&e, b"cruise_id"),
                                attr(&e, b"class_id"),
                                attr(&e, b"price"),
                            ) {
                                match Decimal::from_str(&price_s) {
                                    Ok(price) => raw.prices.push(RawPrice {
                                        cruise_id,
                                        class_id,
                                        price,
                                        nofull: parse_bool(attr(&e, b"nofull").as_deref()),
                                    }),
                                    Err(e) => {
                                        warn!(price = %price_s, error = %e, "invalid price, skipping")
                                    }
                                }
                            }
                        }
                        (false, b"spo") if ctx == Ctx::Spos => {
                            if let (Some(cruise_id), Some(class_id), Some(cabin_id), Some(spo_s)) = (
                                attr(&e, b"cruise_id"),
                                attr(&e, b"class_id"),
                                attr(&e, b"cabin_id"),
                                attr(&e, b"spo"),
                            ) {
                                match Decimal::from_str(&spo_s) {
                                    Ok(spo) => raw.spos.push(RawSpo {
                                        cruise_id,
                                        class_id,
                                        cabin_id,
                                        spo,
                                        nofull: parse_bool(attr(&e, b"nofull").as_deref()),
                                    }),
                                    Err(e) => {
                                        warn!(spo = %spo_s, error = %e, "invalid spo, skipping")
                                    }
                                }
                            }
                        }
                        // ============ Внутри <free> ============
                        // Самозакрывающийся <cruise id="X"/> — редкость, но поддержим
                        (true, b"cruise") => {
                            current_free_cruise = attr(&e, b"id");
                        }
                        (true, b"cabin") => {
                            if let (Some(cruise_id), Some(cabin_id)) =
                                (current_free_cruise.as_ref(), attr(&e, b"id"))
                            {
                                // Атрибут free="0"/"1" — легаси, игнорируем.
                                // Факт наличия связки (cruise_id, cabin_id) в <free> означает,
                                // что каюта доступна. Если связка не пришла — каюта недоступна
                                // (это обрабатывается в upsert_availability на стороне БД).
                                raw.free.push(RawFree {
                                    cruise_id: cruise_id.clone(),
                                    cabin_id,
                                });
                            }
                        }
                        _ => {}
                    }
                }
                Ok(Event::Eof) => break,
                Err(e) => return Err(ProviderError::Parse(e.to_string())),
                _ => {}
            }
            buf.clear();
        }

        Ok(raw)
    }
}

#[inline]
fn attr(e: &BytesStart<'_>, key: &[u8]) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == key)
        .and_then(|a| {
            a.normalized_value(XmlVersion::Explicit1_0)
                .ok()
                .map(|v| v.into_owned())
        })
}

#[inline]
fn parse_bool(s: Option<&str>) -> Option<bool> {
    match s? {
        "1" | "true" | "yes" => Some(true),
        "0" | "false" | "no" => Some(false),
        _ => None,
    }
}

#[inline]
fn parse_decimal(s: Option<&str>) -> Option<Decimal> {
    let s = s?.trim();
    if s.is_empty() {
        return None;
    }
    Decimal::from_str(s).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?>
<root>
    <ships>
        <ship id="100" name="Ship A"/>
        <ship id="200" name="Ship B"/>
    </ships>
    <decks>
        <deck id="d1" name="Deck 1"/>
    </decks>
    <classes>
        <class id="c10" name="Class 10" comment="Lux" m_count="2" r_count="1" no_full="1"/>
        <class id="c20" name="Class 20" m_count="4" no_full="0"/>
    </classes>
    <cabins>
        <cabin id="cab1" ship="100" number="101" class_id="c10" deck="d1"/>
        <cabin id="cab2" ship="100" number="102" class_id="c20" deck="d1"/>
    </cabins>
    <cruises>
        <cruise id="448" ship_id="100" begin_date="01.09.2026" begin_time="10:00"
                end_date="05.09.2026" end_time="18:00" route="Perm-Samara"
                child_price="5000" dop_price="1000"/>
    </cruises>
    <prices>
        <price cruise_id="448" class_id="c10" price="50000.00" nofull="0"/>
        <price cruise_id="448" class_id="c20" price="30000.00" nofull="1"/>
    </prices>
    <spos>
        <spo cruise_id="448" class_id="c10" cabin_id="cab1" spo="45000.00" nofull="0"/>
    </spos>
    <free>
        <cruise id="448">
            <cabin id="cab1"/>
            <cabin id="cab2"/>
        </cruise>
    </free>
</root>"#;

    fn parsed() -> RawData {
        VolgaParser.parse(FIXTURE).expect("parse fixture")
    }

    // ============================================================
    // Capacity-эвристика: константы
    // ============================================================

    #[test]
    fn estimated_elements_never_below_min() {
        // Для пустого и крошечного XML — MIN_ELEMENT_HINT.
        assert_eq!(estimated_elements(0), MIN_ELEMENT_HINT);
        assert_eq!(estimated_elements(10), MIN_ELEMENT_HINT);
    }

    #[test]
    fn estimated_elements_scales_with_size() {
        // Для 1 МБ — 1_048_576 / 128 = 8192.
        assert_eq!(estimated_elements(1024 * 1024), 8192);
    }

    #[test]
    fn avg_element_size_is_reasonable() {
        // Защита от случайного изменения константы. Типичный XML-элемент
        // 30–200 байт; средневзвешенное должно быть в этом диапазоне.
        assert!(
            (30..=300).contains(&AVG_ELEMENT_SIZE_BYTES),
            "AVG_ELEMENT_SIZE_BYTES={AVG_ELEMENT_SIZE_BYTES} вне диапазона 30..=300",
        );
    }

    // ============================================================
    // Функциональные тесты парсера (без изменений)
    // ============================================================

    #[test]
    fn parses_ships() {
        let raw = parsed();
        assert_eq!(raw.ships.len(), 2);
        assert_eq!(raw.ships[0].id, "100");
        assert_eq!(raw.ships[0].name, "Ship A");
        assert_eq!(raw.ships[1].id, "200");
    }

    #[test]
    fn parses_decks() {
        let raw = parsed();
        assert_eq!(raw.decks.len(), 1);
        assert_eq!(raw.decks[0].id, "d1");
        assert_eq!(raw.decks[0].name, "Deck 1");
    }

    #[test]
    fn parses_classes() {
        let raw = parsed();
        assert_eq!(raw.classes.len(), 2);
        let c10 = &raw.classes[0];
        assert_eq!(c10.id, "c10");
        assert_eq!(c10.m_count, Some(2));
        assert_eq!(c10.r_count, Some(1));
        assert_eq!(c10.no_full, Some(true));
        let c20 = &raw.classes[1];
        assert_eq!(c20.comment, None);
        assert_eq!(c20.no_full, Some(false));
    }

    #[test]
    fn parses_cabins() {
        let raw = parsed();
        assert_eq!(raw.cabins.len(), 2);
        assert_eq!(raw.cabins[0].id, "cab1");
        assert_eq!(raw.cabins[0].ship, "100");
        assert_eq!(raw.cabins[0].class_id, "c10");
        assert_eq!(raw.cabins[0].deck, "d1");
    }

    #[test]
    fn parses_cruises_with_dates_and_decimals() {
        let raw = parsed();
        assert_eq!(raw.cruises.len(), 1);
        let c = &raw.cruises[0];
        assert_eq!(c.id, "448");
        assert_eq!(c.ship_id, "100");
        assert_eq!(c.begin_date, "01.09.2026");
        assert_eq!(c.begin_time.as_deref(), Some("10:00"));
        assert_eq!(c.end_date, "05.09.2026");
        assert_eq!(c.route, "Perm-Samara");
        assert_eq!(c.child_price.map(|d| d.to_string()), Some("5000".into()));
    }

    #[test]
    fn parses_prices_with_nofull() {
        let raw = parsed();
        assert_eq!(raw.prices.len(), 2);
        assert_eq!(raw.prices[0].cruise_id, "448");
        assert_eq!(raw.prices[0].class_id, "c10");
        assert_eq!(raw.prices[0].nofull, Some(false));
        assert_eq!(raw.prices[1].nofull, Some(true));
    }

    #[test]
    fn parses_spos() {
        let raw = parsed();
        assert_eq!(raw.spos.len(), 1);
        assert_eq!(raw.spos[0].cabin_id, "cab1");
        assert_eq!(raw.spos[0].spo.to_string(), "45000.00");
    }

    #[test]
    fn parses_free_only_inside_free_block() {
        let raw = parsed();
        assert_eq!(raw.free.len(), 2);
        assert_eq!(raw.free[0].cruise_id, "448");
        assert_eq!(raw.free[0].cabin_id, "cab1");
        assert_eq!(raw.free[1].cabin_id, "cab2");
    }

    #[test]
    fn malformed_xml_returns_error() {
        // Mismatched closing tag. quick-xml с `check_end_names = true`
        // (default) возвращает Err(EndEventMismatch) — это гарантированный
        // путь через ветку Err(e) в парсере.
        let bad = b"<root></wrong>";
        assert!(VolgaParser.parse(bad).is_err());
    }
}
