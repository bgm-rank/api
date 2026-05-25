use anyhow::Result;
use std::sync::Arc;

use crate::api::schemas::{PublicSeasonResponse, PublicSubjectItem, SeasonTop1Item};
use crate::core::sync::dedup_preserving_order;
use crate::dal::{Database, SeasonRepository, SeasonSubjectRepository, SubjectRepository};

pub struct QueryService {
    db: Arc<Database>,
}

impl QueryService {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    pub async fn list_seasons(&self) -> Result<Vec<PublicSeasonResponse>> {
        let pool = self.db.pool();
        let seasons = SeasonRepository::new(pool).find_all().await?;
        let result = seasons
            .into_iter()
            .map(|s| PublicSeasonResponse {
                season_id: s.season_id,
                year: s.year,
                season: s.season,
                name: s.name,
                updated_at: s.updated_at,
            })
            .collect();
        Ok(result)
    }

    pub async fn get_season_subjects(
        &self,
        season_id: i32,
    ) -> Result<Option<Vec<PublicSubjectItem>>> {
        let pool = self.db.pool();

        // 确认季度存在
        let season = SeasonRepository::new(pool).find_by_id(season_id).await?;
        if season.is_none() {
            return Ok(None);
        }

        // 获取关联的 subject_id 列表
        let subject_ids = SeasonSubjectRepository::new(pool)
            .find_by_season_id(season_id)
            .await?;

        // 批量获取番剧详情
        let mut subjects = SubjectRepository::new(pool)
            .find_by_ids(&subject_ids)
            .await?;

        // 按 rank ASC nulls last 排序
        subjects.sort_by(|a, b| match (a.rank, b.rank) {
            (None, None) => std::cmp::Ordering::Equal,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (Some(_), None) => std::cmp::Ordering::Less,
            (Some(ar), Some(br)) => ar.cmp(&br),
        });

        let items = subjects
            .into_iter()
            .map(|s| PublicSubjectItem {
                id: s.id,
                name: s.name,
                name_cn: s.name_cn,
                images_grid: s.images_grid,
                images_large: s.images_large,
                rank: s.rank,
                score: s.score,
                collection_total: s.collection_total,
                average_comment: s.average_comment.unwrap_or(0.0),
                drop_rate: s.drop_rate,
                air_weekday: s.air_weekday,
                meta_tags: dedup_preserving_order(s.meta_tags),
                media_type: s.media_type,
                rating: s.rating,
            })
            .collect();

        Ok(Some(items))
    }

    pub async fn get_current_season(&self) -> Result<Option<PublicSeasonResponse>> {
        let season_id = current_season_id();
        let pool = self.db.pool();
        let season = SeasonRepository::new(pool).find_by_id(season_id).await?;
        Ok(season.map(|s| PublicSeasonResponse {
            season_id: s.season_id,
            year: s.year,
            season: s.season,
            name: s.name,
            updated_at: s.updated_at,
        }))
    }

    pub async fn get_all_seasons_top1(&self) -> Result<Vec<SeasonTop1Item>> {
        let pool = self.db.pool();

        #[derive(sqlx::FromRow)]
        struct Top1Row {
            season_id: i32,
            id: i32,
            name: Option<String>,
            name_cn: Option<String>,
            images_grid: Option<String>,
            images_large: Option<String>,
            rank: Option<i32>,
            score: Option<f64>,
            collection_total: Option<i32>,
            average_comment: Option<f64>,
            drop_rate: Option<f64>,
            air_weekday: Option<String>,
            #[sqlx(json)]
            meta_tags: Vec<String>,
            media_type: Option<String>,
            rating: Option<String>,
        }

        let rows = sqlx::query_as::<_, Top1Row>(
            r#"
            SELECT season_id, id, name, name_cn, images_grid, images_large,
                   rank, score, collection_total, average_comment,
                   drop_rate, air_weekday, meta_tags, media_type, rating
            FROM (
                SELECT ss.season_id,
                       s.id, s.name, s.name_cn, s.images_grid, s.images_large,
                       s.rank, s.score, s.collection_total, s.average_comment,
                       s.drop_rate, s.air_weekday, s.meta_tags, s.media_type, s.rating,
                       ROW_NUMBER() OVER (
                           PARTITION BY ss.season_id
                           ORDER BY s.rank ASC NULLS LAST, s.collection_total DESC NULLS LAST
                       ) as rn
                FROM season_subjects ss
                JOIN subjects s ON ss.subject_id = s.id
            )
            WHERE rn = 1
            ORDER BY season_id DESC
            "#,
        )
        .fetch_all(pool)
        .await?;

        let items = rows
            .into_iter()
            .map(|r| SeasonTop1Item {
                season_id: r.season_id,
                subject: PublicSubjectItem {
                    id: r.id,
                    name: r.name,
                    name_cn: r.name_cn,
                    images_grid: r.images_grid,
                    images_large: r.images_large,
                    rank: r.rank,
                    score: r.score,
                    collection_total: r.collection_total,
                    average_comment: r.average_comment.unwrap_or(0.0),
                    drop_rate: r.drop_rate,
                    air_weekday: r.air_weekday,
                    meta_tags: dedup_preserving_order(r.meta_tags),
                    media_type: r.media_type,
                    rating: r.rating,
                },
            })
            .collect();

        Ok(items)
    }
}

