mod api;
mod core;
mod dal;
mod services;

use std::path::PathBuf;
use std::sync::Arc;

use crate::api::create_app_from_state;
use crate::api::endpoints::AppState;
use crate::core::fetch::{MalFetchService, parse_season_id};
use crate::core::import_season_data;
use crate::core::matcher::MatcherService;
use crate::core::scheduler::{SchedulerHandle, SchedulerService};
use crate::core::sync::SyncService;
use crate::dal::db::Database;
use crate::services::llm::{LlmClient, LlmMatcher};
use crate::services::mal::MalClient;
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

#[derive(Debug, PartialEq, Eq)]
struct FetchMalCliArgs {
    from: i32,
    to: i32,
}

fn take_season_value(
    args: &[String],
    idx: &mut usize,
    flag: &str,
    target: &mut Option<i32>,
) -> Result<bool, String> {
    let mut raw: Option<PathBuf> = None;
    if !take_flag_value(args, idx, flag, &mut raw)? {
        return Ok(false);
    }
    if target.is_some() {
        return Err(format!("duplicate {} argument", flag));
    }
    let raw = raw.unwrap_or_default();
    let raw = raw.to_string_lossy();
    let id: i32 = raw
        .parse()
        .map_err(|_| format!("{} must be a season id like 202601, got '{}'", flag, raw))?;
    parse_season_id(id).map_err(|e| e.to_string())?;
    *target = Some(id);
    Ok(true)
}

fn parse_fetch_mal_args(args: &[String]) -> Result<FetchMalCliArgs, String> {
    const USAGE: &str =
        "usage: bgm-rank-api fetch-mal --season <202601> | --from <202510> --to <202601>";
    let (mut season, mut from, mut to) = (None, None, None);
    let mut i = 0;

    while i < args.len() {
        if take_season_value(args, &mut i, "--season", &mut season)?
            || take_season_value(args, &mut i, "--from", &mut from)?
            || take_season_value(args, &mut i, "--to", &mut to)?
        {
            continue;
        }
        return Err(format!("unexpected argument: {}", args[i]));
    }

    match (season, from, to) {
        (Some(s), None, None) => Ok(FetchMalCliArgs { from: s, to: s }),
        (None, Some(f), Some(t)) if f <= t => Ok(FetchMalCliArgs { from: f, to: t }),
        (None, Some(_), Some(_)) => Err("--from must not be after --to".to_string()),
        _ => Err(format!(
            "specify either --season, or both --from and --to ({})",
            USAGE
        )),
    }
}

