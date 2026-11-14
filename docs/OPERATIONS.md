# Bangumi Rank API 操作手册

所有 Admin 接口均需携带认证头：

```
Authorization: Bearer <ADMIN_TOKEN>
```

`season_id` 格式为 `YYYYMM`，月份只能是 `1`、`4`、`7`、`10`（对应冬/春/夏/秋）。

---

## 一次性历史数据迁移（`import-season-data`）

从旧版 `season-data` SQLite 数据库（`season.db`）一次性将历史季度、MAL 条目、季度关联与匹配决策导入本服务数据库。该命令为内建 CLI 子命令，直接执行导入后退出，不启动 HTTP Server。

### 执行方式

需要先配置目标库连接环境变量 `DATABASE_URL`（支持读取当前目录 `.env` 文件，启动时会自动执行数据库迁移）：

```bash
# 使用编译后的二进制（--mal-json-dir 为可选参数，当前 ../season-data 中暂无该目录，可省略）
bgm-rank-api import-season-data \
  --season-db ../season-data/season.db

# 若后续补齐了原始 MAL JSON 缓存目录，也可在开发环境下带上 --mal-json-dir（目录不存在时会自动告警并回退，不会报错退出）
DATABASE_URL=sqlite:./bgm_rank.db cargo run -- import-season-data \
  --season-db ../season-data/season.db
#  --mal-json-dir ../season-data/data/mal  # 可选，当前不存在
```

参数说明：
- `--season-db <path>`（必填）：旧版只读源数据库路径（也支持 `--season-db=<path>`）。
- `--mal-json-dir <dir>`（可选，当前 `season-data` 仓库中暂无此目录）：旧版原始 MAL JSON 缓存目录，用于还原真实的 MAL 原始分级（`g` / `pg` / `pg_13` / `r` / `r+` / `rx`）；若省略或指向不存在的目录，会记录 `WARN` 日志并自动回退为粗粒度分级映射。

### 导入内容与幂等性

导入在**单个数据库事务**内执行，具备**幂等性**（可安全重复运行，不会重复插入或覆盖人工审核成果）：
1. **`seasons`**：解析 `"YYYY-winter|spring|summer|fall"` 写入对应 `YYYY01/04/07/10`（`ON CONFLICT DO NOTHING`）。
2. **`subjects` 骨架**：为 `season_items` 与 `overrides` 中引用的 `bgm_id` 插入占位骨架行（`last_updated_at = NULL`, `ON CONFLICT DO NOTHING`），待后续同步或调度器补全完整字段。
3. **`mal_anime`**：导入 MAL 元数据；`rating` 优先读取 `--mal-json-dir` 中的原始字段，仅当提供原始分级或目标库现有 `rating` 为 `NULL` 时更新。
4. **`season_mal`**：导入季度关联（原 `origin = 'override'` 映射为 `'manual'`，其余为 `'mal'`），并将 `overrides` 中的 `skip` / `wrong_season` 记录置为 `is_removed = 1`。
5. **`mal_bgm_matches`**：同一 `mal_id` 在多季度存在记录时，按 `included` > 非 `included`、`human` > 其他来源、最新 `updated_at`、有 `bgm_id`、较大 `season_id` 合并去重；不会覆盖目标库中已有的 `status = 'included'` 或 `source = 'human'` 决策。

### 分级回退告警（Rating Fallback Warning）

若未传入 `--mal-json-dir` 或部分 `mal_id` 在 JSON 目录中找不到原始 `rating`，工具会回退为粗粒度映射：
- `kids` → `"g"`
- `r18` → `"rx"`
- `general` → `"pg_13"`

此时日志会输出一条 `WARN`：`used fallback rating mapping for mal_anime rows without raw JSON rating`（附带 `rating_fallback` 计数）。回退映射在公开接口上仍能还原为 `kids` / `r18` / `general`，但会丢失原始 `pg` 与 `g`、`r+` 与 `rx` 等细粒度差异；因此首次迁移时建议带上 `--mal-json-dir`。

### 上线切换顺序（Cutover Order）

在部署新版本（包含 `002_mal_season_model.sql` 与 `003_mal_bgm_matches_check.sql` 迁移）时，**必须严格按以下顺序先离线导入并校验数据，再启动新 HTTP 服务**：

