mod api;
mod core;
mod dal;
mod services;

use std::sync::Arc;

use crate::core::scheduler::SchedulerService;
use crate::dal::db::Database;
use tracing_subscriber::EnvFilter;

// T007 [US1]: 服务启动成功时记录 INFO 事件（含 addr 和 db_status 字段）
fn log_service_started(addr: &str) {
    tracing::info!(addr = %addr, db_status = "connected", "service started");
}

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let addr = "0.0.0.0:3000";
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("无法绑定端口");

    let database_url = std::env::var("DATABASE_URL").unwrap();
    // T008 [US1]: 数据库连接失败时记录 ERROR 事件
    let db = match Database::new(&database_url).await {
        Ok(db) => db,
        Err(e) => {
            tracing::error!(error = %e, "failed to connect to database");
            std::process::exit(1);
        }
    };
    let db = Arc::new(db);

    // T007 [US1]: 数据库连接成功后记录启动日志
    log_service_started(addr);

    let deploy_hook_url = std::env::var("DEPLOY_HOOK_URL").ok();
    let scheduler = SchedulerService::new_with_deploy_hook(Arc::clone(&db), deploy_hook_url);
    tokio::spawn(async move {
        if let Err(e) = scheduler.run().await {
            tracing::error!(error = %e, "Scheduler error");
        }
    });

    axum::serve(listener, api::create_app(db))
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
    use sqlx::PgPool;
    use tower::ServiceExt;

    #[sqlx::test]
    async fn test_health_check(pool: PgPool) {
        let db = Arc::new(Database::from_pool(pool));
        let app = api::create_app(db);

        let request = Request::builder()
            .uri("/health")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    // T005 [US1]: 验证服务启动时 INFO 事件包含 addr 和 db_status="connected"
    #[tracing_test::traced_test]
    #[test]
    fn test_service_startup_log_contains_addr_and_db_status() {
        log_service_started("0.0.0.0:3000");
        assert!(logs_contain("addr"));
        assert!(logs_contain("db_status"));
        assert!(logs_contain("connected"));
    }
}
