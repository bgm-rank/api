use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    middleware,
    response::IntoResponse,
};
use serde::Serialize;
use std::sync::Arc;

use crate::core::AdminService;
use crate::core::SyncService;
use crate::core::query::QueryService;
use crate::core::scheduler::SchedulerHandle;
use crate::dal::Database;

use super::middleware::require_admin_token;
use super::schemas::{
    AcceptedResponse, CreateSeasonRequest, DeleteOrphansResponse, DeleteSeasonResponse,
    DeletedResponse, EditSeasonRequest, EditSubjectRequest, ErrorResponse, OrphanSubjectItem,
    RemovedResponse, SchedulerStatusResponse, SyncResultResponse,
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
        let admin_service = AdminService::new(Arc::clone(&db), handle.clone(), deploy_hook_url);
        Self {
            query_service: Arc::new(QueryService::new(Arc::clone(&db))),
            sync_service: Arc::new(SyncService::new(Arc::clone(&db))),
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

// ── Admin handlers ────────────────────────────────────────────────────────────

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

    match state
        .sync_service
        .create_and_sync(req.year, req.month, req.name)
        .await
    {
        Ok(result) => (
            StatusCode::CREATED,
            Json(SyncResultResponse {
                season_id: result.season_id,
                subjects_added: result.added,
                subjects_removed: result.removed,
                subjects_updated: result.updated,
                subjects_failed: result.failed,
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

pub async fn sync_season(
    State(state): State<AppState>,
    Path(season_id): Path<i32>,
) -> impl IntoResponse {
    match state.sync_service.resync(season_id).await {
        Ok(result) => Json(SyncResultResponse {
            season_id: result.season_id,
            subjects_added: result.added,
            subjects_removed: result.removed,
            subjects_updated: result.updated,
            subjects_failed: result.failed,
        })
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

// US7: DELETE /admin/seasons/{season_id}/subjects/{subject_id}
pub async fn remove_season_subject(
    State(state): State<AppState>,
    Path((season_id, subject_id)): Path<(i32, i32)>,
) -> impl IntoResponse {
    match state
        .admin_service
        .remove_subject_from_season(season_id, subject_id)
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
            "/admin/seasons/{season_id}/subjects/{subject_id}",
            delete(remove_season_subject),
        )
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
    use axum::{
        Router,
        body::Body,
        http::{Request, StatusCode},
        routing::get,
    };
    use tower::ServiceExt;

    async fn test_db() -> Arc<Database> {
        dotenvy::dotenv().ok();
        Arc::new(
            Database::new(&std::env::var("DATABASE_URL").unwrap())
                .await
                .unwrap(),
        )
    }

    fn admin_token() -> String {
        format!("Bearer {}", std::env::var("ADMIN_TOKEN").unwrap())
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

    // T011/T012 — GET /api/seasons, GET /api/seasons/{id}/subjects
    #[tokio::test]
    async fn test_list_seasons_returns_200() {
        let app = test_public_app(test_db().await);
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

    #[tokio::test]
    async fn test_get_season_subjects_404_for_unknown() {
        let app = test_public_app(test_db().await);
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
    #[tokio::test]
    async fn test_create_season_invalid_month_returns_400() {
        let app = admin_router(AppState::new(test_db().await));
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

    #[tokio::test]
    async fn test_create_season_no_token_returns_401() {
        let app = admin_router(AppState::new(test_db().await));
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
    #[tokio::test]
    async fn test_sync_season_no_token_returns_401() {
        let app = admin_router(AppState::new(test_db().await));
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

    #[tokio::test]
    async fn test_sync_season_unknown_id_returns_500() {
        let app = admin_router(AppState::new(test_db().await));
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
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    // T040 — GET /admin/subjects/orphans
    #[tokio::test]
    async fn test_list_orphans_no_token_returns_401() {
        let app = admin_router(AppState::new(test_db().await));
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

    #[tokio::test]
    async fn test_list_orphans_with_token_returns_200() {
        let app = admin_router(AppState::new(test_db().await));
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
    #[tokio::test]
    async fn test_delete_season_no_token_returns_401() {
        let app = admin_router(AppState::new(test_db().await));
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

    #[tokio::test]
    async fn test_delete_season_with_token_not_found_returns_404() {
        let app = admin_router(AppState::new(test_db().await));
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

    #[tokio::test]
    async fn test_delete_season_with_token_existing_returns_200() {
        let db = test_db().await;
        // 先创建 season
        {
            use crate::dal::{CreateSeason, SeasonRepository};
            let pool = db.pool();
            SeasonRepository::new(pool)
                .upsert(CreateSeason {
                    season_id: 202688,
                    year: 2026,
                    season: "FALL".to_string(),
                    name: None,
                })
                .await
                .unwrap();
        }
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
    #[tokio::test]
    async fn test_delete_orphans_no_token_returns_401() {
        let app = admin_router(AppState::new(test_db().await));
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

    #[tokio::test]
    async fn test_delete_orphans_with_token_returns_200() {
        let app = admin_router(AppState::new(test_db().await));
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
    #[tokio::test]
    async fn test_trigger_deploy_no_token_returns_401() {
        let app = admin_router(AppState::new(test_db().await));
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

    #[tokio::test]
    async fn test_trigger_deploy_with_valid_token_returns_202() {
        let app = admin_router(AppState::new(test_db().await));
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
    #[tokio::test]
    async fn test_sync_all_seasons_no_token_returns_401() {
        let app = admin_router(AppState::new(test_db().await));
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

    #[tokio::test]
    async fn test_sync_all_seasons_with_token_returns_202() {
        let app = admin_router(AppState::new(test_db().await));
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
    #[tokio::test]
    async fn test_trigger_scheduler_no_token_returns_401() {
        let app = admin_router(AppState::new(test_db().await));
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

    #[tokio::test]
    async fn test_trigger_scheduler_with_token_idle_returns_202() {
        let app = admin_router(AppState::new(test_db().await));
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

    #[tokio::test]
    async fn test_trigger_scheduler_already_running_returns_409() {
        use std::sync::atomic::Ordering;
        let db = test_db().await;
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
    #[tokio::test]
    async fn test_scheduler_status_no_token_returns_401() {
        let app = admin_router(AppState::new(test_db().await));
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

    #[tokio::test]
    async fn test_scheduler_status_with_token_returns_200_with_is_running_field() {
        let app = admin_router(AppState::new(test_db().await));
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
    #[tokio::test]
    async fn test_get_subject_admin_no_token_returns_401() {
        let app = admin_router(AppState::new(test_db().await));
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

    #[tokio::test]
    async fn test_get_subject_admin_not_found_returns_404() {
        let app = admin_router(AppState::new(test_db().await));
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

    #[tokio::test]
    async fn test_get_season_admin_not_found_returns_404() {
        let app = admin_router(AppState::new(test_db().await));
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
    #[tokio::test]
    async fn test_patch_subject_no_token_returns_401() {
        let app = admin_router(AppState::new(test_db().await));
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

    #[tokio::test]
    async fn test_patch_subject_not_found_returns_404() {
        let app = admin_router(AppState::new(test_db().await));
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
    #[tokio::test]
    async fn test_patch_season_no_token_returns_401() {
        let app = admin_router(AppState::new(test_db().await));
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

    #[tokio::test]
    async fn test_patch_season_not_found_returns_404() {
        let app = admin_router(AppState::new(test_db().await));
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
    #[tokio::test]
    async fn test_delete_subject_no_token_returns_401() {
        let app = admin_router(AppState::new(test_db().await));
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

    #[tokio::test]
    async fn test_delete_subject_not_found_returns_404() {
        let app = admin_router(AppState::new(test_db().await));
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

    // T063/T064 — DELETE /admin/seasons/{season_id}/subjects/{subject_id}
    #[tokio::test]
    async fn test_remove_season_subject_no_token_returns_401() {
        let app = admin_router(AppState::new(test_db().await));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/admin/seasons/202601/subjects/1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_remove_season_subject_not_found_returns_404() {
        let app = admin_router(AppState::new(test_db().await));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/admin/seasons/999999/subjects/999999")
                    .header("Authorization", admin_token())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
