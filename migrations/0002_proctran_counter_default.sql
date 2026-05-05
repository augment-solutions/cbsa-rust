CREATE SEQUENCE IF NOT EXISTS proctran_counter_seq;

ALTER TABLE proctran
    ALTER COLUMN counter SET DEFAULT nextval('proctran_counter_seq');

-- Bump the sequence past any rows already present in proctran so that the
-- DEFAULT cannot collide with existing primary keys when this migration is
-- applied to a non-empty database. setval(..., is_called=false) makes the
-- next nextval() return the supplied value verbatim, so on an empty table
-- the first insert still receives counter=1.
SELECT setval(
    'proctran_counter_seq',
    GREATEST(1, COALESCE((SELECT max(counter) FROM proctran), 0) + 1),
    false
);
