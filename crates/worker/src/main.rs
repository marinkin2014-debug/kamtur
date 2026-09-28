mod bootstrap;
mod cli;
mod config;
mod digest;
mod metrics_server;
mod retention;
mod scheduler;

use anyhow::Context;
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    // CLI парсим ДО config: `--help` не должен требовать DATABASE_URL.
    let cmd = match cli::Command::parse() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(2);
        }
    };

    if matches!(cmd, cli::Command::Help) {
        print!("{}", cli::HELP_TEXT);
        return Ok(());
    }

    let config = config::Config::from_env().context("load config")?;

    // Tracing — только для рабочих режимов.
    let file_appender = tracing_appender::rolling::daily("logs", "worker.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,sqlx=warn".into()))
        .with(fmt::layer().with_writer(std::io::stdout))
        .with(
            fmt::layer()
                .with_writer(non_blocking)
                .with_ansi(false)
                .with_target(true),
        )
        .init();

    cli::dispatch(cmd, config).await
}
