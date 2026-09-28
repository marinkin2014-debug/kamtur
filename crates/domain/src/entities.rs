use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use rust_decimal::Decimal;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProviderId(pub String);

pub type ObservedAt = DateTime<Utc>;

// ============================================================
// Canonical
// ============================================================

#[derive(Debug, Clone)]
pub struct CanonicalObject {
    pub external_id: String,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct CanonicalStage {
    pub external_id: String,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct CanonicalClass {
    pub external_object_id: String,
    pub external_class_id: String,
    pub name: String,
    pub description: Option<String>,
    pub base_seats: Option<i32>,
    pub tiers: Option<i32>,
    pub partial_buyout: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct CanonicalRoom {
    pub external_object_id: String,
    pub external_stage_id: String,
    pub external_class_id: String,
    pub external_room_id: String,
    pub number: String,
}

#[derive(Debug, Clone)]
pub struct CanonicalTour {
    pub external_object_id: String,
    pub external_cruise_id: String,
    pub cruise_type_id: i64,
    pub begin_date: NaiveDate,
    pub begin_time: Option<NaiveTime>,
    pub end_date: NaiveDate,
    pub end_time: Option<NaiveTime>,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct CanonicalPrice {
    pub external_cruise_id: String,
    pub external_class_id: String,
    pub base_price: Decimal,
    pub partial_buyout: Option<bool>,
    pub child_price: Option<Decimal>,
    pub extra_seat: Option<Decimal>,
    pub currency: String,
}

#[derive(Debug, Clone)]
pub struct CanonicalSale {
    pub external_cruise_id: String,
    pub external_class_id: String,
    pub external_room_id: String,
    pub base_price: Decimal,
    pub partial_buyout: Option<bool>,
    pub currency: String,
}

#[derive(Debug, Clone)]
pub struct CanonicalAvailability {
    pub external_cruise_id: String,
    pub external_room_id: String,
    pub available: bool,
}

#[derive(Debug, Clone, Default)]
pub struct CanonicalData {
    pub objects: Vec<CanonicalObject>,
    pub stages: Vec<CanonicalStage>,
    pub classes: Vec<CanonicalClass>,
    pub rooms: Vec<CanonicalRoom>,
    pub tours: Vec<CanonicalTour>,
    pub prices: Vec<CanonicalPrice>,
    pub sales: Vec<CanonicalSale>,
    pub availability: Vec<CanonicalAvailability>,
}

// ============================================================
// Enriched
// ============================================================

#[derive(Debug, Clone, Default)]
pub struct EnrichedTour {
    pub external_cruise_id: String,
    pub fields: std::collections::HashMap<String, String>,
}

impl EnrichedTour {
    pub fn set(&mut self, field: &str, value: String) {
        self.fields.insert(field.to_string(), value);
    }

    pub fn get(&self, field: &str) -> Option<&str> {
        self.fields.get(field).map(|s| s.as_str())
    }
}

// ============================================================
// SyncOutcome
// ============================================================

#[derive(Debug, Default, Clone)]
pub struct SyncOutcome {
    pub duplicate: bool,
    pub objects_upserted: usize,
    pub stages_upserted: usize,
    pub classes_upserted: usize,
    pub rooms_upserted: usize,
    pub tours_upserted: usize,
    pub prices_created: usize,
    pub prices_updated: usize,
    pub prices_closed: usize,
    pub sales_created: usize,
    pub sales_updated: usize,
    pub sales_closed: usize,
    pub availability_created: usize,
    pub availability_updated: usize,
    pub availability_closed: usize,
    pub deactivated: usize,
}

#[derive(Debug, Default, Clone)]
pub struct RetentionOutcome {
    pub raw_snapshots_deleted: u64,
    pub health_checks_deleted: u64,
    pub errors_deleted: u64,
}

// ============================================================
// Метрики — enum'ы для лейблов (bounded cardinality)
// ============================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SyncPhase {
    Fetch,
    Hash,
    Dedup,
    Parse,
    Enrich,
    Persist,
}

impl SyncPhase {
    #[inline]
    pub fn as_str(&self) -> &'static str {
        match self {
            SyncPhase::Fetch => "fetch",
            SyncPhase::Hash => "hash",
            SyncPhase::Dedup => "dedup",
            SyncPhase::Parse => "parse",
            SyncPhase::Enrich => "enrich",
            SyncPhase::Persist => "persist",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SyncStatus {
    Success,
    Failed,
    Skipped,
}

impl SyncStatus {
    #[inline]
    pub fn as_str(&self) -> &'static str {
        match self {
            SyncStatus::Success => "success",
            SyncStatus::Failed => "failed",
            SyncStatus::Skipped => "skipped",
        }
    }
}

/// Таблица/категория записи для метрики `kamtur_rows_written_total{table=...}`.
///
/// Разделение Created / Updated / Closed отражает SCD2-семантику:
///
/// - `*Created` — открыта новая версия.
/// - `*Updated` — старая версия закрыта, новая открыта (значение изменилось).
/// - `*Closed`  — старая версия закрыта, новой нет (ключ исчез).
///
/// Значения `as_str()` стабильны — это Prometheus label и строки в БД
/// дашбордов. Менять только через миграцию.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WriteTable {
    Objects,
    Stages,
    Classes,
    Rooms,
    Tours,
    PricesCreated,
    PricesUpdated,
    PricesClosed,
    SalesCreated,
    SalesUpdated,
    SalesClosed,
    AvailabilityCreated,
    AvailabilityUpdated,
    AvailabilityClosed,
}

impl WriteTable {
    #[inline]
    pub fn as_str(&self) -> &'static str {
        match self {
            WriteTable::Objects => "objects",
            WriteTable::Stages => "stages",
            WriteTable::Classes => "classes",
            WriteTable::Rooms => "rooms",
            WriteTable::Tours => "tours",
            WriteTable::PricesCreated => "prices_created",
            WriteTable::PricesUpdated => "prices_updated",
            WriteTable::PricesClosed => "prices_closed",
            WriteTable::SalesCreated => "sales_created",
            WriteTable::SalesUpdated => "sales_updated",
            WriteTable::SalesClosed => "sales_closed",
            WriteTable::AvailabilityCreated => "availability_created",
            WriteTable::AvailabilityUpdated => "availability_updated",
            WriteTable::AvailabilityClosed => "availability_closed",
        }
    }
}

/// Стадия sync-пайплайна для метрики `kamtur_sync_errors_total{stage=...}`
/// и колонки `errors.stage`.
///
/// Порядок вариантов отражает фазы пайплайна (см. `sync_inner`):
///
/// ```text
///   1. Fetch   — fetch_raw() (network, circuit breaker)
///   2. Dedup   — has_snapshot() (проверка, не видели ли этот fingerprint)
///   3. Parse   — spawn_blocking(parse + transform)
///   4. Enrich  — load_enrichment_rules() + enrich_tours()
///   5. Persist — apply_sync() (транзакция)
///   6. Notify  — зарезервировано (notifier вынесен из sync-цепочки)
///   7. Unknown — всё, что не удалось классифицировать
/// ```
///
/// Значения `as_str()` стабильны — это Prometheus label и строки в БД.
/// Менять их можно только через миграцию метрик/данных.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SyncStage {
    Fetch,
    Dedup,
    Parse,
    Enrich,
    Persist,
    Notify,
    Unknown,
}

impl SyncStage {
    #[inline]
    pub fn as_str(&self) -> &'static str {
        match self {
            SyncStage::Fetch => "fetch",
            SyncStage::Dedup => "dedup",
            SyncStage::Parse => "parse",
            SyncStage::Enrich => "enrich",
            SyncStage::Persist => "persist",
            SyncStage::Notify => "notify",
            SyncStage::Unknown => "unknown",
        }
    }
}

