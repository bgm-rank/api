use anyhow::Result;
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use std::sync::Arc;

use crate::dal::Database;
use crate::services::bangumi::BangumiClient;

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

// ── Scheduler service ─────────────────────────────────────────────────────────

pub struct SchedulerService {
    db: Arc<Database>,
    bangumi_client: BangumiClient,
}

impl SchedulerService {
    pub fn new(db: Arc<Database>) -> Self {
        Self {
            db,
            bangumi_client: BangumiClient::new(),
        }
    }

    pub async fn run(self) -> Result<()> {
        use crate::dal::SubjectRepository;
        use tokio::time::{Duration, interval, sleep};

        let mut tick = interval(Duration::from_secs(4 * 3600));

        loop {
            tick.tick().await;

            let today = chrono::Utc::now().date_naive();
            let (year, month) = current_quarter(today);
            let current_season_id = year * 100 + month as i32;

            let pool = self.db.pool();
            let subject_repo = SubjectRepository::new(pool);

            let due_subjects = match subject_repo.find_due_for_update(current_season_id).await {
                Ok(s) => s,
                Err(e) => {
                    log::error!("find_due_for_update 失败: {:#}", e);
                    continue;
                }
            };

            for (subject_id, _, _) in due_subjects {
                match self.bangumi_client.get_subject(subject_id).await {
                    Ok(bgm_subject) => {
                        use crate::dal::CreateSubject;
                        let create = CreateSubject {
                            id: bgm_subject.id,
                            name: bgm_subject.name,
                            name_cn: bgm_subject.name_cn,
                            images_grid: bgm_subject.images.as_ref().and_then(|i| i.grid.clone()),
                            images_large: bgm_subject.images.and_then(|i| i.large),
                            rank: bgm_subject.rating.as_ref().and_then(|r| r.rank),
                            score: bgm_subject.rating.and_then(|r| r.score),
                            collection_total: bgm_subject.collection.map(|c| c.collect),
                            meta_tags: bgm_subject.meta_tags.unwrap_or_default(),
                            ..Default::default()
                        };
                        if let Err(e) = subject_repo.upsert(create).await {
                            log::error!("调度器 upsert subject {} 失败: {:#}", subject_id, e);
                            continue;
                        }
                        if let Err(e) = subject_repo.update_last_updated_at(subject_id).await {
                            log::error!("update_last_updated_at {} 失败: {:#}", subject_id, e);
                        }
                    }
                    Err(e) => {
                        log::error!("调度器拉取 subject {} 失败: {:#}", subject_id, e);
                    }
                }

                sleep(Duration::from_millis(500)).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

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
}
