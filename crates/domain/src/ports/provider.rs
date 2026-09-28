use async_trait::async_trait;
use bytes::Bytes;

use crate::entities::{CanonicalData, ProviderId};
use crate::errors::ProviderError;

#[async_trait]
pub trait CruiseProvider: Send + Sync {
    fn id(&self) -> &ProviderId;
    fn cruise_type_id(&self) -> i64;

    /// Опциональная строка конфига для правил обогащения.
    /// Default — `None`. Провайдеры могут переопределить.
    fn provider_config(&self) -> Option<&str> {
        None
    }

    async fn fetch_raw(&self) -> Result<Bytes, ProviderError>;
    fn parse(&self, raw: &[u8]) -> Result<CanonicalData, ProviderError>;
}
