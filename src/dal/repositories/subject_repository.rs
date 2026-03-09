use crate::dal::dto::{CreateSubject, Subject, UpdateSubject};
use sqlx::PgPool;

pub struct SubjectRepository<'a> {
    pool: &'a PgPool,
}

#[allow(dead_code)]
impl<'a> SubjectRepository<'a> {
    pub fn new(pool: &'a PgPool) -> Self {
        Self { pool }
    }

    pub async fn create(&self, subject: CreateSubject) -> Result<Subject, sqlx::Error> {
        let row = sqlx::query_as::<_, Subject>(
            r#"
            INSERT INTO subjects (
                id, name, name_cn, images_grid, images_large,
                rank, score, collection_total, average_comment,
                drop_rate, air_weekday, meta_tags, media_type, rating)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
            RETURNING *
            "#,
        )
        .bind(subject.id)
        .bind(subject.name)
        .bind(subject.name_cn)
        .bind(subject.images_grid)
        .bind(subject.images_large)
        .bind(subject.rank)
        .bind(subject.score)
        .bind(subject.collection_total)
        .bind(subject.average_comment)
        .bind(subject.drop_rate)
        .bind(subject.air_weekday)
        .bind(subject.meta_tags)
        .bind(subject.media_type)
        .bind(subject.rating)
        .fetch_one(self.pool)
        .await?;

        Ok(row)
    }

    pub async fn upsert(&self, subject: CreateSubject) -> Result<Subject, sqlx::Error> {
        let row = sqlx::query_as::<_, Subject>(
            r#"
            INSERT INTO subjects (
                id, name, name_cn, images_grid, images_large,
                rank, score, collection_total, average_comment,
                drop_rate, air_weekday, meta_tags, media_type, rating
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
            ON CONFLICT (id) DO UPDATE SET
                name = COALESCE(EXCLUDED.name, subjects.name),
                name_cn = COALESCE(EXCLUDED.name_cn, subjects.name_cn),
                images_grid = COALESCE(EXCLUDED.images_grid, subjects.images_grid),
                images_large = COALESCE(EXCLUDED.images_large, subjects.images_large),
                rank = COALESCE(EXCLUDED.rank, subjects.rank),
                score = COALESCE(EXCLUDED.score, subjects.score),
                collection_total = COALESCE(EXCLUDED.collection_total, subjects.collection_total),
                average_comment = COALESCE(EXCLUDED.average_comment, subjects.average_comment),
                drop_rate = COALESCE(EXCLUDED.drop_rate, subjects.drop_rate),
                air_weekday = COALESCE(EXCLUDED.air_weekday, subjects.air_weekday),
                meta_tags = CASE WHEN array_length(EXCLUDED.meta_tags, 1) > 0 THEN EXCLUDED.meta_tags ELSE subjects.meta_tags END,
                media_type = COALESCE(EXCLUDED.media_type, subjects.media_type),
                rating = COALESCE(EXCLUDED.rating, subjects.rating),
                updated_at = CURRENT_TIMESTAMP
            RETURNING *
            "#,
        )
        .bind(subject.id)
        .bind(subject.name)
        .bind(subject.name_cn)
        .bind(subject.images_grid)
        .bind(subject.images_large)
        .bind(subject.rank)
        .bind(subject.score)
        .bind(subject.collection_total)
        .bind(subject.average_comment)
        .bind(subject.drop_rate)
        .bind(subject.air_weekday)
        .bind(subject.meta_tags)
        .bind(subject.media_type)
        .bind(subject.rating)
        .fetch_one(self.pool)
        .await?;

        Ok(row)
    }

    pub async fn find_due_for_update(
        &self,
        current_season_id: i32,
    ) -> Result<Vec<(i32, i32, Option<chrono::DateTime<chrono::Utc>>)>, sqlx::Error> {
        use chrono::Utc;

        let rows = sqlx::query_as::<_, (i32, i32, Option<chrono::DateTime<Utc>>)>(
            r#"
            SELECT s.id, MAX(ss.season_id) as newest_season_id, s.last_updated_at
            FROM subjects s
            JOIN season_subjects ss ON s.id = ss.subject_id
            GROUP BY s.id, s.last_updated_at
            "#,
        )
        .fetch_all(self.pool)
        .await?;

        let now = Utc::now();
        let result = rows
            .into_iter()
            .filter(|(_, newest_season_id, last_updated_at)| {
                let age =
                    crate::core::scheduler::quarters_distance(*newest_season_id, current_season_id);
                crate::core::scheduler::is_due(age, last_updated_at.as_ref(), now)
            })
            .collect();

        Ok(result)
    }

    pub async fn update_last_updated_at(&self, subject_id: i32) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE subjects SET last_updated_at = NOW() WHERE id = $1")
            .bind(subject_id)
            .execute(self.pool)
            .await?;
        Ok(())
    }

