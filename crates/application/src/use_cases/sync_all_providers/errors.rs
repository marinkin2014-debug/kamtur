use domain::entities::SyncStage;
use domain::errors::{ProviderError, RepositoryError};

/// Ошибка одной итерации sync-пайплайна.
///
/// Варианты разложены по стадиям, на которых ошибка возникла. Это
/// позволяет:
///   - инкрементить `kamtur_sync_errors_total{stage=...}` по конкретной фазе;
///   - писать `errors.stage = <phase>` в БД;
///   - группировать дайджест по стадиям.
///
/// ## Почему не `#[from]`
///
/// `RepositoryError` может прийти из четырёх разных стадий — `has_snapshot`
/// (Dedup), `load_enrichment_rules` (Enrich), `apply_sync` (Persist),
/// `record_sync_run_*` (Persist). Автоматический `From<RepositoryError>`
/// потерял бы контекст. Поэтому — **только явные фабрики**: `dedup(e)`,
/// `enrich(e)`, `persist(e)`.
///
/// ## Почему `String`, а не вложенная ошибка
///
/// В БД пишется `Display`, в логах — `Display`, в метрике — только стадия.
/// Никто не матчит `SyncError` по исходной ошибке. `String` даёт простой
/// API и не заставляет тащить типы `ProviderError`/`RepositoryError`
/// во внешние сигнатуры.
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("fetch: {0}")]
    Fetch(String),

    #[error("dedup: {0}")]
    Dedup(String),

    #[error("parse: {0}")]
    Parse(String),

    #[error("enrich: {0}")]
    Enrich(String),

    #[error("persist: {0}")]
    Persist(String),

    /// Защита от неожиданных случаев. На практике должен встречаться
    /// крайне редко — если часто, значит где-то потерян контекст стадии.
    #[error("internal: {0}")]
    Internal(String),
}

impl SyncError {
    /// Стадия пайплайна, на которой произошла ошибка.
    ///
    /// Значение используется как лейбл `stage` в `kamtur_sync_errors_total`
    /// и как значение колонки `errors.stage`.
    #[inline]
    pub fn stage(&self) -> SyncStage {
        match self {
            SyncError::Fetch(_) => SyncStage::Fetch,
            SyncError::Dedup(_) => SyncStage::Dedup,
            SyncError::Parse(_) => SyncStage::Parse,
            SyncError::Enrich(_) => SyncStage::Enrich,
            SyncError::Persist(_) => SyncStage::Persist,
            SyncError::Internal(_) => SyncStage::Unknown,
        }
    }

    /// Фабрика для ошибки `CruiseProvider::fetch_raw`.
    ///
    /// Технически `fetch_raw` возвращает `ProviderError`, но `Parse` там
    /// теоретически возможен (провайдер может парсить URL). Если прилетел
    /// `ProviderError::Parse` — маршрутизируем в правильную стадию,
    /// а не в `Fetch`. `Config` и `CircuitOpen` — тоже `Fetch`
    /// (это стадия инициализации/вызова провайдера).
    pub fn fetch(e: ProviderError) -> Self {
        match e {
            ProviderError::Parse(m) => SyncError::Parse(m),
            other => SyncError::Fetch(other.to_string()),
        }
    }

    /// Фабрика для ошибки `CruiseProvider::parse`.
    pub fn parse(e: ProviderError) -> Self {
        SyncError::Parse(e.to_string())
    }

    /// Фабрика для ошибки на стадии дедупликации (`has_snapshot`).
    pub fn dedup(e: RepositoryError) -> Self {
        SyncError::Dedup(e.to_string())
    }

    /// Фабрика для ошибки на стадии обогащения (`load_enrichment_rules`).
    pub fn enrich(e: RepositoryError) -> Self {
        SyncError::Enrich(e.to_string())
    }

