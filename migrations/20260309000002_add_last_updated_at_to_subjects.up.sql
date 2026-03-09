ALTER TABLE subjects ADD COLUMN last_updated_at TIMESTAMPTZ;
CREATE INDEX idx_subjects_last_updated_at ON subjects (last_updated_at);
