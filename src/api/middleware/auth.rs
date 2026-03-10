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
    let path = req.uri().path().to_string();

    let auth_header = req
        .headers()
        .get("Authorization")
        .and_then(|v| v.to_str().ok());

    let provided = match auth_header {
        Some(h) if h.starts_with("Bearer ") => &h["Bearer ".len()..],
        _ => {
            // T025: 缺失 header 认证失败日志（不含 Token 明文）
            tracing::warn!(path = %path, reason = "missing_header", "auth failed");
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
        // T026: token 不匹配认证失败日志（禁止记录 provided 或 auth_header 值）
        tracing::warn!(path = %path, reason = "invalid_token", "auth failed");
        return (
            StatusCode::UNAUTHORIZED,
            Json(ErrorResponse {
                error: "Invalid token".to_string(),
            }),
        )
            .into_response();
    }

    // T027: 认证成功日志（仅记录路径）
    tracing::debug!(path = %path, "auth success");
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

    // T024 🔴 → 🟢 测试：验证认证失败时 WARN 日志触发，含 reason 字段，不含 Token 明文
    #[tokio::test]
    #[tracing_test::traced_test]
    async fn test_auth_failure_logs_warn_without_token() {
        dotenvy::dotenv().ok();

        // 测试 missing_header 场景
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
        assert!(logs_contain("missing_header"), "应包含 missing_header reason");

        // 测试 invalid_token 场景
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header("Authorization", "Bearer supersecret_xyz999")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        assert!(logs_contain("invalid_token"), "应包含 invalid_token reason");
        assert!(
            !logs_contain("supersecret_xyz999"),
            "日志中不应含 Token 明文"
        );
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
