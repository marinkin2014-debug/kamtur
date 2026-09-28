# Как добавить нового провайдера

Пошаговая инструкция на примере Volga. Для нового провайдера
(например, Vodohod) — те же шаги.

## Что такое провайдер

Крейт `infrastructure::providers::<name>` содержит:

- **fetcher** — HTTP-запросы (`reqwest`)
- **parser** — XML/JSON → `RawData` (`quick-xml`, `serde`)
- **canonical** — `RawData` → `CanonicalData` (domain)
- **provider** — реализация трейта `CruiseProvider`
- **config** — настройки из env
- **raw** — структуры сырых данных

## Шаги

### 1. Создать структуру модуля

```
crates/infrastructure/src/providers/vodohod/
├── mod.rs
├── config.rs
├── fetcher.rs
├── parser.rs
├── canonical.rs
├── provider.rs
└── raw.rs
```

`crates/infrastructure/src/providers/mod.rs`:

```rust
pub mod volga_wolga;
pub mod vodohod;   // новый
```

### 2. Определить `RawData`

Структуры, куда парсер складывает данные. Один-в-один с XML/JSON
провайдера, без логики.

`raw.rs`:

```rust
use rust_decimal::Decimal;

#[derive(Debug, Default)]
pub struct RawData {
    pub ships: Vec<RawShip>,
    pub cruises: Vec<RawCruise>,
    // ...
}

#[derive(Debug)]
pub struct RawShip {
    pub id: String,
    pub name: String,
}

#[derive(Debug)]
pub struct RawCruise {
    pub id: String,
    pub ship_id: String,
    pub begin_date: String,   // как в XML
    pub end_date: String,
    // ...
}
```

### 3. Написать parser

`parser.rs`:

```rust
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use tracing::warn;

use domain::errors::ProviderError;
use super::raw::*;

pub struct VodohodParser;

impl VodohodParser {
    pub fn parse(&self, bytes: &[u8]) -> Result<RawData, ProviderError> {
        let mut raw = RawData::default();
        let mut reader = Reader::from_reader(bytes);
        // ...
        Ok(raw)
    }
}
```

Правила:
- Терпимость к неизвестным тегам: не падать, игнорировать.
- Ошибки формата (невалидный Decimal) — `warn!` + пропуск, не падать.
- Ошибка XML — `ProviderError::Parse`.
- Писать тесты на фикстуре.

### 4. Написать canonical transformer

`canonical.rs`:

```rust
use domain::entities::*;
use domain::errors::ProviderError;
use super::raw::*;

pub struct VodohodCanonicalTransformer {
    pub cruise_type_id: i64,
}

impl VodohodCanonicalTransformer {
    pub fn transform(&self, raw: RawData) -> Result<CanonicalData, ProviderError> {
        let mut data = CanonicalData::default();
        // raw → domain
        Ok(data)
    }
}
```

Отображение на доменные сущности:
- `RawShip` → `CanonicalObject`
- `RawDeck` → `CanonicalStage`
- `RawClass` → `CanonicalClass`
- `RawCabin` → `CanonicalRoom`
- `RawCruise` → `CanonicalTour`
- `RawPrice` → `CanonicalPrice`
- `RawSpo` → `CanonicalSale`
- `RawFree` → `CanonicalAvailability`

`external_id` — как провайдер назвал. Никаких префиксов, чтобы не
ломать FK.

### 5. Написать fetcher

`fetcher.rs`:

```rust
use std::time::Duration;
use bytes::Bytes;
use reqwest::Client;

use domain::errors::ProviderError;

pub struct VodohodFetcher {
    client: Client,
    url: String,
}

impl VodohodFetcher {
    pub fn new(url: impl Into<String>, timeout: Duration, connect_timeout: Duration) -> Self {
        let client = Client::builder()
            .timeout(timeout)
            .connect_timeout(connect_timeout)
            .build()
            .expect("reqwest client");
        Self { client, url: url.into() }
    }

    pub async fn fetch_raw(&self) -> Result<Bytes, ProviderError> {
        // ...
    }
}
```

Не забывайте: **никогда не паникуйте в production-path**. `expect` только
при конфигурации клиента (это compile-time в основном).

### 6. Реализовать `CruiseProvider`

`provider.rs`:

