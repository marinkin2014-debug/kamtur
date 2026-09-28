CREATE INDEX IF NOT EXISTS tours_active_idx
    ON cruise_provider_tours (cruise_provider_id, cruise_provider_cruise_id)
    WHERE is_active = true;

CREATE INDEX IF NOT EXISTS objects_active_idx
    ON cruise_provider_objects (cruise_provider_id, cruise_provider_object_id)
    WHERE is_active = true;

CREATE INDEX IF NOT EXISTS stages_active_idx
    ON cruise_provider_stages (cruise_provider_id, cruise_provider_stage_id)
    WHERE is_active = true;

CREATE INDEX IF NOT EXISTS classes_active_idx
    ON cruise_provider_classes (cruise_provider_id, cruise_provider_class_id)
    WHERE is_active = true;

CREATE INDEX IF NOT EXISTS rooms_active_idx
    ON cruise_provider_rooms (cruise_provider_id, cruise_provider_room_id)
    WHERE is_active = true;

CREATE INDEX IF NOT EXISTS sync_runs_provider_status_started
    ON sync_runs (cruise_provider_id, status, started_at DESC);

CREATE INDEX IF NOT EXISTS availability_provider_available
    ON cruise_provider_availability (cruise_provider_id)
    WHERE available = true;
