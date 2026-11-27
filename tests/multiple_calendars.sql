-- Execute against a disposable database after migrations.
BEGIN;
DO $$
DECLARE first_id uuid; second_id uuid;
BEGIN
  INSERT INTO wecal_connections (provider, google_calendar_id) VALUES ('google', 'test-one@example.invalid') RETURNING id INTO first_id;
  INSERT INTO wecal_connections (provider, google_calendar_id) VALUES ('google', 'test-two@example.invalid') RETURNING id INTO second_id;
  INSERT INTO wecal_events (provider, external_id, title, start_at, end_at, connection_id)
    VALUES ('google', first_id::text || '::same', 'first', now(), now() + interval '1 hour', first_id),
           ('google', second_id::text || '::same', 'second', now(), now() + interval '1 hour', second_id);
  DELETE FROM wecal_connections WHERE id = first_id;
  IF EXISTS (SELECT 1 FROM wecal_events WHERE connection_id = first_id)
     OR NOT EXISTS (SELECT 1 FROM wecal_events WHERE connection_id = second_id) THEN
    RAISE EXCEPTION 'Google events were not isolated by connection';
  END IF;
END $$;
ROLLBACK;