```rust
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;

use domain::entities::{CanonicalData, ProviderId};
use domain::errors::ProviderError;
use domain::ports::CruiseProvider;

use crate::circuit_breaker::CircuitBreaker;
use super::{VodohodCanonicalTransformer, VodohodConfig, VodohodFetcher, VodohodParser};

pub struct VodohodProvider {
    id: ProviderId,
    cruise_type_id: i64,
    fetcher: VodohodFetcher,
    parser: VodohodParser,
    transformer: VodohodCanonicalTransformer,
    breaker: CircuitBreaker,
}

impl VodohodProvider {
    pub fn new(config: VodohodConfig) -> Arc<Self> {
        Arc::new(Self {
            id: ProviderId(config.provider_id),
            cruise_type_id: config.cruise_type_id,
            fetcher: VodohodFetcher::new(config.url, config.request_timeout, config.connect_timeout),
            parser: VodohodParser,
            transformer: VodohodCanonicalTransformer::new(config.cruise_type_id),
            breaker: CircuitBreaker::new(10, 2, Duration::from_secs(1800)),
        })
    }
}

#[async_trait]
impl CruiseProvider for VodohodProvider {
    fn id(&self) -> &ProviderId { &self.id }
    fn cruise_type_id(&self) -> i64 { self.cruise_type_id }

    async fn fetch_raw(&self) -> Result<Bytes, ProviderError> {
        let fetcher = self.fetcher.clone();
        self.breaker.call(move || async move { fetcher.fetch_raw().await }).await
    }

    fn parse(&self, raw: &[u8]) -> Result<CanonicalData, ProviderError> {
        let raw_data = self.parser.parse(raw)?;
        self.transformer.transform(raw_data)
    }
}
```

### 7. Config

`config.rs`:

```rust
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct VodohodConfig {
    pub url: String,
    pub provider_id: String,
    pub cruise_type_id: i64,
    pub request_timeout: Duration,
    pub connect_timeout: Duration,
}

impl VodohodConfig {
    pub fn from_env() -> Self {
        Self {
            url: std::env::var("VODOHOD_URL")
                .unwrap_or_else(|_| "https://vodohod.com/api/...".into()),
            provider_id: "2".into(),   // новый ID
            cruise_type_id: 2,
            request_timeout: Duration::from_secs(900),
            connect_timeout: Duration::from_secs(60),
        }
    }
}
```

**Важно:** `provider_id` уникален в БД. Если Volga = `"1"`, новый = `"2"`.

### 8. Регистрация в worker

`crates/worker/src/bootstrap.rs`:

```rust
use infrastructure::providers::vodohod::{VodohodConfig, VodohodProvider};

// ...

let volga_config = VolgaConfig::from_env();
let vodohod_config = VodohodConfig::from_env();

let providers: Vec<Arc<dyn CruiseProvider>> = vec![
    VolgaProvider::new(volga_config) as Arc<dyn CruiseProvider>,
    VodohodProvider::new(vodohod_config) as Arc<dyn CruiseProvider>,
];

for p in &providers {
    PrometheusMetrics::init_zeros(&p.id().0);
}
```

### 9. Schedules

`crates/worker/src/bootstrap.rs`:

```rust
let schedules = vec![
    ProviderSchedule {
        provider_id: "1".into(),
        cron: "0 0 * * * *".into(),
        run_on_start: true,
    },
    ProviderSchedule {
        provider_id: "2".into(),
        cron: "0 30 * * * *".into(),   // в 30 минут, чтобы не пересекаться
        run_on_start: false,
    },
];
```

`run_on_start: false` — не запускать sync при рестарте worker'а, только
по cron. Полезно, если новый провайдер тяжёлый.

### 10. Health checks

Для каждого провайдера — свой freshness check:

```rust
let mut health_checks: Vec<Arc<dyn HealthCheck>> = vec![
    Arc::new(DatabaseHealthCheck::new(pool.clone())),
    Arc::new(SyncPipelineFreshnessCheck::new(pool.clone(), "1", Duration::from_secs(3600))),
    Arc::new(SyncPipelineFreshnessCheck::new(pool.clone(), "2", Duration::from_secs(7200))), // Vodohod медленнее
    Arc::new(ContentFreshnessCheck::new(pool.clone(), "1", Duration::from_secs(86400))),
    Arc::new(ContentFreshnessCheck::new(pool.clone(), "2", Duration::from_secs(86400))),
];
```

### 11. Правила обогащения

Если у провайдера есть уникальные поля (например, `site_name` = "Водоход"),
добавить в БД:

```sql
INSERT INTO enrichment_rules
    (cruise_provider_id, field_name, rule_type, rule_config, priority, is_active)
VALUES
    ('2', 'site_name', 'constant', '{"value": "Водоход"}', 100, true),
    ('2', 'route', 'template', '{"template": "{name} (Водоход)"}', 100, true);
```

