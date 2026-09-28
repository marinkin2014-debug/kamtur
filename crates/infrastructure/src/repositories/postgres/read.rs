use async_trait::async_trait;
use rust_decimal::Decimal;
use sqlx::PgPool;

use domain::errors::ReadError;
use domain::ports::CruiseReadRepository;
use domain::views::*;

pub struct PostgresCruiseReadRepository {
    pool: PgPool,
}

impl PostgresCruiseReadRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn to_read_err(e: sqlx::Error) -> ReadError {
    ReadError::Query(e.to_string())
}

#[async_trait]
impl CruiseReadRepository for PostgresCruiseReadRepository {
    /// Keyset pagination по `(begin_date ASC, cruise_id ASC)`.
    ///
    /// ## Почему keyset, а не OFFSET
    ///
    /// `OFFSET N` заставляет Postgres отсортировать и «пропустить» N строк
    /// перед выдачей страницы. Для N = 100k это 100k бесполезных чтений.
    /// Keyset делает `WHERE (begin_date, cruise_id) > (cursor_date, cursor_id)`
    /// — O(limit) независимо от глубины.
    ///
    /// ## Почему LATERAL, а не коррелированный подзапрос
    ///
    /// Коррелированный `(SELECT MIN(...) FROM (...UNION ALL...)) AS minimal_price`
    /// в `SELECT` выполняется построчно. `LEFT JOIN LATERAL (...) ON true`
    /// эквивалентен семантически, но позволяет планировщику применять
    /// тот же план с лучшей оптимизацией и читается как обычный join.
    ///
    /// ## limit+1
    ///
    /// Возвращаем не более `filter.limit + 1` строк. Sentinel нужен use
    /// case'у, чтобы вычислить `has_more` без `COUNT(*)`.
    async fn list_cruises(
        &self,
        filter: &CruiseListFilter,
    ) -> Result<Vec<CruiseListItem>, ReadError> {
        let provider = filter.cruise_provider_id.as_deref().unwrap_or("1");

        // Sentinel-строка: если она вернулась — есть ещё.
        let sql_limit = filter.limit + 1;

        // Курсор: (date, id) или (NULL, NULL).
        let (cursor_date, cursor_id) = match filter.cursor.as_ref() {
            Some(c) => (Some(c.begin_date), Some(c.cruise_id.as_str())),
            None => (None, None),
        };

        #[derive(sqlx::FromRow)]
        struct Row {
            cruise_id: String,
            name: String,
            ship_name: Option<String>,
            begin_date: chrono::NaiveDate,
            end_date: chrono::NaiveDate,
            days: Option<i32>,
            route: Option<String>,
            departure_city: Option<String>,
            minimal_price: Option<Decimal>,
            room_counts: i32,
            is_active: bool,
        }

        let rows: Vec<Row> = sqlx::query_as(
            r#"
            SELECT
                t.cruise_provider_cruise_id                        AS cruise_id,
                t.name,
                o.name                                              AS ship_name,
                t.begin_date,
                t.end_date,
                t.days,
                t.route,
                t.departure_city,
                mp.minimal_price,
                COALESCE(ac.cnt, 0)::int                            AS room_counts,
                t.is_active
            FROM cruise_provider_tours t
            LEFT JOIN cruise_provider_objects o
                   ON o.cruise_provider_id = t.cruise_provider_id
                  AND o.cruise_provider_object_id = t.cruise_provider_object_id
            LEFT JOIN LATERAL (
                SELECT MIN(base_price) AS minimal_price
                FROM (
                    SELECT base_price FROM cruise_provider_prices p
                    WHERE p.cruise_provider_id = t.cruise_provider_id
                      AND p.cruise_provider_cruise_id = t.cruise_provider_cruise_id
                      AND p.valid_to IS NULL
                    UNION ALL
                    SELECT base_price FROM cruise_provider_sales s
                    WHERE s.cruise_provider_id = t.cruise_provider_id
                      AND s.cruise_provider_cruise_id = t.cruise_provider_cruise_id
                      AND s.valid_to IS NULL
                ) sub
            ) mp ON true
            LEFT JOIN LATERAL (
                SELECT count(*) AS cnt
                FROM cruise_provider_availability a
                WHERE a.cruise_provider_id = t.cruise_provider_id
                  AND a.cruise_provider_cruise_id = t.cruise_provider_cruise_id
                  AND a.available = true
                  AND a.valid_to IS NULL
            ) ac ON true
            WHERE t.cruise_provider_id = $1
              AND t.is_active = true
              AND ($2::date IS NULL OR t.begin_date >= $2)
              AND ($3::date IS NULL OR t.begin_date <= $3)
              AND ($4::text IS NULL OR t.departure_city = $4)
              AND (
                  $5::date IS NULL
                  OR t.begin_date > $5::date
                  OR (t.begin_date = $5::date
                      AND t.cruise_provider_cruise_id > $6::text)
              )
            ORDER BY t.begin_date ASC, t.cruise_provider_cruise_id ASC
            LIMIT $7
            "#,
        )
        .bind(provider)
        .bind(filter.begin_from)
        .bind(filter.begin_to)
        .bind(filter.departure_city.as_deref())
        .bind(cursor_date)
        .bind(cursor_id)
        .bind(sql_limit)
        .fetch_all(&self.pool)
        .await
        .map_err(to_read_err)?;

        Ok(rows
            .into_iter()
            .map(|r| CruiseListItem {
                cruise_id: r.cruise_id,
                name: r.name,
                ship_name: r.ship_name,
                begin_date: r.begin_date,
                end_date: r.end_date,
                days: r.days,
                route: r.route,
                departure_city: r.departure_city,
                minimal_price: r.minimal_price,
                room_counts: r.room_counts,
                is_active: r.is_active,
            })
            .collect())
    }

