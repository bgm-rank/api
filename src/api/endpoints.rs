use axum::{
    Json,
    extract::{Path, Query, State},
    http::{StatusCode, header},
    middleware,
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
};
use serde::Serialize;
use std::convert::Infallible;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_stream::StreamExt as _;
use tokio_stream::wrappers::ReceiverStream;

use crate::core::AdminService;
use crate::core::SyncService;
use crate::core::admin::{AdminOpError, PatchMatchInput};
use crate::core::query::QueryService;
use crate::core::scheduler::SchedulerHandle;
use crate::core::sync::SyncProgressEvent;
use crate::dal::Database;

use super::middleware::require_admin_token;
use super::schemas::{
    AcceptedResponse, CreateSeasonRequest, DeleteOrphansResponse, DeleteSeasonResponse,
    DeletedResponse, EditSeasonRequest, EditSubjectRequest, ErrorResponse, ListMatchesQuery,
    MoveSeasonMalRequest, MoveSeasonMalResponse, OrphanSubjectItem, PatchMatchRequest,
    PatchMatchResponse, RemovedResponse, RestoredResponse, SchedulerStatusResponse,
};

#[derive(Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub db: String,
}

pub async fn health_check(State(state): State<AppState>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".to_string(),
        db: match state.db.ping().await {
            Ok(true) => "ok".to_string(),
            Ok(false) => "error".to_string(),
            Err(e) => e.to_string(),
        },
    })
}

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Database>,
    pub sync_service: Arc<SyncService>,
    pub query_service: Arc<QueryService>,
    pub admin_service: Arc<AdminService>,
    pub scheduler_handle: Arc<SchedulerHandle>,
}

impl AppState {
    pub fn new(db: Arc<Database>) -> Self {
        let handle = SchedulerHandle::new();
        let deploy_hook_url = std::env::var("DEPLOY_HOOK_URL").ok();
        let sync_service = Arc::new(SyncService::new(Arc::clone(&db)));
        let admin_service = AdminService::with_sync_service(
            Arc::clone(&db),
            handle.clone(),
            deploy_hook_url,
            Arc::clone(&sync_service),
        );
        Self {
            query_service: Arc::new(QueryService::new(Arc::clone(&db))),
            sync_service,
            admin_service: Arc::new(admin_service),
            scheduler_handle: Arc::new(handle),
            db,
        }
    }
}

// ── Public handlers ───────────────────────────────────────────────────────────

