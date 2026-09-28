use std::collections::HashMap;
use std::str::FromStr;

use chrono::NaiveDate;

use domain::entities::*;
use domain::errors::ProviderError;

use super::raw::*;

#[derive(Clone, Default)]
pub struct VolgaCanonicalTransformer {
    pub cruise_type_id: i64,
}

impl VolgaCanonicalTransformer {
    pub fn new(cruise_type_id: i64) -> Self {
        Self { cruise_type_id }
    }

    pub fn transform(&self, raw: RawData) -> Result<CanonicalData, ProviderError> {
        let mut data = CanonicalData::default();

        for s in raw.ships {
            data.objects.push(CanonicalObject {
                external_id: s.id,
                name: s.name,
            });
        }
        for d in raw.decks {
            data.stages.push(CanonicalStage {
                external_id: d.id,
                name: d.name,
            });
        }
        for c in raw.classes {
            let Some(external_object_id) = object_id_from_class_id(&c.id) else {
                tracing::warn!(class_id = %c.id, "class_id too short, skipping");
                continue;
            };
            data.classes.push(CanonicalClass {
                external_object_id,
                external_class_id: c.id,
                name: c.name,
                description: c.comment,
                base_seats: c.m_count,
                tiers: c.r_count,
                partial_buyout: c.no_full,
            });
        }
        for r in raw.cabins {
            data.rooms.push(CanonicalRoom {
                external_object_id: r.ship,
                external_stage_id: r.deck,
                external_class_id: r.class_id,
                external_room_id: r.id,
                number: r.number,
            });
        }

        let mut cruise_extra: HashMap<
            String,
            (Option<rust_decimal::Decimal>, Option<rust_decimal::Decimal>),
        > = HashMap::with_capacity(raw.cruises.len());

        for c in raw.cruises {
            let begin_date = parse_date(&c.begin_date)?;
            let end_date = parse_date(&c.end_date)?;
            let begin_time = c
                .begin_time
                .as_deref()
                .and_then(|s| chrono::NaiveTime::from_str(s).ok());
            let end_time = c
                .end_time
                .as_deref()
                .and_then(|s| chrono::NaiveTime::from_str(s).ok());

            cruise_extra.insert(c.id.clone(), (c.child_price, c.dop_price));

            data.tours.push(CanonicalTour {
                external_object_id: c.ship_id,
                external_cruise_id: c.id,
                cruise_type_id: self.cruise_type_id,
                begin_date,
                begin_time,
                end_date,
                end_time,
                name: c.route,
            });
        }

        for p in raw.prices {
            let (child_price, extra_seat) = cruise_extra
                .get(&p.cruise_id)
                .copied()
                .unwrap_or((None, None));
            data.prices.push(CanonicalPrice {
                external_cruise_id: p.cruise_id,
                external_class_id: p.class_id,
                base_price: p.price,
                partial_buyout: p.nofull,
                child_price,
                extra_seat,
                currency: "RUB".into(),
            });
        }

        for s in raw.spos {
            data.sales.push(CanonicalSale {
                external_cruise_id: s.cruise_id,
                external_class_id: s.class_id,
                external_room_id: s.cabin_id,
                base_price: s.spo,
                partial_buyout: s.nofull,
                currency: "RUB".into(),
            });
        }

        for f in raw.free {
            data.availability.push(CanonicalAvailability {
                external_cruise_id: f.cruise_id,
                external_room_id: f.cabin_id,
                available: true, // связка есть → доступна
            });
        }

        Ok(data)
    }
}

/// Извлекает `object_id` (id корабля) из `class.id` формата Volga.
///
/// Формат Volga: `<object_id><2-значный_номер_класса>`.
/// Класс всегда оканчивается ровно двумя цифрами; всё, что до них —
/// это `object_id`.
///
/// Примеры из реального фида:
/// ```text
///   "101"   -> "1"    (корабль 1,   класс 01)
///   "1801"  -> "18"   (корабль 18,  класс 01)
///   "19056" -> "190"  (корабль 190, класс 56)
/// ```
///
/// Возвращает `None`, если строка короче 3 символов — `object_id`
/// не может быть пустым.
#[inline]
fn object_id_from_class_id(class_id: &str) -> Option<String> {
    let mut it = class_id.char_indices().rev();
    it.next()?; // skip последний символ (2-я цифра номера класса)
    it.next()?; // skip предпоследний (1-я цифра номера класса)
    let (idx, c) = it.next()?; // последний символ object_id
    let end = idx + c.len_utf8();
    Some(class_id[..end].to_string())
}

