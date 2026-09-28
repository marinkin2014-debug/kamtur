//! Общие хелперы для integration-тестов.
//!
//! Все тесты в этом модуле работают с реальной БД (`TEST_DATABASE_URL` или
//! `DATABASE_URL`) и сериализуются через `db_lock()`.
//!
//! ## Конфигурация
//!
//! URL тестовой БД берётся из:
//!
//! 1. Переменной окружения `TEST_DATABASE_URL`.
//! 2. Переменной окружения `DATABASE_URL`.
//! 3. Файла `.env` в корне воркспейса — `dotenvy::dotenv()` ищет его
//!    в CWD и поднимается по родительским директориям. При `cargo test`
//!    CWD = `crates/infrastructure/`, поэтому находится `../../.env`.
//!
//! Приоритет: реальная env-переменная переопределяет `.env`. Это
//! позволяет CI задать `TEST_DATABASE_URL` без правки файлов, а локально
//! разработчику — прописать в `.env` один раз.
//!
//! ## Миграции
//!
//! `pool()` при первом вызове за сессию прогоняет `sqlx::migrate!` на
//! целевой БД. Это делается один раз через `OnceCell` — не на каждый
//! тест. Идемпотентно (sqlx ведёт `_sqlx_migrations`), поэтому после
//! `docker down -v` достаточно прогнать тесты — схема создастся
//! автоматически.
//!
//! ## Изоляция данных
//!
//! `test_provider_id(prefix)` даёт уникальный `provider_id`, все тестовые
//! данные начинаются с `test-`. `purge_all_test_rows()` чистит их в начале
//! каждого теста.
//!
//! ## Почему `pool()` возвращает owned `PgPool`, а не `&'static PgPool`
//!
//! `#[tokio::test]` создаёт новый Tokio runtime на каждый тест. SQLx внутри
//! пула spawn'ит задачи на runtime, в котором пул создан. Если пул
//! кэширован в `static`, то при смене теста старый runtime умирает, и
//! TCP-соединения остаются открытыми, но никто не читает ответы. Первый
//! `acquire()` в следующем тесте ждёт таймаут (~8 сек). Owned pool на тест
//! решает проблему.

use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use tokio::sync::{Mutex, MutexGuard, OnceCell};

static DB_TEST_MUTEX: Mutex<()> = Mutex::const_new(());
static MIGRATED: OnceCell<()> = OnceCell::const_new();

/// Глобальный мьютекс для тестов, работающих с БД.
///
/// Сериализует все integration-тесты. Причина: `claim_batch`,
/// `sweep_dead_claims`, `cleanup_orphan_runs` работают по всей таблице,
/// и параллельные тесты крадут строки друг у друга.
pub async fn db_lock() -> MutexGuard<'static, ()> {
    DB_TEST_MUTEX.lock().await
}

/// Создаёт новый пул при каждом вызове. Прогоняет миграции один раз за
/// сессию (через `MIGRATED: OnceCell`).
///
/// Загружает `.env` из корня воркспейса — если он есть. Порядок разрешения
/// URL: env → .env → паника.
pub async fn pool() -> PgPool {
    // Идемпотентно: повторные вызовы no-op. Ищем .env в CWD и выше.
    let _ = dotenvy::dotenv();

    let url = std::env::var("TEST_DATABASE_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .expect(
            "TEST_DATABASE_URL or DATABASE_URL must be set for integration tests. \
             Either export the variable or add it to workspace .env",
        );

    let pool = PgPoolOptions::new()
        .max_connections(5)
        .min_connections(1)
        .acquire_timeout(std::time::Duration::from_secs(5))
        .connect(&url)
        .await
        .expect("connect postgres");

    // Один раз за процесс — прогнать миграции.
    // Параллельные тесты ждут первого; идемпотентно.
    MIGRATED
        .get_or_init(|| async {
            sqlx::migrate!("../../migrations")
                .run(&pool)
                .await
                .expect("run migrations on test db");
        })
        .await;

    pool
}

/// Уникальный provider_id с префиксом `test-`.
pub fn test_provider_id(prefix: &str) -> String {
    format!("test-{}-{}", prefix, uuid::Uuid::new_v4())
}

/// Удаляет все тестовые строки. Вызывается в начале каждого теста.
pub async fn purge_all_test_rows(pool: &PgPool) {
    // Порядок важен из-за FK.
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
            "DELETE FROM {table} WHERE cruise_provider_id LIKE 'test-%'"
        ))
        .execute(pool)
        .await
        .ok();
    }
    sqlx::query("DELETE FROM cruise_providers WHERE id LIKE 'test-%'")
        .execute(pool)
        .await
        .ok();
}

pub async fn insert_provider(pool: &PgPool, id: &str) {
    sqlx::query("INSERT INTO cruise_providers (id, name) VALUES ($1, $2)")
        .bind(id)
        .bind(format!("Test {id}"))
        .execute(pool)
        .await
        .expect("insert test provider");
}

pub async fn cleanup_provider(pool: &PgPool, id: &str) {
    purge_all_test_rows(pool).await;
    sqlx::query("DELETE FROM cruise_providers WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await
        .ok();
}
