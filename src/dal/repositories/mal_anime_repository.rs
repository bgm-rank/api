use super::log_db_error;
use crate::dal::dto::{MalAnime, UpsertMalAnime};
use sqlx::SqlitePool;

pub struct MalAnimeRepository<'a> {
    pool: &'a SqlitePool,
}

#[allow(dead_code)]
impl<'a> MalAnimeRepository<'a> {
    pub fn new(pool: &'a SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn upsert(&self, anime: UpsertMalAnime) -> Result<MalAnime, sqlx::Error> {
        let row = sqlx::query_as::<_, MalAnime>(
            r#"
            INSERT INTO mal_anime (
                mal_id, title, title_ja, title_en, media_type, rating,
                start_date, end_date, num_episodes, source, studios,
                synopsis, start_season_year, start_season, fetched_at
            )
            VALUES (
                ?, ?, ?, ?, ?, ?,
                ?, ?, ?, ?, ?,
                ?, ?, ?, strftime('%Y-%m-%d %H:%M:%f', 'now')
            )
            ON CONFLICT (mal_id) DO UPDATE SET
                title = EXCLUDED.title,
                title_ja = EXCLUDED.title_ja,
                title_en = EXCLUDED.title_en,
                media_type = EXCLUDED.media_type,
                rating = EXCLUDED.rating,
                start_date = EXCLUDED.start_date,
                end_date = EXCLUDED.end_date,
                num_episodes = EXCLUDED.num_episodes,
                source = EXCLUDED.source,
                studios = EXCLUDED.studios,
                synopsis = EXCLUDED.synopsis,
                start_season_year = EXCLUDED.start_season_year,
                start_season = EXCLUDED.start_season,
                fetched_at = strftime('%Y-%m-%d %H:%M:%f', 'now')
            RETURNING *
            "#,
        )
        .bind(anime.mal_id)
        .bind(anime.title)
        .bind(anime.title_ja)
        .bind(anime.title_en)
        .bind(anime.media_type)
        .bind(anime.rating)
        .bind(anime.start_date)
        .bind(anime.end_date)
        .bind(anime.num_episodes)
        .bind(anime.source)
        .bind(anime.studios)
        .bind(anime.synopsis)
        .bind(anime.start_season_year)
        .bind(anime.start_season)
        .fetch_one(self.pool)
        .await
        .inspect_err(|e| log_db_error("upsert", "mal_anime", e))?;

        Ok(row)
    }

    pub async fn get(&self, mal_id: i32) -> Result<Option<MalAnime>, sqlx::Error> {
        let row = sqlx::query_as::<_, MalAnime>("SELECT * FROM mal_anime WHERE mal_id = ?")
            .bind(mal_id)
            .fetch_optional(self.pool)
            .await
            .inspect_err(|e| log_db_error("get", "mal_anime", e))?;

        Ok(row)
    }

    pub async fn find_by_id(&self, mal_id: i32) -> Result<Option<MalAnime>, sqlx::Error> {
        self.get(mal_id).await
    }

    pub async fn find_by_ids(&self, mal_ids: &[i32]) -> Result<Vec<MalAnime>, sqlx::Error> {
        if mal_ids.is_empty() {
            return Ok(vec![]);
        }
        let ids_json = serde_json::to_string(mal_ids).unwrap_or_default();
        let rows = sqlx::query_as::<_, MalAnime>(
            "SELECT * FROM mal_anime WHERE mal_id IN (SELECT value FROM json_each(?)) ORDER BY mal_id ASC",
        )
        .bind(ids_json)
        .fetch_all(self.pool)
        .await
        .inspect_err(|e| log_db_error("find_by_ids", "mal_anime", e))?;

        Ok(rows)
    }

