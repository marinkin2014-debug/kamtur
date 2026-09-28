use chrono::{DateTime, Utc};
use sqlx::PgPool;
use tracing::info;

use domain::entities::{ClaimedErrorBatch, ErrorDigestEntry, RetentionOutcome};
use domain::errors::RepositoryError;

// ============================================================
// Retention
// ============================================================

pub(super) async fn cleanup_old_data(
    pool: &PgPool,
    raw_snapshots_days: i32,
    health_checks_days: i32,
    resolved_errors_days: i32,
) -> Result<RetentionOutcome, RepositoryError> {
    // 1. raw_snapshots: удаляем старые, кроме тех, на которые ссылаются sync_runs.
    let raw_deleted = sqlx::query(
        "DELETE FROM raw_snapshots
         WHERE fetched_at < now() - make_interval(days => $1)
           AND id NOT IN (
               SELECT raw_snapshot_id FROM sync_runs WHERE raw_snapshot_id IS NOT NULL
           )",
    )
    .bind(raw_snapshots_days)
    .execute(pool)
    .await
    .map_err(|e| RepositoryError::Connection(e.to_string()))?
    .rows_affected();

    // 2. health_checks: просто старые.
    let health_deleted = sqlx::query(
        "DELETE FROM health_checks
         WHERE checked_at < now() - make_interval(days => $1)",
    )
    .bind(health_checks_days)
    .execute(pool)
    .await
    .map_err(|e| RepositoryError::Connection(e.to_string()))?
    .rows_affected();

    // 3. errors: только resolved, старше N дней.
    let errors_deleted = sqlx::query(
        "DELETE FROM errors
         WHERE resolved = true
           AND occurred_at < now() - make_interval(days => $1)",
    )
    .bind(resolved_errors_days)
    .execute(pool)
    .await
    .map_err(|e| RepositoryError::Connection(e.to_string()))?
    .rows_affected();

    info!(
        raw_snapshots_deleted = raw_deleted,
        health_checks_deleted = health_deleted,
        errors_deleted = errors_deleted,
        "retention completed"
    );

    Ok(RetentionOutcome {
        raw_snapshots_deleted: raw_deleted,
        health_checks_deleted: health_deleted,
        errors_deleted,
    })
}

/// Возвращает `(total_bytes, total_count)` для таблицы `raw_snapshots`.
///
/// `total_bytes` — реальный размер таблицы на диске: heap, TOAST, индексы,
/// FSM и VM. Источник — `pg_total_relation_size()`, читает метаданные
/// из `pg_class`. Операция O(1).
///
/// ## Почему не `sum(octet_length(payload))`
///
/// Раньше функция делала `sum(octet_length(payload))`. Это:
///
/// 1. Читало все TOAST-страницы — несколько тысяч случайных IO на 100+ МБ.
/// 2. Недооценивало реальный размер: не учитывало TOAST overhead,
///    индексы, FSM, VM.
/// 3. Вызывалось каждые 5 минут из retention loop и после каждого
///    `cleanup_old_data`.
///
/// `pg_total_relation_size` — O(1), даёт честный размер на диске.
/// Метрика `kamtur_raw_snapshots_size_bytes` может вырасти в 2-3 раза
/// относительно прежнего значения — это нормально, она теперь отражает
/// реальность.
///
/// `total_count` — точный `count(*)`. Оставлен как есть (не заменён на
/// `pg_stat_user_tables.n_live_tup`), потому что count использует
/// index-only scan и точен. Для gauge в Prometheus точность важнее
/// скорости на этом объёме.
pub async fn raw_snapshots_stats(pool: &PgPool) -> Result<(u64, i64), RepositoryError> {
    #[derive(sqlx::FromRow)]
    struct Row {
        total_bytes: i64,
        total_count: i64,
    }

    let row: Row = sqlx::query_as(
        "SELECT
            pg_total_relation_size('raw_snapshots')::bigint AS total_bytes,
            (SELECT count(*) FROM raw_snapshots)::bigint   AS total_count",
    )
    .fetch_one(pool)
    .await
    .map_err(|e| RepositoryError::Connection(e.to_string()))?;

    Ok((row.total_bytes.max(0) as u64, row.total_count))
}

