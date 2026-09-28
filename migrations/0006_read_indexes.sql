-- Индексы для API чтения.

-- Список круизов с фильтром "активные, по дате"
CREATE INDEX IF NOT EXISTS tours_provider_active_date
    ON cruise_provider_tours (cruise_provider_id, is_active, begin_date);

-- Фильтр по городу отправления
CREATE INDEX IF NOT EXISTS tours_departure_city
    ON cruise_provider_tours (cruise_provider_id, departure_city)
    WHERE is_active = true;

-- Обратный индекс для room_counts и availability (уже есть)
-- availability_current_uniq покрывает (provider, cruise, room)
-- availability_cruise покрывает (provider, cruise) WHERE valid_to IS NULL — используем

-- Для деталей круиза: цены по классам
CREATE INDEX IF NOT EXISTS prices_cruise_current
    ON cruise_provider_prices (cruise_provider_id, cruise_provider_cruise_id)
    WHERE valid_to IS NULL;

-- Для sales по каюте
CREATE INDEX IF NOT EXISTS sales_cruise_room_current
    ON cruise_provider_sales (cruise_provider_id, cruise_provider_cruise_id,
                              cruise_provider_room_id)
    WHERE valid_to IS NULL;