    pub async fn delete(&self, mal_id: i32) -> Result<bool, sqlx::Error> {
        let result = sqlx::query("DELETE FROM mal_anime WHERE mal_id = ?")
            .bind(mal_id)
            .execute(self.pool)
            .await
            .inspect_err(|e| log_db_error("delete", "mal_anime", e))?;

        Ok(result.rows_affected() > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dal::Database;
    use crate::dal::dto::{CreateSubject, UpdateMalBgmMatch};
    use crate::dal::repositories::{MalBgmMatchRepository, SubjectRepository};

    #[sqlx::test]
    async fn test_upsert_updates_mal_fields_and_leaves_matches_untouched(
        pool: SqlitePool,
    ) -> sqlx::Result<()> {
        let db = Database::from_pool(pool);
        let mal_repo = MalAnimeRepository::new(db.pool());
        let subject_repo = SubjectRepository::new(db.pool());
        let match_repo = MalBgmMatchRepository::new(db.pool());

        subject_repo
            .create(CreateSubject {
                id: 515759,
                name: Some("葬送のフリーレン 第2期".to_string()),
                ..Default::default()
            })
            .await?;

        let inserted = mal_repo
            .upsert(UpsertMalAnime {
                mal_id: 59978,
                title: "Sousou no Frieren 2nd Season".to_string(),
                title_ja: Some("葬送のフリーレン 第2期".to_string()),
                title_en: Some("Frieren: Beyond Journey's End Season 2".to_string()),
                media_type: "tv".to_string(),
                rating: Some("pg_13".to_string()),
                start_date: Some("2026-01-16".to_string()),
                num_episodes: Some(10),
                source: Some("manga".to_string()),
                studios: Some(r#"["Madhouse"]"#.to_string()),
                synopsis: Some("Initial synopsis".to_string()),
                start_season_year: Some(2026),
                start_season: Some("winter".to_string()),
                ..Default::default()
            })
            .await?;

        assert_eq!(inserted.mal_id, 59978);
        assert_eq!(inserted.media_type, "tv");

        match_repo
            .upsert(
                59978,
                UpdateMalBgmMatch {
                    status: "included".to_string(),
                    source: Some("human".to_string()),
                    bgm_id: Some(515759),
                    confidence: Some(1.0),
                    note: Some("manually verified".to_string()),
                    ..Default::default()
                },
            )
            .await?;

        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        let updated = mal_repo
            .upsert(UpsertMalAnime {
                mal_id: 59978,
                title: "Sousou no Frieren S2 (Updated)".to_string(),
                title_ja: Some("葬送のフリーレン 第2期".to_string()),
                title_en: None,
                media_type: "ona".to_string(),
                rating: Some("r".to_string()),
                start_date: Some("2026-01-16".to_string()),
                end_date: Some("2026-03-27".to_string()),
                num_episodes: Some(12),
                source: Some("manga".to_string()),
                studios: Some(r#"["Madhouse"]"#.to_string()),
                synopsis: Some("Updated synopsis".to_string()),
                start_season_year: Some(2026),
                start_season: Some("winter".to_string()),
            })
            .await?;

        assert_eq!(updated.title, "Sousou no Frieren S2 (Updated)");
        assert_eq!(updated.title_en, None);
        assert_eq!(updated.media_type, "ona");
        assert_eq!(updated.rating, Some("r".to_string()));
        assert_eq!(updated.end_date, Some("2026-03-27".to_string()));
        assert_eq!(updated.num_episodes, Some(12));
        assert!(updated.fetched_at > inserted.fetched_at);

        let m = match_repo
            .get(59978)
            .await?
            .expect("match row should still exist");
        assert_eq!(m.status, "included");
        assert_eq!(m.source, Some("human".to_string()));
        assert_eq!(m.bgm_id, Some(515759));
        assert_eq!(m.note, Some("manually verified".to_string()));

        Ok(())
    }

    #[sqlx::test]
    async fn test_get_and_find_by_ids(pool: SqlitePool) -> sqlx::Result<()> {
        let db = Database::from_pool(pool);
        let repo = MalAnimeRepository::new(db.pool());

        assert!(repo.get(1).await?.is_none());

        repo.upsert(UpsertMalAnime {
            mal_id: 10,
            title: "Anime A".to_string(),
            media_type: "tv".to_string(),
            ..Default::default()
        })
        .await?;
        repo.upsert(UpsertMalAnime {
            mal_id: 20,
            title: "Anime B".to_string(),
            media_type: "movie".to_string(),
            ..Default::default()
        })
        .await?;

        let found = repo.get(10).await?.expect("should find mal_id 10");
        assert_eq!(found.title, "Anime A");

        let list = repo.find_by_ids(&[20, 10, 999]).await?;
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].mal_id, 10);
        assert_eq!(list[1].mal_id, 20);

        assert!(repo.find_by_ids(&[]).await?.is_empty());

        Ok(())
    }
}
