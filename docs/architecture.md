# Архитектура

## Обзор

```
┌─────────────────────────────────────────────────────────────┐
│                       External providers                     │
│              (Volga, future: Vodohod, Infoflot)              │
└──────────────────────────┬──────────────────────────────────┘
                           │ HTTP/XML
                           ▼
┌─────────────────────────────────────────────────────────────┐
│                         worker (daemon)                      │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────────┐   │
│  │  Scheduler   │  │   Digest     │  │    Retention     │   │
│  │  (cron)      │  │ (2-phase)    │  │  (raw_snapshots, │   │
│  │              │  │              │  │   health, errors)│   │
│  └──────┬───────┘  └──────┬───────┘  └────────┬─────────┘   │
│         │                 │                   │              │
│         ▼                 ▼                   ▼              │
│  ┌──────────────────────────────────────────────────────┐   │
│  │              SyncAllProvidersUseCase                  │   │
│  │      fetch → hash → dedup → parse → enrich → persist  │   │
│  └───────────────────────────┬──────────────────────────┘   │
└──────────────────────────────┼───────────────────────────────┘
                               │
                               ▼
                     ┌──────────────────┐
                     │    Postgres      │
                     │  (нормализованные│
                     │     данные)      │
                     └─────────┬────────┘
                               │
                               ▼
┌─────────────────────────────────────────────────────────────┐
│                            api                               │
│   ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌──────────────┐  │
│   │ auth     │ │rate_limit│ │  cache   │ │ request_id   │  │
│   └──────────┘ └──────────┘ └──────────┘ └──────────────┘  │
│                                                              │
│   /cruises         → ListCruisesUseCase                     │
│   /cruises/:id     → GetCruiseUseCase                       │
│   /health          → ok                                     │
│   /metrics         → Prometheus                             │
└─────────────────────────────────────────────────────────────┘
```

## Слои

### `domain`

Чистый слой. Не зависит ни от чего, кроме базовых крейтов
(`chrono`, `rust_decimal`, `serde`, `sha2`).

Содержит:
- **entities** — `CanonicalObject`, `CanonicalTour`, `SyncOutcome`, ...
- **views** — read-модели: `CruiseListItem`, `CruiseDetail`
- **errors** — `ProviderError`, `RepositoryError`, `ReadError`
- **ports** — трейты: `CruiseProvider`, `CruiseRepository`, `CruiseReadRepository`,
  `Notifier`, `Clock`, `MetricsRecorder`, `HealthCheck`, ...
- **rules** — `EnrichmentRule`, `RuleType`, `RuleContext`
- **fingerprint** — SHA256-хеш для дедупликации

Ключевое правило: **domain не знает про Postgres, HTTP, SMTP**. Всё, что
«наружу», — через порт.

### `application`

Use cases. Координируют domain + порты. Не знают про конкретные адаптеры.

- `ListCruisesUseCase` — clamp limit/offset, вызов репозитория
- `GetCruiseUseCase` — обёртка над `get_cruise`
- `SyncAllProvidersUseCase` — параллельный sync провайдеров через `JoinSet`
  + `Semaphore`; `execute_provider(id)` — один провайдер (для scheduler)
- `sync_one` — полный цикл одного провайдера (см. Pipeline ниже)
- `enrich_tours` — применение правил обогащения

### `infrastructure`

Адаптеры. Знает про Postgres, HTTP, SMTP, XML.

- `providers/volga_wolga/` — fetch (reqwest), parse (quick-xml),
  canonical transformer, circuit breaker
- `repositories/postgres/` — репозитории с SCD2, батчевыми upsert'ами,
  retry, advisory locks
- `notifier.rs` — SMTP через `lettre` или `NoopNotifier`
- `metrics/` — Prometheus recorder, `NoopMetrics` для тестов
- `health.rs` — `DatabaseHealthCheck`, `SyncPipelineFreshnessCheck`,
  `ContentFreshnessCheck`, `SmtpHealthCheck`
