use std::sync::Arc;

use domain::errors::ReadError;
use domain::ports::CruiseReadRepository;
use domain::views::{CruiseListCursor, CruiseListFilter, CruiseListItem};

pub struct ListCruisesUseCase {
    repository: Arc<dyn CruiseReadRepository>,
}

/// Результат страницы списка круизов.
///
/// `has_more` = есть ли ещё строки после этой страницы.
/// `next_cursor` = позиция последней отданной строки, если `has_more`.
/// Оба поля вычисляются use case'ом по sentinel-строке, которую
/// вернул репозиторий.
#[derive(Debug)]
pub struct ListCruisesResult {
    pub items: Vec<CruiseListItem>,
    pub limit: i64,
    pub has_more: bool,
    pub next_cursor: Option<CruiseListCursor>,
}

impl ListCruisesUseCase {
    pub fn new(repository: Arc<dyn CruiseReadRepository>) -> Self {
        Self { repository }
    }

    /// Возвращает страницу круизов.
    ///
    /// ## Защита от неограниченных выборок
    ///
    /// `limit` клампится в `[1, 100]`. Это инвариант API: 100 — потолок,
    /// больше не имеет смысла на одном запросе (порядок страницы — 20-50).
    ///
    /// ## Sentinel-строка
    ///
    /// Репозиторий возвращает не более `limit + 1` строк. Если вернулось
    /// `limit + 1` — есть ещё; отрезаем последнюю и отдаём её как
    /// `next_cursor`. Если меньше или равно `limit` — это последняя
    /// страница, `next_cursor = None`.
    ///
    /// Такой подход экономит `COUNT(*)` — при keyset pagination он
    /// бессмыслен (общее число меняется между страницами) и дорог.
    pub async fn execute(
        &self,
        mut filter: CruiseListFilter,
    ) -> Result<ListCruisesResult, ReadError> {
        filter.limit = filter.limit.clamp(1, 100);

        let mut rows = self.repository.list_cruises(&filter).await?;

        let has_more = rows.len() as i64 > filter.limit;
        if has_more {
            rows.truncate(filter.limit as usize);
        }

        let next_cursor = if has_more {
            rows.last()
                .map(|r| CruiseListCursor::new(r.begin_date, r.cruise_id.clone()))
        } else {
            None
        };

        Ok(ListCruisesResult {
            items: rows,
            limit: filter.limit,
            has_more,
            next_cursor,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use chrono::NaiveDate;
    use domain::views::{CruiseDetail, CruiseListFilter};

    /// In-memory репозиторий: возвращает фиксированный список.
    /// Поддерживает keyset по `(begin_date, cruise_id)`.
    struct MockRepo {
        items: Vec<CruiseListItem>,
    }

    #[async_trait]
    impl CruiseReadRepository for MockRepo {
        async fn list_cruises(
            &self,
            filter: &CruiseListFilter,
        ) -> Result<Vec<CruiseListItem>, ReadError> {
            let sql_limit = filter.limit + 1;

            let mut result: Vec<CruiseListItem> = self
                .items
                .iter()
                .filter(|i| {
                    if let Some(c) = &filter.cursor {
                        // keyset: (begin_date, cruise_id) > (cursor.begin_date, cursor.cruise_id)
                        (i.begin_date, &i.cruise_id) > (c.begin_date, &c.cruise_id)
                    } else {
                        true
                    }
                })
                .cloned()
                .collect();

            result.truncate(sql_limit as usize);
            Ok(result)
        }

        async fn get_cruise(&self, _: &str, _: &str) -> Result<Option<CruiseDetail>, ReadError> {
            Ok(None)
        }
    }

    fn item(id: &str, date: NaiveDate) -> CruiseListItem {
        CruiseListItem {
            cruise_id: id.into(),
            name: format!("Route {id}"),
            ship_name: None,
            begin_date: date,
            end_date: date,
            days: None,
            route: None,
            departure_city: None,
            minimal_price: None,
            room_counts: 0,
            is_active: true,
        }
    }

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    fn repo_with(items: Vec<CruiseListItem>) -> Arc<dyn CruiseReadRepository> {
        Arc::new(MockRepo { items })
    }

    fn base_filter() -> CruiseListFilter {
        CruiseListFilter {
            cruise_provider_id: Some("1".into()),
            limit: 10,
            ..Default::default()
        }
    }

    // ============================================================
    // limit clamp
    // ============================================================

    #[tokio::test]
    async fn limit_clamped_to_max_100() {
        let repo = repo_with(vec![]);
        let uc = ListCruisesUseCase::new(repo);

        let mut f = base_filter();
        f.limit = 1000;

        let r = uc.execute(f).await.expect("execute");
        assert_eq!(r.limit, 100);
    }

    #[tokio::test]
    async fn limit_clamped_to_min_1() {
        let repo = repo_with(vec![]);
        let uc = ListCruisesUseCase::new(repo);

        let mut f = base_filter();
        f.limit = 0;

        let r = uc.execute(f).await.expect("execute");
        assert_eq!(r.limit, 1);
    }

    #[tokio::test]
    async fn negative_limit_clamped_to_1() {
        let repo = repo_with(vec![]);
        let uc = ListCruisesUseCase::new(repo);

        let mut f = base_filter();
        f.limit = -5;

        let r = uc.execute(f).await.expect("execute");
        assert_eq!(r.limit, 1);
    }

    // ============================================================
    // has_more / next_cursor
    // ============================================================

    /// Репозиторий отдал ровно `limit` строк → это последняя страница.
    #[tokio::test]
    async fn no_more_when_rows_equal_limit() {
        let repo = repo_with(vec![
            item("c1", date(2026, 9, 1)),
            item("c2", date(2026, 9, 2)),
        ]);
        let uc = ListCruisesUseCase::new(repo);

        let mut f = base_filter();
        f.limit = 2;

        let r = uc.execute(f).await.expect("execute");
        assert_eq!(r.items.len(), 2);
        assert!(!r.has_more);
        assert!(r.next_cursor.is_none());
    }

    /// Репозиторий отдал `limit + 1` → есть ещё.
    #[tokio::test]
    async fn has_more_when_rows_exceed_limit() {
        let repo = repo_with(vec![
            item("c1", date(2026, 9, 1)),
            item("c2", date(2026, 9, 2)),
            item("c3", date(2026, 9, 3)),
        ]);
        let uc = ListCruisesUseCase::new(repo);

        let mut f = base_filter();
        f.limit = 2;

        let r = uc.execute(f).await.expect("execute");
        assert_eq!(r.items.len(), 2, "sentinel срезан");
        assert!(r.has_more);
        assert_eq!(
            r.next_cursor,
            Some(CruiseListCursor::new(date(2026, 9, 2), "c2")),
            "курсор указывает на последний отданный item",
        );
    }

    /// Курсор в запросе должен прокинуться в репозиторий и вернуть хвост.
    #[tokio::test]
    async fn cursor_passes_through_to_repo() {
        let repo = repo_with(vec![
            item("c1", date(2026, 9, 1)),
            item("c2", date(2026, 9, 2)),
            item("c3", date(2026, 9, 3)),
        ]);
        let uc = ListCruisesUseCase::new(repo);

        let mut f = base_filter();
        f.limit = 2;
        f.cursor = Some(CruiseListCursor::new(date(2026, 9, 1), "c1"));

        let r = uc.execute(f).await.expect("execute");
        assert_eq!(r.items.len(), 2);
        assert_eq!(r.items[0].cruise_id, "c2");
        assert_eq!(r.items[1].cruise_id, "c3");
        assert!(!r.has_more);
    }
}
