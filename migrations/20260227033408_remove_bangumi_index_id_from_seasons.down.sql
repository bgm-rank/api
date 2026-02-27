-- Add down migration script here
ALTER TABLE seasons ADD COLUMN bangumi_index_id INTEGER NOT NULL DEFAULT 0;