> [!WARNING]
> - **禁止在运行 `import-season-data` 之前先启动新版本 HTTP 服务或新容器镜像**：`002` 迁移会移除旧 `season_subjects` 表并新建空的 `mal_anime`、`season_mal`、`mal_bgm_matches` 表。若先启动服务，所有公开排行榜接口（`/api/seasons/{season_id}/subjects`、`/api/seasons/top1`）将返回空列表。
> - **导入完成前严禁执行孤儿番剧清理（`DELETE /admin/subjects/orphans`）或整季删除**：在 `mal_bgm_matches` 尚未导入前，原有 `subjects` 记录均无 `mal_bgm_matches.bgm_id` 引用，会被判定为孤儿记录并永久误删。

建议切换步骤：
1. **停止旧服务并备份数据库**：确保没有旧进程写入 `bgm_rank.db`，提前复制一份冷备份（如 `cp bgm_rank.db bgm_rank.db.bak`），且**暂不启动**新版 HTTP 服务。
2. **离线执行 `import-season-data`**：在目标 `bgm_rank.db` 上运行一次导入命令（命令内部会自动应用 `002`/`003` 迁移并在单事务内写入历史数据）：
   ```bash
   DATABASE_URL=sqlite:./bgm_rank.db bgm-rank-api import-season-data \
     --season-db ../season-data/season.db
   ```
3. **启动前用 `sqlite3` 校验非空计数**：确认 `season_mal` 与 `mal_bgm_matches` 均已成功落库且非空，再放行上线：
   ```bash
   sqlite3 ./bgm_rank.db "SELECT COUNT(*) AS season_mal_count FROM season_mal;"
   sqlite3 ./bgm_rank.db "SELECT COUNT(*) AS matches_count FROM mal_bgm_matches;"
   sqlite3 ./bgm_rank.db "SELECT status, COUNT(*) FROM mal_bgm_matches GROUP BY status;"
   ```
   确认 `season_mal_count > 0`、`matches_count > 0` 且 `included` 条目数量符合预期。
4. **启动新版 HTTP 服务**：校验通过后再启动新版 `bgm-rank-api` 服务进程/容器，并抽检 `/health` 与 `/api/seasons/current`、`/api/seasons/{season_id}/subjects` 确认排行榜数据正常。

---

## 场景一：新增并同步一个季度

**触发时机**：新季度开播，库中尚未建立该季度记录（或已提前写入 `mal_anime`、`season_mal` 与 `mal_bgm_matches` 数据，需要创建季度并拉取 Bangumi 详情）。

**操作步骤**：

```
POST /admin/seasons
Content-Type: application/json

{
  "year": 2026,
  "month": 4,
  "name": "2026年春季"   // 可选，不填则没有别名
}
```

**它做了什么**：
1. 在 `seasons` 表里 upsert 该季度记录。
2. 在数据库内查询 `season_mal (is_removed = 0)` 关联 `mal_bgm_matches (status = 'included' AND bgm_id IS NOT NULL)` 得到的去重 `bgm_id` 列表。
3. 逐个调用 Bangumi API 拉取每部番组的评分、已播正片剧集平均评论数、弃坑率、放送星期等详细信息并写入 `subjects` 表，最后刷新该季度的 `updated_at`。

**成功响应**（`200 OK`，`Content-Type: text/event-stream` SSE 事件流）：

```text
data: {"type":"progress","total":42,"done":1,"subject_id":12345}
...
data: {"type":"done","season_id":202604,"added":0,"removed":0,"updated":42,"failed":0}
```

**注意**：
- 若 `month` 不是 `1` / `4` / `7` / `10`，会直接返回 `400 Bad Request` JSON 错误。
- `failed > 0` 不是致命错误，通常为 Bangumi API 单次拉取超时或限流，下次调度器定时 tick 会自动补全。

---

## 场景二：数据库里已有季度，想更新番组的评分/评论数等数据

**触发时机**：当季节目在播中，想手动刷新一次数据（不等调度器自动触发）。

### 方式 A：触发一次调度器 tick（推荐）

调度器会找出所有到期需要更新的番组，批量刷新后自动触发 Cloudflare 部署。

```
POST /admin/scheduler/trigger
```

**成功响应**（`202 Accepted`）：异步执行，立即返回。

查看执行状态：

```
GET /admin/scheduler/status
```

响应：

```json
{
  "is_running": true,
  "last_run_at": "2026-05-15T04:00:00Z",
  "last_stats": {
    "run_at": "2026-05-15T04:00:00Z",
    "due_count": 38,
    "success_count": 37,
    "fail_count": 1,
    "elapsed_ms": 21000
  }
}
```

