-- ============================================================
-- Availability SCD2.
--
-- Было: SCD1 (перезапись available).
-- Стало: SCD2 — каждая смена доступности создаёт новую версию.
-- ============================================================

-- 1. Дропнуть зависимые view и индексы старой таблицы
DROP VIEW IF EXISTS cruise_provider_tours_view CASCADE;
DROP INDEX IF EXISTS availability_tour;
DROP INDEX IF EXISTS availability_provider_available;

-- 2. Переименовать старую
ALTER TABLE cruise_provider_availability
    RENAME TO cruise_provider_availability_scd1;

-- 3. Создать новую
CREATE TABLE cruise_provider_availability (
    id                        BIGSERIAL PRIMARY KEY,
    cruise_provider_id        TEXT NOT NULL REFERENCES cruise_providers(id) ON DELETE CASCADE,
    cruise_provider_cruise_id TEXT NOT NULL,
    cruise_provider_room_id   TEXT NOT NULL,
    available                 BOOLEAN NOT NULL,
    valid_from                TIMESTAMPTZ NOT NULL DEFAULT now(),
    valid_to                  TIMESTAMPTZ,
    UNIQUE (cruise_provider_id, cruise_provider_cruise_id,
            cruise_provider_room_id, valid_from)
);

CREATE UNIQUE INDEX availability_current_uniq
    ON cruise_provider_availability
        (cruise_provider_id, cruise_provider_cruise_id, cruise_provider_room_id)
    WHERE valid_to IS NULL;

CREATE INDEX availability_cruise
    ON cruise_provider_availability (cruise_provider_id, cruise_provider_cruise_id);

CREATE INDEX availability_room
    ON cruise_provider_availability (cruise_provider_id, cruise_provider_room_id);

-- 4. Перенести данные из SCD1 в SCD2
INSERT INTO cruise_provider_availability
    (cruise_provider_id, cruise_provider_cruise_id, cruise_provider_room_id,
     available, valid_from)
SELECT
    cruise_provider_id,
    cruise_provider_cruise_id,
    cruise_provider_room_id,
    available,
    COALESCE(updated_at, now())
FROM cruise_provider_availability_scd1;

-- 5. Удалить старую
DROP TABLE cruise_provider_availability_scd1;

-- 6. View: только текущие (открытые) записи
CREATE OR REPLACE VIEW cruise_provider_availability_current AS
SELECT
    cruise_provider_id,
    cruise_provider_cruise_id,
    cruise_provider_room_id,
    available,
    valid_from
FROM cruise_provider_availability
WHERE valid_to IS NULL;

-- 7. Восстановить cruise_provider_tours_view с учётом SCD2
CREATE OR REPLACE VIEW cruise_provider_tours_view AS
SELECT
    t.*,
    COALESCE((
        SELECT count(*) FROM cruise_provider_availability a
        WHERE a.cruise_provider_id = t.cruise_provider_id
          AND a.cruise_provider_cruise_id = t.cruise_provider_cruise_id
          AND a.available = true
          AND a.valid_to IS NULL
    ), 0)::INT AS room_counts,
    (
        SELECT MIN(base_price) FROM (
            SELECT base_price FROM cruise_provider_prices p
            WHERE p.cruise_provider_id = t.cruise_provider_id
              AND p.cruise_provider_cruise_id = t.cruise_provider_cruise_id
              AND p.valid_to IS NULL
            UNION ALL
            SELECT base_price FROM cruise_provider_sales s
            WHERE s.cruise_provider_id = t.cruise_provider_id
              AND s.cruise_provider_cruise_id = t.cruise_provider_cruise_id
              AND s.valid_to IS NULL
        ) sub
    ) AS minimal_price
FROM cruise_provider_tours t;
