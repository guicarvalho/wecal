CREATE TEMP TABLE wecal_import_color_palette (
    source_filename TEXT PRIMARY KEY,
    calendar_color TEXT NOT NULL,
    red INTEGER NOT NULL,
    green INTEGER NOT NULL,
    blue INTEGER NOT NULL
) ON COMMIT DROP;

DO $$
DECLARE
    calendar RECORD;
    candidate_red INTEGER;
    candidate_green INTEGER;
    candidate_blue INTEGER;
    color TEXT;
    found_color BOOLEAN;
BEGIN
    FOR calendar IN
        SELECT source_filename FROM wecal_import_calendars ORDER BY source_filename
    LOOP
        found_color := FALSE;
        FOR attempt IN 1..100000 LOOP
            candidate_red := 128 + floor(random() * 128)::INTEGER;
            candidate_green := 128 + floor(random() * 128)::INTEGER;
            candidate_blue := 128 + floor(random() * 128)::INTEGER;
            IF (candidate_red - 139) * (candidate_red - 139) + (candidate_green - 92) * (candidate_green - 92) + (candidate_blue - 246) * (candidate_blue - 246) >= 10000
               AND (candidate_red - 99) * (candidate_red - 99) + (candidate_green - 102) * (candidate_green - 102) + (candidate_blue - 241) * (candidate_blue - 241) >= 10000
               AND (candidate_red - 100) * (candidate_red - 100) + (candidate_green - 116) * (candidate_green - 116) + (candidate_blue - 139) * (candidate_blue - 139) >= 10000
               AND NOT EXISTS (
                   SELECT 1 FROM wecal_import_color_palette existing
                   WHERE (candidate_red - existing.red) * (candidate_red - existing.red)
                       + (candidate_green - existing.green) * (candidate_green - existing.green)
                       + (candidate_blue - existing.blue) * (candidate_blue - existing.blue) < 10000
               ) THEN
                color := '#' || lpad(to_hex(candidate_red), 2, '0') || lpad(to_hex(candidate_green), 2, '0') || lpad(to_hex(candidate_blue), 2, '0');
                INSERT INTO wecal_import_color_palette VALUES (calendar.source_filename, color, candidate_red, candidate_green, candidate_blue);
                found_color := TRUE;
                EXIT;
            END IF;
        END LOOP;
        IF NOT found_color THEN
            RAISE EXCEPTION 'could not allocate a bright, distinct color for imported calendar %', calendar.source_filename;
        END IF;
    END LOOP;

    UPDATE wecal_import_calendars calendars
    SET calendar_color = palette.calendar_color
    FROM wecal_import_color_palette palette
    WHERE calendars.source_filename = palette.source_filename;
END $$;