    pub async fn find_orphans(&self) -> Result<Vec<Subject>, sqlx::Error> {
        let rows = sqlx::query_as::<_, Subject>(
            r#"
            SELECT * FROM subjects s
            WHERE NOT EXISTS (
                SELECT 1 FROM season_subjects ss WHERE ss.subject_id = s.id
            )
            "#,
        )
        .fetch_all(self.pool)
        .await?;

        Ok(rows)
    }

    pub async fn delete_orphans(&self) -> Result<u64, sqlx::Error> {
        let mut tx = self.pool.begin().await?;

        let rows = sqlx::query_scalar::<_, i32>(
            r#"
            DELETE FROM subjects
            WHERE id NOT IN (SELECT DISTINCT subject_id FROM season_subjects)
            RETURNING id
            "#,
        )
        .fetch_all(&mut *tx)
        .await?;

        let count = rows.len() as u64;
        tx.commit().await?;
        Ok(count)
    }

    pub async fn find_by_ids(&self, ids: &[i32]) -> Result<Vec<Subject>, sqlx::Error> {
        if ids.is_empty() {
            return Ok(vec![]);
        }
        let rows = sqlx::query_as::<_, Subject>("SELECT * FROM subjects WHERE id = ANY($1)")
            .bind(ids)
            .fetch_all(self.pool)
            .await?;

        Ok(rows)
    }

    pub async fn find_by_id(&self, subject_id: i32) -> Result<Option<Subject>, sqlx::Error> {
        let row = sqlx::query_as::<_, Subject>(
            r#"
            SELECT * FROM subjects
            WHERE id = $1
            "#,
        )
        .bind(subject_id)
        .fetch_optional(self.pool)
        .await?;

        Ok(row)
    }

    pub async fn update(
        &self,
        subject_id: i32,
        subject: UpdateSubject,
    ) -> Result<Subject, sqlx::Error> {
        let row = sqlx::query_as(
            r#"
            UPDATE subjects
            SET
                name = COALESCE($2, name),
                name_cn = COALESCE($3, name_cn),
                images_grid = COALESCE($4, images_grid),
                images_large = COALESCE($5, images_large),
                rank = COALESCE($6, rank),
                score = COALESCE($7, score),
                collection_total = COALESCE($8, collection_total),
                average_comment = COALESCE($9, average_comment),
                drop_rate = COALESCE($10, drop_rate),
                air_weekday = COALESCE($11, air_weekday),
                meta_tags = COALESCE($12, meta_tags)
            WHERE id = $1
            RETURNING id, name, name_cn, images_grid, images_large,
                rank, score, collection_total, average_comment,
                drop_rate, air_weekday, meta_tags, updated_at
            "#,
        )
        .bind(subject_id)
        .bind(subject.name)
        .bind(subject.name_cn)
        .bind(subject.images_grid)
        .bind(subject.images_large)
        .bind(subject.rank)
        .bind(subject.score)
        .bind(subject.collection_total)
        .bind(subject.average_comment)
        .bind(subject.drop_rate)
        .bind(subject.air_weekday)
        .bind(subject.meta_tags)
        .fetch_one(self.pool)
        .await?;

        Ok(row)
    }

