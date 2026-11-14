use super::log_db_error;
use crate::dal::dto::{CreateSubject, Subject, UpdateSubject};
use sqlx::SqlitePool;

pub struct SubjectRepository<'a> {
    pool: &'a SqlitePool,
}

#[allow(dead_code)]
impl<'a> SubjectRepository<'a> {
    pub fn new(pool: &'a SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn create(&self, subject: CreateSubject) -> Result<Subject, sqlx::Error> {
        let meta_tags_json = serde_json::to_string(&subject.meta_tags).unwrap_or_default();
        let row = sqlx::query_as::<_, Subject>(
            r#"
            INSERT INTO subjects (
                id, name, name_cn, air_date, platform, summary, tags, nsfw,
                images_grid, images_large, rank, score, collection_total,
                average_comment, drop_rate, air_weekday, meta_tags
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            RETURNING *
            "#,
        )
        .bind(subject.id)
        .bind(subject.name)
        .bind(subject.name_cn)
        .bind(subject.air_date)
        .bind(subject.platform)
        .bind(subject.summary)
        .bind(subject.tags)
        .bind(subject.nsfw)
        .bind(subject.images_grid)
        .bind(subject.images_large)
        .bind(subject.rank)
        .bind(subject.score)
        .bind(subject.collection_total)
        .bind(subject.average_comment)
        .bind(subject.drop_rate)
        .bind(subject.air_weekday)
        .bind(meta_tags_json)
        .fetch_one(self.pool)
        .await?;

        Ok(row)
    }

    pub async fn upsert(&self, subject: CreateSubject) -> Result<Subject, sqlx::Error> {
        let meta_tags_json = serde_json::to_string(&subject.meta_tags).unwrap_or_default();
        let row = sqlx::query_as::<_, Subject>(
            r#"
            INSERT INTO subjects (
                id, name, name_cn, air_date, platform, summary, tags, nsfw,
                images_grid, images_large, rank, score, collection_total,
                average_comment, drop_rate, air_weekday, meta_tags
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT (id) DO UPDATE SET
                name = EXCLUDED.name,
                name_cn = EXCLUDED.name_cn,
                air_date = EXCLUDED.air_date,
                platform = EXCLUDED.platform,
                summary = EXCLUDED.summary,
                tags = EXCLUDED.tags,
                nsfw = EXCLUDED.nsfw,
                images_grid = EXCLUDED.images_grid,
                images_large = EXCLUDED.images_large,
                rank = EXCLUDED.rank,
                score = EXCLUDED.score,
                collection_total = EXCLUDED.collection_total,
                average_comment = EXCLUDED.average_comment,
                drop_rate = EXCLUDED.drop_rate,
                air_weekday = EXCLUDED.air_weekday,
                meta_tags = CASE WHEN json_array_length(EXCLUDED.meta_tags) > 0 THEN EXCLUDED.meta_tags ELSE subjects.meta_tags END,
                updated_at = strftime('%Y-%m-%d %H:%M:%f', 'now')
            RETURNING *
            "#,
        )
        .bind(subject.id)
        .bind(subject.name)
        .bind(subject.name_cn)
        .bind(subject.air_date)
        .bind(subject.platform)
        .bind(subject.summary)
        .bind(subject.tags)
        .bind(subject.nsfw)
        .bind(subject.images_grid)
        .bind(subject.images_large)
        .bind(subject.rank)
        .bind(subject.score)
        .bind(subject.collection_total)
        .bind(subject.average_comment)
        .bind(subject.drop_rate)
        .bind(subject.air_weekday)
        .bind(meta_tags_json)
        .fetch_one(self.pool)
        .await
        .inspect_err(|e| log_db_error("upsert", "subjects", e))?;

        Ok(row)
    }

    pub async fn find_due_for_update(
        &self,
        current_season_id: i32,
    ) -> Result<Vec<(i32, i32, Option<chrono::DateTime<chrono::Utc>>)>, sqlx::Error> {
        use chrono::Utc;

        let rows = sqlx::query_as::<_, (i32, i32, Option<chrono::DateTime<Utc>>)>(
            r#"
            SELECT s.id, MAX(sm.season_id) as newest_season_id, s.last_updated_at
            FROM subjects s
            JOIN mal_bgm_matches mbm ON mbm.bgm_id = s.id
            JOIN season_mal sm       ON sm.mal_id = mbm.mal_id
            WHERE mbm.status = 'included'
              AND sm.is_removed = 0
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
        let now = chrono::Utc::now();
        sqlx::query("UPDATE subjects SET last_updated_at = ? WHERE id = ?")
            .bind(now)
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
                SELECT 1
                FROM mal_bgm_matches mbm
                WHERE mbm.bgm_id = s.id
                  AND mbm.bgm_id IS NOT NULL
            )
            "#,
        )
        .fetch_all(self.pool)
        .await
        .inspect_err(|e| log_db_error("find_orphans", "subjects", e))?;

        Ok(rows)
    }

    pub async fn delete_orphans(&self) -> Result<u64, sqlx::Error> {
        let mut tx = self.pool.begin().await?;

        let rows = sqlx::query_scalar::<_, i32>(
            r#"
            DELETE FROM subjects
            WHERE NOT EXISTS (
                SELECT 1
                FROM mal_bgm_matches mbm
                WHERE mbm.bgm_id = subjects.id
                  AND mbm.bgm_id IS NOT NULL
            )
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
        let ids_json = serde_json::to_string(ids).unwrap_or_default();
        let rows = sqlx::query_as::<_, Subject>(
            "SELECT * FROM subjects WHERE id IN (SELECT value FROM json_each(?))",
        )
        .bind(ids_json)
        .fetch_all(self.pool)
        .await?;

        Ok(rows)
    }

    pub async fn find_by_id(&self, subject_id: i32) -> Result<Option<Subject>, sqlx::Error> {
        let row = sqlx::query_as::<_, Subject>(
            r#"
            SELECT * FROM subjects
            WHERE id = ?
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
        let meta_tags_json = subject
            .meta_tags
            .as_ref()
            .map(|t| serde_json::to_string(t).unwrap_or_default());
        let row = sqlx::query_as(
            r#"
            UPDATE subjects
            SET
                name = COALESCE(?, name),
                name_cn = COALESCE(?, name_cn),
                images_grid = COALESCE(?, images_grid),
                images_large = COALESCE(?, images_large),
                rank = COALESCE(?, rank),
                score = COALESCE(?, score),
                collection_total = COALESCE(?, collection_total),
                average_comment = COALESCE(?, average_comment),
                drop_rate = COALESCE(?, drop_rate),
                air_weekday = COALESCE(?, air_weekday),
                meta_tags = COALESCE(?, meta_tags)
            WHERE id = ?
            RETURNING *
            "#,
        )
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
        .bind(meta_tags_json)
        .bind(subject_id)
        .fetch_one(self.pool)
        .await?;

        Ok(row)
    }

    pub async fn delete(&self, subject_id: i32) -> Result<bool, sqlx::Error> {
        let mut tx = self.pool.begin().await?;

        sqlx::query(
            r#"
            UPDATE mal_bgm_matches
            SET
                status = 'pending',
                bgm_id = NULL,
                source = NULL,
                confidence = NULL,
                updated_at = strftime('%Y-%m-%d %H:%M:%f', 'now')
            WHERE bgm_id = ?
            "#,
        )
        .bind(subject_id)
        .execute(&mut *tx)
        .await
        .inspect_err(|e| log_db_error("delete_reset_matches", "mal_bgm_matches", e))?;

        let result = sqlx::query(
            r#"
            DELETE FROM subjects WHERE id = ?
            "#,
        )
        .bind(subject_id)
        .execute(&mut *tx)
        .await
        .inspect_err(|e| log_db_error("delete", "subjects", e))?;

        tx.commit().await?;

        Ok(result.rows_affected() > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dal::Database;
    use crate::dal::dto::{CreateSeason, UpdateMalBgmMatch, UpsertMalAnime};
    use crate::dal::repositories::{
        MalAnimeRepository, MalBgmMatchRepository, SeasonMalRepository, SeasonRepository,
    };

    async fn link_season_subject(
        pool: &SqlitePool,
        season_id: i32,
        mal_id: i32,
        subject_id: i32,
        status: &str,
        is_removed: bool,
    ) -> sqlx::Result<()> {
        MalAnimeRepository::new(pool)
            .upsert(UpsertMalAnime {
                mal_id,
                title: format!("MAL {}", mal_id),
                media_type: "tv".to_string(),
                ..Default::default()
            })
            .await?;

        let sm_repo = SeasonMalRepository::new(pool);
        sm_repo.insert_ignore(season_id, mal_id).await?;
        if is_removed {
            sm_repo.set_removed(season_id, mal_id, true, None).await?;
        }

        MalBgmMatchRepository::new(pool)
            .upsert(
                mal_id,
                UpdateMalBgmMatch {
                    status: status.to_string(),
                    source: Some("exact".to_string()),
                    bgm_id: Some(subject_id),
                    confidence: Some(1.0),
                    ..Default::default()
                },
            )
            .await?;

        Ok(())
    }

    #[sqlx::test]
    async fn test_find_orphans(pool: SqlitePool) -> sqlx::Result<()> {
        let db = Database::from_pool(pool);
        SeasonRepository::new(db.pool())
            .create(CreateSeason {
                season_id: 202601,
                year: 2026,
                season: "WINTER".to_string(),
                name: None,
            })
            .await?;

        let repo = SubjectRepository::new(db.pool());
        for id in [1, 2, 10, 11, 12, 13, 14] {
            repo.create(CreateSubject {
                id,
                ..Default::default()
            })
            .await?;
        }

        // 1: active season_mal + included match -> NOT orphan
        link_season_subject(db.pool(), 202601, 101, 1, "included", false).await?;
        // 2: one removed + one active, both included -> NOT orphan
        link_season_subject(db.pool(), 202601, 102, 2, "included", true).await?;
        link_season_subject(db.pool(), 202601, 103, 2, "included", false).await?;
        // 10: no match -> orphan
        // 11: active season_mal + excluded match with bgm_id -> NOT orphan (referenced by match)
        link_season_subject(db.pool(), 202601, 111, 11, "excluded", false).await?;
        // 12: removed season_mal + included match with bgm_id -> NOT orphan (referenced by match)
        link_season_subject(db.pool(), 202601, 112, 12, "included", true).await?;
        // 13: active season_mal + pending match with bgm_id -> NOT orphan (referenced by match)
        link_season_subject(db.pool(), 202601, 113, 13, "pending", false).await?;
        // 14: exists only alongside a match with bgm_id IS NULL -> orphan
        MalAnimeRepository::new(db.pool())
            .upsert(UpsertMalAnime {
                mal_id: 114,
                title: "MAL 114".to_string(),
                media_type: "tv".to_string(),
                ..Default::default()
            })
            .await?;
        MalBgmMatchRepository::new(db.pool())
            .insert_pending_ignore(114)
            .await?;

        let orphans = repo.find_orphans().await?;
        assert_eq!(orphans.len(), 2);
        let ids: Vec<i32> = orphans.iter().map(|s| s.id).collect();
        assert!(ids.contains(&10));
        assert!(ids.contains(&14));
        Ok(())
    }

    #[sqlx::test]
    async fn test_delete_orphans(pool: SqlitePool) -> sqlx::Result<()> {
        let db = Database::from_pool(pool);
        SeasonRepository::new(db.pool())
            .create(CreateSeason {
                season_id: 202601,
                year: 2026,
                season: "WINTER".to_string(),
                name: None,
            })
            .await?;

        let repo = SubjectRepository::new(db.pool());
        for id in [1, 10, 11, 12, 13] {
            repo.create(CreateSubject {
                id,
                ..Default::default()
            })
            .await?;
        }

        // 1: active included -> kept
        link_season_subject(db.pool(), 202601, 101, 1, "included", false).await?;
        // 10: unlinked -> deleted
        // 11: excluded with bgm_id -> kept (referenced by mal_bgm_matches)
        link_season_subject(db.pool(), 202601, 111, 11, "excluded", false).await?;
        // 12: removed season_mal + included with bgm_id -> kept (referenced by mal_bgm_matches)
        link_season_subject(db.pool(), 202601, 112, 12, "included", true).await?;
        // 13: pending with bgm_id -> kept (referenced by mal_bgm_matches)
        link_season_subject(db.pool(), 202601, 113, 13, "pending", false).await?;

        let deleted = repo.delete_orphans().await?;
        assert_eq!(deleted, 1);

        let orphans = repo.find_orphans().await?;
        assert!(orphans.is_empty());
        assert!(repo.find_by_id(1).await?.is_some());
        assert!(repo.find_by_id(10).await?.is_none());
        assert!(repo.find_by_id(11).await?.is_some());
        assert!(repo.find_by_id(12).await?.is_some());
        assert!(repo.find_by_id(13).await?.is_some());

        let m11 = MalBgmMatchRepository::new(db.pool())
            .get(111)
            .await?
            .unwrap();
        assert_eq!(m11.bgm_id, Some(11));
        let m12 = MalBgmMatchRepository::new(db.pool())
            .get(112)
            .await?
            .unwrap();
        assert_eq!(m12.bgm_id, Some(12));
        Ok(())
    }

    #[sqlx::test]
    async fn test_find_due_for_update(pool: SqlitePool) -> sqlx::Result<()> {
        let db = Database::from_pool(pool);
        SeasonRepository::new(db.pool())
            .create(CreateSeason {
                season_id: 202601,
                year: 2026,
                season: "WINTER".to_string(),
                name: None,
            })
            .await?;
        SeasonRepository::new(db.pool())
            .create(CreateSeason {
                season_id: 202504,
                year: 2025,
                season: "SPRING".to_string(),
                name: None,
            })
            .await?;

        let repo = SubjectRepository::new(db.pool());
        for id in [1, 2, 3, 4, 5, 6] {
            repo.create(CreateSubject {
                id,
                ..Default::default()
            })
            .await?;
        }

        // 1: current season (202601), active included -> due
        link_season_subject(db.pool(), 202601, 201, 1, "included", false).await?;

        // 2: old season (202504), stale (4 days) -> due
        link_season_subject(db.pool(), 202504, 202, 2, "included", false).await?;
        sqlx::query(
            "UPDATE subjects SET last_updated_at = datetime('now', '-4 days') WHERE id = ?",
        )
        .bind(2)
        .execute(db.pool())
        .await?;

        // 3: old season (202504), fresh (1 day) -> NOT due
        link_season_subject(db.pool(), 202504, 203, 3, "included", false).await?;
        sqlx::query("UPDATE subjects SET last_updated_at = datetime('now', '-1 day') WHERE id = ?")
            .bind(3)
            .execute(db.pool())
            .await?;

        // 4: multi-season N:1 (202504 + 202601), fresh (1 day) -> MAX(season_id)=202601 -> due
        link_season_subject(db.pool(), 202504, 204, 4, "included", false).await?;
        link_season_subject(db.pool(), 202601, 205, 4, "included", false).await?;
        sqlx::query("UPDATE subjects SET last_updated_at = datetime('now', '-1 day') WHERE id = ?")
            .bind(4)
            .execute(db.pool())
            .await?;

        // 5: removed in 202601 -> NOT due
        link_season_subject(db.pool(), 202601, 206, 5, "included", true).await?;

        // 6: excluded in 202601 -> NOT due
        link_season_subject(db.pool(), 202601, 207, 6, "excluded", false).await?;

        let due = repo.find_due_for_update(202601).await?;
        let due_ids: Vec<i32> = due.iter().map(|(id, _, _)| *id).collect();

        assert!(
            due_ids.contains(&1),
            "Subject 1 (current season) should be due"
        );
        assert!(due_ids.contains(&2), "Subject 2 (stale) should be due");
        assert!(!due_ids.contains(&3), "Subject 3 (fresh) should NOT be due");
        assert!(
            due_ids.contains(&4),
            "Subject 4 (newest season 202601) should be due"
        );
        assert!(
            !due_ids.contains(&5),
            "Subject 5 (removed) should NOT be due"
        );
        assert!(
            !due_ids.contains(&6),
            "Subject 6 (excluded) should NOT be due"
        );

        let sub4 = due.iter().find(|(id, _, _)| *id == 4).unwrap();
        assert_eq!(sub4.1, 202601);

        Ok(())
    }

    #[sqlx::test]
    async fn test_update_last_updated_at(pool: SqlitePool) -> sqlx::Result<()> {
        let db = Database::from_pool(pool);
        let repo = SubjectRepository::new(db.pool());
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
    async fn test_create_subject(pool: SqlitePool) -> sqlx::Result<()> {
        let db = Database::from_pool(pool);
        let repo = SubjectRepository::new(db.pool());

        let create_subject = CreateSubject {
            id: 515759,
            name: Some("葬送のフリーレン 第2期".to_string()),
            name_cn: Some("葬送的芙莉莲 第二季".to_string()),
            air_date: Some("2026-01-16".to_string()),
            platform: Some("TV".to_string()),
            summary: Some("旅途继续".to_string()),
            tags: Some(r#"[{"name":"奇幻","count":100}]"#.to_string()),
            nsfw: Some(false),
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
        assert_eq!(subject.air_date, Some("2026-01-16".to_string()));
        assert_eq!(subject.platform, Some("TV".to_string()));
        assert_eq!(subject.summary, Some("旅途继续".to_string()));
        assert_eq!(subject.nsfw, Some(false));
        assert_eq!(subject.meta_tags[0], "TV".to_string());

        Ok(())
    }

    #[sqlx::test]
    async fn test_upsert_subject(pool: SqlitePool) -> sqlx::Result<()> {
        let db = Database::from_pool(pool);
        let repo = SubjectRepository::new(db.pool());

        let create_subject = CreateSubject {
            id: 515759,
            name: Some("葬送のフリーレン 第2期".to_string()),
            name_cn: Some("葬送的芙莉莲 第二季".to_string()),
            air_date: Some("2026-01-16".to_string()),
            platform: Some("TV".to_string()),
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
            air_date: Some("2026-01-16".to_string()),
            platform: Some("WEB".to_string()),
            nsfw: Some(false),
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
        assert_eq!(subject.platform, Some("WEB".to_string()));
        assert_eq!(subject.nsfw, Some(false));

        Ok(())
    }

    #[sqlx::test]
    async fn test_find_subject_by_id(pool: SqlitePool) -> sqlx::Result<()> {
        let db = Database::from_pool(pool);
        let repo = SubjectRepository::new(db.pool());

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
    async fn test_update_subject(pool: SqlitePool) -> sqlx::Result<()> {
        let db = Database::from_pool(pool);
        let repo = SubjectRepository::new(db.pool());

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
    async fn test_score_float_precision_survives_roundtrip(pool: SqlitePool) -> sqlx::Result<()> {
        let db = Database::from_pool(pool);
        let repo = SubjectRepository::new(db.pool());

        let score = 8.123456f64;
        repo.create(CreateSubject {
            id: 99901,
            score: Some(score),
            ..Default::default()
        })
        .await?;

        let subject = repo.find_by_id(99901).await?.unwrap();
        let roundtripped = subject.score.expect("score should be set");
        assert!(
            (roundtripped - score).abs() < 0.0001,
            "Expected score ≈ {score}, got {roundtripped}"
        );

        Ok(())
    }

    #[sqlx::test]
    async fn test_delete_subject(pool: SqlitePool) -> sqlx::Result<()> {
        let db = Database::from_pool(pool);
        let repo = SubjectRepository::new(db.pool());

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

        assert!(result);

        let not_found = repo.find_by_id(548818).await?;

        assert!(not_found.is_none());

        Ok(())
    }

    #[sqlx::test]
    async fn test_delete_subject_resets_referencing_matches(pool: SqlitePool) -> sqlx::Result<()> {
        let db = Database::from_pool(pool);
        SeasonRepository::new(db.pool())
            .create(CreateSeason {
                season_id: 202601,
                year: 2026,
                season: "WINTER".to_string(),
                name: None,
            })
            .await?;

        let repo = SubjectRepository::new(db.pool());
        repo.create(CreateSubject {
            id: 548818,
            ..Default::default()
        })
        .await?;

        link_season_subject(db.pool(), 202601, 9001, 548818, "included", false).await?;
        link_season_subject(db.pool(), 202601, 9002, 548818, "excluded", false).await?;

        let match_repo = MalBgmMatchRepository::new(db.pool());
        let before_9001 = match_repo.get(9001).await?.unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        assert!(repo.delete(548818).await?);
        assert!(repo.find_by_id(548818).await?.is_none());

        for mal_id in [9001, 9002] {
            let m = match_repo.get(mal_id).await?.unwrap();
            assert_eq!(m.status, "pending");
            assert_eq!(m.bgm_id, None);
            assert_eq!(m.source, None);
            assert_eq!(m.confidence, None);
            assert!(m.updated_at > before_9001.updated_at);
        }

        // Deleting non-existent subject returns false
        assert!(!repo.delete(548818).await?);

        Ok(())
    }
}
