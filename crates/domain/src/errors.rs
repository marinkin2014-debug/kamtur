use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProviderError {
    /// Runtime-ошибка сетевого взаимодействия: timeout, connect failed,
    /// http status, body read failed. Transient — обычно retry помогает.
    #[error("network: {0}")]
    Network(String),

    /// Ошибка разбора ответа провайдера (XML, JSON, формат даты).
    /// Permanent — retry не поможет, нужен фикс парсера.
    #[error("parse: {0}")]
    Parse(String),

    /// Ошибка конфигурации провайдера: не удалось построить HTTP-клиент,
    /// некорректный URL, отсутствующий TLS-backend в сборке.
    /// Permanent — fail-fast на старте, retry бессмысленен.
    #[error("config: {0}")]
    Config(String),

    /// Circuit breaker разомкнут, вызов не выполнялся.
    /// Transient — сработает после остывания.
    #[error("circuit open")]
    CircuitOpen,
}

#[derive(Debug, Error)]
pub enum RepositoryError {
    #[error("connection: {0}")]
    Connection(String),
    #[error("transaction: {0}")]
    Transaction(String),
}

#[derive(Debug, Error)]
pub enum NotifyError {
    #[error("smtp: {0}")]
    Smtp(String),
}

#[derive(Debug, Error)]
pub enum ReadError {
    #[error("connection: {0}")]
    Connection(String),
    #[error("query: {0}")]
    Query(String),
}
