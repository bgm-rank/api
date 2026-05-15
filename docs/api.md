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

选取规则：优先选该季度 `rank` 最小的番剧；若该季度所有番剧均无 `rank`，则选 `collection_total` 最多的。

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
      "media_type": "TV",
      "rating": "G"
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

获取指定季度的番剧列表，按 `rank` 升序排列（无 rank 的排在末尾）。

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
    "media_type": "TV",
    "rating": "G"
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
| `rank` | `number \| null` | Bangumi 全站排名，如果没有排名则返回999999 |
| `score` | `number \| null` | 评分（精度约 4 位小数） |
| `collection_total` | `number \| null` | 总收藏数 |
| `average_comment` | `number \| null` | 平均吐槽数（由已播集数计算） |
| `drop_rate` | `number \| null` | 弃坑率（0~1） |
| `air_weekday` | `string \| null` | 播出星期，如 `"星期五"` |
| `meta_tags` | `string[]` | 标签列表，已去重保留首次出现顺序 |
| `media_type` | `string \| null` | 媒体类型，如 `"TV"` |
| `rating` | `string \| null` | 分级，如 `"G"` |

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

创建新季度并从 Bangumi 同步数据（同步操作为阻塞，可能耗时较长）。

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

**响应 201**

```json
{
  "season_id": 202601,
  "subjects_added": 42,
  "subjects_removed": 0,
  "subjects_updated": 3,
  "subjects_failed": 0
}
```

**响应 400** — `month` 不在允许值内

---

### POST /admin/seasons/{season_id}/sync

重新同步指定季度的数据（阻塞）。

**响应 200**

```json
{
  "season_id": 202601,
  "subjects_added": 0,
  "subjects_removed": 0,
  "subjects_updated": 5,
  "subjects_failed": 0
}
```

**响应 500** — 季度不存在或同步失败

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

**响应 200** — 返回更新后的 Season 对象（同 GET /admin/seasons/{id}）

**响应 404** — 季度不存在

---

### DELETE /admin/seasons/{season_id}

删除指定季度（不删除关联的番剧记录）。

**响应 200**

```json
{
  "season_id": 202601,
  "deleted": true
}
```

**响应 404** — 季度不存在

---

### DELETE /admin/seasons/{season_id}/subjects/{subject_id}

将指定番剧从季度中移除（仅解除关联，不删除番剧记录本身）。

**响应 200**

```json
{ "removed": true }
```

**响应 404** — 关联不存在

---

### GET /admin/subjects/{subject_id}

获取番剧详情（含内部字段 `updated_at`、`last_updated_at`）。

**响应 200**

```json
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
  "meta_tags": ["TV", "奇幻"],
  "media_type": "TV",
  "rating": "G",
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

删除番剧记录（同时解除其所有季度关联）。

**响应 200**

```json
{ "deleted": true }
```

**响应 404** — 番剧不存在

---

### GET /admin/subjects/orphans

查询孤儿番剧（不属于任何季度的番剧记录）。

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

清理所有孤儿番剧。

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
