use super::log_db_error;
use crate::dal::dto::{
    LinkedMalItem, MalBgmMatch, MalBgmMatchExportRow, MalBgmMatchWithDetails, MatchSubjectSummary,
    UpdateMalBgmMatch,
};
use sqlx::{QueryBuilder, Sqlite, SqlitePool};
use std::collections::HashMap;

#[derive(sqlx::FromRow)]
struct MatchDetailRow {
    #[sqlx(flatten)]
    match_row: MalBgmMatch,
    title: String,
    title_ja: Option<String>,
    title_en: Option<String>,
    media_type: String,
    rating: Option<String>,
    start_date: Option<String>,
    end_date: Option<String>,
    num_episodes: Option<i32>,
    mal_source: Option<String>,
    studios: Option<String>,
    synopsis: Option<String>,
    subject_id: Option<i32>,
    subject_name: Option<String>,
    subject_name_cn: Option<String>,
    subject_rank: Option<i32>,
    subject_score: Option<f64>,
    subject_air_date: Option<String>,
    subject_platform: Option<String>,
    subject_nsfw: Option<bool>,
    subject_tags: Option<String>,
    subject_summary: Option<String>,
    subject_images_grid: Option<String>,
}

pub struct MalBgmMatchRepository<'a> {
    pool: &'a SqlitePool,
}

#[allow(dead_code)]
impl<'a> MalBgmMatchRepository<'a> {
    pub fn new(pool: &'a SqlitePool) -> Self {
        Self { pool }
    }

    /// Insert a default `pending` match row if `mal_id` does not already exist.
    /// Uses `ON CONFLICT (mal_id) DO NOTHING` so existing decisions are never overwritten
    /// while still enforcing the foreign key to `mal_anime(mal_id)`.
    pub async fn insert_pending_ignore(&self, mal_id: i32) -> Result<bool, sqlx::Error> {
        let result = sqlx::query(
            r#"
            INSERT INTO mal_bgm_matches (mal_id, status, updated_at)
            VALUES (?, 'pending', strftime('%Y-%m-%d %H:%M:%f', 'now'))
            ON CONFLICT (mal_id) DO NOTHING
            "#,
        )
        .bind(mal_id)
        .execute(self.pool)
        .await
        .inspect_err(|e| log_db_error("insert_pending_ignore", "mal_bgm_matches", e))?;

        Ok(result.rows_affected() > 0)
    }

    pub async fn get(&self, mal_id: i32) -> Result<Option<MalBgmMatch>, sqlx::Error> {
        let row =
            sqlx::query_as::<_, MalBgmMatch>("SELECT * FROM mal_bgm_matches WHERE mal_id = ?")
                .bind(mal_id)
                .fetch_optional(self.pool)
                .await
                .inspect_err(|e| log_db_error("get", "mal_bgm_matches", e))?;

        Ok(row)
    }

    pub async fn upsert(
        &self,
        mal_id: i32,
        data: UpdateMalBgmMatch,
    ) -> Result<MalBgmMatch, sqlx::Error> {
        let row = sqlx::query_as::<_, MalBgmMatch>(
            r#"
            INSERT INTO mal_bgm_matches (
                mal_id, status, source, bgm_id, confidence,
                candidates, reason, note, error, updated_at
            )
            VALUES (
                ?, ?, ?, ?, ?,
                ?, ?, ?, ?, strftime('%Y-%m-%d %H:%M:%f', 'now')
            )
            ON CONFLICT (mal_id) DO UPDATE SET
                status = EXCLUDED.status,
                source = EXCLUDED.source,
                bgm_id = EXCLUDED.bgm_id,
                confidence = EXCLUDED.confidence,
                candidates = EXCLUDED.candidates,
                reason = EXCLUDED.reason,
                note = EXCLUDED.note,
                error = EXCLUDED.error,
                updated_at = strftime('%Y-%m-%d %H:%M:%f', 'now')
            RETURNING *
            "#,
        )
        .bind(mal_id)
        .bind(data.status)
        .bind(data.source)
        .bind(data.bgm_id)
        .bind(data.confidence)
        .bind(data.candidates)
        .bind(data.reason)
        .bind(data.note)
        .bind(data.error)
        .fetch_one(self.pool)
        .await
        .inspect_err(|e| log_db_error("upsert", "mal_bgm_matches", e))?;

        Ok(row)
    }

