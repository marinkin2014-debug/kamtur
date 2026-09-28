//! Замер производительности соединения с Postgres.
//! Используется для диагностики медленных тестов.
//!
//! Запуск:
//!   $env:TEST_DATABASE_URL = "postgres://test:test@127.0.0.1:5433/kamtur_test"
//!   cargo run --example db_bench -p worker

use std::time::Instant;

use sqlx::postgres::PgPoolOptions;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let url = std::env::var("TEST_DATABASE_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .expect("TEST_DATABASE_URL or DATABASE_URL");
    println!("target: {url}");

    // 1. Первое соединение
    let t = Instant::now();
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .min_connections(2)
        .connect(&url)
        .await?;
    println!(
        "1. connect() (with 2 min connections):  {:>10.1?}",
        t.elapsed()
    );

    // 2. 100 тривиальных запросов через пул
    let t = Instant::now();
    for _ in 0..100 {
        let _: i32 = sqlx::query_scalar("SELECT 1").fetch_one(&pool).await?;
    }
    println!(
        "2. 100x SELECT 1 (same pool):            {:>10.1?}",
        t.elapsed()
    );

    // 3. 100 транзакций (begin + query + rollback)
    let t = Instant::now();
    for _ in 0..100 {
        let mut tx = pool.begin().await?;
        let _: i32 = sqlx::query_scalar("SELECT 1").fetch_one(&mut *tx).await?;
        tx.rollback().await?;
    }
    println!(
        "3. 100x begin/query/rollback:            {:>10.1?}",
        t.elapsed()
    );

    // 4. 10 свежих соединений (симулирует pool-per-test)
    let t = Instant::now();
    for _ in 0..10 {
        let p = PgPoolOptions::new()
            .max_connections(1)
            .connect(&url)
            .await?;
        let _: i32 = sqlx::query_scalar("SELECT 1").fetch_one(&p).await?;
        p.close().await;
    }
    println!(
        "4. 10x fresh connection + query:         {:>10.1?}",
        t.elapsed()
    );

    // 5. 100 INSERT + DELETE в свежей таблице
    sqlx::query("CREATE TEMP TABLE IF NOT EXISTS bench (id serial primary key, v text)")
        .execute(&pool)
        .await?;
    let t = Instant::now();
    for i in 0..100 {
        sqlx::query("INSERT INTO bench (v) VALUES ($1)")
            .bind(format!("v{i}"))
            .execute(&pool)
            .await?;
    }
    println!(
        "5. 100x INSERT:                          {:>10.1?}",
        t.elapsed()
    );

    pool.close().await;
    Ok(())
}