// ============================================================
// Orphan sync_runs
// ============================================================

/// Помечает зависшие `running` записи в `sync_runs` как `failed`.
pub async fn cleanup_orphan_runs(
    pool: &PgPool,
    stale_after_secs: i64,
) -> Result<u64, RepositoryError> {
    let affected = sqlx::query(
        "UPDATE sync_runs
         SET status = 'failed',
             finished_at = now(),
             error_message = COALESCE(error_message, 'orphan: worker restarted while running')
         WHERE status = 'running'
           AND started_at < now() - make_interval(secs => $1)",
    )
    .bind(stale_after_secs)
    .execute(pool)
    .await
    .map_err(|e| RepositoryError::Connection(e.to_string()))?
    .rows_affected();

    Ok(affected)
}

// ============================================================
// Error notification: claim / mark_sent / mark_failed / sweep
// ============================================================

/// Атомарно забирает батч ошибок на отправку.
///
/// Состояния (см. migration `errors_notification_state.sql`):
///
/// - `pending` — готово к отправке.
/// - `claimed` — забрано воркером, lease активен.
/// - `sent`    — успешно отправлено.
/// - `dead`    — попытки исчерпаны, сдались.
///
/// `FOR UPDATE SKIP LOCKED` защищает от параллельных claim'ов.
pub(super) async fn claim_batch(
    pool: &PgPool,
    window_secs: i64,
    max_attempts: i32,
    lease_secs: i64,
    limit: i64,
) -> Result<ClaimedErrorBatch, RepositoryError> {
    #[derive(sqlx::FromRow)]
    struct Raw {
        id: i64,
        stage: String,
        severity: String,
        message: String,
        occurred_at: DateTime<Utc>,
    }

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| RepositoryError::Connection(e.to_string()))?;

    let rows: Vec<Raw> = sqlx::query_as(
        r#"
        UPDATE errors
        SET notification_state = 'claimed',
            notification_claimed_at = now(),
            notification_attempts = notification_attempts + 1
        WHERE id IN (
            SELECT id FROM errors
            WHERE resolved = false
              AND notified_at IS NULL
              AND notification_attempts < $2
              AND occurred_at > now() - make_interval(secs => $1)
              AND (
                  notification_state = 'pending'
                  OR (notification_state = 'claimed'
                      AND notification_claimed_at < now() - make_interval(secs => $3))
              )
            ORDER BY occurred_at ASC
            LIMIT $4
            FOR UPDATE SKIP LOCKED
        )
        RETURNING id, stage, severity, message, occurred_at
        "#,
    )
    .bind(window_secs)
    .bind(max_attempts)
    .bind(lease_secs)
    .bind(limit)
    .fetch_all(&mut *tx)
    .await
    .map_err(|e| RepositoryError::Transaction(e.to_string()))?;

    tx.commit()
        .await
        .map_err(|e| RepositoryError::Transaction(e.to_string()))?;

    if rows.is_empty() {
        return Ok(ClaimedErrorBatch::empty());
    }

    let claimed_ids: Vec<i64> = rows.iter().map(|r| r.id).collect();

    // Группировка в Rust: экономим второй SQL-заход.
    use std::collections::HashMap;

    struct Group {
        count: i64,
        first_seen: DateTime<Utc>,
        last_seen: DateTime<Utc>,
        latest_message: String,
        latest_at: DateTime<Utc>,
    }

    let mut groups: HashMap<(String, String, String), Group> = HashMap::new();
    for r in rows {
        // Префикс: первые 80 байт. Обрезаем по границе UTF-8.
        let prefix = if r.message.len() > 80 {
            let mut end = 80;
            while !r.message.is_char_boundary(end) && end > 0 {
                end -= 1;
            }
            r.message[..end].to_string()
        } else {
            r.message.clone()
        };

        let key = (r.stage, r.severity, prefix);

        let group = groups.entry(key).or_insert_with(|| Group {
            count: 0,
            first_seen: r.occurred_at,
            last_seen: r.occurred_at,
            latest_message: r.message.clone(),
            latest_at: r.occurred_at,
        });

        group.count += 1;
        if r.occurred_at < group.first_seen {
            group.first_seen = r.occurred_at;
        }
        if r.occurred_at > group.last_seen {
            group.last_seen = r.occurred_at;
        }
        if r.occurred_at > group.latest_at {
            group.latest_at = r.occurred_at;
            group.latest_message = r.message;
        }
    }

    let mut entries: Vec<ErrorDigestEntry> = groups
        .into_iter()
        .map(|((stage, severity, message_prefix), g)| ErrorDigestEntry {
            stage,
            severity,
            message_prefix,
            count: g.count,
            first_seen: g.first_seen,
            last_seen: g.last_seen,
            sample_message: g.latest_message,
        })
        .collect();

    // Сортировка: сначала самые частые.
    entries.sort_by_key(|e| std::cmp::Reverse(e.count));

    Ok(ClaimedErrorBatch {
        entries,
        claimed_ids,
    })
}

