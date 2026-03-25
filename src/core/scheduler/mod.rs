use anyhow::Result;
use chrono::{DateTime, Datelike, FixedOffset, NaiveDate, NaiveTime, TimeZone, Utc};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::dal::Database;
use crate::services::bangumi::BangumiClient;
use crate::services::deploy_hook::DeployHookClient;

// ── Scheduler timing ──────────────────────────────────────────────────────────

const SCHEDULE_HOURS: &[u32] = &[0, 4, 8, 12, 16, 20];

/// 计算下一个 UTC+8 整点（0/4/8/12/16/20 时），严格大于 `now`
pub fn next_scheduled_instant(now: DateTime<Utc>) -> DateTime<Utc> {
    let cst = FixedOffset::east_opt(8 * 3600).unwrap();
    let now_cst = now.with_timezone(&cst);
    let today = now_cst.date_naive();

    // 找当日下一个严格大于 now 的整点
    for &h in SCHEDULE_HOURS {
        let candidate_time = NaiveTime::from_hms_opt(h, 0, 0).unwrap();
        let candidate = cst
            .from_local_datetime(&today.and_time(candidate_time))
            .unwrap();
        if candidate > now {
            return candidate.to_utc();
        }
    }

    // 全部整点已过，返回次日 00:00 CST
    let tomorrow = today.succ_opt().unwrap();
    let midnight = NaiveTime::from_hms_opt(0, 0, 0).unwrap();
    cst.from_local_datetime(&tomorrow.and_time(midnight))
        .unwrap()
        .to_utc()
}

// ── Pure functions ─────────────────────────────────────────────────────────────

pub fn current_quarter(date: NaiveDate) -> (i32, u32) {
    let month = date.month();
    let quarter_month = if month <= 3 {
        1
    } else if month <= 6 {
        4
    } else if month <= 9 {
        7
    } else {
        10
    };
    (date.year(), quarter_month)
}

pub fn season_id_to_quarter_index(season_id: i32) -> i32 {
    let year = season_id / 100;
    let month = season_id % 100;
    let quarter = match month {
        1 => 0,
        4 => 1,
        7 => 2,
        10 => 3,
        _ => 0,
    };
    year * 4 + quarter
}

pub fn quarters_distance(from_season_id: i32, to_season_id: i32) -> i32 {
    season_id_to_quarter_index(to_season_id) - season_id_to_quarter_index(from_season_id)
}

pub fn is_due(age: i32, last_updated_at: Option<&DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    if age == 0 {
        return true;
    }
    match last_updated_at {
        None => true,
        Some(last) => {
            let elapsed_days = (now - *last).num_days();
            elapsed_days >= age as i64
        }
    }
}

// ── Concurrency guard ──────────────────────────────────────────────────────────

pub(super) fn try_acquire_run(flag: &AtomicBool) -> bool {
    flag.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
}

// ── TickStats ──────────────────────────────────────────────────────────────────

#[derive(Default)]
pub(super) struct TickStats {
    pub due_count: usize,
    pub success_count: usize,
    pub fail_count: usize,
    pub elapsed_ms: u64,
}

// ── Scheduler service ─────────────────────────────────────────────────────────

pub struct SchedulerService {
    db: Arc<Database>,
    bangumi_client: BangumiClient,
    deploy_hook_client: DeployHookClient,
    is_running: Arc<AtomicBool>,
}

impl SchedulerService {
    #[allow(dead_code)]
    pub fn new(db: Arc<Database>) -> Self {
        Self::new_with_deploy_hook(db, None)
    }

    pub fn new_with_deploy_hook(db: Arc<Database>, deploy_hook_url: Option<String>) -> Self {
        Self {
            db,
            bangumi_client: BangumiClient::new(),
            deploy_hook_client: DeployHookClient::new(deploy_hook_url),
            is_running: Arc::new(AtomicBool::new(false)),
        }
    }