    pub async fn update_decision(
        &self,
        mal_id: i32,
        data: UpdateMalBgmMatch,
    ) -> Result<Option<MalBgmMatch>, sqlx::Error> {
        let row = sqlx::query_as::<_, MalBgmMatch>(
            r#"
            UPDATE mal_bgm_matches
            SET
                status = ?,
                source = ?,
                bgm_id = ?,
                confidence = ?,
                candidates = ?,
                reason = ?,
                note = ?,
                error = ?,
                updated_at = strftime('%Y-%m-%d %H:%M:%f', 'now')
            WHERE mal_id = ?
            RETURNING *
            "#,
        )
        .bind(data.status)
        .bind(data.source)
        .bind(data.bgm_id)
        .bind(data.confidence)
        .bind(data.candidates)
        .bind(data.reason)
        .bind(data.note)
        .bind(data.error)
        .bind(mal_id)
        .fetch_optional(self.pool)
        .await
        .inspect_err(|e| log_db_error("update_decision", "mal_bgm_matches", e))?;

        Ok(row)
    }

    pub async fn list_by_status(&self, status: &str) -> Result<Vec<MalBgmMatch>, sqlx::Error> {
        let rows = sqlx::query_as::<_, MalBgmMatch>(
            "SELECT * FROM mal_bgm_matches WHERE status = ? ORDER BY mal_id ASC",
        )
        .bind(status)
        .fetch_all(self.pool)
        .await
        .inspect_err(|e| log_db_error("list_by_status", "mal_bgm_matches", e))?;

        Ok(rows)
    }

    pub async fn list_by_bgm_id(&self, bgm_id: i32) -> Result<Vec<MalBgmMatch>, sqlx::Error> {
        let rows = sqlx::query_as::<_, MalBgmMatch>(
            "SELECT * FROM mal_bgm_matches WHERE bgm_id = ? ORDER BY mal_id ASC",
        )
        .bind(bgm_id)
        .fetch_all(self.pool)
        .await
        .inspect_err(|e| log_db_error("list_by_bgm_id", "mal_bgm_matches", e))?;

        Ok(rows)
    }

    pub async fn delete(&self, mal_id: i32) -> Result<bool, sqlx::Error> {
        let result = sqlx::query("DELETE FROM mal_bgm_matches WHERE mal_id = ?")
            .bind(mal_id)
            .execute(self.pool)
            .await
            .inspect_err(|e| log_db_error("delete", "mal_bgm_matches", e))?;

        Ok(result.rows_affected() > 0)
    }

    pub async fn list_for_export(&self) -> Result<Vec<MalBgmMatchExportRow>, sqlx::Error> {
        let rows = sqlx::query_as::<_, MalBgmMatchExportRow>(
            r#"
            SELECT mal_id, status, source, bgm_id, confidence, reason, note
            FROM mal_bgm_matches
            ORDER BY mal_id ASC
            "#,
        )
        .fetch_all(self.pool)
        .await
        .inspect_err(|e| log_db_error("list_for_export", "mal_bgm_matches", e))?;

        Ok(rows)
    }

