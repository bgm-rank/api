use anyhow::Result;
use serde::Serialize;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use crate::core::fetch::{FetchSummary, MalFetchService, parse_season_id};
use crate::core::scheduler::{PublicTickStats, SchedulerHandle};
use crate::core::sync::SyncService;
use crate::dal::dto::{
    LinkedMalItem, MalBgmMatch, MalBgmMatchWithDetails, Season, SeasonMal, Subject, UpdateSeason,
    UpdateSubject,
};
use crate::dal::{
    Database, MalAnimeRepository, MalBgmMatchRepository, SeasonMalRepository, SeasonRepository,
    SubjectRepository,
};
use crate::services::bangumi::Subject as BangumiApiSubject;
use crate::services::deploy_hook::DeployHookClient;
use crate::services::mal::MalClient;

#[derive(Debug)]
pub struct SchedulerBusy;

impl std::fmt::Display for SchedulerBusy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Scheduler is already running")
    }
}

#[derive(Debug)]
pub enum AdminOpError {
    BadRequest(String),
    NotFound(String),
    Internal(anyhow::Error),
}

impl From<anyhow::Error> for AdminOpError {
    fn from(err: anyhow::Error) -> Self {
        Self::Internal(err)
    }
}

impl From<sqlx::Error> for AdminOpError {
    fn from(err: sqlx::Error) -> Self {
        Self::Internal(err.into())
    }
}

pub struct PatchMatchInput {
    pub status: Option<String>,
    pub bgm_id: Option<i32>,
    pub reason: Option<Option<String>>,
    pub note: Option<Option<String>>,
}

pub struct PatchMatchResult {
    pub match_row: MalBgmMatch,
    pub hydrated: bool,
}

pub struct MoveSeasonMalResult {
    pub source: SeasonMal,
    pub target: SeasonMal,
}