- `clock.rs` — `SystemClock`

### `api`

HTTP-слой на axum.

- `bootstrap.rs` — сборка Router + TCP listener + graceful shutdown
- `routes/` — обработчики
- `middleware/` — auth, rate_limit, cache
- `state.rs` — `AppState` с хешем токена (не plaintext), rate limiter,
  use cases
- `dto/` — сериализация ответов

### `worker`

Daemon + CLI.

- `cli.rs` — `run` / `sync --all` / `sync --provider=N`
- `bootstrap.rs` — wiring: pool, providers, use case, scheduler, health,
  retention, digest, metrics
- `scheduler.rs` — cron через `cron` crate; per-provider tick lock
- `digest.rs` — two-phase доставка error-дайджестов
- `retention.rs` — cleanup `raw_snapshots`, `health_checks`, `errors`,
  orphan `sync_runs`
- `metrics_server.rs` — HTTP на отдельном порту для Prometheus

## Sync pipeline

Каждый цикл `sync_one` проходит 7 фаз. Все фазы измеряются
гистограммой `kamtur_sync_phase_duration_seconds{provider, phase}`.

### 1. Fetch (`fetch_ms`)

`provider.fetch_raw()` под circuit breaker. Volga отдаёт XML
до 15 минут. Reqwest с `timeout(900s)` + `connect_timeout(60s)`.

### 2. Hash (`hash_ms`)

`Fingerprint::of(raw)` — SHA256 от сырого ответа. Детерминированная
идентификация контента.

### 3. Dedup (`dedup_ms`)

`has_snapshot(provider_id, fingerprint)` — есть ли такой sha256
в `raw_snapshots`?

- Есть → **возвращаем `SyncOutcome { duplicate: true }`**, sync
  завершается со статусом `skipped`, БД не трогается.
- Нет → продолжаем.

### 4. Parse (`parse_ms`)

`provider.parse(raw)` в `spawn_blocking` — XML парсинг CPU-bound,
не должен блокировать Tokio runtime.

Guard: если `objects.is_empty() && tours.is_empty()` — ошибка.
Защита от парсерного бага, который иначе деактивировал бы всё в БД.

### 5. Load rules

`repository.load_enrichment_rules(id)` — правила из БД. Пока простаивают
в фазе 6.

### 6. Enrich (`enrich_ms`)

`enrich_tours(canonical, rules, provider_config)` — для каждого тура
применяем правила по приоритету. Правило с меньшим `priority` побеждает.
Результат — `Vec<EnrichedTour>` с полями `site_name`, `route`, `days`, ...

### 7. Persist (`persist_ms`)

Одна транзакция, `pg_advisory_xact_lock(hashtext(provider_id))` —
сериализация sync одного провайдера. Внутри:

1. `insert_snapshot` — если sha256 уже есть, `duplicate: true` (race).
2. `upsert_objects`, `stages`, `classes`, `rooms`, `tours`
3. `apply_prices_scd2`, `apply_sales_scd2`, `apply_availability_scd2`
4. `deactivate_missing` — то, что пропало у провайдера, помечаем
   `is_active = false`

## SCD2 — как работает

Slowly Changing Dimensions type 2. Применяется к трём таблицам:
`cruise_provider_prices`, `cruise_provider_sales`, `cruise_provider_availability`.

Поля: `valid_from`, `valid_to`. Актуальная версия — `valid_to IS NULL`.

При sync:

1. **Изменение** — старая версия закрывается `valid_to = now()`,
   открывается новая. `updated += 1`.
2. **Пропажа** — только закрытие. `closed += 1`.
3. **Новая** — открывается. `created += 1`.

Ключ SCD2 включает все размерности, которые могут меняться независимо:
- prices: `(cruise_id, class_id, partial_buyout)`
- sales: `(cruise_id, class_id, room_id, partial_buyout)`
- availability: `(cruise_id, room_id)`

Два класса в одной цене круиза → две независимые версии.

