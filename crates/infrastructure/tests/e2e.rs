//! End-to-end тесты полного pipeline: XML → parse → transform → enrich →
//! persist → read.
//!
//! Проверяют склейку модулей, а не отдельные операции. Именно здесь
//! вылавливаются баги вида "парсер вернул одни external_id, а repository
//! ожидает другие".
//!
//! Требуют `TEST_DATABASE_URL` (или `DATABASE_URL`). Работают против
//! реального Postgres — как integration-тесты infrastructure.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use chrono::Utc;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

use domain::entities::ProviderId;
use domain::fingerprint::Fingerprint;
use domain::ports::{MetricsRecorder, SnapshotRepository, SyncRepository};
use infrastructure::metrics::NoopMetrics;
use infrastructure::providers::volga_wolga::{VolgaCanonicalTransformer, VolgaParser};
use infrastructure::repositories::postgres::PostgresCruiseRepository;

// ============================================================
// Test database helpers
// ============================================================

static DB_TEST_MUTEX: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
static MIGRATED: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();

async fn pool() -> PgPool {
    // Идемпотентно: подхватываем .env из корня воркспейса, если есть.
    let _ = dotenvy::dotenv();

    let url = std::env::var("TEST_DATABASE_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .expect(
            "TEST_DATABASE_URL or DATABASE_URL must be set. \
             Either export the variable or add it to workspace .env",
        );

    let pool = PgPoolOptions::new()
        .max_connections(5)
        .min_connections(1)
        .acquire_timeout(Duration::from_secs(5))
        .connect(&url)
        .await
        .expect("connect postgres");

    MIGRATED
        .get_or_init(|| async {
            sqlx::migrate!("../../migrations")
                .run(&pool)
                .await
                .expect("run migrations");
        })
        .await;

    pool
}

fn test_provider_id(prefix: &str) -> String {
    format!("test-e2e-{}-{}", prefix, uuid::Uuid::new_v4())
}

async fn purge_all_test_rows(pool: &PgPool) {
    for table in &[
        "errors",
        "sync_runs",
        "raw_snapshots",
        "cruise_provider_sales",
        "cruise_provider_prices",
        "cruise_provider_availability",
        "cruise_provider_rooms",
        "cruise_provider_classes",
        "cruise_provider_stages",
        "cruise_provider_objects",
        "cruise_provider_tours",
    ] {
        sqlx::query(&format!(
            "DELETE FROM {table} WHERE cruise_provider_id LIKE 'test-e2e-%'"
        ))
        .execute(pool)
        .await
        .ok();
    }
    sqlx::query("DELETE FROM cruise_providers WHERE id LIKE 'test-e2e-%'")
        .execute(pool)
        .await
        .ok();
}

async fn insert_provider(pool: &PgPool, id: &str) {
    sqlx::query("INSERT INTO cruise_providers (id, name) VALUES ($1, $2)")
        .bind(id)
        .bind(format!("Test {id}"))
        .execute(pool)
        .await
        .expect("insert provider");
}