    pub async fn delete(&self, subject_id: i32) -> Result<bool, sqlx::Error> {
        let result = sqlx::query(
            r#"
            DELETE FROM subjects WHERE id = $1
            "#,
        )
        .bind(subject_id)
        .execute(self.pool)
        .await?;

        Ok(result.rows_affected() > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // T033/T034 — find_orphans
    #[sqlx::test]
    async fn test_find_orphans(pool: PgPool) -> sqlx::Result<()> {
        use crate::dal::dto::{CreateSeason, CreateSeasonSubject};
        use crate::dal::repositories::SeasonRepository;
        use crate::dal::repositories::SeasonSubjectRepository;

        SeasonRepository::new(&pool)
            .create(CreateSeason {
                season_id: 202601,
                year: 2026,
                season: "WINTER".to_string(),
                name: None,
            })
            .await?;

        let repo = SubjectRepository::new(&pool);
        // Associated subjects (not orphans)
        repo.create(CreateSubject {
            id: 1,
            ..Default::default()
        })
        .await?;
        repo.create(CreateSubject {
            id: 2,
            ..Default::default()
        })
        .await?;
        SeasonSubjectRepository::new(&pool)
            .create(CreateSeasonSubject {
                season_id: 202601,
                subject_id: 1,
            })
            .await?;
        SeasonSubjectRepository::new(&pool)
            .create(CreateSeasonSubject {
                season_id: 202601,
                subject_id: 2,
            })
            .await?;

        // Orphans
        repo.create(CreateSubject {
            id: 10,
            ..Default::default()
        })
        .await?;
        repo.create(CreateSubject {
            id: 11,
            ..Default::default()
        })
        .await?;
        repo.create(CreateSubject {
            id: 12,
            ..Default::default()
        })
        .await?;

        let orphans = repo.find_orphans().await?;
        assert_eq!(orphans.len(), 3);
        let ids: Vec<i32> = orphans.iter().map(|s| s.id).collect();
        assert!(ids.contains(&10));
        assert!(ids.contains(&11));
        assert!(ids.contains(&12));
        Ok(())
    }

    // T035/T036 — delete_orphans
    #[sqlx::test]
    async fn test_delete_orphans(pool: PgPool) -> sqlx::Result<()> {
        use crate::dal::dto::{CreateSeason, CreateSeasonSubject};
        use crate::dal::repositories::SeasonRepository;
        use crate::dal::repositories::SeasonSubjectRepository;

        SeasonRepository::new(&pool)
            .create(CreateSeason {
                season_id: 202601,
                year: 2026,
                season: "WINTER".to_string(),
                name: None,
            })
            .await?;

        let repo = SubjectRepository::new(&pool);
        repo.create(CreateSubject {
            id: 1,
            ..Default::default()
        })
        .await?;
        SeasonSubjectRepository::new(&pool)
            .create(CreateSeasonSubject {
                season_id: 202601,
                subject_id: 1,
            })
            .await?;
        repo.create(CreateSubject {
            id: 10,
            ..Default::default()
        })
        .await?;
        repo.create(CreateSubject {
            id: 11,
            ..Default::default()
        })
        .await?;

        let deleted = repo.delete_orphans().await?;
        assert_eq!(deleted, 2);

        let orphans = repo.find_orphans().await?;
        assert!(orphans.is_empty());
        Ok(())
    }

    // T026/T027 — find_due_for_update
    #[sqlx::test]
    async fn test_find_due_for_update(pool: PgPool) -> sqlx::Result<()> {
        use crate::dal::dto::{CreateSeason, CreateSeasonSubject};
        use crate::dal::repositories::SeasonRepository;
        use crate::dal::repositories::SeasonSubjectRepository;

        // Setup: current season 202601, past season 202504
        SeasonRepository::new(&pool)
            .create(CreateSeason {
                season_id: 202601,
                year: 2026,
                season: "WINTER".to_string(),
                name: None,
            })
            .await?;
        SeasonRepository::new(&pool)
            .create(CreateSeason {
                season_id: 202504,
                year: 2025,
                season: "SPRING".to_string(),
                name: None,
            })
            .await?;

        let repo = SubjectRepository::new(&pool);

        // Subject 1: current season → always due
        repo.create(CreateSubject {
            id: 1,
            ..Default::default()
        })
        .await?;
        SeasonSubjectRepository::new(&pool)
            .create(CreateSeasonSubject {
                season_id: 202601,
                subject_id: 1,
            })
            .await?;

        // Subject 2: past season (age=3), last_updated_at = 4 days ago → due
        repo.create(CreateSubject {
            id: 2,
            ..Default::default()
        })
        .await?;
        SeasonSubjectRepository::new(&pool)
            .create(CreateSeasonSubject {
                season_id: 202504,
                subject_id: 2,
            })
            .await?;
        sqlx::query("UPDATE subjects SET last_updated_at = NOW() - INTERVAL '4 days' WHERE id = 2")
            .execute(&pool)
            .await?;

        // Subject 3: past season (age=3), last_updated_at = 1 day ago → NOT due
        repo.create(CreateSubject {
            id: 3,
            ..Default::default()
        })
        .await?;
        SeasonSubjectRepository::new(&pool)
            .create(CreateSeasonSubject {
                season_id: 202504,
                subject_id: 3,
            })
            .await?;
        sqlx::query("UPDATE subjects SET last_updated_at = NOW() - INTERVAL '1 day' WHERE id = 3")
            .execute(&pool)
            .await?;

        // current_season_id is 202510 (autumn 2025) → distance from 202504=3, from 202601=-3
        // Wait, let me reconsider. current_season_id=202510 means:
        // Subject 1 (season 202601): quarters_distance(202601, 202510) = -3 (past) but we pass current as 202601
        // Let me use current_season_id = 202601:
        // Subject 1 (202601): age=0 → always due
        // Subject 2 (202504): age = quarters_distance(202504, 202601) = 3 → need 3 days → 4 days → DUE
        // Subject 3 (202504): age=3 → 1 day → NOT DUE
        let due = repo.find_due_for_update(202601).await?;
        let due_ids: Vec<i32> = due.iter().map(|(id, _, _)| *id).collect();

        assert!(
            due_ids.contains(&1),
            "Subject 1 (current season) should be due"
        );
        assert!(due_ids.contains(&2), "Subject 2 (stale) should be due");
        assert!(!due_ids.contains(&3), "Subject 3 (fresh) should NOT be due");

        Ok(())
    }

    // T028/T029 — update_last_updated_at
    #[sqlx::test]
    async fn test_update_last_updated_at(pool: PgPool) -> sqlx::Result<()> {
        let repo = SubjectRepository::new(&pool);
        repo.create(CreateSubject {
            id: 999,
            ..Default::default()
        })
        .await?;

        repo.update_last_updated_at(999).await?;

        let subject = repo.find_by_id(999).await?.unwrap();
        assert!(
            subject.last_updated_at.is_some(),
            "last_updated_at should be set"
        );

        Ok(())
    }

    #[sqlx::test]
    async fn test_create_subject(pool: PgPool) -> sqlx::Result<()> {
        let repo = SubjectRepository::new(&pool);

        let create_subject = CreateSubject {
            id: 515759,
            name: Some("葬送のフリーレン 第2期".to_string()),
            name_cn: Some("葬送的芙莉莲 第二季".to_string()),
            images_grid: Some(
                "https://lain.bgm.tv/r/100/pic/cover/l/0b/24/515759_qA1Zc.jpg".to_string(),
            ),
            images_large: Some(
                "https://lain.bgm.tv/pic/cover/l/0b/24/515759_qA1Zc.jpg".to_string(),
            ),
            rank: Some(395),
            collection_total: Some(6781),
            drop_rate: Some(0.0017696504940274296),
            meta_tags: vec![
                "TV".to_string(),
                "日本".to_string(),
                "奇幻".to_string(),
                "漫画改".to_string(),
            ],
            score: Some(8.155339805825243),
            average_comment: Some(0.0),
            air_weekday: Some("星期五".to_string()),
            ..Default::default()
        };

        let subject = repo.create(create_subject).await?;

        assert_eq!(subject.id, 515759);
        assert_eq!(subject.name_cn, Some("葬送的芙莉莲 第二季".to_string()));
        assert_eq!(subject.meta_tags[0], "TV".to_string());

        Ok(())
    }

    #[sqlx::test]
    async fn test_upsert_subject(pool: PgPool) -> sqlx::Result<()> {
        let repo = SubjectRepository::new(&pool);

        let create_subject = CreateSubject {
            id: 515759,
            name: Some("葬送のフリーレン 第2期".to_string()),
            name_cn: Some("葬送的芙莉莲 第二季".to_string()),
            images_grid: Some(
                "https://lain.bgm.tv/r/100/pic/cover/l/0b/24/515759_qA1Zc.jpg".to_string(),
            ),
            images_large: Some(
                "https://lain.bgm.tv/pic/cover/l/0b/24/515759_qA1Zc.jpg".to_string(),
            ),
            rank: Some(395),
            collection_total: Some(6781),
            drop_rate: Some(0.0017696504940274296),
            meta_tags: vec![
                "TV".to_string(),
                "日本".to_string(),
                "奇幻".to_string(),
                "漫画改".to_string(),
            ],
            score: Some(8.155339805825243),
            average_comment: Some(0.0),
            air_weekday: Some("星期五".to_string()),
            ..Default::default()
        };

        let subject = repo.create(create_subject).await?;

        assert_eq!(subject.id, 515759);
        assert_eq!(subject.name_cn, Some("葬送的芙莉莲 第二季".to_string()));
        assert_eq!(subject.meta_tags[0], "TV".to_string());

        let create_subject = CreateSubject {
            id: 515759,
            name: Some("葬送のフリーレン 第2期".to_string()),
            name_cn: Some("葬送的芙莉莲 第二季".to_string()),
            images_grid: Some(
                "https://lain.bgm.tv/r/100/pic/cover/l/0b/24/515759_qA1Zc.jpg".to_string(),
            ),
            images_large: Some(
                "https://lain.bgm.tv/pic/cover/l/0b/24/515759_qA1Zc.jpg".to_string(),
            ),
            rank: Some(380),
            collection_total: Some(6781),
            drop_rate: Some(0.0017696504940274296),
            meta_tags: vec![
                "TV".to_string(),
                "日本".to_string(),
                "奇幻".to_string(),
                "漫画改".to_string(),
            ],
            score: Some(8.155339805825243),
            average_comment: Some(0.0),
            air_weekday: Some("星期五".to_string()),
            ..Default::default()
        };

        let subject = repo.upsert(create_subject).await?;

        assert_eq!(subject.id, 515759);
        assert_eq!(subject.rank, Some(380));

        Ok(())
    }

    #[sqlx::test]
    async fn test_find_subject_by_id(pool: PgPool) -> sqlx::Result<()> {
        let repo = SubjectRepository::new(&pool);

        let create_subject = CreateSubject {
            id: 443106,
            name: Some("ゴールデンカムイ 最終章".to_string()),
            name_cn: Some("黄金神威 最终章".to_string()),
            images_grid: Some(
                "https://lain.bgm.tv/r/100/pic/cover/l/7c/f1/443106_7p6M7.jpg".to_string(),
            ),
            images_large: Some(
                "https://lain.bgm.tv/pic/cover/l/7c/f1/443106_7p6M7.jpg".to_string(),
            ),
            rank: Some(999999),
            collection_total: Some(817),
            drop_rate: Some(0.006119951040391677),
            meta_tags: vec![
                "TV".to_string(),
                "日本".to_string(),
                "漫画改".to_string(),
                "战斗".to_string(),
                "冒险".to_string(),
            ],
            score: Some(7.285714285714286),
            average_comment: Some(0.0),
            air_weekday: Some("星期一".to_string()),
            ..Default::default()
        };

        repo.create(create_subject).await?;

        let subject = repo.find_by_id(443106).await?.unwrap();

        assert_eq!(subject.id, 443106);
        assert_eq!(subject.name_cn, Some("黄金神威 最终章".to_string()));
        assert_eq!(subject.meta_tags[0], "TV".to_string());

        let not_found = repo.find_by_id(99999999).await.unwrap();

        assert!(not_found.is_none());

        Ok(())
    }

    #[sqlx::test]
    async fn test_update_subject(pool: PgPool) -> sqlx::Result<()> {
        let repo = SubjectRepository::new(&pool);

        let create_subject = CreateSubject {
            id: 517057,
            name: Some("【推しの子】 第3期".to_string()),
            name_cn: Some("【我推的孩子】 第三季".to_string()),
            images_grid: Some(
                "https://lain.bgm.tv/r/100/pic/cover/l/92/95/517057_257ad.jpg".to_string(),
            ),
            images_large: Some(
                "https://lain.bgm.tv/pic/cover/l/92/95/517057_257ad.jpg".to_string(),
            ),
            rank: Some(999999),
            collection_total: Some(3089),
            drop_rate: Some(0.011330527678860473),
            meta_tags: vec![
                "TV".to_string(),
                "恋爱".to_string(),
                "日本".to_string(),
                "奇幻".to_string(),
                "漫画改".to_string(),
            ],
            score: Some(5.333333333333333),
            average_comment: Some(0.0),
            air_weekday: Some("星期三".to_string()),
            ..Default::default()
        };

        repo.create(create_subject).await?;

        let update_subject = UpdateSubject {
            name: None,
            name_cn: None,
            images_grid: None,
            images_large: None,
            rank: Some(20),
            score: Some(6.333333333333333),
            collection_total: Some(3090),
            average_comment: Some(114.3),
            drop_rate: Some(0.012330527678860473),
            air_weekday: None,
            meta_tags: None,
        };

        let subject = repo.update(517057, update_subject).await?;

        assert_eq!(subject.name_cn, Some("【我推的孩子】 第三季".to_string()));
        assert_eq!(subject.rank, Some(20));
        assert_eq!(subject.meta_tags[0], "TV".to_string());

        Ok(())
    }

    #[sqlx::test]
    async fn test_delete_subject(pool: PgPool) -> sqlx::Result<()> {
        let repo = SubjectRepository::new(&pool);

        let create_subject = CreateSubject {
            id: 548818,
            name: Some("メダリスト 第2期".to_string()),
            name_cn: Some("金牌得主 第二季".to_string()),
            images_grid: Some(
                "https://lain.bgm.tv/r/100/pic/cover/l/0c/08/548818_iLSG6.jpg".to_string(),
            ),
            images_large: Some(
                "https://lain.bgm.tv/pic/cover/l/0c/08/548818_iLSG6.jpg".to_string(),
            ),
            rank: Some(999999),
            collection_total: Some(2536),
            drop_rate: Some(0.003943217665615142),
            meta_tags: vec![
                "TV".to_string(),
                "日本".to_string(),
                "运动".to_string(),
                "漫画改".to_string(),
            ],
            score: Some(8.11111111111111),
            average_comment: Some(0.0),
            air_weekday: Some("星期六".to_string()),
            ..Default::default()
        };

        repo.create(create_subject).await?;

        let result = repo.delete(548818).await?;

        assert_eq!(result, true);

        let not_found = repo.find_by_id(548818).await?;

        assert!(not_found.is_none());

        Ok(())
    }
}
