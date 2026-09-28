use std::time::Duration;

use anyhow::Context;

pub struct Config {
    pub database_url: String,
    pub max_concurrency: u32,
    pub batch_size: usize,
    pub compress_raw: bool,

    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_user: String,
    pub smtp_password: String,
    pub smtp_from: String,
    pub smtp_to: Vec<String>,

    pub health_check_interval: Duration,
    pub shutdown_grace_period: Duration,
    pub metrics_bind: String,

    /// Порог «осиротевших» sync_runs: `running` старше этого количества секунд
    /// будет помечен `failed`. Должен быть БОЛЬШЕ самого долгого sync'а —
    /// у Volga timeout 15 минут, ставим с большим запасом.
    pub orphan_sync_run_stale_secs: i64,

    /// Интервал проверки осиротевших sync_runs.
    pub orphan_check_interval: Duration,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        Ok(Self {
            database_url: std::env::var("DATABASE_URL").context("DATABASE_URL")?,
            max_concurrency: std::env::var("MAX_CONCURRENCY")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(8),
            batch_size: std::env::var("BATCH_SIZE")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(5000),
            compress_raw: std::env::var("RAW_COMPRESSION")
                .map(|v| v.eq_ignore_ascii_case("zstd"))
                .unwrap_or(true),

            smtp_host: std::env::var("SMTP_HOST").unwrap_or_default(),
            smtp_port: std::env::var("SMTP_PORT")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(587),
            smtp_user: std::env::var("SMTP_USER").unwrap_or_default(),
            smtp_password: std::env::var("SMTP_PASSWORD").unwrap_or_default(),
            smtp_from: std::env::var("SMTP_FROM").unwrap_or_default(),
            smtp_to: std::env::var("SMTP_TO")
                .map(|s| s.split(',').map(|x| x.trim().to_string()).collect())
                .unwrap_or_default(),

            health_check_interval: Duration::from_secs(
                std::env::var("HEALTH_CHECK_INTERVAL_SECS")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(60),
            ),
            shutdown_grace_period: Duration::from_secs(
                std::env::var("SHUTDOWN_GRACE_PERIOD_SECS")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(60),
            ),
            metrics_bind: std::env::var("METRICS_BIND").unwrap_or_else(|_| "0.0.0.0:9090".into()),

            orphan_sync_run_stale_secs: std::env::var("ORPHAN_SYNC_RUN_STALE_SECS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(7200), // 2 часа

            orphan_check_interval: Duration::from_secs(
                std::env::var("ORPHAN_CHECK_INTERVAL_SECS")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(3600), // 1 час
            ),
        })
    }
}
