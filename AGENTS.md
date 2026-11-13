# AGENTS.md — `bgm-rank-api` 架构与开发指南

## 1. 项目概述

`bgm-rank-api` 是 Bangumi 季度新番排行榜的后端 API 服务，采用 Rust (Edition 2024) 构建。核心职责包括：
1. 从上游 [`season-data`](https://github.com/bgm-rank/season-data) 与 [Bangumi API (`api.bgm.tv`)](https://api.bgm.tv) 同步季度番剧索引、条目详情与剧集数据，计算派生指标（如弃番率、剧集平均评论数、放送星期等）并持久化到本地 SQLite 数据库。
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
/home/yui/repos/others/bgm-rank/api
├── Cargo.toml                  # 依赖与包配置
├── Dockerfile                  # 多阶段容器镜像构建
├── compose.yml                 # Docker Compose 编排配置
├── .env.example                # 环境变量模板
├── migrations/                 # SQLx 自动迁移脚本
└── src/
    ├── main.rs                 # 程序入口：环境初始化、DB 连接、调度器后台任务、HTTP Server 启动
    ├── api/                    # 接口层 (HTTP Routing, Handlers, Middleware, Schemas)
    │   ├── mod.rs              # Router 组装与中间件层 (RequestId, TraceLayer)
    │   ├── endpoints.rs        # Public 与 Admin 路由 Handler 实现及 AppState 定义
    │   ├── schemas.rs          # HTTP 请求/响应 DTO 定义
    │   └── middleware/
    │       ├── mod.rs
    │       └── auth.rs         # 管理员 Bearer Token 鉴权中间件 (`require_admin_token`)
    ├── core/                   # 核心业务逻辑层 (Domain Services)
    │   ├── mod.rs
    │   ├── query/mod.rs        # QueryService：公开接口的数据查询与聚合组装
    │   ├── sync/mod.rs         # SyncService：番剧拉取、指标计算、SSE 同步流与全量对账 (Reconcile)
    │   ├── scheduler/mod.rs    # SchedulerService：定时与手动触发的后台调度器
    │   └── admin/mod.rs        # AdminService：管理端操作（部署触发、调度控制、条目/季度 CRUD）
    ├── dal/                    # 数据访问层 (Data Access Layer)
    │   ├── mod.rs
    │   ├── db.rs               # Database 封装 (SqlitePool, WAL, Migrate, Ping)
    │   ├── dto/                # 数据库表结构映射实体 (FromRow) 与写入/更新 DTO
    │   │   ├── season.rs
    │   │   ├── subject.rs
    │   │   └── season_subject.rs
    │   └── repositories/       # 仓储实现：封装所有原始 SQL 操作
    │       ├── season_repository.rs
    │       ├── subject_repository.rs
    │       └── season_subject_repository.rs
    └── services/               # 外部服务客户端层
        ├── mod.rs
        ├── bangumi/            # Bangumi v0 API 客户端 (`GET /v0/subjects/{id}`, `GET /v0/episodes`)
        ├── season_data/        # GitHub Releases `season-data.json` 拉取客户端
        └── deploy_hook/        # 外部部署 Webhook 触发客户端 (`DeployHookClient`)
```

---

## 3. 四层架构与职责边界

项目严格遵循自顶向下的四层架构设计：

1. **API 层 (`src/api/`)**
   - 仅负责 HTTP 协议转换（路径参数、JSON Body 解析、HTTP 状态码映射、SSE 事件流包装）、请求级追踪 (`x-request-id`) 和权限校验。
   - **禁止**在 Handler 中直接编写 SQL 或直接调用 `src/services/` 中的外部 HTTP 客户端，所有操作必须委托给 `AppState` 中的 `core` 服务。
2. **Core 业务层 (`src/core/`)**
   - **`QueryService`**: 处理公开查询，包含季度列表、当前季度判定、各季度 Top 1 番剧提取、季度下属番剧按 `rank ASC NULLS LAST` 排序等逻辑。
   - **`SyncService`**: 核心同步引擎。结合 `SeasonDataClient` 与 `BangumiClient`，拉取番剧元信息与本篇剧集列表，计算派生字段：
     - `collection_total`: `collect + doing + on_hold + dropped`（不含 `wish`）
     - `drop_rate`: `dropped / collection_total`
     - `average_comment`: 已放送正片剧集的评论数均值
     - `air_weekday`: 从 Infobox 中提取放送星期
     - 支持通过 `mpsc::Sender<SyncProgressEvent>` 推送实时同步进度，以及全量对账 (`reconcile_all`)。
   - **`SchedulerService`**: 在 UTC+8 的 `0, 4, 8, 12, 16, 20` 整点唤醒（或通过 `SchedulerHandle` 手动唤醒）。每次 Tick 先执行 `reconcile_all` 对账，再根据季度距离（`age`）按不同时间间隔刷新到期番剧，最后触发 Deploy Hook（手动触发除外）。使用 `AtomicBool` 防止重入并发。
   - **`AdminService`**: 封装管理员对季度、番剧、关联关系的修改/删除，以及对调度器和 Deploy Hook 的控制。
3. **DAL 数据访问层 (`src/dal/`)**
   - `Database` 负责管理 `SqlitePool`。
   - `SeasonRepository`、`SubjectRepository`、`SeasonSubjectRepository` 封装针对 `seasons`、`subjects`、`season_subjects` 三张表的所有 SQL 查询。
   - 数据库错误通过统一的 `log_db_error` 按错误类型分级记录日志（`RowNotFound` -> `debug`, 唯一约束冲突 -> `warn`, 其他 -> `error`）。
4. **Services 外部客户端层 (`src/services/`)**
   - 封装对第三方 HTTP 服务的请求、超时/重定向策略与响应反序列化。
   - 所有客户端均提供注入自定义 URL 的构造方法（如 `BangumiClient::with_base_url`、`SeasonDataClient::with_url`），以便在单元测试中配合 `mockito` 拦截请求。

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
| `POST` | `/admin/seasons` | 创建并同步新季度（仅允许月份 `1, 4, 7, 10`） |
| `POST` | `/admin/seasons/sync-all` | 后台异步重同步所有季度 |
| `POST` | `/admin/seasons/reconcile` | 以 `season-data.json` 为准执行全量季度成员对账 |
| `GET` | `/admin/seasons/{season_id}` | 获取单个季度详情 |
| `PATCH` | `/admin/seasons/{season_id}` | 修改季度信息（如自定义名称） |
| `DELETE` | `/admin/seasons/{season_id}` | 删除指定季度及其关联关系 |
| `POST` | `/admin/seasons/{season_id}/sync` | 重同步指定季度，返回 **SSE (`text/event-stream`)** 实时进度事件 |
| `DELETE` | `/admin/seasons/{season_id}/subjects/{subject_id}` | 移除某季度与某番剧的关联关系 |
| `GET` | `/admin/subjects/orphans` | 查询不属于任何季度的孤儿番剧 |
| `DELETE` | `/admin/subjects/orphans` | 批量清理孤儿番剧 |
| `GET` | `/admin/subjects/{subject_id}` | 获取单个番剧详情 |
| `PATCH` | `/admin/subjects/{subject_id}` | 手动微调番剧字段 |
| `DELETE` | `/admin/subjects/{subject_id}` | 删除单个番剧 |
| `POST` | `/admin/scheduler/trigger` | 手动触发一次调度器 Tick（若正在运行则返回 `409 Conflict`） |
| `GET` | `/admin/scheduler/status` | 查询调度器运行状态及上次 Tick 统计信息 |
| `POST` | `/admin/deploy` | 手动触发 `DEPLOY_HOOK_URL` |

---

## 5. 环境变量说明

| 变量名 | 必填 | 默认值 | 说明 |
| :--- | :---: | :--- | :--- |
| `DATABASE_URL` | 是 | - | SQLite 连接字符串，例如 `sqlite:./bgm_rank.db` |
| `ADMIN_TOKEN` | 是 | - | `/admin/*` 接口鉴权所需的 Bearer Token |
| `BGM_TOKEN` | 否 | 无 | Bangumi API 访问令牌（用于提升速率限制或访问受限条目） |
| `DEPLOY_HOOK_URL` | 否 | 无 | 同步完成后触发的前端构建 Webhook URL |
| `RUST_LOG` | 否 | `info` | `tracing` 日志过滤级别（如 `debug`, `info`） |

---

## 6. Agent 开发与维护规范

在此代码库中进行功能开发、重构或问题排查时，必须遵守以下规范：

### 6.1 构建与验证命令
- **运行全部测试**: `cargo test`
- **代码格式检查与修复**: `cargo fmt`
- **静态分析与 Lint**: `cargo clippy`

### 6.2 测试规范 (TDD)
- **数据库隔离测试**: 涉及 `dal`、`core` 或 `api` 的测试应优先使用 `#[sqlx::test]` 宏（由 `sqlx` 自动创建临时内存/文件数据库并执行 `migrations/`），通过 `Database::from_pool(pool)` 构造测试实例，严禁在单元测试中污染本地开发库。
- **外部网络隔离**: 任何涉及 `BangumiClient`、`SeasonDataClient` 或 `DeployHookClient` 的测试必须使用 `mockito::Server::new_async().await` 在本地启动 Mock Server，并通过 `with_base_url` / `with_url` / `SyncService::with_clients` 注入，严禁在测试中发起真实外网请求。
- **路由与中间件测试**: 使用 `tower::ServiceExt::oneshot` 直接向 `axum::Router` 发送构造的 `http::Request` 进行端到端断言。
- **日志断言**: 需要验证特定日志字段（如 `request_id`、鉴权失败原因、启动状态）时，使用 `#[tracing_test::traced_test]` 配合 `logs_contain(...)`。

### 6.3 安全与可观测性红线
- **禁止泄露敏感凭据**: 在编写日志（`tracing::info!`, `warn!`, `error!`, `debug!`）或返回 HTTP 错误响应时，**严禁**输出 `ADMIN_TOKEN`、`BGM_TOKEN` 或请求头中的 `Authorization` 明文。
- **结构化日志字段**: 新增日志应尽量采用 `tracing` 结构化键值对（例如 `tracing::warn!(season_id, subject_id, error = %e, "message")`），保持与现有可观测性体系一致。
- **数据库迁移**: 修改表结构时，必须在 `migrations/` 目录下新增按序号递增的 `.sql` 迁移脚本，并同步更新 `src/dal/dto/` 与对应的 Repository 测试。