    /// Фабрика для ошибки на стадии persist (`apply_sync`, `record_sync_run_*`).
    pub fn persist(e: RepositoryError) -> Self {
        SyncError::Persist(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ============================================================
    // stage() — точное соответствие вариант → стадия
    // ============================================================

    #[test]
    fn stage_mapping_is_exact() {
        assert_eq!(SyncError::Fetch(String::new()).stage(), SyncStage::Fetch);
        assert_eq!(SyncError::Dedup(String::new()).stage(), SyncStage::Dedup);
        assert_eq!(SyncError::Parse(String::new()).stage(), SyncStage::Parse);
        assert_eq!(SyncError::Enrich(String::new()).stage(), SyncStage::Enrich);
        assert_eq!(
            SyncError::Persist(String::new()).stage(),
            SyncStage::Persist
        );
        assert_eq!(
            SyncError::Internal(String::new()).stage(),
            SyncStage::Unknown
        );
    }

    // ============================================================
    // Фабрики
    // ============================================================

    #[test]
    fn fetch_factory_routes_network_to_fetch_stage() {
        let e = SyncError::fetch(ProviderError::Network("timeout".into()));
        assert!(matches!(e, SyncError::Fetch(_)));
        assert_eq!(e.stage(), SyncStage::Fetch);
        assert!(e.to_string().contains("timeout"));
    }

    #[test]
    fn fetch_factory_routes_parse_to_parse_stage() {
        let e = SyncError::fetch(ProviderError::Parse("bad xml".into()));
        assert!(matches!(e, SyncError::Parse(_)));
        assert_eq!(e.stage(), SyncStage::Parse);
    }

    /// `ProviderError::Config` — ошибка конфигурации клиента. Возникает
    /// при построении HTTP-клиента (fail-fast на старте воркера), но
    /// если она всё-таки дойдёт до sync-цикла — должна попасть в `Fetch`.
    #[test]
    fn fetch_factory_routes_config_to_fetch_stage() {
        let e = SyncError::fetch(ProviderError::Config("bad tls backend".into()));
        assert!(matches!(e, SyncError::Fetch(_)));
        assert_eq!(e.stage(), SyncStage::Fetch);
        assert!(e.to_string().contains("bad tls backend"));
    }

    #[test]
    fn fetch_factory_routes_circuit_open_to_fetch_stage() {
        let e = SyncError::fetch(ProviderError::CircuitOpen);
        assert!(matches!(e, SyncError::Fetch(_)));
        assert_eq!(e.stage(), SyncStage::Fetch);
    }

    #[test]
    fn repository_factories_preserve_stage() {
        assert_eq!(
            SyncError::dedup(RepositoryError::Connection("x".into())).stage(),
            SyncStage::Dedup
        );
        assert_eq!(
            SyncError::enrich(RepositoryError::Connection("x".into())).stage(),
            SyncStage::Enrich
        );
        assert_eq!(
            SyncError::persist(RepositoryError::Transaction("y".into())).stage(),
            SyncStage::Persist
        );
    }

    // ============================================================
    // Стабильность as_str() для Prometheus label и БД
    // ============================================================

    /// Зафиксировано: значения уходят в Prometheus label `stage` и
    /// в колонку `errors.stage`. Менять только через миграцию.
    #[test]
    fn sync_stage_as_str_is_stable() {
        assert_eq!(SyncStage::Fetch.as_str(), "fetch");
        assert_eq!(SyncStage::Dedup.as_str(), "dedup");
        assert_eq!(SyncStage::Parse.as_str(), "parse");
        assert_eq!(SyncStage::Enrich.as_str(), "enrich");
        assert_eq!(SyncStage::Persist.as_str(), "persist");
        assert_eq!(SyncStage::Notify.as_str(), "notify");
        assert_eq!(SyncStage::Unknown.as_str(), "unknown");
    }

    #[test]
    fn every_error_variant_has_stage() {
        let _ = SyncError::Fetch(String::new()).stage();
        let _ = SyncError::Dedup(String::new()).stage();
        let _ = SyncError::Parse(String::new()).stage();
        let _ = SyncError::Enrich(String::new()).stage();
        let _ = SyncError::Persist(String::new()).stage();
        let _ = SyncError::Internal(String::new()).stage();
    }
}
