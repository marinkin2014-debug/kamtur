//! Criterion benchmark для keyset pagination в `list_cruises`.
//!
//! ## Что измеряем
//!
//! Latency `PostgresCruiseReadRepository::list_cruises` на **разной глубине**
//! пагинации при keyset-подходе. Гипотеза: keyset даёт плоский профиль —
//! время запроса не зависит от глубины. Если это так — пагинация в
//! production-API масштабируется на любой размер фида.
//!
//! ## Почему НЕ сравниваем с OFFSET
//!
//! Изначально планировалось сравнение keyset vs offset. Отказались (HANDOFF
//! §5.2, «Выбор: вариант В»):
//!
//! - OFFSET-пагинация в проекте **не используется**. Добавлять метод в
//!   репозиторий ради bench — вредная практика: тестовый код диктует
//!   production-API.
//! - Вместо сравнения с несуществующим конкурентом показываем **свойство**,
//!   которое важно для реального API: keyset-латентность плоская по глубине.
//!
//! ## Глубины и размеры
//!
//! Глубины: page 1, 100, 1000 (limit = 50). Это offset 0, 5_000, 50_000.
//! OFFSET сканировал бы все строки до указанной позиции; keyset должен дать
//! одинаковое время во всех трёх точках.
//!
//! Размеры БД: 10k / 100k / 1M строк. На 10k глубина 1000 выходит за
//! пределы таблицы (10k / 50 = 200 страниц) — кейс пропускается с
//! предупреждением в stderr.
//!
//! ## Setup
//!
//! Требует `TEST_DATABASE_URL` (или `DATABASE_URL`). Если переменной нет —
//! bench пропускается с сообщением. Миграции прогоняются автоматически
//! (`sqlx::migrate!`), идентично `test_support`.
//!
//! ## Изоляция
//!
//! Bench-данные используют `provider_id = "bench-list-<size>-<uuid>"`.
//! Префикс `bench-list-` не пересекается с `test-%` из `test_support`,
//! поэтому обычные integration-тесты не тронут наши строки.
//!
//! ## Cleanup
//!
//! Seed удаляется после каждой size-группы. Если bench был прерван
//! (Ctrl+C) — записи остаются в БД. Ручная очистка:
//!
//! ```sql
//! DELETE FROM cruise_provider_tours   WHERE cruise_provider_id LIKE 'bench-list-%';
//! DELETE FROM cruise_provider_objects WHERE cruise_provider_id LIKE 'bench-list-%';
//! DELETE FROM cruise_providers        WHERE id                 LIKE 'bench-list-%';
//! ```
//!
//! ## Запуск
//!
//! ```text
//! # Полный прогон (~5-15 минут)
//! cargo bench -p infrastructure --bench list_cruises
//!
//! # Быстрый smoke
//! cargo bench -p infrastructure --bench list_cruises -- \
//!     --warm-up-time 1 --measurement-time 2 --sample-size 10
//!
//! # Только один размер (criterion фильтрует по подстроке benchmark id)
//! cargo bench -p infrastructure --bench list_cruises -- 100k
//! ```

use std::time::Duration;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

use domain::ports::CruiseReadRepository;
use domain::views::{CruiseListCursor, CruiseListFilter};
use infrastructure::repositories::postgres::PostgresCruiseReadRepository;

// ============================================================
// Константы конфигурации
// ============================================================

/// Префикс provider_id. Все bench-строки имеют вид `bench-list-*`,
/// не пересекаются с `test-%` из обычных integration-тестов.
const PROVIDER_PREFIX: &str = "bench-list";

/// Внешний object_id для всех bench-tours. Один на provider, FK в
/// `cruise_provider_tours.cruise_provider_object_id`.
const BENCH_OBJECT_ID: &str = "bench-obj";

/// Размер страницы. 50 — типичный клиентский размер; 20 из дефолта API
/// слишком мал, чтобы разница между глубинами была заметна.
const PAGE_LIMIT: i64 = 50;

