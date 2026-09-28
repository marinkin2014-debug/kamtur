use chrono::{DateTime, Utc};
use sqlx::{Postgres, Transaction};

use domain::entities::*;
use domain::errors::RepositoryError;

use super::error::tx_err;

pub(super) async fn upsert_objects(
    tx: &mut Transaction<'_, Postgres>,
    provider_id: &ProviderId,
    objects: &[CanonicalObject],
    batch_size: usize,
    now: DateTime<Utc>,
) -> Result<(), RepositoryError> {
    if objects.is_empty() {
        return Ok(());
    }
    let bs = batch_size.max(1);
    for chunk in objects.chunks(bs) {
        let ids: Vec<&str> = chunk.iter().map(|o| o.external_id.as_str()).collect();
        let names: Vec<&str> = chunk.iter().map(|o| o.name.as_str()).collect();

        sqlx::query(
            "INSERT INTO cruise_provider_objects
                 (cruise_provider_id, cruise_provider_object_id, name, is_active, updated_at)
             SELECT $1, i.id, i.name, true, $4
             FROM UNNEST($2::text[], $3::text[]) AS i(id, name)
             ON CONFLICT (cruise_provider_id, cruise_provider_object_id) DO UPDATE SET
                 name = EXCLUDED.name,
                 is_active = true,
                 updated_at = EXCLUDED.updated_at",
        )
        .bind(&provider_id.0)
        .bind(&ids)
        .bind(&names)
        .bind(now)
        .execute(&mut **tx)
        .await
        .map_err(tx_err)?;
    }
    Ok(())
}

pub(super) async fn upsert_stages(
    tx: &mut Transaction<'_, Postgres>,
    provider_id: &ProviderId,
    stages: &[CanonicalStage],
    batch_size: usize,
    now: DateTime<Utc>,
) -> Result<(), RepositoryError> {
    if stages.is_empty() {
        return Ok(());
    }
    let bs = batch_size.max(1);
    for chunk in stages.chunks(bs) {
        let ids: Vec<&str> = chunk.iter().map(|s| s.external_id.as_str()).collect();
        let names: Vec<&str> = chunk.iter().map(|s| s.name.as_str()).collect();

        sqlx::query(
            "INSERT INTO cruise_provider_stages
                 (cruise_provider_id, cruise_provider_stage_id, name, is_active, updated_at)
             SELECT $1, i.id, i.name, true, $4
             FROM UNNEST($2::text[], $3::text[]) AS i(id, name)
             ON CONFLICT (cruise_provider_id, cruise_provider_stage_id) DO UPDATE SET
                 name = EXCLUDED.name,
                 is_active = true,
                 updated_at = EXCLUDED.updated_at",
        )
        .bind(&provider_id.0)
        .bind(&ids)
        .bind(&names)
        .bind(now)
        .execute(&mut **tx)
        .await
        .map_err(tx_err)?;
    }
    Ok(())
}

pub(super) async fn upsert_classes(
    tx: &mut Transaction<'_, Postgres>,
    provider_id: &ProviderId,
    classes: &[CanonicalClass],
    batch_size: usize,
    now: DateTime<Utc>,
) -> Result<(), RepositoryError> {
    if classes.is_empty() {
        return Ok(());
    }
    let bs = batch_size.max(1);
    for chunk in classes.chunks(bs) {
        let obj_ids: Vec<&str> = chunk
            .iter()
            .map(|c| c.external_object_id.as_str())
            .collect();
        let cls_ids: Vec<&str> = chunk.iter().map(|c| c.external_class_id.as_str()).collect();
        let names: Vec<&str> = chunk.iter().map(|c| c.name.as_str()).collect();
        let descs: Vec<Option<&str>> = chunk.iter().map(|c| c.description.as_deref()).collect();
        let seats: Vec<Option<i32>> = chunk.iter().map(|c| c.base_seats).collect();
        let tiers: Vec<Option<i32>> = chunk.iter().map(|c| c.tiers).collect();
        let nofull: Vec<Option<bool>> = chunk.iter().map(|c| c.partial_buyout).collect();

        sqlx::query(
            "INSERT INTO cruise_provider_classes
                 (cruise_provider_id, cruise_provider_object_id, cruise_provider_class_id,
                  name, description, base_seats, tiers, partial_buyout, is_active, updated_at)
             SELECT $1, i.obj, i.cls, i.name, i.descr, i.seats, i.tiers, i.nofull, true, $9
             FROM UNNEST($2::text[], $3::text[], $4::text[], $5::text[],
                         $6::int[], $7::int[], $8::bool[])
                  AS i(obj, cls, name, descr, seats, tiers, nofull)
             ON CONFLICT (cruise_provider_id, cruise_provider_class_id) DO UPDATE SET
                 cruise_provider_object_id = EXCLUDED.cruise_provider_object_id,
                 name = EXCLUDED.name,
                 description = EXCLUDED.description,
                 base_seats = EXCLUDED.base_seats,
                 tiers = EXCLUDED.tiers,
                 partial_buyout = EXCLUDED.partial_buyout,
                 is_active = true,
                 updated_at = EXCLUDED.updated_at",
        )
        .bind(&provider_id.0)
        .bind(&obj_ids)
        .bind(&cls_ids)
        .bind(&names)
        .bind(&descs)
        .bind(&seats)
        .bind(&tiers)
        .bind(&nofull)
        .bind(now)
        .execute(&mut **tx)
        .await
        .map_err(tx_err)?;
    }
    Ok(())
}

