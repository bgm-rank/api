mod core;
mod dal;
mod services;

use std::sync::Arc;

use crate::dal::db::Database;
use axum::{Json, Router, extract::State, routing::get};
use serde::Serialize;

#[derive(Serialize)]
struct HealthResponse {
    status: String,
    db: String,
}

async fn health_check(State(db): State<Arc<Database>>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".to_string(),
        db: match db.ping().await {
            Ok(true) => "ok".to_string(),
            Ok(false) => "error".to_string(),
            Err(e) => e.to_string(),
        },
    })
}

fn app(db: Arc<Database>) -> Router {
    Router::new()
        .route("/health", get(health_check))
        .with_state(db)
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

        // TODO: assert response body == {"status": "ok"}
    }
}
