use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;

use domain::entities::{CanonicalData, ProviderId};
use domain::errors::ProviderError;
use domain::ports::{CruiseProvider, MetricsRecorder};

use super::{VolgaCanonicalTransformer, VolgaConfig, VolgaFetcher, VolgaParser};
use crate::circuit_breaker::CircuitBreaker;

pub struct VolgaProvider {
    id: ProviderId,
    cruise_type_id: i64,
    fetcher: VolgaFetcher,
    parser: VolgaParser,
    transformer: VolgaCanonicalTransformer,
    breaker: CircuitBreaker,
}

impl VolgaProvider {
    /// Создаёт провайдера. Возвращает `Err(ProviderError::Config)`, если
    /// не удалось построить HTTP-клиент (пробрасывается из `VolgaFetcher::new`).
    ///
    /// Вызывается на bootstrap воркера — ошибка должна валить процесс,
    /// а не молча деградировать в runtime.
    pub fn new(
        config: VolgaConfig,
        metrics: Arc<dyn MetricsRecorder>,
    ) -> Result<Arc<Self>, ProviderError> {
        // provider_id нужен в двух местах: в ProviderId и в CircuitBreaker.
        // Клонируем один раз, чтобы `config` можно было частично move'нуть.
        let provider_id = config.provider_id.clone();
        let cruise_type_id = config.cruise_type_id;

        let fetcher =
            VolgaFetcher::new(config.url, config.request_timeout, config.connect_timeout)?;

        Ok(Arc::new(Self {
            id: ProviderId(provider_id.clone()),
            cruise_type_id,
            fetcher,
            parser: VolgaParser,
            transformer: VolgaCanonicalTransformer::new(cruise_type_id),
            // threshold 10 подряд идущих ошибок, восстановление через 30 минут.
            // Долгие запросы (до 15 минут) — норма, поэтому допускаем редкие сбои.
            breaker: CircuitBreaker::new(10, 2, Duration::from_secs(1800), provider_id, metrics),
        }))
    }
}

#[async_trait]
impl CruiseProvider for VolgaProvider {
    fn id(&self) -> &ProviderId {
        &self.id
    }

    fn cruise_type_id(&self) -> i64 {
        self.cruise_type_id
    }

    async fn fetch_raw(&self) -> Result<Bytes, ProviderError> {
        let fetcher = self.fetcher.clone();
        self.breaker
            .call(move || async move { fetcher.fetch_raw().await })
            .await
    }

    fn parse(&self, raw: &[u8]) -> Result<CanonicalData, ProviderError> {
        let raw_data = self.parser.parse(raw)?;
        self.transformer.transform(raw_data)
    }
}
