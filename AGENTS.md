# AGENTS.md — `bgm-rank-api` 架构与开发指南

## 1. 项目概述

`bgm-rank-api` 是 Bangumi 季度新番排行榜的后端 API 服务，采用 Rust (Edition 2024) 构建。核心职责包括：
1. 管理 MAL 季度番剧索引、MAL-BGM 匹配关系与 [Bangumi API (`api.bgm.tv`)](https://api.bgm.tv) 条目详情及剧集数据，计算派生指标（如弃番率、剧集平均评论数、放送星期等）并持久化到本地 SQLite 数据库。
2. 通过内置定时调度器按季度新鲜度分级刷新番剧数据，并在同步完成后触发前端部署 Webhook（如 Cloudflare Pages）。
3. 面向前端提供低延迟的公开只读 API，面向管理员提供带 Bearer Token 鉴权的管理与同步接口（支持 SSE 进度流）。

### 核心技术栈
- **语言与运行时**: Rust 2024 Edition, `tokio` (异步运行时)
- **Web 框架**: `axum` 0.8 + `tower-http` (`trace`, `request-id`)
- **数据库与 ORM**: `sqlx` 0.8 + SQLite (启用 WAL 模式、外键约束、启动时自动执行 `migrations/`)
- **HTTP 客户端**: `reqwest` 0.12 (JSON 序列化、自定义 User-Agent 与 Bearer Token)
- **可观测性**: `tracing` + `tracing-subscriber` (`EnvFilter`)
- **测试工具链**: `#[sqlx::test]`, `mockito`, `tower::ServiceExt::oneshot`, `tracing-test`

---

## 2. 目录结构

```text
api
├── Cargo.toml                  # 依赖与包配置
├── Dockerfile                  # 多阶段容器镜像构建
├── compose.yml                 # Docker Compose 编排配置
├── .env.example                # 环境变量模板
├── migrations/                 # SQLx 自动迁移脚本
└── src/
    ├── main.rs                 # 程序入口：CLI 子命令分发 (fetch-mal / match / import-season-data)、环境初始化、DB 连接、调度器后台任务、HTTP Server 启动
    ├── api/                    # 接口层 (HTTP Routing, Handlers, Middleware, Schemas)
    │   ├── mod.rs              # Router 组装与中间件层 (RequestId, TraceLayer)
    │   ├── endpoints.rs        # Public 与 Admin 路由 Handler 实现及 AppState 定义
    │   ├── schemas.rs          # HTTP 请求/响应 DTO 定义
    │   └── middleware/
    │       ├── mod.rs
    │       └── auth.rs         # 管理员 Bearer Token 鉴权中间件 (`require_admin_token`)
    ├── core/                   # 核心业务逻辑层 (Domain Services)
    │   ├── mod.rs
    │   ├── fetch/mod.rs        # MalFetchService：MAL 季度新番抓取与入库（仅收录当季开播新番）
    │   ├── matcher/mod.rs      # MatcherService：MAL→BGM 四层匹配管线（规则/搜索/精确/LLM）
    │   ├── import/mod.rs       # ImportService (`import_season_data`)：从旧 season.db 一次性迁移数据
    │   ├── query/mod.rs        # QueryService：公开接口的数据查询与聚合组装
    │   ├── sync/mod.rs         # SyncService：番剧拉取、指标计算与 SSE 同步流
    │   ├── scheduler/mod.rs    # SchedulerService：定时与手动触发的后台调度器
    │   └── admin/mod.rs        # AdminService：管理端操作（部署触发、调度控制、匹配审核、JSONL 导出、条目/季度 CRUD）
    ├── dal/                    # 数据访问层 (Data Access Layer)
    │   ├── mod.rs
    │   ├── db.rs               # Database 封装 (SqlitePool, WAL, Migrate, Ping)
    │   ├── dto/                # 数据库表结构映射实体 (FromRow)、写入/更新 DTO 与导出行 DTO
    │   │   ├── mod.rs
    │   │   ├── mal_anime.rs
    │   │   ├── mal_bgm_match.rs # 含 MalBgmMatch、UpdateMalBgmMatch、MalBgmMatchExportRow
    │   │   ├── season.rs
    │   │   ├── season_mal.rs    # 含 SeasonMal、SeasonMalExportRow
    │   │   └── subject.rs
    │   └── repositories/       # 仓储实现：封装所有原始 SQL 操作（含导出查询）
    │       ├── mod.rs
    │       ├── mal_anime_repository.rs
    │       ├── mal_bgm_match_repository.rs # 含 list_for_export 导出查询
    │       ├── season_mal_repository.rs    # 含 list_for_export 导出查询
    │       ├── season_repository.rs
    │       └── subject_repository.rs
    └── services/               # 外部服务客户端层
        ├── mod.rs
        ├── bangumi/            # Bangumi v0 API 客户端 (`GET /v0/subjects/{id}`, `GET /v0/episodes`, `POST /v0/search/subjects`)
        ├── mal/                # MAL v2 API 客户端 (`MalClient`：`GET /v2/anime/season/{year}/{season}`)
        ├── llm/                # OpenAI 兼容 LLM 客户端 (`LlmClient`，trait `LlmMatcher`：`match_anime` / `suggest_search`)
        └── deploy_hook/        # 外部部署 Webhook 触发客户端 (`DeployHookClient`)
```

---

## 3. 四层架构与职责边界

项目严格遵循自顶向下的四层架构设计（入口由 `src/main.rs` 负责 CLI 子命令分发：当首个参数为 `import-season-data` 时直接执行一次性导入后退出，否则启动后台调度器与 HTTP Server）：

1. **API 层 (`src/api/`)**
   - 仅负责 HTTP 协议转换（路径参数、JSON Body / NDJSON 导出响应构建、HTTP 状态码映射、SSE 事件流包装）、请求级追踪 (`x-request-id`) 和权限校验。
   - **禁止**在 Handler 中直接编写 SQL 或直接调用 `src/services/` 中的外部 HTTP 客户端，所有操作必须委托给 `AppState` 中的 `core` 服务。
2. **Core 业务层 (`src/core/`)**
   - **`QueryService`**: 处理公开查询，包含季度列表、当前季度判定、各季度 Top 1 番剧提取（仅 `tv` 且排除 `kids`/`r18`）、季度下属番剧按 `rank ASC NULLS LAST` 排序（含 `N:1` 映射去重及 `map_mal_rating` 分级映射）等逻辑。
   - **`SyncService`**: 核心同步引擎。使用 `BangumiClient` 按 `season_mal (is_removed = 0)` 与 `mal_bgm_matches (status = 'included')` 关联拉取番剧元信息与本篇剧集列表，计算派生字段：
     - `collection_total`: `wish + collect + doing + on_hold + dropped`
     - `drop_rate`: `dropped / collection_total`
     - `average_comment`: 已放送正片剧集的评论数均值
     - `air_weekday`: 从 Infobox 中提取放送星期
     - 支持通过 `mpsc::Sender<SyncProgressEvent>` 推送实时同步进度。
   - **`SchedulerService`**: 在 UTC+8 的 `0, 4, 8, 12, 16, 20` 整点唤醒（或通过 `SchedulerHandle` 手动唤醒）。每次 Tick 根据季度距离（`age`）按不同时间间隔刷新到期番剧，最后触发 Deploy Hook（手动触发除外）。使用 `AtomicBool` 防止重入并发。
   - **`MalFetchService`** (`core/fetch`): `fetch_season(season_id)` / `fetch_range(from, to)`，拉取 MAL 季度列表并按 `is_new_anime` 过滤非当季开播的续播/长篇番剧，幂等写入 `mal_anime`、`season_mal`（`ON CONFLICT DO NOTHING`）与 `mal_bgm_matches` 初始 `pending` 行。
   - **`MatcherService`** (`core/matcher`): `match_mal(mal_id, retry)` / `match_season(season_id, retry)`，按 `docs/drop-season-data-analysis.md` §2 的四层管线写 `mal_bgm_matches`（已 included/excluded 短路；韩文标题只允许精确匹配且不调用 LLM；精确匹配与 LLM `confidence >= 0.85` 均需通过本地 `enrich` 后的 `match_conflicts` 硬规则校验才自动 included 并按需触发 `hydrate_subject`）。LLM 通过 `LlmMatcher` trait 注入，测试使用 fake。
   - **`AdminService`**: 封装管理员对季度、番剧、匹配审核、`season_mal` 跨季移动的修改/删除，JSONL 导出（`export_season_mal_jsonl`、`export_matches_jsonl`），以及对调度器和 Deploy Hook 的控制。
   - **`import` 模块 (`import_season_data`)**: 供 `main.rs` 的一次性 `import-season-data` CLI 子命令调用，在单事务中从旧 `season.db`（及可选原始 MAL JSON 目录）幂等迁移 `seasons`、`subjects` 骨架、`mal_anime`、`season_mal` 与 `mal_bgm_matches`。
3. **DAL 数据访问层 (`src/dal/`)**
   - `Database` 负责管理 `SqlitePool`。
   - `SeasonRepository`、`SubjectRepository`、`MalAnimeRepository`、`SeasonMalRepository`、`MalBgmMatchRepository` 封装针对 `seasons`、`subjects`、`mal_anime`、`season_mal`、`mal_bgm_matches` 五张表的所有 SQL 查询；其中 `SeasonMalRepository::list_for_export` 与 `MalBgmMatchRepository::list_for_export` 分别投影导出 `SeasonMalExportRow` 与 `MalBgmMatchExportRow`。
   - 数据库错误通过统一的 `log_db_error` 按错误类型分级记录日志（`RowNotFound` -> `debug`, 唯一约束冲突 -> `warn`, 其他 -> `error`）。
4. **Services 外部客户端层 (`src/services/`)**
   - 封装对第三方 HTTP 服务的请求、超时/重定向策略与响应反序列化。
   - 所有客户端均提供注入自定义 URL 的构造方法（如 `BangumiClient::with_base_url`、`DeployHookClient::new`），以便在单元测试中配合 `mockito` 拦截请求。

---

## 4. API 路由全景

### 公开接口 (Public API)
| 方法 | 路径 | 说明 |
| :--- | :--- | :--- |
| `GET` | `/health` | 健康检查，返回服务与数据库连接状态 |
| `GET` | `/api/seasons` | 获取所有已收录季度列表 |
| `GET` | `/api/seasons/current` | 获取当前最新季度信息 |
| `GET` | `/api/seasons/top1` | 获取每个季度的评分/排名第一番剧 |
| `GET` | `/api/seasons/{season_id}/subjects` | 获取指定季度（如 `202601`）的番剧排行榜 |

### 管理员接口 (Admin API)
> 所有 `/admin/*` 路由均经过 `require_admin_token` 中间件保护，需携带请求头 `Authorization: Bearer <ADMIN_TOKEN>`。

| 方法 | 路径 | 说明 |
| :--- | :--- | :--- |
| `POST` | `/admin/seasons` | 创建并同步新季度（仅允许月份 `1, 4, 7, 10`），返回 **SSE (`text/event-stream`)** 实时进度事件 |
| `POST` | `/admin/seasons/sync-all` | 后台异步重同步所有季度 |
| `GET` | `/admin/seasons/{season_id}` | 获取单个季度详情 |
| `PATCH` | `/admin/seasons/{season_id}` | 修改季度信息（如自定义名称） |
| `DELETE` | `/admin/seasons/{season_id}` | 删除指定季度及其关联关系 |
| `POST` | `/admin/seasons/{season_id}/sync` | 重同步指定季度，返回 **SSE (`text/event-stream`)** 实时进度事件 |
| `POST` | `/admin/seasons/{season_id}/fetch-mal` | 抓取指定季度的 MAL 新番并幂等入库 `mal_anime` / `season_mal` / `mal_bgm_matches` |
| `POST` | `/admin/seasons/{season_id}/match` | 执行指定季度的自动匹配管线（支持 `?retry=true`），返回 **SSE (`text/event-stream`)** 进度事件 |
| `POST` | `/admin/seasons/{season_id}/mal` | 手动向指定季度补入单个 `mal_id`（若本地无记录则调用 MAL API 抓取后写入 `origin='manual'`） |
| `DELETE` | `/admin/seasons/{season_id}/mal/{mal_id}` | 标记某季度的 `season_mal` 行 `is_removed = 1` |
| `POST` | `/admin/seasons/{season_id}/mal/{mal_id}/move` | 将 `mal_id` 从当前季度移至目标季度（源季标记 `is_removed = 1`，目标季写入 `manual`） |
| `POST` | `/admin/seasons/{season_id}/mal/{mal_id}/restore` | 恢复某季度被软移除的 `season_mal` 行（`is_removed = 0`，行不存在返回 `404`） |
| `GET` | `/admin/matches` | 查询匹配记录列表（支持可选 `?season_id=`（关联 `season_mal.is_removed = 0`）与 `?status=` 过滤；返回匹配字段、`mal_anime` 完整对比字段及已绑定 `subject` 摘要） |
| `POST` | `/admin/matches/rerun` | 批量将指定 `mal_ids` 打回 `pending` 并重跑自动匹配管线，返回 **SSE (`text/event-stream`)** 进度事件 |
| `PATCH` | `/admin/matches/{mal_id}` | 人工更新匹配状态/绑定 `bgm_id`（`bgm_id > 0` 时确保骨架行；`included`/`excluded` 设 `source = 'human'`，`pending` 重置 `source`/`confidence`/`bgm_id`；`included` 时非致命触发 `hydrate_subject` 并返回 `hydrated: bool`） |
| `GET` | `/admin/bgm/search` | 按关键词实时搜索 Bangumi 动画条目（`?q=`），附带库内已有 `mal_bgm_matches` 关联列表 |
| `GET` | `/admin/bgm/subjects/{bgm_id}` | 预览单个 Bangumi 番剧详情（含 `eps`/`platform`/`tags` 及库内共享 `bgm_id` 关联列表） |
| `GET` | `/admin/export/season_mal` | 导出 `season_mal` 人工覆盖与移除记录（`origin = 'manual' OR is_removed = 1`），返回 `application/x-ndjson` |
| `GET` | `/admin/export/matches` | 导出全量 `mal_bgm_matches` 匹配记录，返回 `application/x-ndjson` |
| `GET` | `/admin/subjects/orphans` | 查询未被任何 `mal_bgm_matches.bgm_id IS NOT NULL` 引用的孤儿番剧 |
| `DELETE` | `/admin/subjects/orphans` | 批量清理孤儿番剧（排除任何 `mal_bgm_matches.bgm_id IS NOT NULL` 引用的番剧） |
| `GET` | `/admin/subjects/{subject_id}` | 获取单个番剧详情 |
| `PATCH` | `/admin/subjects/{subject_id}` | 手动微调番剧字段 |
| `DELETE` | `/admin/subjects/{subject_id}` | 删除单个番剧（同事务重置关联 `mal_bgm_matches` 为 `pending`） |
| `POST` | `/admin/subjects/{subject_id}/sync` | 手动重刷单个 Bangumi 番剧评分、排名与剧集评论均值 |
| `POST` | `/admin/scheduler/trigger` | 手动触发一次调度器 Tick（若正在运行则返回 `409 Conflict`） |
| `GET` | `/admin/scheduler/status` | 查询调度器运行状态及上次 Tick 统计信息 |
| `POST` | `/admin/deploy` | 手动触发 `DEPLOY_HOOK_URL` |

---

## 5. 环境变量说明

| 变量名 | 必填 | 默认值 | 说明 |
| :--- | :---: | :--- | :--- |
| `DATABASE_URL` | 是 | - | SQLite 连接字符串，例如 `sqlite:./bgm_rank.db` |
| `ADMIN_TOKEN` | 是 | - | `/admin/*` 接口鉴权所需的 Bearer Token（缺失或为空时服务拒绝启动，中间件亦拒绝所有请求） |
| `BGM_TOKEN` | 否 | 无 | Bangumi API 访问令牌（用于提升速率限制或访问受限条目） |
| `DEPLOY_HOOK_URL` | 否 | 无 | 同步完成后触发的前端构建 Webhook URL |
| `MAL_CLIENT_ID` | `fetch-mal` 必填 | - | MAL API Client ID（请求头 `X-MAL-CLIENT-ID`，严禁写入日志） |
| `DEEPSEEK_API_KEY` | `match` 可选 | 无 | DeepSeek API Key（优先于 OpenRouter；严禁写入日志） |
| `DEEPSEEK_MODEL` | 否 | `deepseek-chat` | 使用 DeepSeek 时的模型名覆盖（为空或未设时取默认值） |
| `OPENROUTER_API_KEY` | `match` 可选 | 无 | OpenRouter API Key（未设 `DEEPSEEK_API_KEY` 时使用；两者都未设则跳过 LLM 层；严禁写入日志） |
| `OPENROUTER_MODEL` | 否 | `google/gemini-2.5-flash-lite` | 使用 OpenRouter 时的模型名覆盖（如 `deepseek/deepseek-v3.2`；为空或未设时取默认值） |
| `LLM_PROMPT_MODE` | 否 | `low` | LLM prompt 丰富度 `low` / `high`（`high` 输入 token 约 2.5~3 倍） |
| `RUST_LOG` | 否 | `info` | `tracing` 日志过滤级别（如 `debug`, `info`） |

---

## 6. Agent 开发与维护规范

在此代码库中进行功能开发、重构或问题排查时，必须遵守以下规范：

### 6.1 构建与验证命令
- **运行全部测试**: `cargo test`
- **代码格式检查与修复**: `cargo fmt`
- **静态分析与 Lint**: `cargo clippy`
- **MAL 抓取 CLI**: `cargo run -- fetch-mal --season 202601`（或 `--from 202510 --to 202601`；需 `DATABASE_URL`、`MAL_CLIENT_ID`），由 `core/fetch` 的 `MalFetchService` + `services/mal` 的 `MalClient` 实现
- **匹配 CLI**: `cargo run -- match --season 202601 [--retry]` 或 `cargo run -- match --mal-id <id> [--retry]`（需 `DATABASE_URL`；可选 `DEEPSEEK_API_KEY` / `OPENROUTER_API_KEY` / `LLM_PROMPT_MODE`；`--mal-id` 同样接受 `--retry`），由 `core/matcher` 的 `MatcherService` 实现
- **一次性数据迁移 CLI**: `cargo run -- import-season-data --season-db <path> [--mal-json-dir <dir>]`（需配置 `DATABASE_URL`）

### 6.2 测试规范 (TDD)
- **数据库隔离测试**: 涉及 `dal`、`core` 或 `api` 的测试应优先使用 `#[sqlx::test]` 宏（由 `sqlx` 自动创建临时内存/文件数据库并执行 `migrations/`），通过 `Database::from_pool(pool)` 构造测试实例，严禁在单元测试中污染本地开发库。
- **外部网络隔离**: 任何涉及 `BangumiClient` 或 `DeployHookClient` 的测试必须使用 `mockito::Server::new_async().await` 在本地启动 Mock Server，并通过 `with_base_url` / `SyncService::with_clients` 注入，严禁在测试中发起真实外网请求。
- **路由与中间件测试**: 使用 `tower::ServiceExt::oneshot` 直接向 `axum::Router` 发送构造的 `http::Request` 进行端到端断言。
- **日志断言**: 需要验证特定日志字段（如 `request_id`、鉴权失败原因、启动状态）时，使用 `#[tracing_test::traced_test]` 配合 `logs_contain(...)`。

### 6.3 安全与可观测性红线
- **禁止泄露敏感凭据**: 在编写日志（`tracing::info!`, `warn!`, `error!`, `debug!`）或返回 HTTP 错误响应时，**严禁**输出 `ADMIN_TOKEN`、`BGM_TOKEN` 或请求头中的 `Authorization` 明文。
- **结构化日志字段**: 新增日志应尽量采用 `tracing` 结构化键值对（例如 `tracing::warn!(season_id, subject_id, error = %e, "message")`），保持与现有可观测性体系一致。
- **数据库迁移**: 修改表结构时，必须在 `migrations/` 目录下新增按序号递增的 `.sql` 迁移脚本，并同步更新 `src/dal/dto/` 与对应的 Repository 测试。
