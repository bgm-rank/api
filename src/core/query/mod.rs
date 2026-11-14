use anyhow::Result;
use std::sync::Arc;

use crate::api::schemas::{PublicSeasonResponse, PublicSubjectItem, SeasonTop1Item};
use crate::core::sync::dedup_preserving_order;
use crate::dal::{Database, SeasonRepository};

/// Maps a raw MAL `rating` string (`g`, `pg`, `pg_13`, `r`, `r+`, `rx`, or `None`)
/// to the frontend display bucket (`kids`, `r18`, or `general`).
pub fn map_mal_rating(raw_rating: Option<&str>) -> &'static str {
    match raw_rating {
        Some("g") | Some("pg") => "kids",
        Some("r+") | Some("rx") => "r18",
        // "pg_13", "r", None 或其他值统一归为 general
        _ => "general",
    }
}

#[derive(sqlx::FromRow)]
struct PublicSubjectRow {
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
    media_type: String,
    raw_mal_rating: Option<String>,
}

impl From<PublicSubjectRow> for PublicSubjectItem {
    fn from(r: PublicSubjectRow) -> Self {
        let rating = map_mal_rating(r.raw_mal_rating.as_deref()).to_string();
        Self {
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
            media_type: Some(r.media_type),
            rating: Some(rating),
        }
    }
}

