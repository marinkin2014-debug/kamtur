use async_trait::async_trait;

use crate::errors::NotifyError;

#[async_trait]
pub trait Notifier: Send + Sync {
    async fn send(&self, severity: &str, subject: &str, body: &str) -> Result<(), NotifyError>;
}