fn current_season_id() -> i32 {
    use chrono::Datelike;
    let now = chrono::Local::now();
    let year = now.year();
    let month = now.month();
    let season_month = match month {
        1..=3 => 1,
        4..=6 => 4,
        7..=9 => 7,
        _ => 10,
    };
    year * 100 + season_month as i32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dal::db::Database;
    use crate::dal::dto::{CreateSeason, CreateSeasonSubject, CreateSubject};
    use crate::dal::{SeasonRepository, SeasonSubjectRepository, SubjectRepository};
    use sqlx::SqlitePool;

    fn make_create_subject(id: i32, rank: Option<i32>) -> CreateSubject {
        CreateSubject {
            id,
            name: Some(format!("Subject {}", id)),
            name_cn: None,
            images_grid: None,
            images_large: None,
            rank,
            score: None,
            collection_total: None,
            average_comment: None,
            drop_rate: None,
            air_weekday: None,
            meta_tags: vec![],
            ..Default::default()
        }
    }

    #[sqlx::test]
    async fn test_list_seasons(pool: SqlitePool) -> sqlx::Result<()> {
        let db = Arc::new(Database::from_pool(pool.clone()));

        // Insert a season
        SeasonRepository::new(&pool)
            .create(CreateSeason {
                season_id: 202601,
                year: 2026,
                season: "WINTER".to_string(),
                name: Some("2026冬".to_string()),
            })
            .await?;

        let svc = QueryService::new(db);
        let seasons = svc.list_seasons().await.unwrap();
        assert_eq!(seasons.len(), 1);
        assert_eq!(seasons[0].season_id, 202601);
        Ok(())
    }

    // T019 — query_service 精确评分与 rank 测试
    #[sqlx::test]
    async fn test_query_service_returns_exact_score(pool: SqlitePool) -> sqlx::Result<()> {
        let db = Arc::new(Database::from_pool(pool.clone()));
        SeasonRepository::new(&pool)
            .create(CreateSeason {
                season_id: 202601,
                year: 2026,
                season: "WINTER".to_string(),
                name: None,
            })
            .await?;
        SubjectRepository::new(&pool)
            .create(CreateSubject {
                id: 101,
                score: Some(8.1234),
                ..Default::default()
            })
            .await?;
        SeasonSubjectRepository::new(&pool)
            .create(CreateSeasonSubject {
                season_id: 202601,
                subject_id: 101,
            })
            .await?;
        let svc = QueryService::new(db);
        let items = svc.get_season_subjects(202601).await.unwrap().unwrap();
        let score = items[0].score.unwrap();
        assert!(
            (score - 8.1234).abs() < 0.0001,
            "expected 8.1234, got {score}"
        );
        Ok(())
    }

    #[sqlx::test]
    async fn test_query_service_rank_999999_preserved(pool: SqlitePool) -> sqlx::Result<()> {
        let db = Arc::new(Database::from_pool(pool.clone()));
        SeasonRepository::new(&pool)
            .create(CreateSeason {
                season_id: 202601,
                year: 2026,
                season: "WINTER".to_string(),
                name: None,
            })
            .await?;
        SubjectRepository::new(&pool)
            .create(CreateSubject {
                id: 102,
                rank: Some(999999),
                ..Default::default()
            })
            .await?;
        SeasonSubjectRepository::new(&pool)
            .create(CreateSeasonSubject {
                season_id: 202601,
                subject_id: 102,
            })
            .await?;
        let svc = QueryService::new(db);
        let items = svc.get_season_subjects(202601).await.unwrap().unwrap();
        assert_eq!(items[0].rank, Some(999999));
        Ok(())
    }

    // T039 — get_season_subjects 对 meta_tags 去重
    #[sqlx::test]
    async fn test_query_service_deduplicates_meta_tags(pool: SqlitePool) -> sqlx::Result<()> {
        let db = Arc::new(Database::from_pool(pool.clone()));
        SeasonRepository::new(&pool)
            .create(CreateSeason {
                season_id: 202601,
                year: 2026,
                season: "WINTER".to_string(),
                name: None,
            })
            .await?;
        SubjectRepository::new(&pool)
            .create(CreateSubject {
                id: 301,
                meta_tags: vec!["TV".to_string(), "TV".to_string(), "动作".to_string()],
                ..Default::default()
            })
            .await?;
        SeasonSubjectRepository::new(&pool)
            .create(CreateSeasonSubject {
                season_id: 202601,
                subject_id: 301,
            })
            .await?;
        let svc = QueryService::new(db);
        let items = svc.get_season_subjects(202601).await.unwrap().unwrap();
        assert_eq!(
            items[0].meta_tags,
            vec!["TV".to_string(), "动作".to_string()],
            "重复的 meta_tags 应被去重，保留首次出现顺序"
        );
        Ok(())
    }

    #[sqlx::test]
    async fn test_query_service_dedup_preserves_order(pool: SqlitePool) -> sqlx::Result<()> {
        let db = Arc::new(Database::from_pool(pool.clone()));
        SeasonRepository::new(&pool)
            .create(CreateSeason {
                season_id: 202601,
                year: 2026,
                season: "WINTER".to_string(),
                name: None,
            })
            .await?;
        SubjectRepository::new(&pool)
            .create(CreateSubject {
                id: 302,
                meta_tags: vec![
                    "动作".to_string(),
                    "TV".to_string(),
                    "动作".to_string(),
                    "剧情".to_string(),
                ],
                ..Default::default()
            })
            .await?;
        SeasonSubjectRepository::new(&pool)
            .create(CreateSeasonSubject {
                season_id: 202601,
                subject_id: 302,
            })
            .await?;
        let svc = QueryService::new(db);
        let items = svc.get_season_subjects(202601).await.unwrap().unwrap();
        assert_eq!(
            items[0].meta_tags,
            vec!["动作".to_string(), "TV".to_string(), "剧情".to_string()],
            "去重后应保留首次出现顺序"
        );
        Ok(())
    }

    // T032 — list_seasons 包含 updated_at 字段
    #[sqlx::test]
    async fn test_list_seasons_includes_updated_at(pool: SqlitePool) -> sqlx::Result<()> {
        let db = Arc::new(Database::from_pool(pool.clone()));
        SeasonRepository::new(&pool)
            .create(CreateSeason {
                season_id: 202601,
                year: 2026,
                season: "WINTER".to_string(),
                name: None,
            })
            .await?;
        let svc = QueryService::new(db);
        let seasons = svc.list_seasons().await.unwrap();
        assert_eq!(seasons.len(), 1);
        // updated_at 非默认零值，应为合理时间（> 2020-01-01）
        let epoch = chrono::NaiveDate::from_ymd_opt(2020, 1, 1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap();
        assert!(
            seasons[0].updated_at > epoch,
            "updated_at should be a recent timestamp"
        );
        Ok(())
    }

    // T027 — query_service 新字段测试（air_weekday / drop_rate / average_comment）

    #[sqlx::test]
    async fn test_query_service_air_weekday_returned(pool: SqlitePool) -> sqlx::Result<()> {
        let db = Arc::new(Database::from_pool(pool.clone()));
        SeasonRepository::new(&pool)
            .create(CreateSeason {
                season_id: 202601,
                year: 2026,
                season: "WINTER".to_string(),
                name: None,
            })
            .await?;
        SubjectRepository::new(&pool)
            .create(CreateSubject {
                id: 201,
                air_weekday: Some("星期五".to_string()),
                ..Default::default()
            })
            .await?;
        SeasonSubjectRepository::new(&pool)
            .create(CreateSeasonSubject {
                season_id: 202601,
                subject_id: 201,
            })
            .await?;
        let svc = QueryService::new(db);
        let items = svc.get_season_subjects(202601).await.unwrap().unwrap();
        assert_eq!(items[0].air_weekday, Some("星期五".to_string()));
        Ok(())
    }

    #[sqlx::test]
    async fn test_query_service_drop_rate_returned(pool: SqlitePool) -> sqlx::Result<()> {
        let db = Arc::new(Database::from_pool(pool.clone()));
        SeasonRepository::new(&pool)
            .create(CreateSeason {
                season_id: 202601,
                year: 2026,
                season: "WINTER".to_string(),
                name: None,
            })
            .await?;
        SubjectRepository::new(&pool)
            .create(CreateSubject {
                id: 202,
                drop_rate: Some(0.1),
                ..Default::default()
            })
            .await?;
        SeasonSubjectRepository::new(&pool)
            .create(CreateSeasonSubject {
                season_id: 202601,
                subject_id: 202,
            })
            .await?;
        let svc = QueryService::new(db);
        let items = svc.get_season_subjects(202601).await.unwrap().unwrap();
        let rate = items[0].drop_rate.unwrap();
        assert!((rate - 0.1).abs() < 0.0001, "expected 0.1, got {rate}");
        Ok(())
    }

    #[sqlx::test]
    async fn test_query_service_average_comment_returned(pool: SqlitePool) -> sqlx::Result<()> {
        let db = Arc::new(Database::from_pool(pool.clone()));
        SeasonRepository::new(&pool)
            .create(CreateSeason {
                season_id: 202601,
                year: 2026,
                season: "WINTER".to_string(),
                name: None,
            })
            .await?;
        SubjectRepository::new(&pool)
            .create(CreateSubject {
                id: 203,
                average_comment: Some(3.5),
                ..Default::default()
            })
            .await?;
        SeasonSubjectRepository::new(&pool)
            .create(CreateSeasonSubject {
                season_id: 202601,
                subject_id: 203,
            })
            .await?;
        let svc = QueryService::new(db);
        let items = svc.get_season_subjects(202601).await.unwrap().unwrap();
        let avg = items[0].average_comment;
        assert!((avg - 3.5).abs() < 0.0001, "expected 3.5, got {avg}");
        Ok(())
    }

    #[sqlx::test]
    async fn test_get_season_subjects_none_when_not_found(pool: SqlitePool) -> sqlx::Result<()> {
        let db = Arc::new(Database::from_pool(pool));
        let svc = QueryService::new(db);
        let result = svc.get_season_subjects(999999).await.unwrap();
        assert!(result.is_none());
        Ok(())
    }

    #[sqlx::test]
    async fn test_get_season_subjects_sorted_by_rank(pool: SqlitePool) -> sqlx::Result<()> {
        let db = Arc::new(Database::from_pool(pool.clone()));

        SeasonRepository::new(&pool)
            .create(CreateSeason {
                season_id: 202601,
                year: 2026,
                season: "WINTER".to_string(),
                name: None,
            })
            .await?;

        let subj_repo = SubjectRepository::new(&pool);
        subj_repo.create(make_create_subject(1, Some(100))).await?;
        subj_repo.create(make_create_subject(2, Some(50))).await?;
        subj_repo.create(make_create_subject(3, None)).await?;

        let ss_repo = SeasonSubjectRepository::new(&pool);
        for sid in [1, 2, 3] {
            ss_repo
                .create(CreateSeasonSubject {
                    season_id: 202601,
                    subject_id: sid,
                })
                .await?;
        }

        let svc = QueryService::new(db);
        let items = svc.get_season_subjects(202601).await.unwrap().unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].id, 2); // rank 50 first
        assert_eq!(items[1].id, 1); // rank 100 second
        assert_eq!(items[2].id, 3); // None last
        Ok(())
    }
}
