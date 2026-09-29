-- Seed-данные для k6 load-test (B.6).
--
-- Создаёт изолированный провайдер 'k6-bench' с 100k tours и 10k prices.
-- Изоляция по префиксу provider_id: 'k6-%' не пересекается с 'test-*'
-- (integration-тесты) и 'bench-*' (criterion-бенчмарки). Уборка —
-- одним DELETE FROM cruise_providers WHERE id = 'k6-bench'
-- (FK ON DELETE CASCADE на cruise_provider_* таблицы).
--
-- Идемпотентен через **delete-then-insert**: старый seed удаляется
-- первой строкой, новый вставляется следом. Раньше была попытка
-- ON CONFLICT DO NOTHING, но у cruise_provider_prices нет UNIQUE-
-- constraint'а на (provider, cruise, class, partial_buyout) — SCD2
-- хранит несколько версий одной пары, constraint там был бы неверным.
--
-- Запуск (PowerShell):
--   Get-Content scripts/k6/seed.sql | `
--     docker exec -i kamtur-postgres-test psql -U test -d kamtur_test
--
-- Уборка (если нужна вручную):
--   DELETE FROM cruise_providers WHERE id = 'k6-bench';
--   VACUUM FULL cruise_provider_tours;
--   VACUUM FULL cruise_provider_prices;

BEGIN;

-- 1. Удалить предыдущий seed ------------------------------------------------
--
-- Каскадом снесёт cruise_provider_tours, cruise_provider_prices,
-- cruise_provider_objects (все имеют FK на cruise_providers с
-- ON DELETE CASCADE). Порядок вставок ниже — provider → object →
-- tours → prices, чтобы FK-зависимости были удовлетворены.

DELETE FROM cruise_providers WHERE id = 'k6-bench';

-- 2. Провайдер ---------------------------------------------------------------

INSERT INTO cruise_providers (id, name)
VALUES ('k6-bench', 'k6 load-test provider');

-- 3. Корабль (object) --------------------------------------------------------
--
-- list_cruises делает LEFT JOIN cruise_provider_objects для ship_name.
-- Без строки здесь ship_name был бы NULL в каждом ответе — валидный
-- JSON, но менее реалистичная форма payload'а.

INSERT INTO cruise_provider_objects
    (cruise_provider_id, cruise_provider_object_id, name, is_active, updated_at)
VALUES ('k6-bench', 'k6-ship', 'k6 Ship', true, now());

-- 4. Tours: 100k строк -------------------------------------------------------
--
-- Распределение:
--   begin_date = 2026-01-01 + (i % 365)   — равномерно по году
--   end_date   = begin_date + 5 дней
--   cruise_id  = 'k6-cruise-' || lpad(i, 7, '0')  — лексикографически
--                отсортированный, совпадает с числовым порядком
--
-- Это даёт индекс idx_tours_keyset полноценную работу: строки
-- кластеризованы по (provider_id, begin_date, cruise_id), и любой
-- курсорный запрос идёт через index-only seek.

INSERT INTO cruise_provider_tours
    (cruise_provider_id, cruise_provider_object_id, cruise_provider_cruise_id,
     cruise_type_id, begin_date, end_date, name, is_active, updated_at)
SELECT
    'k6-bench',
    'k6-ship',
    'k6-cruise-' || lpad(i::text, 7, '0'),
    1,
    DATE '2026-01-01' + (i % 365),
    DATE '2026-01-01' + (i % 365) + 5,
    'k6 Route ' || i,
    true,
    now()
FROM generate_series(0, 99999) AS i;

-- 5. Prices: 10k строк -------------------------------------------------------
--
-- Одна цена на 10 туров. list_cruises считает minimal_price через
-- LEFT JOIN LATERAL over cruise_provider_prices UNION ALL
-- cruise_provider_sales. Разреженные prices дают NULL у большинства
-- туров — реалистично для текущего состояния фида Volga.
--
-- SCD2: не указываем ON CONFLICT, потому что UNIQUE-constraint'а нет.
-- Уникальность seed'а обеспечивается delete-then-insert в начале файла.

INSERT INTO cruise_provider_prices
    (cruise_provider_id, cruise_provider_cruise_id, cruise_provider_class_id,
     partial_buyout, base_price, currency, valid_from)
SELECT
    'k6-bench',
    'k6-cruise-' || lpad((i * 10)::text, 7, '0'),
    'k6-class-001',
    false,
    50000 + (i % 1000),
    'RUB',
    now()
FROM generate_series(0, 9999) AS i;

COMMIT;

-- Обновляем статистику planner'а, чтобы keyset-индекс и join'ы
-- стоили корректно. Без этого следующий k6-прогон использовал бы
-- оценки «до bulk-load».
--
-- ANALYZE вне транзакции: сбрасывает pg_statistic немедленно.

ANALYZE cruise_provider_tours;
ANALYZE cruise_provider_prices;
ANALYZE cruise_provider_objects;
