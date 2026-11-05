-- WeCal: conexões reais de calendário (OAuth Google + link ICS Apple)
CREATE TABLE IF NOT EXISTS wecal_connections (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    provider      TEXT NOT NULL UNIQUE CHECK (provider IN ('google', 'apple_ics')),
    -- Google OAuth
    access_token  TEXT,
    refresh_token TEXT,
    expires_at    TIMESTAMPTZ,
    -- Apple/iCloud: URL pública do feed .ics
    ics_url       TEXT,
    calendar_name TEXT,
    last_sync_at  TIMESTAMPTZ,
    last_error    TEXT,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Eventos vindos de APIs externas ganham ID do provedor (upsert na sync)
-- e carimbo de sincronização (para expulsar eventos removidos no provedor).
ALTER TABLE wecal_events ADD COLUMN IF NOT EXISTS external_id TEXT;
ALTER TABLE wecal_events ADD COLUMN IF NOT EXISTS synced_at  TIMESTAMPTZ;

-- Único por provedor; múltiplos NULL (eventos seed/mock) são permitidos no Postgres.
CREATE UNIQUE INDEX IF NOT EXISTS uq_wecal_events_provider_external
    ON wecal_events (provider, external_id);
