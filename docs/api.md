# bgm-rank API 文档

Base URL (dev): `http://0.0.0.0:3000`
Base URL (prod): `https://api.rinshankaiho.fun`

所有响应均为 JSON 格式。错误响应统一结构：

```json
{ "error": "错误描述" }
```

---

## 公开接口

无需鉴权。

### GET /health

健康检查。

**响应 200**

```json
{
  "status": "ok",
  "db": "ok"
}
```

| 字段 | 类型 | 说明 |
|------|------|------|
| `status` | `string` | 固定为 `"ok"` |
| `db` | `string` | `"ok"` 或错误信息 |

---

### GET /api/seasons

获取所有季度列表，按 `season_id` 升序排列。

**响应 200**

```json
[
  {
    "season_id": 202601,
    "year": 2026,
    "season": "WINTER",
    "name": "2026冬",
    "updated_at": "2026-03-10T08:00:00"
  }
]
```

| 字段 | 类型 | 说明 |
|------|------|------|
| `season_id` | `number` | 季度 ID，格式 `YYYYMM`，如 `202601` |
| `year` | `number` | 年份 |
| `season` | `string` | 季度名，枚举值：`WINTER` / `SPRING` / `SUMMER` / `FALL` |
| `name` | `string \| null` | 自定义显示名称 |
| `updated_at` | `string` | 最近数据更新时间，ISO 8601 无时区（UTC） |

---

### GET /api/seasons/current

获取当前季度信息。当前季度由服务器系统时间推算：1–3 月→`01`，4–6 月→`04`，7–9 月→`07`，10–12 月→`10`。

**响应 200**

```json
{
  "season_id": 202604,
  "year": 2026,
  "season": "SPRING",
  "name": "2026春",
  "updated_at": "2026-05-10T08:00:00"
}
```

字段说明同 `GET /api/seasons`。

**响应 404** — 当前季度尚未录入数据库

---

### GET /api/seasons/top1

获取所有季度各自排名第一的番剧，按 `season_id` 降序排列。

选取规则：仅统计 `mal_anime.media_type = 'tv'` 且排除 `kids`（`g` / `pg`）与 `r18`（`r+` / `rx`）分级的番剧；按 `rank` 升序排列（无 `rank` 的排在末尾），若所有番剧均无 `rank` 则选 `collection_total` 最多者。

**响应 200**

```json
[
  {
    "season_id": 202604,
    "subject": {
      "id": 12345,
      "name": "某番剧",
      "name_cn": "某番剧（中文）",
      "images_grid": "https://...",
      "images_large": "https://...",
      "rank": 1,
      "score": 9.1,
      "collection_total": 180000,
      "average_comment": 4.2,
      "drop_rate": 0.03,
      "air_weekday": "星期五",
      "meta_tags": ["TV", "奇幻"],
      "media_type": "tv",
      "rating": "general"
    }
  }
]
```

| 字段 | 类型 | 说明 |
|------|------|------|
| `season_id` | `number` | 季度 ID |
| `subject` | `object` | 该季度第一名的番剧，字段同 `GET /api/seasons/{season_id}/subjects` 单项 |

---

### GET /api/seasons/{season_id}/subjects

获取指定季度的番剧列表，按 `rank` 升序排列（无 rank 的排在末尾）。仅返回 `season_mal.is_removed = 0` 且 `mal_bgm_matches.status = 'included'` 的条目；当多个 `mal_id` 映射到同一 Bangumi `subject_id` 时，按 `media_type` 优先级（`tv > movie > ova > ona > 其他`）及 `mal_id` 升序去重。

**路径参数**

| 参数 | 类型 | 说明 |
|------|------|------|
| `season_id` | `number` | 季度 ID，如 `202601` |

**响应 200**

```json
[
  {
    "id": 12345,
    "name": "葬送的芙莉莲",
    "name_cn": "葬送的芙莉莲",
    "images_grid": "https://...",
    "images_large": "https://...",
    "rank": 1,
    "score": 9.1,
    "collection_total": 180000,
    "average_comment": 4.2,
    "drop_rate": 0.03,
    "air_weekday": "星期五",
    "meta_tags": ["TV", "奇幻", "冒险"],
    "media_type": "tv",
    "rating": "general"
  }
]
```

