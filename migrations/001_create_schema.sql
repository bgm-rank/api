CREATE TABLE IF NOT EXISTS seasons (
    season_id INTEGER PRIMARY KEY,
    year      INTEGER NOT NULL,
    season    TEXT NOT NULL,
    name      TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%d %H:%M:%f', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%d %H:%M:%f', 'now'))
);

CREATE INDEX IF NOT EXISTS idx_seasons_year_season ON seasons(year, season);

CREATE TABLE IF NOT EXISTS subjects (
    id               INTEGER PRIMARY KEY,
    name             TEXT,
    name_cn          TEXT,
    images_grid      TEXT,
    images_large     TEXT,
    rank             INTEGER,
    score            REAL,
    collection_total INTEGER,
    average_comment  REAL,
    drop_rate        REAL,
    air_weekday      TEXT,
    meta_tags        TEXT NOT NULL DEFAULT '[]',
    media_type       TEXT,
    rating           TEXT,
    updated_at       TEXT NOT NULL DEFAULT (strftime('%Y-%m-%d %H:%M:%f', 'now')),
    last_updated_at  TEXT
);

CREATE INDEX IF NOT EXISTS idx_subjects_rank  ON subjects(rank);
CREATE INDEX IF NOT EXISTS idx_subjects_score ON subjects(score DESC);

CREATE TABLE IF NOT EXISTS season_subjects (
    season_id  INTEGER NOT NULL REFERENCES seasons(season_id) ON DELETE CASCADE,
    subject_id INTEGER NOT NULL REFERENCES subjects(id) ON DELETE CASCADE,
    added_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%d %H:%M:%f', 'now')),
    PRIMARY KEY (season_id, subject_id)
);

CREATE INDEX IF NOT EXISTS idx_season_subjects_season  ON season_subjects(season_id);
CREATE INDEX IF NOT EXISTS idx_season_subjects_subject ON season_subjects(subject_id);
