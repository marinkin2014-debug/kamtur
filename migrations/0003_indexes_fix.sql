-- Индексы, отсутствовавшие в ранней версии 0002.
-- Идемпотентно благодаря IF NOT EXISTS.

CREATE INDEX IF NOT EXISTS sync_runs_provider_status_started
    ON sync_runs (cruise_provider_id, status, started_at DESC);

CREATE INDEX IF NOT EXISTS availability_provider_available
    ON cruise_provider_availability (cruise_provider_id)
    WHERE available = true;