| 字段 | 类型 | 说明 |
|------|------|------|
| `id` | `number` | Bangumi subject ID |
| `name` | `string \| null` | 原名 |
| `name_cn` | `string \| null` | 中文名 |
| `images_grid` | `string \| null` | 小图 URL（网格封面） |
| `images_large` | `string \| null` | 大图 URL |
| `rank` | `number \| null` | Bangumi 全站排名；同步时原始 `rank = 0` 会规范化为 `999999` |
| `score` | `number \| null` | 评分（由各星级人数加权计算，精度约 4 位小数） |
| `collection_total` | `number \| null` | 总收藏数 |
| `average_comment` | `number` | 平均吐槽数（由已播正片集数计算，无已播集数时为 `0`） |
| `drop_rate` | `number \| null` | 弃坑率（0~1） |
| `air_weekday` | `string \| null` | 播出星期，如 `"星期五"` |
| `meta_tags` | `string[]` | 标签列表，已去重保留首次出现顺序 |
| `media_type` | `string \| null` | 原始 MAL 媒体类型（如 `"tv"`、`"ona"`、`"ova"`、`"movie"`、`"special"`、`"tv_special"` 等） |
| `rating` | `string \| null` | 分级归类结果，枚举值：`general` / `kids` / `r18`；由 `mal_anime.rating` 映射：`g` / `pg` → `kids`，`r+` / `rx` → `r18`，`pg_13` / `r` / `null` 及其他值 → `general` |

**响应 404** — 季度不存在

---

## 管理员接口

所有 `/admin/*` 接口均需在请求头中携带：

```
Authorization: Bearer <ADMIN_TOKEN>
```

未携带或 token 错误返回 **401**。

---

### POST /admin/seasons

创建（或更新）季度并从 Bangumi 同步该季度内已确认匹配（`season_mal.is_removed = 0` 且 `mal_bgm_matches.status = 'included'`）的条目详情，返回 **SSE (`text/event-stream`)** 实时进度流。

**请求体**

```json
{
  "year": 2026,
  "month": 1,
  "name": "2026冬"
}
```

| 字段 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `year` | `number` | 是 | 年份 |
| `month` | `number` | 是 | 月份，仅支持 `1`、`4`、`7`、`10` |
| `name` | `string` | 否 | 自定义名称 |

**响应 200（`Content-Type: text/event-stream`）**

每条事件为标准 SSE `data:` 行（JSON 负载）：

```text
data: {"type":"progress","total":42,"done":1,"subject_id":12345}

data: {"type":"done","season_id":202601,"added":0,"removed":0,"updated":42,"failed":0}
```

失败时推送：

```text
data: {"type":"error","message":"错误描述"}
```

**响应 400** — `month` 不在允许值内（返回标准 JSON `{ "error": "..." }`）

---

### POST /admin/seasons/{season_id}/sync

重新同步指定季度内已纳入匹配（`season_mal.is_removed = 0` 且 `mal_bgm_matches.status = 'included'`）的条目详情，返回 **SSE (`text/event-stream`)** 实时进度流。

**响应 200（`Content-Type: text/event-stream`）**

事件格式同 `POST /admin/seasons`。若季度不存在，流内会推送 `{"type":"error","message":"Season <season_id> not found"}`。

---

### POST /admin/seasons/sync-all

异步同步所有季度，立即返回 202，后台执行。

**响应 202**

```json
{
  "status": "accepted",
  "message": "Sync all seasons started"
}
```

---

### GET /admin/seasons/{season_id}

获取指定季度详情（含 `created_at`）。

**响应 200**

```json
{
  "season_id": 202601,
  "year": 2026,
  "season": "WINTER",
  "name": "2026冬",
  "created_at": "2026-01-01T00:00:00",
  "updated_at": "2026-03-10T08:00:00"
}
```