pub async fn list_seasons(State(state): State<AppState>) -> impl IntoResponse {
    match state.query_service.list_seasons().await {
        Ok(seasons) => Json(seasons).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

pub async fn get_season_subjects(
    State(state): State<AppState>,
    Path(season_id): Path<i32>,
) -> impl IntoResponse {
    match state.query_service.get_season_subjects(season_id).await {
        Ok(Some(items)) => Json(items).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: format!("Season {} not found", season_id),
            }),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

pub async fn get_current_season(State(state): State<AppState>) -> impl IntoResponse {
    match state.query_service.get_current_season().await {
        Ok(Some(season)) => Json(season).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "Current season not found".to_string(),
            }),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

pub async fn get_all_seasons_top1(State(state): State<AppState>) -> impl IntoResponse {
    match state.query_service.get_all_seasons_top1().await {
        Ok(items) => Json(items).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

// ── Admin handlers ────────────────────────────────────────────────────────────

fn progress_to_sse_stream(
    rx: mpsc::Receiver<SyncProgressEvent>,
) -> impl tokio_stream::Stream<Item = Result<Event, Infallible>> {
    ReceiverStream::new(rx).map(|event| {
        let data = match event {
            SyncProgressEvent::Progress { total, done, subject_id } => {
                serde_json::json!({ "type": "progress", "total": total, "done": done, "subject_id": subject_id })
            }
            SyncProgressEvent::Done { season_id, added, removed, updated, failed } => {
                serde_json::json!({ "type": "done", "season_id": season_id, "added": added, "removed": removed, "updated": updated, "failed": failed })
            }
            SyncProgressEvent::Error { message } => {
                serde_json::json!({ "type": "error", "message": message })
            }
        };
        Ok::<Event, Infallible>(Event::default().data(data.to_string()))
    })
}

pub async fn create_season(
    State(state): State<AppState>,
    Json(req): Json<CreateSeasonRequest>,
) -> impl IntoResponse {
    if ![1, 4, 7, 10].contains(&req.month) {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: format!("Invalid month: {}. Must be 1, 4, 7, or 10", req.month),
            }),
        )
            .into_response();
    }

    let (tx, rx) = mpsc::channel::<SyncProgressEvent>(64);
    let svc = Arc::clone(&state.sync_service);
    let (year, month, name) = (req.year, req.month, req.name);

    tokio::spawn(async move {
        if let Err(e) = svc
            .create_and_sync(year, month, name, Some(tx.clone()))
            .await
        {
            let _ = tx
                .send(SyncProgressEvent::Error {
                    message: e.to_string(),
                })
                .await;
        }
    });

    Sse::new(progress_to_sse_stream(rx))
        .keep_alive(KeepAlive::default())
        .into_response()
}

pub async fn sync_season(
    State(state): State<AppState>,
    Path(season_id): Path<i32>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
    let (tx, rx) = mpsc::channel::<SyncProgressEvent>(64);
    let svc = Arc::clone(&state.sync_service);

    tokio::spawn(async move {
        if let Err(e) = svc.resync(season_id, Some(tx.clone())).await {
            let _ = tx
                .send(SyncProgressEvent::Error {
                    message: e.to_string(),
                })
                .await;
        }
    });

    Sse::new(progress_to_sse_stream(rx)).keep_alive(KeepAlive::default())
}

pub async fn list_orphan_subjects(State(state): State<AppState>) -> impl IntoResponse {
    match state.sync_service.find_orphans().await {
        Ok(items) => {
            let response: Vec<OrphanSubjectItem> = items
                .into_iter()
                .map(|o| OrphanSubjectItem {
                    id: o.id,
                    name: o.name,
                    name_cn: o.name_cn,
                })
                .collect();
            Json(response).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

pub async fn delete_season(
    State(state): State<AppState>,
    Path(season_id): Path<i32>,
) -> impl IntoResponse {
    match state.sync_service.delete_season(season_id).await {
        Ok(true) => (
            StatusCode::OK,
            Json(DeleteSeasonResponse {
                season_id,
                deleted: true,
            }),
        )
            .into_response(),
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: format!("Season {} not found", season_id),
            }),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

pub async fn delete_orphan_subjects(State(state): State<AppState>) -> impl IntoResponse {
    match state.sync_service.delete_orphans().await {
        Ok(deleted) => Json(DeleteOrphansResponse { deleted }).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

// US1: POST /admin/deploy
pub async fn trigger_deploy(State(state): State<AppState>) -> impl IntoResponse {
    match state.admin_service.trigger_deploy().await {
        Ok(()) => (
            StatusCode::ACCEPTED,
            Json(AcceptedResponse {
                status: "accepted".to_string(),
                message: "Deploy triggered".to_string(),
            }),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

// US2: POST /admin/seasons/sync-all
pub async fn sync_all_seasons(State(state): State<AppState>) -> impl IntoResponse {
    let svc = Arc::clone(&state.admin_service);
    tokio::spawn(async move {
        svc.sync_all_seasons().await;
    });
    (
        StatusCode::ACCEPTED,
        Json(AcceptedResponse {
            status: "accepted".to_string(),
            message: "Sync all seasons started".to_string(),
        }),
    )
        .into_response()
}

// US3: POST /admin/scheduler/trigger
pub async fn trigger_scheduler(State(state): State<AppState>) -> impl IntoResponse {
    match state.admin_service.trigger_scheduler_tick() {
        Ok(()) => (
            StatusCode::ACCEPTED,
            Json(AcceptedResponse {
                status: "accepted".to_string(),
                message: "Scheduler tick triggered".to_string(),
            }),
        )
            .into_response(),
        Err(_busy) => (
            StatusCode::CONFLICT,
            Json(ErrorResponse {
                error: "Scheduler is already running".to_string(),
            }),
        )
            .into_response(),
    }
}

// US4: GET /admin/scheduler/status
pub async fn scheduler_status(State(state): State<AppState>) -> impl IntoResponse {
    let status = state.admin_service.get_scheduler_status();
    Json(SchedulerStatusResponse {
        is_running: status.is_running,
        last_run_at: status.last_run_at,
        last_stats: status.last_stats,
    })
    .into_response()
}

// US9: GET /admin/subjects/{id}
pub async fn admin_get_subject(
    State(state): State<AppState>,
    Path(subject_id): Path<i32>,
) -> impl IntoResponse {
    match state.admin_service.get_subject(subject_id).await {
        Ok(Some(s)) => Json(s).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: format!("Subject {} not found", subject_id),
            }),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

// US9: GET /admin/seasons/{id}
pub async fn admin_get_season(
    State(state): State<AppState>,
    Path(season_id): Path<i32>,
) -> impl IntoResponse {
    match state.admin_service.get_season(season_id).await {
        Ok(Some(s)) => Json(s).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: format!("Season {} not found", season_id),
            }),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

// US5: PATCH /admin/subjects/{id}
pub async fn patch_subject(
    State(state): State<AppState>,
    Path(subject_id): Path<i32>,
    Json(req): Json<EditSubjectRequest>,
) -> impl IntoResponse {
    use crate::dal::dto::UpdateSubject;
    let update = UpdateSubject {
        name: req.name,
        name_cn: req.name_cn,
        images_grid: req.images_grid,
        images_large: req.images_large,
        rank: req.rank,
        score: req.score,
        collection_total: req.collection_total,
        average_comment: req.average_comment,
        drop_rate: req.drop_rate,
        air_weekday: req.air_weekday,
        meta_tags: req.meta_tags,
    };
    match state.admin_service.update_subject(subject_id, update).await {
        Ok(Some(s)) => Json(s).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: format!("Subject {} not found", subject_id),
            }),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

// US8: PATCH /admin/seasons/{id}
pub async fn patch_season(
    State(state): State<AppState>,
    Path(season_id): Path<i32>,
    Json(req): Json<EditSeasonRequest>,
) -> impl IntoResponse {
    use crate::dal::dto::UpdateSeason;
    let update = UpdateSeason {
        year: None,
        season: None,
        name: req.name,
    };
    match state.admin_service.update_season(season_id, update).await {
        Ok(Some(s)) => Json(s).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: format!("Season {} not found", season_id),
            }),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

// US6: DELETE /admin/subjects/{id}
pub async fn delete_subject(
    State(state): State<AppState>,
    Path(subject_id): Path<i32>,
) -> impl IntoResponse {
    match state.admin_service.delete_subject(subject_id).await {
        Ok(true) => Json(DeletedResponse { deleted: true }).into_response(),
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: format!("Subject {} not found", subject_id),
            }),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

fn admin_op_error_response(err: AdminOpError) -> Response {
    match err {
        AdminOpError::BadRequest(msg) => {
            (StatusCode::BAD_REQUEST, Json(ErrorResponse { error: msg })).into_response()
        }
        AdminOpError::NotFound(msg) => {
            (StatusCode::NOT_FOUND, Json(ErrorResponse { error: msg })).into_response()
        }
        AdminOpError::Internal(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

// US7: DELETE /admin/seasons/{season_id}/mal/{mal_id}
pub async fn remove_season_mal(
    State(state): State<AppState>,
    Path((season_id, mal_id)): Path<(i32, i32)>,
) -> impl IntoResponse {
    match state
        .admin_service
        .remove_mal_from_season(season_id, mal_id)
        .await
    {
        Ok(true) => Json(RemovedResponse { removed: true }).into_response(),
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "Association not found".to_string(),
            }),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

// POST /admin/seasons/{season_id}/mal/{mal_id}/restore
pub async fn restore_season_mal(
    State(state): State<AppState>,
    Path((season_id, mal_id)): Path<(i32, i32)>,
) -> impl IntoResponse {
    match state
        .admin_service
        .restore_mal_to_season(season_id, mal_id)
        .await
    {
        Ok(true) => Json(RestoredResponse { restored: true }).into_response(),
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "Association not found".to_string(),
            }),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

// GET /admin/matches?season_id=&status=
pub async fn list_matches(
    State(state): State<AppState>,
    Query(query): Query<ListMatchesQuery>,
) -> impl IntoResponse {
    match state
        .admin_service
        .list_matches(query.season_id, query.status.as_deref())
        .await
    {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => admin_op_error_response(e),
    }
}

// PATCH /admin/matches/{mal_id}
pub async fn patch_match(
    State(state): State<AppState>,
    Path(mal_id): Path<i32>,
    Json(req): Json<PatchMatchRequest>,
) -> impl IntoResponse {
    let input = PatchMatchInput {
        status: req.status,
        bgm_id: req.bgm_id,
        reason: req.reason,
        note: req.note,
    };
    match state.admin_service.patch_match(mal_id, input).await {
        Ok(res) => Json(PatchMatchResponse {
            match_row: res.match_row,
            hydrated: res.hydrated,
        })
        .into_response(),
        Err(e) => admin_op_error_response(e),
    }
}

// POST /admin/seasons/{season_id}/mal/{mal_id}/move
pub async fn move_season_mal(
    State(state): State<AppState>,
    Path((season_id, mal_id)): Path<(i32, i32)>,
    Json(req): Json<MoveSeasonMalRequest>,
) -> impl IntoResponse {
    match state
        .admin_service
        .move_season_mal(season_id, mal_id, req.target_season_id, req.note)
        .await
    {
        Ok(res) => Json(MoveSeasonMalResponse {
            moved: true,
            source: res.source,
            target: res.target,
        })
        .into_response(),
        Err(e) => admin_op_error_response(e),
    }
}

fn ndjson_response(result: anyhow::Result<String>) -> Response {
    match result {
        Ok(body) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/x-ndjson")],
            body,
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

// GET /admin/export/season_mal
pub async fn export_season_mal(State(state): State<AppState>) -> impl IntoResponse {
    ndjson_response(state.admin_service.export_season_mal_jsonl().await)
}

// GET /admin/export/matches
pub async fn export_matches(State(state): State<AppState>) -> impl IntoResponse {
    ndjson_response(state.admin_service.export_matches_jsonl().await)
}

pub fn admin_router(state: AppState) -> axum::Router {
    use axum::routing::{delete, get, patch, post};
    axum::Router::new()
        .route("/admin/deploy", post(trigger_deploy))
        .route("/admin/seasons", post(create_season))
        .route("/admin/seasons/sync-all", post(sync_all_seasons))
        .route("/admin/seasons/{season_id}", get(admin_get_season))
        .route("/admin/seasons/{season_id}", delete(delete_season))
        .route("/admin/seasons/{season_id}", patch(patch_season))
        .route("/admin/seasons/{season_id}/sync", post(sync_season))
        .route(
            "/admin/seasons/{season_id}/mal/{mal_id}",
            delete(remove_season_mal),
        )
        .route(
            "/admin/seasons/{season_id}/mal/{mal_id}/move",
            post(move_season_mal),
        )
        .route(
            "/admin/seasons/{season_id}/mal/{mal_id}/restore",
            post(restore_season_mal),
        )
        .route("/admin/matches", get(list_matches))
        .route("/admin/matches/{mal_id}", patch(patch_match))
        .route("/admin/export/season_mal", get(export_season_mal))
        .route("/admin/export/matches", get(export_matches))
        .route("/admin/scheduler/trigger", post(trigger_scheduler))
        .route("/admin/scheduler/status", get(scheduler_status))
        .route("/admin/subjects/orphans", get(list_orphan_subjects))
        .route("/admin/subjects/orphans", delete(delete_orphan_subjects))
        .route("/admin/subjects/{subject_id}", get(admin_get_subject))
        .route("/admin/subjects/{subject_id}", patch(patch_subject))
        .route("/admin/subjects/{subject_id}", delete(delete_subject))
        .layer(middleware::from_fn(require_admin_token))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dal::dto::{CreateSeason, UpdateMalBgmMatch, UpsertMalAnime};
    use crate::dal::repositories::{
        MalAnimeRepository, MalBgmMatchRepository, SeasonMalRepository, SeasonRepository,
        SubjectRepository,
    };
    use crate::services::bangumi::BangumiClient;
    use axum::{
        Router,
        body::Body,
        http::{Request, StatusCode},
        routing::get,
    };
    use sqlx::SqlitePool;
    use tower::ServiceExt;

    fn test_db(pool: SqlitePool) -> Arc<Database> {
        Arc::new(Database::from_pool(pool))
    }

    fn admin_token() -> String {
        format!(
            "Bearer {}",
            std::env::var("ADMIN_TOKEN").unwrap_or_else(|_| "testtoken".to_string())
        )
    }

    fn state_with_bgm_url(db: Arc<Database>, bgm_url: &str) -> AppState {
        let handle = SchedulerHandle::new();
        let sync_service = Arc::new(SyncService::with_clients(
            Arc::clone(&db),
            BangumiClient::with_base_url(bgm_url),
        ));
        let admin_service = AdminService::with_sync_service(
            Arc::clone(&db),
            handle.clone(),
            None,
            Arc::clone(&sync_service),
        );
        AppState {
            query_service: Arc::new(QueryService::new(Arc::clone(&db))),
            sync_service,
            admin_service: Arc::new(admin_service),
            scheduler_handle: Arc::new(handle),
            db,
        }
    }

    fn test_public_app(db: Arc<Database>) -> Router {
        let state = AppState::new(db);
        Router::new()
            .route("/api/seasons", get(list_seasons))
            .route(
                "/api/seasons/{season_id}/subjects",
                get(get_season_subjects),
            )
            .with_state(state)
    }

    async fn seed_season_mal_and_match(
        pool: &SqlitePool,
        season_id: i32,
        mal_id: i32,
        status: &str,
        bgm_id: Option<i32>,
    ) {
        let season_str = match season_id % 100 {
            1 => "WINTER",
            4 => "SPRING",
            7 => "SUMMER",
            _ => "FALL",
        };
        SeasonRepository::new(pool)
            .upsert(CreateSeason {
                season_id,
                year: season_id / 100,
                season: season_str.to_string(),
                name: None,
            })
            .await
            .unwrap();
        MalAnimeRepository::new(pool)
            .upsert(UpsertMalAnime {
                mal_id,
                title: format!("MAL {}", mal_id),
                media_type: "tv".to_string(),
                ..Default::default()
            })
            .await
            .unwrap();
        SeasonMalRepository::new(pool)
            .insert_ignore(season_id, mal_id)
            .await
            .unwrap();
        if let Some(id) = bgm_id {
            sqlx::query("INSERT OR IGNORE INTO subjects (id) VALUES (?)")
                .bind(id)
                .execute(pool)
                .await
                .unwrap();
        }
        MalBgmMatchRepository::new(pool)
            .upsert(
                mal_id,
                UpdateMalBgmMatch {
                    status: status.to_string(),
                    bgm_id,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
    }

    // T011/T012 — GET /api/seasons, GET /api/seasons/{id}/subjects
    #[sqlx::test]
    async fn test_list_seasons_returns_200(pool: SqlitePool) {
        let app = test_public_app(test_db(pool));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/seasons")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[sqlx::test]
    async fn test_get_season_subjects_404_for_unknown(pool: SqlitePool) {
        let app = test_public_app(test_db(pool));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/seasons/999999/subjects")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    // T021/T022 — POST /admin/seasons
    #[sqlx::test]
    async fn test_create_season_invalid_month_returns_400(pool: SqlitePool) {
        let app = admin_router(AppState::new(test_db(pool)));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/seasons")
                    .header("Authorization", admin_token())
                    .header("Content-Type", "application/json")
                    .body(Body::from(r#"{"year":2026,"month":2}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[sqlx::test]
    async fn test_create_season_no_token_returns_401(pool: SqlitePool) {
        let app = admin_router(AppState::new(test_db(pool)));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/seasons")
                    .header("Content-Type", "application/json")
                    .body(Body::from(r#"{"year":2026,"month":1}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    // T023/T024 — POST /admin/seasons/{id}/sync
    #[sqlx::test]
    async fn test_sync_season_no_token_returns_401(pool: SqlitePool) {
        let app = admin_router(AppState::new(test_db(pool)));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/seasons/202601/sync")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[sqlx::test]
    async fn test_sync_season_unknown_id_returns_sse_error(pool: SqlitePool) {
        let app = admin_router(AppState::new(test_db(pool)));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/seasons/999999/sync")
                    .header("Authorization", admin_token())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        assert!(
            body_str.contains("\"type\":\"error\""),
            "expected SSE error event, got: {body_str}"
        );
    }

    // T040 — GET /admin/subjects/orphans
    #[sqlx::test]
    async fn test_list_orphans_no_token_returns_401(pool: SqlitePool) {
        let app = admin_router(AppState::new(test_db(pool)));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/admin/subjects/orphans")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[sqlx::test]
    async fn test_list_orphans_with_token_returns_200(pool: SqlitePool) {
        let app = admin_router(AppState::new(test_db(pool)));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/admin/subjects/orphans")
                    .header("Authorization", admin_token())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    // T012 — DELETE /admin/seasons/{season_id}
    #[sqlx::test]
    async fn test_delete_season_no_token_returns_401(pool: SqlitePool) {
        let app = admin_router(AppState::new(test_db(pool)));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/admin/seasons/202601")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[sqlx::test]
    async fn test_delete_season_with_token_not_found_returns_404(pool: SqlitePool) {
        let app = admin_router(AppState::new(test_db(pool)));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/admin/seasons/999989")
                    .header("Authorization", admin_token())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[sqlx::test]
    async fn test_delete_season_with_token_existing_returns_200(pool: SqlitePool) {
        let db = test_db(pool);
        SeasonRepository::new(db.pool())
            .upsert(CreateSeason {
                season_id: 202688,
                year: 2026,
                season: "FALL".to_string(),
                name: None,
            })
            .await
            .unwrap();

        let app = admin_router(AppState::new(db));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/admin/seasons/202688")
                    .header("Authorization", admin_token())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["deleted"], true);
    }

    // T042 — DELETE /admin/subjects/orphans
    #[sqlx::test]
    async fn test_delete_orphans_no_token_returns_401(pool: SqlitePool) {
        let app = admin_router(AppState::new(test_db(pool)));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/admin/subjects/orphans")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[sqlx::test]
    async fn test_delete_orphans_with_token_returns_200(pool: SqlitePool) {
        let app = admin_router(AppState::new(test_db(pool)));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/admin/subjects/orphans")
                    .header("Authorization", admin_token())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    // T011/T012 — POST /admin/deploy
    #[sqlx::test]
    async fn test_trigger_deploy_no_token_returns_401(pool: SqlitePool) {
        let app = admin_router(AppState::new(test_db(pool)));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/deploy")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[sqlx::test]
    async fn test_trigger_deploy_with_valid_token_returns_202(pool: SqlitePool) {
        let app = admin_router(AppState::new(test_db(pool)));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/deploy")
                    .header("Authorization", admin_token())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
    }

    // T018/T019 — POST /admin/seasons/sync-all
    #[sqlx::test]
    async fn test_sync_all_seasons_no_token_returns_401(pool: SqlitePool) {
        let app = admin_router(AppState::new(test_db(pool)));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/seasons/sync-all")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[sqlx::test]
    async fn test_sync_all_seasons_with_token_returns_202(pool: SqlitePool) {
        let app = admin_router(AppState::new(test_db(pool)));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/seasons/sync-all")
                    .header("Authorization", admin_token())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
    }

    // T024/T025/T026 — POST /admin/scheduler/trigger
    #[sqlx::test]
    async fn test_trigger_scheduler_no_token_returns_401(pool: SqlitePool) {
        let app = admin_router(AppState::new(test_db(pool)));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/scheduler/trigger")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[sqlx::test]
    async fn test_trigger_scheduler_with_token_idle_returns_202(pool: SqlitePool) {
        let app = admin_router(AppState::new(test_db(pool)));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/scheduler/trigger")
                    .header("Authorization", admin_token())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
    }

    #[sqlx::test]
    async fn test_trigger_scheduler_already_running_returns_409(pool: SqlitePool) {
        use std::sync::atomic::Ordering;
        let db = test_db(pool);
        let state = AppState::new(db);
        state
            .scheduler_handle
            .is_running
            .store(true, Ordering::SeqCst);
        let app = admin_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/scheduler/trigger")
                    .header("Authorization", admin_token())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    // T031/T032 — GET /admin/scheduler/status
    #[sqlx::test]
    async fn test_scheduler_status_no_token_returns_401(pool: SqlitePool) {
        let app = admin_router(AppState::new(test_db(pool)));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/admin/scheduler/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[sqlx::test]
    async fn test_scheduler_status_with_token_returns_200_with_is_running_field(pool: SqlitePool) {
        let app = admin_router(AppState::new(test_db(pool)));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/admin/scheduler/status")
                    .header("Authorization", admin_token())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json.get("is_running").is_some());
    }

    // T037/T038/T039 — GET /admin/subjects/{id}, GET /admin/seasons/{id}
    #[sqlx::test]
    async fn test_get_subject_admin_no_token_returns_401(pool: SqlitePool) {
        let app = admin_router(AppState::new(test_db(pool)));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/admin/subjects/1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[sqlx::test]
    async fn test_get_subject_admin_not_found_returns_404(pool: SqlitePool) {
        let app = admin_router(AppState::new(test_db(pool)));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/admin/subjects/99999999")
                    .header("Authorization", admin_token())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[sqlx::test]
    async fn test_get_season_admin_not_found_returns_404(pool: SqlitePool) {
        let app = admin_router(AppState::new(test_db(pool)));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/admin/seasons/999999")
                    .header("Authorization", admin_token())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    // T045/T046 — PATCH /admin/subjects/{id}
    #[sqlx::test]
    async fn test_patch_subject_no_token_returns_401(pool: SqlitePool) {
        let app = admin_router(AppState::new(test_db(pool)));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/admin/subjects/1")
                    .header("Content-Type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[sqlx::test]
    async fn test_patch_subject_not_found_returns_404(pool: SqlitePool) {
        let app = admin_router(AppState::new(test_db(pool)));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/admin/subjects/99999999")
                    .header("Authorization", admin_token())
                    .header("Content-Type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    // T052/T053 — PATCH /admin/seasons/{id}
    #[sqlx::test]
    async fn test_patch_season_no_token_returns_401(pool: SqlitePool) {
        let app = admin_router(AppState::new(test_db(pool)));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/admin/seasons/202601")
                    .header("Content-Type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[sqlx::test]
    async fn test_patch_season_not_found_returns_404(pool: SqlitePool) {
        let app = admin_router(AppState::new(test_db(pool)));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/admin/seasons/999999")
                    .header("Authorization", admin_token())
                    .header("Content-Type", "application/json")
                    .body(Body::from(r#"{"name":"test"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    // T058/T059 — DELETE /admin/subjects/{id}
    #[sqlx::test]
    async fn test_delete_subject_no_token_returns_401(pool: SqlitePool) {
        let app = admin_router(AppState::new(test_db(pool)));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/admin/subjects/1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[sqlx::test]
    async fn test_delete_subject_not_found_returns_404(pool: SqlitePool) {
        let app = admin_router(AppState::new(test_db(pool)));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/admin/subjects/99999999")
                    .header("Authorization", admin_token())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[sqlx::test]
    async fn test_delete_subject_resets_referencing_matches_to_pending(pool: SqlitePool) {
        seed_season_mal_and_match(&pool, 202601, 2001, "included", Some(515759)).await;
        seed_season_mal_and_match(&pool, 202601, 2002, "excluded", Some(515759)).await;
        MalBgmMatchRepository::new(&pool)
            .upsert(
                2001,
                UpdateMalBgmMatch {
                    status: "included".to_string(),
                    source: Some("human".to_string()),
                    bgm_id: Some(515759),
                    confidence: Some(1.0),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let app = admin_router(AppState::new(test_db(pool.clone())));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/admin/subjects/515759")
                    .header("Authorization", admin_token())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["deleted"], true);

        assert!(
            SubjectRepository::new(&pool)
                .find_by_id(515759)
                .await
                .unwrap()
                .is_none()
        );
        for mal_id in [2001, 2002] {
            let m = MalBgmMatchRepository::new(&pool)
                .get(mal_id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(m.status, "pending");
            assert_eq!(m.bgm_id, None);
            assert_eq!(m.source, None);
            assert_eq!(m.confidence, None);
        }
    }

    #[sqlx::test]
    async fn test_app_state_reuses_single_sync_service(pool: SqlitePool) {
        let state = AppState::new(test_db(pool));
        assert!(Arc::ptr_eq(
            &state.sync_service,
            state.admin_service.sync_service()
        ));
    }

    // DELETE /admin/seasons/{season_id}/mal/{mal_id}
    #[sqlx::test]
    async fn test_remove_season_mal_no_token_returns_401(pool: SqlitePool) {
        let app = admin_router(AppState::new(test_db(pool)));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/admin/seasons/202601/mal/1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[sqlx::test]
    async fn test_remove_season_mal_not_found_returns_404(pool: SqlitePool) {
        let app = admin_router(AppState::new(test_db(pool)));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/admin/seasons/999999/mal/999999")
                    .header("Authorization", admin_token())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[sqlx::test]
    async fn test_remove_season_mal_marks_is_removed_returns_200(pool: SqlitePool) {
        seed_season_mal_and_match(&pool, 202601, 1001, "included", Some(515759)).await;

        let app = admin_router(AppState::new(test_db(pool.clone())));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/admin/seasons/202601/mal/1001")
                    .header("Authorization", admin_token())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["removed"], true);

        let row = SeasonMalRepository::new(&pool)
            .get(202601, 1001)
            .await
            .unwrap()
            .unwrap();
        assert!(row.is_removed);
    }

    #[sqlx::test]
    async fn test_old_remove_season_subjects_route_removed(pool: SqlitePool) {
        seed_season_mal_and_match(&pool, 202601, 1001, "included", Some(515759)).await;

        let app = admin_router(AppState::new(test_db(pool.clone())));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/admin/seasons/202601/subjects/1001")
                    .header("Authorization", admin_token())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(
            matches!(
                resp.status(),
                StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
            ),
            "expected 404/405 for removed route, got {}",
            resp.status()
        );

        let row = SeasonMalRepository::new(&pool)
            .get(202601, 1001)
            .await
            .unwrap()
            .unwrap();
        assert!(!row.is_removed);
    }

    // POST /admin/seasons/{season_id}/mal/{mal_id}/restore
    #[sqlx::test]
    async fn test_restore_season_mal_no_token_returns_401(pool: SqlitePool) {
        let app = admin_router(AppState::new(test_db(pool)));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/seasons/202601/mal/1001/restore")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[sqlx::test]
    async fn test_restore_season_mal_not_found_returns_404(pool: SqlitePool) {
        let app = admin_router(AppState::new(test_db(pool)));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/seasons/202601/mal/999999/restore")
                    .header("Authorization", admin_token())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[sqlx::test]
    async fn test_restore_season_mal_sets_is_removed_false_returns_200(pool: SqlitePool) {
        seed_season_mal_and_match(&pool, 202601, 1001, "included", Some(515759)).await;
        SeasonMalRepository::new(&pool)
            .set_removed(202601, 1001, true, Some("soft removed".to_string()))
            .await
            .unwrap();

        let app = admin_router(AppState::new(test_db(pool.clone())));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/seasons/202601/mal/1001/restore")
                    .header("Authorization", admin_token())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["restored"], true);

        let row = SeasonMalRepository::new(&pool)
            .get(202601, 1001)
            .await
            .unwrap()
            .unwrap();
        assert!(!row.is_removed);
        assert_eq!(row.note.as_deref(), Some("soft removed"));
    }

    // GET /admin/matches?season_id=&status=
    #[sqlx::test]
    async fn test_list_matches_no_token_returns_401(pool: SqlitePool) {
        let app = admin_router(AppState::new(test_db(pool)));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/admin/matches")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[sqlx::test]
    async fn test_list_matches_filters_by_season_and_status_with_joined_fields(pool: SqlitePool) {
        use crate::dal::dto::CreateSubject;

        seed_season_mal_and_match(&pool, 202601, 1001, "pending", None).await;
        seed_season_mal_and_match(&pool, 202601, 1002, "included", Some(515759)).await;
        seed_season_mal_and_match(&pool, 202601, 1003, "excluded", None).await;
        seed_season_mal_and_match(&pool, 202604, 1004, "pending", None).await;

        // Soft-remove 1003 from 202601 so season_id=202601 excludes it
        SeasonMalRepository::new(&pool)
            .set_removed(202601, 1003, true, None)
            .await
            .unwrap();

        // Populate rich mal_anime and subject fields for 1001 and 1002
        MalAnimeRepository::new(&pool)
            .upsert(UpsertMalAnime {
                mal_id: 1002,
                title: "Sousou no Frieren 2nd Season".to_string(),
                title_ja: Some("葬送のフリーレン 第2期".to_string()),
                media_type: "tv".to_string(),
                rating: Some("pg_13".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        SubjectRepository::new(&pool)
            .upsert(CreateSubject {
                id: 515759,
                name: Some("葬送のフリーレン 第2期".to_string()),
                name_cn: Some("葬送的芙莉莲 第二季".to_string()),
                rank: Some(7),
                score: Some(8.85),
                ..Default::default()
            })
            .await
            .unwrap();

        let db = test_db(pool);
        let app = admin_router(AppState::new(Arc::clone(&db)));
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/admin/matches")
                    .header("Authorization", admin_token())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let all: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert_eq!(all.len(), 4);

        // Verify unbound item (1001) has mal_anime fields and subject: null
        assert_eq!(all[0]["mal_id"], 1001);
        assert_eq!(all[0]["title"], "MAL 1001");
        assert_eq!(all[0]["title_ja"], serde_json::Value::Null);
        assert_eq!(all[0]["media_type"], "tv");
        assert_eq!(all[0]["rating"], serde_json::Value::Null);
        assert_eq!(all[0]["subject"], serde_json::Value::Null);

        // Verify bound item (1002) has mal_anime fields and subject object
        assert_eq!(all[1]["mal_id"], 1002);
        assert_eq!(all[1]["status"], "included");
        assert_eq!(all[1]["bgm_id"], 515759);
        assert_eq!(all[1]["title"], "Sousou no Frieren 2nd Season");
        assert_eq!(all[1]["title_ja"], "葬送のフリーレン 第2期");
        assert_eq!(all[1]["media_type"], "tv");
        assert_eq!(all[1]["rating"], "pg_13");
        assert_eq!(
            all[1]["subject"],
            serde_json::json!({
                "id": 515759,
                "name": "葬送のフリーレン 第2期",
                "name_cn": "葬送的芙莉莲 第二季",
                "rank": 7,
                "score": 8.85
            })
        );

        // Filter by status=pending returns 1001 and 1004
        let resp_pending = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/admin/matches?status=pending")
                    .header("Authorization", admin_token())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp_pending.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp_pending.into_body(), usize::MAX)
            .await
            .unwrap();
        let pending: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0]["mal_id"], 1001);
        assert_eq!(pending[1]["mal_id"], 1004);

        // Filter by season_id=202601 returns 1001 & 1002 (excludes 1003 is_removed=1 and 1004 from 202604)
        let resp_season = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/admin/matches?season_id=202601")
                    .header("Authorization", admin_token())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp_season.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp_season.into_body(), usize::MAX)
            .await
            .unwrap();
        let season_items: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert_eq!(season_items.len(), 2);
        assert_eq!(season_items[0]["mal_id"], 1001);
        assert_eq!(season_items[1]["mal_id"], 1002);

        // Filter by both season_id=202601 and status=pending returns only 1001
        let resp_combined = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/admin/matches?season_id=202601&status=pending")
                    .header("Authorization", admin_token())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp_combined.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp_combined.into_body(), usize::MAX)
            .await
            .unwrap();
        let combined: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert_eq!(combined.len(), 1);
        assert_eq!(combined[0]["mal_id"], 1001);

        let resp_bad = app
            .oneshot(
                Request::builder()
                    .uri("/admin/matches?status=invalid")
                    .header("Authorization", admin_token())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp_bad.status(), StatusCode::BAD_REQUEST);
    }

    // PATCH /admin/matches/{mal_id}
    #[sqlx::test]
    async fn test_patch_match_no_token_returns_401(pool: SqlitePool) {
        let app = admin_router(AppState::new(test_db(pool)));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/admin/matches/1001")
                    .header("Content-Type", "application/json")
                    .body(Body::from(r#"{"status":"excluded"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[sqlx::test]
    async fn test_patch_match_included_creates_skeleton_hydrates_and_sets_human(pool: SqlitePool) {
        let mut bgm_server = mockito::Server::new_async().await;
        let ep_mock = bgm_server
            .mock("GET", mockito::Matcher::Regex(r"^/v0/episodes".into()))
            .with_header("content-type", "application/json")
            .with_body(r#"{"total":0,"limit":100,"offset":0,"data":[]}"#)
            .expect(1)
            .create_async()
            .await;
        let subj_mock = bgm_server
            .mock("GET", "/v0/subjects/515759")
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"id":515759,"type":2,"name":"葬送のフリーレン","name_cn":"葬送的芙莉莲",
                    "date":"2026-01-16","platform":"TV","summary":"简介","nsfw":false,
                    "rating":{"rank":10,"total":10,"count":{"9":10}},
                    "collection":{"wish":1,"collect":2,"doing":3,"on_hold":4,"dropped":5},
                    "infobox":[],"meta_tags":["TV"],"tags":[{"name":"奇幻","count":100}]}"#,
            )
            .expect(1)
            .create_async()
            .await;

        seed_season_mal_and_match(&pool, 202601, 59978, "pending", None).await;

        let db = test_db(pool.clone());
        let app = admin_router(state_with_bgm_url(db, &bgm_server.url()));

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/admin/matches/59978")
                    .header("Authorization", admin_token())
                    .header("Content-Type", "application/json")
                    .body(Body::from(
                        r#"{"status":"included","bgm_id":515759,"note":"人工确认"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["mal_id"], 59978);
        assert_eq!(json["status"], "included");
        assert_eq!(json["source"], "human");
        assert_eq!(json["bgm_id"], 515759);
        assert_eq!(json["note"], "人工确认");
        assert_eq!(json["hydrated"], true);

        ep_mock.assert_async().await;
        subj_mock.assert_async().await;

        let subject = SubjectRepository::new(&pool)
            .find_by_id(515759)
            .await
            .unwrap()
            .expect("hydrated subject should exist");
        assert_eq!(subject.name_cn.as_deref(), Some("葬送的芙莉莲"));
        assert_eq!(subject.air_date.as_deref(), Some("2026-01-16"));
        assert!(subject.last_updated_at.is_some());

        // 缺少 bgm_id 的 included 应返回 400
        seed_season_mal_and_match(&pool, 202601, 59979, "pending", None).await;
        let bad_resp = app
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/admin/matches/59979")
                    .header("Authorization", admin_token())
                    .header("Content-Type", "application/json")
                    .body(Body::from(r#"{"status":"included"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(bad_resp.status(), StatusCode::BAD_REQUEST);
    }

    #[sqlx::test]
    async fn test_patch_match_validates_bgm_id_positive(pool: SqlitePool) {
        seed_season_mal_and_match(&pool, 202601, 60001, "pending", None).await;
        let app = admin_router(AppState::new(test_db(pool)));

        for payload in [
            r#"{"status":"included","bgm_id":0}"#,
            r#"{"status":"included","bgm_id":-10}"#,
            r#"{"status":"excluded","bgm_id":0}"#,
            r#"{"status":"pending","bgm_id":-1}"#,
        ] {
            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("PATCH")
                        .uri("/admin/matches/60001")
                        .header("Authorization", admin_token())
                        .header("Content-Type", "application/json")
                        .body(Body::from(payload))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "payload={payload}");
        }
    }

    #[sqlx::test]
    async fn test_patch_match_excluded_and_pending_with_and_without_bgm_id(pool: SqlitePool) {
        seed_season_mal_and_match(&pool, 202601, 60010, "included", Some(5001)).await;
        MalBgmMatchRepository::new(&pool)
            .upsert(
                60010,
                UpdateMalBgmMatch {
                    status: "included".to_string(),
                    source: Some("llm".to_string()),
                    bgm_id: Some(5001),
                    confidence: Some(0.92),
                    note: Some("initial note".to_string()),
                    reason: Some("initial reason".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let app = admin_router(AppState::new(test_db(pool.clone())));

        // 1. excluded without bgm_id -> 200, source='human', bgm_id=null, hydrated=false
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/admin/matches/60010")
                    .header("Authorization", admin_token())
                    .header("Content-Type", "application/json")
                    .body(Body::from(r#"{"status":"excluded","reason":"mini"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "excluded");
        assert_eq!(json["source"], "human");
        assert_eq!(json["bgm_id"], serde_json::Value::Null);
        assert_eq!(json["reason"], "mini");
        assert_eq!(json["note"], "initial note");
        assert_eq!(json["hydrated"], false);

        // 2. excluded WITH non-existent bgm_id -> creates skeleton (no FK 500), hydrated=false
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/admin/matches/60010")
                    .header("Authorization", admin_token())
                    .header("Content-Type", "application/json")
                    .body(Body::from(r#"{"status":"excluded","bgm_id":88001}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "excluded");
        assert_eq!(json["source"], "human");
        assert_eq!(json["bgm_id"], 88001);
        assert_eq!(json["reason"], "mini");
        assert_eq!(json["hydrated"], false);
        assert!(
            SubjectRepository::new(&pool)
                .find_by_id(88001)
                .await
                .unwrap()
                .is_some()
        );

        // 3. pending WITH explicit non-existent bgm_id -> skeleton created, source=null, confidence=null
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/admin/matches/60010")
                    .header("Authorization", admin_token())
                    .header("Content-Type", "application/json")
                    .body(Body::from(r#"{"status":"pending","bgm_id":88002}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "pending");
        assert_eq!(json["source"], serde_json::Value::Null);
        assert_eq!(json["confidence"], serde_json::Value::Null);
        assert_eq!(json["bgm_id"], 88002);
        assert_eq!(json["hydrated"], false);
        assert!(
            SubjectRepository::new(&pool)
                .find_by_id(88002)
                .await
                .unwrap()
                .is_some()
        );

        // 4. pending WITHOUT bgm_id -> resets bgm_id=null, source=null, confidence=null; explicit null clears note & reason
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/admin/matches/60010")
                    .header("Authorization", admin_token())
                    .header("Content-Type", "application/json")
                    .body(Body::from(
                        r#"{"status":"pending","reason":null,"note":null}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "pending");
        assert_eq!(json["source"], serde_json::Value::Null);
        assert_eq!(json["confidence"], serde_json::Value::Null);
        assert_eq!(json["bgm_id"], serde_json::Value::Null);
        assert_eq!(json["reason"], serde_json::Value::Null);
        assert_eq!(json["note"], serde_json::Value::Null);
        assert_eq!(json["hydrated"], false);
    }

    #[tracing_test::traced_test]
    #[sqlx::test]
    async fn test_patch_match_hydrate_failure_is_non_fatal_and_reports_hydrated_false(
        pool: SqlitePool,
    ) {
        let mut bgm_server = mockito::Server::new_async().await;
        let _ep_mock = bgm_server
            .mock("GET", mockito::Matcher::Regex(r"^/v0/episodes".into()))
            .with_status(500)
            .create_async()
            .await;
        let _subj_mock = bgm_server
            .mock("GET", "/v0/subjects/515760")
            .with_status(500)
            .create_async()
            .await;

        seed_season_mal_and_match(&pool, 202601, 60020, "pending", None).await;

        let db = test_db(pool.clone());
        let app = admin_router(state_with_bgm_url(db, &bgm_server.url()));

        let resp = app
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/admin/matches/60020")
                    .header("Authorization", admin_token())
                    .header("Content-Type", "application/json")
                    .body(Body::from(r#"{"status":"included","bgm_id":515760}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "included");
        assert_eq!(json["source"], "human");
        assert_eq!(json["bgm_id"], 515760);
        assert_eq!(json["hydrated"], false);

        // Skeleton row still exists even though hydration failed
        let subject = SubjectRepository::new(&pool)
            .find_by_id(515760)
            .await
            .unwrap()
            .expect("skeleton subject should exist");
        assert!(subject.last_updated_at.is_none());

        assert!(logs_contain("hydrate_subject failed after patch_match"));
        assert!(logs_contain("mal_id"));
        assert!(logs_contain("bgm_id"));
    }

    // POST /admin/seasons/{season_id}/mal/{mal_id}/move
    #[sqlx::test]
    async fn test_move_season_mal_no_token_returns_401(pool: SqlitePool) {
        let app = admin_router(AppState::new(test_db(pool)));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/seasons/202510/mal/1001/move")
                    .header("Content-Type", "application/json")
                    .body(Body::from(r#"{"target_season_id":202601}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[sqlx::test]
    async fn test_move_season_mal_marks_source_removed_and_inserts_target_manual(pool: SqlitePool) {
        seed_season_mal_and_match(&pool, 202510, 1001, "included", Some(515759)).await;
        SeasonRepository::new(&pool)
            .upsert(CreateSeason {
                season_id: 202601,
                year: 2026,
                season: "WINTER".to_string(),
                name: None,
            })
            .await
            .unwrap();

        let db = test_db(pool.clone());
        let app = admin_router(AppState::new(Arc::clone(&db)));

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/seasons/202510/mal/1001/move")
                    .header("Authorization", admin_token())
                    .header("Content-Type", "application/json")
                    .body(Body::from(
                        r#"{"target_season_id":202601,"note":"跨季修正"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["moved"], true);
        assert_eq!(json["source"]["season_id"], 202510);
        assert_eq!(json["source"]["is_removed"], true);
        assert_eq!(json["source"]["note"], "跨季修正");
        assert_eq!(json["target"]["season_id"], 202601);
        assert_eq!(json["target"]["origin"], "manual");
        assert_eq!(json["target"]["is_removed"], false);
        assert_eq!(json["target"]["note"], "跨季修正");

        // 同季移动返回 400，未知目标季返回 404
        let same_resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/seasons/202601/mal/1001/move")
                    .header("Authorization", admin_token())
                    .header("Content-Type", "application/json")
                    .body(Body::from(r#"{"target_season_id":202601}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(same_resp.status(), StatusCode::BAD_REQUEST);

        let missing_target_resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/seasons/202601/mal/1001/move")
                    .header("Authorization", admin_token())
                    .header("Content-Type", "application/json")
                    .body(Body::from(r#"{"target_season_id":209901}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(missing_target_resp.status(), StatusCode::NOT_FOUND);
    }

    // GET /admin/export/season_mal & GET /admin/export/matches
    #[sqlx::test]
    async fn test_export_endpoints_require_admin_token(pool: SqlitePool) {
        let app = admin_router(AppState::new(test_db(pool)));
        for uri in ["/admin/export/season_mal", "/admin/export/matches"] {
            let resp = app
                .clone()
                .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "uri={uri}");
        }
    }

    #[sqlx::test]
    async fn test_export_endpoints_empty_body_when_no_rows(pool: SqlitePool) {
        let app = admin_router(AppState::new(test_db(pool)));
        for uri in ["/admin/export/season_mal", "/admin/export/matches"] {
            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(uri)
                        .header("Authorization", admin_token())
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK, "uri={uri}");
            assert_eq!(
                resp.headers()
                    .get(header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok()),
                Some("application/x-ndjson")
            );
            let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap();
            assert!(
                bytes.is_empty(),
                "empty DB must return empty body for {uri}"
            );
        }
    }

    #[sqlx::test]
    async fn test_export_season_mal_filters_orders_and_formats_ndjson(pool: SqlitePool) {
        // origin='mal', is_removed=0 -> must NOT appear in export
        seed_season_mal_and_match(&pool, 202601, 100, "included", Some(5001)).await;
        // origin='mal', is_removed=1 -> MUST appear
        seed_season_mal_and_match(&pool, 202601, 102, "excluded", None).await;
        SeasonMalRepository::new(&pool)
            .set_removed(202601, 102, true, Some("wrong season".to_string()))
            .await
            .unwrap();
        // origin='manual', is_removed=0 -> MUST appear (before 102 in 202601)
        MalAnimeRepository::new(&pool)
            .upsert(UpsertMalAnime {
                mal_id: 101,
                title: "MAL 101".to_string(),
                media_type: "tv".to_string(),
                ..Default::default()
            })
            .await
            .unwrap();
        SeasonMalRepository::new(&pool)
            .insert_manual(202601, 101, Some("manual add".to_string()))
            .await
            .unwrap();
        // earlier season 202510, manual -> MUST appear first
        SeasonRepository::new(&pool)
            .upsert(CreateSeason {
                season_id: 202510,
                year: 2025,
                season: "FALL".to_string(),
                name: None,
            })
            .await
            .unwrap();
        SeasonMalRepository::new(&pool)
            .insert_manual(202510, 101, None)
            .await
            .unwrap();

        let app = admin_router(AppState::new(test_db(pool)));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/admin/export/season_mal")
                    .header("Authorization", admin_token())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("application/x-ndjson")
        );

        let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = std::str::from_utf8(&body_bytes).unwrap();
        assert!(body.ends_with('\n'));
        let lines: Vec<serde_json::Value> = body
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(
            lines,
            vec![
                serde_json::json!({
                    "season_id": 202510,
                    "mal_id": 101,
                    "origin": "manual",
                    "is_removed": false,
                    "note": null
                }),
                serde_json::json!({
                    "season_id": 202601,
                    "mal_id": 101,
                    "origin": "manual",
                    "is_removed": false,
                    "note": "manual add"
                }),
                serde_json::json!({
                    "season_id": 202601,
                    "mal_id": 102,
                    "origin": "mal",
                    "is_removed": true,
                    "note": "wrong season"
                }),
            ]
        );
    }

    #[sqlx::test]
    async fn test_export_matches_orders_and_projects_exact_fields(pool: SqlitePool) {
        seed_season_mal_and_match(&pool, 202601, 200, "pending", None).await;
        seed_season_mal_and_match(&pool, 202601, 100, "included", Some(5001)).await;
        MalBgmMatchRepository::new(&pool)
            .upsert(
                100,
                UpdateMalBgmMatch {
                    status: "included".to_string(),
                    source: Some("exact".to_string()),
                    bgm_id: Some(5001),
                    confidence: Some(1.0),
                    candidates: Some("[{\"bgm_id\":5001}]".to_string()),
                    reason: None,
                    note: Some("exact match".to_string()),
                    error: Some("none".to_string()),
                },
            )
            .await
            .unwrap();

        let app = admin_router(AppState::new(test_db(pool)));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/admin/export/matches")
                    .header("Authorization", admin_token())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("application/x-ndjson")
        );

        let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = std::str::from_utf8(&body_bytes).unwrap();
        assert!(body.ends_with('\n'));
        let lines: Vec<serde_json::Value> = body
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);

        // Exact keys check (must not include candidates, error, updated_at)
        let first_obj = lines[0].as_object().unwrap();
        assert_eq!(first_obj.len(), 7);
        assert_eq!(
            lines,
            vec![
                serde_json::json!({
                    "mal_id": 100,
                    "status": "included",
                    "source": "exact",
                    "bgm_id": 5001,
                    "confidence": 1.0,
                    "reason": null,
                    "note": "exact match"
                }),
                serde_json::json!({
                    "mal_id": 200,
                    "status": "pending",
                    "source": null,
                    "bgm_id": null,
                    "confidence": null,
                    "reason": null,
                    "note": null
                }),
            ]
        );
    }
}
