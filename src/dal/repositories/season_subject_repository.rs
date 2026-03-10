use crate::dal::dto::{CreateSeasonSubject, SeasonSubject, Subject};
use sqlx::PgPool;

fn log_db_error(operation: &'static str, table: &'static str, e: &sqlx::Error) {
    match e {
        sqlx::Error::RowNotFound => {
            tracing::debug!(operation, table, error = %e, "db error");
        }
        sqlx::Error::Database(db_err) if db_err.is_unique_violation() => {
            tracing::warn!(operation, table, error = %e, "db error");
        }
        _ => {
            tracing::error!(operation, table, error = %e, "db error");
        }
    }
}

pub struct SeasonSubjectRepository<'a> {
    pool: &'a PgPool,
}

#[allow(dead_code)]
impl<'a> SeasonSubjectRepository<'a> {
    pub fn new(pool: &'a PgPool) -> Self {
        Self { pool }
    }

    pub async fn create(
        &self,
        season_subject: CreateSeasonSubject,
    ) -> Result<SeasonSubject, sqlx::Error> {
        let row = sqlx::query_as::<_, SeasonSubject>(
            r#"
            INSERT INTO season_subjects (season_id, subject_id)
            VALUES ($1, $2)
            RETURNING season_id, subject_id, added_at
            "#,
        )
        .bind(season_subject.season_id)
        .bind(season_subject.subject_id)
        .fetch_one(self.pool)
        .await?;

        Ok(row)
    }

    pub async fn insert_or_ignore(
        &self,
        season_subject: CreateSeasonSubject,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            r#"
            INSERT INTO season_subjects (season_id, subject_id)
            VALUES ($1, $2)
            ON CONFLICT (season_id, subject_id) DO NOTHING
            "#,
        )
        .bind(season_subject.season_id)
        .bind(season_subject.subject_id)
        .execute(self.pool)
        .await?;

