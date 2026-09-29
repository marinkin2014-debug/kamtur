//! Criterion benchmarks для `apply_prices_scd2` — главного SCD2-писателя.
//!
//! ## Что измеряем
//!
//! `apply_prices_scd2` вызывается в проде из `apply_sync` на каждом sync-цикле
//! и обрабатывает весь набор `CanonicalPrice` фида. Внутри — TEMP TABLE +
//! batch INSERT через `UNNEST` + три statement'а (close-changed, close-missing,
//! open-new). SCD2-логика: каждая цена имеет историю версий с `valid_from` /
//! `valid_to`, ключ версии — `(cruise_id, class_id, partial_buyout)`.
//!
//! ## Проблема измерения SCD2-писателей
//!
//! Наивный `b.iter(|| apply_prices_scd2(...))` измеряет только **первую**
//! итерацию. Вторая итерация с теми же данными даст no-op (SCD2 уже
//! применилась), третья — то же самое. Criterion сложил бы «холодный» вызов
//! с сотнями no-op и выдал бы бессмысленно оптимистичное среднее.
//!
//! Решение — `b.iter_batched` с **rollback'ом после каждой итерации**:
//!
//! ```text
//! iteration:
//!     tx = pool.begin()      // ~0.1 ms
//!     apply_prices_scd2(tx)  // ← измеряется
//!     tx.rollback()          // ~1 ms
//! ```
//!
//! Между итерациями БД возвращается в baseline. Measurement включает
//! begin+rollback — их общий оверхед ~1.1 ms на круг, для масштабов 100k+
//! пренебрежимо.
//!
//! ## Кейсы
//!
//! - **noop**        — incoming идентичен текущему состоянию. Ожидание: 0/0/0.
//! - **full_change** — те же ключи, другие base_price. Ожидание: N updated + N created.
//! - **empty**       — пустой incoming. Ожидание: 0/0/N closed.
//! - **small_vs_temp** — 50 (small path) vs 5000 (temp table path, порог 100).
//! - **batch_size** — 500 / 5000 / 50000 при фиксированном incoming = 5000.
//!
//! Размеры seed'а для noop/full_change/empty: 10k / 100k / 1M.
//!
//! ## Setup
//!
//! Требует `TEST_DATABASE_URL` (или `DATABASE_URL`). Без переменной bench
//! gracefully skip'ается. Миграции прогоняются автоматически (см. `setup`).
//!
//! ## Cleanup
//!
//! Все bench-данные под `provider_id = bench-scd2-<uuid>`, префикс не
//! пересекается с `test-%`/`bench-list-%`. `purge_all_test_rows` из
//! `test_support` их не тронет, `cleanup` в этом файле — тоже.
//! После каждого size-блока вызывается `cleanup`. Если bench прерван
//! (Ctrl+C), ручная очистка:
//!
//! ```sql
//! DELETE FROM cruise_provider_prices WHERE cruise_provider_id LIKE 'bench-scd2-%';
//! DELETE FROM cruise_providers       WHERE id                 LIKE 'bench-scd2-%';
//! ```
//!
//! ## Запуск
//!
//! ```text
//! # Полный прогон (~10-15 минут)
//! cargo bench -p infrastructure --bench scd2
//!
//! # Быстрый smoke (числа грязные, но всё скомпилируется и прогонится)
//! cargo bench -p infrastructure --bench scd2 -- \
//!     --warm-up-time 1 --measurement-time 2 --sample-size 10
//!
//! # Только один размер (criterion фильтрует по подстроке benchmark id)
//! cargo bench -p infrastructure --bench scd2 -- 100k
//! ```

use std::time::Duration;

use chrono::{DateTime, Utc};
use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput};
use rust_decimal::Decimal;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

use domain::entities::{CanonicalPrice, ProviderId};

use infrastructure::repositories::postgres::prices::apply_prices_scd2;

// ============================================================
// Константы
// ============================================================

const PROVIDER_PREFIX: &str = "bench-scd2";

/// Батч для seed-стадии. Не измеряется — только setup.
const SEED_BATCH: usize = 5000;

