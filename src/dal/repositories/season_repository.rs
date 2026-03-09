use crate::dal::dto::{CreateSeason, Season, UpdateSeason};
use sqlx::PgPool;

pub struct SeasonRepository<'a> {
    pool: &'a PgPool,
}

#[allow(dead_code)]
impl<'a> SeasonRepository<'a> {
    pub fn new(pool: &'a PgPool) -> Self {
        Self { pool }
    }

    pub async fn create(&self, create_season: CreateSeason) -> Result<Season, sqlx::Error> {
        let row = sqlx::query_as::<_, Season>(
            r#"
            INSERT INTO seasons (season_id, year, season, name)
            VALUES ($1, $2, $3, $4)
            RETURNING season_id, year, season, name, created_at, updated_at
            "#,
        )
        .bind(create_season.season_id)
        .bind(create_season.year)
        .bind(create_season.season)
        .bind(create_season.name)
        .fetch_one(self.pool)
        .await?;

        Ok(row)
    }

    pub async fn upsert(&self, season: CreateSeason) -> Result<Season, sqlx::Error> {
        let row = sqlx::query_as::<_, Season>(
            r#"
            INSERT INTO seasons (season_id, year, season, name)
            VALUES ($1, $2, $3, $4)
            ON CONFLICT (season_id)
            DO UPDATE SET
                year = EXCLUDED.year,
                season = EXCLUDED.season,
                name = COALESCE(EXCLUDED.name, seasons.name),
                updated_at = CURRENT_TIMESTAMP
            RETURNING season_id, year, season, name, created_at, updated_at
            "#,
        )
        .bind(season.season_id)
        .bind(season.year)
        .bind(season.season)
        .bind(season.name)
        .fetch_one(self.pool)
        .await?;

        Ok(row)
    }

    pub async fn find_by_id(&self, season_id: i32) -> Result<Option<Season>, sqlx::Error> {
        let row = sqlx::query_as::<_, Season>(
            r#"
            SELECT * FROM seasons WHERE season_id = $1
            "#,
        )
        .bind(season_id)
        .fetch_optional(self.pool)
        .await?;

        Ok(row)
    }

    pub async fn find_all(&self) -> Result<Vec<Season>, sqlx::Error> {
        let rows = sqlx::query_as::<_, Season>(
            r#"
            SELECT * FROM seasons
            ORDER BY season_id DESC
            "#,
        )
        .fetch_all(self.pool)
        .await?;

        Ok(rows)
    }

    pub async fn update(
        &self,
        season_id: i32,
        update_season: UpdateSeason,
    ) -> Result<Season, sqlx::Error> {
        let row = sqlx::query_as::<_, Season>(
            r#"
            UPDATE seasons
            SET
                year = COALESCE($2, year),
                season = COALESCE($3, season),
                name = COALESCE($4, name),
                updated_at = CURRENT_TIMESTAMP
            WHERE season_id = $1
            RETURNING season_id, year, season, name, created_at, updated_at
            "#,
        )
        .bind(season_id)
        .bind(update_season.year)
        .bind(update_season.season)
        .bind(update_season.name)
        .fetch_one(self.pool)
        .await?;

        Ok(row)
    }