    pub async fn run(self) -> Result<()> {
        use crate::dal::SubjectRepository;
        use tokio::time::{Duration, Instant, sleep, sleep_until};

        tracing::info!("调度器已启动");

        let mut tick_count: u64 = 0;
        loop {
            let now = Utc::now();
            let next = next_scheduled_instant(now);
            let cst = FixedOffset::east_opt(8 * 3600).unwrap();
            tracing::info!("下次触发时间: {}", next.with_timezone(&cst));
            let wait = (next - now).to_std().unwrap_or(Duration::from_secs(0));
            sleep_until(Instant::now() + wait).await;

            tick_count += 1;

            if !try_acquire_run(&self.is_running) {
                tracing::warn!(tick = tick_count, "上一轮调度尚未完成，跳过本次 tick");
                continue;
            }

            let today = chrono::Utc::now().date_naive();
            let (year, month) = current_quarter(today);
            let current_season_id = year * 100 + month as i32;

            let pool = self.db.pool();
            let subject_repo = SubjectRepository::new(pool);

            let due_subjects = match subject_repo.find_due_for_update(current_season_id).await {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!(tick = tick_count, event_type = "error", error = %e, "find_due_for_update 失败");
                    continue;
                }
            };

            tracing::info!(
                tick = tick_count,
                due = due_subjects.len(),
                event_type = "start",
                "调度器触发"
            );

            let mut stats = TickStats {
                due_count: due_subjects.len(),
                ..Default::default()
            };

            let today = chrono::Utc::now().date_naive();
            let tick_start = std::time::Instant::now();

            for (subject_id, _, _) in due_subjects {
                let avg_comment = match self
                    .bangumi_client
                    .get_episodes(subject_id, 0, 100, 0)
                    .await
                {
                    Ok(paged) => crate::core::sync::calculate_average_comment(&paged.data, today),
                    Err(e) => {
                        tracing::warn!(subject_id, error = %e, "调度器拉取 episodes 失败，降级为 None");
                        None
                    }
                };

                match self.bangumi_client.get_subject(subject_id).await {
                    Ok(bgm_subject) => {
                        let create = crate::core::sync::to_create_subject(bgm_subject, avg_comment);
                        if let Err(e) = subject_repo.upsert(create).await {
                            tracing::error!(subject_id, error = %e, "调度器 upsert subject 失败");
                            stats.fail_count += 1;
                            continue;
                        }
                        if let Err(e) = subject_repo.update_last_updated_at(subject_id).await {
                            tracing::error!(subject_id, error = %e, "update_last_updated_at 失败");
                        }
                        stats.success_count += 1;
                    }
                    Err(e) => {
                        tracing::error!(subject_id, error = %e, "调度器拉取 subject 失败");
                        stats.fail_count += 1;
                    }
                }

                sleep(Duration::from_millis(500)).await;
            }

            // 更新当前季度的 updated_at 时间戳
            if let Err(e) = crate::dal::SeasonRepository::new(pool)
                .touch_updated_at(current_season_id)
                .await
            {
                tracing::warn!(current_season_id, error = %e, "调度器 touch_updated_at 失败");
            }

            if stats.success_count > 0
                && let Err(e) = self.deploy_hook_client.trigger().await
            {
                tracing::error!(error = %e, "Deploy Hook 触发失败");
            }

            stats.elapsed_ms = tick_start.elapsed().as_millis() as u64;
            tracing::info!(
                tick = tick_count,
                due = stats.due_count,
                success = stats.success_count,
                fail = stats.fail_count,
                elapsed_ms = stats.elapsed_ms,
                event_type = "complete",
                "scheduler tick"
            );
            self.is_running.store(false, Ordering::SeqCst);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{FixedOffset, TimeZone, Utc};

    /// 构造 CST（UTC+8）2026-03-10 当日 h:m:s 对应的 UTC 时间
    fn cst_at(h: u32, m: u32, s: u32) -> DateTime<Utc> {
        let offset = FixedOffset::east_opt(8 * 3600).unwrap();
        offset
            .with_ymd_and_hms(2026, 3, 10, h, m, s)
            .unwrap()
            .to_utc()
    }

    // T016: subject 级别错误隔离
    // run() 中对每个 subject 的拉取/upsert 失败均使用 continue，不终止外层循环。
    // 这确保单个 subject 异常不影响其他 subjects 的更新。

    // T014 🔴 → T015 🟢
    #[test]
    fn test_is_running_guard_skips_when_busy() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let flag = AtomicBool::new(false);
        // 首次获取应成功
        assert!(try_acquire_run(&flag));
        // flag 已为 true，再次获取应失败
        assert!(!try_acquire_run(&flag));
        // 释放后可再次获取
        flag.store(false, Ordering::SeqCst);
        assert!(try_acquire_run(&flag));
    }

    // T010 🔴 → T011 🟢
    #[test]
    fn test_scheduler_new_accepts_deploy_hook_url() {
        // 编译期验证 new_with_deploy_hook 函数签名存在且类型匹配
        let _f: fn(Arc<Database>, Option<String>) -> SchedulerService =
            SchedulerService::new_with_deploy_hook;
    }

    // T004 🔴 → T005 🟢
    #[test]
    fn test_tick_stats_defaults() {
        let stats = TickStats::default();
        assert_eq!(stats.due_count, 0);
        assert_eq!(stats.success_count, 0);
        assert_eq!(stats.fail_count, 0);
    }

