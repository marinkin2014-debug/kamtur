use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use sqlx::{Postgres, Transaction};

use domain::entities::*;
use domain::errors::RepositoryError;

use super::error::tx_err;

#[derive(Debug, Default)]
pub(super) struct ScdOutcome {
    pub created: usize,
    pub updated: usize,
    pub closed: usize,
}

pub(super) async fn apply_prices_scd2(
    tx: &mut Transaction<'_, Postgres>,
    provider_id: &ProviderId,
    prices: &[CanonicalPrice],
    batch_size: usize,
    now: DateTime<Utc>,
) -> Result<ScdOutcome, RepositoryError> {
    sqlx::query("DROP TABLE IF EXISTS incoming_prices")
        .execute(&mut **tx)
        .await
        .map_err(tx_err)?;

    sqlx::query(
        "CREATE TEMP TABLE incoming_prices (
             cruise_id      TEXT NOT NULL,
             class_id       TEXT NOT NULL,
             partial_buyout BOOLEAN NOT NULL,
             base_price     NUMERIC(12,2) NOT NULL,
             child_price    NUMERIC(12,2),
             extra_seat     NUMERIC(12,2),
             currency       TEXT NOT NULL,
             PRIMARY KEY (cruise_id, class_id, partial_buyout)
         ) ON COMMIT DROP",
    )
    .execute(&mut **tx)
    .await
    .map_err(tx_err)?;

    let bs = batch_size.max(1);
    for chunk in prices.chunks(bs) {
        let cruise: Vec<&str> = chunk
            .iter()
            .map(|p| p.external_cruise_id.as_str())
            .collect();
        let class: Vec<&str> = chunk.iter().map(|p| p.external_class_id.as_str()).collect();
        let nofull: Vec<bool> = chunk
            .iter()
            .map(|p| p.partial_buyout.unwrap_or(false))
            .collect();
        let base: Vec<Decimal> = chunk.iter().map(|p| p.base_price).collect();
        let child: Vec<Option<Decimal>> = chunk.iter().map(|p| p.child_price).collect();
        let extra: Vec<Option<Decimal>> = chunk.iter().map(|p| p.extra_seat).collect();
        let curr: Vec<&str> = chunk.iter().map(|p| p.currency.as_str()).collect();

        sqlx::query(
            "INSERT INTO incoming_prices
                (cruise_id, class_id, partial_buyout, base_price,
                 child_price, extra_seat, currency)
             SELECT * FROM UNNEST($1::text[], $2::text[], $3::bool[], $4::numeric[],
                                  $5::numeric[], $6::numeric[], $7::text[])
             ON CONFLICT (cruise_id, class_id, partial_buyout) DO NOTHING",
        )
        .bind(&cruise)
        .bind(&class)
        .bind(&nofull)
        .bind(&base)
        .bind(&child)
        .bind(&extra)
        .bind(&curr)
        .execute(&mut **tx)
        .await
        .map_err(tx_err)?;
    }

    // 1. Закрыть изменившиеся (только внутри одного partial_buyout)
    let updated = sqlx::query(
        "UPDATE cruise_provider_prices h SET valid_to = $2
         WHERE h.cruise_provider_id = $1 AND h.valid_to IS NULL
           AND EXISTS (
               SELECT 1 FROM incoming_prices i
               WHERE i.cruise_id      = h.cruise_provider_cruise_id
                 AND i.class_id       = h.cruise_provider_class_id
                 AND i.partial_buyout = h.partial_buyout
                 AND (i.base_price  <> h.base_price
                      OR i.child_price IS DISTINCT FROM h.child_price
                      OR i.extra_seat  IS DISTINCT FROM h.extra_seat
                      OR i.currency    <> h.currency)
           )",
    )
    .bind(&provider_id.0)
    .bind(now)
    .execute(&mut **tx)
    .await
    .map_err(tx_err)?
    .rows_affected() as usize;

    // 2. Закрыть пропавшие
    let closed = sqlx::query(
        "UPDATE cruise_provider_prices h SET valid_to = $2
         WHERE h.cruise_provider_id = $1 AND h.valid_to IS NULL
           AND NOT EXISTS (
               SELECT 1 FROM incoming_prices i
               WHERE i.cruise_id      = h.cruise_provider_cruise_id
                 AND i.class_id       = h.cruise_provider_class_id
                 AND i.partial_buyout = h.partial_buyout
           )",
    )
    .bind(&provider_id.0)
    .bind(now)
    .execute(&mut **tx)
    .await
    .map_err(tx_err)?
    .rows_affected() as usize;

    // 3. Открыть новые версии
    let created = sqlx::query(
        "INSERT INTO cruise_provider_prices
             (cruise_provider_id, cruise_provider_cruise_id, cruise_provider_class_id,
              partial_buyout, base_price, child_price, extra_seat, currency, valid_from)
         SELECT $1, i.cruise_id, i.class_id, i.partial_buyout,
                i.base_price, i.child_price, i.extra_seat, i.currency, $2
         FROM incoming_prices i
         WHERE NOT EXISTS (
             SELECT 1 FROM cruise_provider_prices h
             WHERE h.cruise_provider_id      = $1
               AND h.cruise_provider_cruise_id = i.cruise_id
               AND h.cruise_provider_class_id  = i.class_id
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

    fn price(cruise: &str, class: &str, base: &str) -> CanonicalPrice {
        CanonicalPrice {
            external_cruise_id: cruise.into(),
            external_class_id: class.into(),
            base_price: base.parse().unwrap(),
            partial_buyout: Some(false),
            child_price: None,
            extra_seat: None,
            currency: "RUB".into(),
        }
    }

    async fn count_open(tx: &mut Transaction<'_, Postgres>, provider_id: &str) -> i64 {
        sqlx::query_scalar(
            "SELECT count(*) FROM cruise_provider_prices
             WHERE cruise_provider_id = $1 AND valid_to IS NULL",
        )
        .bind(provider_id)
        .fetch_one(&mut **tx)
        .await
        .expect("count open")
    }

    async fn count_for_class(
        tx: &mut Transaction<'_, Postgres>,
        provider_id: &str,
        class: &str,
    ) -> i64 {
        sqlx::query_scalar(
            "SELECT count(*) FROM cruise_provider_prices
             WHERE cruise_provider_id = $1 AND cruise_provider_class_id = $2",
        )
        .bind(provider_id)
        .bind(class)
        .fetch_one(&mut **tx)
        .await
        .expect("count by class")
    }

    /// Изменение base_price → новая версия.
    #[tokio::test]
    async fn base_price_change_creates_new_version() {
        let _guard = db_lock().await;
        let pool_owned = pool().await;
        let pool = &pool_owned;
        purge_all_test_rows(pool).await;
        let pid = test_provider_id("prices");
        insert_provider(pool, &pid).await;

        let mut tx = pool.begin().await.expect("begin");
        let provider = ProviderId(pid.clone());
        let now = Utc::now();

        apply_prices_scd2(
            &mut tx,
            &provider,
            &[price("c1", "cls1", "50000.00")],
            100,
            now,
        )
        .await
        .expect("baseline");

        let outcome = apply_prices_scd2(
            &mut tx,
            &provider,
            &[price("c1", "cls1", "55000.00")],
            100,
            now + Duration::seconds(60),
        )
        .await
        .expect("change");

        assert_eq!(outcome.created, 1);
        assert_eq!(outcome.updated, 1);
        assert_eq!(outcome.closed, 0);
        assert_eq!(count_for_class(&mut tx, &pid, "cls1").await, 2);

        tx.rollback().await.ok();
        cleanup_provider(pool, &pid).await;
    }

    /// Изменение child_price → новая версия (отличие от sales).
    #[tokio::test]
    async fn child_price_change_creates_new_version() {
        let _guard = db_lock().await;
        let pool_owned = pool().await;
        let pool = &pool_owned;
        purge_all_test_rows(pool).await;
        let pid = test_provider_id("prices");
        insert_provider(pool, &pid).await;

        let mut tx = pool.begin().await.expect("begin");
        let provider = ProviderId(pid.clone());
        let now = Utc::now();

        let mut p = price("c1", "cls1", "50000.00");
        p.child_price = Some("5000.00".parse().unwrap());

        apply_prices_scd2(&mut tx, &provider, std::slice::from_ref(&p), 100, now)
            .await
            .expect("baseline");

        p.child_price = Some("6000.00".parse().unwrap());
        let outcome = apply_prices_scd2(&mut tx, &provider, &[p], 100, now + Duration::seconds(60))
            .await
            .expect("change");

        assert_eq!(outcome.created, 1);
        assert_eq!(outcome.updated, 1);

        tx.rollback().await.ok();
        cleanup_provider(pool, &pid).await;
    }

    /// Изменение extra_seat → новая версия.
    #[tokio::test]
    async fn extra_seat_change_creates_new_version() {
        let _guard = db_lock().await;
        let pool_owned = pool().await;
        let pool = &pool_owned;
        purge_all_test_rows(pool).await;
        let pid = test_provider_id("prices");
        insert_provider(pool, &pid).await;

        let mut tx = pool.begin().await.expect("begin");
        let provider = ProviderId(pid.clone());
        let now = Utc::now();

        let mut p = price("c1", "cls1", "50000.00");
        p.extra_seat = Some("1000.00".parse().unwrap());

        apply_prices_scd2(&mut tx, &provider, std::slice::from_ref(&p), 100, now)
            .await
            .expect("baseline");

        p.extra_seat = Some("2000.00".parse().unwrap());
        let outcome = apply_prices_scd2(&mut tx, &provider, &[p], 100, now + Duration::seconds(60))
            .await
            .expect("change");

        assert_eq!(outcome.created, 1);
        assert_eq!(outcome.updated, 1);

        tx.rollback().await.ok();
        cleanup_provider(pool, &pid).await;
    }

    /// Идентичные данные → новых версий нет.
    #[tokio::test]
    async fn unchanged_prices_do_not_create_new_version() {
        let _guard = db_lock().await;
        let pool_owned = pool().await;
        let pool = &pool_owned;
        purge_all_test_rows(pool).await;
        let pid = test_provider_id("prices");
        insert_provider(pool, &pid).await;

        let mut tx = pool.begin().await.expect("begin");
        let provider = ProviderId(pid.clone());
        let now = Utc::now();

        let p = price("c1", "cls1", "50000.00");
        apply_prices_scd2(&mut tx, &provider, std::slice::from_ref(&p), 100, now)
            .await
            .expect("baseline");

        let outcome = apply_prices_scd2(&mut tx, &provider, &[p], 100, now + Duration::seconds(60))
            .await
            .expect("second");

        assert_eq!(outcome.created, 0);
        assert_eq!(outcome.updated, 0);
        assert_eq!(outcome.closed, 0);
        assert_eq!(count_open(&mut tx, &pid).await, 1);

        tx.rollback().await.ok();
        cleanup_provider(pool, &pid).await;
    }

    /// partial_buyout как часть ключа — два price для одной (cruise,class) с разным
    /// nofull → две независимые версии.
    #[tokio::test]
    async fn partial_buyout_is_part_of_key() {
        let _guard = db_lock().await;
        let pool_owned = pool().await;
        let pool = &pool_owned;
        purge_all_test_rows(pool).await;
        let pid = test_provider_id("prices");
        insert_provider(pool, &pid).await;

        let mut tx = pool.begin().await.expect("begin");
        let provider = ProviderId(pid.clone());
        let now = Utc::now();

        let p_full = price("c1", "cls1", "50000.00");
        let mut p_partial = price("c1", "cls1", "60000.00");
        p_partial.partial_buyout = Some(true);

        apply_prices_scd2(&mut tx, &provider, &[p_full, p_partial], 100, now)
            .await
            .expect("baseline");

        // 2 открытых версии для одного (cruise, class) — по одной на каждый nofull
        assert_eq!(count_open(&mut tx, &pid).await, 2);
        assert_eq!(count_for_class(&mut tx, &pid, "cls1").await, 2);

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
        let pid = test_provider_id("prices");
        insert_provider(pool, &pid).await;

        let mut tx = pool.begin().await.expect("begin");
        let provider = ProviderId(pid.clone());
        let now = Utc::now();

        apply_prices_scd2(
            &mut tx,
            &provider,
            &[price("c1", "cls1", "50000.00")],
            100,
            now,
        )
        .await
        .expect("baseline");
        assert_eq!(count_open(&mut tx, &pid).await, 1);

        let outcome = apply_prices_scd2(&mut tx, &provider, &[], 100, now + Duration::seconds(60))
            .await
            .expect("empty");

        assert_eq!(outcome.created, 0);
        assert_eq!(outcome.updated, 0);
        assert_eq!(outcome.closed, 1);
        assert_eq!(count_open(&mut tx, &pid).await, 0);

        tx.rollback().await.ok();
        cleanup_provider(pool, &pid).await;
    }
}