// ============================================================
// ErrorDigestEntry — агрегированная запись для дайджеста ошибок
// ============================================================

#[derive(Debug, Clone)]
pub struct ErrorDigestEntry {
    pub stage: String,
    pub severity: String,
    pub message_prefix: String,
    pub count: i64,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub sample_message: String,
}

// ============================================================
// ClaimedErrorBatch — результат claim-фазы two-phase digest'а
// ============================================================

/// Батч ошибок, атомарно забранный на отправку.
///
/// `entries` — агрегированные группы для тела письма.
/// `claimed_ids` — плоский список id заклеймленных строк. Вызывающий
/// обязан ack/nack-нуть их через `ErrorNotificationRepository::mark_sent`
/// или `::mark_failed`.
#[derive(Debug, Clone)]
pub struct ClaimedErrorBatch {
    pub entries: Vec<ErrorDigestEntry>,
    pub claimed_ids: Vec<i64>,
}

impl ClaimedErrorBatch {
    pub fn empty() -> Self {
        Self {
            entries: Vec::new(),
            claimed_ids: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.claimed_ids.is_empty()
    }

    pub fn total_events(&self) -> i64 {
        self.entries.iter().map(|e| e.count).sum()
    }
}

// ============================================================
// Tests
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// Все 14 вариантов `WriteTable` перечислены явно, `as_str()`
    /// возвращает уникальные строки. Защита от копипаста при добавлении
    /// новых вариантов — если забыть обновить match, компилятор поймает,
    /// если продублировать строку — этот тест.
    #[test]
    fn write_table_as_str_values_are_unique() {
        let all = [
            WriteTable::Objects,
            WriteTable::Stages,
            WriteTable::Classes,
            WriteTable::Rooms,
            WriteTable::Tours,
            WriteTable::PricesCreated,
            WriteTable::PricesUpdated,
            WriteTable::PricesClosed,
            WriteTable::SalesCreated,
            WriteTable::SalesUpdated,
            WriteTable::SalesClosed,
            WriteTable::AvailabilityCreated,
            WriteTable::AvailabilityUpdated,
            WriteTable::AvailabilityClosed,
        ];

        let mut seen = HashSet::new();
        for t in &all {
            assert!(
                seen.insert(t.as_str()),
                "duplicate as_str: {} ({:?})",
                t.as_str(),
                t
            );
        }
        assert_eq!(seen.len(), 14, "ровно 14 уникальных label-значений");
    }

    /// Зафиксировано: `*Closed` варианты уходят в Prometheus label
    /// `table` и в дашборд «Rows written (per second)».
    #[test]
    fn write_table_closed_variants_as_str() {
        assert_eq!(WriteTable::PricesClosed.as_str(), "prices_closed");
        assert_eq!(WriteTable::SalesClosed.as_str(), "sales_closed");
        assert_eq!(
            WriteTable::AvailabilityClosed.as_str(),
            "availability_closed"
        );
    }

    /// Регрессия: значения `as_str()` стабильны. Эти строки живут в
    /// Prometheus TSDB и на дашбордах. Менять — только через миграцию.
    #[test]
    fn write_table_known_labels_are_stable() {
        assert_eq!(WriteTable::Objects.as_str(), "objects");
        assert_eq!(WriteTable::Tours.as_str(), "tours");
        assert_eq!(WriteTable::PricesCreated.as_str(), "prices_created");
        assert_eq!(WriteTable::SalesUpdated.as_str(), "sales_updated");
        assert_eq!(
            WriteTable::AvailabilityCreated.as_str(),
            "availability_created"
        );
    }
}