    async fn get_cruise(
        &self,
        cruise_provider_id: &str,
        cruise_provider_cruise_id: &str,
    ) -> Result<Option<CruiseDetail>, ReadError> {
        #[derive(sqlx::FromRow)]
        struct TourRow {
            cruise_id: String,
            name: String,
            ship_name: Option<String>,
            begin_date: chrono::NaiveDate,
            begin_time: Option<chrono::NaiveTime>,
            end_date: chrono::NaiveDate,
            end_time: Option<chrono::NaiveTime>,
            days: Option<i32>,
            route: Option<String>,
            departure_city: Option<String>,
            city_from: Option<String>,
            city_to: Option<String>,
            is_return: Option<bool>,
            is_weekend: Option<bool>,
            is_active: bool,
            status: String,
        }

        let tour: Option<TourRow> = sqlx::query_as(
            r#"
            SELECT
                t.cruise_provider_cruise_id AS cruise_id,
                t.name,
                o.name                       AS ship_name,
                t.begin_date,
                t.begin_time,
                t.end_date,
                t.end_time,
                t.days,
                t.route,
                t.departure_city,
                t.city_from,
                t.city_to,
                t.is_return,
                t.is_weekend,
                t.is_active,
                t.status
            FROM cruise_provider_tours t
            LEFT JOIN cruise_provider_objects o
                   ON o.cruise_provider_id = t.cruise_provider_id
                  AND o.cruise_provider_object_id = t.cruise_provider_object_id
            WHERE t.cruise_provider_id = $1
              AND t.cruise_provider_cruise_id = $2
            "#,
        )
        .bind(cruise_provider_id)
        .bind(cruise_provider_cruise_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(to_read_err)?;

        let Some(tour) = tour else {
            return Ok(None);
        };

        let (prices, rooms, availability) = tokio::try_join!(
            load_class_prices(&self.pool, cruise_provider_id, cruise_provider_cruise_id),
            load_rooms(&self.pool, cruise_provider_id, cruise_provider_cruise_id),
            load_availability(&self.pool, cruise_provider_id, cruise_provider_cruise_id),
        )?;

        let mut prices_by_class: std::collections::HashMap<String, Vec<RoomPriceView>> =
            std::collections::HashMap::with_capacity(prices.len());
        for p in prices.iter() {
            prices_by_class
                .entry(p.class_id.clone())
                .or_default()
                .push(RoomPriceView {
                    partial_buyout: p.partial_buyout,
                    base_price: p.base_price,
                    currency: p.currency.clone(),
                });
        }

        let avail_by_room: std::collections::HashMap<String, bool> = availability;

        let rooms: Vec<RoomView> = rooms
            .into_iter()
            .map(|r| {
                let available = avail_by_room.get(&r.room_id).copied().unwrap_or(false);
                let mut room_prices = prices_by_class
                    .get(&r.class_id)
                    .cloned()
                    .unwrap_or_default();
                room_prices.sort_by_key(|p| p.partial_buyout);
                RoomView {
                    room_id: r.room_id,
                    number: r.number,
                    class_id: r.class_id,
                    class_name: r.class_name,
                    stage_id: r.stage_id,
                    stage_name: r.stage_name,
                    available,
                    prices: room_prices,
                }
            })
            .collect();

        Ok(Some(CruiseDetail {
            cruise_id: tour.cruise_id,
            name: tour.name,
            ship_name: tour.ship_name,
            begin_date: tour.begin_date,
            begin_time: tour.begin_time,
            end_date: tour.end_date,
            end_time: tour.end_time,
            days: tour.days,
            route: tour.route,
            departure_city: tour.departure_city,
            city_from: tour.city_from,
            city_to: tour.city_to,
            is_return: tour.is_return,
            is_weekend: tour.is_weekend,
            is_active: tour.is_active,
            status: tour.status,
            prices,
            rooms,
        }))
    }
}

// ============================================================
// Helpers — принимают &PgPool
// ============================================================

async fn load_class_prices(
    pool: &PgPool,
    provider_id: &str,
    cruise_id: &str,
) -> Result<Vec<ClassPriceView>, ReadError> {
    #[derive(sqlx::FromRow)]
    struct Row {
        class_id: String,
        class_name: Option<String>,
        description: Option<String>,
        base_seats: Option<i32>,
        tiers: Option<i32>,
        partial_buyout: bool,
        base_price: Decimal,
        child_price: Option<Decimal>,
        extra_seat: Option<Decimal>,
        currency: String,
    }

    let rows: Vec<Row> = sqlx::query_as(
        r#"
        SELECT
            p.cruise_provider_class_id AS class_id,
            c.name                     AS class_name,
            c.description,
            c.base_seats,
            c.tiers,
            p.partial_buyout,
            p.base_price,
            p.child_price,
            p.extra_seat,
            p.currency
        FROM cruise_provider_prices p
        LEFT JOIN cruise_provider_classes c
               ON c.cruise_provider_id = p.cruise_provider_id
              AND c.cruise_provider_class_id = p.cruise_provider_class_id
        WHERE p.cruise_provider_id = $1
          AND p.cruise_provider_cruise_id = $2
          AND p.valid_to IS NULL
        ORDER BY p.base_price ASC, p.partial_buyout ASC
        "#,
    )
    .bind(provider_id)
    .bind(cruise_id)
    .fetch_all(pool)
    .await
    .map_err(to_read_err)?;

    Ok(rows
        .into_iter()
        .map(|r| ClassPriceView {
            class_id: r.class_id,
            class_name: r.class_name.unwrap_or_default(),
            description: r.description,
            base_seats: r.base_seats,
            tiers: r.tiers,
            partial_buyout: r.partial_buyout,
            base_price: r.base_price,
            child_price: r.child_price,
            extra_seat: r.extra_seat,
            currency: r.currency,
        })
        .collect())
}

struct RoomRow {
    room_id: String,
    number: String,
    class_id: String,
    class_name: String,
    stage_id: Option<String>,
    stage_name: Option<String>,
}

async fn load_rooms(
    pool: &PgPool,
    provider_id: &str,
    cruise_id: &str,
) -> Result<Vec<RoomRow>, ReadError> {
    #[derive(sqlx::FromRow)]
    struct Row {
        room_id: String,
        number: String,
        class_id: String,
        class_name: Option<String>,
        stage_id: Option<String>,
        stage_name: Option<String>,
    }

    let rows: Vec<Row> = sqlx::query_as(
        r#"
        SELECT
            r.cruise_provider_room_id AS room_id,
            r.number,
            r.cruise_provider_class_id AS class_id,
            c.name                     AS class_name,
            r.cruise_provider_stage_id AS stage_id,
            s.name                     AS stage_name
        FROM cruise_provider_rooms r
        LEFT JOIN cruise_provider_classes c
               ON c.cruise_provider_id = r.cruise_provider_id
              AND c.cruise_provider_class_id = r.cruise_provider_class_id
        LEFT JOIN cruise_provider_stages s
               ON s.cruise_provider_id = r.cruise_provider_id
              AND s.cruise_provider_stage_id = r.cruise_provider_stage_id
        WHERE r.cruise_provider_id = $1
          AND EXISTS (
              SELECT 1
              FROM cruise_provider_availability a
              WHERE a.cruise_provider_id = r.cruise_provider_id
                AND a.cruise_provider_cruise_id = $2
                AND a.cruise_provider_room_id = r.cruise_provider_room_id
                AND a.valid_to IS NULL
          )
        ORDER BY r.number ASC
        "#,
    )
    .bind(provider_id)
    .bind(cruise_id)
    .fetch_all(pool)
    .await
    .map_err(to_read_err)?;

    Ok(rows
        .into_iter()
        .map(|r| RoomRow {
            room_id: r.room_id,
            number: r.number,
            class_id: r.class_id,
            class_name: r.class_name.unwrap_or_default(),
            stage_id: r.stage_id,
            stage_name: r.stage_name,
        })
        .collect())
}

async fn load_availability(
    pool: &PgPool,
    provider_id: &str,
    cruise_id: &str,
) -> Result<std::collections::HashMap<String, bool>, ReadError> {
    #[derive(sqlx::FromRow)]
    struct Row {
        room_id: String,
        available: bool,
    }

    let rows: Vec<Row> = sqlx::query_as(
        r#"
        SELECT cruise_provider_room_id AS room_id, available
        FROM cruise_provider_availability
        WHERE cruise_provider_id = $1
          AND cruise_provider_cruise_id = $2
          AND valid_to IS NULL
        "#,
    )
    .bind(provider_id)
    .bind(cruise_id)
    .fetch_all(pool)
    .await
    .map_err(to_read_err)?;

    Ok(rows.into_iter().map(|r| (r.room_id, r.available)).collect())
}

// ============================================================
// Tests
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repositories::postgres::test_support::{
        cleanup_provider, db_lock, insert_provider, pool, purge_all_test_rows, test_provider_id,
    };
    use chrono::NaiveDate;
    use rust_decimal::Decimal;
    use std::str::FromStr;

