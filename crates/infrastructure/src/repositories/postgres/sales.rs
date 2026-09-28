use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use sqlx::{Postgres, Transaction};

use domain::entities::*;
use domain::errors::RepositoryError;

use super::error::tx_err;
use super::prices::ScdOutcome;

const TEMP_TABLE_THRESHOLD: usize = 100;

pub(super) async fn apply_sales_scd2(
    tx: &mut Transaction<'_, Postgres>,
    provider_id: &ProviderId,
    sales: &[CanonicalSale],
    batch_size: usize,
    now: DateTime<Utc>,
) -> Result<ScdOutcome, RepositoryError> {
    if sales.is_empty() {
        // Никаких sales не пришло — закрываем все открытые версии.
        let closed = sqlx::query(
            "UPDATE cruise_provider_sales SET valid_to = $2
             WHERE cruise_provider_id = $1 AND valid_to IS NULL",
        )
        .bind(&provider_id.0)
        .bind(now)
        .execute(&mut **tx)
        .await
        .map_err(tx_err)?
        .rows_affected() as usize;

        return Ok(ScdOutcome {
            created: 0,
            updated: 0,
            closed,
        });
    }

    if sales.len() >= TEMP_TABLE_THRESHOLD {
        apply_with_temp_table(tx, provider_id, sales, batch_size, now).await
    } else {
        apply_small(tx, provider_id, sales, now).await
    }
}

/// Для маленьких наборов: три statement'а с прямым UNNEST.
///
/// ВАЖНО: логика сравнения совпадает с `apply_with_temp_table` — версия
/// закрывается только если `base_price` или `currency` реально изменились.
async fn apply_small(
    tx: &mut Transaction<'_, Postgres>,
    provider_id: &ProviderId,
    sales: &[CanonicalSale],
    now: DateTime<Utc>,
) -> Result<ScdOutcome, RepositoryError> {
    let cruise: Vec<&str> = sales
        .iter()
        .map(|s| s.external_cruise_id.as_str())
        .collect();
    let class: Vec<&str> = sales.iter().map(|s| s.external_class_id.as_str()).collect();
    let room: Vec<&str> = sales.iter().map(|s| s.external_room_id.as_str()).collect();
    let nofull: Vec<bool> = sales
        .iter()
        .map(|s| s.partial_buyout.unwrap_or(false))
        .collect();
    let base: Vec<Decimal> = sales.iter().map(|s| s.base_price).collect();
    let curr: Vec<&str> = sales.iter().map(|s| s.currency.as_str()).collect();

    // 1. Закрыть ИЗМЕНИВШИЕСЯ (только при реальной смене цены или валюты).
    let updated = sqlx::query(
        "UPDATE cruise_provider_sales h SET valid_to = $8
         WHERE h.cruise_provider_id = $1
           AND h.valid_to IS NULL
           AND EXISTS (
               SELECT 1
               FROM UNNEST($2::text[], $3::text[], $4::text[], $5::bool[],
                           $6::numeric[], $7::text[])
                    AS i(cruise, class, room, nofull, base, curr)
               WHERE i.cruise = h.cruise_provider_cruise_id
                 AND i.class  = h.cruise_provider_class_id
                 AND i.room   = h.cruise_provider_room_id
                 AND i.nofull = h.partial_buyout
                 AND (i.base <> h.base_price OR i.curr <> h.currency)
           )",
    )
    .bind(&provider_id.0)
    .bind(&cruise)
    .bind(&class)
    .bind(&room)
    .bind(&nofull)
    .bind(&base)
    .bind(&curr)
    .bind(now)
    .execute(&mut **tx)
    .await
    .map_err(tx_err)?
    .rows_affected() as usize;

    // 2. Закрыть ПРОПАВШИЕ (ключа нет во входящем наборе).
    let closed = sqlx::query(
        "UPDATE cruise_provider_sales h SET valid_to = $6
         WHERE h.cruise_provider_id = $1
           AND h.valid_to IS NULL
           AND NOT EXISTS (
               SELECT 1
               FROM UNNEST($2::text[], $3::text[], $4::text[], $5::bool[])
                    AS i(cruise, class, room, nofull)
               WHERE i.cruise = h.cruise_provider_cruise_id
                 AND i.class  = h.cruise_provider_class_id
                 AND i.room   = h.cruise_provider_room_id
                 AND i.nofull = h.partial_buyout
           )",
    )
    .bind(&provider_id.0)
    .bind(&cruise)
    .bind(&class)
    .bind(&room)
    .bind(&nofull)
    .bind(now)
    .execute(&mut **tx)
    .await
    .map_err(tx_err)?
    .rows_affected() as usize;

    // 3. Открыть новые версии (новые ключи + закрытые в шаге 1).
    let created = sqlx::query(
        "INSERT INTO cruise_provider_sales
             (cruise_provider_id, cruise_provider_cruise_id, cruise_provider_class_id,
              cruise_provider_room_id, partial_buyout, base_price, currency, valid_from)
         SELECT $1, i.cruise, i.class, i.room, i.nofull, i.base, i.curr, $8
         FROM UNNEST($2::text[], $3::text[], $4::text[], $5::bool[],
                     $6::numeric[], $7::text[])
              AS i(cruise, class, room, nofull, base, curr)
         WHERE NOT EXISTS (
             SELECT 1 FROM cruise_provider_sales h
             WHERE h.cruise_provider_id      = $1
               AND h.cruise_provider_cruise_id = i.cruise
               AND h.cruise_provider_class_id  = i.class
               AND h.cruise_provider_room_id   = i.room
               AND h.partial_buyout            = i.nofull
               AND h.valid_to IS NULL
         )",
    )
    .bind(&provider_id.0)
    .bind(&cruise)
    .bind(&class)
    .bind(&room)
    .bind(&nofull)
    .bind(&base)
    .bind(&curr)
    .bind(now)
    .execute(&mut **tx)
    .await
    .map_err(tx_err)?
    .rows_affected() as usize;

    Ok(ScdOutcome {
        created,
        updated,
        closed,
    })
}

