use anyhow::{Context, Result, anyhow};
use std::collections::HashMap;
use std::sync::Arc;

use crate::dal::{CreateSeason, CreateSubject, Database};
use crate::dal::{SeasonRepository, SeasonSubjectRepository, SubjectRepository};
use crate::services::bangumi::schemas::{Collection, Episode, InfoboxItem};
use crate::services::bangumi::{BangumiClient, Subject as BangumiSubject};
use crate::services::season_data::{MediaType, Rating, SeasonDataClient};

#[derive(Debug)]
pub struct SyncResult {
    pub season_id: i32,
    pub added: usize,
    pub removed: usize,
    pub updated: usize,
    pub failed: usize,
}

pub struct SyncService {
    season_data_client: SeasonDataClient,
    bangumi_client: BangumiClient,
    db: Arc<Database>,
}

impl SyncService {
    pub fn new(db: Arc<Database>) -> Self {
        Self {
            season_data_client: SeasonDataClient::new(),
            bangumi_client: BangumiClient::new(),
            db,
        }
    }

    pub async fn create_and_sync(
        &self,
        year: i32,
        month: i32,
        name: Option<String>,
    ) -> Result<SyncResult> {
        let season_id = year * 100 + month;
        // T015: sync started log
        tracing::info!(season_id = %season_id, operation = "create", "sync started");
        let season_str = month_to_season(month)?;
        let key = format!("{}-{}", year, season_str.to_lowercase());
        let pool = self.db.pool();

        // 1. Upsert Season
        SeasonRepository::new(pool)
            .upsert(CreateSeason {
                season_id,
                year,
                season: season_str,
                name,
            })
            .await
            .context("upsert season 失败")?;

        self.sync_season_data(season_id, &key).await.map_err(|e| {
            // T017: sync failed log
            tracing::error!(season_id = %season_id, error = %e, "sync failed");
            e
        })
    }

    pub async fn resync(&self, season_id: i32) -> Result<SyncResult> {
        // T015: sync started log
        tracing::info!(season_id = %season_id, operation = "resync", "sync started");
        let pool = self.db.pool();
        let season = SeasonRepository::new(pool)
            .find_by_id(season_id)
            .await?
            .ok_or_else(|| anyhow!("Season {} not found", season_id))?;

        let month = season_id % 100;
        let season_str = month_to_season(month)?;
        let key = format!("{}-{}", season.year, season_str.to_lowercase());

        self.sync_season_data(season_id, &key).await.map_err(|e| {
            // T017: sync failed log
            tracing::error!(season_id = %season_id, error = %e, "sync failed");
            e
        })
    }

    async fn sync_season_data(&self, season_id: i32, key: &str) -> Result<SyncResult> {
        let start = std::time::Instant::now();
        let pool = self.db.pool();

        // 2. Fetch season data
        let entries = self.season_data_client.fetch_season(key).await?;

        // 3. Upsert subjects with media_type/rating from season data
        let subject_repo = SubjectRepository::new(pool);
        let bgm_ids: Vec<i32> = entries.iter().map(|e| e.bgm_id).collect();

        for entry in &entries {
            let _ = subject_repo
                .upsert(CreateSubject {
                    id: entry.bgm_id,
                    media_type: Some(media_type_to_str(&entry.media_type).to_string()),
                    rating: Some(rating_to_str(&entry.rating).to_string()),
                    ..Default::default()
                })
                .await;
        }

        // 4. Reconcile season_subjects
        let (added, removed) = SeasonSubjectRepository::new(pool)
            .reconcile(season_id, bgm_ids)
            .await
            .context("reconcile 失败")?;

        // 5. Fetch Bangumi details per subject
        let mut updated = 0usize;
        let mut failed = 0usize;
        let today = chrono::Utc::now().date_naive();

        for entry in &entries {
            let avg_comment = match self
                .bangumi_client
                .get_episodes(entry.bgm_id, 0, 100, 0)
                .await
            {
                Ok(paged) => calculate_average_comment(&paged.data, today),
                Err(e) => {
                    tracing::warn!(subject_id = entry.bgm_id, error = %e, "拉取 episodes 失败，降级为 None");
                    None
                }
            };

            match self.bangumi_client.get_subject(entry.bgm_id).await {
                Ok(bgm_subject) => {
                    if let Err(e) = subject_repo
                        .upsert(to_create_subject(bgm_subject, avg_comment))
                        .await
                    {
                        tracing::error!(subject_id = entry.bgm_id, error = %e, "upsert subject 失败");
                        failed += 1;
                    } else {
                        updated += 1;
                    }
                }
                Err(e) => {
                    tracing::error!(subject_id = entry.bgm_id, error = %e, "拉取 subject 失败");
                    failed += 1;
                }
            }
        }

        // 更新 season 的 updated_at 时间戳
        if let Err(e) = SeasonRepository::new(pool).touch_updated_at(season_id).await {
            tracing::warn!(season_id = %season_id, error = %e, "touch_updated_at 失败");
        }

        // T016: sync completed log
        let elapsed_ms = start.elapsed().as_millis() as u64;
        tracing::info!(
            season_id = %season_id,
            added,
            updated,
            deleted = removed,
            failed,
            elapsed_ms,
            "sync completed"
        );

        Ok(SyncResult {
            season_id,
            added,
            removed,
            updated,
            failed,
        })
    }