#[derive(sqlx::FromRow)]
struct Top1Row {
    season_id: i32,
    #[sqlx(flatten)]
    subject: PublicSubjectRow,
}

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

        let rows = sqlx::query_as::<_, PublicSubjectRow>(
            r#"
            SELECT
                id, name, name_cn, images_grid, images_large,
                rank, score, collection_total, average_comment,
                drop_rate, air_weekday, meta_tags,
                media_type, raw_mal_rating
            FROM (
                SELECT
                    s.id, s.name, s.name_cn, s.images_grid, s.images_large,
                    s.rank, s.score, s.collection_total, s.average_comment,
                    s.drop_rate, s.air_weekday, s.meta_tags,
                    m.media_type,
                    m.rating AS raw_mal_rating,
                    ROW_NUMBER() OVER (
                        PARTITION BY s.id
                        ORDER BY CASE m.media_type
                            WHEN 'tv' THEN 1
                            WHEN 'movie' THEN 2
                            WHEN 'ova' THEN 3
                            WHEN 'ona' THEN 4
                            ELSE 5
                        END ASC, m.mal_id ASC
                    ) AS rn
                FROM season_mal sm
                JOIN mal_bgm_matches mbm ON mbm.mal_id = sm.mal_id
                JOIN mal_anime m         ON m.mal_id = sm.mal_id
                JOIN subjects s          ON s.id = mbm.bgm_id
                WHERE sm.season_id = ?
                  AND sm.is_removed = 0
                  AND mbm.status = 'included'
            )
            WHERE rn = 1
            ORDER BY rank ASC NULLS LAST, id ASC
            "#,
        )
        .bind(season_id)
        .fetch_all(pool)
        .await?;

        let items = rows.into_iter().map(PublicSubjectItem::from).collect();
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

        let rows = sqlx::query_as::<_, Top1Row>(
            r#"
            SELECT season_id, id, name, name_cn, images_grid, images_large,
                   rank, score, collection_total, average_comment,
                   drop_rate, air_weekday, meta_tags, media_type, raw_mal_rating
            FROM (
                SELECT season_id, id, name, name_cn, images_grid, images_large,
                       rank, score, collection_total, average_comment,
                       drop_rate, air_weekday, meta_tags, media_type, raw_mal_rating,
                       ROW_NUMBER() OVER (
                           PARTITION BY season_id
                           ORDER BY rank ASC NULLS LAST, collection_total DESC NULLS LAST, id ASC
                       ) AS top_rn
                FROM (
                    SELECT sm.season_id,
                           s.id, s.name, s.name_cn, s.images_grid, s.images_large,
                           s.rank, s.score, s.collection_total, s.average_comment,
                           s.drop_rate, s.air_weekday, s.meta_tags,
                           m.media_type,
                           m.rating AS raw_mal_rating,
                           ROW_NUMBER() OVER (
                               PARTITION BY sm.season_id, s.id
                               ORDER BY CASE m.media_type
                                   WHEN 'tv' THEN 1
                                   WHEN 'movie' THEN 2
                                   WHEN 'ova' THEN 3
                                   WHEN 'ona' THEN 4
                                   ELSE 5
                               END ASC, m.mal_id ASC
                           ) AS media_rn
                    FROM season_mal sm
                    JOIN mal_bgm_matches mbm ON mbm.mal_id = sm.mal_id
                    JOIN mal_anime m         ON m.mal_id = sm.mal_id
                    JOIN subjects s          ON s.id = mbm.bgm_id
                    WHERE sm.is_removed = 0
                      AND mbm.status = 'included'
                )
                WHERE media_rn = 1
                  AND media_type = 'tv'
                  AND COALESCE(raw_mal_rating, '') NOT IN ('g', 'pg', 'r+', 'rx')
            )
            WHERE top_rn = 1
            ORDER BY season_id DESC
            "#,
        )
        .fetch_all(pool)
        .await?;

        let items = rows
            .into_iter()
            .map(|r| SeasonTop1Item {
                season_id: r.season_id,
                subject: PublicSubjectItem::from(r.subject),
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
    year * 100 + season_month
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dal::db::Database;
    use crate::dal::dto::{CreateSeason, CreateSubject, UpdateMalBgmMatch, UpsertMalAnime};
    use crate::dal::{
        MalAnimeRepository, MalBgmMatchRepository, SeasonMalRepository, SeasonRepository,
        SubjectRepository,
    };
    use sqlx::SqlitePool;

    async fn seed_season(pool: &SqlitePool, season_id: i32) -> sqlx::Result<()> {
        let season = match season_id % 100 {
            1 => "WINTER",
            4 => "SPRING",
            7 => "SUMMER",
            _ => "FALL",
        };
        SeasonRepository::new(pool)
            .upsert(CreateSeason {
                season_id,
                year: season_id / 100,
                season: season.to_string(),
                name: None,
            })
            .await?;
        Ok(())
    }

    async fn seed_mal_link(
        pool: &SqlitePool,
        season_id: i32,
        mal_id: i32,
        media_type: &str,
        rating: Option<&str>,
        status: &str,
        bgm_id: Option<i32>,
    ) -> sqlx::Result<()> {
        MalAnimeRepository::new(pool)
            .upsert(UpsertMalAnime {
                mal_id,
                title: format!("MAL {}", mal_id),
                media_type: media_type.to_string(),
                rating: rating.map(str::to_string),
                ..Default::default()
            })
            .await?;
        SeasonMalRepository::new(pool)
            .insert_ignore(season_id, mal_id)
            .await?;
        MalBgmMatchRepository::new(pool)
            .upsert(
                mal_id,
                UpdateMalBgmMatch {
                    status: status.to_string(),
                    source: Some("human".to_string()),
                    bgm_id,
                    ..Default::default()
                },
            )
            .await?;
        Ok(())
    }

    async fn seed_included_subject(
        pool: &SqlitePool,
        season_id: i32,
        mal_id: i32,
        media_type: &str,
        rating: Option<&str>,
        subject: CreateSubject,
    ) -> sqlx::Result<()> {
        let bgm_id = subject.id;
        SubjectRepository::new(pool).upsert(subject).await?;
        seed_mal_link(
            pool,
            season_id,
            mal_id,
            media_type,
            rating,
            "included",
            Some(bgm_id),
        )
        .await?;
        Ok(())
    }

    fn make_create_subject(id: i32, rank: Option<i32>) -> CreateSubject {
        CreateSubject {
            id,
            name: Some(format!("Subject {}", id)),
            rank,
            ..Default::default()
        }
    }

    #[test]
    fn test_map_mal_rating() {
        assert_eq!(map_mal_rating(Some("g")), "kids");
        assert_eq!(map_mal_rating(Some("pg")), "kids");
        assert_eq!(map_mal_rating(Some("r+")), "r18");
        assert_eq!(map_mal_rating(Some("rx")), "r18");
        assert_eq!(map_mal_rating(Some("pg_13")), "general");
        assert_eq!(map_mal_rating(Some("r")), "general");
        assert_eq!(map_mal_rating(None), "general");
        assert_eq!(map_mal_rating(Some("unknown")), "general");
    }

    #[sqlx::test]
    async fn test_list_seasons(pool: SqlitePool) -> sqlx::Result<()> {
        let db = Arc::new(Database::from_pool(pool.clone()));

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
        seed_season(&pool, 202601).await?;
        seed_included_subject(
            &pool,
            202601,
            1001,
            "tv",
            Some("pg_13"),
            CreateSubject {
                id: 101,
                score: Some(8.1234),
                ..Default::default()
            },
        )
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
        seed_season(&pool, 202601).await?;
        seed_included_subject(
            &pool,
            202601,
            1002,
            "tv",
            Some("pg_13"),
            CreateSubject {
                id: 102,
                rank: Some(999999),
                ..Default::default()
            },
        )
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
        seed_season(&pool, 202601).await?;
        seed_included_subject(
            &pool,
            202601,
            1301,
            "tv",
            Some("pg_13"),
            CreateSubject {
                id: 301,
                meta_tags: vec!["TV".to_string(), "TV".to_string(), "动作".to_string()],
                ..Default::default()
            },
        )
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
        seed_season(&pool, 202601).await?;
        seed_included_subject(
            &pool,
            202601,
            1302,
            "tv",
            Some("pg_13"),
            CreateSubject {
                id: 302,
                meta_tags: vec![
                    "动作".to_string(),
                    "TV".to_string(),
                    "动作".to_string(),
                    "剧情".to_string(),
                ],
                ..Default::default()
            },
        )
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
        seed_season(&pool, 202601).await?;
        let svc = QueryService::new(db);
        let seasons = svc.list_seasons().await.unwrap();
        assert_eq!(seasons.len(), 1);
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
        seed_season(&pool, 202601).await?;
        seed_included_subject(
            &pool,
            202601,
            1201,
            "tv",
            Some("pg_13"),
            CreateSubject {
                id: 201,
                air_weekday: Some("星期五".to_string()),
                ..Default::default()
            },
        )
        .await?;

        let svc = QueryService::new(db);
        let items = svc.get_season_subjects(202601).await.unwrap().unwrap();
        assert_eq!(items[0].air_weekday, Some("星期五".to_string()));
        Ok(())
    }

    #[sqlx::test]
    async fn test_query_service_drop_rate_returned(pool: SqlitePool) -> sqlx::Result<()> {
        let db = Arc::new(Database::from_pool(pool.clone()));
        seed_season(&pool, 202601).await?;
        seed_included_subject(
            &pool,
            202601,
            1202,
            "tv",
            Some("pg_13"),
            CreateSubject {
                id: 202,
                drop_rate: Some(0.1),
                ..Default::default()
            },
        )
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
        seed_season(&pool, 202601).await?;
        seed_included_subject(
            &pool,
            202601,
            1203,
            "tv",
            Some("pg_13"),
            CreateSubject {
                id: 203,
                average_comment: Some(3.5),
                ..Default::default()
            },
        )
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
        seed_season(&pool, 202601).await?;

        seed_included_subject(
            &pool,
            202601,
            1001,
            "tv",
            Some("pg_13"),
            make_create_subject(1, Some(100)),
        )
        .await?;
        seed_included_subject(
            &pool,
            202601,
            1002,
            "tv",
            Some("pg_13"),
            make_create_subject(2, Some(50)),
        )
        .await?;
        seed_included_subject(
            &pool,
            202601,
            1003,
            "tv",
            Some("pg_13"),
            make_create_subject(3, None),
        )
        .await?;

        let svc = QueryService::new(db);
        let items = svc.get_season_subjects(202601).await.unwrap().unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].id, 2); // rank 50 first
        assert_eq!(items[1].id, 1); // rank 100 second
        assert_eq!(items[2].id, 3); // None last
        Ok(())
    }

    #[sqlx::test]
    async fn test_get_season_subjects_maps_mal_rating_and_preserves_media_type(
        pool: SqlitePool,
    ) -> sqlx::Result<()> {
        let db = Arc::new(Database::from_pool(pool.clone()));
        seed_season(&pool, 202601).await?;

        seed_included_subject(
            &pool,
            202601,
            1001,
            "tv",
            Some("pg"),
            make_create_subject(1, Some(1)),
        )
        .await?;
        seed_included_subject(
            &pool,
            202601,
            1002,
            "ona",
            Some("r+"),
            make_create_subject(2, Some(2)),
        )
        .await?;
        seed_included_subject(
            &pool,
            202601,
            1003,
            "movie",
            Some("pg_13"),
            make_create_subject(3, Some(3)),
        )
        .await?;
        seed_included_subject(
            &pool,
            202601,
            1004,
            "tv_special",
            None,
            make_create_subject(4, Some(4)),
        )
        .await?;

        let svc = QueryService::new(db);
        let items = svc.get_season_subjects(202601).await.unwrap().unwrap();
        assert_eq!(items.len(), 4);

        assert_eq!(items[0].media_type.as_deref(), Some("tv"));
        assert_eq!(items[0].rating.as_deref(), Some("kids"));

        assert_eq!(items[1].media_type.as_deref(), Some("ona"));
        assert_eq!(items[1].rating.as_deref(), Some("r18"));

        assert_eq!(items[2].media_type.as_deref(), Some("movie"));
        assert_eq!(items[2].rating.as_deref(), Some("general"));

        assert_eq!(items[3].media_type.as_deref(), Some("tv_special"));
        assert_eq!(items[3].rating.as_deref(), Some("general"));
        Ok(())
    }

    #[sqlx::test]
    async fn test_get_season_subjects_n_to_1_priority_dedupe(pool: SqlitePool) -> sqlx::Result<()> {
        let db = Arc::new(Database::from_pool(pool.clone()));
        seed_season(&pool, 202601).await?;

        // Same bgm_id=500 mapped by an ONA (smaller mal_id=1001) and a TV (larger mal_id=1002):
        // tv must beat ona regardless of mal_id order.
        SubjectRepository::new(&pool)
            .upsert(make_create_subject(500, Some(10)))
            .await?;
        seed_mal_link(&pool, 202601, 1001, "ona", Some("g"), "included", Some(500)).await?;
        seed_mal_link(
            &pool,
            202601,
            1002,
            "tv",
            Some("pg_13"),
            "included",
            Some(500),
        )
        .await?;

        // Same bgm_id=501 mapped by ova (mal_id=2001) and movie (mal_id=2002):
        // movie must beat ova.
        SubjectRepository::new(&pool)
            .upsert(make_create_subject(501, Some(20)))
            .await?;
        seed_mal_link(
            &pool,
            202601,
            2001,
            "ova",
            Some("r+"),
            "included",
            Some(501),
        )
        .await?;
        seed_mal_link(
            &pool,
            202601,
            2002,
            "movie",
            Some("pg_13"),
            "included",
            Some(501),
        )
        .await?;

        // Same bgm_id=502 mapped by two TVs (mal_id=3002 with rating="g", mal_id=3001 with rating="r"):
        // equal priority breaks ties by mal_id ASC -> 3001 ("general") wins.
        SubjectRepository::new(&pool)
            .upsert(make_create_subject(502, Some(30)))
            .await?;
        seed_mal_link(&pool, 202601, 3002, "tv", Some("g"), "included", Some(502)).await?;
        seed_mal_link(&pool, 202601, 3001, "tv", Some("r"), "included", Some(502)).await?;

        let svc = QueryService::new(db);
        let items = svc.get_season_subjects(202601).await.unwrap().unwrap();
        assert_eq!(items.len(), 3);

        assert_eq!(items[0].id, 500);
        assert_eq!(items[0].media_type.as_deref(), Some("tv"));
        assert_eq!(items[0].rating.as_deref(), Some("general"));

        assert_eq!(items[1].id, 501);
        assert_eq!(items[1].media_type.as_deref(), Some("movie"));
        assert_eq!(items[1].rating.as_deref(), Some("general"));

        assert_eq!(items[2].id, 502);
        assert_eq!(items[2].media_type.as_deref(), Some("tv"));
        assert_eq!(items[2].rating.as_deref(), Some("general"));
        Ok(())
    }

    #[sqlx::test]
    async fn test_get_season_subjects_excludes_removed_and_non_included(
        pool: SqlitePool,
    ) -> sqlx::Result<()> {
        let db = Arc::new(Database::from_pool(pool.clone()));
        seed_season(&pool, 202601).await?;

        // Active & included
        seed_included_subject(
            &pool,
            202601,
            1001,
            "tv",
            Some("pg_13"),
            make_create_subject(1, Some(10)),
        )
        .await?;

        // Included in mal_bgm_matches, but marked is_removed = 1 in season_mal
        seed_included_subject(
            &pool,
            202601,
            1002,
            "tv",
            Some("pg_13"),
            make_create_subject(2, Some(1)),
        )
        .await?;
        SeasonMalRepository::new(&pool)
            .set_removed(202601, 1002, true, Some("wrong season".to_string()))
            .await?;

        // Active in season_mal, but pending / excluded in mal_bgm_matches
        SubjectRepository::new(&pool)
            .upsert(make_create_subject(3, Some(2)))
            .await?;
        seed_mal_link(&pool, 202601, 1003, "tv", Some("pg_13"), "pending", Some(3)).await?;

        SubjectRepository::new(&pool)
            .upsert(make_create_subject(4, Some(3)))
            .await?;
        seed_mal_link(
            &pool,
            202601,
            1004,
            "tv",
            Some("pg_13"),
            "excluded",
            Some(4),
        )
        .await?;

        let svc = QueryService::new(db);
        let items = svc.get_season_subjects(202601).await.unwrap().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, 1);
        Ok(())
    }

    #[sqlx::test]
    async fn test_get_all_seasons_top1_excludes_ona_only_and_non_general(
        pool: SqlitePool,
    ) -> sqlx::Result<()> {
        let db = Arc::new(Database::from_pool(pool.clone()));

        // Season 202601: ONA rank 1 should be ignored; kids/r18 TV should be ignored;
        // general TV rank 5 should win.
        seed_season(&pool, 202601).await?;
        seed_included_subject(
            &pool,
            202601,
            1001,
            "ona",
            Some("pg_13"),
            make_create_subject(11, Some(1)),
        )
        .await?;
        seed_included_subject(
            &pool,
            202601,
            1002,
            "tv",
            Some("g"),
            make_create_subject(12, Some(2)),
        )
        .await?;
        seed_included_subject(
            &pool,
            202601,
            1003,
            "tv",
            Some("r+"),
            make_create_subject(13, Some(3)),
        )
        .await?;
        seed_included_subject(
            &pool,
            202601,
            1004,
            "tv",
            Some("pg_13"),
            make_create_subject(14, Some(5)),
        )
        .await?;

        // Season 202510: ONA-only season -> should have no top1 entry at all.
        seed_season(&pool, 202510).await?;
        seed_included_subject(
            &pool,
            202510,
            2001,
            "ona",
            Some("pg_13"),
            make_create_subject(21, Some(1)),
        )
        .await?;

        // Season 202507: TV with NULL rating is allowed (mapped to general);
        // tie-break on equal rank picks higher collection_total.
        seed_season(&pool, 202507).await?;
        seed_included_subject(
            &pool,
            202507,
            3001,
            "tv",
            None,
            CreateSubject {
                id: 31,
                rank: Some(10),
                collection_total: Some(100),
                ..Default::default()
            },
        )
        .await?;
        seed_included_subject(
            &pool,
            202507,
            3002,
            "tv",
            Some("r"),
            CreateSubject {
                id: 32,
                rank: Some(10),
                collection_total: Some(500),
                ..Default::default()
            },
        )
        .await?;

        // Season 202504: TV marked is_removed = 1 -> excluded from top1.
        seed_season(&pool, 202504).await?;
        seed_included_subject(
            &pool,
            202504,
            4001,
            "tv",
            Some("pg_13"),
            make_create_subject(41, Some(1)),
        )
        .await?;
        SeasonMalRepository::new(&pool)
            .set_removed(202504, 4001, true, None)
            .await?;

        let svc = QueryService::new(db);
        let top1 = svc.get_all_seasons_top1().await.unwrap();
        assert_eq!(top1.len(), 2);

        // Ordered by season_id DESC
        assert_eq!(top1[0].season_id, 202601);
        assert_eq!(top1[0].subject.id, 14);
        assert_eq!(top1[0].subject.media_type.as_deref(), Some("tv"));
        assert_eq!(top1[0].subject.rating.as_deref(), Some("general"));

        assert_eq!(top1[1].season_id, 202507);
        assert_eq!(top1[1].subject.id, 32);
        assert_eq!(top1[1].subject.rating.as_deref(), Some("general"));
        Ok(())
    }

    #[sqlx::test]
    async fn test_get_all_seasons_top1_dedupes_before_filtering(
        pool: SqlitePool,
    ) -> sqlx::Result<()> {
        let db = Arc::new(Database::from_pool(pool.clone()));

        // Season 202601: bgm_id=100 (rank 1) has higher-priority kids TV (mal_id=1001, rating='g')
        // and lower-priority general TV (mal_id=1002, rating='pg_13').
        // Dedupe (rn=1) picks mal_id=1001 first, then rating filter excludes it, so bgm_id=100
        // must NOT surface as top1 via mal_id=1002; bgm_id=200 (rank 5) wins instead.
        seed_season(&pool, 202601).await?;
        SubjectRepository::new(&pool)
            .upsert(make_create_subject(100, Some(1)))
            .await?;
        seed_mal_link(&pool, 202601, 1001, "tv", Some("g"), "included", Some(100)).await?;
        seed_mal_link(
            &pool,
            202601,
            1002,
            "tv",
            Some("pg_13"),
            "included",
            Some(100),
        )
        .await?;
        seed_included_subject(
            &pool,
            202601,
            1003,
            "tv",
            Some("pg_13"),
            make_create_subject(200, Some(5)),
        )
        .await?;

        // Season 202510: only bgm_id=300 where higher-priority mal_id=2001 is r18 ('rx')
        // and lower-priority mal_id=2002 is general ('pg_13') -> no top1 entry at all.
        seed_season(&pool, 202510).await?;
        SubjectRepository::new(&pool)
            .upsert(make_create_subject(300, Some(1)))
            .await?;
        seed_mal_link(&pool, 202510, 2001, "tv", Some("rx"), "included", Some(300)).await?;
        seed_mal_link(
            &pool,
            202510,
            2002,
            "tv",
            Some("pg_13"),
            "included",
            Some(300),
        )
        .await?;

        // Season 202507: ona (mal_id=3001) and tv (mal_id=3002) share bgm_id=400 -> tv wins rn=1
        // and surfaces as top1.
        seed_season(&pool, 202507).await?;
        SubjectRepository::new(&pool)
            .upsert(make_create_subject(400, Some(1)))
            .await?;
        seed_mal_link(
            &pool,
            202507,
            3001,
            "ona",
            Some("pg_13"),
            "included",
            Some(400),
        )
        .await?;
        seed_mal_link(
            &pool,
            202507,
            3002,
            "tv",
            Some("pg_13"),
            "included",
            Some(400),
        )
        .await?;

        let svc = QueryService::new(db);
        let top1 = svc.get_all_seasons_top1().await.unwrap();
        assert_eq!(top1.len(), 2);

        assert_eq!(top1[0].season_id, 202601);
        assert_eq!(top1[0].subject.id, 200);

        assert_eq!(top1[1].season_id, 202507);
        assert_eq!(top1[1].subject.id, 400);
        assert_eq!(top1[1].subject.media_type.as_deref(), Some("tv"));
        Ok(())
    }

    #[sqlx::test]
    async fn test_deterministic_id_tiebreaker_in_season_subjects_and_top1(
        pool: SqlitePool,
    ) -> sqlx::Result<()> {
        let db = Arc::new(Database::from_pool(pool.clone()));
        seed_season(&pool, 202601).await?;

        // Insert higher id first with identical rank and collection_total.
        seed_included_subject(
            &pool,
            202601,
            1001,
            "tv",
            Some("pg_13"),
            CreateSubject {
                id: 502,
                rank: Some(10),
                collection_total: Some(200),
                ..Default::default()
            },
        )
        .await?;
        seed_included_subject(
            &pool,
            202601,
            1002,
            "tv",
            Some("pg_13"),
            CreateSubject {
                id: 501,
                rank: Some(10),
                collection_total: Some(200),
                ..Default::default()
            },
        )
        .await?;

        let svc = QueryService::new(db);
        let subjects = svc.get_season_subjects(202601).await.unwrap().unwrap();
        assert_eq!(subjects.len(), 2);
        assert_eq!(subjects[0].id, 501);
        assert_eq!(subjects[1].id, 502);

        let top1 = svc.get_all_seasons_top1().await.unwrap();
        assert_eq!(top1.len(), 1);
        assert_eq!(top1[0].subject.id, 501);
        Ok(())
    }
}
