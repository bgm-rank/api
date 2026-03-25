use anyhow::Result;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use crate::core::scheduler::{PublicTickStats, SchedulerHandle};
use crate::core::sync::SyncService;
use crate::dal::dto::{Season, Subject, UpdateSeason, UpdateSubject};
use crate::dal::{Database, SeasonRepository, SubjectRepository};
use crate::services::deploy_hook::DeployHookClient;

#[derive(Debug)]
pub struct SchedulerBusy;

impl std::fmt::Display for SchedulerBusy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Scheduler is already running")
    }
}

pub struct SchedulerStatus {
    pub is_running: bool,
    pub last_run_at: Option<chrono::DateTime<chrono::Utc>>,
    pub last_stats: Option<PublicTickStats>,
}

pub struct AdminService {
    db: Arc<Database>,
    scheduler_handle: SchedulerHandle,
    deploy_hook_client: DeployHookClient,
    sync_service: Arc<SyncService>,
}

impl AdminService {
    pub fn new(
        db: Arc<Database>,
        scheduler_handle: SchedulerHandle,
        deploy_hook_url: Option<String>,
    ) -> Self {
        Self {
            sync_service: Arc::new(SyncService::new(Arc::clone(&db))),
            scheduler_handle,
            deploy_hook_client: DeployHookClient::new(deploy_hook_url),
            db,
        }
    }

    pub async fn trigger_deploy(&self) -> Result<()> {
        self.deploy_hook_client.trigger().await
    }

    pub async fn sync_all_seasons(&self) {
        let pool = self.db.pool();
        let seasons = match SeasonRepository::new(pool).find_all().await {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(error = %e, "sync_all_seasons: failed to find all seasons");
                return;
            }
        };
        for season in seasons {
            match self.sync_service.resync(season.season_id).await {
                Ok(result) => tracing::info!(
                    season_id = season.season_id,
                    added = result.added,
                    updated = result.updated,
                    "sync_all_seasons: season done"
                ),
                Err(e) => tracing::warn!(
                    season_id = season.season_id,
                    error = %e,
                    "sync_all_seasons: season failed"
                ),
            }
        }
    }

    pub fn trigger_scheduler_tick(&self) -> Result<(), SchedulerBusy> {
        if self.scheduler_handle.is_running.load(Ordering::SeqCst) {
            return Err(SchedulerBusy);
        }
        self.scheduler_handle.manual_trigger.notify_one();
        Ok(())
    }

    pub fn get_scheduler_status(&self) -> SchedulerStatus {
        let is_running = self.scheduler_handle.is_running.load(Ordering::SeqCst);
        let last_stats = self.scheduler_handle.last_stats.lock().unwrap().clone();
        let last_run_at = last_stats.as_ref().map(|s| s.run_at);
        SchedulerStatus {
            is_running,
            last_run_at,
            last_stats,
        }
    }

    pub async fn get_subject(&self, id: i32) -> Result<Option<Subject>> {
        SubjectRepository::new(self.db.pool())
            .find_by_id(id)
            .await
            .map_err(anyhow::Error::from)
    }

    pub async fn get_season(&self, id: i32) -> Result<Option<Season>> {
        SeasonRepository::new(self.db.pool())
            .find_by_id(id)
            .await
            .map_err(anyhow::Error::from)
    }

    pub async fn update_subject(&self, id: i32, req: UpdateSubject) -> Result<Option<Subject>> {
        match SubjectRepository::new(self.db.pool()).update(id, req).await {
            Ok(s) => Ok(Some(s)),
            Err(sqlx::Error::RowNotFound) => Ok(None),
            Err(e) => Err(anyhow::Error::from(e)),
        }
    }

    pub async fn update_season(&self, id: i32, req: UpdateSeason) -> Result<Option<Season>> {
        match SeasonRepository::new(self.db.pool()).update(id, req).await {
            Ok(s) => Ok(Some(s)),
            Err(sqlx::Error::RowNotFound) => Ok(None),
            Err(e) => Err(anyhow::Error::from(e)),
        }
    }

    pub async fn delete_subject(&self, id: i32) -> Result<bool> {
        SubjectRepository::new(self.db.pool())
            .delete(id)
            .await
            .map_err(anyhow::Error::from)
    }

    pub async fn remove_subject_from_season(
        &self,
        season_id: i32,
        subject_id: i32,
    ) -> Result<bool> {
        crate::dal::SeasonSubjectRepository::new(self.db.pool())
            .delete_by_season_and_subject(season_id, subject_id)
            .await
            .map_err(anyhow::Error::from)
    }
}
