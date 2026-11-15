use anyhow::{Context, Result, anyhow};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

use crate::dal::{CreateSeason, CreateSubject, Database};
use crate::dal::{SeasonRepository, SubjectRepository};
use crate::services::bangumi::schemas::{Collection, Episode, InfoboxItem};
use crate::services::bangumi::{BangumiClient, Subject as BangumiSubject};

pub enum SyncProgressEvent {
    Progress {
        total: usize,
        done: usize,
        subject_id: i32,
    },
    Done {
        season_id: i32,
        added: usize,
        removed: usize,
        updated: usize,
        failed: usize,
    },
    Error {
        message: String,
    },
}

#[derive(Debug)]
#[allow(dead_code)]
pub struct SyncResult {
    pub season_id: i32,
    pub added: usize,
    pub removed: usize,
    pub updated: usize,
    pub failed: usize,
}

const DEFAULT_HYDRATE_DELAY: Duration = Duration::from_millis(500);

pub struct SyncService {
    bangumi_client: BangumiClient,
    db: Arc<Database>,
    hydrate_delay: Duration,
}

impl SyncService {
    pub fn new(db: Arc<Database>) -> Self {
        Self {
            bangumi_client: BangumiClient::new(),
            db,
            hydrate_delay: DEFAULT_HYDRATE_DELAY,
        }
    }

    /// 注入自定义 client，供测试指向 mock server（测试默认零延迟）
    #[cfg(test)]
    pub fn with_clients(db: Arc<Database>, bangumi_client: BangumiClient) -> Self {
        Self {
            bangumi_client,
            db,
            hydrate_delay: Duration::ZERO,
        }
    }

    #[cfg(test)]
    pub fn with_hydrate_delay(mut self, delay: Duration) -> Self {
        self.hydrate_delay = delay;
        self
    }

    pub fn hydrate_delay(&self) -> Duration {
        self.hydrate_delay
    }

    pub async fn create_and_sync(
        &self,
        year: i32,
        month: i32,
        name: Option<String>,
        progress_tx: Option<mpsc::Sender<SyncProgressEvent>>,
    ) -> Result<SyncResult> {
        let season_id = year * 100 + month;
        // T015: sync started log
        tracing::info!(season_id = %season_id, operation = "create", "sync started");
        let season_str = month_to_season(month)?;
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

        self.sync_season_subjects(season_id, progress_tx)
            .await
            .map_err(|e| {
                // T017: sync failed log
                tracing::error!(season_id = %season_id, error = %format!("{:#}", e), "sync failed");
                e
            })
    }

    pub async fn resync(
        &self,
        season_id: i32,
        progress_tx: Option<mpsc::Sender<SyncProgressEvent>>,
    ) -> Result<SyncResult> {
        let mut seen_bgm_ids = HashSet::new();
        let mut has_hydrated = false;
        self.resync_in_batch(season_id, &mut seen_bgm_ids, &mut has_hydrated, progress_tx)
            .await
    }

    /// Re-syncs a single season as part of a multi-season batch, skipping `bgm_id`s
    /// already present in `seen_bgm_ids` and sleeping `hydrate_delay` between hydrate calls.
    pub async fn resync_in_batch(
        &self,
        season_id: i32,
        seen_bgm_ids: &mut HashSet<i32>,
        has_hydrated: &mut bool,
        progress_tx: Option<mpsc::Sender<SyncProgressEvent>>,
    ) -> Result<SyncResult> {
        // T015: sync started log
        tracing::info!(season_id = %season_id, operation = "resync", "sync started");
        let pool = self.db.pool();
        SeasonRepository::new(pool)
            .find_by_id(season_id)
            .await?
            .ok_or_else(|| anyhow!("Season {} not found", season_id))?;

        self.sync_season_subjects_in_batch(season_id, seen_bgm_ids, has_hydrated, progress_tx)
            .await
            .map_err(|e| {
                // T017: sync failed log
                tracing::error!(season_id = %season_id, error = %format!("{:#}", e), "sync failed");
                e
            })
    }

    async fn sync_season_subjects(
        &self,
        season_id: i32,
        progress_tx: Option<mpsc::Sender<SyncProgressEvent>>,
    ) -> Result<SyncResult> {
        let mut seen_bgm_ids = HashSet::new();
        let mut has_hydrated = false;
        self.sync_season_subjects_in_batch(
            season_id,
            &mut seen_bgm_ids,
            &mut has_hydrated,
            progress_tx,
        )
        .await
    }