async fn cleanup_provider(pool: &PgPool, id: &str) {
    purge_all_test_rows(pool).await;
    sqlx::query("DELETE FROM cruise_providers WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await
        .ok();
}

// ============================================================
// Fixture XML
// ============================================================
//
// Формат Volga:
//   <ship id="1"/>   + <class id="101"/>  → class принадлежит кораблю 1
//   (class id = <object_id><2-значный_номер_класса>)

/// Минимальный, но полный XML: 2 ship, 2 cruise, 4 cabin, 2 class,
/// prices для каждого cruise, availability для одного cruise.
const FIXTURE_V1: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?>
<root>
    <ships>
        <ship id="1"  name="Ship Alpha"/>
        <ship id="18" name="Ship Beta"/>
    </ships>
    <decks>
        <deck id="d1" name="Main Deck"/>
    </decks>
    <classes>
        <class id="101" name="Lux"      m_count="2" r_count="1" no_full="0"/>
        <class id="102" name="Standard" m_count="4" no_full="0"/>
    </classes>
    <cabins>
        <cabin id="cab1" ship="1" number="101" class_id="101" deck="d1"/>
        <cabin id="cab2" ship="1" number="102" class_id="101" deck="d1"/>
        <cabin id="cab3" ship="1" number="103" class_id="102" deck="d1"/>
        <cabin id="cab4" ship="1" number="104" class_id="102" deck="d1"/>
    </cabins>
    <cruises>
        <cruise id="c1" ship_id="1" begin_date="01.09.2026" begin_time="10:00"
                end_date="05.09.2026" end_time="18:00" route="Perm-Samara"
                child_price="5000" dop_price="1000"/>
        <cruise id="c2" ship_id="1" begin_date="10.09.2026"
                end_date="15.09.2026" route="Samara-Astrakhan"/>
    </cruises>
    <prices>
        <price cruise_id="c1" class_id="101" price="50000.00" nofull="0"/>
        <price cruise_id="c1" class_id="102" price="30000.00" nofull="0"/>
        <price cruise_id="c2" class_id="101" price="45000.00" nofull="0"/>
    </prices>
    <spos>
        <spo cruise_id="c1" class_id="101" cabin_id="cab1" spo="48000.00" nofull="0"/>
    </spos>
    <free>
        <cruise id="c1">
            <cabin id="cab1"/>
            <cabin id="cab3"/>
        </cruise>
    </free>
</root>"#;

/// V2: цена c1 подорожала (50000 → 55000), c2 исчез, c3 появился.
/// Используется для проверки SCD2-цикла.
const FIXTURE_V2: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?>
<root>
    <ships>
        <ship id="1" name="Ship Alpha"/>
    </ships>
    <decks>
        <deck id="d1" name="Main Deck"/>
    </decks>
    <classes>
        <class id="101" name="Lux" m_count="2" r_count="1" no_full="0"/>
    </classes>
    <cabins>
        <cabin id="cab1" ship="1" number="101" class_id="101" deck="d1"/>
    </cabins>
    <cruises>
        <cruise id="c1" ship_id="1" begin_date="01.09.2026"
                end_date="05.09.2026" route="Perm-Samara"/>
        <cruise id="c3" ship_id="1" begin_date="20.09.2026"
                end_date="25.09.2026" route="Kazan-Perm"/>
    </cruises>
    <prices>
        <price cruise_id="c1" class_id="101" price="55000.00" nofull="0"/>
        <price cruise_id="c3" class_id="101" price="60000.00" nofull="0"/>
    </prices>
    <spos/>
    <free/>
</root>"#;

// ============================================================
// Pipeline helper
// ============================================================

/// Прогоняет полный pipeline на XML. Возвращает `SyncOutcome`.
async fn run_pipeline(
    repo: &PostgresCruiseRepository,
    provider_id: &str,
    xml: &[u8],
) -> domain::entities::SyncOutcome {
    let provider = ProviderId(provider_id.into());

    let parser = VolgaParser;
    let transformer = VolgaCanonicalTransformer::new(1);
    let raw_data = parser.parse(xml).expect("parse");
    let canonical = transformer.transform(raw_data).expect("transform");

    // Enrich (пустые правила — в проде их грузит репозиторий из БД)
    let enriched = Vec::new();

    let raw_bytes = Bytes::copy_from_slice(xml);
    let fingerprint = Fingerprint::of(&raw_bytes);

    repo.apply_sync(
        &provider,
        &raw_bytes,
        &fingerprint,
        &canonical,
        &enriched,
        Utc::now(),
    )
    .await
    .expect("apply_sync")
}

// ============================================================
// Tests
// ============================================================

#[tokio::test]
async fn full_pipeline_xml_to_read() {
    let _guard = DB_TEST_MUTEX.lock().await;
    let pool = pool().await;
    purge_all_test_rows(&pool).await;

    let pid = test_provider_id("full");
    insert_provider(&pool, &pid).await;

    let metrics: Arc<dyn MetricsRecorder> = Arc::new(NoopMetrics);
    let repo = PostgresCruiseRepository::new(pool.clone(), false, 5000, metrics);

    let outcome = run_pipeline(&repo, &pid, FIXTURE_V1).await;

    assert!(!outcome.duplicate);
    assert_eq!(outcome.objects_upserted, 2, "2 ship");
    assert_eq!(outcome.tours_upserted, 2, "2 cruise");
    assert_eq!(outcome.rooms_upserted, 4, "4 cabin");
    assert!(outcome.prices_created >= 3, "3 price");
    assert!(outcome.sales_created >= 1, "1 spo");

    // Читаем напрямую из БД — не тянем read-репозиторий.
    let tour_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM cruise_provider_tours
         WHERE cruise_provider_id = $1 AND is_active = true",
    )
    .bind(&pid)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(tour_count, 2);

    let price_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM cruise_provider_prices
         WHERE cruise_provider_id = $1 AND valid_to IS NULL",
    )
    .bind(&pid)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(price_count, 3);

    let avail_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM cruise_provider_availability
         WHERE cruise_provider_id = $1 AND valid_to IS NULL AND available = true",
    )
    .bind(&pid)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(avail_count, 2, "cab1 и cab3 доступны для c1");

    cleanup_provider(&pool, &pid).await;
}

