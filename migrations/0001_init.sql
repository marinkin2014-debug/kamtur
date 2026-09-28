-- ============================================================
-- СПРАВОЧНИКИ
-- ============================================================

CREATE TABLE cruise_providers (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    schedule    TEXT NOT NULL DEFAULT '0 0 * * * *',
    is_active   BOOLEAN NOT NULL DEFAULT true,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE cruise_types (
    id   BIGSERIAL PRIMARY KEY,
    name TEXT NOT NULL UNIQUE
);

INSERT INTO cruise_types (name) VALUES ('речной круиз');

-- ============================================================
-- ГЛОБАЛЬНЫЕ ПАЛУБЫ
-- ============================================================

CREATE TABLE stages (
    id          BIGSERIAL PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE,
    sort_order  INT,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE stages_mapping (
    cruise_provider_id        TEXT NOT NULL REFERENCES cruise_providers(id) ON DELETE CASCADE,
    cruise_provider_stage_id  TEXT NOT NULL,
    stage_id                  BIGINT NOT NULL REFERENCES stages(id) ON DELETE CASCADE,
    created_at                TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (cruise_provider_id, cruise_provider_stage_id)
);
CREATE INDEX stages_mapping_stage ON stages_mapping (stage_id);

-- ============================================================
-- СЫРЫЕ ДАННЫЕ ПОСТАВЩИКОВ
-- ============================================================

CREATE TABLE cruise_provider_objects (
    id                        BIGSERIAL PRIMARY KEY,
    cruise_provider_id        TEXT NOT NULL REFERENCES cruise_providers(id) ON DELETE CASCADE,
    cruise_provider_object_id TEXT NOT NULL,
    name                      TEXT NOT NULL,
    is_active                 BOOLEAN NOT NULL DEFAULT true,
    created_at                TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at                TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (cruise_provider_id, cruise_provider_object_id)
);

CREATE TABLE cruise_provider_stages (
    id                        BIGSERIAL PRIMARY KEY,
    cruise_provider_id        TEXT NOT NULL REFERENCES cruise_providers(id) ON DELETE CASCADE,
    cruise_provider_stage_id  TEXT NOT NULL,
    name                      TEXT NOT NULL,
    is_active                 BOOLEAN NOT NULL DEFAULT true,
    created_at                TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at                TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (cruise_provider_id, cruise_provider_stage_id)
);

CREATE TABLE cruise_provider_classes (
    id                        BIGSERIAL PRIMARY KEY,
    cruise_provider_id        TEXT NOT NULL REFERENCES cruise_providers(id) ON DELETE CASCADE,
    cruise_provider_object_id TEXT NOT NULL,
    cruise_provider_class_id  TEXT NOT NULL,
    name                      TEXT NOT NULL,
    description               TEXT,
    base_seats                INT,
    tiers                     INT,
    partial_buyout            BOOLEAN,
    is_active                 BOOLEAN NOT NULL DEFAULT true,
    created_at                TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at                TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (cruise_provider_id, cruise_provider_class_id)
);
CREATE INDEX classes_object
    ON cruise_provider_classes (cruise_provider_id, cruise_provider_object_id);

CREATE TABLE cruise_provider_rooms (
    id                        BIGSERIAL PRIMARY KEY,
    cruise_provider_id        TEXT NOT NULL REFERENCES cruise_providers(id) ON DELETE CASCADE,
    cruise_provider_object_id TEXT NOT NULL,
    cruise_provider_stage_id  TEXT NOT NULL,
    cruise_provider_class_id  TEXT NOT NULL,
    cruise_provider_room_id   TEXT NOT NULL,
    number                    TEXT NOT NULL,
    is_active                 BOOLEAN NOT NULL DEFAULT true,
    created_at                TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at                TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (cruise_provider_id, cruise_provider_room_id)
);
CREATE INDEX rooms_class  ON cruise_provider_rooms (cruise_provider_id, cruise_provider_class_id);
CREATE INDEX rooms_object ON cruise_provider_rooms (cruise_provider_id, cruise_provider_object_id);

CREATE TABLE cruise_provider_tours (
    id                        BIGSERIAL PRIMARY KEY,
    cruise_provider_id        TEXT NOT NULL REFERENCES cruise_providers(id) ON DELETE CASCADE,
    cruise_provider_object_id TEXT NOT NULL,
    cruise_provider_cruise_id TEXT NOT NULL,
    cruise_type_id            BIGINT REFERENCES cruise_types(id),
    begin_date                DATE NOT NULL,
    begin_time                TIME,
    end_date                  DATE NOT NULL,
    end_time                  TIME,
    name                      TEXT NOT NULL,
    site_name                 TEXT,
    site_cruise_object_id     TEXT,
    route                     TEXT,
    city_from                 TEXT,
    city_to                   TEXT,
    departure_city            TEXT,
    days                      INT,
    is_return                 BOOLEAN,
    is_weekend                BOOLEAN,
    is_active                 BOOLEAN NOT NULL DEFAULT true,
    status                    TEXT NOT NULL DEFAULT 'active',
    created_at                TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at                TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (cruise_provider_id, cruise_provider_cruise_id)
);
CREATE INDEX tours_dates  ON cruise_provider_tours (begin_date, end_date);
CREATE INDEX tours_object ON cruise_provider_tours (cruise_provider_id, cruise_provider_object_id);

-- ============================================================
-- ЦЕНЫ (SCD2)
-- ============================================================

CREATE TABLE cruise_provider_prices (
    id                        BIGSERIAL PRIMARY KEY,
    cruise_provider_id        TEXT NOT NULL REFERENCES cruise_providers(id) ON DELETE CASCADE,
    cruise_provider_cruise_id TEXT NOT NULL,
    cruise_provider_class_id  TEXT NOT NULL,
    base_price                NUMERIC(12,2) NOT NULL,
    partial_buyout            BOOLEAN,
    child_price               NUMERIC(12,2),
    extra_seat                NUMERIC(12,2),
    currency                  TEXT NOT NULL DEFAULT 'RUB',
    valid_from                TIMESTAMPTZ NOT NULL DEFAULT now(),
    valid_to                  TIMESTAMPTZ,
    UNIQUE (cruise_provider_id, cruise_provider_cruise_id,
            cruise_provider_class_id, valid_from)
);
CREATE UNIQUE INDEX prices_current_uniq
    ON cruise_provider_prices (cruise_provider_id, cruise_provider_cruise_id, cruise_provider_class_id)
    WHERE valid_to IS NULL;
CREATE INDEX prices_tour
    ON cruise_provider_prices (cruise_provider_id, cruise_provider_cruise_id);

CREATE TABLE cruise_provider_sales (
    id                        BIGSERIAL PRIMARY KEY,
    cruise_provider_id        TEXT NOT NULL REFERENCES cruise_providers(id) ON DELETE CASCADE,
    cruise_provider_cruise_id TEXT NOT NULL,
    cruise_provider_class_id  TEXT NOT NULL,
    cruise_provider_room_id   TEXT NOT NULL,
    base_price                NUMERIC(12,2) NOT NULL,
    partial_buyout            BOOLEAN,
    currency                  TEXT NOT NULL DEFAULT 'RUB',
    valid_from                TIMESTAMPTZ NOT NULL DEFAULT now(),
    valid_to                  TIMESTAMPTZ,
    UNIQUE (cruise_provider_id, cruise_provider_cruise_id,
            cruise_provider_class_id, cruise_provider_room_id, valid_from)
);
CREATE UNIQUE INDEX sales_current_uniq
    ON cruise_provider_sales (cruise_provider_id, cruise_provider_cruise_id,
                              cruise_provider_class_id, cruise_provider_room_id)
    WHERE valid_to IS NULL;
CREATE INDEX sales_tour
    ON cruise_provider_sales (cruise_provider_id, cruise_provider_cruise_id);

-- ============================================================
-- ДОСТУПНОСТЬ
-- ============================================================

CREATE TABLE cruise_provider_availability (
    cruise_provider_id        TEXT NOT NULL REFERENCES cruise_providers(id) ON DELETE CASCADE,
    cruise_provider_cruise_id TEXT NOT NULL,
    cruise_provider_room_id   TEXT NOT NULL,
    available                 BOOLEAN NOT NULL,
    updated_at                TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (cruise_provider_id, cruise_provider_cruise_id, cruise_provider_room_id)
);
CREATE INDEX availability_tour
    ON cruise_provider_availability (cruise_provider_id, cruise_provider_cruise_id);

-- ============================================================
-- ИНФРАСТРУКТУРА
-- ============================================================

CREATE TABLE raw_snapshots (
    id                  BIGSERIAL PRIMARY KEY,
    cruise_provider_id  TEXT NOT NULL REFERENCES cruise_providers(id) ON DELETE CASCADE,
    sha256              TEXT NOT NULL,
    payload             BYTEA NOT NULL,
    payload_encoding    TEXT NOT NULL DEFAULT 'none',
    fetched_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (cruise_provider_id, sha256)
);
CREATE INDEX raw_snapshots_time
    ON raw_snapshots (cruise_provider_id, fetched_at DESC);

CREATE TABLE errors (
    id                  BIGSERIAL PRIMARY KEY,
    occurred_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    cruise_provider_id  TEXT,
    stage               TEXT NOT NULL,
    severity            TEXT NOT NULL,
    message             TEXT NOT NULL,
    context             JSONB,
    resolved            BOOLEAN NOT NULL DEFAULT false
);
CREATE INDEX errors_provider_time ON errors (cruise_provider_id, occurred_at DESC);
CREATE INDEX errors_severity_unresolved
    ON errors (severity, occurred_at DESC) WHERE resolved = false;

CREATE TABLE enrichment_rules (
    id                  BIGSERIAL PRIMARY KEY,
    cruise_provider_id  TEXT REFERENCES cruise_providers(id) ON DELETE CASCADE,
    field_name          TEXT NOT NULL,
    rule_type           TEXT NOT NULL,
    rule_config         JSONB NOT NULL,
    priority            INT NOT NULL DEFAULT 100,
    is_active           BOOLEAN NOT NULL DEFAULT true,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (cruise_provider_id, field_name)
);

CREATE TABLE sync_runs (
    id                  BIGSERIAL PRIMARY KEY,
    cruise_provider_id  TEXT NOT NULL REFERENCES cruise_providers(id) ON DELETE CASCADE,
    started_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at         TIMESTAMPTZ,
    status              TEXT NOT NULL DEFAULT 'running',
    raw_snapshot_id     BIGINT REFERENCES raw_snapshots(id),
    rows_read           INT,
    rows_written        INT,
    duration_ms         INT,
    error_message       TEXT
);
CREATE INDEX sync_runs_provider_time
    ON sync_runs (cruise_provider_id, started_at DESC);

CREATE TABLE health_checks (
    id          BIGSERIAL PRIMARY KEY,
    checked_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    component   TEXT NOT NULL,
    status      TEXT NOT NULL,
    latency_ms  INT,
    details     JSONB
);
CREATE INDEX health_checks_component_time
    ON health_checks (component, checked_at DESC);

-- ============================================================
-- VIEWS
-- ============================================================

CREATE VIEW cruise_provider_prices_current AS
SELECT * FROM cruise_provider_prices WHERE valid_to IS NULL;

CREATE VIEW cruise_provider_sales_current AS
SELECT * FROM cruise_provider_sales WHERE valid_to IS NULL;

CREATE VIEW cruise_provider_tours_view AS
SELECT
    t.*,
    COALESCE((
        SELECT count(*) FROM cruise_provider_availability a
        WHERE a.cruise_provider_id = t.cruise_provider_id
          AND a.cruise_provider_cruise_id = t.cruise_provider_cruise_id
          AND a.available = true
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

-- ============================================================
-- SEED
-- ============================================================

INSERT INTO cruise_providers (id, name, schedule)
VALUES ('1', 'Volga Wolga', '0 0 * * * *');

INSERT INTO enrichment_rules (cruise_provider_id, field_name, rule_type, rule_config) VALUES
    (NULL, 'site_name',             'template',     '{"template": "{name} Название для сайта"}'),
    (NULL, 'site_cruise_object_id', 'constant',     '{"value": "1"}'),
    (NULL, 'route',                 'template',     '{"template": "Маршрут круиза {cruise_id}"}'),
    (NULL, 'city_from',             'constant',     '{"value": "Пермь"}'),
    (NULL, 'city_to',               'constant',     '{"value": "Пермь"}'),
    (NULL, 'departure_city',        'constant',     '{"value": "Астрахань"}'),
    (NULL, 'days',                  'days_between', '{"plus": 1}'),
    (NULL, 'is_return',             'constant',     '{"value": "true"}'),
    (NULL, 'is_weekend',            'constant',     '{"value": "false"}');