    // T030 🔴 → T031 🟢

    #[test]
    fn test_current_quarter_mar31_gives_winter() {
        let date = NaiveDate::from_ymd_opt(2026, 3, 31).unwrap();
        assert_eq!(current_quarter(date), (2026, 1));
    }

    #[test]
    fn test_current_quarter_apr1_gives_spring() {
        let date = NaiveDate::from_ymd_opt(2026, 4, 1).unwrap();
        assert_eq!(current_quarter(date), (2026, 4));
    }

    #[test]
    fn test_current_quarter_dec31_gives_autumn() {
        let date = NaiveDate::from_ymd_opt(2026, 12, 31).unwrap();
        assert_eq!(current_quarter(date), (2026, 10));
    }

    #[test]
    fn test_quarters_distance_same() {
        assert_eq!(quarters_distance(202601, 202601), 0);
    }

    #[test]
    fn test_quarters_distance_one_quarter_forward() {
        assert_eq!(quarters_distance(202601, 202604), 1);
    }

    #[test]
    fn test_quarters_distance_cross_year() {
        assert_eq!(quarters_distance(202510, 202601), 1);
    }

    #[test]
    fn test_quarters_distance_two_years() {
        assert_eq!(quarters_distance(202401, 202601), 8);
    }

    #[test]
    fn test_is_due_age_zero_always_true() {
        let now = Utc::now();
        assert!(is_due(0, None, now));
        let past = Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();
        assert!(is_due(0, Some(&past), now));
        assert!(is_due(0, Some(&now), now));
    }

    #[test]
    fn test_is_due_none_always_true() {
        let now = Utc::now();
        assert!(is_due(1, None, now));
        assert!(is_due(5, None, now));
    }

    #[test]
    fn test_is_due_stale() {
        let now = Utc::now();
        let stale = now - chrono::Duration::days(10);
        assert!(is_due(5, Some(&stale), now));
    }

    #[test]
    fn test_is_due_fresh() {
        let now = Utc::now();
        let fresh = now - chrono::Duration::days(1);
        assert!(!is_due(5, Some(&fresh), now));
    }

    // T002-T007: next_scheduled_instant 测试（Red 阶段）

    fn cst(h: u32, m: u32, s: u32) -> DateTime<Utc> {
        let cst = FixedOffset::east_opt(8 * 3600).unwrap();
        cst.with_ymd_and_hms(2026, 3, 10, h, m, s).unwrap().to_utc()
    }

    fn cst_next_day(h: u32, m: u32, s: u32) -> DateTime<Utc> {
        let cst = FixedOffset::east_opt(8 * 3600).unwrap();
        cst.with_ymd_and_hms(2026, 3, 11, h, m, s).unwrap().to_utc()
    }

    #[test]
    fn test_next_scheduled_instant_after_midnight() {
        // 00:01 CST → 04:00 CST 同日
        let now = cst(0, 1, 0);
        let expected = cst(4, 0, 0);
        assert_eq!(next_scheduled_instant(now), expected);
    }

    #[test]
    fn test_next_scheduled_instant_at_noon() {
        // 12:01 CST → 16:00 CST 同日
        let now = cst(12, 1, 0);
        let expected = cst(16, 0, 0);
        assert_eq!(next_scheduled_instant(now), expected);
    }

    #[test]
    fn test_next_scheduled_instant_before_midnight() {
        // 23:50 CST → 次日 00:00 CST
        let now = cst(23, 50, 0);
        let expected = cst_next_day(0, 0, 0);
        assert_eq!(next_scheduled_instant(now), expected);
    }

    #[test]
    fn test_next_scheduled_instant_at_exact_hour() {
        // 08:00:00 CST 恰好整点 → 12:00 CST（不含当前整点）
        let now = cst(8, 0, 0);
        let expected = cst(12, 0, 0);
        assert_eq!(next_scheduled_instant(now), expected);
    }

    #[test]
    fn test_next_scheduled_instant_at_last_slot() {
        // 20:00:00 CST 最后整点 → 次日 00:00 CST
        let now = cst(20, 0, 0);
        let expected = cst_next_day(0, 0, 0);
        assert_eq!(next_scheduled_instant(now), expected);
    }

    #[test]
    fn test_next_scheduled_instant_just_before_slot() {
        // 03:59:59 CST → 04:00 CST 同日
        let now = cst(3, 59, 59);
        let expected = cst(4, 0, 0);
        assert_eq!(next_scheduled_instant(now), expected);
    }
}
