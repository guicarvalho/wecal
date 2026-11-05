CREATE TABLE wecal_import_calendars (
    source_filename TEXT PRIMARY KEY,
    calendar_name TEXT NOT NULL,
    calendar_color TEXT NOT NULL UNIQUE
);

INSERT INTO wecal_import_calendars (source_filename, calendar_name, calendar_color)
SELECT source_filename,
       regexp_replace(split_part(source_filename, '/', array_length(string_to_array(source_filename, '/'), 1)), '\.[^.]+$', ''),
       '#' || lpad(to_hex(((idx - 1) % 112)::integer), 2, '0')
           || lpad(to_hex((((idx - 1) / 112) % 112)::integer), 2, '0')
           || lpad(to_hex((((idx - 1) / 12544) % 112)::integer), 2, '0')
FROM (
    SELECT source_filename, row_number() OVER (ORDER BY source_filename) AS idx
    FROM (SELECT DISTINCT source_filename FROM wecal_events WHERE provider = 'import' AND source_filename IS NOT NULL) files
) imported;
