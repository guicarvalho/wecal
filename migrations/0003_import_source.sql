-- WeCal: coluna para rastrear origem do arquivo importado
ALTER TABLE wecal_events ADD COLUMN IF NOT EXISTS source_filename TEXT;

-- Índice para buscar por arquivo de origem
CREATE INDEX IF NOT EXISTS idx_wecal_events_source_filename
    ON wecal_events (source_filename);