-- Индекс под запрос списка круизов: фильтр по провайдеру + активные + сортировка по дате.
-- Ускоряет `list_cruises` с ORDER BY begin_date + LIMIT.
CREATE INDEX IF NOT EXISTS tours_provider_active_date
    ON cruise_provider_tours (cruise_provider_id, is_active, begin_date);