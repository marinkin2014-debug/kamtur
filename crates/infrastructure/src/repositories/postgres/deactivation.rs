use chrono::{DateTime, Utc};
use sqlx::{Postgres, Transaction};

use domain::entities::*;
use domain::errors::RepositoryError;

use super::error::tx_err;

/// Тип сущности, для которой собираем incoming-ids в temp table.
///
/// Значения `as_str()` уходят в столбец `entity_type` temp table
/// `incoming_ids`. Это часть SQL-контракта, менять их нельзя без миграции.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntityType {
    Tours,
    Objects,
    Stages,
    Classes,
    Rooms,
}

impl EntityType {
    #[inline]
    fn as_str(&self) -> &'static str {
        match self {
            EntityType::Tours => "tours",
            EntityType::Objects => "objects",
            EntityType::Stages => "stages",
            EntityType::Classes => "classes",
            EntityType::Rooms => "rooms",
        }
    }
}

/// Помечает `is_active = false` строки, которых нет в incoming-наборе.
///
/// ## Как это работает
///
/// Загружаем все id из `canonical` во временную таблицу
/// `incoming_ids(entity_type, id)` (одна на транзакцию, `ON COMMIT DROP`),
/// затем для каждой целевой таблицы делаем `UPDATE ... WHERE NOT EXISTS`.
///
/// ## Почему temp table, а не `<> ALL($2::text[])`
///
/// `NOT IN` / `<> ALL` по массиву — антипаттерн:
///
/// - Planner не всегда переписывает в anti-join; на 10k+ элементах
///   возможен O(active × incoming).
/// - Для 5 типов приходится держать 5 массивов и 5 SQL statements.
/// - `format!` для table/column — потенциально ломает plan-cache.
///
/// Temp table + `NOT EXISTS` даёт стабильно:
///
/// - O(N+M) через hash anti-join, planner видит PK `(entity_type, id)`.
/// - Одна подготовленная форма запроса для всех 5 таблиц.
/// - Одна temp table вместо 5 массивов в памяти.
///
/// ## Идемпотентность
///
/// `DROP TABLE IF EXISTS` перед `CREATE` — на случай, если функция
/// вызывается дважды в одной транзакции (например, из тестов).
/// `ON COMMIT DROP` убирает таблицу при завершении транзакции.
pub(super) async fn deactivate_missing(
    tx: &mut Transaction<'_, Postgres>,
    provider_id: &ProviderId,
    canonical: &CanonicalData,
    batch_size: usize,
    now: DateTime<Utc>,
) -> Result<usize, RepositoryError> {
    create_incoming_ids_table(tx).await?;

    let bs = batch_size.max(1);

    load_incoming_ids(
        tx,
        EntityType::Tours,
        canonical
            .tours
            .iter()
            .map(|t| t.external_cruise_id.as_str()),
        bs,
    )
    .await?;
    load_incoming_ids(
        tx,
        EntityType::Objects,
        canonical.objects.iter().map(|o| o.external_id.as_str()),
        bs,
    )
    .await?;
    load_incoming_ids(
        tx,
        EntityType::Stages,
        canonical.stages.iter().map(|s| s.external_id.as_str()),
        bs,
    )
    .await?;
    load_incoming_ids(
        tx,
        EntityType::Classes,
        canonical
            .classes
            .iter()
            .map(|c| c.external_class_id.as_str()),
        bs,
    )
    .await?;
    load_incoming_ids(
        tx,
        EntityType::Rooms,
        canonical.rooms.iter().map(|r| r.external_room_id.as_str()),
        bs,
    )
    .await?;

    let mut total = 0usize;

    total += deactivate_by_type(
        tx,
        provider_id,
        EntityType::Tours,
        "cruise_provider_tours",
        "cruise_provider_cruise_id",
        now,
    )
    .await?;

    total += deactivate_by_type(
        tx,
        provider_id,
        EntityType::Objects,
        "cruise_provider_objects",
        "cruise_provider_object_id",
        now,
    )
    .await?;

    total += deactivate_by_type(
        tx,
        provider_id,
        EntityType::Stages,
        "cruise_provider_stages",
        "cruise_provider_stage_id",
        now,
    )
    .await?;

    total += deactivate_by_type(
        tx,
        provider_id,
        EntityType::Classes,
        "cruise_provider_classes",
        "cruise_provider_class_id",
        now,
    )
    .await?;

    total += deactivate_by_type(
        tx,
        provider_id,
        EntityType::Rooms,
        "cruise_provider_rooms",
        "cruise_provider_room_id",
        now,
    )
    .await?;

    Ok(total)
}

