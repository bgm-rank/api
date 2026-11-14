CREATE TABLE mal_bgm_matches_new (
    mal_id      INTEGER PRIMARY KEY REFERENCES mal_anime(mal_id),
    status      TEXT NOT NULL DEFAULT 'pending' CHECK(status IN ('pending', 'included', 'excluded')),
    source      TEXT CHECK(source IN ('rule', 'exact', 'llm', 'human') OR source IS NULL),
    bgm_id      INTEGER REFERENCES subjects(id) ON DELETE SET NULL,
    confidence  REAL,
    candidates  TEXT,
    reason      TEXT,
    note        TEXT,
    error       TEXT,
    updated_at  DATETIME NOT NULL,
    CHECK (status != 'included' OR bgm_id IS NOT NULL)
);

INSERT INTO mal_bgm_matches_new (
    mal_id,
    status,
    source,
    bgm_id,
    confidence,
    candidates,
    reason,
    note,
    error,
    updated_at
)
SELECT
    mal_id,
    CASE
        WHEN status = 'included' AND bgm_id IS NULL THEN 'pending'
        ELSE status
    END,
    CASE
        WHEN status = 'included' AND bgm_id IS NULL THEN NULL
        ELSE source
    END,
    bgm_id,
    CASE
        WHEN status = 'included' AND bgm_id IS NULL THEN NULL
        ELSE confidence
    END,
    candidates,
    reason,
    note,
    error,
    updated_at
FROM mal_bgm_matches;

DROP TABLE mal_bgm_matches;

ALTER TABLE mal_bgm_matches_new RENAME TO mal_bgm_matches;

CREATE INDEX idx_matches_status ON mal_bgm_matches(status);
CREATE INDEX idx_matches_bgm_id ON mal_bgm_matches(bgm_id) WHERE bgm_id IS NOT NULL;
