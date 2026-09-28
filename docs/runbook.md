# Runbook

On-call справочник. Что делать, если алерт или что-то сломалось.

## Быстрые ссылки

| Что | Где |
|---|---|
| Метрики | `http://<worker-host>:9090/metrics` |
| API health | `http://<api-host>:8080/health` |
| Логи worker | `logs/worker.log.YYYY-MM-DD` |
| Логи api | `logs/api.log.YYYY-MM-DD` |
| Health checks в БД | `SELECT * FROM health_checks ORDER BY checked_at DESC LIMIT 20;` |
| Sync history | `SELECT * FROM sync_runs ORDER BY started_at DESC LIMIT 20;` |
| Ошибки | `SELECT * FROM errors WHERE resolved = false ORDER BY occurred_at DESC LIMIT 20;` |
| Digest state | `SELECT notification_state, count(*) FROM errors GROUP BY 1;` |

## Основные команды

### Ручной sync одного провайдера

Когда: после фикса парсера, при подозрении на stale-данные, при
разборе инцидента.

```bash
cargo run --release --bin worker -- sync --provider=1
```

Exit code 0 — успех, 1 — были ошибки. Логи в stdout.

### Проверка state digest

```sql
SELECT notification_state, count(*)
FROM errors
WHERE resolved = false
GROUP BY 1;
```

Ожидаемо: почти всё в `sent`. `pending` — свежие ошибки, ещё не
отправленные. `claimed` — сейчас в полёте (единицы). `dead` — алерт.

### Проверка orphan sync_runs

```sql
SELECT id, started_at, status
FROM sync_runs
WHERE status = 'running'
  AND started_at < now() - interval '2 hours';
```

Ожидаемо: пусто. Если есть — worker упал во время sync. Через
`ORPHAN_CHECK_INTERVAL_SECS` (1ч) пометятся `failed` автоматически.
Принудительно:

```bash
worker sync --provider=1   # проверит и запишет новый run
```

## Алерты

### `sync:pipeline_freshness != ok`

Последний sync (success или skipped) был > 1 часа назад.

**Проверить:**
```sql
SELECT id, started_at, finished_at, status, error_message
FROM sync_runs
WHERE cruise_provider_id = '1'
ORDER BY started_at DESC
LIMIT 5;
```

**Что это значит:**

- **Все `failed`** — провайдер недоступен или ошибка парсера. Смотрим
  `errors`, `error_message` в `sync_runs`.
- **Все `running`** — текущий sync завис. Проверить, что worker жив:
  `ps aux | grep worker`.
- **Пусто** — worker не запущен или scheduler сломан.

**Что делать:**
```bash
# 1. Проверить worker
ps aux | grep worker

# 2. Если не запущен — поднять
systemctl start kamtur-worker

# 3. Если запущен — ручной sync для диагностики
cargo run --release --bin worker -- sync --provider=1

# 4. Смотреть логи
tail -f logs/worker.log.$(date +%F)
```

### `sync:content_freshness == degraded`

> 24 часа провайдер не менял контент (только `skipped`).

Это **сигнал к разбору**, не авария. Если провайдер объективно не
меняет расписание — это нормально.

**Проверить:**
```sql
SELECT id, started_at, status
FROM sync_runs
WHERE cruise_provider_id = '1'
  AND status = 'success'
ORDER BY started_at DESC
LIMIT 5;
```

Если последний `success` очень давно — смотреть:

1. Провайдер отдаёт кэш (см. ниже).
2. Реально ничего не меняется — ок, но обновить threshold если это
   ожидаемо.

**Проверить кэш провайдера:**
```bash
curl -i "http://test.volgaural.ru/php/xml/2023/index-kamtur.php" | head -20
```

Посмотреть `Last-Modified`, `ETag`, размер. Если XML не меняется
день-два — это норма.

### `circuit_breaker_state == open`

Провайдер перестал отвечать, breaker отключил его на 30 минут.

**Проверить:**
```bash
# Прямой запрос
curl -i "$VOLGA_URL" --max-time 30
```

**Что делать:**
- Если провайдер лёг — ждать. Breaker сам перейдёт в HalfOpen через
  30 минут.
- Если фикс — ручной sync после того, как провайдер поднялся.
- Если ошибка на нашей стороне — смотреть логи, фиксить, рестартить.

### `db_pool_available == 0`

Пул соединений исчерпан.

**Проверить:**
```sql
SELECT count(*), state
FROM pg_stat_activity
WHERE datname = 'kamtur24'
GROUP BY state;
```

**Что делать:**
- Медленные запросы держат соединения — смотреть `pg_stat_activity`
  с `query_start < now() - interval '1 minute'`.
- Убить: `SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE ...`.
- Увеличить пул в конфиге: `max_connections` в `bootstrap.rs`.

### `smtp == down`

SMTP-сервер недоступен.

**Проверить:**
```bash
# Из shell на worker-хосте
timeout 5 nc -vz $SMTP_HOST $SMTP_PORT
```

**Что делать:** ошибки в digest продолжат накапливаться в `pending`,
при восстановлении SMTP они отправятся. Ничего не потеряется.

## Troubleshooting

### Sync возвращает `skipped`, а данные не меняются

Это **нормально**. `skipped` = провайдер отдал байт-в-байт тот же XML.
sha256 совпал, БД не трогали.

Если данные должны были измениться — смотрите на стороне провайдера.
Можно проверить вручную:

```bash
# Получить сырой XML
curl -s "$VOLGA_URL" | sha256sum

# Сравнить с последним snapshot в БД
psql "$DATABASE_URL" -c "SELECT sha256, fetched_at FROM raw_snapshots WHERE cruise_provider_id = '1' ORDER BY fetched_at DESC LIMIT 1;"
```

### `errors` растут, `dead` не ноль

Значит часть ошибок не удалось отправить за `MAX_ATTEMPTS = 5` попыток.
Это **уже не будет доставлено** через digest. Посмотреть:

```sql
SELECT id, stage, severity, message, notification_attempts, notified_at
FROM errors
WHERE notification_state = 'dead'
ORDER BY occurred_at DESC
LIMIT 20;
```

Причина обычно: SMTP лежал > 5 циклов (25 минут). Найти время:

```sql
SELECT min(occurred_at), max(occurred_at), count(*)
FROM errors
WHERE notification_state = 'dead'
  AND occurred_at > now() - interval '1 day';
```

### `incoming_sales already exists` / `incoming_prices already exists`

Теоретически возможно при `apply_sync` дважды в одной транзакции. У нас
такого быть не должно — guard через advisory lock.

Если появилось — это **баг**, см. `DROP TABLE IF EXISTS` в
`sales.rs`, `prices.rs`, `availability.rs`. Должен быть первым стейтментом
перед `CREATE TEMP TABLE`.

### Sync зависает на `running` дольше часа

Worker упал с активной транзакцией. Advisory lock `pg_advisory_xact_lock`
автоматически снимется при разрыве соединения. Но запись `sync_runs`
останется в `running`.

Автоматически: через 2 часа `orphan_check_interval` пометит
`failed`.

Принудительно:
```sql
UPDATE sync_runs
SET status = 'failed',
    finished_at = now(),
    error_message = 'orphan: manual cleanup'
WHERE status = 'running'
  AND started_at < now() - interval '1 hour';
```

### API отвечает 401 для валидного токена

1. `API_TOKEN` в env у API процесса:
   ```bash
   systemctl show kamtur-api --property=Environment
   ```
2. Токен сравнивается через SHA256 + `subtle::ConstantTimeEq`. Хеш
   считается один раз при старте. Если env обновился, а процесс не
   рестартился — старый хеш.
3. Рестарт: `systemctl restart kamtur-api`.

### API отвечает 429 всем клиентам

Rate limiter по IP. Если клиент за прокси — все запросы идут с одного
IP (nginx). Нужно `TRUSTED_PROXIES`.

**Проверить:**
```bash
# На API-хосте
echo $TRUSTED_PROXIES

# Из логов — какой IP видит API
grep "rate limit exceeded" logs/api.log.$(date +%F) | tail -5
```

**Исправить:** установить `TRUSTED_PROXIES` (CIDR прокси) и рестарт.

## Ротация данных (retention)

Автоматически раз в сутки `run_retention_loop`:

- `raw_snapshots` — старше 90 дней, кроме тех, что ссылаются `sync_runs`.
- `health_checks` — старше 30 дней.
- `errors` (resolved = true) — старше 90 дней.

Проверка размера raw_snapshots:

```bash
curl -s http://localhost:9090/metrics | grep kamtur_raw_snapshots
```

Ожидаемо: `kamtur_raw_snapshots_size_bytes` в пределах 1-5 GB.

## Recovery

### Восстановление из бэкапа БД

1. Восстановить дамп.
2. **Обязательно** применить миграции:
   ```bash
   cargo run --example migrate -p worker
   ```
   (если example удалён — временно через `worker run`).
3. Проверить `_sqlx_migrations`.

### После сбоя worker'а

```bash
# 1. Поднять
systemctl start kamtur-worker

# 2. Проверить, что orphan sync_runs пометились
psql "$DATABASE_URL" -c "SELECT status, count(*) FROM sync_runs WHERE status = 'running' GROUP BY 1;"

# 3. Ручной sync для проверки
cargo run --release --bin worker -- sync --provider=1

# 4. Смотреть метрики
curl -s http://localhost:9090/metrics | grep kamtur_sync_cycles_total
```

### После рестарта Postgres

Worker сам восстановит соединения (retry с exponential backoff в
`with_retry`). Может понадобиться 5-10 секунд.

Если что-то зависло — рестарт worker'а.

## Maintenance

### Обновление правил обогащения

```sql
INSERT INTO enrichment_rules
    (cruise_provider_id, field_name, rule_type, rule_config, priority, is_active)
VALUES
    ('1', 'site_name', 'constant', '{"value": "Volga Wolga"}', 100, true);
```

Применится при следующем sync. Правила кэшируются на один цикл.

### Изменение расписания sync

`worker/src/bootstrap.rs`:

```rust
let schedules = vec![ProviderSchedule {
    provider_id: "1".into(),
    cron: "0 0 * * * *".into(),   // каждый час
    run_on_start: true,
}];
```

Рестарт worker'а после изменения.

### Просмотр активных advisory locks

```sql
SELECT objid::bigint AS key,
       (objid::bigint >> 32) AS dbid,
       mode, granted
FROM pg_locks
WHERE locktype = 'advisory';
```

В норме — пусто или единицы.

## Escalation

| Severity | Пример | Что делать |
|---|---|---|
| **Critical** | API лежит, БД недоступна | Дежурный инженер |
| **High** | Sync сломан > 2 часов | Разбор в течение дня |
| **Medium** | Content freshness degraded | Разбор в течение недели |
| **Low** | Единичные errors | Наблюдение |

## Контакты

- Команда разработки: `#kamtur-dev` (Slack)
- On-call: PagerDuty rotation