// ============================================================
// Internals
// ============================================================

async fn create_incoming_ids_table(
    tx: &mut Transaction<'_, Postgres>,
) -> Result<(), RepositoryError> {
    // Идемпотентно: при повторном вызове в одной транзакции
    // (например, из тестов) — пересоздаём.
    sqlx::query("DROP TABLE IF EXISTS incoming_ids")
        .execute(&mut **tx)
        .await
        .map_err(tx_err)?;

    // PK (entity_type, id) — даёт:
    //   1. Btree-индекс для hash/index scan в anti-join.
    //   2. Автоматическую дедупликацию при ON CONFLICT DO NOTHING.
    sqlx::query(
        "CREATE TEMP TABLE incoming_ids (
             entity_type TEXT NOT NULL,
             id          TEXT NOT NULL,
             PRIMARY KEY (entity_type, id)
         ) ON COMMIT DROP",
    )
    .execute(&mut **tx)
    .await
    .map_err(tx_err)?;

    Ok(())
}

/// Загружает id одного типа в temp table батчами через `UNNEST`.
///
/// `ON CONFLICT DO NOTHING` — на случай дубликатов в `canonical`
/// (не должно быть, но PK защищает без ошибки).
async fn load_incoming_ids<'a, I>(
    tx: &mut Transaction<'_, Postgres>,
    entity_type: EntityType,
    ids: I,
    batch_size: usize,
) -> Result<(), RepositoryError>
where
    I: Iterator<Item = &'a str>,
{
    let ids: Vec<&str> = ids.collect();
    if ids.is_empty() {
        return Ok(());
    }

    for chunk in ids.chunks(batch_size) {
        sqlx::query(
            "INSERT INTO incoming_ids (entity_type, id)
             SELECT $1, unnest($2::text[])
             ON CONFLICT (entity_type, id) DO NOTHING",
        )
        .bind(entity_type.as_str())
        .bind(chunk)
        .execute(&mut **tx)
        .await
        .map_err(tx_err)?;
    }

    Ok(())
}