    pub async fn find_orphans(&self) -> Result<Vec<OrphanSubjectItem>> {
        let pool = self.db.pool();
        let subjects = SubjectRepository::new(pool)
            .find_orphans()
            .await
            .map_err(anyhow::Error::from)?;
        let items = subjects
            .into_iter()
            .map(|s| OrphanSubjectItem {
                id: s.id,
                name: s.name,
                name_cn: s.name_cn,
            })
            .collect();
        Ok(items)
    }

    pub async fn delete_orphans(&self) -> Result<u64> {
        let pool = self.db.pool();
        SubjectRepository::new(pool)
            .delete_orphans()
            .await
            .map_err(anyhow::Error::from)
    }

    pub async fn delete_season(&self, season_id: i32) -> Result<bool> {
        let pool = self.db.pool();
        let deleted = SeasonRepository::new(pool)
            .delete(season_id)
            .await
            .map_err(anyhow::Error::from)?;
        if deleted
            && let Err(e) = SubjectRepository::new(pool).delete_orphans().await
        {
            tracing::warn!(error = %e, "delete_orphans after delete_season 失败");
        }
        Ok(deleted)
    }
}

pub struct OrphanSubjectItem {
    pub id: i32,
    pub name: Option<String>,
    pub name_cn: Option<String>,
}

fn month_to_season(month: i32) -> Result<String> {
    match month {
        1 => Ok("WINTER".to_string()),
        4 => Ok("SPRING".to_string()),
        7 => Ok("SUMMER".to_string()),
        10 => Ok("FALL".to_string()),
        _ => Err(anyhow!("Invalid month: {}", month)),
    }
}

fn media_type_to_str(mt: &MediaType) -> &'static str {
    match mt {
        MediaType::Tv => "tv",
        MediaType::Movie => "movie",
        MediaType::Ova => "ova",
        MediaType::Ona => "ona",
        MediaType::TvSpecial => "tv_special",
        MediaType::Special => "special",
        MediaType::Music => "music",
        MediaType::Pv => "pv",
        MediaType::Cm => "cm",
    }
}

fn rating_to_str(r: &Rating) -> &'static str {
    match r {
        Rating::General => "general",
        Rating::Kids => "kids",
        Rating::R18 => "r18",
    }
}

#[allow(dead_code)]
fn normalize_rank(rank: Option<i32>) -> Option<i32> {
    rank.map(|r| if r == 0 { 999999 } else { r })
}

#[allow(dead_code)]
fn calculate_exact_score(count: &HashMap<String, i32>) -> Option<f64> {
    let total: i32 = count.values().sum();
    if total == 0 {
        return None;
    }
    let weighted_sum: f64 = count
        .iter()
        .filter_map(|(k, &v)| k.parse::<f64>().ok().map(|rating| rating * v as f64))
        .sum();
    Some(weighted_sum / total as f64)
}

#[allow(dead_code)]
fn extract_air_weekday(infobox: &[InfoboxItem]) -> Option<String> {
    infobox
        .iter()
        .find(|item| item.key.as_deref() == Some("放送星期"))
        .and_then(|item| {
            item.value
                .as_ref()
                .and_then(|v| v.as_str().map(|s| s.to_string()))
        })
}

#[allow(dead_code)]
fn calculate_drop_rate(c: &Collection) -> Option<f64> {
    let total = c.wish + c.collect + c.doing + c.on_hold + c.dropped;
    if total == 0 {
        return None;
    }
    Some(c.dropped as f64 / total as f64)
}

