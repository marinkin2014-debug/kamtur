use std::time::Duration;

#[derive(Debug, Clone)]
pub struct VolgaConfig {
    pub url: String,
    pub provider_id: String,
    pub provider_name: String,
    pub cruise_type_id: i64,
    pub request_timeout: Duration,
    pub connect_timeout: Duration,
}

impl VolgaConfig {
    pub fn from_env() -> Self {
        Self {
            url: std::env::var("VOLGA_URL").unwrap_or_else(|_| {
                "http://test.volgaural.ru/php/xml/2023/index-kamtur.php".into()
            }),
            provider_id: "1".into(),
            provider_name: "Volga Wolga".into(),
            cruise_type_id: 1,
            request_timeout: Duration::from_secs(
                std::env::var("VOLGA_REQUEST_TIMEOUT_SECS")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(900), // 15 минут
            ),
            connect_timeout: Duration::from_secs(
                std::env::var("VOLGA_CONNECT_TIMEOUT_SECS")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(60), // 1 минута
            ),
        }
    }
}
