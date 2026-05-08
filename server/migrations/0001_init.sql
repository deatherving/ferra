CREATE TABLE IF NOT EXISTS kv_configs (
    key        text PRIMARY KEY,
    value      jsonb       NOT NULL,
    event_id   bigint      NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_kv_configs_prefix
    ON kv_configs (key text_pattern_ops);

CREATE TABLE IF NOT EXISTS kv_events (
    id         bigserial PRIMARY KEY,
    key        text        NOT NULL,
    operation  text        NOT NULL,
    value      jsonb,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_kv_events_id
    ON kv_events (id);

CREATE INDEX IF NOT EXISTS idx_kv_events_key
    ON kv_events (key);