若返回 `409 Conflict`，说明调度器正在运行，等它完成再试。

### 方式 B：强制全量 sync 某个特定季度

```
POST /admin/seasons/202604/sync
```

**它做了什么**：
1. 确认 `seasons` 表中存在该季度。
2. 按当前库内 `season_mal (is_removed = 0)` 与 `mal_bgm_matches (status = 'included' AND bgm_id IS NOT NULL)` 查询待同步 `bgm_id` 列表。
3. 逐个调用 Bangumi API 刷新所有已纳入番组的评分、评论数等数据并更新 `seasons.updated_at`。

响应格式同场景一的 SSE 事件流。注意该接口仅刷新已通过 `mal_bgm_matches` 确认纳入（`status = 'included'`）的条目详情，不会改动 `season_mal` 关联关系。

**验证结果**：

```
GET /api/seasons/202604/subjects
```

返回该季度当前的番组列表，按 `rank` 升序排列，`rank` 为空的排在最后。

---

## 场景三：人工审核匹配（`mal_bgm_matches`）

**触发时机**：查看待审核或需修正的 MAL-BGM 匹配关系，人工绑定 `bgm_id` 或排除条目。

1. 查看待审核列表（支持按 `season_id` 与 `status` 过滤，每项附带 `mal_anime` 基础字段与已绑定的 `subject` 摘要）：

```
GET /admin/matches?season_id=202604&status=pending
```

2. 确认绑定 Bangumi 条目或标记排除：

```
PATCH /admin/matches/52991
Content-Type: application/json

{
  "status": "included",
  "bgm_id": 400602,
  "note": "人工核对"
}
```

提交 `status = "included"` 时，系统会将 `source` 标记为 `"human"`，确保 `subjects` 骨架行存在，并立即拉取一次 Bangumi 详情填充该番剧数据。

---

## 场景四：从季度中软移除或跨季移动 MAL 条目

**触发时机**：某部 MAL 番剧被错误地归入了本季度，需要从该季度移除或移动到正确季度。

### 从当前季度软移除

先确定其 `mal_id`（注意路径参数为 **`mal_id`**，不是 Bangumi `subject_id`）：

```
DELETE /admin/seasons/202604/mal/52991
```

**说明**：该操作将 `season_mal` 对应行标记为 `is_removed = 1`，不会删除 `mal_anime` 或 `subjects` 本体。若该条目关联的 `bgm_id` 不再被任何 `mal_bgm_matches.bgm_id` 引用，它将成为孤立番组（orphan），可按场景五清理。

### 恢复已被软移除的条目

```
POST /admin/seasons/202604/mal/52991/restore
```

**说明**：将指定 `season_mal` 记录的 `is_removed` 重置为 `0`（若关联记录不存在则返回 `404`）。

### 跨季移动到目标季度

```
POST /admin/seasons/202604/mal/52991/move
Content-Type: application/json

{
  "target_season_id": 202607,
  "note": "实际为7月番"
}
```

**说明**：在单个事务内将源季度 `season_mal` 标为 `is_removed = 1`，并在目标季度写入或更新为 `origin = "manual"`, `is_removed = 0`。

---

## 场景五：清理孤立番组

**触发时机**：移除季度关联、修改匹配或删除番组后，`subjects` 表里存在不再被任何有效（`is_removed = 0` 且 `status = 'included'`）记录引用的孤立番组。

先查看有哪些孤立番组：

```
GET /admin/subjects/orphans
```

响应：

```json
[
  { "id": 123456, "name": "テスト", "name_cn": "测试" }
]
```

确认后批量删除：

```
DELETE /admin/subjects/orphans
```

响应：

```json
{ "deleted": 3 }
```

**注意**：`DELETE /admin/seasons/{id}` 删除整个季度时会自动清理孤立番组，无需手动执行这一步。

---

## 场景六：删除整个季度

**触发时机**：某季度数据错误需要重来，或测试数据需要清除。

```
DELETE /admin/seasons/202604
```

**它做了什么**：
1. 删除 `seasons` 表里该条记录（外键 `ON DELETE CASCADE` 自动级联删除该季度的 `season_mal` 记录）。
2. **自动**清理因此不再被任何有效 `included` 匹配引用的孤立 `subjects`。

---

## 场景七：手动修正某部番组的数据

**触发时机**：Bangumi 数据有误，或需要手动覆盖某些字段。

先查看当前数据：

```
GET /admin/subjects/123456
```