#[derive(Debug, Clone, Serialize)]
pub struct BgmPreviewSubject {
    pub id: i32,
    pub name: Option<String>,
    pub name_cn: Option<String>,
    pub air_date: Option<String>,
    pub platform: Option<String>,
    pub eps: Option<i32>,
    pub score: Option<f64>,
    pub rank: Option<i32>,
    pub nsfw: Option<bool>,
    pub tags: Vec<String>,
    pub summary: Option<String>,
    pub images_grid: Option<String>,
    pub linked_matches: Vec<LinkedMalItem>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AddManualMalResult {
    pub season_mal: SeasonMal,
    pub fetched_from_mal: bool,
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
    #[allow(dead_code)]
    pub fn new(
        db: Arc<Database>,
        scheduler_handle: SchedulerHandle,
        deploy_hook_url: Option<String>,
    ) -> Self {
        let sync_service = Arc::new(SyncService::new(Arc::clone(&db)));
        Self::with_sync_service(db, scheduler_handle, deploy_hook_url, sync_service)
    }

    pub fn with_sync_service(
        db: Arc<Database>,
        scheduler_handle: SchedulerHandle,
        deploy_hook_url: Option<String>,
        sync_service: Arc<SyncService>,
    ) -> Self {
        Self {
            sync_service,
            scheduler_handle,
            deploy_hook_client: DeployHookClient::new(deploy_hook_url),
            db,
        }
    }

    #[cfg(test)]
    pub fn sync_service(&self) -> &Arc<SyncService> {
        &self.sync_service
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
        let mut seen_bgm_ids = std::collections::HashSet::new();
        let mut has_hydrated = false;
        for season in seasons {
            match self
                .sync_service
                .resync_in_batch(season.season_id, &mut seen_bgm_ids, &mut has_hydrated, None)
                .await
            {
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

    pub async fn remove_mal_from_season(&self, season_id: i32, mal_id: i32) -> Result<bool> {
        SeasonMalRepository::new(self.db.pool())
            .set_removed(season_id, mal_id, true, None)
            .await
            .map(|opt| opt.is_some())
            .map_err(anyhow::Error::from)
    }

    pub async fn restore_mal_to_season(&self, season_id: i32, mal_id: i32) -> Result<bool> {
        SeasonMalRepository::new(self.db.pool())
            .set_removed(season_id, mal_id, false, None)
            .await
            .map(|opt| opt.is_some())
            .map_err(anyhow::Error::from)
    }

    pub async fn list_matches(
        &self,
        season_id: Option<i32>,
        status: Option<&str>,
    ) -> Result<Vec<MalBgmMatchWithDetails>, AdminOpError> {
        let pool = self.db.pool();
        let status = status.map(str::trim).filter(|s| !s.is_empty());
        if let Some(s) = status
            && !matches!(s, "pending" | "included" | "excluded")
        {
            return Err(AdminOpError::BadRequest(format!(
                "Invalid status: {}. Must be pending, included, or excluded",
                s
            )));
        }
        let rows = MalBgmMatchRepository::new(pool)
            .list_with_details(season_id, status)
            .await?;
        Ok(rows)
    }

    pub async fn patch_match(
        &self,
        mal_id: i32,
        input: PatchMatchInput,
    ) -> Result<PatchMatchResult, AdminOpError> {
        if let Some(id) = input.bgm_id
            && id <= 0
        {
            return Err(AdminOpError::BadRequest(
                "bgm_id must be greater than 0".to_string(),
            ));
        }

        let pool = self.db.pool();
        let match_repo = MalBgmMatchRepository::new(pool);

        let existing = match_repo.get(mal_id).await?.ok_or_else(|| {
            AdminOpError::NotFound(format!("Match for mal_id {} not found", mal_id))
        })?;

        let status = input.status.unwrap_or_else(|| existing.status.clone());
        if !matches!(status.as_str(), "pending" | "included" | "excluded") {
            return Err(AdminOpError::BadRequest(format!(
                "Invalid status: {}. Must be pending, included, or excluded",
                status
            )));
        }

        let (bgm_id, source, confidence) = match status.as_str() {
            "included" => {
                let id = input.bgm_id.or(existing.bgm_id).ok_or_else(|| {
                    AdminOpError::BadRequest(
                        "bgm_id is required when status is included".to_string(),
                    )
                })?;
                if id <= 0 {
                    return Err(AdminOpError::BadRequest(
                        "bgm_id must be greater than 0".to_string(),
                    ));
                }
                (Some(id), Some("human".to_string()), existing.confidence)
            }
            "excluded" => (input.bgm_id, Some("human".to_string()), existing.confidence),
            _ => (input.bgm_id, None, None),
        };

        let reason = input.reason.unwrap_or(existing.reason);
        let note = input.note.unwrap_or(existing.note);

        let mut tx = pool.begin().await?;

        if let Some(id) = bgm_id {
            sqlx::query("INSERT OR IGNORE INTO subjects (id) VALUES (?)")
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }

        let updated = sqlx::query_as::<_, MalBgmMatch>(
            r#"
            UPDATE mal_bgm_matches
            SET
                status = ?,
                source = ?,
                bgm_id = ?,
                confidence = ?,
                candidates = ?,
                reason = ?,
                note = ?,
                error = ?,
                updated_at = strftime('%Y-%m-%d %H:%M:%f', 'now')
            WHERE mal_id = ?
            RETURNING *
            "#,
        )
        .bind(&status)
        .bind(&source)
        .bind(bgm_id)
        .bind(confidence)
        .bind(&existing.candidates)
        .bind(&reason)
        .bind(&note)
        .bind(Option::<String>::None)
        .bind(mal_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| AdminOpError::NotFound(format!("Match for mal_id {} not found", mal_id)))?;

        tx.commit().await?;

        let mut hydrated = false;
        if status == "included"
            && let Some(id) = updated.bgm_id
        {
            let today = chrono::Utc::now().date_naive();
            match self.sync_service.hydrate_subject(id, today).await {
                Ok(()) => hydrated = true,
                Err(e) => {
                    tracing::warn!(
                        mal_id,
                        bgm_id = id,
                        error = %e,
                        "hydrate_subject failed after patch_match"
                    );
                }
            }
        }

        Ok(PatchMatchResult {
            match_row: updated,
            hydrated,
        })
    }

    pub async fn move_season_mal(
        &self,
        season_id: i32,
        mal_id: i32,
        target_season_id: i32,
        note: Option<String>,
    ) -> Result<MoveSeasonMalResult, AdminOpError> {
        if season_id == target_season_id {
            return Err(AdminOpError::BadRequest(
                "target_season_id must differ from source season_id".to_string(),
            ));
        }

        let pool = self.db.pool();
        if SeasonRepository::new(pool)
            .find_by_id(target_season_id)
            .await?
            .is_none()
        {
            return Err(AdminOpError::NotFound(format!(
                "Target season {} not found",
                target_season_id
            )));
        }

        let mut tx = pool.begin().await?;

        let source = sqlx::query_as::<_, SeasonMal>(
            r#"
            UPDATE season_mal
            SET is_removed = 1,
                note = COALESCE(?, note)
            WHERE season_id = ? AND mal_id = ?
            RETURNING *
            "#,
        )
        .bind(note.as_deref())
        .bind(season_id)
        .bind(mal_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| {
            AdminOpError::NotFound(format!(
                "Season {} mal_id {} association not found",
                season_id, mal_id
            ))
        })?;

        let target = sqlx::query_as::<_, SeasonMal>(
            r#"
            INSERT INTO season_mal (season_id, mal_id, origin, is_removed, note)
            VALUES (?, ?, 'manual', 0, ?)
            ON CONFLICT (season_id, mal_id) DO UPDATE SET
                origin = 'manual',
                is_removed = 0,
                note = COALESCE(EXCLUDED.note, season_mal.note)
            RETURNING *
            "#,
        )
        .bind(target_season_id)
        .bind(mal_id)
        .bind(note.as_deref())
        .fetch_one(&mut *tx)
        .await?;

        tx.commit().await?;

        Ok(MoveSeasonMalResult { source, target })
    }

    pub async fn export_season_mal_jsonl(&self) -> Result<String> {
        let rows = SeasonMalRepository::new(self.db.pool())
            .list_for_export()
            .await?;
        to_ndjson(&rows)
    }

    pub async fn export_matches_jsonl(&self) -> Result<String> {
        let rows = MalBgmMatchRepository::new(self.db.pool())
            .list_for_export()
            .await?;
        to_ndjson(&rows)
    }

    pub async fn search_bgm_subjects(
        &self,
        keyword: &str,
    ) -> Result<Vec<BgmPreviewSubject>, AdminOpError> {
        let q = keyword.trim();
        if q.is_empty() {
            return Err(AdminOpError::BadRequest(
                "Query parameter 'q' cannot be empty".to_string(),
            ));
        }

        let subjects = self
            .sync_service
            .bangumi_client()
            .search_anime(q, None)
            .await?;
        let ids: Vec<i32> = subjects.iter().map(|s| s.id).collect();
        let linked = MalBgmMatchRepository::new(self.db.pool())
            .list_linked_by_bgm_ids(&ids)
            .await?;

        Ok(subjects
            .into_iter()
            .map(|s| {
                let id = s.id;
                let item_links = linked.iter().filter(|l| l.bgm_id == id).cloned().collect();
                bgm_api_to_preview(s, item_links)
            })
            .collect())
    }

    pub async fn preview_bgm_subject(
        &self,
        bgm_id: i32,
    ) -> Result<BgmPreviewSubject, AdminOpError> {
        if bgm_id <= 0 {
            return Err(AdminOpError::BadRequest(
                "bgm_id must be greater than 0".to_string(),
            ));
        }

        let pool = self.db.pool();
        let linked = MalBgmMatchRepository::new(pool)
            .list_linked_by_bgm_ids(&[bgm_id])
            .await?;

        match self.sync_service.bangumi_client().get_subject(bgm_id).await {
            Ok(s) => Ok(bgm_api_to_preview(s, linked)),
            Err(api_err) => {
                if let Some(local) = SubjectRepository::new(pool).find_by_id(bgm_id).await?
                    && local.name.is_some()
                {
                    let tags: Vec<String> = local
                        .tags
                        .as_deref()
                        .and_then(|t| serde_json::from_str(t).ok())
                        .unwrap_or_default();
                    return Ok(BgmPreviewSubject {
                        id: local.id,
                        name: local.name,
                        name_cn: local.name_cn,
                        air_date: local.air_date,
                        platform: local.platform,
                        eps: None,
                        score: local.score,
                        rank: local.rank,
                        nsfw: local.nsfw,
                        tags,
                        summary: local.summary,
                        images_grid: local.images_grid,
                        linked_matches: linked,
                    });
                }
                let msg = api_err.to_string();
                if msg.contains("404") {
                    Err(AdminOpError::NotFound(format!(
                        "Bangumi subject {} not found",
                        bgm_id
                    )))
                } else {
                    Err(AdminOpError::Internal(api_err))
                }
            }
        }
    }

    pub async fn refresh_subject(&self, subject_id: i32) -> Result<Subject, AdminOpError> {
        if subject_id <= 0 {
            return Err(AdminOpError::BadRequest(
                "subject_id must be greater than 0".to_string(),
            ));
        }
        let today = chrono::Utc::now().date_naive();
        self.sync_service.hydrate_subject(subject_id, today).await?;
        SubjectRepository::new(self.db.pool())
            .find_by_id(subject_id)
            .await?
            .ok_or_else(|| AdminOpError::NotFound(format!("Subject {} not found", subject_id)))
    }

    pub async fn add_manual_mal_to_season(
        &self,
        season_id: i32,
        mal_id: i32,
        note: Option<String>,
        mal_client: Option<&MalClient>,
    ) -> Result<AddManualMalResult, AdminOpError> {
        if mal_id <= 0 {
            return Err(AdminOpError::BadRequest(
                "mal_id must be greater than 0".to_string(),
            ));
        }

        let pool = self.db.pool();
        if SeasonRepository::new(pool)
            .find_by_id(season_id)
            .await?
            .is_none()
        {
            return Err(AdminOpError::NotFound(format!(
                "Season {} not found",
                season_id
            )));
        }

        let anime_repo = MalAnimeRepository::new(pool);
        let mut fetched_from_mal = false;
        if anime_repo.get(mal_id).await?.is_none() {
            let client = mal_client.ok_or_else(|| {
                AdminOpError::BadRequest(
                    "mal_id is not in local database and MAL_CLIENT_ID is not configured"
                        .to_string(),
                )
            })?;
            let node = client
                .get_anime(mal_id)
                .await?
                .ok_or_else(|| AdminOpError::NotFound(format!("MAL anime {} not found", mal_id)))?;
            anime_repo
                .upsert(crate::core::fetch::to_upsert(node))
                .await?;
            fetched_from_mal = true;
        }

        let season_mal = SeasonMalRepository::new(pool)
            .insert_manual(season_id, mal_id, note)
            .await?;
        MalBgmMatchRepository::new(pool)
            .insert_pending_ignore(mal_id)
            .await?;

        Ok(AddManualMalResult {
            season_mal,
            fetched_from_mal,
        })
    }

    pub async fn fetch_season_mal(
        &self,
        season_id: i32,
        mal_client: Option<MalClient>,
    ) -> Result<FetchSummary, AdminOpError> {
        parse_season_id(season_id).map_err(|e| AdminOpError::BadRequest(e.to_string()))?;
        let client = mal_client.ok_or_else(|| {
            AdminOpError::BadRequest("MAL_CLIENT_ID is not configured on server".to_string())
        })?;
        let svc = MalFetchService::new(Arc::clone(&self.db), client);
        Ok(svc.fetch_season(season_id).await?)
    }
}

fn bgm_api_to_preview(
    s: BangumiApiSubject,
    linked_matches: Vec<LinkedMalItem>,
) -> BgmPreviewSubject {
    let eps = s
        .eps
        .filter(|&e| e > 0)
        .or(s.total_episodes.filter(|&e| e > 0));
    let score = s.rating.as_ref().and_then(|r| r.score);
    let rank = s.rating.as_ref().and_then(|r| r.rank);
    let images_grid = s
        .images
        .and_then(|img| img.grid.or(img.small).or(img.medium));
    let tags = s
        .tags
        .unwrap_or_default()
        .into_iter()
        .filter_map(|t| t.name)
        .take(10)
        .collect();
    BgmPreviewSubject {
        id: s.id,
        name: s.name,
        name_cn: s.name_cn,
        air_date: s.date,
        platform: s.platform,
        eps,
        score,
        rank,
        nsfw: s.nsfw,
        tags,
        summary: s.summary,
        images_grid,
        linked_matches,
    }
}

fn to_ndjson<T: Serialize>(rows: &[T]) -> Result<String> {
    let mut out = String::new();
    for row in rows {
        let line = serde_json::to_string(row)?;
        out.push_str(&line);
        out.push('\n');
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dal::dto::{CreateSeason, CreateSubject, UpdateMalBgmMatch, UpsertMalAnime};
    use crate::dal::repositories::{MalAnimeRepository, SubjectRepository};
    use sqlx::SqlitePool;

    #[sqlx::test]
    async fn test_admin_service_exports_empty_and_populated_ndjson(pool: SqlitePool) -> Result<()> {
        let db = Arc::new(Database::from_pool(pool));
        let svc = AdminService::new(Arc::clone(&db), SchedulerHandle::new(), None);

        assert_eq!(svc.export_season_mal_jsonl().await?, "");
        assert_eq!(svc.export_matches_jsonl().await?, "");

        SeasonRepository::new(db.pool())
            .upsert(CreateSeason {
                season_id: 202601,
                year: 2026,
                season: "WINTER".to_string(),
                name: None,
            })
            .await?;
        MalAnimeRepository::new(db.pool())
            .upsert(UpsertMalAnime {
                mal_id: 42,
                title: "Test".to_string(),
                media_type: "tv".to_string(),
                ..Default::default()
            })
            .await?;
        SubjectRepository::new(db.pool())
            .create(CreateSubject {
                id: 900,
                ..Default::default()
            })
            .await?;
        SeasonMalRepository::new(db.pool())
            .insert_manual(202601, 42, Some("manual".to_string()))
            .await?;
        MalBgmMatchRepository::new(db.pool())
            .upsert(
                42,
                UpdateMalBgmMatch {
                    status: "included".to_string(),
                    source: Some("human".to_string()),
                    bgm_id: Some(900),
                    confidence: Some(1.0),
                    note: Some("verified".to_string()),
                    ..Default::default()
                },
            )
            .await?;

        let sm_ndjson = svc.export_season_mal_jsonl().await?;
        assert!(sm_ndjson.ends_with('\n'));
        assert_eq!(sm_ndjson.lines().count(), 1);
        let sm_val: serde_json::Value =
            serde_json::from_str(sm_ndjson.trim_end_matches('\n')).unwrap();
        assert_eq!(
            sm_val,
            serde_json::json!({
                "season_id": 202601,
                "mal_id": 42,
                "origin": "manual",
                "is_removed": false,
                "note": "manual"
            })
        );

        let m_ndjson = svc.export_matches_jsonl().await?;
        assert!(m_ndjson.ends_with('\n'));
        assert_eq!(m_ndjson.lines().count(), 1);
        let m_val: serde_json::Value =
            serde_json::from_str(m_ndjson.trim_end_matches('\n')).unwrap();
        assert_eq!(
            m_val,
            serde_json::json!({
                "mal_id": 42,
                "status": "included",
                "source": "human",
                "bgm_id": 900,
                "confidence": 1.0,
                "reason": null,
                "note": "verified"
            })
        );

        Ok(())
    }

    #[sqlx::test]
    async fn test_sync_all_seasons_dedupes_shared_bgm_id_across_seasons(
        pool: SqlitePool,
    ) -> Result<()> {
        let mut bgm_server = mockito::Server::new_async().await;
        let ep_500 = bgm_server
            .mock(
                "GET",
                "/v0/episodes?subject_id=500&type=0&limit=100&offset=0",
            )
            .with_header("content-type", "application/json")
            .with_body(r#"{"total":0,"limit":100,"offset":0,"data":[]}"#)
            .expect(1)
            .create_async()
            .await;
        let subj_500 = bgm_server
            .mock("GET", "/v0/subjects/500")
            .with_header("content-type", "application/json")
            .with_body(r#"{"id":500,"type":2,"name":"Shared Show","name_cn":"共享番剧"}"#)
            .expect(1)
            .create_async()
            .await;
        let ep_501 = bgm_server
            .mock(
                "GET",
                "/v0/episodes?subject_id=501&type=0&limit=100&offset=0",
            )
            .with_header("content-type", "application/json")
            .with_body(r#"{"total":0,"limit":100,"offset":0,"data":[]}"#)
            .expect(1)
            .create_async()
            .await;
        let subj_501 = bgm_server
            .mock("GET", "/v0/subjects/501")
            .with_header("content-type", "application/json")
            .with_body(r#"{"id":501,"type":2,"name":"Other Show","name_cn":"其他番剧"}"#)
            .expect(1)
            .create_async()
            .await;

        let db = Arc::new(Database::from_pool(pool.clone()));
        let sync_service = Arc::new(SyncService::with_clients(
            Arc::clone(&db),
            crate::services::bangumi::BangumiClient::with_base_url(&bgm_server.url()),
        ));
        let admin = AdminService::with_sync_service(
            Arc::clone(&db),
            SchedulerHandle::new(),
            None,
            sync_service,
        );

        for (season_id, year, season) in [(202601, 2026, "WINTER"), (202604, 2026, "SPRING")] {
            SeasonRepository::new(&pool)
                .upsert(CreateSeason {
                    season_id,
                    year,
                    season: season.to_string(),
                    name: None,
                })
                .await?;
        }
        for id in [500, 501] {
            SubjectRepository::new(&pool)
                .create(CreateSubject {
                    id,
                    ..Default::default()
                })
                .await?;
        }

        // Two mal_ids across two seasons (and one duplicate within 202601) all bound to bgm_id=500,
        // plus one mal_id bound to bgm_id=501.
        for (season_id, mal_id, bgm_id) in [
            (202601, 1001, 500),
            (202601, 1002, 500),
            (202604, 1003, 500),
            (202604, 1004, 501),
        ] {
            MalAnimeRepository::new(&pool)
                .upsert(UpsertMalAnime {
                    mal_id,
                    title: format!("MAL {}", mal_id),
                    media_type: "tv".to_string(),
                    ..Default::default()
                })
                .await?;
            SeasonMalRepository::new(&pool)
                .insert_ignore(season_id, mal_id)
                .await?;
            MalBgmMatchRepository::new(&pool)
                .upsert(
                    mal_id,
                    UpdateMalBgmMatch {
                        status: "included".to_string(),
                        source: Some("human".to_string()),
                        bgm_id: Some(bgm_id),
                        ..Default::default()
                    },
                )
                .await?;
        }

        admin.sync_all_seasons().await;

        ep_500.assert_async().await;
        subj_500.assert_async().await;
        ep_501.assert_async().await;
        subj_501.assert_async().await;

        let s500 = SubjectRepository::new(&pool)
            .find_by_id(500)
            .await?
            .unwrap();
        assert_eq!(s500.name_cn.as_deref(), Some("共享番剧"));
        assert!(s500.last_updated_at.is_some());

        let s501 = SubjectRepository::new(&pool)
            .find_by_id(501)
            .await?
            .unwrap();
        assert_eq!(s501.name_cn.as_deref(), Some("其他番剧"));
        assert!(s501.last_updated_at.is_some());

        Ok(())
    }
}
