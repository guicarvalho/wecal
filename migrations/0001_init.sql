-- WeCal: tabela central de eventos (providers Google e Apple)
CREATE TABLE IF NOT EXISTS wecal_events (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    provider    TEXT NOT NULL CHECK (provider IN ('google', 'apple')),
    title       TEXT NOT NULL,
    description TEXT,
    start_at    TIMESTAMPTZ NOT NULL,
    end_at      TIMESTAMPTZ NOT NULL CHECK (end_at >= start_at),
    is_private  BOOLEAN NOT NULL DEFAULT false,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_wecal_events_range
    ON wecal_events (start_at, end_at);

CREATE INDEX IF NOT EXISTS idx_wecal_events_provider
    ON wecal_events (provider);