        Ok(())
    }

    pub async fn reconcile(
        &self,
        season_id: i32,
        new_subject_ids: Vec<i32>,
    ) -> Result<(usize, usize), sqlx::Error> {
        use std::collections::HashSet;

        let current: HashSet<i32> = self
            .find_by_season_id(season_id)
            .await?
            .into_iter()
            .collect();
        let new_set: HashSet<i32> = new_subject_ids.into_iter().collect();

        let to_add: Vec<i32> = new_set.difference(&current).copied().collect();
        let to_remove: Vec<i32> = current.difference(&new_set).copied().collect();

        let mut tx = self.pool.begin().await
            .inspect_err(|e| log_db_error("reconcile_begin_tx", "season_subjects", e))?;

        for subject_id in &to_add {
            sqlx::query(
                "INSERT INTO season_subjects (season_id, subject_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
            )
            .bind(season_id)
            .bind(subject_id)
            .execute(&mut *tx)
            .await
            .inspect_err(|e| log_db_error("reconcile_insert", "season_subjects", e))?;
        }

        if !to_remove.is_empty() {
            sqlx::query(
                "DELETE FROM season_subjects WHERE season_id = $1 AND subject_id = ANY($2)",
            )
            .bind(season_id)
            .bind(&to_remove)
            .execute(&mut *tx)
            .await
            .inspect_err(|e| log_db_error("reconcile_delete", "season_subjects", e))?;
        }

        tx.commit().await
            .inspect_err(|e| log_db_error("reconcile_commit", "season_subjects", e))?;
        Ok((to_add.len(), to_remove.len()))
    }

    pub async fn find_by_season_id(&self, season_id: i32) -> Result<Vec<i32>, sqlx::Error> {
        let ids = sqlx::query_scalar::<_, i32>(
            "SELECT subject_id FROM season_subjects WHERE season_id = $1",
        )
        .bind(season_id)
        .fetch_all(self.pool)
        .await?;

        Ok(ids)
    }

    pub async fn find_by_season(&self, season_id: i32) -> Result<Vec<Subject>, sqlx::Error> {
        let row = sqlx::query_as::<_, Subject>(
            r#"
            SELECT s.*
            FROM subjects s
            JOIN season_subjects ss ON s.id = ss.subject_id
            WHERE ss.season_id = $1
            ORDER BY s.rank ASC, s.collection_total DESC
            "#,
        )
        .bind(season_id)
        .fetch_all(self.pool)
        .await?;

        Ok(row)
    }

    pub async fn delete_and_cleanup(
        &self,
        season_id: i32,
        subject_id: i32,
    ) -> Result<bool, sqlx::Error> {
        let mut tx = self.pool.begin().await?;

        sqlx::query("DELETE FROM season_subjects WHERE season_id = $1 AND subject_id = $2")
            .bind(season_id)
            .bind(subject_id)
            .execute(&mut *tx)
            .await?;

        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM season_subjects WHERE subject_id = $1")
                .bind(subject_id)
                .fetch_one(&mut *tx)
                .await?;

        if count == 0 {
            sqlx::query("DELETE FROM subjects WHERE id = $1")
                .bind(subject_id)
                .execute(&mut *tx)
                .await?;
        }

        tx.commit().await?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dal::dto::{CreateSeason, CreateSubject};
    use crate::dal::repositories::{SeasonRepository, SubjectRepository};

    async fn create_test_season(pool: &PgPool) -> sqlx::Result<()> {
        let repo = SeasonRepository::new(&pool);

        let create_season = CreateSeason {
            season_id: 202601,
            year: 2026,
            season: "WINTER".to_string(),
            name: Some("2026年冬季番".to_string()),
        };

        repo.create(create_season).await?;

        Ok(())
    }

    async fn create_test_subjects(pool: &PgPool) -> sqlx::Result<()> {
        let repo = SubjectRepository::new(pool);

        let create_subjects = vec![
            CreateSubject {
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
            },
            CreateSubject {
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
            },
            CreateSubject {
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
            },
            CreateSubject {
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
            },
        ];

        for create_subject in create_subjects.into_iter() {
            repo.create(create_subject).await?;
        }

        Ok(())
    }

    // T014 🔴 reconcile 红灯测试
    #[sqlx::test]
    async fn test_reconcile(pool: PgPool) -> sqlx::Result<()> {
        create_test_season(&pool).await?;
        create_test_subjects(&pool).await?;

        let repo = SeasonSubjectRepository::new(&pool);

        // 初始关联：A=443106, B=515759, C=517057
        for sid in [443106, 515759, 517057] {
            repo.create(CreateSeasonSubject {
                season_id: 202601,
                subject_id: sid,
            })
            .await?;
        }

        // reconcile: 保留 B/C, 添加 D=548818, 删除 A=443106
        let (added, removed) = repo.reconcile(202601, vec![515759, 517057, 548818]).await?;

        assert_eq!(added, 1);
        assert_eq!(removed, 1);

        let remaining = repo.find_by_season_id(202601).await?;
        assert!(!remaining.contains(&443106), "A 应被删除");
        assert!(remaining.contains(&548818), "D 应被添加");
        assert!(remaining.contains(&515759), "B 应保留");
        assert!(remaining.contains(&517057), "C 应保留");

        Ok(())
    }

    // T006 🔴 find_by_season_id 红灯测试
    #[sqlx::test]
    async fn test_find_by_season_id(pool: PgPool) -> sqlx::Result<()> {
        create_test_season(&pool).await?;
        create_test_subjects(&pool).await?;

        let repo = SeasonSubjectRepository::new(&pool);

        repo.create(CreateSeasonSubject {
            season_id: 202601,
            subject_id: 443106,
        })
        .await?;
        repo.create(CreateSeasonSubject {
            season_id: 202601,
            subject_id: 515759,
        })
        .await?;

        let subject_ids = repo.find_by_season_id(202601).await?;

        assert_eq!(subject_ids.len(), 2);
        assert!(subject_ids.contains(&443106));
        assert!(subject_ids.contains(&515759));

        Ok(())
    }

    #[sqlx::test]
    async fn test_create_season_subject(pool: PgPool) -> sqlx::Result<()> {
        create_test_season(&pool).await?;
        create_test_subjects(&pool).await?;

        let repo = SeasonSubjectRepository::new(&pool);

        let create_season_subject = CreateSeasonSubject {
            season_id: 202601,
            subject_id: 515759,
        };

        let season_subject = repo.create(create_season_subject).await?;

        assert_eq!(season_subject.season_id, 202601);
        assert_eq!(season_subject.subject_id, 515759);

        Ok(())
    }

    #[sqlx::test]
    async fn test_find_all_subjects_by_season_id(pool: PgPool) -> sqlx::Result<()> {
        create_test_season(&pool).await?;
        create_test_subjects(&pool).await?;

        let repo = SeasonSubjectRepository::new(&pool);

        let create_season_subjects = vec![
            CreateSeasonSubject {
                season_id: 202601,
                subject_id: 443106,
            },
            CreateSeasonSubject {
                season_id: 202601,
                subject_id: 517057,
            },
            CreateSeasonSubject {
                season_id: 202601,
                subject_id: 548818,
            },
        ];

        for create_season_subject in create_season_subjects.into_iter() {
            repo.create(create_season_subject).await?;
        }

        let subjects = repo.find_by_season(202601).await?;

        assert_eq!(subjects.len(), 3);

        Ok(())
    }

    #[sqlx::test]
    async fn test_insert_or_ignore(pool: PgPool) -> sqlx::Result<()> {
        create_test_season(&pool).await?;
        create_test_subjects(&pool).await?;

        let repo = SeasonSubjectRepository::new(&pool);

        let entry = CreateSeasonSubject {
            season_id: 202601,
            subject_id: 515759,
        };

        // 第一次插入成功
        repo.insert_or_ignore(entry).await?;

        // 重复插入不报错
        let entry = CreateSeasonSubject {
            season_id: 202601,
            subject_id: 515759,
        };
        repo.insert_or_ignore(entry).await?;

        // 确认只有一条记录
        let subjects = repo.find_by_season(202601).await?;
        assert_eq!(subjects.len(), 1);

        Ok(())
    }

    #[sqlx::test]
    async fn test_delete_and_cleanup(pool: PgPool) -> sqlx::Result<()> {
        create_test_season(&pool).await?;
        create_test_subjects(&pool).await?;

        let repo = SeasonSubjectRepository::new(&pool);

        let create_season_subjects = vec![
            CreateSeasonSubject {
                season_id: 202601,
                subject_id: 443106,
            },
            CreateSeasonSubject {
                season_id: 202601,
                subject_id: 517057,
            },
            CreateSeasonSubject {
                season_id: 202601,
                subject_id: 548818,
            },
        ];

        for create_season_subject in create_season_subjects.into_iter() {
            repo.create(create_season_subject).await?;
        }

        repo.delete_and_cleanup(202601, 443106).await?;

        let subjects = repo.find_by_season(202601).await?;

        assert_eq!(subjects.len(), 2);

        Ok(())
    }
}
