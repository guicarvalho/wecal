ALTER TABLE wecal_connections DROP CONSTRAINT wecal_connections_provider_key;
ALTER TABLE wecal_connections ADD COLUMN google_calendar_id TEXT;
CREATE UNIQUE INDEX wecal_connections_google_calendar ON wecal_connections (google_calendar_id) WHERE provider = 'google';
CREATE UNIQUE INDEX wecal_connections_apple ON wecal_connections (provider) WHERE provider = 'apple_ics';
ALTER TABLE wecal_events ADD COLUMN connection_id UUID REFERENCES wecal_connections(id) ON DELETE CASCADE;
UPDATE wecal_events SET connection_id = (SELECT id FROM wecal_connections WHERE provider = 'google')
WHERE provider = 'google' AND external_id IS NOT NULL;
UPDATE wecal_events SET external_id = connection_id::text || '::' || external_id
WHERE provider = 'google' AND connection_id IS NOT NULL AND external_id IS NOT NULL;
