use chrono::{DateTime, Utc};
use sqlx::{Postgres, Transaction};

use domain::entities::*;
use domain::errors::RepositoryError;

use super::error::tx_err;

#[derive(Debug, Default)]
pub(super) struct AvailabilityOutcome {
    pub created: usize, // новых открытых версий (available = true)
    pub updated: usize, // переходов (true → false и false → true)
    pub closed: usize,  // было true → стало false
}

pub(super) async fn apply_availability_scd2(
    tx: &mut Transaction<'_, Postgres>,
    provider_id: &ProviderId,
    items: &[CanonicalAvailability],
    batch_size: usize,
    now: DateTime<Utc>,
) -> Result<AvailabilityOutcome, RepositoryError> {
    // Пустой incoming — все открытые версии закрываются.
    if items.is_empty() {
        let closed = sqlx::query(
            "UPDATE cruise_provider_availability SET valid_to = $2
             WHERE cruise_provider_id = $1 AND valid_to IS NULL",
        )
        .bind(&provider_id.0)
        .bind(now)
        .execute(&mut **tx)
        .await
        .map_err(tx_err)?
        .rows_affected() as usize;

        return Ok(AvailabilityOutcome {
            created: 0,
            updated: 0,
            closed,
        });
    }

    // ============ 1. TEMP TABLE incoming_availability ============
    sqlx::query("DROP TABLE IF EXISTS incoming_availability")
        .execute(&mut **tx)
        .await
        .map_err(tx_err)?;

    sqlx::query(
        "CREATE TEMP TABLE incoming_availability (
             cruise_id TEXT NOT NULL,
             room_id   TEXT NOT NULL,
             PRIMARY KEY (cruise_id, room_id)
         ) ON COMMIT DROP",
    )
    .execute(&mut **tx)
    .await
    .map_err(tx_err)?;

    let bs = batch_size.max(1);
    for chunk in items.chunks(bs) {
        let cruise: Vec<&str> = chunk
            .iter()
            .map(|a| a.external_cruise_id.as_str())
            .collect();
        let room: Vec<&str> = chunk.iter().map(|a| a.external_room_id.as_str()).collect();

        sqlx::query(
            "INSERT INTO incoming_availability (cruise_id, room_id)
             SELECT * FROM UNNEST($1::text[], $2::text[])
             ON CONFLICT (cruise_id, room_id) DO NOTHING",
        )
        .bind(&cruise)
        .bind(&room)
        .execute(&mut **tx)
        .await
        .map_err(tx_err)?;
    }

    // ============ 2. TEMP TABLE changed_availability ============
    sqlx::query("DROP TABLE IF EXISTS changed_availability")
        .execute(&mut **tx)
        .await
        .map_err(tx_err)?;

    sqlx::query(
        "CREATE TEMP TABLE changed_availability (
             cruise_id     TEXT NOT NULL,
             room_id       TEXT NOT NULL,
             was_available BOOLEAN NOT NULL
         ) ON COMMIT DROP",
    )
    .execute(&mut **tx)
    .await
    .map_err(tx_err)?;

    // ============ 3. Закрыть изменившиеся ============
    sqlx::query(
        "WITH closed AS (
             UPDATE cruise_provider_availability h SET valid_to = $2
             WHERE h.cruise_provider_id = $1
               AND h.valid_to IS NULL
               AND (
                   -- было true, стало false (связки нет в incoming)
                   (h.available = true AND NOT EXISTS (
                       SELECT 1 FROM incoming_availability i
                       WHERE i.cruise_id = h.cruise_provider_cruise_id
                         AND i.room_id   = h.cruise_provider_room_id
                   ))
                   OR
                   -- было false, стало true (связка снова в incoming)
                   (h.available = false AND EXISTS (
                       SELECT 1 FROM incoming_availability i
                       WHERE i.cruise_id = h.cruise_provider_cruise_id
                         AND i.room_id   = h.cruise_provider_room_id
                   ))
               )
             RETURNING
                 h.cruise_provider_cruise_id AS cruise_id,
                 h.cruise_provider_room_id   AS room_id,
                 h.available                 AS was_available
         )
         INSERT INTO changed_availability (cruise_id, room_id, was_available)
         SELECT cruise_id, room_id, was_available FROM closed",
    )
    .bind(&provider_id.0)
    .bind(now)
    .execute(&mut **tx)
    .await
    .map_err(tx_err)?;

    // ============ 4. Открыть инвертированные для changed ============
    let updated = sqlx::query(
        "INSERT INTO cruise_provider_availability
             (cruise_provider_id, cruise_provider_cruise_id,
              cruise_provider_room_id, available, valid_from)
         SELECT $1, cruise_id, room_id, NOT was_available, $2
         FROM changed_availability",
    )
    .bind(&provider_id.0)
    .bind(now)
    .execute(&mut **tx)
    .await
    .map_err(tx_err)?
    .rows_affected() as usize;

    // ============ 5. Открыть новые (true) из incoming ============
    let created = sqlx::query(
        "INSERT INTO cruise_provider_availability
             (cruise_provider_id, cruise_provider_cruise_id,
              cruise_provider_room_id, available, valid_from)
         SELECT $1, i.cruise_id, i.room_id, true, $2
         FROM incoming_availability i
         WHERE NOT EXISTS (
             SELECT 1 FROM cruise_provider_availability h
             WHERE h.cruise_provider_id      = $1
               AND h.cruise_provider_cruise_id = i.cruise_id
               AND h.cruise_provider_room_id   = i.room_id
               AND h.valid_to IS NULL
         )",
    )
    .bind(&provider_id.0)
    .bind(now)
    .execute(&mut **tx)
    .await
    .map_err(tx_err)?
    .rows_affected() as usize;

    // ============ 6. Посчитать закрытые (true → false) ============
    let closed: i64 =
        sqlx::query_scalar("SELECT count(*) FROM changed_availability WHERE was_available = true")
            .fetch_one(&mut **tx)
            .await
            .map_err(tx_err)?;

    Ok(AvailabilityOutcome {
        created,
        updated,
        closed: closed as usize,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repositories::postgres::test_support::{
        cleanup_provider, db_lock, insert_provider, pool, purge_all_test_rows, test_provider_id,
    };
    use chrono::Duration;

    fn avail(cruise: &str, room: &str) -> CanonicalAvailability {
        CanonicalAvailability {
            external_cruise_id: cruise.into(),
            external_room_id: room.into(),
            available: true,
        }
    }

    async fn count_open(tx: &mut Transaction<'_, Postgres>, provider_id: &str) -> i64 {
        sqlx::query_scalar(
            "SELECT count(*) FROM cruise_provider_availability
             WHERE cruise_provider_id = $1 AND valid_to IS NULL",
        )
        .bind(provider_id)
        .fetch_one(&mut **tx)
        .await
        .expect("count open")
    }

    async fn current_state(
        tx: &mut Transaction<'_, Postgres>,
        provider_id: &str,
        room: &str,
    ) -> Option<bool> {
        sqlx::query_scalar(
            "SELECT available FROM cruise_provider_availability
             WHERE cruise_provider_id = $1
               AND cruise_provider_room_id = $2
               AND valid_to IS NULL
             LIMIT 1",
        )
        .bind(provider_id)
        .bind(room)
        .fetch_optional(&mut **tx)
        .await
        .expect("fetch current state")
    }

    /// Первый sync с непустым списком: комната → открытая версия true.
    #[tokio::test]
    async fn first_appearance_creates_open_version() {
        let _guard = db_lock().await;
        let pool_owned = pool().await;
        let pool = &pool_owned;
        purge_all_test_rows(pool).await;
        let pid = test_provider_id("avail");
        insert_provider(pool, &pid).await;

        let mut tx = pool.begin().await.expect("begin");
        let provider = ProviderId(pid.clone());
        let now = Utc::now();

        let outcome = apply_availability_scd2(
            &mut tx,
            &provider,
            &[avail("c1", "r1"), avail("c1", "r2")],
            100,
            now,
        )
        .await
        .expect("first");

        assert_eq!(outcome.created, 2);
        assert_eq!(outcome.updated, 0);
        assert_eq!(outcome.closed, 0);
        assert_eq!(count_open(&mut tx, &pid).await, 2);
        assert_eq!(current_state(&mut tx, &pid, "r1").await, Some(true));

        tx.rollback().await.ok();
        cleanup_provider(pool, &pid).await;
    }

    /// Комната пропала: true → false. 1 закрытие, 1 новая версия `false`.
    #[tokio::test]
    async fn room_disappeared_transitions_true_to_false() {
        let _guard = db_lock().await;
        let pool_owned = pool().await;
        let pool = &pool_owned;
        purge_all_test_rows(pool).await;
        let pid = test_provider_id("avail");
        insert_provider(pool, &pid).await;

        let mut tx = pool.begin().await.expect("begin");
        let provider = ProviderId(pid.clone());
        let now = Utc::now();

        // Baseline: 2 комнаты доступны
        apply_availability_scd2(
            &mut tx,
            &provider,
            &[avail("c1", "r1"), avail("c1", "r2")],
            100,
            now,
        )
        .await
        .expect("baseline");
        assert_eq!(count_open(&mut tx, &pid).await, 2);

        // r2 пропала
        let outcome = apply_availability_scd2(
            &mut tx,
            &provider,
            &[avail("c1", "r1")],
            100,
            now + Duration::seconds(60),
        )
        .await
        .expect("second");

        assert_eq!(outcome.created, 0, "нет новых комнат");
        assert_eq!(outcome.updated, 1, "r2 перевёрнута в false");
        assert_eq!(outcome.closed, 1, "одна была true → стала false");
        assert_eq!(count_open(&mut tx, &pid).await, 2, "r1 true, r2 false");
        assert_eq!(current_state(&mut tx, &pid, "r2").await, Some(false));

        tx.rollback().await.ok();
        cleanup_provider(pool, &pid).await;
    }

    /// Комната вернулась: false → true. Переход в обе стороны.
    #[tokio::test]
    async fn room_returned_transitions_false_to_true() {
        let _guard = db_lock().await;
        let pool_owned = pool().await;
        let pool = &pool_owned;
        purge_all_test_rows(pool).await;
        let pid = test_provider_id("avail");
        insert_provider(pool, &pid).await;

        let mut tx = pool.begin().await.expect("begin");
        let provider = ProviderId(pid.clone());
        let now = Utc::now();

        // Baseline: r1 доступна, r2 недоступна (отсутствует)
        apply_availability_scd2(&mut tx, &provider, &[avail("c1", "r1")], 100, now)
            .await
            .expect("baseline");

        // Сначала r2 уходит
        apply_availability_scd2(
            &mut tx,
            &provider,
            &[avail("c1", "r1"), avail("c1", "r2")],
            100,
            now + Duration::seconds(30),
        )
        .await
        .expect("r2 appears");
        assert_eq!(current_state(&mut tx, &pid, "r2").await, Some(true));

        // Теперь r2 возвращается
        let outcome = apply_availability_scd2(
            &mut tx,
            &provider,
            &[avail("c1", "r1")],
            100,
            now + Duration::seconds(60),
        )
        .await
        .expect("r2 disappears");
        assert_eq!(outcome.closed, 1);
        assert_eq!(current_state(&mut tx, &pid, "r2").await, Some(false));

        // И снова появляется
        let outcome = apply_availability_scd2(
            &mut tx,
            &provider,
            &[avail("c1", "r1"), avail("c1", "r2")],
            100,
            now + Duration::seconds(90),
        )
        .await
        .expect("r2 returns");

        assert_eq!(outcome.updated, 1, "r2: false → true");
        assert_eq!(current_state(&mut tx, &pid, "r2").await, Some(true));

        tx.rollback().await.ok();
        cleanup_provider(pool, &pid).await;
    }

    /// Одинаковый список дважды → ничего не меняется.
    #[tokio::test]
    async fn unchanged_availability_creates_no_new_versions() {
        let _guard = db_lock().await;
        let pool_owned = pool().await;
        let pool = &pool_owned;
        purge_all_test_rows(pool).await;
        let pid = test_provider_id("avail");
        insert_provider(pool, &pid).await;

        let mut tx = pool.begin().await.expect("begin");
        let provider = ProviderId(pid.clone());
        let now = Utc::now();

        let items = vec![avail("c1", "r1"), avail("c1", "r2")];

        apply_availability_scd2(&mut tx, &provider, &items, 100, now)
            .await
            .expect("baseline");

        let outcome =
            apply_availability_scd2(&mut tx, &provider, &items, 100, now + Duration::seconds(60))
                .await
                .expect("second");

        assert_eq!(outcome.created, 0);
        assert_eq!(outcome.updated, 0);
        assert_eq!(outcome.closed, 0);
        assert_eq!(count_open(&mut tx, &pid).await, 2);

        tx.rollback().await.ok();
        cleanup_provider(pool, &pid).await;
    }

    /// Пустой вход → все открытые версии закрываются.
    #[tokio::test]
    async fn empty_input_closes_all_open_versions() {
        let _guard = db_lock().await;
        let pool_owned = pool().await;
        let pool = &pool_owned;
        purge_all_test_rows(pool).await;
        let pid = test_provider_id("avail");
        insert_provider(pool, &pid).await;

        let mut tx = pool.begin().await.expect("begin");
        let provider = ProviderId(pid.clone());
        let now = Utc::now();

        apply_availability_scd2(
            &mut tx,
            &provider,
            &[avail("c1", "r1"), avail("c1", "r2")],
            100,
            now,
        )
        .await
        .expect("baseline");
        assert_eq!(count_open(&mut tx, &pid).await, 2);

        let outcome =
            apply_availability_scd2(&mut tx, &provider, &[], 100, now + Duration::seconds(60))
                .await
                .expect("empty");

        assert_eq!(outcome.created, 0);
        assert_eq!(outcome.updated, 0);
        assert_eq!(outcome.closed, 2);
        assert_eq!(count_open(&mut tx, &pid).await, 0);

        tx.rollback().await.ok();
        cleanup_provider(pool, &pid).await;
    }

    /// Регрессия: `apply_availability_scd2` вызывается дважды в одной транзакции.
    /// Именно тут был баг: вторая DROP-строка дропала `incoming_availability`
    /// вместо `changed_availability`.
    #[tokio::test]
    async fn two_calls_in_one_transaction_work() {
        let _guard = db_lock().await;
        let pool_owned = pool().await;
        let pool = &pool_owned;
        purge_all_test_rows(pool).await;
        let pid = test_provider_id("avail");
        insert_provider(pool, &pid).await;

        let mut tx = pool.begin().await.expect("begin");
        let provider = ProviderId(pid.clone());
        let now = Utc::now();

        // Первый вызов
        apply_availability_scd2(&mut tx, &provider, &[avail("c1", "r1")], 100, now)
            .await
            .expect("first call");

        // Второй вызов в той же транзакции — должен работать
        let outcome = apply_availability_scd2(
            &mut tx,
            &provider,
            &[avail("c1", "r1"), avail("c1", "r2")],
            100,
            now + Duration::seconds(60),
        )
        .await
        .expect("second call in same tx");

        assert_eq!(outcome.created, 1, "r2 новая");
        assert_eq!(count_open(&mut tx, &pid).await, 2);

        tx.rollback().await.ok();
        cleanup_provider(pool, &pid).await;
    }
}
