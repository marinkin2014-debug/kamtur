//! CLI-диспатчер worker'а.
//!
//! Подкоманды:
//!   run                   — daemon: scheduler + health + retention + digest
//!   sync --provider=<id>  — прогнать sync одного провайдера и выйти
//!   sync --all            — прогнать sync всех провайдеров и выйти
//!
//! `sync` не поднимает фоновые задачи: только pool → providers → use case.
//! Используется для ручных операций (после фикса парсера, при инцидентах,
//! в CI-тестах). Требует тех же ENV, что и `run`.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context};
use sqlx::postgres::PgPoolOptions;
use tracing::info;

use application::use_cases::sync_all_providers::SyncAllProvidersUseCase;
use domain::entities::ProviderId;
use domain::ports::{Clock, CruiseProvider, CruiseRepository, MetricsRecorder};
use infrastructure::clock::SystemClock;
use infrastructure::metrics::NoopMetrics;
use infrastructure::providers::volga_wolga::{VolgaConfig, VolgaProvider};
use infrastructure::repositories::postgres::PostgresCruiseRepository;

use crate::config::Config;

pub const HELP_TEXT: &str = "\
Kamtur worker

USAGE:
    worker run                    Run as daemon (scheduler + health + retention + digest)
    worker sync --all             Run sync for all providers once, then exit
    worker sync --provider=<id>   Run sync for a single provider once, then exit
    worker --help

ENVIRONMENT:
    DATABASE_URL                  Postgres connection string (required)
    MAX_CONCURRENCY               Parallel providers (default: 8)
    BATCH_SIZE                    DB batch size (default: 5000)
    RAW_COMPRESSION               zstd | none (default: zstd)
    ORPHAN_SYNC_RUN_STALE_SECS    Orphan running sync_runs threshold (default: 7200)
    ORPHAN_CHECK_INTERVAL_SECS    Orphan cleanup interval (default: 3600)
    METRICS_BIND                  Prometheus bind (default: 0.0.0.0:9090)
    HEALTH_CHECK_INTERVAL_SECS    Health loop interval (default: 60)
    SHUTDOWN_GRACE_PERIOD_SECS    Graceful shutdown timeout (default: 60)

    SMTP_HOST, SMTP_PORT, SMTP_USER, SMTP_PASSWORD, SMTP_FROM, SMTP_TO
                                  If SMTP_HOST empty, NoopNotifier is used

    VOLGA_URL, VOLGA_REQUEST_TIMEOUT_SECS, VOLGA_CONNECT_TIMEOUT_SECS
";

#[derive(Debug)]
pub enum Command {
    Run,
    SyncAll,
    SyncOne(ProviderId),
    Help,
}

impl Command {
    /// Разбирает argv. `--help` и `-h` обрабатываются без ENV и без tracing.
    pub fn parse() -> anyhow::Result<Self> {
        let args: Vec<String> = std::env::args().skip(1).collect();

        match args.first().map(String::as_str) {
            None | Some("run") => {
                if args.len() > 1 {
                    bail!("`run` accepts no arguments");
                }
                Ok(Command::Run)
            }
            Some("--help") | Some("-h") | Some("help") => Ok(Command::Help),
            Some("sync") => {
                let mut all = false;
                let mut provider: Option<String> = None;

                for a in &args[1..] {
                    match a.as_str() {
                        "--all" => all = true,
                        _ if a.starts_with("--provider=") => {
                            let id = a.trim_start_matches("--provider=");
                            if id.is_empty() {
                                bail!("--provider requires a non-empty id");
                            }
                            provider = Some(id.to_string());
                        }
                        other => bail!("unknown flag for `sync`: {other}"),
                    }
                }

                match (all, provider) {
                    (true, None) => Ok(Command::SyncAll),
                    (false, Some(id)) => Ok(Command::SyncOne(ProviderId(id))),
                    (false, None) => bail!("`sync` requires either --all or --provider=<id>"),
                    (true, Some(_)) => bail!("`sync` accepts either --all or --provider, not both"),
                }
            }
            Some(other) => bail!("unknown subcommand: {other}\n\n{HELP_TEXT}"),
        }
    }
}

/// Публичный entrypoint после парсинга.
pub async fn dispatch(cmd: Command, config: Config) -> anyhow::Result<()> {
    match cmd {
        Command::Run => crate::bootstrap::run(config).await,
        Command::SyncAll => run_one_shot(config, SyncTarget::All).await,
        Command::SyncOne(pid) => run_one_shot(config, SyncTarget::One(pid)).await,
        Command::Help => unreachable!("help handled in main"),
    }
}

enum SyncTarget {
    All,
    One(ProviderId),
}

/// One-shot: минимальный контекст, без scheduler/health/retention/digest.
/// Метрики — Noop, потому что процесс короткоживущий и /metrics не отдаёт.
async fn run_one_shot(config: Config, target: SyncTarget) -> anyhow::Result<()> {
    let metrics: Arc<dyn MetricsRecorder> = Arc::new(NoopMetrics);

    let pool = PgPoolOptions::new()
        .max_connections(4)
        .min_connections(1)
        .acquire_timeout(Duration::from_secs(5))
        .connect(&config.database_url)
        .await
        .context("connect postgres")?;

    let repository: Arc<dyn CruiseRepository> = Arc::new(PostgresCruiseRepository::new(
        pool,
        config.compress_raw,
        config.batch_size,
        Arc::clone(&metrics),
    ));

    let volga = VolgaProvider::new(VolgaConfig::from_env(), Arc::clone(&metrics))
        .context("create Volga provider")?;
    let providers: Vec<Arc<dyn CruiseProvider>> = vec![volga as Arc<dyn CruiseProvider>];

    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let sync_uc = SyncAllProvidersUseCase::new(
        providers,
        repository,
        clock,
        metrics,
        config.max_concurrency as usize,
    );

    let report = match target {
        SyncTarget::All => sync_uc.execute().await,
        SyncTarget::One(pid) => sync_uc.execute_provider(&pid).await,
    };

    info!(
        ok = report.succeeded.len(),
        failed = report.failed.len(),
        elapsed_ms = report.duration.as_millis() as u64,
        "one-shot sync complete"
    );

    if !report.failed.is_empty() {
        for (id, err) in &report.failed {
            tracing::error!(provider = %id.0, error = %err, "sync failed");
        }
        bail!(
            "one-shot sync had failures ({} provider(s))",
            report.failed.len()
        );
    }

    Ok(())
}