Деталь: `sales::apply_sales_scd2` имеет две ветки — `apply_small` (< 100
строк, прямые UNNEST) и `apply_with_temp_table` (≥ 100, TEMP TABLE).
**Логика сравнения одинаковая** (это тестируется).

## БД — обзор схемы

| Таблица | Назначение | Особенности |
|---|---|---|
| `cruise_providers` | Справочник провайдеров | PK `id` |
| `cruise_provider_objects` | Корабли | PK `(provider, object_id)` |
| `cruise_provider_stages` | Палубы | |
| `cruise_provider_classes` | Классы кают | |
| `cruise_provider_rooms` | Каюты | |
| `cruise_provider_tours` | Круизы | Обогащённые поля |
| `cruise_provider_prices` | Цены (SCD2) | `valid_from`, `valid_to` |
| `cruise_provider_sales` | СПО (SCD2) | |
| `cruise_provider_availability` | Доступность (SCD2) | |
| `raw_snapshots` | Сырые XML с sha256 | zstd если > 1 MB |
| `sync_runs` | История sync | `status`: running/success/failed/skipped |
| `errors` | Ошибки с state machine | `notification_state`: pending/claimed/sent/dead |
| `health_checks` | Health check логи | |
| `enrichment_rules` | Правила обогащения | |
| `cruise_types` | Справочник типов | |
| `stages`, `stages_mapping` | Служебные | |

Индексы:
- `errors_claimable_idx` — partial index для claim_batch
- `raw_snapshots (provider, sha256)` — unique
- Partial index `WHERE is_active = true` для ускорения deactivation

## Digest state machine

```
             ┌──────────┐
             │  pending │ ←───────┐
             └────┬─────┘         │
       claim_batch│               │ mark_failed
                  ▼               │ (attempts < max)
             ┌──────────┐         │
             │ claimed  │─────────┘
             └────┬─────┘
          ┌───────┼────────┐
   mark_sent│              │mark_failed (attempts >= max)
          ▼               ▼
      ┌──────┐        ┌──────┐
      │ sent │        │ dead │
      └──────┘        └──────┘
```

- `claim_batch(window, max_attempts, lease, limit)` — атомарно забирает
  `pending` + просроченные `claimed`. Инкрементит `notification_attempts`.
- `mark_sent(ids)` — успех, ставит `notified_at = now()`.
- `mark_failed(ids, max_attempts)` — retry или dead.
- `sweep_dead_claims` — для залипших `claimed` с исчерпанными попытками.

Гарантии:
- SMTP падает → `mark_failed` → `pending` → retry.
- Worker падает между claim и ack → lease истекает → следующий `claim_batch` подхватит.
- Ошибки старше окна (24 часа) → `sweep_dead_claims` → `dead`.

## Circuit breaker

На провайдера. `failure_threshold = 10`, `success_threshold = 2`,
`timeout = 30 минут`.

- `Closed` — норма.
- `Open` — ошибок ≥ 10 → не пускаем запросы 30 минут.
- `HalfOpen` — после timeout, пропускаем 2 запроса. Успех → `Closed`,
  ошибка → `Open` снова.

Volga может отвечать 15 минут — это норма, не ошибка. `expect("failed")` в
fetcher не паникует, только при конфигурационных ошибках.

## Read pipeline

### `GET /cruises`

Один SQL: tours + join objects + subquery MIN(base_price) + subquery
room_counts + `COUNT(*) OVER()` для total.

Fallback: если offset за пределами — отдельный count.

### `GET /cruises/:id`

Три параллельных запроса через `tokio::try_join!`:
- `load_class_prices` — цены по классам
- `load_rooms` — каюты (только те, что в availability)
- `load_availability` — доступность

Собираем в `CruiseDetail` с `prices` и `rooms`, где `rooms` содержат
цены своего класса.

## Middleware — порядок на API

Слои применяются снизу вверх. На входящий запрос:

