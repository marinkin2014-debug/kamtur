-- Индекс для быстрой перезаписи флага available в upsert_availability.
-- Было в 0002, но не применилось на некоторых окружениях.
CREATE INDEX IF NOT EXISTS availability_provider_available
    ON cruise_provider_availability (cruise_provider_id)
    WHERE available = true;