Применятся при следующем sync.

### 12. API

Если новый провайдер должен быть доступен через API, обновить
`DEFAULT_PROVIDER_ID` или передавать `?cruise_provider_id=2` в запросе.

`crates/api/src/routes/cruises.rs`:

```rust
const DEFAULT_PROVIDER_ID: &str = "1";
```

`?cruise_provider_id=2` уже поддерживается через query param.

### 13. Тесты

Обязательно:

**Parser** (`parser.rs`):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &[u8] = br#"<?xml version="1.0"?>...<your xml>..."#;

    #[test]
    fn parses_ships() { /* ... */ }
    #[test]
    fn parses_cruises() { /* ... */ }
    // ... как для Volga
}
```

**E2E** (`crates/infrastructure/tests/e2e.rs`):

Добавить второй провайдер через параметризованный хелпер:

```rust
async fn run_pipeline_vodohod(
    repo: &PostgresCruiseRepository,
    provider_id: &str,
    xml: &[u8],
) -> SyncOutcome {
    let parser = VodohodParser;
    let transformer = VodohodCanonicalTransformer::new(2);
    // ... то же, что для Volga
}
```

**Опционально:** общий `test_support` для параметризации по провайдеру.

### 14. Проверка end-to-end

```bash
# Пересобрать
cargo build --release --workspace

# Ручной sync нового провайдера
cargo run --release --bin worker -- sync --provider=2

# Проверить в БД
psql "$DATABASE_URL" -c "SELECT count(*) FROM cruise_provider_tours WHERE cruise_provider_id = '2';"

# API
curl.exe -H "Authorization: Bearer $API_TOKEN" \
     "http://127.0.0.1:8080/cruises?cruise_provider_id=2&limit=5"
```

### 15. Деплой

1. Обновить env: `VODOHOD_URL`, `VODOHOD_REQUEST_TIMEOUT_SECS`.
2. Обновить конфиги мониторинга: добавить `provider="2"` в dashboards.
3. Обновить runbook: если у нового провайдера другие SLA.
4. Рестарт worker.
5. Смотреть первые 3-5 циклов.

## Checklist

- [ ] Модуль создан в `infrastructure/providers/vodohod/`
- [ ] `RawData` соответствует XML
- [ ] Parser покрыт тестами (min 5-7 тестов)
- [ ] Canonical transformer
- [ ] Fetcher без `expect` в runtime
- [ ] `CruiseProvider` реализован
- [ ] `Config::from_env`
- [ ] Зарегистрирован в `worker/bootstrap.rs`
- [ ] Добавлен в `schedules`
- [ ] Freshness checks добавлены
- [ ] Enrichment rules в БД
- [ ] E2E тест на фикстуре
- [ ] `cargo test --workspace` зелёный
- [ ] `cargo clippy -- -D warnings` зелёный
- [ ] `cargo fmt --all -- --check` чистый
- [ ] Env vars в деплое
- [ ] Метрики видны на `/metrics`
- [ ] Первый sync прошёл
- [ ] Runbook обновлён

## Частые ошибки

### `relation "cruise_providers" does not exist`

Не применены миграции. При запуске `worker run` они применяются
автоматически. Для тестов — `test_support::pool()` прогоняет миграции
при первом вызове.

### `foreign key constraint violation`

`external_object_id` в `CanonicalClass` должен ссылаться на существующий
`external_id` в `CanonicalObject`. Проверьте порядок в transformer:
объекты (ships) → стадии (decks) → классы → комнаты → туры → цены.

### Sync возвращает 0 объектов, но XML непустой

Парсер не находит ожидаемые теги. Проверьте:
- Namespace XML (`<ns:ship>` vs `<ship>`)
- Регистр (`<Cruise>` vs `<cruise>`)
- Вложенность (`<free><cruise id="1"/></free>`)

Написать unit-тест на реальном фрагменте XML.

### `raw_snapshots` взрывается по размеру

Если провайдер отдаёт огромный XML (> 100 MB), `zstd` уровнем 1
сжимает до 10-20%. Если всё равно много:
- Увеличить `RAW_COMPRESSION` (пока не поддерживается)
- Уменьшить retention в `retention.rs` (`RAW_SNAPSHOTS_DAYS`)

### Circuit breaker не пускает sync

Посмотреть:
```sql
SELECT * FROM errors WHERE stage = 'fetch' ORDER BY occurred_at DESC LIMIT 10;
```

Или в логах: `grep "circuit open" logs/worker.log.*`.

Подождать 30 минут (timeout) или рестарт worker'а (state сбросится).
