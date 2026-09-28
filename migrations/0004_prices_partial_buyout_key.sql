-- ============================================================
-- Расширяем natural key для цен и спец.цен: добавляем partial_buyout.
--
-- Причина: у одного (cruise_id, class_id) может быть несколько
-- вариантов размещения — полный/неполный выкуп (nofull). Раньше
-- второй вариант терялся из-за UNIQUE-ключа без partial_buyout.
-- ============================================================

-- ---------- cruise_provider_prices ----------

-- 1. Заполнить NULL значениями по умолчанию
UPDATE cruise_provider_prices SET partial_buyout = false WHERE partial_buyout IS NULL;

-- 2. Удалить все UNIQUE-ограничения и частичные индексы
DO $$
DECLARE
    r RECORD;
BEGIN
    FOR r IN
        SELECT conname FROM pg_constraint
        WHERE conrelid = 'cruise_provider_prices'::regclass
          AND contype = 'u'
    LOOP
        EXECUTE 'ALTER TABLE cruise_provider_prices DROP CONSTRAINT ' || quote_ident(r.conname);
    END LOOP;
END $$;

DROP INDEX IF EXISTS prices_current_uniq;

-- 3. Пересоздать с partial_buyout в ключе
ALTER TABLE cruise_provider_prices
    ADD CONSTRAINT prices_uniq UNIQUE
        (cruise_provider_id, cruise_provider_cruise_id,
         cruise_provider_class_id, partial_buyout, valid_from);

CREATE UNIQUE INDEX prices_current_uniq
    ON cruise_provider_prices
        (cruise_provider_id, cruise_provider_cruise_id,
         cruise_provider_class_id, partial_buyout)
    WHERE valid_to IS NULL;

-- 4. NOT NULL DEFAULT false
ALTER TABLE cruise_provider_prices
    ALTER COLUMN partial_buyout SET DEFAULT false;
ALTER TABLE cruise_provider_prices
    ALTER COLUMN partial_buyout SET NOT NULL;


-- ---------- cruise_provider_sales ----------

UPDATE cruise_provider_sales SET partial_buyout = false WHERE partial_buyout IS NULL;

DO $$
DECLARE
    r RECORD;
BEGIN
    FOR r IN
        SELECT conname FROM pg_constraint
        WHERE conrelid = 'cruise_provider_sales'::regclass
          AND contype = 'u'
    LOOP
        EXECUTE 'ALTER TABLE cruise_provider_sales DROP CONSTRAINT ' || quote_ident(r.conname);
    END LOOP;
END $$;

DROP INDEX IF EXISTS sales_current_uniq;

ALTER TABLE cruise_provider_sales
    ADD CONSTRAINT sales_uniq UNIQUE
        (cruise_provider_id, cruise_provider_cruise_id,
         cruise_provider_class_id, cruise_provider_room_id,
         partial_buyout, valid_from);

CREATE UNIQUE INDEX sales_current_uniq
    ON cruise_provider_sales
        (cruise_provider_id, cruise_provider_cruise_id,
         cruise_provider_class_id, cruise_provider_room_id, partial_buyout)
    WHERE valid_to IS NULL;

ALTER TABLE cruise_provider_sales
    ALTER COLUMN partial_buyout SET DEFAULT false;
ALTER TABLE cruise_provider_sales
    ALTER COLUMN partial_buyout SET NOT NULL;