    pub async fn list_with_details(
        &self,
        season_id: Option<i32>,
        status: Option<&str>,
    ) -> Result<Vec<MalBgmMatchWithDetails>, sqlx::Error> {
        let mut qb = QueryBuilder::<Sqlite>::new(
            r#"
            SELECT
                m.mal_id,
                m.status,
                m.source,
                m.bgm_id,
                m.confidence,
                m.candidates,
                m.reason,
                m.note,
                m.error,
                m.updated_at,
                ma.title,
                ma.title_ja,
                ma.title_en,
                ma.media_type,
                ma.rating,
                ma.start_date,
                ma.end_date,
                ma.num_episodes,
                ma.source AS mal_source,
                ma.studios,
                ma.synopsis,
                s.id AS subject_id,
                s.name AS subject_name,
                s.name_cn AS subject_name_cn,
                s.rank AS subject_rank,
                s.score AS subject_score,
                s.air_date AS subject_air_date,
                s.platform AS subject_platform,
                s.nsfw AS subject_nsfw,
                s.tags AS subject_tags,
                s.summary AS subject_summary,
                s.images_grid AS subject_images_grid
            FROM mal_bgm_matches m
            JOIN mal_anime ma ON ma.mal_id = m.mal_id
            LEFT JOIN subjects s ON s.id = m.bgm_id
            "#,
        );

        if let Some(sid) = season_id {
            qb.push(" JOIN season_mal sm ON sm.mal_id = m.mal_id AND sm.season_id = ");
            qb.push_bind(sid);
            qb.push(" AND sm.is_removed = 0");
        }

        if let Some(st) = status {
            qb.push(" WHERE m.status = ");
            qb.push_bind(st);
        }

        qb.push(" ORDER BY m.mal_id ASC");

        let rows = qb
            .build_query_as::<MatchDetailRow>()
            .fetch_all(self.pool)
            .await
            .inspect_err(|e| log_db_error("list_with_details", "mal_bgm_matches", e))?;

        Ok(rows
            .into_iter()
            .map(|r| {
                let subject = r.subject_id.map(|id| MatchSubjectSummary {
                    id,
                    name: r.subject_name,
                    name_cn: r.subject_name_cn,
                    rank: r.subject_rank,
                    score: r.subject_score,
                    air_date: r.subject_air_date,
                    platform: r.subject_platform,
                    nsfw: r.subject_nsfw,
                    tags: r.subject_tags,
                    summary: r.subject_summary,
                    images_grid: r.subject_images_grid,
                });
                MalBgmMatchWithDetails {
                    match_row: r.match_row,
                    title: r.title,
                    title_ja: r.title_ja,
                    title_en: r.title_en,
                    media_type: r.media_type,
                    rating: r.rating,
                    start_date: r.start_date,
                    end_date: r.end_date,
                    num_episodes: r.num_episodes,
                    mal_source: r.mal_source,
                    studios: r.studios,
                    synopsis: r.synopsis,
                    subject,
                }
            })
            .collect())
    }

