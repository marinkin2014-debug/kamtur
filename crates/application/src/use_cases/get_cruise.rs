use std::sync::Arc;

use domain::errors::ReadError;
use domain::ports::CruiseReadRepository;
use domain::views::CruiseDetail;

pub struct GetCruiseUseCase {
    repository: Arc<dyn CruiseReadRepository>,
}

impl GetCruiseUseCase {
    pub fn new(repository: Arc<dyn CruiseReadRepository>) -> Self {
        Self { repository }
    }

    pub async fn execute(
        &self,
        cruise_provider_id: &str,
        cruise_provider_cruise_id: &str,
    ) -> Result<Option<CruiseDetail>, ReadError> {
        self.repository
            .get_cruise(cruise_provider_id, cruise_provider_cruise_id)
            .await
    }
}