**响应 404** — 季度不存在

---

### PATCH /admin/seasons/{season_id}

修改季度的显示名称。

**请求体**（所有字段均为可选，仅传需要修改的字段）

```json
{
  "name": "新名称"
}
```

**响应 200** — 返回更新后的 Season 对象（同 `GET /admin/seasons/{season_id}`）

**响应 404** — 季度不存在

---

### DELETE /admin/seasons/{season_id}

删除指定季度（级联删除该季度的 `season_mal` 关联，并自动清理因此成为孤立番剧的 `subjects` 记录）。

**响应 200**

```json
{
  "season_id": 202601,
  "deleted": true
}
```

**响应 404** — 季度不存在

---

### DELETE /admin/seasons/{season_id}/mal/{mal_id}

将指定 `mal_id` 从该季度软移除（将 `season_mal.is_removed` 置为 `1`，不删除 `mal_anime` 或 `subjects` 记录）。

**路径参数**

| 参数 | 类型 | 说明 |
|------|------|------|
| `season_id` | `number` | 季度 ID |
| `mal_id` | `number` | MAL 条目 ID（注意不是 Bangumi `subject_id`） |

**响应 200**

```json
{ "removed": true }
```

**响应 404** — 关联不存在

---

### POST /admin/seasons/{season_id}/mal/{mal_id}/restore

恢复该季度中被软移除的 `mal_id`（将 `season_mal.is_removed` 置为 `0`）。

**路径参数**

| 参数 | 类型 | 说明 |
|------|------|------|
| `season_id` | `number` | 季度 ID |
| `mal_id` | `number` | MAL 条目 ID |

**响应 200**

```json
{ "restored": true }
```

**响应 404** — 关联不存在

---

### POST /admin/seasons/{season_id}/mal/{mal_id}/move

将 `mal_id` 从当前季度移动到目标季度（在源季度标记 `is_removed = 1`，在目标季度写入/更新为 `origin = "manual"`, `is_removed = 0`）。

**请求体**

```json
{
  "target_season_id": 202604,
  "note": "跨季调整"
}
```

| 字段 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `target_season_id` | `number` | 是 | 目标季度 ID（不可与源 `season_id` 相同） |
| `note` | `string` | 否 | 备注（同时更新源记录和目标记录的 `note`） |

**响应 200**

```json
{
  "moved": true,
  "source": {
    "season_id": 202601,
    "mal_id": 1001,
    "origin": "mal",
    "is_removed": true,
    "note": "跨季调整",
    "created_at": "2026-01-01T00:00:00"
  },
  "target": {
    "season_id": 202604,
    "mal_id": 1001,
    "origin": "manual",
    "is_removed": false,
    "note": "跨季调整",
    "created_at": "2026-04-01T00:00:00"
  }
}
```

**响应 400** — `target_season_id` 与源 `season_id` 相同  
**响应 404** — 目标季度不存在，或源季度与 `mal_id` 的关联不存在

---

### GET /admin/matches

查询 `mal_bgm_matches` 匹配记录列表，按 `mal_id` 升序排列。每条记录除匹配字段外，同时内联关联的 `mal_anime` 基础字段（`title`、`title_ja`、`media_type`、`rating`）以及已绑定的 `subject` 摘要（未绑定时为 `null`）。

**查询参数**

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `season_id` | `number` | 否 | 按季度过滤（内连接 `season_mal` 且仅包含 `is_removed = 0` 的有效条目） |
| `status` | `string` | 否 | 可选过滤值：`pending` / `included` / `excluded` |

**响应 200**

```json
[
  {
    "mal_id": 52991,
    "status": "included",
    "source": "human",
    "bgm_id": 400602,
    "confidence": 1.0,
    "candidates": null,
    "reason": null,
    "note": "人工核对",
    "error": null,
    "updated_at": "2026-03-10T08:00:00",
    "title": "Sousou no Frieren",
    "title_ja": "葬送のフリーレン",
    "media_type": "tv",
    "rating": "pg_13",
    "subject": {
      "id": 400602,
      "name": "葬送のフリーレン",
      "name_cn": "葬送的芙莉莲",
      "rank": 1,
      "score": 9.1
    }
  }
]
```