pub(super) async fn mark_sent(pool: &PgPool, ids: &[i64]) -> Result<(), RepositoryError> {
    if ids.is_empty() {
        return Ok(());
    }

    sqlx::query(
        "UPDATE errors
         SET notification_state = 'sent', notified_at = now()
         WHERE id = ANY($1)
           AND notification_state = 'claimed'",
    )
    .bind(ids)
    .execute(pool)
    .await
    .map_err(|e| RepositoryError::Connection(e.to_string()))?;

    Ok(())
}

pub(super) async fn mark_failed(
    pool: &PgPool,
    ids: &[i64],
    max_attempts: i32,
) -> Result<(), RepositoryError> {
    if ids.is_empty() {
        return Ok(());
    }

    sqlx::query(
        "UPDATE errors
         SET notification_state = CASE
                 WHEN notification_attempts >= $2 THEN 'dead'
                 ELSE 'pending'
             END
         WHERE id = ANY($1)
           AND notification_state = 'claimed'",
    )
    .bind(ids)
    .bind(max_attempts)
    .execute(pool)
    .await
    .map_err(|e| RepositoryError::Connection(e.to_string()))?;

    Ok(())
}

pub(super) async fn sweep_dead_claims(
    pool: &PgPool,
    max_attempts: i32,
    lease_secs: i64,
) -> Result<u64, RepositoryError> {
    let affected = sqlx::query(
        "UPDATE errors
         SET notification_state = 'dead'
         WHERE notification_state = 'claimed'
           AND notification_attempts >= $1
           AND notification_claimed_at < now() - make_interval(secs => $2)",
    )
    .bind(max_attempts)
    .bind(lease_secs)
    .execute(pool)
    .await
    .map_err(|e| RepositoryError::Connection(e.to_string()))?
    .rows_affected();

    Ok(affected)
}