    async fn sync_season_subjects_in_batch(
        &self,
        season_id: i32,
        seen_bgm_ids: &mut HashSet<i32>,
        has_hydrated: &mut bool,
        progress_tx: Option<mpsc::Sender<SyncProgressEvent>>,
    ) -> Result<SyncResult> {
        let start = std::time::Instant::now();
        let pool = self.db.pool();

        let mut bgm_ids = sqlx::query_scalar::<_, i32>(
            r#"
            SELECT DISTINCT mbm.bgm_id
            FROM season_mal sm
            JOIN mal_bgm_matches mbm ON mbm.mal_id = sm.mal_id
            WHERE sm.season_id = ?
              AND sm.is_removed = 0
              AND mbm.status = 'included'
              AND mbm.bgm_id IS NOT NULL
            ORDER BY mbm.bgm_id ASC
            "#,
        )
        .bind(season_id)
        .fetch_all(pool)
        .await
        .context("查询待同步 bgm_id 列表失败")?;

        bgm_ids.retain(|id| seen_bgm_ids.insert(*id));

        let mut updated = 0usize;
        let mut failed = 0usize;
        let today = chrono::Utc::now().date_naive();
        let total = bgm_ids.len();

        for (idx, bgm_id) in bgm_ids.into_iter().enumerate() {
            if *has_hydrated && !self.hydrate_delay.is_zero() {
                tokio::time::sleep(self.hydrate_delay).await;
            }
            *has_hydrated = true;

            match self.hydrate_subject(bgm_id, today).await {
                Ok(()) => updated += 1,
                Err(_) => failed += 1,
            }
            let done = idx + 1;
            if let Some(tx) = &progress_tx {
                let _ = tx
                    .send(SyncProgressEvent::Progress {
                        total,
                        done,
                        subject_id: bgm_id,
                    })
                    .await;
            }
        }

        // 更新 season 的 updated_at 时间戳
        if let Err(e) = SeasonRepository::new(pool)
            .touch_updated_at(season_id)
            .await
        {
            tracing::warn!(season_id = %season_id, error = %e, "touch_updated_at 失败");
        }

        if let Some(tx) = &progress_tx {
            let _ = tx
                .send(SyncProgressEvent::Done {
                    season_id,
                    added: 0,
                    removed: 0,
                    updated,
                    failed,
                })
                .await;
        }

        // T016: sync completed log
        let elapsed_ms = start.elapsed().as_millis() as u64;
        tracing::info!(
            season_id = %season_id,
            added = 0usize,
            updated,
            deleted = 0usize,
            failed,
            elapsed_ms,
            "sync completed"
        );

        Ok(SyncResult {
            season_id,
            added: 0,
            removed: 0,
            updated,
            failed,
        })
    }

    /// 拉取单条番剧的 Bangumi 详情并写库。
    ///
    /// episodes 拉取失败降级为 None（不算失败）；subject 拉取或 upsert 失败返回 Err。
    /// 错误已在内部记日志，调用方只需计数。
    pub async fn hydrate_subject(&self, bgm_id: i32, today: chrono::NaiveDate) -> Result<()> {
        let avg_comment = match self.bangumi_client.get_episodes(bgm_id, 0, 100, 0).await {
            Ok(paged) => calculate_average_comment(&paged.data, today),
            Err(e) => {
                tracing::warn!(subject_id = bgm_id, error = %e, "拉取 episodes 失败，降级为 None");
                None
            }
        };

        let bgm_subject = match self.bangumi_client.get_subject(bgm_id).await {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(subject_id = bgm_id, error = %e, "拉取 subject 失败");
                return Err(e);
            }
        };

        let repo = SubjectRepository::new(self.db.pool());
        repo.upsert(to_create_subject(bgm_subject, avg_comment))
            .await
            .inspect_err(|e| {
                tracing::error!(subject_id = bgm_id, error = %e, "upsert subject 失败");
            })?;

        if let Err(e) = repo.update_last_updated_at(bgm_id).await {
            tracing::warn!(subject_id = bgm_id, error = %e, "update_last_updated_at 失败");
        }

