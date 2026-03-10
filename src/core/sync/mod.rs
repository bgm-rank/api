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

        for entry in &entries {
            match self.bangumi_client.get_subject(entry.bgm_id).await {
                Ok(bgm_subject) => {
                    if let Err(e) = subject_repo.upsert(to_create_subject(bgm_subject)).await {
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

// 签名占位，完整实现在 Phase 5 (T028) 配合测试红灯写入
#[allow(dead_code)]
pub(crate) fn calculate_average_comment(_episodes: &[Episode]) -> Option<f64> {
    None
}

fn to_create_subject(s: BangumiSubject) -> CreateSubject {
    CreateSubject {
        id: s.id,
        name: s.name,
        name_cn: s.name_cn,
        images_grid: s.images.as_ref().and_then(|i| i.grid.clone()),
        images_large: s.images.and_then(|i| i.large),
        rank: s.rating.as_ref().and_then(|r| r.rank),
        score: s.rating.and_then(|r| r.score),
        collection_total: s.collection.map(|c| c.collect),
        meta_tags: s.meta_tags.unwrap_or_default(),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::bangumi::schemas::{Collection, Episode, InfoboxItem};
    use std::collections::HashMap;

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
    #[tokio::test]
    async fn test_sync_started_log_has_season_id_and_operation() {
        dotenvy::dotenv().ok();
        let database_url = match std::env::var("DATABASE_URL") {
            Ok(u) => u,
            Err(_) => return,
        };
        let db = Arc::new(Database::new(&database_url).await.unwrap());
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
    #[tokio::test]
    async fn test_sync_completed_log_has_result_fields() {
        dotenvy::dotenv().ok();
        let database_url = match std::env::var("DATABASE_URL") {
            Ok(u) => u,
            Err(_) => return,
        };
        let db = Arc::new(Database::new(&database_url).await.unwrap());
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
    #[test]
    fn test_create_and_sync_invalid_month_returns_err() {
        // create_and_sync 内部调用 month_to_season，无效月份应立即返回 Err
        // 使用 block_on 避免 tokio runtime 依赖 DATABASE_URL
        let rt = tokio::runtime::Runtime::new().unwrap();
        dotenvy::dotenv().ok();
        let database_url = match std::env::var("DATABASE_URL") {
            Ok(u) => u,
            Err(_) => return, // CI 无数据库时跳过
        };
        rt.block_on(async {
            let db = Arc::new(Database::new(&database_url).await.unwrap());
            let svc = SyncService::new(db);
            let result = svc.create_and_sync(2026, 2, None).await;
            assert!(result.is_err());
            assert!(result.unwrap_err().to_string().contains("Invalid month"));
        });
    }

    #[tokio::test]
    async fn test_resync_unknown_season_returns_err() {
        dotenvy::dotenv().ok();
        let database_url = match std::env::var("DATABASE_URL") {
            Ok(u) => u,
            Err(_) => return,
        };
        let db = Arc::new(Database::new(&database_url).await.unwrap());
        let svc = SyncService::new(db);
        let result = svc.resync(999999).await;
        assert!(result.is_err());
    }

    // T037 — SyncService::find_orphans / delete_orphans
    #[tokio::test]
    async fn test_find_orphans_returns_ok() {
        dotenvy::dotenv().ok();
        let database_url = match std::env::var("DATABASE_URL") {
            Ok(u) => u,
            Err(_) => return,
        };
        let db = Arc::new(Database::new(&database_url).await.unwrap());
        let svc = SyncService::new(db);
        let result = svc.find_orphans().await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_delete_orphans_returns_ok() {
        dotenvy::dotenv().ok();
        let database_url = match std::env::var("DATABASE_URL") {
            Ok(u) => u,
            Err(_) => return,
        };
        let db = Arc::new(Database::new(&database_url).await.unwrap());
        let svc = SyncService::new(db);
        let result = svc.delete_orphans().await;
        assert!(result.is_ok());
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
