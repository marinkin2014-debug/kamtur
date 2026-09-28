use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use metrics_exporter_prometheus::PrometheusBuilder;
use sqlx::postgres::PgPoolOptions;
use tokio::signal;
use tokio_util::sync::CancellationToken;
use tracing::info;

use application::use_cases::sync_all_providers::SyncAllProvidersUseCase;
use domain::ports::{
    Clock, CruiseProvider, CruiseRepository, HealthCheck, MetricsRecorder, Notifier,
};
use infrastructure::clock::SystemClock;
use infrastructure::health::{
    ContentFreshnessCheck, DatabaseHealthCheck, SmtpHealthCheck, SyncPipelineFreshnessCheck,
};
use infrastructure::metrics::PrometheusMetrics;
use infrastructure::notifier::{NoopNotifier, SmtpNotifier};
use infrastructure::providers::volga_wolga::{VolgaConfig, VolgaProvider};
use infrastructure::repositories::postgres::PostgresCruiseRepository;

use crate::config::Config;
use crate::digest::run_digest_loop;
use crate::metrics_server::run_metrics_server;
use crate::retention::run_retention_loop;
use crate::scheduler::{health_loop, ProviderSchedule, Scheduler};

pub async fn run(config: Config) -> anyhow::Result<()> {
    // ---------- Metrics: установка глобального recorder'а ----------
    let metrics_bind: std::net::SocketAddr = config
        .metrics_bind
        .parse()
        .with_context(|| format!("parse METRICS_BIND={}", config.metrics_bind))?;

    let prometheus_handle = PrometheusBuilder::new()
        .set_buckets_for_metric(
            metrics_exporter_prometheus::Matcher::Full(
                "kamtur_sync_phase_duration_seconds".to_string(),
            ),
            &[
                0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0, 120.0,
                300.0,
            ],
        )
        .context("set prometheus buckets")?
        .install_recorder()
        .context("install prometheus recorder")?;

    let process_collector = metrics_process::Collector::default();
    process_collector.describe();
    process_collector.collect();

    let metrics: Arc<dyn MetricsRecorder> = Arc::new(PrometheusMetrics::new());
    info!(addr = %metrics_bind, "prometheus recorder installed");

    // ---------- Pool ----------
    let pool = PgPoolOptions::new()
        .max_connections(config.max_concurrency + 4)
        .min_connections(2)
        .acquire_timeout(Duration::from_secs(5))
        .idle_timeout(Duration::from_secs(600))
        .connect(&config.database_url)
        .await
        .context("connect postgres")?;

    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .context("run migrations")?;

    // ---------- Notifier (для digest loop) ----------
    let notifier: Arc<dyn Notifier> = if config.smtp_host.is_empty() {
        info!("SMTP not configured, using NoopNotifier");
        Arc::new(NoopNotifier)
    } else {
        Arc::new(SmtpNotifier::new(
            &config.smtp_host,
            config.smtp_port,
            &config.smtp_user,
            &config.smtp_password,
            config.smtp_from.clone(),
            config.smtp_to.clone(),
        )?)
    };

    // ---------- Repository ----------
    let repository: Arc<dyn CruiseRepository> = Arc::new(PostgresCruiseRepository::new(
        pool.clone(),
        config.compress_raw,
        config.batch_size,
        Arc::clone(&metrics),
    ));

    // ---------- Providers ----------
    let volga_config = VolgaConfig::from_env();
    let volga =
        VolgaProvider::new(volga_config, Arc::clone(&metrics)).context("create Volga provider")?;
    let providers: Vec<Arc<dyn CruiseProvider>> = vec![volga as Arc<dyn CruiseProvider>];

    // Инициализируем нулевые счётчики ДЛЯ КАЖДОГО реального провайдера.
    for p in &providers {
        PrometheusMetrics::init_zeros(&p.id().0);
    }

    // ---------- Use case ----------
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let sync_uc = Arc::new(SyncAllProvidersUseCase::new(
        providers,
        Arc::clone(&repository),
        clock,
        Arc::clone(&metrics),
        config.max_concurrency as usize,
    ));

    // ---------- Schedules ----------
    let schedules = vec![ProviderSchedule {
        provider_id: "1".into(),
        cron: "0 0 * * * *".into(),
        run_on_start: true,
    }];

    // ---------- Health checks ----------
    let mut health_checks: Vec<Arc<dyn HealthCheck>> = vec![
        Arc::new(DatabaseHealthCheck::new(pool.clone())),
        Arc::new(SyncPipelineFreshnessCheck::new(
            pool.clone(),
            "1",
            Duration::from_secs(3600),
        )),
        Arc::new(ContentFreshnessCheck::new(
            pool.clone(),
            "1",
            Duration::from_secs(86400),
        )),
    ];

    if !config.smtp_host.is_empty() {
        // Полноценный SMTP handshake вместо TCP-connect: см.
        // `SmtpHealthCheck` docstring. Падаем при ошибке конфигурации
        // на bootstrap — лучше узнать сейчас, чем при первой отправке
        // алерта.
        health_checks.push(Arc::new(SmtpHealthCheck::new(
            &config.smtp_host,
            config.smtp_port,
            &config.smtp_user,
            &config.smtp_password,
        )?));
    }

    // ---------- Cancellation ----------
    let cancel = CancellationToken::new();

    // ---------- Background tasks ----------
    let scheduler_handle =
        tokio::spawn(Scheduler::new(schedules, Arc::clone(&sync_uc), cancel.clone()).run());

    let health_handle = tokio::spawn(health_loop(
        health_checks,
        Arc::clone(&repository),
        config.health_check_interval,
        cancel.clone(),
    ));

    let retention_handle = tokio::spawn(run_retention_loop(
        Arc::clone(&repository),
        Arc::clone(&metrics),
        pool.clone(),
        config.orphan_sync_run_stale_secs,
        config.orphan_check_interval,
        cancel.clone(),
    ));

    let digest_handle = tokio::spawn(run_digest_loop(
        Arc::clone(&repository),
        Arc::clone(&notifier),
        Duration::from_secs(300),
        cancel.clone(),
    ));

    let metrics_handle = tokio::spawn(run_metrics_server(
        metrics_bind,
        prometheus_handle,
        cancel.clone(),
    ));

    // ---------- DB pool metrics ----------
    let pool_metrics_handle = {
        let pool = pool.clone();
        let metrics = Arc::clone(&metrics);
        let cancel = cancel.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(30));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = ticker.tick() => {
                        let size = pool.size();
                        let idle = pool.num_idle() as u32;
                        metrics.set_db_pool_size(size);
                        metrics.set_db_pool_available(idle);
                    }
                    _ = cancel.cancelled() => return,
                }
            }
        })
    };

    // ---------- Process metrics ----------
    let process_metrics_handle = {
        let cancel = cancel.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(15));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = ticker.tick() => process_collector.collect(),
                    _ = cancel.cancelled() => return,
                }
            }
        })
    };

    info!("worker started");

    // ---------- Graceful shutdown ----------
    signal::ctrl_c().await?;
    info!("shutdown signal received, cancelling tasks");
    cancel.cancel();

    let _ = tokio::time::timeout(config.shutdown_grace_period, async {
        let _ = scheduler_handle.await;
        let _ = health_handle.await;
        let _ = retention_handle.await;
        let _ = digest_handle.await;
        let _ = metrics_handle.await;
        let _ = pool_metrics_handle.await;
        let _ = process_metrics_handle.await;
    })
    .await;

    info!("shutdown complete");
    Ok(())
}