1. `SetRequestIdLayer` — кладёт `X-Request-Id` (от клиента или UUID)
2. `TraceLayer` — создаёт span, читает `request_id`
3. `PropagateRequestIdLayer` — копирует в response
4. `SetSensitiveRequestHeadersLayer` — маскирует `Authorization`
5. `cache_headers` — ETag, Cache-Control
6. `rate_limit_mw` — token bucket
7. `prometheus_layer` — метрики
8. `require_token` (per-route) — Bearer
9. handler

Порядок важен: `SetRequestId` должен быть **внешним**, иначе `TraceLayer`
не увидит `x-request-id`.

## Решения и trade-offs

### Почему SCD2, а не UPSERT

Цены меняются со временем. Если терять историю, нельзя:
- Анализировать изменения (провайдер поднял цены в выходные?)
- Отлаживать клиентов, которые жалуются на «сейчас дешевле, чем было утром»

UPSERT оставляет только текущее состояние. SCD2 — всю историю.

### Почему SHA256 для дедупликации

- Детерминированная идентификация без хранения самого XML для сравнения.
- `raw_snapshots (provider, sha256)` — unique index, гонки исключены.
- Volga отдаёт до 100 MB XML — сравнение через `==` дорого.

### Почему advisory lock на провайдера

Два воркера могут sync'ить одного провайдера параллельно (rolling
deploy, случайный cron overlap). Advisory lock сериализует их на уровне БД.

Разные провайдеры — параллельно: `hashtext(provider_id)` даёт разные
ключи.

### Почему два временных ряда freshness

`pipeline_freshness` (status IN ('success', 'skipped')) vs
`content_freshness` (status = 'success'). Разные вопросы:
- Пайплайн жив? → pipeline
- Провайдер меняет контент? → content

Провайдер может 3 дня отдавать идентичный XML. Пайплайн зелёный, content
degraded — это **сигнал**, не авария.

### Почему `BATCH_SIZE = 5000`

UNNEST с 5000 массивов text/date/time/bool в PostgreSQL — оптимум
между размером пакета и memory pressure. Больше 10000 — риск OOM
на большом XML.

### Почему `synchronous_commit = off`

В `apply_sync` и в worker pool. Мы не теряем данные при crash —
теряем последние миллисекунды. `sync_one` идемпотентен, повторный sync
сверит состояние. Скорость ×2-3.

## Наблюдаемость

### Логи

Structured через `tracing`:

```
INFO sync_cycle{provider=1 run_id=42}: sync done provider=1 fetch_ms=37882 ...
INFO http_request{method=GET uri=/cruises request_id=<uuid>}: ...
```

`run_id` в worker — из `sync_runs`. По нему можно найти запись в БД.
`request_id` в API — correlation id клиента.

### Метрики

Полный список — в `infrastructure/metrics/prometheus.rs`. Ключевые:

- `kamtur_sync_cycles_total{provider, status}` — счётчик циклов
- `kamtur_sync_last_success_timestamp_seconds{provider}` — важнее всего
- `kamtur_sync_phase_duration_seconds{provider, phase}` — гистограмма
- `kamtur_db_pool_available`, `kamtur_db_pool_size`

### Health checks

`worker` пишет в `health_checks` каждые `HEALTH_CHECK_INTERVAL_SECS`:
- `database` — `SELECT 1`, latency > 3s → degraded
- `sync:pipeline_freshness` — последний success/skipped < 1h
- `sync:content_freshness` — последний success < 24h
- `smtp` — TCP connect к SMTP

## Что отложено

- **Cursor pagination** — текущий `OFFSET` ок для < 10k строк. При росте
  заменим на `WHERE (begin_date, cruise_id) > ($cursor)`.
- **Redis cache** для `/cruises` — TTL 60s. In-memory ETag уже даёт
  экономию трафика, но не снимает нагрузку с БД.
- **Партиционирование** `prices_history` по `valid_from` — нужен объём.
- **OpenTelemetry** — сквозная трассировка API → worker → БД.
- **Distributed rate limiting** — Redis. Пока один instance.