    /// Строит сцену в БД: object, 3 tours, class, 2 rooms, prices, availability.
    async fn setup_scene(pool: &PgPool, pid: &str) {
        sqlx::query(
            "INSERT INTO cruise_provider_objects
                (cruise_provider_id, cruise_provider_object_id, name, is_active, updated_at)
             VALUES ($1, 'o1', 'Ship Alpha', true, now())",
        )
        .bind(pid)
        .execute(pool)
        .await
        .expect("insert object");

        sqlx::query(
            "INSERT INTO cruise_provider_tours
                (cruise_provider_id, cruise_provider_object_id, cruise_provider_cruise_id,
                 cruise_type_id, begin_date, end_date, name, is_active, updated_at)
             VALUES
                ($1, 'o1', 'c1', 1, '2026-09-10', '2026-09-15', 'Route A', true, now()),
                ($1, 'o1', 'c2', 1, '2026-09-20', '2026-09-25', 'Route B', true, now()),
                ($1, 'o1', 'c3', 1, '2026-10-01', '2026-10-05', 'Route C', true, now())",
        )
        .bind(pid)
        .execute(pool)
        .await
        .expect("insert tours");

        sqlx::query(
            "INSERT INTO cruise_provider_classes
                (cruise_provider_id, cruise_provider_object_id, cruise_provider_class_id,
                 name, base_seats, tiers, partial_buyout, is_active, updated_at)
             VALUES ($1, 'o1', 'cls1', 'Lux', 2, 1, false, true, now())",
        )
        .bind(pid)
        .execute(pool)
        .await
        .expect("insert class");

        sqlx::query(
            "INSERT INTO cruise_provider_rooms
                (cruise_provider_id, cruise_provider_object_id, cruise_provider_stage_id,
                 cruise_provider_class_id, cruise_provider_room_id, number, is_active, updated_at)
             VALUES
                ($1, 'o1', 'd1', 'cls1', 'r1', '101', true, now()),
                ($1, 'o1', 'd1', 'cls1', 'r2', '102', true, now())",
        )
        .bind(pid)
        .execute(pool)
        .await
        .expect("insert rooms");

        sqlx::query(
            "INSERT INTO cruise_provider_prices
                (cruise_provider_id, cruise_provider_cruise_id, cruise_provider_class_id,
                 partial_buyout, base_price, currency, valid_from)
             VALUES
                ($1, 'c1', 'cls1', false, 50000.00, 'RUB', now()),
                ($1, 'c2', 'cls1', false, 30000.00, 'RUB', now())",
        )
        .bind(pid)
        .execute(pool)
        .await
        .expect("insert prices");

        sqlx::query(
            "INSERT INTO cruise_provider_availability
                (cruise_provider_id, cruise_provider_cruise_id,
                 cruise_provider_room_id, available, valid_from)
             VALUES
                ($1, 'c1', 'r1', true, now()),
                ($1, 'c1', 'r2', false, now())",
        )
        .bind(pid)
        .execute(pool)
        .await
        .expect("insert availability");
    }

