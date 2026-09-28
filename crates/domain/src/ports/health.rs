use async_trait::async_trait;

use crate::errors::RepositoryError;

#[async_trait]
pub trait HealthCheck: Send + Sync {
    fn component(&self) -> &str;
    async fn check(&self) -> HealthStatus;
}

#[derive(Debug, Clone)]
pub struct HealthStatus {
    pub status: String,
    pub latency_ms: Option<i32>,
    pub details: serde_json::Value,
}

#[async_trait]
pub trait HealthRepository: Send + Sync {
    async fn record_health_check(
        &self,
        component: &str,
        status: &str,
        latency_ms: Option<i32>,
        details: serde_json::Value,
    ) -> Result<(), RepositoryError>;
}
