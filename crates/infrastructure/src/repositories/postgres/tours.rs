use chrono::{DateTime, Utc};
use sqlx::{Postgres, Transaction};

use domain::entities::*;
use domain::errors::RepositoryError;

use super::error::tx_err;

pub(super) async fn upsert_tours(
    tx: &mut Transaction<'_, Postgres>,
    provider_id: &ProviderId,
    tours: &[CanonicalTour],
    enriched: &[EnrichedTour],
    batch_size: usize,
    now: DateTime<Utc>,
) -> Result<(), RepositoryError> {
    if tours.is_empty() {
        return Ok(());
    }

    let mut by_id: std::collections::HashMap<&str, &EnrichedTour> =
        std::collections::HashMap::with_capacity(enriched.len());
    for e in enriched {
        by_id.insert(e.external_cruise_id.as_str(), e);
    }

    let bs = batch_size.max(1);
    for chunk in tours.chunks(bs) {
        let n = chunk.len();
        let mut obj: Vec<&str> = Vec::with_capacity(n);
        let mut cid: Vec<&str> = Vec::with_capacity(n);
        let mut ctype: Vec<i64> = Vec::with_capacity(n);
        let mut bdate: Vec<chrono::NaiveDate> = Vec::with_capacity(n);
        let mut btime: Vec<Option<chrono::NaiveTime>> = Vec::with_capacity(n);
        let mut edate: Vec<chrono::NaiveDate> = Vec::with_capacity(n);
        let mut etime: Vec<Option<chrono::NaiveTime>> = Vec::with_capacity(n);
        let mut name: Vec<&str> = Vec::with_capacity(n);
        let mut site_name: Vec<Option<String>> = Vec::with_capacity(n);
        let mut site_coid: Vec<Option<String>> = Vec::with_capacity(n);
        let mut route: Vec<Option<String>> = Vec::with_capacity(n);
        let mut city_from: Vec<Option<String>> = Vec::with_capacity(n);
        let mut city_to: Vec<Option<String>> = Vec::with_capacity(n);
        let mut dep_city: Vec<Option<String>> = Vec::with_capacity(n);
        let mut days: Vec<Option<i32>> = Vec::with_capacity(n);
        let mut is_ret: Vec<Option<bool>> = Vec::with_capacity(n);
        let mut is_week: Vec<Option<bool>> = Vec::with_capacity(n);

        for t in chunk {
            let e = by_id.get(t.external_cruise_id.as_str());
            let get = |f: &str| e.and_then(|x| x.get(f).map(String::from));
            let get_bool = |f: &str| get(f).and_then(|v| v.parse().ok());
            let get_i32 = |f: &str| get(f).and_then(|v| v.parse().ok());

            obj.push(&t.external_object_id);
            cid.push(&t.external_cruise_id);
            ctype.push(t.cruise_type_id);
            bdate.push(t.begin_date);
            btime.push(t.begin_time);
            edate.push(t.end_date);
            etime.push(t.end_time);
            name.push(&t.name);
            site_name.push(get("site_name"));
            site_coid.push(get("site_cruise_object_id"));
            route.push(get("route"));
            city_from.push(get("city_from"));
            city_to.push(get("city_to"));
            dep_city.push(get("departure_city"));
            days.push(get_i32("days"));
            is_ret.push(get_bool("is_return"));
            is_week.push(get_bool("is_weekend"));
        }

        sqlx::query(
            "INSERT INTO cruise_provider_tours
                 (cruise_provider_id, cruise_provider_object_id, cruise_provider_cruise_id,
                  cruise_type_id, begin_date, begin_time, end_date, end_time, name,
                  site_name, site_cruise_object_id, route, city_from, city_to, departure_city,
                  days, is_return, is_weekend, is_active, updated_at)
             SELECT $1,
                    i.obj, i.cid, i.ctype, i.bdate, i.btime, i.edate, i.etime, i.name,
                    i.site_name, i.site_coid, i.route, i.city_from, i.city_to, i.dep_city,
                    i.days, i.is_ret, i.is_week,
                    true, $19
             FROM UNNEST(
                 $2::text[], $3::text[], $4::bigint[],
                 $5::date[], $6::time[], $7::date[], $8::time[], $9::text[],
                 $10::text[], $11::text[], $12::text[], $13::text[], $14::text[], $15::text[],
                 $16::int[], $17::bool[], $18::bool[]
             ) AS i(obj, cid, ctype, bdate, btime, edate, etime, name,
                    site_name, site_coid, route, city_from, city_to, dep_city,
                    days, is_ret, is_week)
             ON CONFLICT (cruise_provider_id, cruise_provider_cruise_id) DO UPDATE SET
                 cruise_provider_object_id = EXCLUDED.cruise_provider_object_id,
                 cruise_type_id = EXCLUDED.cruise_type_id,
                 begin_date = EXCLUDED.begin_date,
                 begin_time = EXCLUDED.begin_time,
                 end_date = EXCLUDED.end_date,
                 end_time = EXCLUDED.end_time,
                 name = EXCLUDED.name,
                 site_name = EXCLUDED.site_name,
                 site_cruise_object_id = EXCLUDED.site_cruise_object_id,
                 route = EXCLUDED.route,
                 city_from = EXCLUDED.city_from,
                 city_to = EXCLUDED.city_to,
                 departure_city = EXCLUDED.departure_city,
                 days = EXCLUDED.days,
                 is_return = EXCLUDED.is_return,
                 is_weekend = EXCLUDED.is_weekend,
                 is_active = true,
                 updated_at = EXCLUDED.updated_at",
        )
        .bind(&provider_id.0)
        .bind(&obj)
        .bind(&cid)
        .bind(&ctype)
        .bind(&bdate)
        .bind(&btime)
        .bind(&edate)
        .bind(&etime)
        .bind(&name)
        .bind(&site_name)
        .bind(&site_coid)
        .bind(&route)
        .bind(&city_from)
        .bind(&city_to)
        .bind(&dep_city)
        .bind(&days)
        .bind(&is_ret)
        .bind(&is_week)
        .bind(now)
        .execute(&mut **tx)
        .await
        .map_err(tx_err)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repositories::postgres::test_support::{
        cleanup_provider, db_lock, insert_provider, pool, purge_all_test_rows, test_provider_id,
    };
    use chrono::NaiveDate;

    fn tour(id: &str, obj: &str, name: &str) -> CanonicalTour {
        CanonicalTour {
            external_object_id: obj.into(),
            external_cruise_id: id.into(),
            cruise_type_id: 1,
            begin_date: NaiveDate::from_ymd_opt(2026, 9, 1).unwrap(),
            begin_time: None,
            end_date: NaiveDate::from_ymd_opt(2026, 9, 5).unwrap(),
            end_time: None,
            name: name.into(),
        }
    }

    fn enriched(id: &str, site_name: &str) -> EnrichedTour {
        let mut e = EnrichedTour {
            external_cruise_id: id.into(),
            fields: std::collections::HashMap::new(),
        };
        e.set("site_name", site_name.into());
        e
    }

    #[tokio::test]
    async fn upsert_tours_with_enrichment() {
        let _guard = db_lock().await;
        let pool_owned = pool().await;
        let pool = &pool_owned;
        purge_all_test_rows(pool).await;
        let pid = test_provider_id("tour");
        insert_provider(pool, &pid).await;

        let mut tx = pool.begin().await.expect("begin");
        upsert_tours(
            &mut tx,
            &ProviderId(pid.clone()),
            &[tour("c1", "o1", "Perm-Samara")],
            &[enriched("c1", "Site Name")],
            100,
            Utc::now(),
        )
        .await
        .expect("insert");
        tx.commit().await.expect("commit");

        let (name, site_name): (String, Option<String>) = sqlx::query_as(
            "SELECT name, site_name FROM cruise_provider_tours
             WHERE cruise_provider_id = $1 AND cruise_provider_cruise_id = 'c1'",
        )
        .bind(&pid)
        .fetch_one(pool)
        .await
        .expect("fetch");

        assert_eq!(name, "Perm-Samara");
        assert_eq!(site_name.as_deref(), Some("Site Name"));

        cleanup_provider(pool, &pid).await;
    }

    #[tokio::test]
    async fn upsert_tours_without_enrichment_has_null_fields() {
        let _guard = db_lock().await;
        let pool_owned = pool().await;
        let pool = &pool_owned;
        purge_all_test_rows(pool).await;
        let pid = test_provider_id("tour");
        insert_provider(pool, &pid).await;

        let mut tx = pool.begin().await.expect("begin");
        upsert_tours(
            &mut tx,
            &ProviderId(pid.clone()),
            &[tour("c1", "o1", "Route")],
            &[],
            100,
            Utc::now(),
        )
        .await
        .expect("insert");
        tx.commit().await.expect("commit");

        let site_name: Option<String> = sqlx::query_scalar(
            "SELECT site_name FROM cruise_provider_tours
             WHERE cruise_provider_id = $1 AND cruise_provider_cruise_id = 'c1'",
        )
        .bind(&pid)
        .fetch_one(pool)
        .await
        .expect("fetch");
        assert!(site_name.is_none());

        cleanup_provider(pool, &pid).await;
    }

    #[tokio::test]
    async fn upsert_tours_updates_enriched_fields_on_conflict() {
        let _guard = db_lock().await;
        let pool_owned = pool().await;
        let pool = &pool_owned;
        purge_all_test_rows(pool).await;
        let pid = test_provider_id("tour");
        insert_provider(pool, &pid).await;

        let provider = ProviderId(pid.clone());
        let mut tx = pool.begin().await.expect("begin");
        upsert_tours(
            &mut tx,
            &provider,
            &[tour("c1", "o1", "R")],
            &[enriched("c1", "Old")],
            100,
            Utc::now(),
        )
        .await
        .expect("first");
        upsert_tours(
            &mut tx,
            &provider,
            &[tour("c1", "o1", "R")],
            &[enriched("c1", "New")],
            100,
            Utc::now(),
        )
        .await
        .expect("second");
        tx.commit().await.expect("commit");

        let site_name: Option<String> = sqlx::query_scalar(
            "SELECT site_name FROM cruise_provider_tours
             WHERE cruise_provider_id = $1 AND cruise_provider_cruise_id = 'c1'",
        )
        .bind(&pid)
        .fetch_one(pool)
        .await
        .expect("fetch");
        assert_eq!(site_name.as_deref(), Some("New"));

        cleanup_provider(pool, &pid).await;
    }
}
