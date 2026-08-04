-- Image lifecycle (design M14b): import images through the platform, not just
-- register a pre-placed file by path.
--
-- `status` tracks an async import (importing -> ready | failed). `managed` marks
-- images whose backing file the platform created (so delete removes it; a
-- registered-by-path image's file is left alone). Existing images default to a
-- ready, unmanaged, registered image — unchanged behaviour.

ALTER TABLE images
    ADD COLUMN status     TEXT NOT NULL DEFAULT 'ready',   -- ready | importing | failed
    ADD COLUMN managed    BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN size_bytes BIGINT,
    ADD COLUMN error      TEXT;