| 字段 | 类型 | 说明 |
|------|------|------|
| `mal_id` ~ `updated_at` | - | `mal_bgm_matches` 匹配字段 |
| `title` | `string` | `mal_anime.title` |
| `title_ja` | `string \| null` | `mal_anime.title_ja` |
| `media_type` | `string` | `mal_anime.media_type` |
| `rating` | `string \| null` | `mal_anime.rating`（MAL 原始分级） |
| `subject` | `object \| null` | 绑定的 Bangumi 番剧摘要（`id`、`name`、`name_cn`、`rank`、`score`）；未绑定 `bgm_id` 时为 `null` |

**响应 400** — `status` 参数不在允许值内

---

### PATCH /admin/matches/{mal_id}

人工审核并更新指定 `mal_id` 的匹配状态与绑定的 `bgm_id`：
- 当最终 `status` 为 `"included"` 或 `"excluded"` 时，将 `source` 置为 `"human"`。
- 当最终 `status` 为 `"pending"` 时，将 `source`、`confidence` 重置为 `null`；若未显式传入 `bgm_id`，同时将 `bgm_id` 重置为 `null`。
- 若传入 `bgm_id`（必须为 `> 0` 的正整数），无论最终 `status` 为何值，均会确保 `subjects` 骨架行存在（`INSERT OR IGNORE`）。
- 当最终 `status` 为 `"included"` 时，会调用 Bangumi API 同步该 `bgm_id` 的详情；若同步失败，匹配决策仍保留成功并返回 **200**，响应中通过 `hydrated: false` 标明详情未同步成功，并在服务端记录 `WARN` 日志。

**请求体**（所有字段均为可选）

```json
{
  "status": "included",
  "bgm_id": 400602,
  "reason": null,
  "note": "人工绑定"
}
```

| 字段 | 类型 | 说明 |
|------|------|------|
| `status` | `string` | `pending` / `included` / `excluded`，省略时沿用原值 |
| `bgm_id` | `number` | 必须为正整数（`> 0`）。当最终 `status` 为 `"included"` 时必填（若原记录已有 `bgm_id` 可省略）；`"excluded"` 不要求 `bgm_id` |
| `reason` | `string \| null` | 排除或备注原因（省略时保留原值，显式传 `null` 时清空为 `null`） |
| `note` | `string \| null` | 审核备注（省略时保留原值，显式传 `null` 时清空为 `null`） |

**响应 200** — 返回更新后的 `MalBgmMatch` 字段以及 `hydrated` 标记：

```json
{
  "mal_id": 52991,
  "status": "included",
  "source": "human",
  "bgm_id": 400602,
  "confidence": 1.0,
  "candidates": null,
  "reason": null,
  "note": "人工绑定",
  "error": null,
  "updated_at": "2026-03-10T08:00:00",
  "hydrated": true
}
```

| 字段 | 类型 | 说明 |
|------|------|------|
| `hydrated` | `boolean` | 当 `status = "included"` 且成功调用 Bangumi API 同步详情时为 `true`，同步失败或未触发同步（`excluded` / `pending`）时为 `false` |

**响应 400** — `status` 非法、`bgm_id <= 0`，或 `status = "included"` 时缺少 `bgm_id`  
**响应 404** — `mal_id` 对应的匹配记录不存在

---

### GET /admin/export/season_mal

导出 `season_mal` 中的人工干预记录（`origin = 'manual' OR is_removed = 1`），按 `season_id` 升序、`mal_id` 升序排列，以 NDJSON（Newline Delimited JSON）格式返回。数据库无符合条件记录时返回空响应体 `""`。

**响应 200（`Content-Type: application/x-ndjson`）**