// ============================================================
// Tests
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repositories::postgres::test_support::{
        cleanup_provider, db_lock, insert_provider, pool, purge_all_test_rows, test_provider_id,
    };

    async fn insert_error(
        pool: &PgPool,
        provider_id: &str,
        occurred_at: DateTime<Utc>,
        state: &str,
        attempts: i32,
        claimed_at: Option<DateTime<Utc>>,
    ) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO errors
                (cruise_provider_id, stage, severity, message, context,
                 occurred_at, resolved, notification_state,
                 notification_claimed_at, notification_attempts)
             VALUES ($1, 'test', 'error', 'test message', '{}'::jsonb,
                     $2, false, $3, $4, $5)
             RETURNING id",
        )
        .bind(provider_id)
        .bind(occurred_at)
        .bind(state)
        .bind(claimed_at)
        .bind(attempts)
        .fetch_one(pool)
        .await
        .expect("insert error")
    }

    async fn error_state(pool: &PgPool, id: i64) -> (String, i32, Option<DateTime<Utc>>) {
        sqlx::query_as(
            "SELECT notification_state, notification_attempts, notified_at
             FROM errors WHERE id = $1",
        )
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("fetch error state")
    }

    // ============================================================
    // raw_snapshots_stats
    // ============================================================
    //
    // Тесты работают с таблицей целиком (функция возвращает статистику
    // по всей `raw_snapshots`, не по провайдеру). Поэтому они не
    // предполагают пустую таблицу и не зовут `purge_all_test_rows` —
    // проверяют только дельту.

    /// Smoke-тест: функция возвращает валидные числа.
    ///
    /// Таблица `raw_snapshots` существует после миграций. Даже пустая
    /// она занимает минимум одну heap-страницу (8 KB) плюс индексы —
    /// `pg_total_relation_size` гарантирует `bytes > 0`.
    #[tokio::test]
    async fn raw_snapshots_stats_returns_valid_numbers() {
        let _guard = db_lock().await;
        let pool_owned = pool().await;
        let pool = &pool_owned;

        let (bytes, count) = raw_snapshots_stats(pool).await.expect("stats");

        assert!(
            bytes > 0,
            "raw_snapshots существует → размер > 0, got {bytes}"
        );
        assert!(count >= 0, "count не может быть отрицательным, got {count}");
    }

    /// Функция возвращает согласованные данные после вставки snapshot:
    /// count растёт на 1, bytes — увеличивается.
    ///
    /// Сравнение идёт с состоянием «до», а не с нулём, потому что в
    /// тестовой БД могут быть не-`test-*` строки (следы ручных запусков
    /// воркера). `purge_all_test_rows` их не трогает — так и задумано.
    #[tokio::test]
    async fn raw_snapshots_stats_grows_after_insert() {
        let _guard = db_lock().await;
        let pool_owned = pool().await;
        let pool = &pool_owned;

        let pid = test_provider_id("stats");
        insert_provider(pool, &pid).await;

        let (before_bytes, before_count) = raw_snapshots_stats(pool).await.expect("stats before");

        // 1 МБ payload — гарантированно меняет page-счётчик.
        let payload = vec![0u8; 1024 * 1024];
        sqlx::query(
            "INSERT INTO raw_snapshots
                 (cruise_provider_id, sha256, payload, payload_encoding, fetched_at)
             VALUES ($1, $2, $3, 'none', now())",
        )
        .bind(&pid)
        .bind("0".repeat(64))
        .bind(&payload)
        .execute(pool)
        .await
        .expect("insert snapshot");

        let (after_bytes, after_count) = raw_snapshots_stats(pool).await.expect("stats after");

        assert_eq!(
            after_count,
            before_count + 1,
            "count должен вырасти ровно на 1: before={before_count}, after={after_count}"
        );
        assert!(
            after_bytes > before_bytes,
            "размер должен вырасти: before={before_bytes}, after={after_bytes}"
        );

        cleanup_provider(pool, &pid).await;
    }

    // ============================================================
    // claim_batch
    // ============================================================

    /// claim_batch переводит pending → claimed, инкрементит attempts,
    /// но НЕ трогает notified_at.
    #[tokio::test]
    async fn claim_batch_does_not_set_notified_at() {
        let _guard = db_lock().await;
        let pool_owned = pool().await;
        let pool = &pool_owned;
        purge_all_test_rows(pool).await;
        let pid = test_provider_id("digest");
        insert_provider(pool, &pid).await;

        let id = insert_error(pool, &pid, Utc::now(), "pending", 0, None).await;

        let batch = claim_batch(pool, 86_400, 5, 600, 100).await.expect("claim");
        assert!(batch.claimed_ids.contains(&id));

        let (state, attempts, notified_at) = error_state(pool, id).await;
        assert_eq!(state, "claimed");
        assert_eq!(attempts, 1);
        assert!(
            notified_at.is_none(),
            "notified_at must stay NULL until mark_sent"
        );

        cleanup_provider(pool, &pid).await;
    }

    #[tokio::test]
    async fn claim_batch_skips_actively_claimed() {
        let _guard = db_lock().await;
        let pool_owned = pool().await;
        let pool = &pool_owned;
        purge_all_test_rows(pool).await;
        let pid = test_provider_id("digest");
        insert_provider(pool, &pid).await;

        let id = insert_error(pool, &pid, Utc::now(), "pending", 0, None).await;

        let batch1 = claim_batch(pool, 86_400, 5, 600, 100)
            .await
            .expect("claim1");
        assert!(batch1.claimed_ids.contains(&id));

        let batch2 = claim_batch(pool, 86_400, 5, 600, 100)
            .await
            .expect("claim2");
        assert!(
            !batch2.claimed_ids.contains(&id),
            "second claim should not see the actively-claimed row"
        );

        cleanup_provider(pool, &pid).await;
    }

    #[tokio::test]
    async fn claim_batch_respects_max_attempts() {
        let _guard = db_lock().await;
        let pool_owned = pool().await;
        let pool = &pool_owned;
        purge_all_test_rows(pool).await;
        let pid = test_provider_id("digest");
        insert_provider(pool, &pid).await;

        let id = insert_error(pool, &pid, Utc::now(), "pending", 5, None).await;

        let batch = claim_batch(pool, 86_400, 5, 600, 100).await.expect("claim");
        assert!(
            !batch.claimed_ids.contains(&id),
            "row at max attempts must not be claimed"
        );

        cleanup_provider(pool, &pid).await;
    }

    #[tokio::test]
    async fn claim_batch_respects_window() {
        let _guard = db_lock().await;
        let pool_owned = pool().await;
        let pool = &pool_owned;
        purge_all_test_rows(pool).await;
        let pid = test_provider_id("digest");
        insert_provider(pool, &pid).await;

        let old = Utc::now() - chrono::Duration::hours(48);
        let id = insert_error(pool, &pid, old, "pending", 0, None).await;

        // окно 1 час → запись возрастом 48 часов не подходит
        let batch = claim_batch(pool, 3600, 5, 600, 100).await.expect("claim");
        assert!(
            !batch.claimed_ids.contains(&id),
            "old row must not be claimed outside window"
        );

        cleanup_provider(pool, &pid).await;
    }

    #[tokio::test]
    async fn claim_batch_reclaims_expired_lease() {
        let _guard = db_lock().await;
        let pool_owned = pool().await;
        let pool = &pool_owned;
        purge_all_test_rows(pool).await;
        let pid = test_provider_id("digest");
        insert_provider(pool, &pid).await;

        // claimed 1 час назад, lease 10 минут → истёк
        let claimed_at = Utc::now() - chrono::Duration::hours(1);
        let id = insert_error(pool, &pid, Utc::now(), "claimed", 1, Some(claimed_at)).await;

        let batch = claim_batch(pool, 86_400, 5, 600, 100).await.expect("claim");
        assert!(
            batch.claimed_ids.contains(&id),
            "expired lease should be reclaimable"
        );

        let (state, attempts, _) = error_state(pool, id).await;
        assert_eq!(state, "claimed");
        assert_eq!(attempts, 2, "attempts must be incremented on reclaim");

        cleanup_provider(pool, &pid).await;
    }

    // ============================================================
    // mark_sent
    // ============================================================

    #[tokio::test]
    async fn mark_sent_transitions_to_sent_and_sets_notified_at() {
        let _guard = db_lock().await;
        let pool_owned = pool().await;
        let pool = &pool_owned;
        purge_all_test_rows(pool).await;
        let pid = test_provider_id("digest");
        insert_provider(pool, &pid).await;

        let id = insert_error(pool, &pid, Utc::now(), "pending", 0, None).await;
        claim_batch(pool, 86_400, 5, 600, 100).await.expect("claim");

        mark_sent(pool, &[id]).await.expect("mark_sent");

        let (state, _, notified_at) = error_state(pool, id).await;
        assert_eq!(state, "sent");
        assert!(
            notified_at.is_some(),
            "notified_at must be set by mark_sent"
        );

        cleanup_provider(pool, &pid).await;
    }

    #[tokio::test]
    async fn mark_sent_is_idempotent() {
        let _guard = db_lock().await;
        let pool_owned = pool().await;
        let pool = &pool_owned;
        purge_all_test_rows(pool).await;
        let pid = test_provider_id("digest");
        insert_provider(pool, &pid).await;

        let id = insert_error(pool, &pid, Utc::now(), "pending", 0, None).await;
        claim_batch(pool, 86_400, 5, 600, 100).await.expect("claim");
        mark_sent(pool, &[id]).await.expect("first mark_sent");

        // повторный — не должен падать
        mark_sent(pool, &[id]).await.expect("second mark_sent");

        let (state, _, _) = error_state(pool, id).await;
        assert_eq!(state, "sent");

        cleanup_provider(pool, &pid).await;
    }

    // ============================================================
    // mark_failed
    // ============================================================

    #[tokio::test]
    async fn mark_failed_returns_to_pending_when_attempts_remain() {
        let _guard = db_lock().await;
        let pool_owned = pool().await;
        let pool = &pool_owned;
        purge_all_test_rows(pool).await;
        let pid = test_provider_id("digest");
        insert_provider(pool, &pid).await;

        let id = insert_error(pool, &pid, Utc::now(), "pending", 0, None).await;
        claim_batch(pool, 86_400, 5, 600, 100).await.expect("claim");
        // attempts теперь 1

        mark_failed(pool, &[id], 5).await.expect("mark_failed");

        let (state, attempts, notified_at) = error_state(pool, id).await;
        assert_eq!(state, "pending", "attempts remain → back to pending");
        assert_eq!(attempts, 1);
        assert!(notified_at.is_none());

        cleanup_provider(pool, &pid).await;
    }

    #[tokio::test]
    async fn mark_failed_transitions_to_dead_when_max_reached() {
        let _guard = db_lock().await;
        let pool_owned = pool().await;
        let pool = &pool_owned;
        purge_all_test_rows(pool).await;
        let pid = test_provider_id("digest");
        insert_provider(pool, &pid).await;

        // attempts = 4 (не 5). После claim станет 5 = max
        let id = insert_error(pool, &pid, Utc::now(), "pending", 4, None).await;
        claim_batch(pool, 86_400, 5, 600, 100).await.expect("claim");
        // attempts теперь 5

        mark_failed(pool, &[id], 5).await.expect("mark_failed");

        let (state, attempts, _) = error_state(pool, id).await;
        assert_eq!(state, "dead", "max attempts reached → dead");
        assert_eq!(attempts, 5);

        cleanup_provider(pool, &pid).await;
    }

    // ============================================================
    // sweep_dead_claims
    // ============================================================

    #[tokio::test]
    async fn sweep_dead_claims_marks_stuck_as_dead() {
        let _guard = db_lock().await;
        let pool_owned = pool().await;
        let pool = &pool_owned;
        purge_all_test_rows(pool).await;
        let pid = test_provider_id("digest");
        insert_provider(pool, &pid).await;

        // claimed, attempts = 5, lease истёк час назад
        let claimed_at = Utc::now() - chrono::Duration::hours(1);
        let id = insert_error(pool, &pid, Utc::now(), "claimed", 5, Some(claimed_at)).await;

        let swept = sweep_dead_claims(pool, 5, 600).await.expect("sweep");
        assert!(swept >= 1, "at least one row swept");

        let (state, _, _) = error_state(pool, id).await;
        assert_eq!(state, "dead");

        cleanup_provider(pool, &pid).await;
    }

    #[tokio::test]
    async fn sweep_dead_claims_ignores_fresh_claims() {
        let _guard = db_lock().await;
        let pool_owned = pool().await;
        let pool = &pool_owned;
        purge_all_test_rows(pool).await;
        let pid = test_provider_id("digest");
        insert_provider(pool, &pid).await;

        // claimed 10 секунд назад, lease 600 → активен
        let claimed_at = Utc::now() - chrono::Duration::seconds(10);
        let id = insert_error(pool, &pid, Utc::now(), "claimed", 5, Some(claimed_at)).await;

        sweep_dead_claims(pool, 5, 600).await.expect("sweep");

        let (state, _, _) = error_state(pool, id).await;
        assert_eq!(state, "claimed", "fresh claim must not be swept");

        cleanup_provider(pool, &pid).await;
    }

    #[tokio::test]
    async fn sweep_dead_claims_ignores_attempts_remaining() {
        let _guard = db_lock().await;
        let pool_owned = pool().await;
        let pool = &pool_owned;
        purge_all_test_rows(pool).await;
        let pid = test_provider_id("digest");
        insert_provider(pool, &pid).await;

        let claimed_at = Utc::now() - chrono::Duration::hours(1);
        let id = insert_error(pool, &pid, Utc::now(), "claimed", 2, Some(claimed_at)).await;

        sweep_dead_claims(pool, 5, 600).await.expect("sweep");

        let (state, _, _) = error_state(pool, id).await;
        assert_eq!(
            state, "claimed",
            "should stay claimed — claim_batch will reclaim it"
        );

        cleanup_provider(pool, &pid).await;
    }

    // ============================================================
    // cleanup_orphan_runs
    // ============================================================

    #[tokio::test]
    async fn orphan_cleanup_marks_old_running_as_failed() {
        let _guard = db_lock().await;
        let pool_owned = pool().await;
        let pool = &pool_owned;
        purge_all_test_rows(pool).await;
        let pid = test_provider_id("digest");
        insert_provider(pool, &pid).await;

        let old_started = Utc::now() - chrono::Duration::hours(3);
        let run_id: i64 = sqlx::query_scalar(
            "INSERT INTO sync_runs (cruise_provider_id, status, started_at)
             VALUES ($1, 'running', $2) RETURNING id",
        )
        .bind(&pid)
        .bind(old_started)
        .fetch_one(pool)
        .await
        .expect("insert sync_run");

        // stale 2 часа → запись возрастом 3 часа подходит
        let affected = cleanup_orphan_runs(pool, 7200).await.expect("cleanup");
        assert!(affected >= 1);

        let (status, error_msg): (String, Option<String>) =
            sqlx::query_as("SELECT status, error_message FROM sync_runs WHERE id = $1")
                .bind(run_id)
                .fetch_one(pool)
                .await
                .expect("fetch sync_run");

        assert_eq!(status, "failed");
        assert!(
            error_msg.as_deref().unwrap_or("").contains("orphan"),
            "error_message must mention orphan, got: {:?}",
            error_msg
        );

        cleanup_provider(pool, &pid).await;
    }

    #[tokio::test]
    async fn orphan_cleanup_ignores_fresh_running() {
        let _guard = db_lock().await;
        let pool_owned = pool().await;
        let pool = &pool_owned;
        purge_all_test_rows(pool).await;
        let pid = test_provider_id("digest");
        insert_provider(pool, &pid).await;

        // started 10 секунд назад, stale 2 часа → не трогаем
        let fresh_started = Utc::now() - chrono::Duration::seconds(10);
        let run_id: i64 = sqlx::query_scalar(
            "INSERT INTO sync_runs (cruise_provider_id, status, started_at)
             VALUES ($1, 'running', $2) RETURNING id",
        )
        .bind(&pid)
        .bind(fresh_started)
        .fetch_one(pool)
        .await
        .expect("insert sync_run");

        cleanup_orphan_runs(pool, 7200).await.expect("cleanup");

        let status: String = sqlx::query_scalar("SELECT status FROM sync_runs WHERE id = $1")
            .bind(run_id)
            .fetch_one(pool)
            .await
            .expect("fetch");
        assert_eq!(status, "running", "fresh running must not be touched");

        cleanup_provider(pool, &pid).await;
    }
}