    fn repo(pool: &PgPool) -> PostgresCruiseReadRepository {
        PostgresCruiseReadRepository::new(pool.clone())
    }

    fn empty_filter(pid: &str) -> CruiseListFilter {
        CruiseListFilter {
            cruise_provider_id: Some(pid.into()),
            limit: 20,
            ..Default::default()
        }
    }

    // ============================================================
    // list_cruises — базовое
    // ============================================================

    #[tokio::test]
    async fn list_returns_all_active_tours() {
        let _guard = db_lock().await;
        let pool = pool().await;
        purge_all_test_rows(&pool).await;
        let pid = test_provider_id("read");
        insert_provider(&pool, &pid).await;
        setup_scene(&pool, &pid).await;

        let r = repo(&pool);
        let items = r.list_cruises(&empty_filter(&pid)).await.expect("list");

        assert_eq!(items.len(), 3);
        assert_eq!(items[0].cruise_id, "c1");
        assert_eq!(items[1].cruise_id, "c2");
        assert_eq!(items[2].cruise_id, "c3");

        cleanup_provider(&pool, &pid).await;
    }

    #[tokio::test]
    async fn list_includes_ship_name_via_join() {
        let _guard = db_lock().await;
        let pool = pool().await;
        purge_all_test_rows(&pool).await;
        let pid = test_provider_id("read");
        insert_provider(&pool, &pid).await;
        setup_scene(&pool, &pid).await;

        let r = repo(&pool);
        let items = r.list_cruises(&empty_filter(&pid)).await.expect("list");

        assert_eq!(items[0].ship_name.as_deref(), Some("Ship Alpha"));

        cleanup_provider(&pool, &pid).await;
    }

