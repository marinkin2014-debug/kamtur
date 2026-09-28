# Kamtur

[![CI](https://github.com/marinkin2014-debug/kamtur/actions/workflows/ci.yml/badge.svg)](https://github.com/marinkin2014-debug/kamtur/actions/workflows/ci.yml)
[![Rust](https://img.shields.io/badge/rust-stable-93450a?logo=rust)](https://www.rust-lang.org)
[![Postgres 16](https://img.shields.io/badge/postgres-16-336791?logo=postgresql)](https://www.postgresql.org)

Парсер круизных туров провайдера Volga с read-only HTTP API поверх PostgreSQL.

## Что это

- **Синхронизация**. Воркер забирает XML-фид провайдера, нормализует данные, дедуплицирует по SHA256-fingerprint, обогащает через правила (`enrichment_rules`) и пишет в PostgreSQL.
- **История изменений**. Цены и продажи хранятся по схеме SCD Type 2 (`valid_from` / `valid_to`). Изменение цены закрывает старую версию и открывает новую.
- **Read API**. `axum`-сервис отдаёт список круизов (keyset-пагинация) и детали одного круиза.
- **Наблюдаемость**. Метрики Prometheus из воркера и API, дашборды Grafana, алерты Alertmanager с SMTP-уведомлениями.
- **Качество**. 200+ тестов, `cargo clippy -D warnings`, `cargo audit`, `cargo deny` — все зелёные в CI на каждый push.

## Архитектура

Workspace из 5 крейтов, зависимости направлены строго внутрь:

| Крейт | Ответственность | Зависит от |
|-------|-----------------|-----------|
| `domain` | Чистые типы, порты (traits), ошибки | `std`, `chrono`, `serde`, `sha2` |
| `application` | Use cases: sync pipeline, list/get cruises | `domain` |
| `infrastructure` | Адаптеры: PostgreSQL, Volga, SMTP, Prometheus, circuit breaker | `domain` |
| `worker` | Бинарь: scheduler, sync, health, retention, error digest | `application`, `infrastructure` |
| `api` | Бинарь: HTTP read API (axum 0.7) | `application`, `infrastructure` |

**Поток данных:**

```text
Volga XML ──fetch──▶ parse ──transform──▶ Canonical ──enrich──▶ repository ──▶ PostgreSQL
                                                                                   │
API ◀──────────────────── read repository ◀────────────────────────────────────────┘
```

**Ключевые инварианты:**

- Fetch обёрнут в circuit breaker (10 подряд ошибок → open на 30 мин).
- Дедупликация через `raw_snapshots.cruise_provider_id + sha256` — повторный sync того же XML не пишет в БД.
- Все записи одного sync'а идут в одной транзакции с `pg_advisory_xact_lock` по `provider_id`.
- Деактивация отсутствующих сущностей (tours, objects, stages, classes, rooms) — через temp table + `NOT EXISTS`, не через `<> ALL($array)`.

## Быстрый старт

### Требования

- [Rust](https://rustup.rs) (stable)
- [Docker](https://docs.docker.com/get-docker/) — для локального PostgreSQL
- Git

### 1. Клонирование и сборка

```bash
git clone https://github.com/marinkin2014-debug/kamtur.git
cd kamtur
cargo build --workspace
```

### 2. Локальный PostgreSQL

```bash
docker run -d --name kamtur-postgres-test \
    -e POSTGRES_USER=test \
    -e POSTGRES_PASSWORD=test \
    -e POSTGRES_DB=kamtur_test \
    -p 5433:5432 \
    postgres:16-alpine
```

### 3. Переменные окружения

Создать `.env` в корне репозитория:

```dotenv
DATABASE_URL=postgres://test:test@127.0.0.1:5433/kamtur_test
TEST_DATABASE_URL=postgres://test:test@127.0.0.1:5433/kamtur_test
API_TOKEN=dev-token-change-me
```

`.env` в `.gitignore`. Миграции применяются автоматически при старте воркера.

### 4. Запуск

В отдельных терминалах:

```bash
# Воркер: scheduler + sync + health + retention + digest. Миграции применяются здесь.
cargo run -p worker -- run

# API: read-only HTTP. Ожидает, что миграции уже применены.
cargo run -p api
```

### 5. Проверка

```bash
curl http://127.0.0.1:8080/health
# {"status":"ok"}

curl http://127.0.0.1:8080/ready
# {"status":"ok"}   (или 503, если БД недоступна)

curl -H "Authorization: Bearer dev-token-change-me" \
    "http://127.0.0.1:8080/cruises?limit=5"
```

## Конфигурация

### Worker

| Переменная | Default | Описание |
|------------|---------|----------|
| `DATABASE_URL` | — (required) | PostgreSQL connection string |
| `MAX_CONCURRENCY` | `8` | Параллельно sync'ащихся провайдеров |
| `BATCH_SIZE` | `5000` | Размер батча в `UNNEST`-запросах |
| `RAW_COMPRESSION` | `zstd` | `zstd` или `none` для `raw_snapshots.payload` |
| `METRICS_BIND` | `0.0.0.0:9090` | Prometheus endpoint |
| `HEALTH_CHECK_INTERVAL_SECS` | `60` | Период health-loop |
| `SHUTDOWN_GRACE_PERIOD_SECS` | `60` | Таймаут graceful shutdown |
| `ORPHAN_SYNC_RUN_STALE_SECS` | `7200` | `running` старше → `failed` |
| `ORPHAN_CHECK_INTERVAL_SECS` | `3600` | Период orphan-проверки |
| `SMTP_HOST` | — | Если пусто — `NoopNotifier` |
| `SMTP_PORT` | `587` | |
| `SMTP_USER` | — | |
| `SMTP_PASSWORD` | — | |
| `SMTP_FROM` | — | |
| `SMTP_TO` | — | CSV, несколько адресов |
| `VOLGA_URL` | (тестовый фид) | URL XML-фида |
| `VOLGA_REQUEST_TIMEOUT_SECS` | `900` | 15 минут — фид отдаётся долго |
| `VOLGA_CONNECT_TIMEOUT_SECS` | `60` | |

### API

| Переменная | Default | Описание |
|------------|---------|----------|
| `DATABASE_URL` | — (required) | PostgreSQL connection string |
| `DATABASE_URL_REPLICA` | — | Опционально: read replica |
| `API_BIND` | `0.0.0.0:8080` | HTTP bind |
| `API_TOKEN` | — (required) | Bearer-токен; хранится как SHA256-хэш |
| `TRUSTED_PROXIES` | — | CIDR-список через запятую: `10.0.0.0/8,127.0.0.1/32` |
| `DEFAULT_PROVIDER_ID` | `1` | Если query-параметр не задан |
| `READ_STATEMENT_TIMEOUT_SECS` | `5` | `statement_timeout` для read-пула |
| `REQUEST_TIMEOUT_SECS` | `30` | Сквозной HTTP-таймаут; должен быть больше `READ_STATEMENT_TIMEOUT_SECS` |

## CLI воркера

```text
Kamtur worker

USAGE:
    worker run                    Daemon: scheduler + sync + health + retention + digest
    worker sync --all             Прогнать sync всех провайдеров и выйти
    worker sync --provider=<id>   Прогнать sync одного провайдера и выйти
    worker --help
```

`sync` не поднимает фоновые задачи — только pool → providers → use case. Удобно для ручных операций после фикса парсера или при инцидентах. Возвращает ненулевой exit code, если хотя бы один провайдер упал.

## HTTP API

### Endpoints

| Method | Path | Auth | Описание |
|--------|------|------|----------|
| `GET` | `/health` | — | Liveness. Всегда 200, если процесс жив |
| `GET` | `/ready` | — | Readiness. Ping БД (timeout 2 сек) |
| `GET` | `/metrics` | — | Prometheus |
| `GET` | `/cruises` | Bearer | Список круизов |
| `GET` | `/cruises/:id` | Bearer | Детали одного круиза |

### Query-параметры `GET /cruises`

| Параметр | Тип | Описание |
|----------|-----|----------|
| `cruise_provider_id` | string | Default — `DEFAULT_PROVIDER_ID` |
| `begin_from` | `YYYY-MM-DD` | Фильтр `begin_date >= ...` |
| `begin_to` | `YYYY-MM-DD` | Фильтр `begin_date <= ...` |
| `departure_city` | string | Точное совпадение |
| `limit` | int (1..100) | Default 20, клампится |
| `cursor` | string | Opaque, из `next_cursor` |

### Пагинация

Keyset по `(begin_date ASC, cruise_id ASC)`. Opaque-курсор — hex-строка, клиент не парсит.

```bash
# Первая страница
curl -H "Authorization: Bearer $TOKEN" \
    "http://127.0.0.1:8080/cruises?limit=50"
# → { items: [...], limit: 50, has_more: true, next_cursor: "..." }

# Следующая страница
curl -H "Authorization: Bearer $TOKEN" \
    "http://127.0.0.1:8080/cruises?limit=50&cursor=..."
# → { items: [...], limit: 50, has_more: false }
```

`total` не возвращается: при keyset-пагинации он бессмысленен (меняется между страницами) и дорог.

### Кэширование

- `Cache-Control: private, max-age=60` — только приватный кэш (ответ содержит авторизационный контекст).
- `ETag` = `"<16 hex>"` от SHA256-хэша тела. При `If-None-Match` — `304 Not Modified`.
- Тело буферизуется для хэша только если ≤ 1 MiB. Иначе `ETag` не выставляется.

### Ограничения

- **Rate limit**: token bucket per IP, 100 burst + 20 rps refill.
- **Timeout**: 30 сек (default). Срабатывает раньше `statement_timeout` — 504 вместо 500.
- **X-Forwarded-For** учитывается только если peer в `TRUSTED_PROXIES`.

### Формат ошибки

```json
{ "error": "not_found", "message": "cruise 999 not found" }
```

| `error` | HTTP |
|---------|------|
| `bad_request` | 400 |
| `not_found` | 404 |
| `internal_error` | 500 |
| `gateway_timeout` | 504 |

## Наблюдаемость

### Метрики воркера (`:9090/metrics`)

| Метрика | Тип | Labels | Описание |
|---------|-----|--------|----------|
| `kamtur_sync_phase_duration_seconds` | histogram | provider, phase | Время фаз: fetch, hash, dedup, parse, enrich, persist |
| `kamtur_sync_cycles_total` | counter | provider, status | Циклы: success / failed / skipped |
| `kamtur_sync_errors_total` | counter | provider, stage | Ошибки по стадиям |
| `kamtur_sync_skipped_total` | counter | provider | Пропущено из-за дедупликации |
| `kamtur_sync_last_success_timestamp_seconds` | gauge | provider | Unix timestamp |
| `kamtur_rows_written_total` | counter | provider, table | Записи по таблицам (created/updated/closed) |
| `kamtur_deactivated_total` | counter | provider | Деактивировано отсутствующих сущностей |
| `kamtur_fetch_bytes_total` | counter | provider | Байт скачано |
| `kamtur_raw_snapshots_size_bytes` | gauge | — | `pg_total_relation_size` |
| `kamtur_raw_snapshots_count` | gauge | — | Rows |
| `kamtur_db_pool_size` / `_available` | gauge | — | Пул соединений |
| `kamtur_enrichment_rules_loaded` | gauge | provider | Правил обогащения |
| `kamtur_circuit_breaker_state` | gauge | provider | 0=closed, 1=open, 2=half_open |

### Метрики API (`:8080/metrics`)

Из `axum-prometheus`:

- `axum_http_requests_total` — counter, labels: `method`, `path`, `status`
- `axum_http_requests_duration_seconds` — histogram
- `axum_http_requests_pending` — gauge

Плюс `process_*` из `metrics-process`.

### Grafana

Дашборды в `deploy/grafana/dashboards/`, provisioning автоматический:

- `kamtur-overview.json` — состояние воркера: sync phases, rows written, errors, circuit breaker.
- `kamtur-api.json` — RPS, latency (p50/p95/p99), error rate, pending requests.

Запуск локального стека:

```bash
cd deploy
cp .env.example .env    # заполнить SMTP и Grafana admin password
docker compose up -d
```

Grafana — `http://127.0.0.1:3000`, Prometheus — `http://127.0.0.1:9091`, Alertmanager — `http://127.0.0.1:9093`.

### Алерты

Правила в `deploy/prometheus/rules.yml`. Критичные:

- `WorkerDown` — Prometheus не может scrape'ить воркера > 2 мин.
- `SyncNoSuccessForTwoHours` — последний успешный sync старше 2 ч.
- `SyncErrorsHigh` — rate ошибок > 0.1/сек за 10 мин.

Warning: `SyncPersistSlow`, `SyncFetchSlow`, `RawSnapshotsSizeGrowing`, `DBPoolSaturated`, `CircuitBreakerOpen`, `ApiHighErrorRate`, `ApiHighLatency`.

## Разработка

### Pre-commit hooks

Ставится один раз:

```powershell
# Windows
.\scripts\install-hooks.ps1

# Linux / macOS / WSL / Git Bash
./scripts/install-hooks.sh
```

На коммит: whitespace-гигиена + `cargo fmt --check` + `cargo clippy -D warnings`.
На push: `cargo test --workspace` (требует `TEST_DATABASE_URL`).

Полная документация: [docs/pre-commit.md](docs/pre-commit.md).

### Тесты

```bash
# Windows PowerShell
$env:TEST_DATABASE_URL = "postgres://test:test@127.0.0.1:5433/kamtur_test"

# Linux / macOS
export TEST_DATABASE_URL="postgres://test:test@127.0.0.1:5433/kamtur_test"

cargo test --workspace
```

Integration-тесты `infrastructure` подключаются к реальному Postgres. Миграции применяются автоматически при первом тесте за сессию.

### Проверки перед push

Ровно то же, что в CI:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo audit
cargo deny check
```

### CI

GitHub Actions, 6 job'ов:

| Job | Что делает |
|-----|-----------|
| `fmt + clippy (stable)` | `cargo fmt --check`, `cargo clippy -D warnings` |
| `fmt + clippy (beta)` | То же на beta-тулчейне |
| `test (stable)` | `cargo test --workspace` с Postgres service |
| `test (beta)` | То же на beta |
| `audit` | `cargo-audit` по `RUSTSEC` |
| `deny` | `cargo-deny check`: licenses, bans, advisories, sources |

Конфиги: `deny.toml`, `.cargo/audit.toml`. Игнорируемые advisory — задокументированы в `.cargo/audit.toml` (rsa через sqlx-mysql, не эксплуатируется на Linux-таргете).

## Структура репозитория

```text
kamtur/
├── Cargo.toml                     # workspace
├── deny.toml                      # cargo-deny конфиг
├── .cargo/audit.toml              # cargo-audit конфиг
├── .pre-commit-config.yaml        # локальные git-хуки
├── README.md                      # этот файл
├── migrations/                    # 11 sqlx-миграций
├── docs/
│   ├── architecture.md            # слои, зависимости, ключевые решения
│   ├── runbook.md                 # операционные процедуры
│   ├── add-provider.md            # как добавить провайдера
│   └── pre-commit.md              # локальные git-хуки
├── deploy/
│   ├── docker-compose.yml         # Prometheus + Grafana + Alertmanager
│   ├── prometheus/
│   ├── grafana/
│   └── alertmanager/
├── scripts/
│   ├── install-hooks.ps1
│   └── install-hooks.sh
├── crates/
│   ├── domain/
│   ├── application/
│   ├── infrastructure/
│   ├── worker/
│   └── api/
└── .github/workflows/ci.yml       # CI (6 job'ов)
```

## Документация

- [docs/architecture.md](docs/architecture.md) — детали архитектуры, обоснования решений.
- [docs/runbook.md](docs/runbook.md) — инциденты, диагностика, восстановление.
- [docs/add-provider.md](docs/add-provider.md) — как добавить нового провайдера к pipeline.
- [docs/pre-commit.md](docs/pre-commit.md) — локальные git-хуки.

## Стек

- **Rust** (edition 2021, stable)
- **tokio** 1.x — async runtime
- **axum** 0.7 — HTTP framework
- **sqlx** 0.8 — PostgreSQL, без ORM
- **PostgreSQL** 16
- **quick-xml** 0.41 — парсер XML-фида
- **tracing** — структурированное логирование
- **metrics** + **metrics-exporter-prometheus** — метрики
- **Prometheus** / **Grafana** / **Alertmanager** — observability stack

## Лицензия

Private. `publish = false`.
