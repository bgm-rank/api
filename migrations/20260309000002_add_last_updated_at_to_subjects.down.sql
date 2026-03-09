DROP INDEX IF EXISTS idx_subjects_last_updated_at;
ALTER TABLE subjects DROP COLUMN last_updated_at;