async fn run_fetch_mal_cli(args: &[String]) -> i32 {
    let parsed = match parse_fetch_mal_args(args) {
        Ok(a) => a,
        Err(msg) => {
            eprintln!("Argument error: {}", msg);
            return 1;
        }
    };

    let client = match MalClient::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error: {:#}", e);
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
        Ok(db) => Arc::new(db),
        Err(e) => {
            tracing::error!(error = %e, "failed to connect to database");
            eprintln!("Error: failed to connect to database: {}", e);
            return 1;
        }
    };

    let service = MalFetchService::new(db, client);
    match service.fetch_range(parsed.from, parsed.to).await {
        Ok(summary) => {
            println!("fetch-mal completed: {}", summary);
            0
        }
        Err(e) => {
            tracing::error!(error = %format!("{:#}", e), "fetch-mal failed");
            eprintln!("fetch-mal failed: {:#}", e);
            1
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum MatchTarget {
    Season(i32),
    MalId(i32),
}

#[derive(Debug, PartialEq, Eq)]
struct MatchCliArgs {
    target: MatchTarget,
    retry: bool,
}

fn parse_match_args(args: &[String]) -> Result<MatchCliArgs, String> {
    const USAGE: &str =
        "usage: bgm-rank-api match --season <202601> [--retry] | --mal-id <id> [--retry]";
    let (mut season, mut mal_id) = (None, None);
    let mut retry = false;
    let mut i = 0;

    while i < args.len() {
        if take_season_value(args, &mut i, "--season", &mut season)? {
            continue;
        }
        let mut raw: Option<PathBuf> = None;
        if take_flag_value(args, &mut i, "--mal-id", &mut raw)? {
            if mal_id.is_some() {
                return Err("duplicate --mal-id argument".to_string());
            }
            let raw = raw.unwrap_or_default();
            let raw = raw.to_string_lossy();
            let id: i32 = raw
                .parse()
                .ok()
                .filter(|id| *id > 0)
                .ok_or_else(|| format!("--mal-id must be a positive integer, got '{}'", raw))?;
            mal_id = Some(id);
            continue;
        }
        if args[i] == "--retry" {
            retry = true;
            i += 1;
            continue;
        }
        return Err(format!("unexpected argument: {}", args[i]));
    }

    let target = match (season, mal_id) {
        (Some(s), None) => MatchTarget::Season(s),
        (None, Some(m)) => MatchTarget::MalId(m),
        _ => {
            return Err(format!(
                "specify exactly one of --season or --mal-id ({})",
                USAGE
            ));
        }
    };
    Ok(MatchCliArgs { target, retry })
}

async fn run_match_cli(args: &[String]) -> i32 {
    let parsed = match parse_match_args(args) {
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
        Ok(db) => Arc::new(db),
        Err(e) => {
            tracing::error!(error = %e, "failed to connect to database");
            eprintln!("Error: failed to connect to database: {}", e);
            return 1;
        }
    };

    let llm: Option<Arc<dyn LlmMatcher>> = match LlmClient::from_env() {
        Ok(Some(c)) => Some(Arc::new(c)),
        Ok(None) => {
            tracing::warn!(
                "neither DEEPSEEK_API_KEY nor OPENROUTER_API_KEY is set; LLM layer disabled"
            );
            None
        }
        Err(e) => {
            eprintln!("Error: {:#}", e);
            return 1;
        }
    };

    let sync = Arc::new(SyncService::new(Arc::clone(&db)));
    let service = MatcherService::new(db, sync, llm);
    let result = match parsed.target {
        MatchTarget::Season(id) => service
            .match_season(id, parsed.retry)
            .await
            .map(|s| s.to_string()),
        MatchTarget::MalId(id) => service
            .match_mal(id, parsed.retry)
            .await
            .map(|o| format!("{:?}", o)),
    };
    match result {
        Ok(summary) => {
            println!("match completed: {}", summary);
            0
        }
        Err(e) => {
            tracing::error!(error = %format!("{:#}", e), "match failed");
            eprintln!("match failed: {:#}", e);
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

    if args.get(1).map(String::as_str) == Some("fetch-mal") {
        let code = run_fetch_mal_cli(&args[2..]).await;
        if code != 0 {
            std::process::exit(code);
        }
        return;
    }

    if args.get(1).map(String::as_str) == Some("match") {
        let code = run_match_cli(&args[2..]).await;
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
    fn test_parse_fetch_mal_args() {
        let v = |items: &[&str]| items.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        assert_eq!(
            parse_fetch_mal_args(&v(&["--season", "202601"])).unwrap(),
            FetchMalCliArgs {
                from: 202601,
                to: 202601
            }
        );
        assert_eq!(
            parse_fetch_mal_args(&v(&["--from=202510", "--to", "202604"])).unwrap(),
            FetchMalCliArgs {
                from: 202510,
                to: 202604
            }
        );
        assert!(parse_fetch_mal_args(&v(&[])).is_err());
        assert!(parse_fetch_mal_args(&v(&["--season", "202602"])).is_err());
        assert!(parse_fetch_mal_args(&v(&["--season", "abc"])).is_err());
        assert!(parse_fetch_mal_args(&v(&["--from", "202604", "--to", "202601"])).is_err());
        assert!(parse_fetch_mal_args(&v(&["--from", "202601"])).is_err());
        assert!(parse_fetch_mal_args(&v(&["--season", "202601", "--to", "202604"])).is_err());
    }

    #[test]
    fn test_parse_match_args() {
        let v = |items: &[&str]| items.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        assert_eq!(
            parse_match_args(&v(&["--season", "202601", "--retry"])).unwrap(),
            MatchCliArgs {
                target: MatchTarget::Season(202601),
                retry: true
            }
        );
        assert_eq!(
            parse_match_args(&v(&["--mal-id=42"])).unwrap(),
            MatchCliArgs {
                target: MatchTarget::MalId(42),
                retry: false
            }
        );
        assert_eq!(
            parse_match_args(&v(&["--mal-id", "42", "--retry"])).unwrap(),
            MatchCliArgs {
                target: MatchTarget::MalId(42),
                retry: true
            }
        );
        assert!(parse_match_args(&v(&[])).is_err());
        assert!(parse_match_args(&v(&["--season", "202602"])).is_err());
        assert!(parse_match_args(&v(&["--mal-id", "0"])).is_err());
        assert!(parse_match_args(&v(&["--mal-id", "x"])).is_err());
        assert!(parse_match_args(&v(&["--season", "202601", "--mal-id", "1"])).is_err());
        assert!(parse_match_args(&v(&["--season", "202601", "--bogus"])).is_err());
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
