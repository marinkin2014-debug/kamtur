# Kamtur — парсер круизов

Backend-сервис для сбора, нормализации и отдачи данных о речных круизах
от внешних провайдеров. Worker тянет XML/JSON с сайтов-поставщиков,
парсит, обогащает правилами и сохраняет в Postgres. API отдаёт
нормализованные данные клиентам.

## Возможности

- **Sync с провайдеров** — периодический fetch, парсинг, persist в Postgres.
- **SCD2** — история изменений цен, продаж, доступности. Не переписываем
  прошлое — закрываем старую версию (`valid_to`), открываем новую.
- **Дедупликация** — если провайдер отдал тот же XML, sha256 совпал,
  sync помечается `skipped`, БД не трогается.
- **Обогащение** — правила в БД (constant / template / days_between /
  config_lookup), применяются в порядке приоритета.
- **Circuit breaker** — при повторных ошибках провайдер временно
  отключается, чтобы не долбить по сети.
- **Read API** — HTTP на axum с Bearer-токеном, ETag, rate limit.
- **Two-phase digest** — алерты по ошибкам через SMTP с честной доставкой:
  claim → send → ack/nack, retry, dead-letter.
- **Наблюдаемость** — Prometheus метрики, structured logs, health checks.

## Быстрый старт (5 минут)

### 1. Postgres

```bash
docker compose -f docker-compose.test.yml up -d
```

Или свой Postgres:

```bash
export DATABASE_URL="postgres://user:pass@localhost:5432/kamtur"
```

### 2. Env

Минимальный `.env`:

```env
DATABASE_URL=postgres://test:test@127.0.0.1:5433/kamtur_test
API_TOKEN=dev-token-change-me2
```

### 3. Сборка

```bash
cargo build --release --workspace
```

### 4. Sync (один раз)

```bash
cargo run --release --bin worker -- sync --provider=1
```

Volga отдаёт XML ~30-60 секунд. После sync в БД появятся данные.

### 5. API

```bash
cargo run --release --bin api
```

Проверка:

```bash
curl.exe -i http://127.0.0.1:8080/health
curl.exe -i -H "Authorization: Bearer dev-token-change-me2" \
     http://127.0.0.1:8080/cruises
```

### 6. Daemon

Для прода — `worker run`. Это daemon с scheduler + health + retention +
digest. Cron задан в `worker/src/bootstrap.rs`.

## CLI

```
worker run                    # daemon: scheduler + health + retention + digest
worker sync --all             # one-shot: sync всех провайдеров
worker sync --provider=<id>   # one-shot: одного провайдера
worker --help                 # справка без ENV
```

`sync` не поднимает фоновые задачи — только pool → providers → use case.
Используется для ручных операций: после фикса парсера, при инцидентах,
в CI.

## Переменные окружения

### Worker

| Переменная | Default | Описание |
|---|---|---|
| `DATABASE_URL` | — | Postgres connection string (обязательно) |
| `MAX_CONCURRENCY` | `8` | Параллельных провайдеров |
| `BATCH_SIZE` | `5000` | Размер батча для UNNEST-вставок |
| `RAW_COMPRESSION` | `zstd` | `zstd` или `none` для raw-снапшотов |
| `ORPHAN_SYNC_RUN_STALE_SECS` | `7200` | Порог «осиротевших» `running` sync_runs |
| `ORPHAN_CHECK_INTERVAL_SECS` | `3600` | Интервал проверки |
| `METRICS_BIND` | `0.0.0.0:9090` | Порт Prometheus |
| `HEALTH_CHECK_INTERVAL_SECS` | `60` | Интервал health loop |
| `SHUTDOWN_GRACE_PERIOD_SECS` | `60` | Graceful shutdown timeout |
| `SMTP_HOST` | — | Пусто → `NoopNotifier` |
| `SMTP_PORT` | `587` | |
| `SMTP_USER` | — | |
| `SMTP_PASSWORD` | — | |
| `SMTP_FROM` | — | |
| `SMTP_TO` | — | Список через запятую |
| `VOLGA_URL` | test.volgaural.ru | URL провайдера |
| `VOLGA_REQUEST_TIMEOUT_SECS` | `900` | 15 минут |
| `VOLGA_CONNECT_TIMEOUT_SECS` | `60` | 1 минута |

### API

| Переменная | Default | Описание |
|---|---|---|
| `DATABASE_URL` | — | Postgres (обязательно) |
| `DATABASE_URL_REPLICA` | — | Read replica, если есть |
| `API_BIND` | `0.0.0.0:8080` | |
| `API_TOKEN` | — | Bearer-токен (обязательно) |
| `TRUSTED_PROXIES` | — | CIDR через запятую, напр. `10.0.0.0/8,127.0.0.1/32`. Пусто → XFF игнорируется |

## Тесты

```bash
# Локальный Postgres в Docker (tmpfs, быстро)
docker compose -f docker-compose.test.yml up -d

# Env
export TEST_DATABASE_URL=postgres://test:test@127.0.0.1:5433/kamtur_test
export DATABASE_URL=$TEST_DATABASE_URL

# Прогон
cargo test --workspace
```

Тесты **сами применяют миграции** при первом запуске. `docker down -v`
безопасен.

Покрытие:
- **domain**: fingerprint, rules — 13 тестов
- **infrastructure**: parser, SCD2, digest state machine, read — 66 тестов
- **e2e**: полный pipeline — 4 теста
- **api**: HTTP-обвязка — 35 тестов (unit + integration)

Время прогона: **~30 секунд** на локальной БД.

## Структура проекта

```
crates/
├── domain/          # сущности, порты, ошибки — без внешних зависимостей
├── application/     # use cases (sync, list, get) — координация
├── infrastructure/  # адаптеры: postgres, providers, notifier, metrics
├── api/             # HTTP API (axum)
└── worker/          # daemon: scheduler + sync + digest + retention
```

Зависимости:
```
api ──▶ infrastructure ──▶ application ──▶ domain
worker ──▶ infrastructure ──▶ application ──▶ domain
```

Domain не знает про Postgres, HTTP, SMTP. Application — только про порты.

## Документация

- [`docs/architecture.md`](docs/architecture.md) — слои, pipeline, БД, решения
- [`docs/runbook.md`](docs/runbook.md) — что делать при алерте
- [`docs/add-provider.md`](docs/add-provider.md) — добавление провайдера

## Метрики

Prometheus на `:9090/metrics`:

- `kamtur_sync_cycles_total{provider, status}` — счётчик циклов
- `kamtur_sync_phase_duration_seconds{provider, phase}` — гистограмма фаз
- `kamtur_sync_errors_total{provider, stage}` — ошибки
- `kamtur_sync_last_success_timestamp_seconds{provider}` — Unix ts последнего успеха
- `kamtur_rows_written_total{provider, table}` — записанные строки
- `kamtur_db_pool_available`, `kamtur_db_pool_size` — пул
- `kamtur_circuit_breaker_state{provider, state}` — 0=closed, 1=open, 2=half_open

## Лицензия

Internal.