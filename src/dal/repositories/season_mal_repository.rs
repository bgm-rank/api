use super::log_db_error;
use crate::dal::dto::{SeasonMal, SeasonMalExportRow};
use sqlx::SqlitePool;

pub struct SeasonMalRepository<'a> {
    pool: &'a SqlitePool,
}

#[allow(dead_code)]
impl<'a> SeasonMalRepository<'a> {
    pub fn new(pool: &'a SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn insert_ignore(&self, season_id: i32, mal_id: i32) -> Result<bool, sqlx::Error> {
        let result = sqlx::query(
            r#"
            INSERT INTO season_mal (season_id, mal_id, origin)
            VALUES (?, ?, 'mal')
            ON CONFLICT (season_id, mal_id) DO NOTHING
            "#,
        )
        .bind(season_id)
        .bind(mal_id)
        .execute(self.pool)
        .await
        .inspect_err(|e| log_db_error("insert_ignore", "season_mal", e))?;

        Ok(result.rows_affected() > 0)
    }

    pub async fn insert_manual(
        &self,
        season_id: i32,
        mal_id: i32,
        note: Option<String>,
    ) -> Result<SeasonMal, sqlx::Error> {
        let row = sqlx::query_as::<_, SeasonMal>(
            r#"
            INSERT INTO season_mal (season_id, mal_id, origin, is_removed, note)
            VALUES (?, ?, 'manual', 0, ?)
            ON CONFLICT (season_id, mal_id) DO UPDATE SET
                origin = 'manual',
                is_removed = 0,
                note = COALESCE(EXCLUDED.note, season_mal.note)
            RETURNING *
            "#,
        )
        .bind(season_id)
        .bind(mal_id)
        .bind(note)
        .fetch_one(self.pool)
        .await
        .inspect_err(|e| log_db_error("insert_manual", "season_mal", e))?;

        Ok(row)
    }

    pub async fn set_removed(
        &self,
        season_id: i32,
        mal_id: i32,
        is_removed: bool,
        note: Option<String>,
    ) -> Result<Option<SeasonMal>, sqlx::Error> {
        let row = sqlx::query_as::<_, SeasonMal>(
            r#"
            UPDATE season_mal
            SET is_removed = ?,
                note = COALESCE(?, note)
            WHERE season_id = ? AND mal_id = ?
            RETURNING *
            "#,
        )
        .bind(is_removed)
        .bind(note)
        .bind(season_id)
        .bind(mal_id)
        .fetch_optional(self.pool)
        .await
        .inspect_err(|e| log_db_error("set_removed", "season_mal", e))?;

        Ok(row)
    }

    pub async fn get(&self, season_id: i32, mal_id: i32) -> Result<Option<SeasonMal>, sqlx::Error> {
        let row = sqlx::query_as::<_, SeasonMal>(
            "SELECT * FROM season_mal WHERE season_id = ? AND mal_id = ?",
        )
        .bind(season_id)
        .bind(mal_id)
        .fetch_optional(self.pool)
        .await
        .inspect_err(|e| log_db_error("get", "season_mal", e))?;

        Ok(row)
    }

    pub async fn list_by_season(
        &self,
        season_id: i32,
        include_removed: bool,
    ) -> Result<Vec<SeasonMal>, sqlx::Error> {
        let sql = if include_removed {
            "SELECT * FROM season_mal WHERE season_id = ? ORDER BY mal_id ASC"
        } else {
            "SELECT * FROM season_mal WHERE season_id = ? AND is_removed = 0 ORDER BY mal_id ASC"
        };
        let rows = sqlx::query_as::<_, SeasonMal>(sql)
            .bind(season_id)
            .fetch_all(self.pool)
            .await
            .inspect_err(|e| log_db_error("list_by_season", "season_mal", e))?;

        Ok(rows)
    }

    pub async fn list_by_mal_id(&self, mal_id: i32) -> Result<Vec<SeasonMal>, sqlx::Error> {
        let rows = sqlx::query_as::<_, SeasonMal>(
            "SELECT * FROM season_mal WHERE mal_id = ? ORDER BY season_id DESC",
        )
        .bind(mal_id)
        .fetch_all(self.pool)
        .await
        .inspect_err(|e| log_db_error("list_by_mal_id", "season_mal", e))?;

        Ok(rows)
    }

    pub async fn delete(&self, season_id: i32, mal_id: i32) -> Result<bool, sqlx::Error> {
        let result = sqlx::query("DELETE FROM season_mal WHERE season_id = ? AND mal_id = ?")
            .bind(season_id)
            .bind(mal_id)
            .execute(self.pool)
            .await
            .inspect_err(|e| log_db_error("delete", "season_mal", e))?;

        Ok(result.rows_affected() > 0)
    }

