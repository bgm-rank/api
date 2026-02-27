use crate::dal::dto::{CreateSubject, Subject, UpdateSubject};
use sqlx::PgPool;

pub struct SubjectRepository<'a> {
    pool: &'a PgPool,
}

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
                drop_rate, air_weekday, meta_tags)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
            RETURNING id, name, name_cn, images_grid, images_large,
                rank, score, collection_total, average_comment,
                drop_rate, air_weekday, meta_tags, updated_at
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
                drop_rate, air_weekday, meta_tags
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
            ON CONFLICT (id) DO UPDATE SET
                name = EXCLUDED.name,
                name_cn = EXCLUDED.name_cn,
                images_grid = EXCLUDED.images_grid,
                images_large = EXCLUDED.images_large,
                rank = EXCLUDED.rank,
                score = EXCLUDED.score,
                collection_total = EXCLUDED.collection_total,
                average_comment = EXCLUDED.average_comment,
                drop_rate = EXCLUDED.drop_rate,
                air_weekday = EXCLUDED.air_weekday,
                meta_tags = EXCLUDED.meta_tags,
                updated_at = CURRENT_TIMESTAMP
            RETURNING id, name, name_cn, images_grid, images_large,
                rank, score, collection_total, average_comment,
                drop_rate, air_weekday, meta_tags, updated_at
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
        .fetch_one(self.pool)
        .await?;

        Ok(row)
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
        };

        repo.create(create_subject).await?;

        let result = repo.delete(548818).await?;

        assert_eq!(result, true);

        let not_found = repo.find_by_id(548818).await?;

        assert!(not_found.is_none());

        Ok(())
    }
}