按需修改（所有字段均为可选，只传需要改的字段）：

```
PATCH /admin/subjects/123456
Content-Type: application/json

{
  "rank": 42,
  "name_cn": "修正后的中文名",
  "meta_tags": ["TV", "动作", "科幻"]
}
```

返回修改后的完整番组数据。

---

## 场景八：修改季度别名

```
PATCH /admin/seasons/202604
Content-Type: application/json

{
  "name": "2026春季（修正版）"
}
```

---

## 场景九：一次性同步全部季度的数据

**触发时机**：服务重启后想立即刷新所有季度，不等调度器。

```
POST /admin/seasons/sync-all
```

**注意**：异步执行（`202 Accepted`），日志里可以看进度。此操作对所有季度重新全量 sync，耗时较长，非必要不要频繁调用。

---

## 场景十：手动触发 Cloudflare Pages 部署

**触发时机**：数据已经更新，但部署 hook 没自动触发（网络异常等情况），需要手动推一次。

```
POST /admin/deploy
```

---

## 人工决策与异常记录备份（JSONL 导出）

服务提供两个 NDJSON（`application/x-ndjson`）导出接口，便于定期将人工修正的季度归属和全量 MAL-BGM 匹配决策备份到 Git 仓库或本地文件：

- `GET /admin/export/season_mal`：导出 `season_mal` 中所有人工添加或软移除的记录（`origin = 'manual' OR is_removed = 1`），按 `season_id ASC, mal_id ASC` 排序。
- `GET /admin/export/matches`：导出全部 `mal_bgm_matches` 决策记录（`mal_id, status, source, bgm_id, confidence, reason, note`），按 `mal_id ASC` 排序。

### `curl` 备份示例

```bash
mkdir -p data

curl -fsS -H "Authorization: Bearer $ADMIN_TOKEN" \
  https://api.rinshankaiho.fun/admin/export/season_mal \
  > data/season_mal.jsonl

curl -fsS -H "Authorization: Bearer $ADMIN_TOKEN" \
  https://api.rinshankaiho.fun/admin/export/matches \
  > data/matches.jsonl
```

本地调试时可将域名替换为 `http://localhost:3000`。若暂无符合条件的记录，接口返回 `200 OK` 与空响应体，输出文件为空文件。

---

## 附录：接口速查表

| 用途 | 方法 | 路径 |
|------|------|------|
| 健康检查 | GET | `/health` |
| 列出所有季度 | GET | `/api/seasons` |
| 获取当前季度 | GET | `/api/seasons/current` |
| 获取各季度 Top 1 | GET | `/api/seasons/top1` |
| 查看季度番组列表 | GET | `/api/seasons/{id}/subjects` |
| **新建季度并同步**（SSE） | POST | `/admin/seasons` |
| **重新同步季度数据**（SSE） | POST | `/admin/seasons/{id}/sync` |
| 同步全部季度 | POST | `/admin/seasons/sync-all` |
| 查看季度详情（Admin） | GET | `/admin/seasons/{id}` |
| 修改季度别名 | PATCH | `/admin/seasons/{id}` |
| 删除整个季度 | DELETE | `/admin/seasons/{id}` |
| 从季度软移除 MAL 条目 | DELETE | `/admin/seasons/{id}/mal/{mal_id}` |
| 跨季移动 MAL 条目 | POST | `/admin/seasons/{id}/mal/{mal_id}/move` |
| 恢复季度中被软移除的 MAL 条目 | POST | `/admin/seasons/{id}/mal/{mal_id}/restore` |
| 查询匹配记录 | GET | `/admin/matches` |
| 人工审核匹配记录 | PATCH | `/admin/matches/{mal_id}` |
| 导出季度人工覆盖记录（NDJSON） | GET | `/admin/export/season_mal` |
| 导出匹配记录（NDJSON） | GET | `/admin/export/matches` |
| 查看番组详情（Admin） | GET | `/admin/subjects/{id}` |
| 手动修改番组字段 | PATCH | `/admin/subjects/{id}` |
| 删除番组 | DELETE | `/admin/subjects/{id}` |
| 查看孤立番组 | GET | `/admin/subjects/orphans` |
| 清理孤立番组 | DELETE | `/admin/subjects/orphans` |
| 手动触发调度器 | POST | `/admin/scheduler/trigger` |
| 查看调度器状态 | GET | `/admin/scheduler/status` |
| 手动触发部署 | POST | `/admin/deploy` |
