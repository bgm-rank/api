# bgm-rank-api

## 1. 简介

`bgm-rank-api` 是一个使用 Rust 编写的 Bangumi 季度新番排行榜更新器的后端 API 服务。
项目采用基于 `axum` 和 `tokio` 的现代异步 Web 框架构建，使用 `sqlx` 与 SQLite 进行交互，并集成了对 Bangumi API 的异步调用和数据同步能力。该服务旨在通过本地 SQLite 数据库存储排行榜数据，定期从 Bangumi 获取和更新番剧信息，为前端提供高效的数据读取接口，同时包含了一套用于管理季度数据的管理员鉴权接口。

## 2. API 简单用法

服务默认运行在 `0.0.0.0:3000`。
接口分为对前端公开访问的 Public API 和 需要鉴权的 Admin API。

### 公开接口 (Public API)

无需鉴权，返回 JSON 格式数据。

- **健康检查**

  ```http
  GET /health
  ```

- **获取所有季度列表**

  ```http
  GET /api/seasons
  ```

- **获取指定季度的番剧排行榜**

  ```http
  GET /api/seasons/{season_id}/subjects
  ```

  *(注: `season_id` 格式例如 `202601`)*

### 管理员接口 (Admin API)

需要在 HTTP 请求头中携带 `Authorization: Bearer <ADMIN_TOKEN>` 进行鉴权。

- **创建并同步新的季度**

  ```http
  POST /admin/seasons
  Content-Type: application/json

  {
      "year": 2026,
      "month": 1,
      "name": "可选的自定义名称"
  }
  ```

  *(注: `month` 仅支持代表季度的 `1, 4, 7, 10`)*

- **重新同步/更新某季度数据**

  ```http
  POST /admin/seasons/{season_id}/sync
  ```

- **删除指定季度**

  ```http
  DELETE /admin/seasons/{season_id}
  ```

- **查询孤儿番剧 (不再属于任何季度的番剧)**

  ```http
  GET /admin/subjects/orphans
  ```

- **清理孤儿番剧**

  ```http
  DELETE /admin/subjects/orphans
  ```

## 3. 快速开始

### 环境依赖

- **Rust**: Edition 2024 (推荐 rustc ≥ 1.83)

### 环境变量配置

在项目根目录下创建一个 `.env` 文件：

```env
# SQLite 数据库文件路径
DATABASE_URL=sqlite:./bgm_rank.db

# 管理员接口调用所需的认证 Token
ADMIN_TOKEN=your_secure_admin_token

# 可选：bangumi.tv 接口调用所需的 Token
BGM_TOKEN=your_bangumi_api_token

# 可选：Cloudflare Pages 部署 Webhook
DEPLOY_HOOK_URL=https://...

# 可选：日志级别配置，默认为 info
RUST_LOG=info
```

### 运行服务

```bash
cargo run
```

数据库文件会在首次启动时自动创建，表结构迁移也会自动执行。服务成功启动后，你将会看到包含 `addr="0.0.0.0:3000" db_status="connected"` 的 INFO 级别启动日志。

## 4. 其他

### 技术栈说明

- **Web 框架**: `axum` 0.8.x + `tower-http`
- **异步运行时**: `tokio` (full features)
- **数据库**: `sqlx` + SQLite (WAL 模式，自动迁移)
- **HTTP 客户端**: `reqwest`
- **序列化**: `serde` + `serde_json`
- **日志监控**: `tracing` + `tracing-subscriber`

### 开发与测试

项目遵循测试驱动开发 (TDD) 流程，内置了 API 路由测试和 `sqlx` 数据库集成测试（每个测试使用独立的临时 SQLite 数据库）。

```bash
# 运行所有测试
cargo test

# 运行代码格式化和 Lint 检查
cargo fmt
cargo clippy
```

### 构建生产版本

```bash
cargo build --release
./target/release/bgm-rank-api
```

### Docker 部署

开发环境（构建镜像）：

```bash
cd api && docker compose up --build
```

生产部署（使用预构建镜像）：

1. 在部署目录下准备 `.env` 文件，`DATABASE_URL` 填写 `sqlite:/app/data/bgm_rank.db`
2. 创建数据目录：`mkdir -p data`
3. 启动：`docker compose up -d`
