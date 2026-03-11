use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    middleware,
    response::IntoResponse,
};
use serde::Serialize;
use std::sync::Arc;

use crate::core::SyncService;
use crate::core::query::QueryService;
use crate::dal::Database;

use super::middleware::require_admin_token;
use super::schemas::{
    CreateSeasonRequest, DeleteOrphansResponse, DeleteSeasonResponse, ErrorResponse,
    OrphanSubjectItem, SyncResultResponse,
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
}

impl AppState {
    pub fn new(db: Arc<Database>) -> Self {
        Self {
            query_service: Arc::new(QueryService::new(Arc::clone(&db))),
            sync_service: Arc::new(SyncService::new(Arc::clone(&db))),
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

pub fn admin_router(state: AppState) -> axum::Router {
    use axum::routing::{delete, get, post};
    axum::Router::new()
        .route("/admin/seasons", post(create_season))
        .route("/admin/seasons/{season_id}", delete(delete_season))
        .route("/admin/seasons/{season_id}/sync", post(sync_season))
        .route("/admin/subjects/orphans", get(list_orphan_subjects))
        .route("/admin/subjects/orphans", delete(delete_orphan_subjects))
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
}