/// Батч для bench-стадии, совпадает с продовым `BATCH_SIZE`.
const PROD_BATCH_SIZE: usize = 5000;

/// Размеры seed'а для noop / full_change / empty.
///
/// ## Почему нет 1M
///
/// На dev-машине контейнер Postgres использует tmpfs для PGDATA (2.9 GB
/// по `df -h /var/lib/postgresql/data`). Criterion требует минимум
/// `--sample-size 10`, а каждая full_change-sample на 1M создаёт ~800 MB
/// dead rows (2M версий). Rollback в `run_once` помечает dead, но не
/// освобождает heap — через 3-4 samples tmpfs переполняется
/// (`could not extend file ... No space left on device`).
///
/// VACUUM между sample'ами criterion не даёт запустить: benchmark loop
/// им контролируется целиком.
///
/// 1M цифры для docs/performance.md получены **линейной экстраполяцией**
/// от 100k (масштабирование подтверждено дважды: noop ×9.98, full_change ×11.9).
///
/// ## Если нужно измерить 1M в будущем
///
/// - Пересоздать контейнер с named volume вместо tmpfs (см. docker-compose.test.yml).
/// - Или написать отдельный example с прямым замером через `Instant::now()`
///   (не criterion) и VACUUM между итерациями.
/// - Оба варианта — за пределами B.5.
const SIZES: &[(&str, usize)] = &[("10k", 10_000), ("100k", 100_000)];

/// Варианты `BATCH_SIZE` для кейса `batch_size`.
const BATCH_SIZES: &[usize] = &[500, 5000, 50_000];

// ============================================================
// Setup
// ============================================================