/// Глубины пагинации (1-based номер страницы).
const PAGES: &[i64] = &[1, 100, 1000];

/// Размеры seed'а.
const SIZES: &[(&str, usize)] = &[("10k", 10_000), ("100k", 100_000), ("1m", 1_000_000)];

/// Батч для seed-inserts. 5000 совпадает с продовым `BATCH_SIZE`.
const SEED_BATCH: usize = 5000;

// ============================================================
// Setup: pool + migrations
// ============================================================

/// Открывает pool и прогоняет миграции. Возвращает `None`, если
/// переменные окружения не заданы — bench должен gracefully skip, а не
/// падать, иначе его нельзя запустить в среде без БД.
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

        // Прогоняем миграции — идемпотентно, sqlx ведёт `_sqlx_migrations`.
        sqlx::migrate!("../../migrations").run(&pool).await.ok()?;

        Some(pool)
    })
}

// ============================================================
// Seed
// ============================================================

/// Заполняет `cruise_provider_tours` `size` строками для нового provider.
///
/// ## Распределение begin_date
///
/// `begin_date = 2026-01-01 + (i % 365)` дней. На 1M строк это ~2700 строк
/// на дату. Ключ сортировки в `list_cruises` — `(begin_date ASC,
/// cruise_id ASC)`, значит строки упорядочены группами по дате, внутри —
/// по лексикографически отсортированному `cruise_id`.
///
/// `cruise_id = format!("bench-{i:07}")` — 7-значный zero-padded, сортируется
/// лексикографически как числовой. Это гарантирует, что порядок в БД
/// совпадает с порядком по `i`.
///
/// ## Batching
///
/// Вставка батчами через `UNNEST` (по 5000 строк за statement). 1M строк =
/// 200 statement'ов. Без явной транзакции — каждая пачка в автокоммите.
/// Для bench-setup это быстрее, чем одна гигантская транзакция (нет
/// долгого WAL flush в конце).
async fn seed_tours(pool: &PgPool, provider_id: &str, size: usize) {
    // ----- 1. provider + object (FK prerequisites) -----
    sqlx::query("INSERT INTO cruise_providers (id, name) VALUES ($1, $2)")
        .bind(provider_id)
        .bind(format!("Bench {provider_id}"))
        .execute(pool)
        .await
        .expect("insert bench provider");

    sqlx::query(
        "INSERT INTO cruise_provider_objects
             (cruise_provider_id, cruise_provider_object_id, name, is_active, updated_at)
         VALUES ($1, $2, 'Bench Object', true, now())",
    )
    .bind(provider_id)
    .bind(BENCH_OBJECT_ID)
    .execute(pool)
    .await
    .expect("insert bench object");

    // ----- 2. tours пачками -----
    let start_date = chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("valid date");
    let total_batches = size.div_ceil(SEED_BATCH);

    for batch_idx in 0..total_batches {
        let start = batch_idx * SEED_BATCH;
        let end = (start + SEED_BATCH).min(size);
        let n = end - start;

        let mut cids: Vec<String> = Vec::with_capacity(n);
        let mut bdates: Vec<chrono::NaiveDate> = Vec::with_capacity(n);
        let mut edates: Vec<chrono::NaiveDate> = Vec::with_capacity(n);
        let mut names: Vec<String> = Vec::with_capacity(n);

        for i in start..end {
            let day_offset = (i % 365) as i64;
            let bdate = start_date + chrono::Duration::days(day_offset);
            let edate = bdate + chrono::Duration::days(5);

            cids.push(format!("bench-{i:07}"));
            bdates.push(bdate);
            edates.push(edate);
            names.push(format!("Bench Route {i}"));
        }

        sqlx::query(
            "INSERT INTO cruise_provider_tours
                 (cruise_provider_id, cruise_provider_object_id, cruise_provider_cruise_id,
                  cruise_type_id, begin_date, end_date, name, is_active, updated_at)
             SELECT $1, $2, t.cid, 1, t.bdate, t.edate, t.name, true, now()
             FROM UNNEST($3::text[], $4::date[], $5::date[], $6::text[])
                  AS t(cid, bdate, edate, name)",
        )
        .bind(provider_id)
        .bind(BENCH_OBJECT_ID)
        .bind(&cids)
        .bind(&bdates)
        .bind(&edates)
        .bind(&names)
        .execute(pool)
        .await
        .expect("seed tours batch");
    }

    // ----- 3. Обновить статистику planner'а -----
    //
    // КРИТИЧНО для benchmark-а. Autovacuum-analyze на свежевставленной
    // таблице срабатывает по порогу `autovacuum_analyze_scale_factor`
    // (дефолт 10% от размера). На 1M строк это 100k изменений до
    // автоматического пересчёта — то есть на свежеvставленные данные
    // planner смотрит как на «маленькую таблицу». Симптом — дисперсия
    // 30-40% между прогонами и suboptimal plans (index scan вместо
    // index seek).
    //
    // `ANALYZE` явно пересчитывает pg_statistic для таблицы: на 100k
    // строк это единицы миллисекунд, на 1M — десятки. На общее время
    // setup не влияет.
    //
    // Анализируем только tours — остальные таблицы в этом бенчмарке
    // не участвуют в запросах list_cruises (objects join по PK, lazy
    // join, stats там не критичны).
    sqlx::query("ANALYZE cruise_provider_tours")
        .execute(pool)
        .await
        .expect("analyze after seed");
}

