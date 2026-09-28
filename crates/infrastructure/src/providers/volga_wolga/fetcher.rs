use std::time::Duration;

use bytes::Bytes;
use reqwest::Client;
use tracing::{debug, info};

use domain::errors::ProviderError;

#[derive(Clone)]
pub struct VolgaFetcher {
    client: Client,
    url: String,
    request_timeout: Duration,
}

impl VolgaFetcher {
    /// Создаёт fetcher. Возвращает `Err`, если не удалось построить
    /// `reqwest::Client` — это ошибка конфигурации, не runtime.
    ///
    /// Причины отказа `Client::builder().build()`:
    ///   - несовместимая фича TLS в сборке (`rustls` не включён);
    ///   - некорректные параметры TLS;
    ///   - сбой при инициализации системного TLS-контекста.
    ///
    /// Все они — permanent. `ProviderError::Config` явно маркирует их
    /// как «не retry», в отличие от `ProviderError::Network`.
    pub fn new(
        url: impl Into<String>,
        request_timeout: Duration,
        connect_timeout: Duration,
    ) -> Result<Self, ProviderError> {
        let client = Client::builder()
            // Общий timeout на весь запрос (включая чтение тела).
            .timeout(request_timeout)
            // Timeout только на TCP-connect.
            .connect_timeout(connect_timeout)
            .pool_max_idle_per_host(4)
            .tcp_keepalive(Duration::from_secs(60))
            .build()
            .map_err(|e| ProviderError::Config(format!("build reqwest client: {e}")))?;

        Ok(Self {
            client,
            url: url.into(),
            request_timeout,
        })
    }

    pub async fn fetch_raw(&self) -> Result<Bytes, ProviderError> {
        let start = std::time::Instant::now();
        info!(
            url = %self.url,
            timeout_secs = self.request_timeout.as_secs(),
            "fetching from provider (may take up to 15 minutes)"
        );

        let response = self.client.get(&self.url).send().await.map_err(|e| {
            let elapsed = start.elapsed();
            if e.is_timeout() {
                ProviderError::Network(format!(
                    "timeout after {}s (limit {}s)",
                    elapsed.as_secs(),
                    self.request_timeout.as_secs()
                ))
            } else if e.is_connect() {
                ProviderError::Network(format!("connect failed: {e}"))
            } else {
                ProviderError::Network(format!("http: {e}"))
            }
        })?;

        let status = response.status();
        if !status.is_success() {
            return Err(ProviderError::Network(format!("http status: {status}")));
        }

        let bytes = response
            .bytes()
            .await
            .map_err(|e| ProviderError::Network(format!("body: {e}")))?;

        debug!(
            elapsed_secs = start.elapsed().as_secs(),
            bytes = bytes.len(),
            "fetch complete"
        );

        Ok(bytes)
    }
}
