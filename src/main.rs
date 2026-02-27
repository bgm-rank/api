mod api;
mod core;
mod dal;
mod services;

use std::sync::Arc;

use crate::api::endpoints::{AppState, sync_season_handler};
use crate::dal::db::Database;
use axum::{Json, Router, extract::State, routing::{get, post}};
use serde::Serialize;

#[derive(Serialize)]
struct HealthResponse {
    status: String,
    db: String,
}

async fn health_check(State(state): State<AppState>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".to_string(),
        db: match state.db.ping().await {
            Ok(true) => "ok".to_string(),
            Ok(false) => "error".to_string(),
            Err(e) => e.to_string(),
        },
    })
}

fn app(db: Arc<Database>) -> Router {
    let state = AppState::new(db);
    Router::new()
        .route("/health", get(health_check))
        .route("/admin/sync/{key}", post(sync_season_handler))
        .with_state(state)
}

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

    let addr = "0.0.0.0:3000";
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("无法绑定端口");

    let database_url = std::env::var("DATABASE_URL").unwrap();
    let db = Database::new(&database_url).await.unwrap();
    let db = Arc::new(db);

    axum::serve(listener, app(db))
        .await
        .expect("服务器运行错误");
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    #[tokio::test]
    async fn test_health_check() {
        dotenvy::dotenv().ok();
        let database_url = std::env::var("DATABASE_URL").unwrap();
        let db = Database::new(&database_url).await.unwrap();
        let db = Arc::new(db);

        let app = app(db);

        let request = Request::builder()
            .uri("/health")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }
}