```json
{"season_id":202510,"mal_id":101,"origin":"manual","is_removed":false,"note":null}
{"season_id":202601,"mal_id":101,"origin":"manual","is_removed":false,"note":"manual add"}
{"season_id":202601,"mal_id":102,"origin":"mal","is_removed":true,"note":"wrong season"}
```

| 字段 | 类型 | 说明 |
|------|------|------|
| `season_id` | `number` | 季度 ID |
| `mal_id` | `number` | MAL 条目 ID |
| `origin` | `string` | 来源（`"mal"` / `"manual"`） |
| `is_removed` | `boolean` | 是否已从该季度软移除 |
| `note` | `string \| null` | 备注 |

---

### GET /admin/export/matches

导出全量 `mal_bgm_matches` 匹配决策记录，按 `mal_id` 升序排列，以 NDJSON 格式返回（仅投影核心字段，不包含 `candidates`、`error`、`updated_at`）。数据库无记录时返回空响应体 `""`。

**响应 200（`Content-Type: application/x-ndjson`）**

```json
{"mal_id":100,"status":"included","source":"exact","bgm_id":5001,"confidence":1.0,"reason":null,"note":"exact match"}
{"mal_id":200,"status":"pending","source":null,"bgm_id":null,"confidence":null,"reason":null,"note":null}
```

| 字段 | 类型 | 说明 |
|------|------|------|
| `mal_id` | `number` | MAL 条目 ID |
| `status` | `string` | 匹配状态（`pending` / `included` / `excluded`） |
| `source` | `string \| null` | 决策来源（`rule` / `exact` / `llm` / `human`） |
| `bgm_id` | `number \| null` | 绑定的 Bangumi subject ID |
| `confidence` | `number \| null` | 匹配置信度 |
| `reason` | `string \| null` | 原因（常见于 `excluded`） |
| `note` | `string \| null` | 备注 |

---

### GET /admin/subjects/{subject_id}

获取番剧详情（含 Bangumi 补充元数据与内部时间戳 `updated_at`、`last_updated_at`）。

**响应 200**

```json
{
  "id": 12345,
  "name": "葬送的芙莉莲",
  "name_cn": "葬送的芙莉莲",
  "air_date": "2023-09-29",
  "platform": "TV",
  "summary": "简介文本",
  "tags": "[\"奇幻\",\"冒险\"]",
  "nsfw": false,
  "images_grid": "https://...",
  "images_large": "https://...",
  "rank": 1,
  "score": 9.1,
  "collection_total": 180000,
  "average_comment": 4.2,
  "drop_rate": 0.03,
  "air_weekday": "星期五",
  "meta_tags": ["TV", "奇幻"],
  "updated_at": "2026-03-10T08:00:00",
  "last_updated_at": "2026-03-10T08:00:00Z"
}
```

**响应 404** — 番剧不存在

---

### PATCH /admin/subjects/{subject_id}

手动覆盖番剧字段（所有字段均为可选，仅传需要修改的字段）。

**请求体**

```json
{
  "name": "新名称",
  "name_cn": "新中文名",
  "images_grid": "https://...",
  "images_large": "https://...",
  "rank": 10,
  "score": 8.5,
  "collection_total": 50000,
  "average_comment": 3.0,
  "drop_rate": 0.05,
  "air_weekday": "星期一",
  "meta_tags": ["TV", "剧情"]
}
```

**响应 200** — 返回更新后的 Subject 对象（同 GET /admin/subjects/{id}）

**响应 404** — 番剧不存在

---

### DELETE /admin/subjects/{subject_id}

删除番剧记录（在同一事务中将引用该 `bgm_id` 的 `mal_bgm_matches` 记录重置为 `status = 'pending'`、`bgm_id = null`、`source = null`、`confidence = null` 并刷新 `updated_at`）。

**响应 200**

```json
{ "deleted": true }
```

**响应 404** — 番剧不存在

---

### GET /admin/subjects/orphans

查询孤儿番剧（未被任何 `mal_bgm_matches.bgm_id` 引用的番剧记录）。

**响应 200**