pub(super) async fn upsert_rooms(
    tx: &mut Transaction<'_, Postgres>,
    provider_id: &ProviderId,
    rooms: &[CanonicalRoom],
    batch_size: usize,
    now: DateTime<Utc>,
) -> Result<(), RepositoryError> {
    if rooms.is_empty() {
        return Ok(());
    }
    let bs = batch_size.max(1);
    for chunk in rooms.chunks(bs) {
        let obj: Vec<&str> = chunk
            .iter()
            .map(|r| r.external_object_id.as_str())
            .collect();
        let stage: Vec<&str> = chunk.iter().map(|r| r.external_stage_id.as_str()).collect();
        let cls: Vec<&str> = chunk.iter().map(|r| r.external_class_id.as_str()).collect();
        let rid: Vec<&str> = chunk.iter().map(|r| r.external_room_id.as_str()).collect();
        let number: Vec<&str> = chunk.iter().map(|r| r.number.as_str()).collect();

        sqlx::query(
            "INSERT INTO cruise_provider_rooms
                 (cruise_provider_id, cruise_provider_object_id, cruise_provider_stage_id,
                  cruise_provider_class_id, cruise_provider_room_id, number, is_active, updated_at)
             SELECT $1, i.obj, i.stage, i.cls, i.rid, i.number, true, $7
             FROM UNNEST($2::text[], $3::text[], $4::text[], $5::text[], $6::text[])
                  AS i(obj, stage, cls, rid, number)
             ON CONFLICT (cruise_provider_id, cruise_provider_room_id) DO UPDATE SET
                 cruise_provider_object_id = EXCLUDED.cruise_provider_object_id,
                 cruise_provider_stage_id = EXCLUDED.cruise_provider_stage_id,
                 cruise_provider_class_id = EXCLUDED.cruise_provider_class_id,
                 number = EXCLUDED.number,
                 is_active = true,
                 updated_at = EXCLUDED.updated_at",
        )
        .bind(&provider_id.0)
        .bind(&obj)
        .bind(&stage)
        .bind(&cls)
        .bind(&rid)
        .bind(&number)
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

    fn obj(id: &str, name: &str) -> CanonicalObject {
        CanonicalObject {
            external_id: id.into(),
            name: name.into(),
        }
    }

    fn stage(id: &str, name: &str) -> CanonicalStage {
        CanonicalStage {
            external_id: id.into(),
            name: name.into(),
        }
    }

    fn class(obj: &str, cls: &str, name: &str) -> CanonicalClass {
        CanonicalClass {
            external_object_id: obj.into(),
            external_class_id: cls.into(),
            name: name.into(),
            description: None,
            base_seats: None,
            tiers: None,
            partial_buyout: None,
        }
    }

    fn room(obj: &str, stage: &str, cls: &str, id: &str, num: &str) -> CanonicalRoom {
        CanonicalRoom {
            external_object_id: obj.into(),
            external_stage_id: stage.into(),
            external_class_id: cls.into(),
            external_room_id: id.into(),
            number: num.into(),
        }
    }

    // ============================================================
    // upsert_objects
    // ============================================================

    #[tokio::test]
    async fn upsert_objects_inserts_new() {
        let _guard = db_lock().await;
        let pool_owned = pool().await;
        let pool = &pool_owned;
        purge_all_test_rows(pool).await;
        let pid = test_provider_id("obj");
        insert_provider(pool, &pid).await;

        let mut tx = pool.begin().await.expect("begin");
        upsert_objects(
            &mut tx,
            &ProviderId(pid.clone()),
            &[obj("o1", "Ship 1"), obj("o2", "Ship 2")],
            100,
            Utc::now(),
        )
        .await
        .expect("upsert");
        tx.commit().await.expect("commit");

        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM cruise_provider_objects
             WHERE cruise_provider_id = $1 AND is_active = true",
        )
        .bind(&pid)
        .fetch_one(pool)
        .await
        .expect("count");
        assert_eq!(count, 2);

        cleanup_provider(pool, &pid).await;
    }

    #[tokio::test]
    async fn upsert_objects_updates_name_on_conflict() {
        let _guard = db_lock().await;
        let pool_owned = pool().await;
        let pool = &pool_owned;
        purge_all_test_rows(pool).await;
        let pid = test_provider_id("obj");
        insert_provider(pool, &pid).await;

        let provider = ProviderId(pid.clone());
        let mut tx = pool.begin().await.expect("begin");
        upsert_objects(&mut tx, &provider, &[obj("o1", "Old")], 100, Utc::now())
            .await
            .expect("first");
        upsert_objects(&mut tx, &provider, &[obj("o1", "New")], 100, Utc::now())
            .await
            .expect("second");
        tx.commit().await.expect("commit");

        let name: String = sqlx::query_scalar(
            "SELECT name FROM cruise_provider_objects
             WHERE cruise_provider_id = $1 AND cruise_provider_object_id = 'o1'",
        )
        .bind(&pid)
        .fetch_one(pool)
        .await
        .expect("fetch");
        assert_eq!(name, "New");

        cleanup_provider(pool, &pid).await;
    }

    #[tokio::test]
    async fn upsert_objects_reactivates_inactive() {
        let _guard = db_lock().await;
        let pool_owned = pool().await;
        let pool = &pool_owned;
        purge_all_test_rows(pool).await;
        let pid = test_provider_id("obj");
        insert_provider(pool, &pid).await;

        let provider = ProviderId(pid.clone());
        let mut tx = pool.begin().await.expect("begin");
        upsert_objects(&mut tx, &provider, &[obj("o1", "X")], 100, Utc::now())
            .await
            .expect("insert");
        tx.commit().await.expect("commit");

        // Вручную деактивируем
        sqlx::query(
            "UPDATE cruise_provider_objects SET is_active = false
             WHERE cruise_provider_id = $1 AND cruise_provider_object_id = 'o1'",
        )
        .bind(&pid)
        .execute(pool)
        .await
        .expect("deactivate");

        // Повторный upsert → реактивирует
        let mut tx = pool.begin().await.expect("begin");
        upsert_objects(&mut tx, &provider, &[obj("o1", "X")], 100, Utc::now())
            .await
            .expect("reactivate");
        tx.commit().await.expect("commit");

        let active: bool = sqlx::query_scalar(
            "SELECT is_active FROM cruise_provider_objects
             WHERE cruise_provider_id = $1 AND cruise_provider_object_id = 'o1'",
        )
        .bind(&pid)
        .fetch_one(pool)
        .await
        .expect("fetch");
        assert!(active);

        cleanup_provider(pool, &pid).await;
    }

    #[tokio::test]
    async fn upsert_objects_empty_input_is_noop() {
        let _guard = db_lock().await;
        let pool_owned = pool().await;
        let pool = &pool_owned;
        purge_all_test_rows(pool).await;
        let pid = test_provider_id("obj");
        insert_provider(pool, &pid).await;

        let mut tx = pool.begin().await.expect("begin");
        upsert_objects(&mut tx, &ProviderId(pid.clone()), &[], 100, Utc::now())
            .await
            .expect("empty");
        tx.commit().await.expect("commit");

        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM cruise_provider_objects WHERE cruise_provider_id = $1",
        )
        .bind(&pid)
        .fetch_one(pool)
        .await
        .expect("count");
        assert_eq!(count, 0);

        cleanup_provider(pool, &pid).await;
    }

    // ============================================================
    // upsert_stages
    // ============================================================

    #[tokio::test]
    async fn upsert_stages_insert_and_update() {
        let _guard = db_lock().await;
        let pool_owned = pool().await;
        let pool = &pool_owned;
        purge_all_test_rows(pool).await;
        let pid = test_provider_id("stage");
        insert_provider(pool, &pid).await;

        let provider = ProviderId(pid.clone());
        let mut tx = pool.begin().await.expect("begin");
        upsert_stages(
            &mut tx,
            &provider,
            &[stage("d1", "Deck 1")],
            100,
            Utc::now(),
        )
        .await
        .expect("first");
        upsert_stages(
            &mut tx,
            &provider,
            &[stage("d1", "Deck One")],
            100,
            Utc::now(),
        )
        .await
        .expect("update");
        tx.commit().await.expect("commit");

        let name: String = sqlx::query_scalar(
            "SELECT name FROM cruise_provider_stages
             WHERE cruise_provider_id = $1 AND cruise_provider_stage_id = 'd1'",
        )
        .bind(&pid)
        .fetch_one(pool)
        .await
        .expect("fetch");
        assert_eq!(name, "Deck One");

        cleanup_provider(pool, &pid).await;
    }

    // ============================================================
    // upsert_classes
    // ============================================================

    #[tokio::test]
    async fn upsert_classes_full_fields() {
        let _guard = db_lock().await;
        let pool_owned = pool().await;
        let pool = &pool_owned;
        purge_all_test_rows(pool).await;
        let pid = test_provider_id("cls");
        insert_provider(pool, &pid).await;

        let mut c = class("o1", "c10", "Lux");
        c.description = Some("Very lux".into());
        c.base_seats = Some(2);
        c.tiers = Some(1);
        c.partial_buyout = Some(true);

        let mut tx = pool.begin().await.expect("begin");
        upsert_classes(&mut tx, &ProviderId(pid.clone()), &[c], 100, Utc::now())
            .await
            .expect("insert");
        tx.commit().await.expect("commit");

        let (name, desc, seats, tiers, nofull): (
            String,
            Option<String>,
            Option<i32>,
            Option<i32>,
            Option<bool>,
        ) = sqlx::query_as(
            "SELECT name, description, base_seats, tiers, partial_buyout
             FROM cruise_provider_classes
             WHERE cruise_provider_id = $1 AND cruise_provider_class_id = 'c10'",
        )
        .bind(&pid)
        .fetch_one(pool)
        .await
        .expect("fetch");

        assert_eq!(name, "Lux");
        assert_eq!(desc.as_deref(), Some("Very lux"));
        assert_eq!(seats, Some(2));
        assert_eq!(tiers, Some(1));
        assert_eq!(nofull, Some(true));

        cleanup_provider(pool, &pid).await;
    }

    // ============================================================
    // upsert_rooms
    // ============================================================

    #[tokio::test]
    async fn upsert_rooms_insert_and_update() {
        let _guard = db_lock().await;
        let pool_owned = pool().await;
        let pool = &pool_owned;
        purge_all_test_rows(pool).await;
        let pid = test_provider_id("room");
        insert_provider(pool, &pid).await;

        let provider = ProviderId(pid.clone());
        let mut tx = pool.begin().await.expect("begin");
        upsert_rooms(
            &mut tx,
            &provider,
            &[room("o1", "d1", "cls1", "r1", "101")],
            100,
            Utc::now(),
        )
        .await
        .expect("insert");

        // Обновляем номер комнаты
        upsert_rooms(
            &mut tx,
            &provider,
            &[room("o1", "d1", "cls1", "r1", "101A")],
            100,
            Utc::now(),
        )
        .await
        .expect("update");
        tx.commit().await.expect("commit");

        let number: String = sqlx::query_scalar(
            "SELECT number FROM cruise_provider_rooms
             WHERE cruise_provider_id = $1 AND cruise_provider_room_id = 'r1'",
        )
        .bind(&pid)
        .fetch_one(pool)
        .await
        .expect("fetch");
        assert_eq!(number, "101A");

        cleanup_provider(pool, &pid).await;
    }
}