    pub async fn list_linked_by_bgm_ids(
        &self,
        bgm_ids: &[i32],
    ) -> Result<Vec<LinkedMalItem>, sqlx::Error> {
        if bgm_ids.is_empty() {
            return Ok(Vec::new());
        }

        #[derive(sqlx::FromRow)]
        struct LinkedRow {
            mal_id: i32,
            bgm_id: i32,
            title: String,
            title_ja: Option<String>,
            media_type: String,
            status: String,
            season_id: Option<i32>,
        }

        let mut qb = QueryBuilder::<Sqlite>::new(
            r#"
            SELECT
                m.mal_id,
                m.bgm_id,
                ma.title,
                ma.title_ja,
                ma.media_type,
                m.status,
                sm.season_id
            FROM mal_bgm_matches m
            JOIN mal_anime ma ON ma.mal_id = m.mal_id
            LEFT JOIN season_mal sm ON sm.mal_id = m.mal_id AND sm.is_removed = 0
            WHERE m.bgm_id IN (
            "#,
        );
        let mut separated = qb.separated(", ");
        for id in bgm_ids {
            separated.push_bind(id);
        }
        separated.push_unseparated(") ORDER BY m.mal_id ASC, sm.season_id ASC");

        let rows = qb
            .build_query_as::<LinkedRow>()
            .fetch_all(self.pool)
            .await
            .inspect_err(|e| log_db_error("list_linked_by_bgm_ids", "mal_bgm_matches", e))?;

        let mut map: HashMap<i32, LinkedMalItem> = HashMap::new();
        let mut order: Vec<i32> = Vec::new();
        for r in rows {
            let entry = map.entry(r.mal_id).or_insert_with(|| {
                order.push(r.mal_id);
                LinkedMalItem {
                    mal_id: r.mal_id,
                    bgm_id: r.bgm_id,
                    title: r.title,
                    title_ja: r.title_ja,
                    media_type: r.media_type,
                    status: r.status,
                    seasons: Vec::new(),
                }
            });
            if let Some(sid) = r.season_id
                && !entry.seasons.contains(&sid)
            {
                entry.seasons.push(sid);
            }
        }

        Ok(order
            .into_iter()
            .filter_map(|mal_id| map.remove(&mal_id))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dal::Database;
    use crate::dal::dto::{CreateSubject, UpsertMalAnime};
    use crate::dal::repositories::{MalAnimeRepository, SubjectRepository};

    async fn seed_mal(pool: &SqlitePool, mal_id: i32) -> sqlx::Result<()> {
        MalAnimeRepository::new(pool)
            .upsert(UpsertMalAnime {
                mal_id,
                title: format!("Anime {}", mal_id),
                media_type: "tv".to_string(),
                ..Default::default()
            })
            .await?;
        Ok(())
    }

    async fn seed_subject(pool: &SqlitePool, bgm_id: i32) -> sqlx::Result<()> {
        SubjectRepository::new(pool)
            .create(CreateSubject {
                id: bgm_id,
                name: Some(format!("Subject {}", bgm_id)),
                ..Default::default()
            })
            .await?;
        Ok(())
    }

    #[sqlx::test]
    async fn test_insert_pending_ignore_and_update_decision(pool: SqlitePool) -> sqlx::Result<()> {
        let db = Database::from_pool(pool);
        seed_mal(db.pool(), 101).await?;
        seed_subject(db.pool(), 5001).await?;

        let repo = MalBgmMatchRepository::new(db.pool());

        assert!(repo.insert_pending_ignore(101).await?);
        let pending = repo.get(101).await?.unwrap();
        assert_eq!(pending.status, "pending");
        assert!(pending.source.is_none());
        assert!(pending.bgm_id.is_none());

        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        let updated = repo
            .update_decision(
                101,
                UpdateMalBgmMatch {
                    status: "included".to_string(),
                    source: Some("exact".to_string()),
                    bgm_id: Some(5001),
                    confidence: Some(1.0),
                    candidates: Some("[]".to_string()),
                    note: Some("exact title".to_string()),
                    ..Default::default()
                },
            )
            .await?
            .expect("row should exist");
        assert_eq!(updated.status, "included");
        assert_eq!(updated.bgm_id, Some(5001));
        assert!(updated.updated_at > pending.updated_at);

        // insert_pending_ignore must not reset existing decision
        assert!(!repo.insert_pending_ignore(101).await?);
        let after = repo.get(101).await?.unwrap();
        assert_eq!(after.status, "included");
        assert_eq!(after.bgm_id, Some(5001));

        let included = repo.list_by_status("included").await?;
        assert_eq!(included.len(), 1);
        assert_eq!(included[0].mal_id, 101);

        Ok(())
    }

    #[sqlx::test]
    async fn test_multiple_mal_ids_to_same_bgm_id(pool: SqlitePool) -> sqlx::Result<()> {
        let db = Database::from_pool(pool);
        seed_mal(db.pool(), 1001).await?;
        seed_mal(db.pool(), 1002).await?;
        seed_subject(db.pool(), 515759).await?;

        let repo = MalBgmMatchRepository::new(db.pool());
        repo.upsert(
            1001,
            UpdateMalBgmMatch {
                status: "included".to_string(),
                source: Some("llm".to_string()),
                bgm_id: Some(515759),
                confidence: Some(0.95),
                ..Default::default()
            },
        )
        .await?;
        repo.upsert(
            1002,
            UpdateMalBgmMatch {
                status: "included".to_string(),
                source: Some("human".to_string()),
                bgm_id: Some(515759),
                confidence: Some(1.0),
                ..Default::default()
            },
        )
        .await?;

        let matches = repo.list_by_bgm_id(515759).await?;
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].mal_id, 1001);
        assert_eq!(matches[1].mal_id, 1002);

        Ok(())
    }

    #[sqlx::test]
    async fn test_bgm_id_set_null_and_match_reset_on_subject_delete(
        pool: SqlitePool,
    ) -> sqlx::Result<()> {
        let db = Database::from_pool(pool);
        seed_mal(db.pool(), 2001).await?;
        seed_mal(db.pool(), 2002).await?;
        seed_subject(db.pool(), 7777).await?;
        seed_subject(db.pool(), 7778).await?;

        let match_repo = MalBgmMatchRepository::new(db.pool());
        match_repo
            .upsert(
                2001,
                UpdateMalBgmMatch {
                    status: "included".to_string(),
                    source: Some("exact".to_string()),
                    bgm_id: Some(7777),
                    confidence: Some(1.0),
                    ..Default::default()
                },
            )
            .await?;

        assert!(SubjectRepository::new(db.pool()).delete(7777).await?);

        let after = match_repo
            .get(2001)
            .await?
            .expect("mal_bgm_matches row must survive subject deletion");
        assert_eq!(after.bgm_id, None);
        assert_eq!(after.status, "pending");
        assert_eq!(after.source, None);
        assert_eq!(after.confidence, None);

        // Raw DELETE on subjects triggers ON DELETE SET NULL for non-included rows
        match_repo
            .upsert(
                2002,
                UpdateMalBgmMatch {
                    status: "excluded".to_string(),
                    source: Some("human".to_string()),
                    bgm_id: Some(7778),
                    ..Default::default()
                },
            )
            .await?;
        sqlx::query("DELETE FROM subjects WHERE id = 7778")
            .execute(db.pool())
            .await?;
        let after_raw = match_repo.get(2002).await?.unwrap();
        assert_eq!(after_raw.bgm_id, None);
        assert_eq!(after_raw.status, "excluded");

        Ok(())
    }

    #[sqlx::test]
    async fn test_check_rejects_included_with_null_bgm_id(pool: SqlitePool) -> sqlx::Result<()> {
        let db = Database::from_pool(pool);
        seed_mal(db.pool(), 3001).await?;
        seed_subject(db.pool(), 8888).await?;

        let repo = MalBgmMatchRepository::new(db.pool());
        let err = repo
            .upsert(
                3001,
                UpdateMalBgmMatch {
                    status: "included".to_string(),
                    source: Some("human".to_string()),
                    bgm_id: None,
                    ..Default::default()
                },
            )
            .await
            .expect_err("CHECK constraint must reject included with NULL bgm_id");
        assert!(
            err.to_string().contains("CHECK constraint failed"),
            "unexpected error: {err}"
        );

        // Valid included row, then attempting raw UPDATE to bgm_id=NULL must fail CHECK
        repo.upsert(
            3001,
            UpdateMalBgmMatch {
                status: "included".to_string(),
                source: Some("human".to_string()),
                bgm_id: Some(8888),
                ..Default::default()
            },
        )
        .await?;

        let update_err =
            sqlx::query("UPDATE mal_bgm_matches SET bgm_id = NULL WHERE mal_id = 3001")
                .execute(db.pool())
                .await
                .expect_err(
                    "CHECK constraint must reject setting bgm_id=NULL while status='included'",
                );
        assert!(
            update_err.to_string().contains("CHECK constraint failed"),
            "unexpected error: {update_err}"
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_migration_003_repairs_violating_rows_and_recreates_indexes() -> sqlx::Result<()> {
        use sqlx::sqlite::SqliteConnectOptions;
        use std::str::FromStr;

        let opts = SqliteConnectOptions::from_str("sqlite::memory:")?.foreign_keys(true);
        let pool = SqlitePool::connect_with(opts).await?;

        // Apply 001 and 002 directly to simulate pre-003 DB state
        sqlx::raw_sql(include_str!("../../../migrations/001_create_schema.sql"))
            .execute(&pool)
            .await?;
        sqlx::raw_sql(include_str!("../../../migrations/002_mal_season_model.sql"))
            .execute(&pool)
            .await?;

        seed_mal(&pool, 4001).await?;
        seed_mal(&pool, 4002).await?;
        seed_subject(&pool, 9001).await?;

        // Insert violating row (status='included', bgm_id=NULL) allowed under 002
        sqlx::query(
            "INSERT INTO mal_bgm_matches (mal_id, status, source, bgm_id, confidence, note, updated_at)
             VALUES (4001, 'included', 'exact', NULL, 0.95, 'kept note', '2026-01-01 00:00:00.000')",
        )
        .execute(&pool)
        .await?;
        // Insert valid row
        sqlx::query(
            "INSERT INTO mal_bgm_matches (mal_id, status, source, bgm_id, confidence, updated_at)
             VALUES (4002, 'included', 'human', 9001, 1.0, '2026-01-01 00:00:00.000')",
        )
        .execute(&pool)
        .await?;

        // Run 003 inside a transaction (matching sqlx::migrate!)
        let mut tx = pool.begin().await?;
        sqlx::raw_sql(include_str!(
            "../../../migrations/003_mal_bgm_matches_check.sql"
        ))
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;

        let repo = MalBgmMatchRepository::new(&pool);
        let repaired = repo.get(4001).await?.unwrap();
        assert_eq!(repaired.status, "pending");
        assert_eq!(repaired.source, None);
        assert_eq!(repaired.confidence, None);
        assert_eq!(repaired.bgm_id, None);
        assert_eq!(repaired.note.as_deref(), Some("kept note"));

        let valid = repo.get(4002).await?.unwrap();
        assert_eq!(valid.status, "included");
        assert_eq!(valid.bgm_id, Some(9001));

        // Verify indexes exist
        let indexes: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name = 'mal_bgm_matches'",
        )
        .fetch_all(&pool)
        .await?;
        assert!(indexes.contains(&"idx_matches_status".to_string()));
        assert!(indexes.contains(&"idx_matches_bgm_id".to_string()));

        // Verify CHECK now rejects new violating row
        seed_mal(&pool, 4003).await?;
        let err = sqlx::query(
            "INSERT INTO mal_bgm_matches (mal_id, status, bgm_id, updated_at)
             VALUES (4003, 'included', NULL, '2026-01-01 00:00:00.000')",
        )
        .execute(&pool)
        .await
        .expect_err("CHECK must reject included with NULL bgm_id after migration 003");
        assert!(err.to_string().contains("CHECK constraint failed"));

        Ok(())
    }

    #[sqlx::test]
    async fn test_list_for_export_orders_by_mal_id_asc(pool: SqlitePool) -> sqlx::Result<()> {
        let db = Database::from_pool(pool);
        seed_mal(db.pool(), 300).await?;
        seed_mal(db.pool(), 100).await?;
        seed_subject(db.pool(), 5001).await?;

        let repo = MalBgmMatchRepository::new(db.pool());
        repo.upsert(
            300,
            UpdateMalBgmMatch {
                status: "excluded".to_string(),
                source: Some("human".to_string()),
                reason: Some("mini".to_string()),
                note: Some("short".to_string()),
                candidates: Some("[{\"bgm_id\":1}]".to_string()),
                error: Some("ignored".to_string()),
                ..Default::default()
            },
        )
        .await?;
        repo.upsert(
            100,
            UpdateMalBgmMatch {
                status: "included".to_string(),
                source: Some("exact".to_string()),
                bgm_id: Some(5001),
                confidence: Some(1.0),
                ..Default::default()
            },
        )
        .await?;

        let exported = repo.list_for_export().await?;
        assert_eq!(
            exported,
            vec![
                MalBgmMatchExportRow {
                    mal_id: 100,
                    status: "included".to_string(),
                    source: Some("exact".to_string()),
                    bgm_id: Some(5001),
                    confidence: Some(1.0),
                    reason: None,
                    note: None,
                },
                MalBgmMatchExportRow {
                    mal_id: 300,
                    status: "excluded".to_string(),
                    source: Some("human".to_string()),
                    bgm_id: None,
                    confidence: None,
                    reason: Some("mini".to_string()),
                    note: Some("short".to_string()),
                },
            ]
        );

        Ok(())
    }

    #[sqlx::test]
    async fn test_list_with_details_filters_and_joins(pool: SqlitePool) -> sqlx::Result<()> {
        use crate::dal::dto::CreateSeason;
        use crate::dal::repositories::{SeasonMalRepository, SeasonRepository};

        let db = Database::from_pool(pool);
        for (season_id, year, season) in [(202601, 2026, "WINTER"), (202604, 2026, "SPRING")] {
            SeasonRepository::new(db.pool())
                .upsert(CreateSeason {
                    season_id,
                    year,
                    season: season.to_string(),
                    name: None,
                })
                .await?;
        }

        MalAnimeRepository::new(db.pool())
            .upsert(UpsertMalAnime {
                mal_id: 10,
                title: "Frieren 2".to_string(),
                title_ja: Some("葬送のフリーレン 2期".to_string()),
                media_type: "tv".to_string(),
                rating: Some("pg_13".to_string()),
                ..Default::default()
            })
            .await?;
        MalAnimeRepository::new(db.pool())
            .upsert(UpsertMalAnime {
                mal_id: 20,
                title: "Removed Show".to_string(),
                title_ja: Some("削除されたアニメ".to_string()),
                media_type: "ona".to_string(),
                rating: Some("g".to_string()),
                ..Default::default()
            })
            .await?;
        MalAnimeRepository::new(db.pool())
            .upsert(UpsertMalAnime {
                mal_id: 30,
                title: "Spring Pending".to_string(),
                title_ja: None,
                media_type: "movie".to_string(),
                rating: None,
                ..Default::default()
            })
            .await?;

        SubjectRepository::new(db.pool())
            .create(CreateSubject {
                id: 515759,
                name: Some("葬送のフリーレン 第2期".to_string()),
                name_cn: Some("葬送的芙莉莲 第二季".to_string()),
                rank: Some(5),
                score: Some(8.9),
                ..Default::default()
            })
            .await?;

        let sm_repo = SeasonMalRepository::new(db.pool());
        sm_repo.insert_ignore(202601, 10).await?;
        sm_repo.insert_ignore(202601, 20).await?;
        sm_repo.set_removed(202601, 20, true, None).await?;
        sm_repo.insert_ignore(202604, 30).await?;

        let repo = MalBgmMatchRepository::new(db.pool());
        repo.upsert(
            10,
            UpdateMalBgmMatch {
                status: "included".to_string(),
                source: Some("human".to_string()),
                bgm_id: Some(515759),
                confidence: Some(1.0),
                ..Default::default()
            },
        )
        .await?;
        repo.upsert(
            20,
            UpdateMalBgmMatch {
                status: "pending".to_string(),
                ..Default::default()
            },
        )
        .await?;
        repo.upsert(
            30,
            UpdateMalBgmMatch {
                status: "pending".to_string(),
                ..Default::default()
            },
        )
        .await?;

        // Unfiltered returns all 3 ordered by mal_id ASC
        let all = repo.list_with_details(None, None).await?;
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].match_row.mal_id, 10);
        assert_eq!(all[0].title, "Frieren 2");
        assert_eq!(all[0].title_ja.as_deref(), Some("葬送のフリーレン 2期"));
        assert_eq!(all[0].media_type, "tv");
        assert_eq!(all[0].rating.as_deref(), Some("pg_13"));
        assert_eq!(
            all[0].subject,
            Some(MatchSubjectSummary {
                id: 515759,
                name: Some("葬送のフリーレン 第2期".to_string()),
                name_cn: Some("葬送的芙莉莲 第二季".to_string()),
                rank: Some(5),
                score: Some(8.9),
                ..Default::default()
            })
        );
        assert_eq!(all[1].match_row.mal_id, 20);
        assert!(all[1].subject.is_none());

        // Filter by season_id=202601 excludes mal_id=20 (is_removed=1) and mal_id=30 (202604)
        let s202601 = repo.list_with_details(Some(202601), None).await?;
        assert_eq!(s202601.len(), 1);
        assert_eq!(s202601[0].match_row.mal_id, 10);

        // Filter by status=pending returns 20 and 30
        let pending = repo.list_with_details(None, Some("pending")).await?;
        assert_eq!(
            pending
                .iter()
                .map(|r| r.match_row.mal_id)
                .collect::<Vec<_>>(),
            vec![20, 30]
        );

        // Combined season_id=202604 + status=pending returns only 30
        let combined = repo
            .list_with_details(Some(202604), Some("pending"))
            .await?;
        assert_eq!(combined.len(), 1);
        assert_eq!(combined[0].match_row.mal_id, 30);
        assert_eq!(combined[0].title_ja, None);
        assert_eq!(combined[0].rating, None);
        assert!(combined[0].subject.is_none());

        Ok(())
    }
}