fn setup(rt: &tokio::runtime::Runtime) -> Option<PgPool> {
    let _ = dotenvy::dotenv();
    let url = std::env::var("TEST_DATABASE_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .ok()?;

    rt.block_on(async move {
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .min_connections(1)
            .acquire_timeout(Duration::from_secs(10))
            .connect(&url)
            .await
            .ok()?;

        sqlx::migrate!("../../migrations").run(&pool).await.ok()?;

        Some(pool)
    })
}

// ============================================================
// Fixtures
// ============================================================

/// Создаёт провайдера. `cruise_provider_prices` имеет FK на `cruise_providers`.
async fn insert_provider(pool: &PgPool, provider_id: &str) {
    sqlx::query("INSERT INTO cruise_providers (id, name) VALUES ($1, $2)")
        .bind(provider_id)
        .bind(format!("Bench scd2 {provider_id}"))
        .execute(pool)
        .await
        .expect("insert bench provider");
}

async fn cleanup(pool: &PgPool, provider_id: &str) {
    // FK ON DELETE CASCADE на cruise_provider_prices → cruise_providers,
    // но удаляем явно, чтобы видеть объём в логах и не зависеть от схемы.
    sqlx::query("DELETE FROM cruise_provider_prices WHERE cruise_provider_id = $1")
        .bind(provider_id)
        .execute(pool)
        .await
        .ok();
    sqlx::query("DELETE FROM cruise_providers WHERE id = $1")
        .bind(provider_id)
        .execute(pool)
        .await
        .ok();
}

/// Генерирует `n` уникальных `CanonicalPrice` для SCD2-ключа.
///
/// Ключ версии = `(cruise_id, class_id, partial_buyout)`. Раскладка:
///
/// ```text
///   cruise_id      = bench-cruise-{i/100:06}    (i/100 уникально для каждой группы 100)
///   class_id       = bench-class-{i%100:03}     (100 классов на каждый cruise)
///   partial_buyout = false                       (один вариант ключа)
/// ```
///
/// Это даёт ровно `n` уникальных ключей при любом `n`, что важно: temp table
/// `incoming_prices` имеет `PRIMARY KEY (cruise_id, class_id, partial_buyout)`,
/// и при дубликатах терялись бы элементы через `ON CONFLICT DO NOTHING`.
///
/// `base_price` — параметризован, чтобы получить «noop» (те же значения)
/// или «full_change» (другие значения, те же ключи).
fn generate_prices(n: usize, base_price: u32) -> Vec<CanonicalPrice> {
    let dec = Decimal::from(base_price);
    (0..n)
        .map(|i| CanonicalPrice {
            external_cruise_id: format!("bench-cruise-{:06}", i / 100),
            external_class_id: format!("bench-class-{:03}", i % 100),
            base_price: dec,
            partial_buyout: Some(false),
            child_price: None,
            extra_seat: None,
            currency: "RUB".into(),
        })
        .collect()
}

/// Заполняет БД через `apply_prices_scd2` — это создаёт baseline-состояние.
/// Коммитим (не rollback), чтобы состояние сохранилось между итерациями
/// bench'а.
async fn seed_baseline(pool: &PgPool, provider_id: &str, prices: &[CanonicalPrice]) {
    let mut tx = pool.begin().await.expect("begin seed tx");
    apply_prices_scd2(
        &mut tx,
        &ProviderId(provider_id.into()),
        prices,
        SEED_BATCH,
        Utc::now(),
    )
    .await
    .expect("seed baseline");
    tx.commit().await.expect("commit seed tx");
}

// ============================================================
// Benchmark: одна итерация apply внутри begin/rollback
// ============================================================

/// Прогоняет одну итерацию `apply_prices_scd2` в транзакции + rollback.
///
/// Возвращает `ScdOutcome` — criterion его `black_box`'ит.
fn run_once(
    rt: &tokio::runtime::Runtime,
    pool: &PgPool,
    provider_id: &str,
    incoming: &[CanonicalPrice],
    batch_size: usize,
    now: DateTime<Utc>,
) -> infrastructure::repositories::postgres::prices::ScdOutcome {
    rt.block_on(async {
        let mut tx = pool.begin().await.expect("begin bench tx");
        let outcome = apply_prices_scd2(
            &mut tx,
            &ProviderId(provider_id.into()),
            incoming,
            batch_size,
            now,
        )
        .await
        .expect("apply_prices_scd2");
        // Rollback возвращает БД в baseline. Между итерациями состояние
        // одно и то же — измеряем чистый SCD2-diff, а не накопление версий.
        tx.rollback().await.expect("rollback bench tx");
        outcome
    })
}

// ============================================================
// Bench groups
// ============================================================

fn bench_scd2_prices(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build tokio runtime");

    let Some(pool) = setup(&rt) else {
        eprintln!(
            "scd2 bench: TEST_DATABASE_URL / DATABASE_URL not set — skipping. \
             Set either variable and re-run."
        );
        return;
    };

    // ---------- Группа 1: noop / full_change / empty по размерам ----------
    for (size_label, size) in SIZES {
        let provider_id = format!("{PROVIDER_PREFIX}-{size_label}-{}", uuid::Uuid::new_v4());

        eprintln!("scd2/{size_label}: seeding {size} prices under provider_id={provider_id}");

        rt.block_on(insert_provider(&pool, &provider_id));

        // Baseline: все цены base_price = 100.
        let baseline = generate_prices(*size, 100);
        rt.block_on(seed_baseline(&pool, &provider_id, &baseline));

        // incoming-наборы для трёх кейсов.
        let incoming_noop = generate_prices(*size, 100); // те же значения
        let incoming_change = generate_prices(*size, 200); // другие значения, те же ключи
        let incoming_empty: Vec<CanonicalPrice> = Vec::new();

        // ---- noop ----
        let mut group = c.benchmark_group(format!("scd2/{size_label}/noop"));
        group.throughput(Throughput::Elements(*size as u64));
        group.bench_function(BenchmarkId::from_parameter("noop"), |b| {
            b.iter_batched(
                || (),
                |_| {
                    let now = Utc::now();
                    let outcome = run_once(
                        &rt,
                        &pool,
                        &provider_id,
                        &incoming_noop,
                        PROD_BATCH_SIZE,
                        now,
                    );
                    debug_assert_eq!(outcome.created, 0, "noop: created must be 0");
                    debug_assert_eq!(outcome.updated, 0, "noop: updated must be 0");
                    debug_assert_eq!(outcome.closed, 0, "noop: closed must be 0");
                    criterion::black_box(outcome);
                },
                BatchSize::LargeInput,
            )
        });
        group.finish();

        // ---- full_change ----
        let mut group = c.benchmark_group(format!("scd2/{size_label}/full_change"));
        group.throughput(Throughput::Elements(*size as u64));
        group.bench_function(BenchmarkId::from_parameter("full_change"), |b| {
            b.iter_batched(
                || (),
                |_| {
                    let now = Utc::now();
                    let outcome = run_once(
                        &rt,
                        &pool,
                        &provider_id,
                        &incoming_change,
                        PROD_BATCH_SIZE,
                        now,
                    );
                    // Rollback возвращает baseline, поэтому каждый вызов
                    // видит одно и то же состояние.
                    criterion::black_box(outcome);
                },
                BatchSize::LargeInput,
            )
        });
        group.finish();

        // ---- empty ----
        let mut group = c.benchmark_group(format!("scd2/{size_label}/empty"));
        group.throughput(Throughput::Elements(*size as u64));
        group.bench_function(BenchmarkId::from_parameter("empty"), |b| {
            b.iter_batched(
                || (),
                |_| {
                    let now = Utc::now();
                    let outcome = run_once(
                        &rt,
                        &pool,
                        &provider_id,
                        &incoming_empty,
                        PROD_BATCH_SIZE,
                        now,
                    );
                    criterion::black_box(outcome);
                },
                BatchSize::LargeInput,
            )
        });
        group.finish();

        rt.block_on(cleanup(&pool, &provider_id));
    }

    // ---------- Группа 2: small path vs temp table path ----------
    //
    // Порог в `prices.rs`: TEMP_TABLE_THRESHOLD = 100 — выше которого
    // используется temp table, ниже — другой путь. Проверяем оба.
    {
        let provider_id = format!("{PROVIDER_PREFIX}-paths-{}", uuid::Uuid::new_v4());
        rt.block_on(insert_provider(&pool, &provider_id));

        let mut group = c.benchmark_group("scd2/paths");

        for &n in &[50usize, 5000] {
            let incoming = generate_prices(n, 100);

            group.throughput(Throughput::Elements(n as u64));
            group.bench_with_input(BenchmarkId::from_parameter(n), &incoming, |b, incoming| {
                b.iter_batched(
                    || (),
                    |_| {
                        let outcome = run_once(
                            &rt,
                            &pool,
                            &provider_id,
                            incoming,
                            PROD_BATCH_SIZE,
                            Utc::now(),
                        );
                        criterion::black_box(outcome);
                    },
                    BatchSize::LargeInput,
                )
            });
        }

        group.finish();
        rt.block_on(cleanup(&pool, &provider_id));
    }

    // ---------- Группа 3: BATCH_SIZE для temp path ----------
    //
    // Вход фиксирован (5000 items > порог, идём через temp table).
    // Меняется только `batch_size` в INSERT'ах. Смотрим, где оптимум.
    {
        let provider_id = format!("{PROVIDER_PREFIX}-bs-{}", uuid::Uuid::new_v4());
        rt.block_on(insert_provider(&pool, &provider_id));

        let incoming = generate_prices(5000, 100);
        let mut group = c.benchmark_group("scd2/batch_size");
        group.throughput(Throughput::Elements(incoming.len() as u64));

        for &bs in BATCH_SIZES {
            group.bench_with_input(BenchmarkId::from_parameter(bs), &bs, |b, &bs| {
                b.iter_batched(
                    || (),
                    |_| {
                        let outcome = run_once(&rt, &pool, &provider_id, &incoming, bs, Utc::now());
                        criterion::black_box(outcome);
                    },
                    BatchSize::LargeInput,
                )
            });
        }

        group.finish();
        rt.block_on(cleanup(&pool, &provider_id));
    }

    // Закрываем pool перед выходом.
    rt.block_on(async move {
        pool.close().await;
    });
}

criterion_group!(benches, bench_scd2_prices);
criterion_main!(benches);
