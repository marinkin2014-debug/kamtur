use chrono::NaiveDate;
use rust_decimal::Decimal;

// ============================================================
// Cursor
// ============================================================

/// Позиция в списке круизов для keyset pagination.
///
/// Сортировка в `list_cruises`: `(begin_date ASC, cruise_id ASC)`.
/// Курсор фиксирует последнюю отданную пару — следующий запрос вернёт
/// строки строго после неё.
///
/// ## Формат wire-строки
///
/// `hex("YYYY-MM-DD|<cruise_id>")` — например,
/// `2026-09-10|c2` → `323032362d30392d31307c6332`.
///
/// Почему hex, а не base64:
///
/// - URL-safe (`[0-9a-f]`), не требует экранирования в query.
/// - Не требует новых зависимостей (`hex` уже есть в domain через
///   `fingerprint.rs`).
/// - Opaque для клиента — не парсится руками.
/// - Легко заменить на HMAC-подписанный токен в будущем.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CruiseListCursor {
    pub begin_date: NaiveDate,
    pub cruise_id: String,
}

#[derive(Debug, thiserror::Error)]
pub enum CursorParseError {
    #[error("invalid hex: {0}")]
    Hex(String),
    #[error("invalid utf-8: {0}")]
    Utf8(String),
    #[error("expected `YYYY-MM-DD|<cruise_id>`, got: {0}")]
    Shape(String),
    #[error("invalid date `{0}`: {1}")]
    Date(String, String),
    #[error("empty cruise_id")]
    EmptyCruiseId,
}

impl CruiseListCursor {
    #[inline]
    pub fn new(begin_date: NaiveDate, cruise_id: impl Into<String>) -> Self {
        Self {
            begin_date,
            cruise_id: cruise_id.into(),
        }
    }

    /// Кодирует курсор в wire-формат (`hex("date|id")`).
    pub fn encode(&self) -> String {
        let raw = format!("{}|{}", self.begin_date, self.cruise_id);
        hex::encode(raw.as_bytes())
    }

    /// Декодирует wire-формат обратно в курсор.
    ///
    /// Строгая валидация: hex, utf-8, разделитель, дата, непустой id.
    /// Любая ошибка → `CursorParseError` — API-слой превратит в 400.
    pub fn decode(s: &str) -> Result<Self, CursorParseError> {
        let bytes = hex::decode(s).map_err(|e| CursorParseError::Hex(e.to_string()))?;
        let raw = String::from_utf8(bytes).map_err(|e| CursorParseError::Utf8(e.to_string()))?;

        let (date_s, cruise_id) = raw
            .split_once('|')
            .ok_or_else(|| CursorParseError::Shape(raw.clone()))?;

        if cruise_id.is_empty() {
            return Err(CursorParseError::EmptyCruiseId);
        }

        let begin_date = NaiveDate::parse_from_str(date_s, "%Y-%m-%d")
            .map_err(|e| CursorParseError::Date(date_s.to_string(), e.to_string()))?;

        Ok(Self {
            begin_date,
            cruise_id: cruise_id.to_string(),
        })
    }
}

// ============================================================
// List / Detail views
// ============================================================

#[derive(Debug, Clone)]
pub struct CruiseListItem {
    pub cruise_id: String,
    pub name: String,
    pub ship_name: Option<String>,
    pub begin_date: NaiveDate,
    pub end_date: NaiveDate,
    pub days: Option<i32>,
    pub route: Option<String>,
    pub departure_city: Option<String>,
    pub minimal_price: Option<Decimal>,
    pub room_counts: i32,
    pub is_active: bool,
}

#[derive(Debug, Clone, Default)]
pub struct CruiseListFilter {
    pub cruise_provider_id: Option<String>,
    pub begin_from: Option<NaiveDate>,
    pub begin_to: Option<NaiveDate>,
    pub departure_city: Option<String>,
    pub limit: i64,
    pub cursor: Option<CruiseListCursor>,
}

