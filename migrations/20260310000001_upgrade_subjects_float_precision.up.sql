-- Ensure score / average_comment / drop_rate are stored as FLOAT8 (DOUBLE PRECISION).
-- This migration is idempotent: columns were already converted to DOUBLE PRECISION
-- by a prior migration (20251225070515). Running ALTER TYPE to the same type is safe.
ALTER TABLE subjects
    ALTER COLUMN score TYPE FLOAT8,
    ALTER COLUMN average_comment TYPE FLOAT8,
    ALTER COLUMN drop_rate TYPE FLOAT8;