    pub async fn delete(&self, season_id: i32) -> Result<bool, sqlx::Error> {
        let result = sqlx::query(
            r#"
            DELETE FROM seasons WHERE season_id = $1
            "#,
        )
        .bind(season_id)
        .execute(self.pool)
        .await?;

        Ok(result.rows_affected() > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[sqlx::test]
    async fn test_create_season(pool: PgPool) -> sqlx::Result<()> {
        let repo = SeasonRepository::new(&pool);

        let create_season = CreateSeason {
            season_id: 202601,
            year: 2026,
            season: "WINTER".to_string(),
            name: Some("2026年冬季番".to_string()),
        };

        let season = repo.create(create_season).await?;
        assert_eq!(season.season_id, 202601);
        assert_eq!(season.year, 2026);
        assert_eq!(season.season, "WINTER");

        Ok(())
    }

    #[sqlx::test]
    async fn test_upsert_season(pool: PgPool) -> sqlx::Result<()> {
        let repo = SeasonRepository::new(&pool);

        // 1. 首次插入
        let insert_data = CreateSeason {
            season_id: 202701,
            year: 2027,
            season: "WINTER".to_string(),
            name: Some("2027年手动备注名称".to_string()),
        };

        let season_inserted = repo.upsert(insert_data).await?;
        assert_eq!(season_inserted.season_id, 202701);
        assert_eq!(season_inserted.name, Some("2027年手动备注名称".to_string()));

        // 稍微等待一下确保 updated_at 的时间戳有变化（如果在意微秒级精度的话）
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        // 2. 模拟同步数据覆盖，name 传 None
        let update_data = CreateSeason {
            season_id: 202701,
            year: 2027,
            season: "SPRING".to_string(), // 更新了季节
            name: None,                   // 同步数据中没有 name
        };

        let season_updated = repo.upsert(update_data).await?;

        assert_eq!(season_updated.season_id, 202701);
        assert_eq!(season_updated.season, "SPRING");

        // 关键断言：验证原有的 name 没有被 NULL 覆盖
        assert_eq!(season_updated.name, Some("2027年手动备注名称".to_string()));

        // 验证 updated_at 被正确更新
        assert!(season_updated.updated_at > season_inserted.updated_at);

        Ok(())
    }

    #[sqlx::test]
    async fn test_find_season_by_id(pool: PgPool) -> sqlx::Result<()> {
        let repo = SeasonRepository::new(&pool);

        let season_id = 202510;
        let create_season = CreateSeason {
            season_id: season_id,
            year: 2025,
            season: "AUTUMN".to_string(),
            name: Some("2025年秋季番".to_string()),
        };

        repo.create(create_season).await?;

        let season = repo.find_by_id(season_id).await?.unwrap();
        assert_eq!(season.season_id, 202510);

        let not_found = repo.find_by_id(999999).await?;
        assert!(not_found.is_none());

        Ok(())
    }

    #[sqlx::test]
    async fn test_find_all_season(pool: PgPool) -> sqlx::Result<()> {
        let repo = SeasonRepository::new(&pool);

        let create_seasons = vec![
            CreateSeason {
                season_id: 202507,
                year: 2025,
                season: "SUMMER".to_string(),
                name: Some("2025年夏季番".to_string()),
            },
            CreateSeason {
                season_id: 202504,
                year: 2025,
                season: "SPRING".to_string(),
                name: Some("2025年春季番".to_string()),
            },
        ];

        for create_season in create_seasons.into_iter() {
            repo.create(create_season).await?;
        }

        let seasons = repo.find_all().await?;

        assert_eq!(seasons.len(), 2);

        Ok(())
    }

    #[sqlx::test]
    async fn test_update_season(pool: PgPool) -> sqlx::Result<()> {
        let repo = SeasonRepository::new(&pool);

        let create_season = CreateSeason {
            season_id: 202501,
            year: 2025,
            season: "WINTER".to_string(),
            name: Some("2025年冬季番".to_string()),
        };

        repo.create(create_season).await?;

        let update_season = UpdateSeason {
            year: None,
            season: None,
            name: Some("UPDATED".to_string()),
        };

        let season = repo.update(202501, update_season).await?;

        assert_eq!(season.year, 2025);
        assert_eq!(season.season, "WINTER");
        assert_eq!(season.name, Some("UPDATED".to_string()));

        Ok(())
    }

    #[sqlx::test]
    async fn test_delete_season(pool: PgPool) -> sqlx::Result<()> {
        let repo = SeasonRepository::new(&pool);

        let create_season = CreateSeason {
            season_id: 202410,
            year: 2024,
            season: "AUTUMN".to_string(),
            name: Some("2024年秋季番".to_string()),
        };

        repo.create(create_season).await?;

        let result = repo.delete(202410).await?;

        assert_eq!(result, true);

        let deleted = repo.find_by_id(202410).await?;

        assert!(deleted.is_none());

        Ok(())
    }
}