#[derive(Debug, Clone)]
pub struct CruiseDetail {
    pub cruise_id: String,
    pub name: String,
    pub ship_name: Option<String>,
    pub begin_date: NaiveDate,
    pub begin_time: Option<chrono::NaiveTime>,
    pub end_date: NaiveDate,
    pub end_time: Option<chrono::NaiveTime>,
    pub days: Option<i32>,
    pub route: Option<String>,
    pub departure_city: Option<String>,
    pub city_from: Option<String>,
    pub city_to: Option<String>,
    pub is_return: Option<bool>,
    pub is_weekend: Option<bool>,
    pub is_active: bool,
    pub status: String,
    pub prices: Vec<ClassPriceView>,
    pub rooms: Vec<RoomView>,
}

#[derive(Debug, Clone)]
pub struct ClassPriceView {
    pub class_id: String,
    pub class_name: String,
    pub description: Option<String>,
    pub base_seats: Option<i32>,
    pub tiers: Option<i32>,
    pub partial_buyout: bool,
    pub base_price: Decimal,
    pub child_price: Option<Decimal>,
    pub extra_seat: Option<Decimal>,
    pub currency: String,
}

#[derive(Debug, Clone)]
pub struct RoomView {
    pub room_id: String,
    pub number: String,
    pub class_id: String,
    pub class_name: String,
    pub stage_id: Option<String>,
    pub stage_name: Option<String>,
    pub available: bool,
    pub prices: Vec<RoomPriceView>,
}

#[derive(Debug, Clone)]
pub struct RoomPriceView {
    pub partial_buyout: bool,
    pub base_price: Decimal,
    pub currency: String,
}

// ============================================================
// Tests
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    // ============================================================
    // CruiseListCursor encode/decode
    // ============================================================

    #[test]
    fn cursor_roundtrip() {
        let c = CruiseListCursor::new(date(2026, 9, 10), "c2");
        let encoded = c.encode();
        let decoded = CruiseListCursor::decode(&encoded).expect("decode");
        assert_eq!(decoded, c);
    }

    #[test]
    fn cursor_roundtrip_with_weird_ids() {
        // Даже если cruise_id когда-то будет содержать спецсимволы
        // (кроме `|`, который ломает формат), roundtrip должен работать.
        for id in &["448", "c-1", "abc.def", "id with spaces"] {
            let c = CruiseListCursor::new(date(2026, 12, 31), *id);
            let decoded = CruiseListCursor::decode(&c.encode()).expect("decode");
            assert_eq!(decoded, c, "id = {id:?}");
        }
    }

    #[test]
    fn cursor_encode_is_url_safe_hex() {
        let c = CruiseListCursor::new(date(2026, 9, 10), "c2");
        let encoded = c.encode();
        assert!(
            encoded
                .chars()
                .all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase()),
            "encoded must be lowercase hex: {encoded}",
        );
    }

    #[test]
    fn cursor_decode_rejects_invalid_hex() {
        let err = CruiseListCursor::decode("zzzz").unwrap_err();
        assert!(matches!(err, CursorParseError::Hex(_)), "got {err:?}");
    }

    #[test]
    fn cursor_decode_rejects_no_separator() {
        // hex("no-separator") — валидный hex, но без `|`.
        let bad = hex::encode("no-separator");
        let err = CruiseListCursor::decode(&bad).unwrap_err();
        assert!(matches!(err, CursorParseError::Shape(_)), "got {err:?}");
    }

    #[test]
    fn cursor_decode_rejects_bad_date() {
        let bad = hex::encode("not-a-date|c2");
        let err = CruiseListCursor::decode(&bad).unwrap_err();
        assert!(matches!(err, CursorParseError::Date(_, _)), "got {err:?}");
    }

    #[test]
    fn cursor_decode_rejects_empty_cruise_id() {
        let bad = hex::encode("2026-09-10|");
        let err = CruiseListCursor::decode(&bad).unwrap_err();
        assert!(
            matches!(err, CursorParseError::EmptyCruiseId),
            "got {err:?}"
        );
    }

    #[test]
    fn cursor_decode_rejects_non_utf8() {
        // 0xff 0xfe невалидный utf-8
        let bad = hex::encode([0xffu8, 0xfe]);
        let err = CruiseListCursor::decode(&bad).unwrap_err();
        assert!(matches!(err, CursorParseError::Utf8(_)), "got {err:?}");
    }
}