/// Регрессия: `class.external_object_id` после transform должен указывать
/// на существующий ship. Старая реализация `transform` могла создать
/// classes с «висячим» object_id (см. `object_id_from_class_id`), и это
/// не ловилось прежними фикстурами — они использовали нереалистичный
/// формат class id, где разделитель не соответствовал боевому фиду.
#[tokio::test]
async fn pipeline_classes_point_to_existing_ships() {
    let _guard = DB_TEST_MUTEX.lock().await;
    let pool = pool().await;
    purge_all_test_rows(&pool).await;

    let pid = test_provider_id("fk");
    insert_provider(&pool, &pid).await;

    let metrics: Arc<dyn MetricsRecorder> = Arc::new(NoopMetrics);
    let repo = PostgresCruiseRepository::new(pool.clone(), false, 5000, metrics);

    run_pipeline(&repo, &pid, FIXTURE_V1).await;

    // Все classes должны иметь object_id, который есть в objects.
    let orphans: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM cruise_provider_classes c
         WHERE c.cruise_provider_id = $1
           AND NOT EXISTS (
               SELECT 1 FROM cruise_provider_objects o
               WHERE o.cruise_provider_id = c.cruise_provider_id
                 AND o.cruise_provider_object_id = c.cruise_provider_object_id
           )",
    )
    .bind(&pid)
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(orphans, 0, "classes с несуществующим object_id: {orphans}");

    cleanup_provider(&pool, &pid).await;
}

#[tokio::test]
async fn second_sync_with_same_xml_is_duplicate() {
    let _guard = DB_TEST_MUTEX.lock().await;
    let pool = pool().await;
    purge_all_test_rows(&pool).await;

    let pid = test_provider_id("dup");
    insert_provider(&pool, &pid).await;

    let metrics: Arc<dyn MetricsRecorder> = Arc::new(NoopMetrics);
    let repo = PostgresCruiseRepository::new(pool.clone(), false, 5000, metrics);

    let first = run_pipeline(&repo, &pid, FIXTURE_V1).await;
    assert!(!first.duplicate);

    let provider = ProviderId(pid.clone());
    let fingerprint = Fingerprint::of(&Bytes::from_static(FIXTURE_V1));
    assert!(repo.has_snapshot(&provider, &fingerprint).await.unwrap());

    let second = run_pipeline(&repo, &pid, FIXTURE_V1).await;
    assert!(
        second.duplicate,
        "второй sync с тем же XML должен вернуть duplicate=true"
    );

    cleanup_provider(&pool, &pid).await;
}

