//! Периодическая отправка дайджеста ошибок.
//!
//! Two-phase commit:
//!   1. claim_batch — атомарно забирает N ошибок, ставит lease.
//!   2. send — вне транзакции.
//!   3. ack/nack:
//!      - `mark_sent` — при успехе.
//!      - `mark_failed` — при ошибке.
//!
//! Если SMTP упал — записи вернутся в pending и будут отправлены
//! при следующем tick'е. Если worker упал между claim и ack/nack —
//! lease истечёт, следующий claim их подхватит.

use std::sync::Arc;
use std::time::Duration;

use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use domain::entities::ErrorDigestEntry;
use domain::ports::{CruiseRepository, Notifier};

/// Размер батча — сколько ошибок забираем за раз.
const MAX_BATCH: i64 = 5_000;

/// Сколько секунд claimed-запись считается «зависшей».
const LEASE_SECS: i64 = 600;

/// Окно: не отправляем об ошибках старше этого возраста.
const WINDOW_SECS: i64 = 86_400; // 24 часа

/// Максимум попыток. После — 'dead'.
const MAX_ATTEMPTS: i32 = 5;

pub async fn run_digest_loop(
    repository: Arc<dyn CruiseRepository>,
    notifier: Arc<dyn Notifier>,
    interval: Duration,
    cancel: CancellationToken,
) {
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    ticker.tick().await; // пропускаем первый tick

    loop {
        tokio::select! {
            _ = ticker.tick() => run_one_cycle(&*repository, &*notifier).await,
            _ = cancel.cancelled() => {
                info!("digest loop: shutdown");
                return;
            }
        }
    }
}

async fn run_one_cycle(repo: &dyn CruiseRepository, notifier: &dyn Notifier) {
    // Sweep: залипшие claimed с исчерпанными попытками → dead.
    match repo.sweep_dead_claims(MAX_ATTEMPTS, LEASE_SECS).await {
        Ok(0) => {}
        Ok(n) => info!(swept = n, "digest: marked stuck claims as dead"),
        Err(e) => warn!(error = %e, "digest: sweep_dead_claims failed"),
    }

    // Claim.
    let batch = match repo
        .claim_batch(WINDOW_SECS, MAX_ATTEMPTS, LEASE_SECS, MAX_BATCH)
        .await
    {
        Ok(b) => b,
        Err(e) => {
            warn!(error = %e, "digest: claim_batch failed");
            return;
        }
    };

    if batch.is_empty() {
        info!("digest: no errors to report");
        return;
    }

    let total = batch.total_events();
    let groups = batch.entries.len();
    let subject = format!("Error digest: {total} events in {groups} groups");
    let body = format_digest(&batch.entries, Duration::from_secs(WINDOW_SECS as u64));

    match notifier.send("error", &subject, &body).await {
        Ok(()) => match repo.mark_sent(&batch.claimed_ids).await {
            Ok(()) => info!(events = total, groups, "digest sent"),
            Err(e) => error!(
                error = %e,
                events = total,
                "digest sent, but mark_sent failed — will resend (idempotent)"
            ),
        },
        Err(e) => {
            error!(
                error = %e,
                events = total,
                "digest send failed — nack, will retry"
            );
            if let Err(e) = repo.mark_failed(&batch.claimed_ids, MAX_ATTEMPTS).await {
                error!(error = %e, "digest: mark_failed also failed");
            }
        }
    }
}

fn format_digest(entries: &[ErrorDigestEntry], window: Duration) -> String {
    let mut out = String::with_capacity(1024);
    out.push_str(&format!(
        "Kamtur error digest — окно {} мин\n\n",
        window.as_secs() / 60
    ));

    let total: i64 = entries.iter().map(|e| e.count).sum();
    out.push_str(&format!("Всего событий: {}\n", total));
    out.push_str(&format!("Групп: {}\n\n", entries.len()));
    out.push_str("────────────────────────────────────\n\n");

    for e in entries {
        out.push_str(&format!(
            "[{}] {} — {} шт.\n",
            e.severity.to_uppercase(),
            e.stage,
            e.count,
        ));
        out.push_str(&format!(
            "    Первое: {}\n    Последнее: {}\n",
            e.first_seen.format("%Y-%m-%d %H:%M:%S UTC"),
            e.last_seen.format("%Y-%m-%d %H:%M:%S UTC"),
        ));
        out.push_str(&format!("    Пример: {}\n\n", e.sample_message));
    }

    out
}
