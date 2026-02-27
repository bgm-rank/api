use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use std::sync::Arc;

use crate::core::SyncService;
use crate::dal::Database;

use super::schemas::{ErrorResponse, MessageResponse};

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Database>,
    pub sync_service: Arc<SyncService>,
}

impl AppState {
    pub fn new(db: Arc<Database>) -> Self {
        Self {
            sync_service: Arc::new(SyncService::new(Arc::clone(&db))),
            db,
        }
    }
}

pub async fn sync_season_handler(
    State(state): State<AppState>,
    Path(key): Path<String>,
) -> impl IntoResponse {
    match state.sync_service.sync_season(&key).await {
        Ok(_) => (
            StatusCode::OK,
            Json(MessageResponse {
                message: format!("{} 同步完成", key),
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
