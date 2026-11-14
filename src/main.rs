mod api;
mod core;
mod dal;
mod services;

use std::path::PathBuf;
use std::sync::Arc;

use crate::api::create_app_from_state;
use crate::api::endpoints::AppState;
use crate::core::import_season_data;
use crate::core::scheduler::{SchedulerHandle, SchedulerService};
use crate::dal::db::Database;
use tracing_subscriber::EnvFilter;

#[derive(Debug, PartialEq, Eq)]
struct ImportCliArgs {
    season_db: PathBuf,
    mal_json_dir: Option<PathBuf>,
}

fn take_flag_value(
    args: &[String],
    idx: &mut usize,
    flag: &str,
    target: &mut Option<PathBuf>,
) -> Result<bool, String> {
    let arg = args[*idx].as_str();
    let val = if arg == flag {
        let next = args
            .get(*idx + 1)
            .ok_or_else(|| format!("{} requires a path argument", flag))?;
        *idx += 2;
        next.as_str()
    } else if let Some(inline) = arg
        .strip_prefix(flag)
        .and_then(|rest| rest.strip_prefix('='))
    {
        *idx += 1;
        inline
    } else {
        return Ok(false);
    };

    if val.is_empty() {
        return Err(format!("{} path cannot be empty", flag));
    }
    if target.is_some() {
        return Err(format!("duplicate {} argument", flag));
    }
    *target = Some(PathBuf::from(val));
    Ok(true)
}

fn parse_import_args(args: &[String]) -> Result<ImportCliArgs, String> {
    let mut season_db: Option<PathBuf> = None;
    let mut mal_json_dir: Option<PathBuf> = None;
    let mut i = 0;

    while i < args.len() {
        if take_flag_value(args, &mut i, "--season-db", &mut season_db)?
            || take_flag_value(args, &mut i, "--mal-json-dir", &mut mal_json_dir)?
        {
            continue;
        }
        return Err(format!("unexpected argument: {}", args[i]));
    }

    let season_db = season_db.ok_or_else(|| {
        "missing required --season-db <path> (usage: bgm-rank-api import-season-data --season-db <path> [--mal-json-dir <dir>])".to_string()
    })?;

    Ok(ImportCliArgs {
        season_db,
        mal_json_dir,
    })
}

async fn run_import_cli(args: &[String]) -> i32 {
    let parsed = match parse_import_args(args) {
        Ok(a) => a,
        Err(msg) => {
            eprintln!("Argument error: {}", msg);
            return 1;
        }
    };

    let database_url = match std::env::var("DATABASE_URL") {
        Ok(v) if !v.trim().is_empty() => v,
        Ok(_) | Err(_) => {
            tracing::error!("DATABASE_URL is not set");
            eprintln!("Error: DATABASE_URL environment variable is required");
            return 1;
        }
    };

    let db = match Database::new(&database_url).await {
        Ok(db) => db,
        Err(e) => {
            tracing::error!(error = %e, "failed to connect to database");
            eprintln!("Error: failed to connect to database: {}", e);
            return 1;
        }
    };

    match import_season_data(&db, &parsed.season_db, parsed.mal_json_dir.as_deref()).await {
        Ok(summary) => {
            println!("Import completed: {}", summary);
            0
        }
        Err(e) => {
            tracing::error!(error = %format!("{:#}", e), "import-season-data failed");
            eprintln!("Import failed: {:#}", e);
            1
        }
    }
}

// T007 [US1]: 服务启动成功时记录 INFO 事件（含 addr 和 db_status 字段）
fn log_service_started(addr: &str) {
    tracing::info!(addr = %addr, db_status = "connected", "service started");
}

fn validate_admin_token(token: Option<&str>) -> Result<&str, &'static str> {
    match token {
        Some(t) if !t.trim().is_empty() => Ok(t),
        _ => Err("ADMIN_TOKEN environment variable is required and must not be empty"),
    }
}

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("import-season-data") {
        let code = run_import_cli(&args[2..]).await;
        if code != 0 {
            std::process::exit(code);
        }
        return;
    }

    let admin_token = std::env::var("ADMIN_TOKEN").ok();
    if let Err(msg) = validate_admin_token(admin_token.as_deref()) {
        tracing::error!("{msg}");
        eprintln!("Error: {msg}");
        std::process::exit(1);
    }

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

    // 创建 AppState（包含 SchedulerHandle）
    let state = AppState::new(Arc::clone(&db));
    let handle: SchedulerHandle = state.scheduler_handle.as_ref().clone();

    let scheduler =
        SchedulerService::new_with_deploy_hook(Arc::clone(&db), deploy_hook_url, handle);
    tokio::spawn(async move {
        if let Err(e) = scheduler.run().await {
            tracing::error!(error = %e, "Scheduler error");
        }
    });

    axum::serve(listener, create_app_from_state(state))
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
    use sqlx::SqlitePool;
    use tower::ServiceExt;

    #[sqlx::test]
    async fn test_health_check(pool: SqlitePool) {
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

    #[test]
    fn test_parse_import_args() {
        let to_vec = |items: &[&str]| items.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();

        let ok_min = parse_import_args(&to_vec(&["--season-db", "/tmp/season.db"])).unwrap();
        assert_eq!(
            ok_min,
            ImportCliArgs {
                season_db: PathBuf::from("/tmp/season.db"),
                mal_json_dir: None,
            }
        );

        let ok_both = parse_import_args(&to_vec(&[
            "--mal-json-dir=/tmp/mal",
            "--season-db=/tmp/season.db",
        ]))
        .unwrap();
        assert_eq!(
            ok_both,
            ImportCliArgs {
                season_db: PathBuf::from("/tmp/season.db"),
                mal_json_dir: Some(PathBuf::from("/tmp/mal")),
            }
        );

        assert!(parse_import_args(&to_vec(&[])).is_err());
        assert!(parse_import_args(&to_vec(&["--season-db"])).is_err());
        assert!(parse_import_args(&to_vec(&["--season-db="])).is_err());
        assert!(parse_import_args(&to_vec(&["--season-db", "a", "--unknown"])).is_err());
        assert!(parse_import_args(&to_vec(&["--season-db", "a", "--season-db", "b"])).is_err());
    }

    #[test]
    fn test_validate_admin_token_rejects_missing_and_empty() {
        assert!(validate_admin_token(None).is_err());
        assert!(validate_admin_token(Some("")).is_err());
        assert!(validate_admin_token(Some("   ")).is_err());
        assert_eq!(
            validate_admin_token(Some("secret-token")),
            Ok("secret-token")
        );
    }
}
