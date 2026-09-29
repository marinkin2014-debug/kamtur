// k6 load-test для GET /cruises.
//
// Сценарий: constant-arrival-rate 100 rps × 10 минут против
// GET /cruises?limit=50 с авторизацией.
//
// ## Зачем (B.6)
//
// Criterion-бенчмарки B.1–B.5 меряют отдельные компоненты in-process:
// парсер, transformer, fingerprint, ETag, keyset-запрос, SCD2. Они не
// включают:
//
//   - HTTP-разбор и маршрутизацию (axum + hyper);
//   - middleware-стек (auth, timeout, cache, rate limit, trace);
//   - JSON-сериализацию и постановку заголовков;
//   - round-trip до Postgres через sqlx-пул.
//
// Этот тест закрывает разрыв: end-to-end latency под реалистичной
// нагрузкой. Числа идут в docs/performance.md (B.7).
//
// ## Обязательные переменные окружения
//
//   BASE_URL     например http://localhost:8080
//   API_TOKEN    должен совпадать с API_TOKEN на сервере
//
// ## Опциональные
//
//   PROVIDER_ID  cruise_provider_id; default "k6-bench"
//
// ## Запуск
//
//   Нативный k6:
//     k6 run -e BASE_URL=http://localhost:8080 `
//            -e API_TOKEN=<token> `
//            scripts/k6/cruises.js
//
//   Docker (без установки k6):
//     docker run --rm -i `
//       -e BASE_URL=http://host.docker.internal:8080 `
//       -e API_TOKEN=<token> `
//       grafana/k6 run - < scripts/k6/cruises.js
//
// ## Rate limit на сервере
//
// Перед запуском API должно быть RATE_LIMIT_ENABLED=false. Иначе
// 100 rps с одного IP (127.0.0.1) упрутся в token bucket
// (capacity=100, refill=20/s) — тест начнёт мерить алгоритм rate limit,
// а не API.
//
// dotenvy НЕ перезаписывает уже установленные env vars, поэтому
// `$env:RATE_LIMIT_ENABLED = "false"` в окне API гарантированно
// победит значение из .env (если оно там есть). Проверка: в логе API
// должна быть строка `rate limit middleware disabled`.
//
// ## Полная инструкция по окружению
//
// docs/runbook.md (B.7).

import http from 'k6/http';
import { check } from 'k6';
import { Rate, Trend } from 'k6/metrics';

// ----- Конфигурация -----

const BASE_URL = __ENV.BASE_URL || 'http://localhost:8080';
const API_TOKEN = __ENV.API_TOKEN;
const PROVIDER_ID = __ENV.PROVIDER_ID || 'k6-bench';

if (!API_TOKEN) {
    throw new Error(
        'API_TOKEN env var is required. Pass it with -e API_TOKEN=<value>.',
    );
}

// ----- Пользовательские метрики -----
//
// Дублируют часть встроенных http_req_duration / http_req_failed, но с
// endpoint-специфичным именем. Полезно, когда в тот же тест добавятся
// другие endpoint'ы: смоук по одному пути, смоук по другому.

const listDuration = new Trend('list_cruises_duration', true);
const listErrors = new Rate('list_cruises_errors');

// ----- Опции k6 -----

export const options = {
    scenarios: {
        // constant-arrival-rate фиксирует ЧИСЛО ЗАПРОСОВ в секунду,
        // независимо от времени ответа. Это правильный executor для
        // «100 rps»: ramping-vus и constant-vus масштабируются по
        // конкурентности, а не по arrival rate, и молча недобирают
        // нагрузку, если ответ медленнее ожидаемого.
        cruises: {
            executor: 'constant-arrival-rate',
            rate: 100,
            timeUnit: '1s',
            duration: '10m',
            // preAllocatedVUs: сколько VU держим «на старте». При
            // 100 rps и ~5 ms на запрос (см. B.4) в среднем нужен
            // 1 активный VU. 50 — с большим запасом на jitter.
            // maxVUs покрывает патологический хвост, не роняя runner.
            preAllocatedVUs: 50,
            maxVUs: 300,
            tags: { endpoint: 'list' },
        },
    },
    thresholds: {
        // SLA теста. Не «ожидание», а самопроверка: если числа хуже —
        // k6 завершится с ненулевым кодом и пометит ✗ в summary.
        // B.4 показал ~4 ms server-side, +сеть/JSON/k6 overhead = ~5-20 ms.
        // 500 ms p95 — очень щедро, ловит только настоящую деградацию.
        'http_req_duration{endpoint:list}': ['p(95)<500', 'p(99)<1000'],
        'http_req_failed{endpoint:list}': ['rate<0.01'],
        'list_cruises_errors': ['rate<0.01'],
    },
    summaryTrendStats: ['min', 'med', 'avg', 'p(90)', 'p(95)', 'p(99)', 'max'],
};

// ----- Тело теста -----

export default function () {
    const url =
        `${BASE_URL}/cruises?limit=50&cruise_provider_id=${PROVIDER_ID}`;

    const params = {
        headers: {
            Authorization: `Bearer ${API_TOKEN}`,
            Accept: 'application/json',
        },
        tags: { endpoint: 'list' },
    };

    const res = http.get(url, params);

    listDuration.add(res.timings.duration, { endpoint: 'list' });
    listErrors.add(res.status !== 200, { endpoint: 'list' });

    check(res, {
        'status is 200': (r) => r.status === 200,
        'body has items': (r) => {
            try {
                const body = JSON.parse(r.body);
                return Array.isArray(body.items) && body.items.length > 0;
            } catch (_) {
                return false;
            }
        },
    });
}
