CREATE TABLE mal_anime (
    mal_id            INTEGER PRIMARY KEY,
    title             TEXT NOT NULL,
    title_ja          TEXT,
    title_en          TEXT,
    media_type        TEXT NOT NULL,
    rating            TEXT,
    start_date        TEXT,
    end_date          TEXT,
    num_episodes      INTEGER,
    source            TEXT,
    studios           TEXT,
    synopsis          TEXT,
    start_season_year INTEGER,
    start_season      TEXT,
    fetched_at        DATETIME NOT NULL
);

CREATE TABLE season_mal (
    season_id   INTEGER NOT NULL REFERENCES seasons(season_id) ON DELETE CASCADE,
    mal_id      INTEGER NOT NULL REFERENCES mal_anime(mal_id),
    origin      TEXT NOT NULL DEFAULT 'mal',
    is_removed  BOOLEAN NOT NULL DEFAULT 0,
    note        TEXT,
    created_at  DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (season_id, mal_id)
);

CREATE INDEX idx_season_mal_active ON season_mal(season_id, is_removed);

CREATE TABLE mal_bgm_matches (
    mal_id      INTEGER PRIMARY KEY REFERENCES mal_anime(mal_id),
    status      TEXT NOT NULL DEFAULT 'pending' CHECK(status IN ('pending', 'included', 'excluded')),
    source      TEXT CHECK(source IN ('rule', 'exact', 'llm', 'human') OR source IS NULL),
    bgm_id      INTEGER REFERENCES subjects(id) ON DELETE SET NULL,
    confidence  REAL,
    candidates  TEXT,
    reason      TEXT,
    note        TEXT,
    error       TEXT,
    updated_at  DATETIME NOT NULL
);

CREATE INDEX idx_matches_status ON mal_bgm_matches(status);
CREATE INDEX idx_matches_bgm_id ON mal_bgm_matches(bgm_id) WHERE bgm_id IS NOT NULL;

ALTER TABLE subjects ADD COLUMN air_date TEXT;
ALTER TABLE subjects ADD COLUMN platform TEXT;
ALTER TABLE subjects ADD COLUMN summary TEXT;
ALTER TABLE subjects ADD COLUMN tags TEXT;
ALTER TABLE subjects ADD COLUMN nsfw BOOLEAN;

ALTER TABLE subjects DROP COLUMN media_type;
ALTER TABLE subjects DROP COLUMN rating;

DROP INDEX IF EXISTS idx_season_subjects_season;
DROP INDEX IF EXISTS idx_season_subjects_subject;
DROP TABLE IF EXISTS season_subjects;
