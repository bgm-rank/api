-- Revert float precision upgrade: restore columns to legacy DECIMAL types.
-- WARNING: this will truncate any precision beyond 2 decimal places.
ALTER TABLE subjects
    ALTER COLUMN score TYPE DECIMAL(4, 2) USING score::DECIMAL(4, 2),
    ALTER COLUMN average_comment TYPE DECIMAL(4, 2) USING average_comment::DECIMAL(4, 2),
    ALTER COLUMN drop_rate TYPE DECIMAL(5, 2) USING drop_rate::DECIMAL(5, 2);
