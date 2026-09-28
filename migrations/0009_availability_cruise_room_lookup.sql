-- Индекс для быстрого EXISTS-подзапроса в load_rooms (read.rs).
-- Ускоряет /cruises/{id}: поиск availability по (provider, cruise, room).
CREATE INDEX IF NOT EXISTS availability_cruise_room_lookup
    ON cruise_provider_availability (
        cruise_provider_id,
        cruise_provider_cruise_id,
        cruise_provider_room_id
    )
    WHERE valid_to IS NULL;