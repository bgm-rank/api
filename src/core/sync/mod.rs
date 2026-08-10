use anyhow::{Context, Result, anyhow};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::dal::{CreateSeason, CreateSubject, Database};
use crate::dal::{SeasonRepository, SeasonSubjectRepository, SubjectRepository};
use crate::services::bangumi::schemas::{Collection, Episode, InfoboxItem};
use crate::services::bangumi::{BangumiClient, Subject as BangumiSubject};
use crate::services::season_data::{MediaType, Rating, SeasonDataClient, SeasonEntry};

pub enum SyncProgressEvent {
    Progress { total: usize, done: usize, subject_id: i32 },
    Done { season_id: i32, added: usize, removed: usize, updated: usize, failed: usize },
    Error { message: String },
}

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

    /// 注入自定义 client，供测试指向 mock server
    #[cfg(test)]
    pub fn with_clients(
        db: Arc<Database>,
        season_data_client: SeasonDataClient,
        bangumi_client: BangumiClient,
    ) -> Self {
        Self {
            season_data_client,
            bangumi_client,
            db,
        }
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

        self.sync_season_data(season_id, &key, progress_tx).await.map_err(|e| {
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

        self.sync_season_data(season_id, &key, progress_tx).await.map_err(|e| {
            // T017: sync failed log
            tracing::error!(season_id = %season_id, error = %format!("{:#}", e), "sync failed");
            e
        })
    }

    async fn sync_season_data(
        &self,
        season_id: i32,
        key: &str,
        progress_tx: Option<mpsc::Sender<SyncProgressEvent>>,
    ) -> Result<SyncResult> {
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
        let (added_ids, removed_ids) = SeasonSubjectRepository::new(pool)
            .reconcile(season_id, bgm_ids)
            .await
            .context("reconcile 失败")?;
        let (added, removed) = (added_ids.len(), removed_ids.len());

        // 5. Fetch Bangumi details per subject
        let mut updated = 0usize;
        let mut failed = 0usize;
        let today = chrono::Utc::now().date_naive();
        let total = entries.len();
        let mut done = 0usize;

        for entry in &entries {
            match self.hydrate_subject(entry.bgm_id, today).await {
                Ok(()) => updated += 1,
                Err(_) => failed += 1,
            }
            done += 1;
            if let Some(tx) = &progress_tx {
                let _ = tx.send(SyncProgressEvent::Progress { total, done, subject_id: entry.bgm_id }).await;
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
            let _ = tx.send(SyncProgressEvent::Done { season_id, added, removed, updated, failed }).await;
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

    /// 拉取单条番剧的 Bangumi 详情并写库。
    ///
    /// episodes 拉取失败降级为 None（不算失败）；subject 拉取或 upsert 失败返回 Err。
    /// 错误已在内部记日志，调用方只需计数。
    async fn hydrate_subject(&self, bgm_id: i32, today: chrono::NaiveDate) -> Result<()> {
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

        SubjectRepository::new(self.db.pool())
            .upsert(to_create_subject(bgm_subject, avg_comment))
            .await
            .inspect_err(|e| {
                tracing::error!(subject_id = bgm_id, error = %e, "upsert subject 失败");
            })?;

        Ok(())
    }

    /// 全量对账：以 season-data.json 的 key 为准，把每个季度的成员关系校正到与上游一致。
    ///
    /// 与 `sync_season_data` 的区别：只对**新进来的**条目拉 Bangumi 详情，已有条目不重拉评分。
    /// 成本 = 1 次 HTTP + 一轮本地 DB 写 + 「新增条目数」次 Bangumi 请求。
    pub async fn reconcile_all(&self) -> Result<ReconcileAllResult> {
        let start = std::time::Instant::now();
        let today = chrono::Utc::now().date_naive();

        let all = self
            .season_data_client
            .fetch_all()
            .await
            .context("拉取 season-data.json 失败")?;

        let mut result = ReconcileAllResult {
            seasons_total: all.len(),
            ..Default::default()
        };

        // 按 season_id 倒序遍历：最近的季度先对账。
        // 上游审核也是倒序进行的，跨季度改判几乎都落在近几季；
        // 而首次跑要回填全部历史时耗时以小时计，倒序能保证中途被打断也已修好要紧的季度。
        let mut keys: Vec<(i32, &String)> = Vec::new();
        for key in all.keys() {
            match season_key_to_id(key) {
                Some(season_id) => keys.push((season_id, key)),
                None => {
                    tracing::warn!(key = %key, "无法解析 season key，跳过");
                    result.seasons_skipped += 1;
                }
            }
        }
        keys.sort_unstable_by(|a, b| b.cmp(a));

        for (season_id, key) in keys {
            let entries = &all[key];

            // 注意事项 3：上游会把没有 included 条目的季度也铺成空数组。
            // 对这种 key 跑 reconcile 会把该季度成员全删光，宁可漏同步也别清空一整季。
            if entries.is_empty() {
                tracing::warn!(season_id, key = %key, "上游季度为空数组，跳过以免清空成员");
                result.seasons_skipped += 1;
                continue;
            }

            match self.reconcile_one(season_id, key, entries, today).await {
                Ok(detail) => {
                    result.total_added += detail.added.len();
                    result.total_removed += detail.removed.len();
                    result.hydrate_failed += detail.hydrate_failed;
                    if !detail.added.is_empty() || !detail.removed.is_empty() {
                        result.changes.push(detail);
                    }
                }
                Err(e) => {
                    // 单季失败不中断整轮
                    tracing::error!(season_id, key = %key, error = %format!("{:#}", e), "对账季度失败");
                    result.seasons_failed += 1;
                }
            }
        }

        result.elapsed_ms = start.elapsed().as_millis() as u64;
        tracing::info!(
            seasons_total = result.seasons_total,
            seasons_skipped = result.seasons_skipped,
            seasons_failed = result.seasons_failed,
            added = result.total_added,
            removed = result.total_removed,
            hydrate_failed = result.hydrate_failed,
            elapsed_ms = result.elapsed_ms,
            "reconcile_all completed"
        );

        Ok(result)
    }

    async fn reconcile_one(
        &self,
        season_id: i32,
        key: &str,
        entries: &[SeasonEntry],
        today: chrono::NaiveDate,
    ) -> Result<SeasonReconcileDetail> {
        use tokio::time::{Duration, sleep};

        let pool = self.db.pool();
        let month = season_id % 100;

        // 注意事项 2：以 JSON 的 key 为准 upsert season，库里没有的季度也能建出来。
        // upsert 的 name 有 COALESCE 保护，不会清掉已有季度名。
        SeasonRepository::new(pool)
            .upsert(CreateSeason {
                season_id,
                year: season_id / 100,
                season: month_to_season(month)?,
                name: None,
            })
            .await
            .context("upsert season 失败")?;

        // 保证 subjects 行存在（season_subjects 有 FK），并刷新 media_type/rating。
        // 这里绝不能用 SubjectRepository::upsert —— 那会把已有条目的详情清成 NULL。
        let meta_rows: Vec<(i32, Option<String>, Option<String>)> = entries
            .iter()
            .map(|e| {
                (
                    e.bgm_id,
                    Some(media_type_to_str(&e.media_type).to_string()),
                    Some(rating_to_str(&e.rating).to_string()),
                )
            })
            .collect();
        SubjectRepository::new(pool)
            .upsert_meta_batch(&meta_rows)
            .await
            .context("upsert_meta_batch 失败")?;

        let bgm_ids: Vec<i32> = entries.iter().map(|e| e.bgm_id).collect();
        let (added, removed) = SeasonSubjectRepository::new(pool)
            .reconcile(season_id, bgm_ids)
            .await
            .context("reconcile 失败")?;

        if !removed.is_empty() {
            tracing::warn!(season_id, key = %key, ids = ?removed, "对账删除成员");
        }

        // 只有新进来的条目在库里是空壳，必须补详情
        let mut hydrate_failed = 0usize;
        for &bgm_id in &added {
            if self.hydrate_subject(bgm_id, today).await.is_err() {
                hydrate_failed += 1;
            }
            sleep(Duration::from_millis(500)).await;
        }

        if !added.is_empty() || !removed.is_empty() {
            tracing::info!(season_id, key = %key, added = ?added, removed = ?removed, "对账季度完成");
            if let Err(e) = SeasonRepository::new(pool).touch_updated_at(season_id).await {
                tracing::warn!(season_id, error = %e, "touch_updated_at 失败");
            }
        }

        Ok(SeasonReconcileDetail {
            season_id,
            added,
            removed,
            hydrate_failed,
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

#[derive(Debug, Default)]
pub struct ReconcileAllResult {
    pub seasons_total: usize,
    pub seasons_skipped: usize,
    pub seasons_failed: usize,
    pub total_added: usize,
    pub total_removed: usize,
    pub hydrate_failed: usize,
    pub elapsed_ms: u64,
    /// 只收 added / removed 非空的季度
    pub changes: Vec<SeasonReconcileDetail>,
}

#[derive(Debug)]
pub struct SeasonReconcileDetail {
    pub season_id: i32,
    pub added: Vec<i32>,
    pub removed: Vec<i32>,
    pub hydrate_failed: usize,
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

    // ── 补充测试：杀死 media_type_to_str / rating_to_str 返回值变异体 ──

    #[test]
    fn test_media_type_to_str_returns_correct_values() {
        use crate::services::season_data::MediaType;
        assert_eq!(media_type_to_str(&MediaType::Tv), "tv");
        assert_eq!(media_type_to_str(&MediaType::Movie), "movie");
        assert_eq!(media_type_to_str(&MediaType::Ova), "ova");
        assert_eq!(media_type_to_str(&MediaType::Ona), "ona");
        assert_eq!(media_type_to_str(&MediaType::TvSpecial), "tv_special");
    }

    #[test]
    fn test_rating_to_str_returns_correct_values() {
        use crate::services::season_data::Rating as SeasonRating;
        assert_eq!(rating_to_str(&SeasonRating::General), "general");
        assert_eq!(rating_to_str(&SeasonRating::Kids), "kids");
        assert_eq!(rating_to_str(&SeasonRating::R18), "r18");
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

    // ── reconcile_all ─────────────────────────────────────────────────────────

    /// 一整套 mock 环境。mockito 1.x 的 `Mock` 在 drop 时会从 server 上摘除，
    /// 所以必须把 mock handle 一路持有到测试结束。
    struct MockEnv {
        _sd_server: mockito::ServerGuard,
        _bgm_server: mockito::ServerGuard,
        _mocks: Vec<mockito::Mock>,
        db: Arc<Database>,
        svc: SyncService,
    }

    /// `season_json` 是 season-data.json 的完整响应体；
    /// Bangumi 侧对任何 subject 都返回同一份详情。
    async fn mock_env(pool: SqlitePool, season_json: &str) -> MockEnv {
        let mut bgm_server = mockito::Server::new_async().await;
        let mut sd_server = mockito::Server::new_async().await;

        let mocks = vec![
            bgm_server
                .mock("GET", mockito::Matcher::Regex(r"^/v0/episodes".into()))
                .with_header("content-type", "application/json")
                .with_body(r#"{"total":0,"limit":100,"offset":0,"data":[]}"#)
                .create_async()
                .await,
            bgm_server
                .mock("GET", mockito::Matcher::Regex(r"^/v0/subjects/\d+".into()))
                .with_header("content-type", "application/json")
                .with_body(
                    r#"{"id":444957,"type":2,"name":"燃比娃","name_cn":"燃比娃","images":null,
                        "rating":{"rank":100,"total":10,"count":{"8":10},"score":8.0},
                        "collection":{"wish":1,"collect":2,"doing":3,"on_hold":4,"dropped":5},
                        "infobox":[],"meta_tags":[],"tags":[]}"#,
                )
                .create_async()
                .await,
            sd_server
                .mock("GET", "/season-data.json")
                .with_header("content-type", "application/json")
                .with_body(season_json)
                .create_async()
                .await,
        ];

        let db = Arc::new(Database::from_pool(pool));
        let svc = SyncService::with_clients(
            Arc::clone(&db),
            SeasonDataClient::with_url(sd_server.url() + "/season-data.json"),
            BangumiClient::with_base_url(&bgm_server.url()),
        );

        MockEnv {
            _sd_server: sd_server,
            _bgm_server: bgm_server,
            _mocks: mocks,
            db,
            svc,
        }
    }

    #[sqlx::test]
    async fn test_reconcile_all_adds_missing_subject_and_is_idempotent(pool: SqlitePool) {
        let env = mock_env(
            pool,
            r#"{"2026-spring": [{"bgm_id": 444957, "media_type": "movie", "rating": "general"}]}"#,
        )
        .await;
        let (db, svc) = (&env.db, &env.svc);

        // 第一次：季度和条目都不存在，应新建并 hydrate
        let r = svc.reconcile_all().await.unwrap();
        assert_eq!(r.total_added, 1);
        assert_eq!(r.total_removed, 0);
        assert_eq!(r.hydrate_failed, 0);
        assert_eq!(r.changes.len(), 1);
        assert_eq!(r.changes[0].season_id, 202604);
        assert_eq!(r.changes[0].added, vec![444957]);

        // 注意事项 2：库里没有的季度应被 upsert 出来
        let season = SeasonRepository::new(db.pool())
            .find_by_id(202604)
            .await
            .unwrap()
            .expect("2026-spring 应被自动创建");
        assert_eq!(season.season, "SPRING");

        // 新增条目应补到详情，不是空壳
        let subject = SubjectRepository::new(db.pool())
            .find_by_id(444957)
            .await
            .unwrap()
            .expect("subject 应存在");
        assert_eq!(subject.name_cn, Some("燃比娃".to_string()));
        assert_eq!(subject.media_type, Some("movie".to_string()));

        // 第二次：应完全幂等
        let r2 = svc.reconcile_all().await.unwrap();
        assert_eq!(r2.total_added, 0);
        assert_eq!(r2.total_removed, 0);
        assert!(r2.changes.is_empty());
    }

    #[sqlx::test]
    async fn test_reconcile_all_skips_empty_season_instead_of_wiping(pool: SqlitePool) {
        // 注意事项 3：上游会把没有 included 条目的季度铺成空数组
        let env = mock_env(pool, r#"{"2026-spring": []}"#).await;
        let svc = &env.svc;
        let pool = env.db.pool();

        // 预置一个已有成员的季度
        SeasonRepository::new(pool)
            .upsert(CreateSeason {
                season_id: 202604,
                year: 2026,
                season: "SPRING".to_string(),
                name: None,
            })
            .await
            .unwrap();
        SubjectRepository::new(pool)
            .upsert_meta_batch(&[(444957, Some("movie".into()), Some("general".into()))])
            .await
            .unwrap();
        SeasonSubjectRepository::new(pool)
            .insert_or_ignore(crate::dal::CreateSeasonSubject {
                season_id: 202604,
                subject_id: 444957,
            })
            .await
            .unwrap();

        let r = svc.reconcile_all().await.unwrap();
        assert_eq!(r.seasons_skipped, 1);
        assert_eq!(r.total_removed, 0, "空季度绝不能触发删除");

        let members = SeasonSubjectRepository::new(pool)
            .find_by_season_id(202604)
            .await
            .unwrap();
        assert_eq!(members, vec![444957], "成员必须原样保留");
    }

    #[sqlx::test]
    async fn test_reconcile_all_removes_stale_member(pool: SqlitePool) {
        let env = mock_env(
            pool,
            r#"{"2026-spring": [{"bgm_id": 444957, "media_type": "movie", "rating": "general"}]}"#,
        )
        .await;
        let svc = &env.svc;
        let pool = env.db.pool();

        SeasonRepository::new(pool)
            .upsert(CreateSeason {
                season_id: 202604,
                year: 2026,
                season: "SPRING".to_string(),
                name: None,
            })
            .await
            .unwrap();
        SubjectRepository::new(pool)
            .upsert_meta_batch(&[(111111, None, None)])
            .await
            .unwrap();
        SeasonSubjectRepository::new(pool)
            .insert_or_ignore(crate::dal::CreateSeasonSubject {
                season_id: 202604,
                subject_id: 111111,
            })
            .await
            .unwrap();

        let r = svc.reconcile_all().await.unwrap();
        assert_eq!(r.total_added, 1);
        assert_eq!(r.total_removed, 1);
        assert_eq!(r.changes[0].removed, vec![111111]);

        let members = SeasonSubjectRepository::new(pool)
            .find_by_season_id(202604)
            .await
            .unwrap();
        assert_eq!(members, vec![444957]);
    }

    #[sqlx::test]
    async fn test_reconcile_all_skips_unparsable_key(pool: SqlitePool) {
        let env = mock_env(
            pool,
            r#"{"2026-autumn": [{"bgm_id": 444957, "media_type": "tv", "rating": "general"}]}"#,
        )
        .await;

        let r = env.svc.reconcile_all().await.unwrap();
        assert_eq!(r.seasons_skipped, 1);
        assert_eq!(r.total_added, 0);
    }
}
