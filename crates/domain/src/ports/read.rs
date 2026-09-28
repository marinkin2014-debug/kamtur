use async_trait::async_trait;

use crate::errors::ReadError;
use crate::views::*;

#[async_trait]
pub trait CruiseReadRepository: Send + Sync {
    /// Возвращает страницу круизов.
    ///
    /// Контракт:
    ///
    /// - Сортировка — `(begin_date ASC, cruise_id ASC)`.
    /// - Если `filter.cursor` задан — вернуть строки строго после него
    ///   (по композитному ключу `(begin_date, cruise_id)`).
    /// - Возвращает **не более** `filter.limit + 1` строк. Лишняя
    ///   (sentinel) строка нужна use case'у для `has_more`. Срезать её —
    ///   ответственность use case.
    /// - Никакого `total`/`COUNT(*)` — при keyset pagination он
    ///   бессмыслен (меняется между страницами) и дорог.
    async fn list_cruises(
        &self,
        filter: &CruiseListFilter,
    ) -> Result<Vec<CruiseListItem>, ReadError>;

    async fn get_cruise(
        &self,
        cruise_provider_id: &str,
        cruise_provider_cruise_id: &str,
    ) -> Result<Option<CruiseDetail>, ReadError>;
}
