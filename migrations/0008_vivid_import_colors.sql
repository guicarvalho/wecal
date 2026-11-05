WITH numbered AS (
    SELECT source_filename, row_number() OVER (ORDER BY source_filename) - 1 AS idx
    FROM wecal_import_calendars
), palette AS (
    SELECT source_filename,
           'hsl(' || ((1310 + idx * 13751) % 36000) / 100 || '.'
               || lpad((((1310 + idx * 13751) % 36000) % 100)::text, 2, '0')
               || 'deg, 78%, 56%)' AS color
    FROM numbered
)
UPDATE wecal_import_calendars calendars
SET calendar_color = palette.color
FROM palette
WHERE calendars.source_filename = palette.source_filename;
