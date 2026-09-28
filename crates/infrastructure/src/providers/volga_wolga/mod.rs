mod canonical;
mod config;
mod fetcher;
mod parser;
mod provider;
mod raw;

pub use canonical::VolgaCanonicalTransformer;
pub use config::VolgaConfig;
pub use fetcher::VolgaFetcher;
pub use parser::VolgaParser;
pub use provider::VolgaProvider;

// ============================================================
// Экспорт для бенчмарков
// ============================================================
//
// `RawData` — внутренний DTO между `VolgaParser::parse` и
// `VolgaCanonicalTransformer::transform`. Снаружи модуля он не нужен
// (пользователь работает с `VolgaProvider::parse`, который делает оба
// шага), но `benches/` — это **отдельный крейт**, и он не может
// назвать тип, если тот не `pub`.
//
// Экспортируем намеренно, чтобы можно было бенчмаркать transform
// изолированно от парсинга. В production-коде используем через
// `VolgaProvider`, не через `RawData` напрямую.
pub use raw::RawData;
