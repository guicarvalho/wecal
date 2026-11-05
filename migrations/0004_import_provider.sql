-- A importação manual de arquivos .ics/.zip usa provider = 'import'.
-- Estende o CHECK constraint do 0001_init (arquivo sagrado, não alterado).
ALTER TABLE wecal_events DROP CONSTRAINT IF EXISTS wecal_events_provider_check;
ALTER TABLE wecal_events
    ADD CONSTRAINT wecal_events_provider_check
    CHECK (provider IN ('google', 'apple', 'import'));