#[tokio::test]
async fn scd2_cycle_creates_new_versions() {
    let _guard = DB_TEST_MUTEX.lock().await;
    let pool = pool().await;
    purge_all_test_rows(&pool).await;

    let pid = test_provider_id("cycle");
    insert_provider(&pool, &pid).await;

    let metrics: Arc<dyn MetricsRecorder> = Arc::new(NoopMetrics);
    let repo = PostgresCruiseRepository::new(pool.clone(), false, 5000, metrics);

    // Sync #1: V1
    run_pipeline(&repo, &pid, FIXTURE_V1).await;

    // Sync #2: V2 — цена c1 изменилась, c2 исчез, c3 добавился
    let v2 = run_pipeline(&repo, &pid, FIXTURE_V2).await;
    assert!(!v2.duplicate);
    assert!(v2.prices_created >= 1, "новая цена c1, c3");
    assert!(v2.prices_updated >= 1, "старая цена c1 закрыта");
    assert!(v2.prices_closed >= 1, "цена c2 закрыта — c2 больше нет");

    // c1 имел 2 класса в V1 (101, 102).
    // В V2:
    //   - 101: цена 50000 → 55000. Старая закрыта, новая открыта.
    //   - 102: класса нет в V2. Старая цена закрыта, новой нет.
    let c1_total: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM cruise_provider_prices
         WHERE cruise_provider_id = $1 AND cruise_provider_cruise_id = 'c1'",
    )
    .bind(&pid)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(c1_total, 3, "c1: 101 × 2 (old+new) + 102 × 1 (old)");

    let c1_cls101: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM cruise_provider_prices
         WHERE cruise_provider_id = $1
           AND cruise_provider_cruise_id = 'c1'
           AND cruise_provider_class_id = '101'",
    )
    .bind(&pid)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(c1_cls101, 2, "class 101: 2 версии (old+new)");

    let c1_cls102: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM cruise_provider_prices
         WHERE cruise_provider_id = $1
           AND cruise_provider_cruise_id = 'c1'
           AND cruise_provider_class_id = '102'",
    )
    .bind(&pid)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(c1_cls102, 1, "class 102: 1 закрытая версия (класс пропал)");

    let c1_open: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM cruise_provider_prices
         WHERE cruise_provider_id = $1
           AND cruise_provider_cruise_id = 'c1'
           AND valid_to IS NULL",
    )
    .bind(&pid)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        c1_open, 1,
        "у c1 ровно одна открытая версия (только class 101)"
    );

    // c2 деактивирован (в V2 его нет)
    let c2_active: bool = sqlx::query_scalar(
        "SELECT is_active FROM cruise_provider_tours
         WHERE cruise_provider_id = $1 AND cruise_provider_cruise_id = 'c2'",
    )
    .bind(&pid)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(!c2_active, "c2 пропал — деактивирован");

    // c3 появился и активен
    let c3_active: bool = sqlx::query_scalar(
        "SELECT is_active FROM cruise_provider_tours
         WHERE cruise_provider_id = $1 AND cruise_provider_cruise_id = 'c3'",
    )
    .bind(&pid)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(c3_active, "c3 появился — активен");

    cleanup_provider(&pool, &pid).await;
}

/// Сквозной тест: после `apply_sync` read-репозиторий отдаёт данные,
/// записанные пайплайном. Проверяет склейку write → read.
#[tokio::test]
async fn read_returns_data_written_by_pipeline() {
    let _guard = DB_TEST_MUTEX.lock().await;
    let pool = pool().await;
    purge_all_test_rows(&pool).await;

    let pid = test_provider_id("read");
    insert_provider(&pool, &pid).await;

    let metrics: Arc<dyn MetricsRecorder> = Arc::new(NoopMetrics);
    let repo = PostgresCruiseRepository::new(pool.clone(), false, 5000, metrics);

    run_pipeline(&repo, &pid, FIXTURE_V1).await;

    // Read-репозиторий (отдельный) видит данные
    let read_repo =
        infrastructure::repositories::postgres::PostgresCruiseReadRepository::new(pool.clone());

    use domain::ports::CruiseReadRepository;
    use domain::views::CruiseListFilter;

    let filter = CruiseListFilter {
        cruise_provider_id: Some(pid.clone()),
        limit: 20,
        ..Default::default()
    };

    // Контракт: list_cruises возвращает не более limit+1 строк.
    // В фикстуре 2 круиза → вернутся оба, без sentinel.
    let items = read_repo.list_cruises(&filter).await.expect("list");
    assert_eq!(items.len(), 2, "2 cruise в фикстуре");

    let c1 = items.iter().find(|i| i.cruise_id == "c1").unwrap();
    assert_eq!(c1.name, "Perm-Samara");
    assert_eq!(c1.ship_name.as_deref(), Some("Ship Alpha"));
    assert_eq!(c1.room_counts, 2, "cab1 и cab3 доступны для c1");

    // Detail
    let detail = read_repo
        .get_cruise(&pid, "c1")
        .await
        .expect("get")
        .expect("exists");
    assert_eq!(detail.cruise_id, "c1");
    assert!(detail.prices.len() >= 2, "c1: 2 класса");
    assert_eq!(detail.rooms.len(), 2, "cab1 и cab3 (те что в availability)");

    // Проверяем, что class_name действительно заполнен (через LEFT JOIN
    // на objects). До фикса формата фикстуры тут мог бы быть NULL.
    for price in &detail.prices {
        assert!(
            !price.class_name.is_empty(),
            "class_name для {} пустой — сломан join classes → objects",
            price.class_id,
        );
    }

    cleanup_provider(&pool, &pid).await;
}