/// Деактивирует строки целевой таблицы, которых нет в incoming для
/// данного `entity_type`.
///
/// `table` и `id_column` — константы внутри кода, не из пользовательского
/// ввода. `format!` безопасен, но мы осознанно не параметризуем SQL
/// (Postgres не поддерживает параметры для идентификаторов). Значения
/// `entity_type` и `id` приходят только как bind-параметры.
async fn deactivate_by_type(
    tx: &mut Transaction<'_, Postgres>,
    provider_id: &ProviderId,
    entity_type: EntityType,
    table: &str,
    id_column: &str,
    now: DateTime<Utc>,
) -> Result<usize, RepositoryError> {
    let sql = format!(
        "UPDATE {table} SET is_active = false, updated_at = $3
         WHERE cruise_provider_id = $1
           AND is_active = true
           AND NOT EXISTS (
               SELECT 1 FROM incoming_ids i
               WHERE i.entity_type = $2
                 AND i.id = {id_column}
           )"
    );

    let affected = sqlx::query(&sql)
        .bind(&provider_id.0)
        .bind(entity_type.as_str())
        .bind(now)
        .execute(&mut **tx)
        .await
        .map_err(tx_err)?
        .rows_affected() as usize;

    Ok(affected)
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
    use sqlx::PgPool;

    fn empty_canonical() -> CanonicalData {
        CanonicalData::default()
    }

    fn canonical_with_tour(id: &str) -> CanonicalData {
        let mut c = CanonicalData::default();
        c.tours.push(CanonicalTour {
            external_object_id: "o1".into(),
            external_cruise_id: id.into(),
            cruise_type_id: 1,
            begin_date: NaiveDate::from_ymd_opt(2026, 9, 1).unwrap(),
            begin_time: None,
            end_date: NaiveDate::from_ymd_opt(2026, 9, 5).unwrap(),
            end_time: None,
            name: "R".into(),
        });
        c
    }

    async fn insert_active_tour(pool: &PgPool, pid: &str, cruise_id: &str) {
        sqlx::query(
            "INSERT INTO cruise_provider_tours
                (cruise_provider_id, cruise_provider_object_id, cruise_provider_cruise_id,
                 cruise_type_id, begin_date, end_date, name, is_active, updated_at)
             VALUES ($1, 'o1', $2, 1, '2026-09-01', '2026-09-05', 'R', true, now())",
        )
        .bind(pid)
        .bind(cruise_id)
        .execute(pool)
        .await
        .expect("insert tour");
    }

    // ============================================================
    // Базовые сценарии (совпадают с прежним поведением)
    // ============================================================

    #[tokio::test]
    async fn deactivate_missing_marks_absent_tours_inactive() {
        let _guard = db_lock().await;
        let pool_owned = pool().await;
        let pool = &pool_owned;
        purge_all_test_rows(pool).await;
        let pid = test_provider_id("deact");
        insert_provider(pool, &pid).await;

        insert_active_tour(pool, &pid, "c1").await;
        insert_active_tour(pool, &pid, "c2").await;

        // В canonical только c1, c2 пропал
        let canonical = canonical_with_tour("c1");

        let mut tx = pool.begin().await.expect("begin");
        let affected = deactivate_missing(
            &mut tx,
            &ProviderId(pid.clone()),
            &canonical,
            5000,
            Utc::now(),
        )
        .await
        .expect("deactivate");
        tx.commit().await.expect("commit");

        assert_eq!(affected, 1);

        let c1_active: bool = sqlx::query_scalar(
            "SELECT is_active FROM cruise_provider_tours
             WHERE cruise_provider_id = $1 AND cruise_provider_cruise_id = 'c1'",
        )
        .bind(&pid)
        .fetch_one(pool)
        .await
        .expect("c1");

        let c2_active: bool = sqlx::query_scalar(
            "SELECT is_active FROM cruise_provider_tours
             WHERE cruise_provider_id = $1 AND cruise_provider_cruise_id = 'c2'",
        )
        .bind(&pid)
        .fetch_one(pool)
        .await
        .expect("c2");

        assert!(c1_active, "c1 в canonical — остаётся активным");
        assert!(!c2_active, "c2 пропал — деактивируется");

        cleanup_provider(pool, &pid).await;
    }

    #[tokio::test]
    async fn deactivate_missing_with_empty_canonical_deactivates_all() {
        let _guard = db_lock().await;
        let pool_owned = pool().await;
        let pool = &pool_owned;
        purge_all_test_rows(pool).await;
        let pid = test_provider_id("deact");
        insert_provider(pool, &pid).await;

        insert_active_tour(pool, &pid, "c1").await;
        insert_active_tour(pool, &pid, "c2").await;

        let mut tx = pool.begin().await.expect("begin");
        let affected = deactivate_missing(
            &mut tx,
            &ProviderId(pid.clone()),
            &empty_canonical(),
            5000,
            Utc::now(),
        )
        .await
        .expect("deactivate");
        tx.commit().await.expect("commit");

        assert_eq!(affected, 2);

        let active_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM cruise_provider_tours
             WHERE cruise_provider_id = $1 AND is_active = true",
        )
        .bind(&pid)
        .fetch_one(pool)
        .await
        .expect("count");
        assert_eq!(active_count, 0);

        cleanup_provider(pool, &pid).await;
    }

    #[tokio::test]
    async fn deactivate_missing_does_not_touch_other_providers() {
        let _guard = db_lock().await;
        let pool_owned = pool().await;
        let pool = &pool_owned;
        purge_all_test_rows(pool).await;
        let pid_a = test_provider_id("deact-a");
        let pid_b = test_provider_id("deact-b");
        insert_provider(pool, &pid_a).await;
        insert_provider(pool, &pid_b).await;

        insert_active_tour(pool, &pid_a, "c1").await;
        insert_active_tour(pool, &pid_b, "c1").await;

        let mut tx = pool.begin().await.expect("begin");
        deactivate_missing(
            &mut tx,
            &ProviderId(pid_a.clone()),
            &empty_canonical(),
            5000,
            Utc::now(),
        )
        .await
        .expect("deactivate");
        tx.commit().await.expect("commit");

        let a_active: bool = sqlx::query_scalar(
            "SELECT is_active FROM cruise_provider_tours
             WHERE cruise_provider_id = $1 AND cruise_provider_cruise_id = 'c1'",
        )
        .bind(&pid_a)
        .fetch_one(pool)
        .await
        .expect("a");

        let b_active: bool = sqlx::query_scalar(
            "SELECT is_active FROM cruise_provider_tours
             WHERE cruise_provider_id = $1 AND cruise_provider_cruise_id = 'c1'",
        )
        .bind(&pid_b)
        .fetch_one(pool)
        .await
        .expect("b");

        assert!(!a_active, "pid_a деактивирован");
        assert!(b_active, "pid_b не тронут");

        cleanup_provider(pool, &pid_a).await;
        cleanup_provider(pool, &pid_b).await;
    }

    // ============================================================
    // Новые сценарии (temp table)
    // ============================================================

    /// Регрессия: `deactivate_missing` вызывается дважды в одной транзакции.
    ///
    /// Раньше temp table не было — проблема могла проявиться при
    /// добавлении в будущем. Сейчас `DROP TABLE IF EXISTS` + `ON COMMIT DROP`
    /// гарантируют корректность при повторном вызове.
    #[tokio::test]
    async fn deactivate_missing_two_calls_in_one_tx_work() {
        let _guard = db_lock().await;
        let pool_owned = pool().await;
        let pool = &pool_owned;
        purge_all_test_rows(pool).await;
        let pid = test_provider_id("deact");
        insert_provider(pool, &pid).await;

        insert_active_tour(pool, &pid, "c1").await;
        insert_active_tour(pool, &pid, "c2").await;

        let mut tx = pool.begin().await.expect("begin");
        let provider = ProviderId(pid.clone());

        // Первый вызов — canonical только с c1, c2 деактивируется
        let first = deactivate_missing(
            &mut tx,
            &provider,
            &canonical_with_tour("c1"),
            5000,
            Utc::now(),
        )
        .await
        .expect("first call");
        assert_eq!(first, 1, "c2 деактивирован");

        // Второй вызов в той же транзакции — не должен падать на DROP/CREATE
        let second = deactivate_missing(&mut tx, &provider, &empty_canonical(), 5000, Utc::now())
            .await
            .expect("second call");
        // c1 ещё активен на момент второго вызова → деактивируется
        assert_eq!(second, 1, "c1 деактивирован вторым вызовом");

        tx.commit().await.expect("commit");

        cleanup_provider(pool, &pid).await;
    }

    /// Дубликаты id в canonical не должны вызывать ошибку PK.
    /// `ON CONFLICT DO NOTHING` их проглатывает.
    #[tokio::test]
    async fn deactivate_missing_handles_duplicate_ids() {
        let _guard = db_lock().await;
        let pool_owned = pool().await;
        let pool = &pool_owned;
        purge_all_test_rows(pool).await;
        let pid = test_provider_id("deact");
        insert_provider(pool, &pid).await;

        insert_active_tour(pool, &pid, "c1").await;
        insert_active_tour(pool, &pid, "c2").await;

        // Canonical с дубликатами c1 три раза
        let mut canonical = CanonicalData::default();
        for _ in 0..3 {
            canonical.tours.push(CanonicalTour {
                external_object_id: "o1".into(),
                external_cruise_id: "c1".into(),
                cruise_type_id: 1,
                begin_date: NaiveDate::from_ymd_opt(2026, 9, 1).unwrap(),
                begin_time: None,
                end_date: NaiveDate::from_ymd_opt(2026, 9, 5).unwrap(),
                end_time: None,
                name: "R".into(),
            });
        }

        let mut tx = pool.begin().await.expect("begin");
        let affected = deactivate_missing(
            &mut tx,
            &ProviderId(pid.clone()),
            &canonical,
            5000,
            Utc::now(),
        )
        .await
        .expect("deactivate");
        tx.commit().await.expect("commit");

        // c2 отсутствует → деактивирован; c1 в canonical (даже с дублями) → активен
        assert_eq!(affected, 1);

        cleanup_provider(pool, &pid).await;
    }

    /// Большой incoming (250+) прогоняется батчами. Проверяем, что
    /// батчинг работает и все id попадают в temp table без потерь.
    #[tokio::test]
    async fn deactivate_missing_large_batch() {
        let _guard = db_lock().await;
        let pool_owned = pool().await;
        let pool = &pool_owned;
        purge_all_test_rows(pool).await;
        let pid = test_provider_id("deact");
        insert_provider(pool, &pid).await;

        // 250 активных туров
        let total = 250usize;
        for i in 0..total {
            insert_active_tour(pool, &pid, &format!("c{i}")).await;
        }

        // Canonical содержит первые 200 — остальные 50 должны деактивироваться
        let mut canonical = CanonicalData::default();
        for i in 0..200 {
            canonical.tours.push(CanonicalTour {
                external_object_id: "o1".into(),
                external_cruise_id: format!("c{i}"),
                cruise_type_id: 1,
                begin_date: NaiveDate::from_ymd_opt(2026, 9, 1).unwrap(),
                begin_time: None,
                end_date: NaiveDate::from_ymd_opt(2026, 9, 5).unwrap(),
                end_time: None,
                name: "R".into(),
            });
        }

        let mut tx = pool.begin().await.expect("begin");
        // batch_size 32 → 7 батчей по 32 + 1 на 16
        let affected = deactivate_missing(
            &mut tx,
            &ProviderId(pid.clone()),
            &canonical,
            32,
            Utc::now(),
        )
        .await
        .expect("deactivate");
        tx.commit().await.expect("commit");

        assert_eq!(affected, 50, "c200..c249 деактивированы");

        let active: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM cruise_provider_tours
             WHERE cruise_provider_id = $1 AND is_active = true",
        )
        .bind(&pid)
        .fetch_one(pool)
        .await
        .expect("count");
        assert_eq!(active, 200);

        cleanup_provider(pool, &pid).await;
    }

    /// Деактивация всех 5 типов в одном вызове: tours, objects, stages,
    /// classes, rooms. Проверяем, что каждая таблица обрабатывается
    /// независимо и суммарное число совпадает.
    #[tokio::test]
    async fn deactivate_missing_covers_all_entity_types() {
        let _guard = db_lock().await;
        let pool_owned = pool().await;
        let pool = &pool_owned;
        purge_all_test_rows(pool).await;
        let pid = test_provider_id("deact");
        insert_provider(pool, &pid).await;

        // По одной активной строке в каждой из 5 таблиц.
        sqlx::query(
            "INSERT INTO cruise_provider_objects
                (cruise_provider_id, cruise_provider_object_id, name, is_active, updated_at)
             VALUES ($1, 'o1', 'X', true, now())",
        )
        .bind(&pid)
        .execute(pool)
        .await
        .expect("object");

        sqlx::query(
            "INSERT INTO cruise_provider_stages
                (cruise_provider_id, cruise_provider_stage_id, name, is_active, updated_at)
             VALUES ($1, 'd1', 'D', true, now())",
        )
        .bind(&pid)
        .execute(pool)
        .await
        .expect("stage");

        sqlx::query(
            "INSERT INTO cruise_provider_classes
                (cruise_provider_id, cruise_provider_object_id, cruise_provider_class_id,
                 name, is_active, updated_at)
             VALUES ($1, 'o1', 'cls1', 'C', true, now())",
        )
        .bind(&pid)
        .execute(pool)
        .await
        .expect("class");

        sqlx::query(
            "INSERT INTO cruise_provider_rooms
                (cruise_provider_id, cruise_provider_object_id, cruise_provider_stage_id,
                 cruise_provider_class_id, cruise_provider_room_id, number, is_active, updated_at)
             VALUES ($1, 'o1', 'd1', 'cls1', 'r1', '101', true, now())",
        )
        .bind(&pid)
        .execute(pool)
        .await
        .expect("room");

        insert_active_tour(pool, &pid, "c1").await;

        // Пустой canonical → деактивируется всё
        let mut tx = pool.begin().await.expect("begin");
        let affected = deactivate_missing(
            &mut tx,
            &ProviderId(pid.clone()),
            &empty_canonical(),
            5000,
            Utc::now(),
        )
        .await
        .expect("deactivate");
        tx.commit().await.expect("commit");

        assert_eq!(affected, 5, "по 1 в каждой из 5 таблиц");

        for (table, id_col, id_val) in &[
            ("cruise_provider_tours", "cruise_provider_cruise_id", "c1"),
            ("cruise_provider_objects", "cruise_provider_object_id", "o1"),
            ("cruise_provider_stages", "cruise_provider_stage_id", "d1"),
            (
                "cruise_provider_classes",
                "cruise_provider_class_id",
                "cls1",
            ),
            ("cruise_provider_rooms", "cruise_provider_room_id", "r1"),
        ] {
            let active: bool = sqlx::query_scalar(&format!(
                "SELECT is_active FROM {table}
                 WHERE cruise_provider_id = $1 AND {id_col} = $2"
            ))
            .bind(&pid)
            .bind(*id_val)
            .fetch_one(pool)
            .await
            .unwrap_or_else(|e| panic!("{table}: {e}"));
            assert!(!active, "{table}/{id_val} должен быть деактивирован");
        }

        cleanup_provider(pool, &pid).await;
    }
}