    #[tokio::test]
    async fn list_minimal_price_from_prices() {
        let _guard = db_lock().await;
        let pool = pool().await;
        purge_all_test_rows(&pool).await;
        let pid = test_provider_id("read");
        insert_provider(&pool, &pid).await;
        setup_scene(&pool, &pid).await;

        let r = repo(&pool);
        let items = r.list_cruises(&empty_filter(&pid)).await.expect("list");

        let c1 = items.iter().find(|i| i.cruise_id == "c1").unwrap();
        assert_eq!(
            c1.minimal_price,
            Some(Decimal::from_str("50000.00").unwrap())
        );

        cleanup_provider(&pool, &pid).await;
    }

    #[tokio::test]
    async fn list_room_counts_only_available() {
        let _guard = db_lock().await;
        let pool = pool().await;
        purge_all_test_rows(&pool).await;
        let pid = test_provider_id("read");
        insert_provider(&pool, &pid).await;
        setup_scene(&pool, &pid).await;

        let r = repo(&pool);
        let items = r.list_cruises(&empty_filter(&pid)).await.expect("list");

        let c1 = items.iter().find(|i| i.cruise_id == "c1").unwrap();
        assert_eq!(c1.room_counts, 1);

        let c2 = items.iter().find(|i| i.cruise_id == "c2").unwrap();
        assert_eq!(c2.room_counts, 0);

        cleanup_provider(&pool, &pid).await;
    }

