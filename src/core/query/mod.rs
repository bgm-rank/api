use anyhow::Result;
use std::sync::Arc;

use crate::api::schemas::{PublicSeasonResponse, PublicSubjectItem};
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
                meta_tags: s.meta_tags,
                media_type: s.media_type,
                rating: s.rating,
            })
            .collect();

        Ok(Some(items))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dal::db::Database;
    use crate::dal::dto::{CreateSeason, CreateSeasonSubject, CreateSubject};
    use crate::dal::{SeasonRepository, SeasonSubjectRepository, SubjectRepository};
    use sqlx::PgPool;

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
    async fn test_list_seasons(pool: PgPool) -> sqlx::Result<()> {
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

    #[sqlx::test]
    async fn test_get_season_subjects_none_when_not_found(pool: PgPool) -> sqlx::Result<()> {
        let db = Arc::new(Database::from_pool(pool));
        let svc = QueryService::new(db);
        let result = svc.get_season_subjects(999999).await.unwrap();
        assert!(result.is_none());
        Ok(())
    }

    #[sqlx::test]
    async fn test_get_season_subjects_sorted_by_rank(pool: PgPool) -> sqlx::Result<()> {
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