/// Удаляет все bench-строки провайдера. Порядок — children → parent (FK).
async fn cleanup(pool: &PgPool, provider_id: &str) {
    // Имена таблиц — константы в исходнике, не из ввода. `format!`
    // безопасен: SQL injection невозможен.
    for table in &["cruise_provider_tours", "cruise_provider_objects"] {
        sqlx::query(&format!(
            "DELETE FROM {table} WHERE cruise_provider_id = $1"
        ))
        .bind(provider_id)
        .execute(pool)
        .await
        .ok();
    }
    sqlx::query("DELETE FROM cruise_providers WHERE id = $1")
        .bind(provider_id)
        .execute(pool)
        .await
        .ok();
}

// ============================================================
// Cursor computation
// ============================================================

/// Находит `(begin_date, cruise_id)` на позиции `(page - 1) * PAGE_LIMIT - 1`
/// (0-based). Это **последняя строка предыдущей страницы** — то, что клиент
/// кладёт в `next_cursor`.
///
/// Возвращает `None` для `page <= 1`: клиент начинает с начала, курсора нет.
///
/// ## OFFSET в этом запросе
///
/// Мы используем `OFFSET` — но это **setup**, не измерение. Выполняется
/// один раз на глубину, до `b.iter`. Если бы мы измеряли OFFSET — это был
/// бы другой бенчмарк; здесь мы просто находим точку старта.
async fn cursor_for_page(pool: &PgPool, provider_id: &str, page: i64) -> Option<CruiseListCursor> {
    if page <= 1 {
        return None;
    }

    // page=2 → строка 50 (1-based) — последняя отданная на 1-й странице.
    // В 0-based это индекс 49.
    let offset = (page - 1) * PAGE_LIMIT - 1;

    #[derive(sqlx::FromRow)]
    struct Row {
        begin_date: chrono::NaiveDate,
        cruise_id: String,
    }

    let row: Option<Row> = sqlx::query_as(
        "SELECT begin_date, cruise_provider_cruise_id AS cruise_id
         FROM cruise_provider_tours
         WHERE cruise_provider_id = $1 AND is_active = true
         ORDER BY begin_date ASC, cruise_provider_cruise_id ASC
         OFFSET $2 LIMIT 1",
    )
    .bind(provider_id)
    .bind(offset)
    .fetch_optional(pool)
    .await
    .expect("cursor lookup");

    row.map(|r| CruiseListCursor::new(r.begin_date, r.cruise_id))
}

