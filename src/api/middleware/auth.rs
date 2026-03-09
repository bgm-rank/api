use axum::{
    Json,
    body::Body,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};

use crate::api::schemas::ErrorResponse;

pub async fn require_admin_token(req: Request<Body>, next: Next) -> Response {
    let token = std::env::var("ADMIN_TOKEN").unwrap_or_default();

    let auth_header = req
        .headers()
        .get("Authorization")
        .and_then(|v| v.to_str().ok());

    let provided = match auth_header {
        Some(h) if h.starts_with("Bearer ") => &h["Bearer ".len()..],
        _ => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(ErrorResponse {
                    error: "Missing or invalid Authorization header".to_string(),
                }),
            )
                .into_response();
        }
    };

    if provided != token {
        return (
            StatusCode::UNAUTHORIZED,
            Json(ErrorResponse {
                error: "Invalid token".to_string(),
            }),
        )
            .into_response();
    }

    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Router,
        body::Body,
        http::{Request, StatusCode},
        middleware,
        routing::get,
    };
    use tower::ServiceExt;

    async fn dummy_handler() -> &'static str {
        "ok"
    }

    fn test_app() -> Router {
        Router::new()
            .route("/protected", get(dummy_handler))
            .layer(middleware::from_fn(require_admin_token))
    }

    // T016 🔴 → T017 🟢 测试
    #[tokio::test]
    async fn test_no_auth_header_returns_401() {
        dotenvy::dotenv().ok();
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_wrong_token_returns_401() {
        dotenvy::dotenv().ok();
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header("Authorization", "Bearer wrongtoken_xyz123")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_correct_token_passes() {
        dotenvy::dotenv().ok();
        let token = std::env::var("ADMIN_TOKEN").unwrap_or_else(|_| "testtoken".to_string());
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header("Authorization", format!("Bearer {}", token))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