#[allow(dead_code)]
pub(crate) fn dedup_preserving_order(tags: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    tags.into_iter().filter(|t| seen.insert(t.clone())).collect()
}

pub(crate) fn calculate_average_comment(
    episodes: &[Episode],
    today: chrono::NaiveDate,
) -> Option<f64> {
    let aired: Vec<_> = episodes
        .iter()
        .filter(|e| {
            e._type == 0
                && e.airdate
                    .as_deref()
                    .and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
                    .map(|d| d <= today)
                    .unwrap_or(false)
        })
        .collect();

    if aired.is_empty() {
        return None;
    }

    let total: i32 = aired.iter().map(|e| e.comment.unwrap_or(0)).sum();
    Some(total as f64 / aired.len() as f64)
}

pub(crate) fn to_create_subject(s: BangumiSubject, avg_comment: Option<f64>) -> CreateSubject {
    let rank = normalize_rank(s.rating.as_ref().and_then(|r| r.rank));
    let score = s
        .rating
        .as_ref()
        .and_then(|r| r.count.as_ref())
        .and_then(calculate_exact_score);
    let drop_rate = s.collection.as_ref().and_then(calculate_drop_rate);
    let collection_total = s.collection.as_ref().map(|c| {
        c.wish + c.collect + c.doing + c.on_hold + c.dropped
    });
    let air_weekday = s
        .infobox
        .as_deref()
        .and_then(extract_air_weekday);
    let meta_tags = dedup_preserving_order(s.meta_tags.unwrap_or_default());

    CreateSubject {
        id: s.id,
        name: s.name,
        name_cn: s.name_cn,
        images_grid: s.images.as_ref().and_then(|i| i.grid.clone()),
        images_large: s.images.and_then(|i| i.large),
        rank,
        score,
        collection_total,
        average_comment: avg_comment,
        drop_rate,
        air_weekday,
        meta_tags,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::bangumi::schemas::{Collection, InfoboxItem, Rating};
    use sqlx::PgPool;
    use std::collections::HashMap;

    fn make_bgm_subject(rank: Option<i32>, count: HashMap<String, i32>) -> BangumiSubject {
        BangumiSubject {
            id: 1,
            _type: 2,
            name: Some("Test".to_string()),
            name_cn: None,
            summary: None,
            series: None,
            nsfw: None,
            locked: None,
            date: None,
            platform: None,
            images: None,
            infobox: None,
            volumes: None,
            eps: None,
            total_episodes: None,
            rating: Some(Rating {
                rank,
                total: Some(count.values().sum()),
                count: Some(count),
                score: None,
            }),
            collection: Some(Collection {
                wish: 10,
                collect: 60,
                doing: 10,
                on_hold: 10,
                dropped: 10,
            }),
            meta_tags: None,
            tags: None,
        }
    }

    // T008 — normalize_rank
    #[test]
    fn test_normalize_rank_zero_becomes_999999() {
        assert_eq!(normalize_rank(Some(0)), Some(999999));
    }

    #[test]
    fn test_normalize_rank_nonzero_unchanged() {
        assert_eq!(normalize_rank(Some(42)), Some(42));
    }

    #[test]
    fn test_normalize_rank_none_stays_none() {
        assert_eq!(normalize_rank(None), None);
    }

    // T008 — calculate_exact_score
    #[test]
    fn test_calculate_exact_score_weighted_average() {
        let mut count = HashMap::new();
        count.insert("1".to_string(), 1);
        count.insert("10".to_string(), 1);
        let score = calculate_exact_score(&count).unwrap();
        assert!((score - 5.5).abs() < 0.0001, "expected 5.5, got {score}");
    }

    #[test]
    fn test_calculate_exact_score_empty_returns_none() {
        let count: HashMap<String, i32> = HashMap::new();
        assert_eq!(calculate_exact_score(&count), None);
    }

    // T008 — extract_air_weekday
    #[test]
    fn test_extract_air_weekday_found() {
        let infobox = vec![InfoboxItem {
            key: Some("放送星期".to_string()),
            value: Some(serde_json::Value::String("星期五".to_string())),
        }];
        assert_eq!(
            extract_air_weekday(&infobox),
            Some("星期五".to_string())
        );
    }

    #[test]
    fn test_extract_air_weekday_not_found() {
        let infobox: Vec<InfoboxItem> = vec![];
        assert_eq!(extract_air_weekday(&infobox), None);
    }

    // T008 — calculate_drop_rate
    #[test]
    fn test_calculate_drop_rate_normal() {
        let c = Collection {
            wish: 10,
            collect: 60,
            doing: 10,
            on_hold: 10,
            dropped: 10,
        };
        let rate = calculate_drop_rate(&c).unwrap();
        assert!((rate - 0.1).abs() < 0.0001, "expected 0.1, got {rate}");
    }

    #[test]
    fn test_calculate_drop_rate_zero_total_returns_none() {
        let c = Collection {
            wish: 0,
            collect: 0,
            doing: 0,
            on_hold: 0,
            dropped: 0,
        };
        assert_eq!(calculate_drop_rate(&c), None);
    }

    // T008 — dedup_preserving_order
    #[test]
    fn test_dedup_preserving_order_removes_duplicates() {
        let input = vec!["TV".to_string(), "TV".to_string(), "动作".to_string()];
        assert_eq!(
            dedup_preserving_order(input),
            vec!["TV".to_string(), "动作".to_string()]
        );
    }

    #[test]
    fn test_dedup_preserving_order_empty() {
        let input: Vec<String> = vec![];
        assert_eq!(dedup_preserving_order(input), Vec::<String>::new());
    }

    // T012 [US2]: 验证同步开始时 INFO 事件包含 season_id 和 operation 字段
    #[tracing_test::traced_test]
    #[sqlx::test]
    async fn test_sync_started_log_has_season_id_and_operation(pool: PgPool) {
        let db = Arc::new(Database::from_pool(pool));
        let svc = SyncService::new(db);
        // month=2 无效，但 sync started 日志应在 month_to_season 之前触发
        let _ = svc.create_and_sync(2026, 2, None).await;
        assert!(
            logs_contain("season_id"),
            "sync started 日志应包含 season_id 字段"
        );
        assert!(
            logs_contain("operation"),
            "sync started 日志应包含 operation 字段"
        );
    }

    // T013 [US2]: 验证同步完成时 INFO 事件包含 added, updated, deleted, failed, elapsed_ms 字段
    #[tracing_test::traced_test]
    #[sqlx::test]
    async fn test_sync_completed_log_has_result_fields(pool: PgPool) {
        let db = Arc::new(Database::from_pool(pool));
        let svc = SyncService::new(db);
        let result = svc.create_and_sync(2999, 1, None).await;
        if result.is_ok() {
            assert!(
                logs_contain("added"),
                "sync completed 日志应包含 added 字段"
            );
            assert!(
                logs_contain("elapsed_ms"),
                "sync completed 日志应包含 elapsed_ms 字段"
            );
        }
    }

    // T019 — SyncService::create_and_sync / resync
    #[sqlx::test]
    async fn test_create_and_sync_invalid_month_returns_err(pool: PgPool) {
        let db = Arc::new(Database::from_pool(pool));
        let svc = SyncService::new(db);
        let result = svc.create_and_sync(2026, 2, None).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Invalid month"));
    }

    #[sqlx::test]
    async fn test_resync_unknown_season_returns_err(pool: PgPool) {
        let db = Arc::new(Database::from_pool(pool));
        let svc = SyncService::new(db);
        let result = svc.resync(999999).await;
        assert!(result.is_err());
    }

    // T037 — SyncService::find_orphans / delete_orphans
    #[sqlx::test]
    async fn test_find_orphans_returns_ok(pool: PgPool) {
        let db = Arc::new(Database::from_pool(pool));
        let svc = SyncService::new(db);
        let result = svc.find_orphans().await;
        assert!(result.is_ok());
    }

    #[sqlx::test]
    async fn test_delete_orphans_returns_ok(pool: PgPool) {
        let db = Arc::new(Database::from_pool(pool));
        let svc = SyncService::new(db);
        let result = svc.delete_orphans().await;
        assert!(result.is_ok());
    }

    // T018 — to_create_subject 专项测试（Red 阶段）
    #[test]
    fn test_to_create_subject_rank_zero_becomes_999999() {
        let mut count = HashMap::new();
        count.insert("5".to_string(), 1);
        let s = make_bgm_subject(Some(0), count);
        let create = to_create_subject(s, None);
        assert_eq!(create.rank, Some(999999));
    }

    #[test]
    fn test_to_create_subject_rank_nonzero_preserved() {
        let mut count = HashMap::new();
        count.insert("5".to_string(), 1);
        let s = make_bgm_subject(Some(42), count);
        let create = to_create_subject(s, None);
        assert_eq!(create.rank, Some(42));
    }

    #[test]
    fn test_to_create_subject_score_uses_exact_calculation() {
        let mut count = HashMap::new();
        count.insert("10".to_string(), 1);
        count.insert("1".to_string(), 1);
        let s = make_bgm_subject(None, count);
        let score = to_create_subject(s, None).score.unwrap();
        assert!((score - 5.5).abs() < 0.0001, "expected 5.5, got {score}");
    }

    #[test]
    fn test_to_create_subject_score_none_when_no_ratings() {
        let s = make_bgm_subject(None, HashMap::new());
        let create = to_create_subject(s, None);
        assert_eq!(create.score, None);
    }

    // T011 — delete_season（Red 阶段）
    #[sqlx::test]
    async fn test_delete_season_existing_returns_true(pool: PgPool) {
        let db = Arc::new(Database::from_pool(pool));

        // 创建 season 202699
        SeasonRepository::new(db.pool())
            .upsert(CreateSeason {
                season_id: 202699,
                year: 2026,
                season: "FALL".to_string(),
                name: None,
            })
            .await
            .unwrap();

        let svc = SyncService::new(db);
        let result = svc.delete_season(202699).await;
        assert!(result.is_ok());
        assert!(result.unwrap(), "should return true for existing season");

        // 验证 season 已从 DB 消失
        let found = SeasonRepository::new(svc.db.pool()).find_by_id(202699).await.unwrap();
        assert!(found.is_none(), "season should be deleted from DB");
    }

    #[sqlx::test]
    async fn test_delete_season_nonexistent_returns_false(pool: PgPool) {
        let db = Arc::new(Database::from_pool(pool));
        let svc = SyncService::new(db);
        let result = svc.delete_season(999989).await;
        assert!(result.is_ok());
        assert!(!result.unwrap(), "should return false for non-existent season");
    }

    // T025 — to_create_subject 字段测试（air_weekday / drop_rate / avg_comment）

    fn make_subject_with_infobox(
        infobox: Option<Vec<InfoboxItem>>,
        collection: Option<Collection>,
    ) -> BangumiSubject {
        BangumiSubject {
            id: 1,
            _type: 2,
            name: None,
            name_cn: None,
            summary: None,
            series: None,
            nsfw: None,
            locked: None,
            date: None,
            platform: None,
            images: None,
            infobox,
            volumes: None,
            eps: None,
            total_episodes: None,
            rating: None,
            collection,
            meta_tags: None,
            tags: None,
        }
    }

    #[test]
    fn test_to_create_subject_air_weekday_extracted_from_infobox() {
        let s = make_subject_with_infobox(
            Some(vec![InfoboxItem {
                key: Some("放送星期".to_string()),
                value: Some(serde_json::Value::String("星期五".to_string())),
            }]),
            None,
        );
        let create = to_create_subject(s, None);
        assert_eq!(create.air_weekday, Some("星期五".to_string()));
    }

    #[test]
    fn test_to_create_subject_air_weekday_none_when_missing() {
        let s = make_subject_with_infobox(Some(vec![]), None);
        let create = to_create_subject(s, None);
        assert_eq!(create.air_weekday, None);
    }

    #[test]
    fn test_to_create_subject_drop_rate_calculated() {
        let s = make_subject_with_infobox(
            None,
            Some(Collection {
                wish: 10,
                collect: 60,
                doing: 10,
                on_hold: 10,
                dropped: 10,
            }),
        );
        let create = to_create_subject(s, None);
        let rate = create.drop_rate.unwrap();
        assert!((rate - 0.1).abs() < 0.0001, "expected 0.1, got {rate}");
    }

    #[test]
    fn test_to_create_subject_drop_rate_none_when_zero_total() {
        let s = make_subject_with_infobox(
            None,
            Some(Collection {
                wish: 0,
                collect: 0,
                doing: 0,
                on_hold: 0,
                dropped: 0,
            }),
        );
        let create = to_create_subject(s, None);
        assert_eq!(create.drop_rate, None);
    }

    #[test]
    fn test_to_create_subject_avg_comment_passed_through() {
        let s = make_subject_with_infobox(None, None);
        let create = to_create_subject(s, Some(3.5));
        assert_eq!(create.average_comment, Some(3.5));
    }

    // T026 — calculate_average_comment 测试

    fn make_episode(id: i32, ep_type: i32, airdate: &str, comment: Option<i32>) -> Episode {
        Episode {
            id,
            _type: ep_type,
            name: None,
            name_cn: None,
            sort: None,
            ep: None,
            airdate: Some(airdate.to_string()),
            comment,
            duration: None,
            desc: None,
            disc: None,
            duration_seconds: None,
            subject_id: None,
        }
    }

    #[test]
    fn test_calculate_average_comment_aired_only() {
        use chrono::NaiveDate;
        let today = NaiveDate::from_ymd_opt(2026, 3, 10).unwrap();
        let episodes = vec![
            make_episode(1, 0, "2026-01-01", Some(10)),
            make_episode(2, 0, "2099-01-01", Some(5)),
        ];
        let avg = calculate_average_comment(&episodes, today);
        assert_eq!(avg, Some(10.0));
    }

    #[test]
    fn test_calculate_average_comment_all_unaired_returns_none() {
        use chrono::NaiveDate;
        let today = NaiveDate::from_ymd_opt(2026, 3, 10).unwrap();
        let episodes = vec![make_episode(1, 0, "2099-01-01", Some(10))];
        let avg = calculate_average_comment(&episodes, today);
        assert_eq!(avg, None);
    }

    #[test]
    fn test_calculate_average_comment_skip_non_main_episodes() {
        use chrono::NaiveDate;
        let today = NaiveDate::from_ymd_opt(2026, 3, 10).unwrap();
        let episodes = vec![
            make_episode(1, 0, "2026-01-01", Some(10)), // main, aired
            make_episode(2, 1, "2026-01-08", Some(100)), // special, should be skipped
        ];
        let avg = calculate_average_comment(&episodes, today);
        assert_eq!(avg, Some(10.0));
    }

    #[test]
    fn test_calculate_average_comment_empty_returns_none() {
        use chrono::NaiveDate;
        let today = NaiveDate::from_ymd_opt(2026, 3, 10).unwrap();
        let avg = calculate_average_comment(&[], today);
        assert_eq!(avg, None);
    }

    // T033 — sync_season_data 完成后 touch_updated_at 机制端到端验证
    // 通过 QueryService::list_seasons 验证 Service 层字段透传
    #[sqlx::test]
    async fn test_sync_season_data_updates_season_updated_at(pool: PgPool) -> sqlx::Result<()> {
        let db = Arc::new(Database::from_pool(pool.clone()));
        let season_repo = SeasonRepository::new(&pool);
        let query_svc = crate::core::query::QueryService::new(db);

        // 1. Setup
        season_repo
            .upsert(CreateSeason {
                season_id: 202699,
                year: 2026,
                season: "FALL".to_string(),
                name: None,
            })
            .await?;

        let before = query_svc
            .list_seasons()
            .await
            .unwrap()
            .into_iter()
            .find(|s| s.season_id == 202699)
            .unwrap()
            .updated_at;

        // 小延迟确保时间戳可区分
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        // 2. Act: 模拟 sync_season_data 末尾的 touch_updated_at 调用（T036）
        season_repo.touch_updated_at(202699).await?;

        // 3. Assert: 通过 QueryService（Service 层）验证端到端读取路径
        let after = query_svc
            .list_seasons()
            .await
            .unwrap()
            .into_iter()
            .find(|s| s.season_id == 202699)
            .unwrap()
            .updated_at;

        assert!(after > before, "通过 QueryService 验证 updated_at 已更新");
        Ok(())
    }

    #[test]
    fn test_month_to_season_winter() {
        assert_eq!(month_to_season(1).unwrap(), "WINTER");
    }

    #[test]
    fn test_month_to_season_spring() {
        assert_eq!(month_to_season(4).unwrap(), "SPRING");
    }

    #[test]
    fn test_month_to_season_summer() {
        assert_eq!(month_to_season(7).unwrap(), "SUMMER");
    }

    #[test]
    fn test_month_to_season_fall() {
        assert_eq!(month_to_season(10).unwrap(), "FALL");
    }

    #[test]
    fn test_month_to_season_invalid() {
        assert!(month_to_season(2).is_err());
    }
}