/// Для больших наборов: TEMP TABLE + PK, батчевая загрузка.
async fn apply_with_temp_table(
    tx: &mut Transaction<'_, Postgres>,
    provider_id: &ProviderId,
    sales: &[CanonicalSale],
    batch_size: usize,
    now: DateTime<Utc>,
) -> Result<ScdOutcome, RepositoryError> {
    // Идемпотентно: если таблица уже создана в этой транзакции (повторный
    // вызов в одном tx — например, из тестов или retry), дропаем и создаём
    // заново. ON COMMIT DROP сам сработает только на COMMIT, а не между
    // вызовами внутри одной транзакции.
    sqlx::query("DROP TABLE IF EXISTS incoming_sales")
        .execute(&mut **tx)
        .await
        .map_err(tx_err)?;

    sqlx::query(
        "CREATE TEMP TABLE incoming_sales (
             cruise_id      TEXT NOT NULL,
             class_id       TEXT NOT NULL,
             room_id        TEXT NOT NULL,
             partial_buyout BOOLEAN NOT NULL,
             base_price     NUMERIC(12,2) NOT NULL,
             currency       TEXT NOT NULL,
             PRIMARY KEY (cruise_id, class_id, room_id, partial_buyout)
         ) ON COMMIT DROP",
    )
    .execute(&mut **tx)
    .await
    .map_err(tx_err)?;

    let bs = batch_size.max(1);
    for chunk in sales.chunks(bs) {
        let cruise: Vec<&str> = chunk
            .iter()
            .map(|s| s.external_cruise_id.as_str())
            .collect();
        let class: Vec<&str> = chunk.iter().map(|s| s.external_class_id.as_str()).collect();
        let room: Vec<&str> = chunk.iter().map(|s| s.external_room_id.as_str()).collect();
        let nofull: Vec<bool> = chunk
            .iter()
            .map(|s| s.partial_buyout.unwrap_or(false))
            .collect();
        let base: Vec<Decimal> = chunk.iter().map(|s| s.base_price).collect();
        let curr: Vec<&str> = chunk.iter().map(|s| s.currency.as_str()).collect();

        sqlx::query(
            "INSERT INTO incoming_sales
                (cruise_id, class_id, room_id, partial_buyout, base_price, currency)
             SELECT * FROM UNNEST($1::text[], $2::text[], $3::text[],
                                  $4::bool[], $5::numeric[], $6::text[])
             ON CONFLICT (cruise_id, class_id, room_id, partial_buyout) DO NOTHING",
        )
        .bind(&cruise)
        .bind(&class)
        .bind(&room)
        .bind(&nofull)
        .bind(&base)
        .bind(&curr)
        .execute(&mut **tx)
        .await
        .map_err(tx_err)?;
    }

    let updated = sqlx::query(
        "UPDATE cruise_provider_sales h SET valid_to = $2
         WHERE h.cruise_provider_id = $1 AND h.valid_to IS NULL
           AND EXISTS (
               SELECT 1 FROM incoming_sales i
               WHERE i.cruise_id      = h.cruise_provider_cruise_id
                 AND i.class_id       = h.cruise_provider_class_id
                 AND i.room_id        = h.cruise_provider_room_id
                 AND i.partial_buyout = h.partial_buyout
                 AND (i.base_price <> h.base_price
                      OR i.currency   <> h.currency)
           )",
    )
    .bind(&provider_id.0)
    .bind(now)
    .execute(&mut **tx)
    .await
    .map_err(tx_err)?
    .rows_affected() as usize;

    let closed = sqlx::query(
        "UPDATE cruise_provider_sales h SET valid_to = $2
         WHERE h.cruise_provider_id = $1 AND h.valid_to IS NULL
           AND NOT EXISTS (
               SELECT 1 FROM incoming_sales i
               WHERE i.cruise_id      = h.cruise_provider_cruise_id
                 AND i.class_id       = h.cruise_provider_class_id
                 AND i.room_id        = h.cruise_provider_room_id
                 AND i.partial_buyout = h.partial_buyout
           )",
    )
    .bind(&provider_id.0)
    .bind(now)
    .execute(&mut **tx)
    .await
    .map_err(tx_err)?
    .rows_affected() as usize;

    let created = sqlx::query(
        "INSERT INTO cruise_provider_sales
             (cruise_provider_id, cruise_provider_cruise_id, cruise_provider_class_id,
              cruise_provider_room_id, partial_buyout, base_price, currency, valid_from)
         SELECT $1, i.cruise_id, i.class_id, i.room_id,
                i.partial_buyout, i.base_price, i.currency, $2
         FROM incoming_sales i
         WHERE NOT EXISTS (
             SELECT 1 FROM cruise_provider_sales h
             WHERE h.cruise_provider_id      = $1
               AND h.cruise_provider_cruise_id = i.cruise_id
               AND h.cruise_provider_class_id  = i.class_id
               AND h.cruise_provider_room_id   = i.room_id
               AND h.partial_buyout            = i.partial_buyout
               AND h.valid_to IS NULL
         )",
    )
    .bind(&provider_id.0)
    .bind(now)
    .execute(&mut **tx)
    .await
    .map_err(tx_err)?
    .rows_affected() as usize;

    Ok(ScdOutcome {
        created,
        updated,
        closed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repositories::postgres::test_support::{
        cleanup_provider, db_lock, insert_provider, pool, purge_all_test_rows, test_provider_id,
    };
    use chrono::Duration;

    fn sale(cruise: &str, class: &str, room: &str, price: &str) -> CanonicalSale {
        CanonicalSale {
            external_cruise_id: cruise.into(),
            external_class_id: class.into(),
            external_room_id: room.into(),
            base_price: price.parse().unwrap(),
            partial_buyout: Some(false),
            currency: "RUB".into(),
        }
    }

    async fn count_open(tx: &mut Transaction<'_, Postgres>, provider_id: &str) -> i64 {
        sqlx::query_scalar(
            "SELECT count(*) FROM cruise_provider_sales
             WHERE cruise_provider_id = $1 AND valid_to IS NULL",
        )
        .bind(provider_id)
        .fetch_one(&mut **tx)
        .await
        .expect("count open")
    }

    async fn count_for_room(
        tx: &mut Transaction<'_, Postgres>,
        provider_id: &str,
        room: &str,
    ) -> i64 {
        sqlx::query_scalar(
            "SELECT count(*) FROM cruise_provider_sales
             WHERE cruise_provider_id = $1 AND cruise_provider_room_id = $2",
        )
        .bind(provider_id)
        .bind(room)
        .fetch_one(&mut **tx)
        .await
        .expect("count by room")
    }

    #[tokio::test]
    async fn small_path_price_change_creates_new_version() {
        let _guard = db_lock().await;
        let pool_owned = pool().await;
        let pool = &pool_owned;
        purge_all_test_rows(pool).await;
        let pid = test_provider_id("scd2");
        insert_provider(pool, &pid).await;

        let mut tx = pool.begin().await.expect("begin");
        let provider = ProviderId(pid.clone());
        let now = Utc::now();

        apply_sales_scd2(
            &mut tx,
            &provider,
            &[
                sale("c1", "cls1", "r1", "100.00"),
                sale("c1", "cls1", "r2", "200.00"),
            ],
            100,
            now,
        )
        .await
        .expect("baseline");
        assert_eq!(count_open(&mut tx, &pid).await, 2);

        let outcome = apply_sales_scd2(
            &mut tx,
            &provider,
            &[
                sale("c1", "cls1", "r1", "150.00"),
                sale("c1", "cls1", "r2", "200.00"),
            ],
            100,
            now + Duration::seconds(60),
        )
        .await
        .expect("change");

        assert_eq!(outcome.created, 1);
        assert_eq!(outcome.updated, 1);
        assert_eq!(outcome.closed, 0);
        assert_eq!(count_for_room(&mut tx, &pid, "r1").await, 2);
        assert_eq!(count_for_room(&mut tx, &pid, "r2").await, 1);

        tx.rollback().await.ok();
        cleanup_provider(pool, &pid).await;
    }

    #[tokio::test]
    async fn large_path_price_change_creates_new_version() {
        let _guard = db_lock().await;
        let pool_owned = pool().await;
        let pool = &pool_owned;
        purge_all_test_rows(pool).await;
        let pid = test_provider_id("scd2");
        insert_provider(pool, &pid).await;

        let mut tx = pool.begin().await.expect("begin");
        let provider = ProviderId(pid.clone());
        let now = Utc::now();

        let mut baseline: Vec<CanonicalSale> = Vec::with_capacity(200);
        for i in 0..200 {
            baseline.push(sale("c1", "cls1", &format!("r{i}"), "100.00"));
        }
        apply_sales_scd2(&mut tx, &provider, &baseline, 500, now)
            .await
            .expect("baseline");
        assert_eq!(count_open(&mut tx, &pid).await, 200);

        let mut changed = baseline.clone();
        changed[0] = sale("c1", "cls1", "r0", "150.00");

        let outcome = apply_sales_scd2(
            &mut tx,
            &provider,
            &changed,
            500,
            now + Duration::seconds(60),
        )
        .await
        .expect("change");

        assert_eq!(outcome.created, 1);
        assert_eq!(outcome.updated, 1);
        assert_eq!(count_for_room(&mut tx, &pid, "r0").await, 2);
        assert_eq!(count_for_room(&mut tx, &pid, "r1").await, 1);

        tx.rollback().await.ok();
        cleanup_provider(pool, &pid).await;
    }

    #[tokio::test]
    async fn unchanged_sales_do_not_create_new_version() {
        let _guard = db_lock().await;
        let pool_owned = pool().await;
        let pool = &pool_owned;
        purge_all_test_rows(pool).await;
        let pid = test_provider_id("scd2");
        insert_provider(pool, &pid).await;

        let mut tx = pool.begin().await.expect("begin");
        let provider = ProviderId(pid.clone());
        let now = Utc::now();
        let sales = vec![sale("c1", "cls1", "r1", "100.00")];

        apply_sales_scd2(&mut tx, &provider, &sales, 100, now)
            .await
            .expect("baseline");

        let outcome =
            apply_sales_scd2(&mut tx, &provider, &sales, 100, now + Duration::seconds(60))
                .await
                .expect("second");

        assert_eq!(outcome.created, 0);
        assert_eq!(outcome.updated, 0);
        assert_eq!(outcome.closed, 0);
        assert_eq!(count_open(&mut tx, &pid).await, 1);

        tx.rollback().await.ok();
        cleanup_provider(pool, &pid).await;
    }

    #[tokio::test]
    async fn empty_input_closes_all_open_versions() {
        let _guard = db_lock().await;
        let pool_owned = pool().await;
        let pool = &pool_owned;
        purge_all_test_rows(pool).await;
        let pid = test_provider_id("scd2");
        insert_provider(pool, &pid).await;

        let mut tx = pool.begin().await.expect("begin");
        let provider = ProviderId(pid.clone());
        let now = Utc::now();

        apply_sales_scd2(
            &mut tx,
            &provider,
            &[sale("c1", "cls1", "r1", "100.00")],
            100,
            now,
        )
        .await
        .expect("baseline");
        assert_eq!(count_open(&mut tx, &pid).await, 1);

        let outcome = apply_sales_scd2(&mut tx, &provider, &[], 100, now + Duration::seconds(60))
            .await
            .expect("empty");

        assert_eq!(outcome.created, 0);
        assert_eq!(outcome.updated, 0);
        assert_eq!(outcome.closed, 1);
        assert_eq!(count_open(&mut tx, &pid).await, 0);

        tx.rollback().await.ok();
        cleanup_provider(pool, &pid).await;
    }

    #[tokio::test]
    async fn currency_change_creates_new_version() {
        let _guard = db_lock().await;
        let pool_owned = pool().await;
        let pool = &pool_owned;
        purge_all_test_rows(pool).await;
        let pid = test_provider_id("scd2");
        insert_provider(pool, &pid).await;

        let mut tx = pool.begin().await.expect("begin");
        let provider = ProviderId(pid.clone());
        let now = Utc::now();

        let mut s = sale("c1", "cls1", "r1", "100.00");
        apply_sales_scd2(&mut tx, &provider, &[s.clone()], 100, now)
            .await
            .expect("baseline");

        s.currency = "USD".into();
        let outcome = apply_sales_scd2(&mut tx, &provider, &[s], 100, now + Duration::seconds(60))
            .await
            .expect("change");

        assert_eq!(outcome.created, 1);
        assert_eq!(outcome.updated, 1);
        assert_eq!(count_for_room(&mut tx, &pid, "r1").await, 2);

        tx.rollback().await.ok();
        cleanup_provider(pool, &pid).await;
    }
}