    pub async fn list_for_export(&self) -> Result<Vec<SeasonMalExportRow>, sqlx::Error> {
        let rows = sqlx::query_as::<_, SeasonMalExportRow>(
            r#"
            SELECT season_id, mal_id, origin, is_removed, note
            FROM season_mal
            WHERE origin = 'manual' OR is_removed = 1
            ORDER BY season_id ASC, mal_id ASC
            "#,
        )
        .fetch_all(self.pool)
        .await
        .inspect_err(|e| log_db_error("list_for_export", "season_mal", e))?;

        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dal::Database;
    use crate::dal::dto::{CreateSeason, UpsertMalAnime};
    use crate::dal::repositories::{MalAnimeRepository, SeasonRepository};

    async fn seed_season_and_mal(
        pool: &SqlitePool,
        season_id: i32,
        mal_id: i32,
    ) -> sqlx::Result<()> {
        SeasonRepository::new(pool)
            .upsert(CreateSeason {
                season_id,
                year: season_id / 100,
                season: "WINTER".to_string(),
                name: None,
            })
            .await?;
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

    #[sqlx::test]
    async fn test_insert_ignore_does_not_resurrect_or_overwrite_removed_row(
        pool: SqlitePool,
    ) -> sqlx::Result<()> {
        let db = Database::from_pool(pool);
        seed_season_and_mal(db.pool(), 202510, 1001).await?;
        seed_season_and_mal(db.pool(), 202601, 1001).await?;

        let repo = SeasonMalRepository::new(db.pool());

        // Initial MAL fetch inserts with origin='mal', is_removed=false
        assert!(repo.insert_ignore(202510, 1001).await?);
        let initial = repo.get(202510, 1001).await?.unwrap();
        assert_eq!(initial.origin, "mal");
        assert!(!initial.is_removed);

        // Mark as removed (e.g. wrong season moved to 202601)
        let removed = repo
            .set_removed(202510, 1001, true, Some("移至 202601".to_string()))
            .await?
            .expect("row should exist");
        assert!(removed.is_removed);
        assert_eq!(removed.note, Some("移至 202601".to_string()));

        // Re-running MAL fetch MUST NOT resurrect or overwrite the removed row
        assert!(!repo.insert_ignore(202510, 1001).await?);
        let after_refetch = repo.get(202510, 1001).await?.unwrap();
        assert!(after_refetch.is_removed);
        assert_eq!(after_refetch.note, Some("移至 202601".to_string()));
        assert_eq!(after_refetch.origin, "mal");

        // Manual insert into target season 202601
        let manual = repo
            .insert_manual(202601, 1001, Some("从 202510 移入".to_string()))
            .await?;
        assert_eq!(manual.origin, "manual");
        assert!(!manual.is_removed);

        // Subsequent insert_ignore on 202601 must not overwrite manual origin/note
        assert!(!repo.insert_ignore(202601, 1001).await?);
        let manual_after = repo.get(202601, 1001).await?.unwrap();
        assert_eq!(manual_after.origin, "manual");
        assert_eq!(manual_after.note, Some("从 202510 移入".to_string()));

        Ok(())
    }

    #[sqlx::test]
    async fn test_list_by_season_active_vs_all(pool: SqlitePool) -> sqlx::Result<()> {
        let db = Database::from_pool(pool);
        seed_season_and_mal(db.pool(), 202601, 1).await?;
        seed_season_and_mal(db.pool(), 202601, 2).await?;

        let repo = SeasonMalRepository::new(db.pool());
        repo.insert_ignore(202601, 1).await?;
        repo.insert_ignore(202601, 2).await?;
        repo.set_removed(202601, 2, true, None).await?;

        let active = repo.list_by_season(202601, false).await?;
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].mal_id, 1);

        let all = repo.list_by_season(202601, true).await?;
        assert_eq!(all.len(), 2);

        Ok(())
    }

    #[sqlx::test]
    async fn test_list_for_export_filters_and_orders(pool: SqlitePool) -> sqlx::Result<()> {
        let db = Database::from_pool(pool);
        seed_season_and_mal(db.pool(), 202604, 200).await?;
        seed_season_and_mal(db.pool(), 202601, 102).await?;
        seed_season_and_mal(db.pool(), 202601, 101).await?;
        seed_season_and_mal(db.pool(), 202601, 100).await?;

        let repo = SeasonMalRepository::new(db.pool());
        // origin='mal', is_removed=0 -> must NOT be exported
        repo.insert_ignore(202601, 100).await?;
        // origin='mal', is_removed=1 -> MUST be exported
        repo.insert_ignore(202601, 102).await?;
        repo.set_removed(202601, 102, true, Some("wrong season".to_string()))
            .await?;
        // origin='manual', is_removed=0 -> MUST be exported
        repo.insert_manual(202601, 101, Some("manual add".to_string()))
            .await?;
        // later season, manual -> ordered after 202601
        repo.insert_manual(202604, 200, None).await?;

        let exported = repo.list_for_export().await?;
        assert_eq!(
            exported,
            vec![
                SeasonMalExportRow {
                    season_id: 202601,
                    mal_id: 101,
                    origin: "manual".to_string(),
                    is_removed: false,
                    note: Some("manual add".to_string()),
                },
                SeasonMalExportRow {
                    season_id: 202601,
                    mal_id: 102,
                    origin: "mal".to_string(),
                    is_removed: true,
                    note: Some("wrong season".to_string()),
                },
                SeasonMalExportRow {
                    season_id: 202604,
                    mal_id: 200,
                    origin: "manual".to_string(),
                    is_removed: false,
                    note: None,
                },
            ]
        );

        Ok(())
    }
}