// ============================================================
// Benchmark
// ============================================================

fn bench_list_cruises(c: &mut Criterion) {
    // Runtime на весь bench. `worker_threads = 2` — достаточно для одной
    // последовательной query; больше тредов только добавляет contention.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build tokio runtime");

    let Some(pool) = setup(&rt) else {
        eprintln!(
            "list_cruises bench: TEST_DATABASE_URL / DATABASE_URL not set — skipping. \
             Set either variable and re-run."
        );
        return;
    };

    let repo = PostgresCruiseReadRepository::new(pool.clone());

    for (size_label, size) in SIZES {
        let provider_id = format!("{PROVIDER_PREFIX}-{size_label}-{}", uuid::Uuid::new_v4());

        eprintln!(
            "list_cruises/{size_label}: seeding {size} tours under provider_id={provider_id}"
        );

        // Setup — до criterion-итераций. Не измеряется.
        rt.block_on(seed_tours(&pool, &provider_id, *size));

        // Группа на каждый размер — criterion не смешивает baseline'ы
        // разных размеров БД.
        let mut group = c.benchmark_group(format!("list_cruises/{size_label}"));
        // 1 query на итерацию.
        group.throughput(Throughput::Elements(1));

        let max_pages = (*size as i64) / PAGE_LIMIT;

        for &page in PAGES {
            if page > max_pages {
                eprintln!(
                    "list_cruises/{size_label}: skipping page {page} \
                     (max pages at size {size} = {max_pages})"
                );
                continue;
            }

            let cursor = rt.block_on(cursor_for_page(&pool, &provider_id, page));

            let filter = CruiseListFilter {
                cruise_provider_id: Some(provider_id.clone()),
                limit: PAGE_LIMIT,
                cursor,
                ..Default::default()
            };

            group.bench_with_input(BenchmarkId::new("page", page), &filter, |b, filter| {
                b.iter(|| {
                    // block_on внутри iteration: criterion синхронный, а
                    // наш API асинхронный. Каждый iteration = полный
                    // round-trip list_cruises через реальный pool.
                    let result = rt
                        .block_on(repo.list_cruises(criterion::black_box(filter)))
                        .expect("list_cruises");
                    criterion::black_box(result);
                });
            });
        }

        group.finish();

        // Cleanup после каждой группы — не держим 1M строк дольше нужного.
        //
        // `BENCH_KEEP_DATA=1` сохраняет данные (для последующего EXPLAIN
        // в psql вручную). Без флага — bench самоочищается.
        //
        // Очистка сохранённого вручную:
        //   DELETE FROM cruise_provider_tours   WHERE cruise_provider_id LIKE 'bench-list-%';
        //   DELETE FROM cruise_provider_objects WHERE cruise_provider_id LIKE 'bench-list-%';
        //   DELETE FROM cruise_providers        WHERE id                 LIKE 'bench-list-%';
        if std::env::var("BENCH_KEEP_DATA").is_ok() {
            eprintln!("list_cruises/{size_label}: KEEPING data (BENCH_KEEP_DATA set)");
            eprintln!("  provider_id = {provider_id}");
            eprintln!("  manual cleanup:");
            eprintln!("    DELETE FROM cruise_provider_tours   WHERE cruise_provider_id = '{provider_id}';");
            eprintln!("    DELETE FROM cruise_provider_objects WHERE cruise_provider_id = '{provider_id}';");
            eprintln!("    DELETE FROM cruise_providers        WHERE id                 = '{provider_id}';");
        } else {
            rt.block_on(cleanup(&pool, &provider_id));
        }
    }

    // Закрываем pool перед выходом. `close()` принимает `self` — берём
    // pool по значению через move в async-блок.
    rt.block_on(async move {
        pool.close().await;
    });
}

criterion_group!(benches, bench_list_cruises);
criterion_main!(benches);