        Ok(())
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
        if deleted && let Err(e) = SubjectRepository::new(pool).delete_orphans().await {
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

/// `month_to_season` 的逆映射：`"2026-spring"` → `202604`
///
/// 无法解析（年份非法、季度名不认识、缺分隔符）时返回 None，由调用方跳过该 key。
pub(crate) fn season_key_to_id(key: &str) -> Option<i32> {
    let (year, season) = key.split_once('-')?;
    let year: i32 = year.parse().ok()?;
    if !(1900..=2999).contains(&year) {
        return None;
    }
    let month = match season {
        "winter" => 1,
        "spring" => 4,
        "summer" => 7,
        "fall" => 10,
        _ => return None,
    };
    Some(year * 100 + month)
}

fn normalize_rank(rank: Option<i32>) -> Option<i32> {
    rank.map(|r| if r == 0 { 999999 } else { r })
}

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

fn calculate_drop_rate(c: &Collection) -> Option<f64> {
    let total = c.wish + c.collect + c.doing + c.on_hold + c.dropped;
    if total == 0 {
        return None;
    }
    Some(c.dropped as f64 / total as f64)
}

pub(crate) fn dedup_preserving_order(tags: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    tags.into_iter()
        .filter(|t| seen.insert(t.clone()))
        .collect()
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
    let collection_total = s
        .collection
        .as_ref()
        .map(|c| c.wish + c.collect + c.doing + c.on_hold + c.dropped);
    let air_weekday = s.infobox.as_deref().and_then(extract_air_weekday);
    let meta_tags = dedup_preserving_order(s.meta_tags.unwrap_or_default());
    let tags = s.tags.map(|tags| {
        let names: Vec<String> = tags.into_iter().filter_map(|t| t.name).collect();
        serde_json::to_string(&names).unwrap_or_else(|_| "[]".to_string())
    });

    CreateSubject {
        id: s.id,
        name: s.name,
        name_cn: s.name_cn,
        air_date: s.date,
        platform: s.platform,
        summary: s.summary,
        tags,
        nsfw: s.nsfw,
        images_grid: s.images.as_ref().and_then(|i| i.grid.clone()),
        images_large: s.images.and_then(|i| i.large),
        rank,
        score,
        collection_total,
        average_comment: avg_comment,
        drop_rate,
        air_weekday,
        meta_tags,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::bangumi::schemas::{Collection, InfoboxItem, Rating};
    use sqlx::SqlitePool;
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
        assert_eq!(extract_air_weekday(&infobox), Some("星期五".to_string()));
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
    async fn test_sync_started_log_has_season_id_and_operation(pool: SqlitePool) {
        let db = Arc::new(Database::from_pool(pool));
        let svc = SyncService::new(db);
        // month=2 无效，但 sync started 日志应在 month_to_season 之前触发
        let _ = svc.create_and_sync(2026, 2, None, None).await;
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
    async fn test_sync_completed_log_has_result_fields(pool: SqlitePool) {
        let db = Arc::new(Database::from_pool(pool));
        let svc = SyncService::new(db);
        let result = svc.create_and_sync(2999, 1, None, None).await;
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
    async fn test_create_and_sync_invalid_month_returns_err(pool: SqlitePool) {
        let db = Arc::new(Database::from_pool(pool));
        let svc = SyncService::new(db);
        let result = svc.create_and_sync(2026, 2, None, None).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Invalid month"));
    }

    #[sqlx::test]
    async fn test_resync_unknown_season_returns_err(pool: SqlitePool) {
        let db = Arc::new(Database::from_pool(pool));
        let svc = SyncService::new(db);
        let result = svc.resync(999999, None).await;
        assert!(result.is_err());
    }

    // T037 — SyncService::find_orphans / delete_orphans
    #[sqlx::test]
    async fn test_find_orphans_returns_ok(pool: SqlitePool) {
        let db = Arc::new(Database::from_pool(pool));
        let svc = SyncService::new(db);
        let result = svc.find_orphans().await;
        assert!(result.is_ok());
    }

    #[sqlx::test]
    async fn test_delete_orphans_returns_ok(pool: SqlitePool) {
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
    async fn test_delete_season_existing_returns_true(pool: SqlitePool) {
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
        let found = SeasonRepository::new(svc.db.pool())
            .find_by_id(202699)
            .await
            .unwrap();
        assert!(found.is_none(), "season should be deleted from DB");
    }

    #[sqlx::test]
    async fn test_delete_season_nonexistent_returns_false(pool: SqlitePool) {
        let db = Arc::new(Database::from_pool(pool));
        let svc = SyncService::new(db);
        let result = svc.delete_season(999989).await;
        assert!(result.is_ok());
        assert!(
            !result.unwrap(),
            "should return false for non-existent season"
        );
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
            make_episode(1, 0, "2026-01-01", Some(10)),  // main, aired
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

    // T040 — to_create_subject 对 meta_tags 去重
    #[test]
    fn test_to_create_subject_deduplicates_meta_tags() {
        let s = BangumiSubject {
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
            infobox: None,
            volumes: None,
            eps: None,
            total_episodes: None,
            rating: None,
            collection: None,
            meta_tags: Some(vec!["TV".to_string(), "TV".to_string(), "动作".to_string()]),
            tags: None,
        };
        let create = to_create_subject(s, None);
        assert_eq!(create.meta_tags, vec!["TV".to_string(), "动作".to_string()]);
    }

    // T033 — sync_season_data 完成后 touch_updated_at 机制端到端验证
    // 通过 QueryService::list_seasons 验证 Service 层字段透传
    #[sqlx::test]
    async fn test_sync_season_data_updates_season_updated_at(pool: SqlitePool) -> sqlx::Result<()> {
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

    // T008 [US2]: dropped=100, others=0 → rate=1.0（杀死 + → - 和 + → * 变异体）
    #[test]
    fn test_calculate_drop_rate_all_dropped_returns_1() {
        let c = Collection {
            wish: 0,
            collect: 0,
            doing: 0,
            on_hold: 0,
            dropped: 100,
        };
        let rate = calculate_drop_rate(&c).unwrap();
        assert!((rate - 1.0).abs() < 0.0001, "expected 1.0, got {rate}");
    }

    // T009 [US2]: dropped=0, collect=100 → rate=0.0（杀死 / → * 和 / → % 变异体）
    #[test]
    fn test_calculate_drop_rate_zero_dropped_returns_0() {
        let c = Collection {
            wish: 0,
            collect: 100,
            doing: 0,
            on_hold: 0,
            dropped: 0,
        };
        let rate = calculate_drop_rate(&c).unwrap();
        assert!(rate.abs() < 0.0001, "expected 0.0, got {rate}");
    }

    // T010 [US2]: airdate == today 应被包含（验证 <= 而非 <，杀死 <= → > 变异体）
    #[test]
    fn test_calculate_average_comment_today_boundary() {
        use chrono::NaiveDate;
        let today = NaiveDate::from_ymd_opt(2026, 3, 10).unwrap();
        let episodes = vec![make_episode(1, 0, "2026-03-10", Some(42))];
        let avg = calculate_average_comment(&episodes, today);
        assert_eq!(avg, Some(42.0), "airdate == today 的集应被纳入计算");
    }

    // T011 [US2]: count={{"7": 1}} → score=7.0（验证单条目加权计算，杀死 * → / 变异体）
    #[test]
    fn test_calculate_exact_score_single_entry() {
        let mut count = HashMap::new();
        count.insert("7".to_string(), 2); // v=2 使 * 与 / 产生不同结果
        let score = calculate_exact_score(&count).unwrap();
        assert!((score - 7.0).abs() < 0.0001, "expected 7.0, got {score}");
    }

    // T012 [US2]: ["A","A","A"] → ["A"]（杀死 dedup 返回常量变异体）
    #[test]
    fn test_dedup_preserving_order_all_duplicates() {
        let input = vec!["A".to_string(), "A".to_string(), "A".to_string()];
        assert_eq!(dedup_preserving_order(input), vec!["A".to_string()]);
    }

    // T013 [US2]: ["C","A","B","A","C"] → ["C","A","B"]（验证顺序保持）
    #[test]
    fn test_dedup_preserving_order_preserves_order_with_multiple() {
        let input = vec![
            "C".to_string(),
            "A".to_string(),
            "B".to_string(),
            "A".to_string(),
            "C".to_string(),
        ];
        assert_eq!(
            dedup_preserving_order(input),
            vec!["C".to_string(), "A".to_string(), "B".to_string()]
        );
    }

    // T014 [US2]: 错误消息包含 "Invalid month"（杀死 month_to_season 返回空字符串变异体）
    #[test]
    fn test_month_to_season_invalid_contains_message() {
        let err = month_to_season(2).unwrap_err();
        assert!(
            err.to_string().contains("Invalid month"),
            "error should contain 'Invalid month', got: {err}"
        );
    }

    // ── 补充测试：杀死 calculate_average_comment L319 / → * 变异体 ──

    #[test]
    fn test_calculate_average_comment_multiple_episodes_exact_avg() {
        use chrono::NaiveDate;
        let today = NaiveDate::from_ymd_opt(2026, 3, 10).unwrap();
        // 2 个已播出集，评论数分别为 10 和 20，平均值应为 15.0
        // / → * 变异体: (10+20)*2 = 60 ≠ 15.0，测试可杀死该变异体
        let episodes = vec![
            make_episode(1, 0, "2026-01-01", Some(10)),
            make_episode(2, 0, "2026-02-01", Some(20)),
        ];
        let avg = calculate_average_comment(&episodes, today);
        assert!(
            (avg.unwrap() - 15.0).abs() < 0.0001,
            "expected 15.0, got {:?}",
            avg
        );
    }

    // ── 补充测试：杀死 to_create_subject L333 collection_total 算术变异体 ──

    #[test]
    fn test_to_create_subject_collection_total_sums_all_components() {
        // wish=1,collect=2,doing=3,on_hold=4,dropped=5 → total=15
        // + → - 变异体会给出错误总和，+ → * 变异体同样
        let s = make_subject_with_infobox(
            None,
            Some(Collection {
                wish: 1,
                collect: 2,
                doing: 3,
                on_hold: 4,
                dropped: 5,
            }),
        );
        let create = to_create_subject(s, None);
        assert_eq!(create.collection_total, Some(15));
    }

    // ── 补充测试：杀死 to_create_subject L338-342,L345 字段删除变异体 ──

    #[test]
    fn test_to_create_subject_maps_id_name_name_cn() {
        use crate::services::bangumi::schemas::Images;
        let s = BangumiSubject {
            id: 42,
            _type: 2,
            name: Some("テスト".to_string()),
            name_cn: Some("测试".to_string()),
            images: Some(Images {
                large: Some("large_url".to_string()),
                common: None,
                medium: None,
                small: None,
                grid: Some("grid_url".to_string()),
            }),
            summary: None,
            series: None,
            nsfw: None,
            locked: None,
            date: None,
            platform: None,
            infobox: None,
            volumes: None,
            eps: None,
            total_episodes: None,
            rating: None,
            collection: Some(Collection {
                wish: 1,
                collect: 2,
                doing: 3,
                on_hold: 4,
                dropped: 5,
            }),
            meta_tags: None,
            tags: None,
        };
        let create = to_create_subject(s, None);
        assert_eq!(create.id, 42); // kills L338 delete field id
        assert_eq!(create.name, Some("テスト".to_string())); // kills L339 delete field name
        assert_eq!(create.name_cn, Some("测试".to_string())); // kills L340 delete field name_cn
        assert_eq!(create.images_grid, Some("grid_url".to_string())); // kills L341 delete field images_grid
        assert_eq!(create.images_large, Some("large_url".to_string())); // kills L342 delete field images_large
        assert_eq!(create.collection_total, Some(15)); // kills L345 delete field collection_total
    }

    // ── season_key_to_id ──────────────────────────────────────────────────────

    #[test]
    fn test_season_key_to_id_all_seasons() {
        assert_eq!(season_key_to_id("2026-winter"), Some(202601));
        assert_eq!(season_key_to_id("2026-spring"), Some(202604));
        assert_eq!(season_key_to_id("2026-summer"), Some(202607));
        assert_eq!(season_key_to_id("2026-fall"), Some(202610));
    }

    #[test]
    fn test_season_key_to_id_roundtrip_with_month_to_season() {
        for (key, season_id, season) in [
            ("2025-winter", 202501, "WINTER"),
            ("2025-spring", 202504, "SPRING"),
            ("2025-summer", 202507, "SUMMER"),
            ("2025-fall", 202510, "FALL"),
        ] {
            assert_eq!(season_key_to_id(key), Some(season_id));
            assert_eq!(month_to_season(season_id % 100).unwrap(), season);
        }
    }

    #[test]
    fn test_season_key_to_id_rejects_bad_input() {
        assert_eq!(season_key_to_id("2026-autumn"), None); // 上游用 fall 不是 autumn
        assert_eq!(season_key_to_id("2026spring"), None); // 缺分隔符
        assert_eq!(season_key_to_id("abcd-spring"), None); // 年份非数字
        assert_eq!(season_key_to_id("1899-spring"), None); // 年份越界
        assert_eq!(season_key_to_id("3000-spring"), None); // 年份越界
        assert_eq!(season_key_to_id(""), None);
    }

    // ── to_create_subject 扩展字段测试 ────────────────────────────────────────

    #[test]
    fn test_to_create_subject_maps_air_date_platform_summary_tags_nsfw() {
        use crate::services::bangumi::schemas::Tag;
        let s = BangumiSubject {
            id: 515759,
            _type: 2,
            name: Some("葬送のフリーレン 第2期".to_string()),
            name_cn: Some("葬送的芙莉莲 第二季".to_string()),
            summary: Some("勇者死后...".to_string()),
            series: None,
            nsfw: Some(false),
            locked: None,
            date: Some("2026-01-16".to_string()),
            platform: Some("TV".to_string()),
            images: None,
            infobox: None,
            volumes: None,
            eps: None,
            total_episodes: None,
            rating: None,
            collection: None,
            meta_tags: None,
            tags: Some(vec![
                Tag {
                    name: Some("奇幻".to_string()),
                    count: Some(120),
                },
                Tag {
                    name: None,
                    count: Some(5),
                },
                Tag {
                    name: Some("冒险".to_string()),
                    count: Some(80),
                },
            ]),
        };
        let create = to_create_subject(s, None);
        assert_eq!(create.air_date.as_deref(), Some("2026-01-16"));
        assert_eq!(create.platform.as_deref(), Some("TV"));
        assert_eq!(create.summary.as_deref(), Some("勇者死后..."));
        assert_eq!(create.nsfw, Some(false));
        assert_eq!(create.tags.as_deref(), Some(r#"["奇幻","冒险"]"#));
    }

    // ── create_and_sync / resync hydrate 行为测试 ─────────────────────────────

    async fn seed_mal_match_for_sync(
        pool: &SqlitePool,
        season_id: i32,
        mal_id: i32,
        bgm_id: Option<i32>,
        status: &str,
        is_removed: bool,
    ) -> sqlx::Result<()> {
        use crate::dal::dto::{UpdateMalBgmMatch, UpsertMalAnime};
        use crate::dal::repositories::{
            MalAnimeRepository, MalBgmMatchRepository, SeasonMalRepository,
        };

        if let Some(id) = bgm_id
            && SubjectRepository::new(pool).find_by_id(id).await?.is_none()
        {
            SubjectRepository::new(pool)
                .create(CreateSubject {
                    id,
                    ..Default::default()
                })
                .await?;
        }

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
                    source: Some("human".to_string()),
                    bgm_id,
                    ..Default::default()
                },
            )
            .await?;

        Ok(())
    }

    #[sqlx::test]
    async fn test_resync_hydrates_only_distinct_included_non_removed_subjects(
        pool: SqlitePool,
    ) -> sqlx::Result<()> {
        let mut bgm_server = mockito::Server::new_async().await;

        let ep_mock = bgm_server
            .mock("GET", mockito::Matcher::Regex(r"^/v0/episodes".into()))
            .with_header("content-type", "application/json")
            .with_body(r#"{"total":0,"limit":100,"offset":0,"data":[]}"#)
            .expect(1)
            .create_async()
            .await;

        let subj_mock = bgm_server
            .mock("GET", "/v0/subjects/444957")
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"id":444957,"type":2,"name":"燃比娃","name_cn":"燃比娃",
                    "date":"2026-04-01","platform":"剧场版","summary":"测试简介","nsfw":false,
                    "images":null,
                    "rating":{"rank":100,"total":10,"count":{"8":10},"score":8.0},
                    "collection":{"wish":1,"collect":2,"doing":3,"on_hold":4,"dropped":5},
                    "infobox":[],"meta_tags":["剧场版"],"tags":[{"name":"奇幻","count":10}]}"#,
            )
            .expect(1)
            .create_async()
            .await;

        let db = Arc::new(Database::from_pool(pool.clone()));
        let svc = SyncService::with_clients(
            Arc::clone(&db),
            BangumiClient::with_base_url(&bgm_server.url()),
        );

        SeasonRepository::new(&pool)
            .upsert(CreateSeason {
                season_id: 202604,
                year: 2026,
                season: "SPRING".to_string(),
                name: None,
            })
            .await?;
        SeasonRepository::new(&pool)
            .upsert(CreateSeason {
                season_id: 202601,
                year: 2026,
                season: "WINTER".to_string(),
                name: None,
            })
            .await?;

        // 两个 mal_id 指向同一个 bgm_id=444957 (included + non-removed): 应去重只拉一次
        seed_mal_match_for_sync(&pool, 202604, 1001, Some(444957), "included", false).await?;
        seed_mal_match_for_sync(&pool, 202604, 1002, Some(444957), "included", false).await?;
        // is_removed = 1: 不应被 hydrate
        seed_mal_match_for_sync(&pool, 202604, 1003, Some(444958), "included", true).await?;
        // pending / excluded: 不应被 hydrate
        seed_mal_match_for_sync(&pool, 202604, 1004, Some(444959), "pending", false).await?;
        seed_mal_match_for_sync(&pool, 202604, 1005, Some(444960), "excluded", false).await?;
        // 其他季度: 不应被 hydrate
        seed_mal_match_for_sync(&pool, 202601, 1006, Some(444961), "included", false).await?;

        let (tx, mut rx) = mpsc::channel(16);
        let result = svc.resync(202604, Some(tx)).await.unwrap();
        assert_eq!(result.season_id, 202604);
        assert_eq!(result.updated, 1);
        assert_eq!(result.failed, 0);

        ep_mock.assert_async().await;
        subj_mock.assert_async().await;

        let subject = SubjectRepository::new(&pool)
            .find_by_id(444957)
            .await?
            .expect("subject 444957 should exist");
        assert_eq!(subject.name_cn.as_deref(), Some("燃比娃"));
        assert_eq!(subject.air_date.as_deref(), Some("2026-04-01"));
        assert_eq!(subject.platform.as_deref(), Some("剧场版"));
        assert_eq!(subject.summary.as_deref(), Some("测试简介"));
        assert_eq!(subject.tags.as_deref(), Some(r#"["奇幻"]"#));
        assert_eq!(subject.nsfw, Some(false));
        assert!(subject.last_updated_at.is_some());

        // 未被 hydrate 的条目保持骨架状态
        assert!(
            SubjectRepository::new(&pool)
                .find_by_id(444958)
                .await?
                .unwrap()
                .last_updated_at
                .is_none()
        );

        let mut saw_progress = false;
        let mut saw_done = false;
        while let Ok(ev) = rx.try_recv() {
            match ev {
                SyncProgressEvent::Progress {
                    total,
                    done,
                    subject_id,
                } => {
                    assert_eq!(total, 1);
                    assert_eq!(done, 1);
                    assert_eq!(subject_id, 444957);
                    saw_progress = true;
                }
                SyncProgressEvent::Done {
                    season_id,
                    updated,
                    failed,
                    ..
                } => {
                    assert_eq!(season_id, 202604);
                    assert_eq!(updated, 1);
                    assert_eq!(failed, 0);
                    saw_done = true;
                }
                SyncProgressEvent::Error { .. } => panic!("unexpected error event"),
            }
        }
        assert!(saw_progress && saw_done);

        Ok(())
    }

    #[sqlx::test]
    async fn test_create_and_sync_creates_season_and_hydrates_included_subjects(
        pool: SqlitePool,
    ) -> sqlx::Result<()> {
        let mut bgm_server = mockito::Server::new_async().await;
        let _ep_mock = bgm_server
            .mock("GET", mockito::Matcher::Regex(r"^/v0/episodes".into()))
            .with_header("content-type", "application/json")
            .with_body(r#"{"total":0,"limit":100,"offset":0,"data":[]}"#)
            .create_async()
            .await;
        let _subj_mock = bgm_server
            .mock("GET", "/v0/subjects/515759")
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"id":515759,"type":2,"name":"葬送のフリーレン","name_cn":"葬送的芙莉莲",
                    "date":"2026-01-16","platform":"TV","summary":"简介","nsfw":false,
                    "rating":{"rank":10,"total":10,"count":{"9":10}},"collection":{"wish":1,"collect":2,"doing":3,"on_hold":4,"dropped":5},
                    "infobox":[],"meta_tags":["TV"],"tags":[{"name":"奇幻","count":100}]}"#,
            )
            .create_async()
            .await;

        let db = Arc::new(Database::from_pool(pool.clone()));
        let svc = SyncService::with_clients(
            Arc::clone(&db),
            BangumiClient::with_base_url(&bgm_server.url()),
        );

        let r = svc
            .create_and_sync(2026, 1, Some("2026冬".to_string()), None)
            .await
            .unwrap();
        assert_eq!(r.season_id, 202601);
        assert_eq!(r.updated, 0);

        seed_mal_match_for_sync(&pool, 202601, 59978, Some(515759), "included", false).await?;

        let r2 = svc.create_and_sync(2026, 1, None, None).await.unwrap();
        assert_eq!(r2.updated, 1);
        assert_eq!(r2.failed, 0);

        let season = SeasonRepository::new(&pool)
            .find_by_id(202601)
            .await?
            .unwrap();
        assert_eq!(season.name.as_deref(), Some("2026冬"));

        let subject = SubjectRepository::new(&pool)
            .find_by_id(515759)
            .await?
            .unwrap();
        assert_eq!(subject.name_cn.as_deref(), Some("葬送的芙莉莲"));
        assert_eq!(subject.air_date.as_deref(), Some("2026-01-16"));

        Ok(())
    }

    #[sqlx::test]
    async fn test_sync_service_hydrate_delay_default_and_configurable(
        pool: SqlitePool,
    ) -> sqlx::Result<()> {
        let db = Arc::new(Database::from_pool(pool.clone()));
        let default_svc = SyncService::new(Arc::clone(&db));
        assert_eq!(default_svc.hydrate_delay(), Duration::from_millis(500));

        let mut bgm_server = mockito::Server::new_async().await;
        let ep_mock = bgm_server
            .mock("GET", mockito::Matcher::Regex(r"^/v0/episodes".into()))
            .with_header("content-type", "application/json")
            .with_body(r#"{"total":0,"limit":100,"offset":0,"data":[]}"#)
            .expect(2)
            .create_async()
            .await;
        let subj_mock = bgm_server
            .mock("GET", mockito::Matcher::Regex(r"^/v0/subjects/\d+$".into()))
            .with_header("content-type", "application/json")
            .with_body(r#"{"id":101,"type":2,"name":"Test"}"#)
            .expect(2)
            .create_async()
            .await;

        let delayed_svc = SyncService::with_clients(
            Arc::clone(&db),
            BangumiClient::with_base_url(&bgm_server.url()),
        )
        .with_hydrate_delay(Duration::from_millis(30));
        assert_eq!(delayed_svc.hydrate_delay(), Duration::from_millis(30));

        SeasonRepository::new(&pool)
            .upsert(CreateSeason {
                season_id: 202604,
                year: 2026,
                season: "SPRING".to_string(),
                name: Some("2026春".to_string()),
            })
            .await?;
        seed_mal_match_for_sync(&pool, 202604, 1001, Some(101), "included", false).await?;
        seed_mal_match_for_sync(&pool, 202604, 1002, Some(102), "included", false).await?;

        let started = std::time::Instant::now();
        let res = delayed_svc.resync(202604, None).await.unwrap();
        let elapsed = started.elapsed();

        assert_eq!(res.updated, 2);
        assert!(
            elapsed >= Duration::from_millis(25),
            "expected delay between 2 hydrate calls, elapsed={:?}",
            elapsed
        );
        ep_mock.assert_async().await;
        subj_mock.assert_async().await;

        // Resync must preserve existing season name (no redundant early upsert)
        let season = SeasonRepository::new(&pool)
            .find_by_id(202604)
            .await?
            .unwrap();
        assert_eq!(season.name.as_deref(), Some("2026春"));

        Ok(())
    }
}