```json
[
  {
    "id": 99999,
    "name": "某番剧",
    "name_cn": null
  }
]
```

---

### DELETE /admin/subjects/orphans

清理所有孤儿番剧（排除任何 `mal_bgm_matches.bgm_id IS NOT NULL` 引用的番剧）。

**响应 200**

```json
{ "deleted": 3 }
```

| 字段 | 类型 | 说明 |
|------|------|------|
| `deleted` | `number` | 实际删除的番剧数量 |

---

### POST /admin/deploy

手动触发 Cloudflare Pages Deploy Hook（需配置 `DEPLOY_HOOK_URL` 环境变量）。

**响应 202**

```json
{
  "status": "accepted",
  "message": "Deploy triggered"
}
```

---

### POST /admin/scheduler/trigger

手动触发一次调度器执行（若调度器正在运行则返回 409）。

调度器会自动在 UTC+8 的 0/4/8/12/16/20 时整点执行，更新当前季度中到期的番剧数据。

**响应 202**

```json
{
  "status": "accepted",
  "message": "Scheduler tick triggered"
}
```

**响应 409** — 调度器当前正在运行

```json
{ "error": "Scheduler is already running" }
```

---

### GET /admin/scheduler/status

查询调度器当前状态及上次执行统计。

**响应 200**

```json
{
  "is_running": false,
  "last_run_at": "2026-03-10T08:00:00Z",
  "last_stats": {
    "run_at": "2026-03-10T08:00:00Z",
    "due_count": 10,
    "success_count": 9,
    "fail_count": 1,
    "elapsed_ms": 4823
  }
}
```

| 字段 | 类型 | 说明 |
|------|------|------|
| `is_running` | `boolean` | 调度器是否正在执行 |
| `last_run_at` | `string \| null` | 上次执行开始时间（UTC，带 Z 后缀） |
| `last_stats` | `object \| null` | 上次执行统计，首次启动前为 `null` |
| `last_stats.due_count` | `number` | 本次应更新的番剧数量 |
| `last_stats.success_count` | `number` | 成功更新数量 |
| `last_stats.fail_count` | `number` | 失败数量 |
| `last_stats.elapsed_ms` | `number` | 执行耗时（毫秒） |

---

## 已移除或变更的接口与字段

以下旧版行为/字段在当前路由与数据模型中已移除或替换：

- **`subjects` 模型与 `GET /admin/subjects/{subject_id}`**：移除了 `media_type` 与 `rating` 字段（现存储于 `mal_anime` 表，仅在公开接口 `/api/seasons/{season_id}/subjects` 与 `/api/seasons/top1` 中关联并映射后输出）；同时新增 `air_date`、`platform`、`summary`、`tags`、`nsfw` 字段。
- **公开接口 `media_type` 与 `rating` 值域**：`media_type` 不再是大写分类值（如 `"TV"`），而是保留 MAL 原始小写值（如 `"tv"`、`"ona"`、`"ova"`、`"movie"` 等）；`rating` 不再透传 `"G"` 等原始字符串，而是映射为 `general` / `kids` / `r18` 三档。
- **`DELETE /admin/seasons/{season_id}/subjects/{subject_id}` 与 `DELETE /admin/seasons/{season_id}/subjects/{mal_id}`**：已完全移除，统一替换为 `DELETE /admin/seasons/{season_id}/mal/{mal_id}`（按 `mal_id` 将 `season_mal.is_removed` 置为 `1`）及 `POST /admin/seasons/{season_id}/mal/{mal_id}/restore`（恢复为 `is_removed = 0`）；原 `season_subjects` 表及其按 `subject_id` 增删关联的逻辑已移除。
- **`POST /admin/seasons` 与 `POST /admin/seasons/{season_id}/sync`**：已移除阻塞式 JSON 响应（`subjects_added`、`subjects_removed`、`subjects_updated`、`subjects_failed`），统一改为 SSE (`text/event-stream`) 事件流，事件字段为 `added`、`removed`、`updated`、`failed`。