    #[tokio::test]
    async fn list_filters_by_begin_date_range() {
        let _guard = db_lock().await;
        let pool = pool().await;
        purge_all_test_rows(&pool).await;
        let pid = test_provider_id("read");
        insert_provider(&pool, &pid).await;
        setup_scene(&pool, &pid).await;

        let r = repo(&pool);
        let mut filter = empty_filter(&pid);
        filter.begin_from = Some(NaiveDate::from_ymd_opt(2026, 9, 15).unwrap());
        filter.begin_to = Some(NaiveDate::from_ymd_opt(2026, 9, 30).unwrap());

        let items = r.list_cruises(&filter).await.expect("list");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].cruise_id, "c2");

        cleanup_provider(&pool, &pid).await;
    }

    #[tokio::test]
    async fn list_ignores_inactive_tours() {
        let _guard = db_lock().await;
        let pool = pool().await;
        purge_all_test_rows(&pool).await;
        let pid = test_provider_id("read");
        insert_provider(&pool, &pid).await;
        setup_scene(&pool, &pid).await;

        sqlx::query(
            "UPDATE cruise_provider_tours SET is_active = false
             WHERE cruise_provider_id = $1 AND cruise_provider_cruise_id = 'c3'",
        )
        .bind(&pid)
        .execute(&pool)
        .await
        .expect("deactivate");

        let r = repo(&pool);
        let items = r.list_cruises(&empty_filter(&pid)).await.expect("list");

        assert_eq!(items.len(), 2);
        assert!(!items.iter().any(|i| i.cruise_id == "c3"));

        cleanup_provider(&pool, &pid).await;
    }

    // ============================================================
    // list_cruises — keyset pagination
    // ============================================================

    /// Репозиторий возвращает не более `limit+1` строк. Это контракт
    /// для use case'а: он отрежет sentinel и вычислит has_more.
    #[tokio::test]
    async fn list_returns_at_most_limit_plus_one() {
        let _guard = db_lock().await;
        let pool = pool().await;
        purge_all_test_rows(&pool).await;
        let pid = test_provider_id("read");
        insert_provider(&pool, &pid).await;
        setup_scene(&pool, &pid).await;

        let r = repo(&pool);
        let mut filter = empty_filter(&pid);
        filter.limit = 2;

        let items = r.list_cruises(&filter).await.expect("list");
        assert_eq!(items.len(), 3, "limit=2 → репозиторий вернёт 3 (2+1)");
        assert_eq!(items[0].cruise_id, "c1");
        assert_eq!(items[1].cruise_id, "c2");
        assert_eq!(items[2].cruise_id, "c3", "sentinel для has_more");

        cleanup_provider(&pool, &pid).await;
    }

    /// Keyset: курсор «(2026-09-10, c1)» → следующая страница начинается
    /// с c2. Строки, которые < курсора, не возвращаются.
    #[tokio::test]
    async fn list_cursor_skips_past_rows() {
        let _guard = db_lock().await;
        let pool = pool().await;
        purge_all_test_rows(&pool).await;
        let pid = test_provider_id("read");
        insert_provider(&pool, &pid).await;
        setup_scene(&pool, &pid).await;

        let r = repo(&pool);
        let mut filter = empty_filter(&pid);
        filter.cursor = Some(CruiseListCursor::new(
            NaiveDate::from_ymd_opt(2026, 9, 10).unwrap(),
            "c1",
        ));

        let items = r.list_cruises(&filter).await.expect("list");
        assert_eq!(items.len(), 2, "осталось c2 и c3");
        assert_eq!(items[0].cruise_id, "c2");
        assert_eq!(items[1].cruise_id, "c3");

        cleanup_provider(&pool, &pid).await;
    }

    /// Курсор с датой в будущем → пустой результат.
    #[tokio::test]
    async fn list_cursor_in_future_returns_empty() {
        let _guard = db_lock().await;
        let pool = pool().await;
        purge_all_test_rows(&pool).await;
        let pid = test_provider_id("read");
        insert_provider(&pool, &pid).await;
        setup_scene(&pool, &pid).await;

        let r = repo(&pool);
        let mut filter = empty_filter(&pid);
        filter.cursor = Some(CruiseListCursor::new(
            NaiveDate::from_ymd_opt(2030, 1, 1).unwrap(),
            "zzz",
        ));

        let items = r.list_cruises(&filter).await.expect("list");
        assert!(items.is_empty());

        cleanup_provider(&pool, &pid).await;
    }

    /// Курсор внутри одной даты, id больше → строки с той же датой и
    /// меньшим/равным id отсеиваются. У нас три даты разные, но
    /// проверим семантику композитного сравнения на двух датах.
    #[tokio::test]
    async fn list_cursor_composite_semantics() {
        let _guard = db_lock().await;
        let pool = pool().await;
        purge_all_test_rows(&pool).await;
        let pid = test_provider_id("read");
        insert_provider(&pool, &pid).await;

        // Три тура на одной дате — проверим сортировку по cruise_id.
        sqlx::query(
            "INSERT INTO cruise_provider_tours
                (cruise_provider_id, cruise_provider_object_id, cruise_provider_cruise_id,
                 cruise_type_id, begin_date, end_date, name, is_active, updated_at)
             VALUES
                ($1, 'o1', 'a1', 1, '2026-09-10', '2026-09-15', 'A', true, now()),
                ($1, 'o1', 'a2', 1, '2026-09-10', '2026-09-15', 'B', true, now()),
                ($1, 'o1', 'a3', 1, '2026-09-10', '2026-09-15', 'C', true, now())",
        )
        .bind(&pid)
        .execute(&pool)
        .await
        .expect("insert tours");

        let r = repo(&pool);
        let mut filter = empty_filter(&pid);
        filter.cursor = Some(CruiseListCursor::new(
            NaiveDate::from_ymd_opt(2026, 9, 10).unwrap(),
            "a2",
        ));

        let items = r.list_cruises(&filter).await.expect("list");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].cruise_id, "a3");

        cleanup_provider(&pool, &pid).await;
    }

    // ============================================================
    // get_cruise
    // ============================================================

    #[tokio::test]
    async fn get_returns_full_detail() {
        let _guard = db_lock().await;
        let pool = pool().await;
        purge_all_test_rows(&pool).await;
        let pid = test_provider_id("read");
        insert_provider(&pool, &pid).await;
        setup_scene(&pool, &pid).await;

        let r = repo(&pool);
        let detail = r
            .get_cruise(&pid, "c1")
            .await
            .expect("get")
            .expect("exists");

        assert_eq!(detail.cruise_id, "c1");
        assert_eq!(detail.ship_name.as_deref(), Some("Ship Alpha"));
        assert_eq!(detail.prices.len(), 1);
        assert_eq!(detail.prices[0].class_name, "Lux");
        assert_eq!(
            detail.prices[0].base_price,
            Decimal::from_str("50000.00").unwrap()
        );
        assert_eq!(detail.rooms.len(), 2);
        let r1 = detail.rooms.iter().find(|r| r.room_id == "r1").unwrap();
        assert!(r1.available);
        let r2 = detail.rooms.iter().find(|r| r.room_id == "r2").unwrap();
        assert!(!r2.available);

        cleanup_provider(&pool, &pid).await;
    }

    #[tokio::test]
    async fn get_returns_none_for_unknown() {
        let _guard = db_lock().await;
        let pool = pool().await;
        purge_all_test_rows(&pool).await;
        let pid = test_provider_id("read");
        insert_provider(&pool, &pid).await;
        setup_scene(&pool, &pid).await;

        let r = repo(&pool);
        let detail = r.get_cruise(&pid, "nonexistent").await.expect("get");
        assert!(detail.is_none());

        cleanup_provider(&pool, &pid).await;
    }

    #[tokio::test]
    async fn get_room_prices_from_class_prices() {
        let _guard = db_lock().await;
        let pool = pool().await;
        purge_all_test_rows(&pool).await;
        let pid = test_provider_id("read");
        insert_provider(&pool, &pid).await;
        setup_scene(&pool, &pid).await;

        let r = repo(&pool);
        let detail = r
            .get_cruise(&pid, "c1")
            .await
            .expect("get")
            .expect("exists");

        for room in &detail.rooms {
            assert_eq!(room.class_id, "cls1");
            assert_eq!(room.prices.len(), 1);
            assert_eq!(
                room.prices[0].base_price,
                Decimal::from_str("50000.00").unwrap()
            );
        }

        cleanup_provider(&pool, &pid).await;
    }

    #[tokio::test]
    async fn get_cruise_without_availability_has_no_rooms() {
        let _guard = db_lock().await;
        let pool = pool().await;
        purge_all_test_rows(&pool).await;
        let pid = test_provider_id("read");
        insert_provider(&pool, &pid).await;
        setup_scene(&pool, &pid).await;

        let r = repo(&pool);
        let detail = r
            .get_cruise(&pid, "c2")
            .await
            .expect("get")
            .expect("exists");

        assert_eq!(detail.rooms.len(), 0);
        assert_eq!(detail.prices.len(), 1);

        cleanup_provider(&pool, &pid).await;
    }

    #[tokio::test]
    async fn get_returns_inactive_tour_if_exists() {
        let _guard = db_lock().await;
        let pool = pool().await;
        purge_all_test_rows(&pool).await;
        let pid = test_provider_id("read");
        insert_provider(&pool, &pid).await;
        setup_scene(&pool, &pid).await;

        sqlx::query(
            "UPDATE cruise_provider_tours SET is_active = false
             WHERE cruise_provider_id = $1 AND cruise_provider_cruise_id = 'c1'",
        )
        .bind(&pid)
        .execute(&pool)
        .await
        .expect("deactivate");

        let r = repo(&pool);
        let detail = r
            .get_cruise(&pid, "c1")
            .await
            .expect("get")
            .expect("exists");

        assert!(!detail.is_active);

        cleanup_provider(&pool, &pid).await;
    }
}
