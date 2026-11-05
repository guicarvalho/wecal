WITH numbered AS (
    SELECT source_filename, row_number() OVER (ORDER BY source_filename) - 1 AS idx
    FROM wecal_import_calendars
), palette AS (
    SELECT source_filename,
           '#' || lpad(to_hex((128 + idx % 128)::integer), 2, '0')
               || lpad(to_hex((128 + (idx / 128) % 128)::integer), 2, '0')
               || lpad(to_hex((128 + (idx / 16384) % 128)::integer), 2, '0') AS color
    FROM numbered
)
UPDATE wecal_import_calendars calendars
SET calendar_color = palette.color
FROM palette
WHERE calendars.source_filename = palette.source_filename;