#[inline]
fn parse_date(s: &str) -> Result<NaiveDate, ProviderError> {
    NaiveDate::parse_from_str(s, "%d.%m.%Y")
        .map_err(|e| ProviderError::Parse(format!("date `{s}`: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    // ============================================================
    // object_id_from_class_id — unit
    // ============================================================

    /// Зафиксировано по реальному фиду Volga:
    /// class id = <object_id><2-значный_номер_класса>.
    #[test]
    fn object_id_from_class_id_real_volga_examples() {
        assert_eq!(object_id_from_class_id("101").as_deref(), Some("1"));
        assert_eq!(object_id_from_class_id("1801").as_deref(), Some("18"));
        assert_eq!(object_id_from_class_id("19056").as_deref(), Some("190"));
    }

    #[test]
    fn object_id_from_class_id_returns_none_for_short_input() {
        assert_eq!(object_id_from_class_id(""), None);
        assert_eq!(object_id_from_class_id("1"), None);
        assert_eq!(object_id_from_class_id("10"), None);
    }

    #[test]
    fn object_id_from_class_id_minimum_valid_length() {
        // 3 символа — минимальный валидный ввод: object_id из 1 цифры + класс из 2
        assert_eq!(object_id_from_class_id("101").as_deref(), Some("1"));
    }

    #[test]
    fn object_id_from_class_id_multichar_utf8_tail() {
        // Страховка: если когда-нибудь в фид попадёт UTF-8 в object_id,
        // не должны паниковать на границе char.
        assert_eq!(
            object_id_from_class_id("корабль-α01").as_deref(),
            Some("корабль-α")
        );
    }

    // ============================================================
    // transform — интеграция внутри модуля
    // ============================================================

    /// Фикстура с кириллицей в атрибутах XML. Обычная raw-строка,
    /// не byte-строка — иначе компилятор требует ASCII-only.
    const FIXTURE_CLASSES: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<root>
    <ships>
        <ship id="1"   name="Ship One"/>
        <ship id="18"  name="Ship Eighteen"/>
        <ship id="190" name="Ship One Ninety"/>
    </ships>
    <decks><deck id="d1" name="Main"/></decks>
    <classes>
        <class id="101"   name="ЛЮКС(2)"  m_count="2" r_count="1" no_full="0"/>
        <class id="1801"  name="Стандарт" m_count="4" no_full="0"/>
        <class id="19056" name="Эконом"   m_count="4" no_full="1"/>
    </classes>
    <cabins/><cruises/><prices/><spos/><free/>
</root>"#;

    /// Регрессия: `external_object_id` каждого класса должен совпадать
    /// с `id` какого-то `<ship>` из того же фида.
    #[test]
    fn class_object_id_matches_ship_id() {
        let raw = crate::providers::volga_wolga::VolgaParser
            .parse(FIXTURE_CLASSES.as_bytes())
            .expect("parse");
        let transformed = VolgaCanonicalTransformer::new(1)
            .transform(raw)
            .expect("transform");

        let ship_ids: HashSet<&str> = transformed
            .objects
            .iter()
            .map(|o| o.external_id.as_str())
            .collect();

        assert_eq!(transformed.classes.len(), 3);
        for class in &transformed.classes {
            assert!(
                ship_ids.contains(class.external_object_id.as_str()),
                "class `{}` ссылается на несуществующий object_id `{}`. Ships: {:?}",
                class.external_class_id,
                class.external_object_id,
                ship_ids,
            );
        }
    }

    /// Слишком короткий class_id — пропускается с warn, не паникует,
    /// остальные классы обрабатываются нормально.
    #[test]
    fn transform_skips_class_with_short_id() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<root>
    <ships><ship id="1" name="S"/></ships>
    <decks/>
    <classes>
        <class id="101" name="ok"/>
        <class id="10"  name="too short"/>
    </classes>
    <cabins/><cruises/><prices/><spos/><free/>
</root>"#;

        let raw = crate::providers::volga_wolga::VolgaParser
            .parse(xml.as_bytes())
            .expect("parse");
        let transformed = VolgaCanonicalTransformer::new(1)
            .transform(raw)
            .expect("transform");

        assert_eq!(transformed.classes.len(), 1);
        assert_eq!(transformed.classes[0].external_class_id, "101");
        assert_eq!(transformed.classes[0].external_object_id, "1");
    }
}
