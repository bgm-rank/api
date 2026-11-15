pub mod endpoints;
pub mod middleware;
pub mod schemas;

#[allow(unused_imports)]
pub use endpoints::*;
#[allow(unused_imports)]
pub use schemas::*;

use std::sync::Arc;

use axum::{
    Router,
    http::{HeaderName, Method, header},
    routing::get,
};
use tower_http::{
    cors::{Any, CorsLayer},
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    trace::TraceLayer,
};

use crate::dal::Database;

use endpoints::{
    AppState, admin_router, get_all_seasons_top1, get_current_season, get_season_subjects,
    health_check, list_seasons,
};

/// 构建带有完整中间件栈的应用 Router（从已有 AppState 构建）
pub fn create_app_from_state(state: AppState) -> Router {
    let admin = admin_router(state.clone());
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PATCH,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([
            header::AUTHORIZATION,
            header::CONTENT_TYPE,
            header::ACCEPT,
            HeaderName::from_static("x-request-id"),
        ])
        .expose_headers([HeaderName::from_static("x-request-id")]);

    Router::new()
        .route("/health", get(health_check))
        .route("/api/seasons", get(list_seasons))
        .route("/api/seasons/current", get(get_current_season))
        .route("/api/seasons/top1", get(get_all_seasons_top1))
        .route(
            "/api/seasons/{season_id}/subjects",
            get(get_season_subjects),
        )
        .with_state(state)
        .merge(admin)
        .layer(cors)
        // T009: 生成 UUID request_id 并写入 x-request-id header，复制到响应
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(
            // T010: TraceLayer 从 x-request-id 提取并注入 span
            TraceLayer::new_for_http().make_span_with(
                |request: &axum::http::Request<axum::body::Body>| {
                    let request_id = request
                        .headers()
                        .get("x-request-id")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("unknown")
                        .to_owned();
                    let method = request.method().to_string();
                    let path = request.uri().path().to_owned();
                    let client_ip = request
                        .headers()
                        .get("x-forwarded-for")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.split(',').next())
                        .map(|s| s.trim())
                        .unwrap_or("unknown")
                        .to_owned();
                    // T011: status_code 和 elapsed_ms 由 TraceLayer on_response 自动记录
                    tracing::info_span!(
                        "http_request",
                        request_id = %request_id,
                        method = %method,
                        path = %path,
                        client_ip = %client_ip,
                    )
                },
            ),
        )
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
}

/// 构建带有完整中间件栈的应用 Router
#[allow(dead_code)]
pub fn create_app(db: Arc<Database>) -> Router {
    create_app_from_state(AppState::new(db))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::{body::Body, http::Request};
    use sqlx::SqlitePool;
    use tower::ServiceExt;

    use crate::dal::Database;

    // T006 [US1]: 发送 HTTP 请求，断言响应包含 x-request-id header
    #[sqlx::test]
    async fn test_response_has_request_id_header(pool: SqlitePool) {
        let db = Arc::new(Database::from_pool(pool));
        let app = super::create_app(db);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert!(
            resp.headers().contains_key("x-request-id"),
            "响应中应包含 x-request-id header"
        );
    }

    #[sqlx::test]
    async fn test_cors_preflight_on_admin_route_succeeds_without_auth(pool: SqlitePool) {
        let db = Arc::new(Database::from_pool(pool));
        let app = super::create_app(db);

        let resp = app
            .oneshot(
                Request::builder()
                    .method("OPTIONS")
                    .uri("/admin/matches")
                    .header("Origin", "http://localhost:5173")
                    .header("Access-Control-Request-Method", "PATCH")
                    .header(
                        "Access-Control-Request-Headers",
                        "authorization,content-type",
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        assert!(
            resp.headers().contains_key("access-control-allow-origin"),
            "CORS preflight response must include access-control-allow-origin"
        );
    }
}
