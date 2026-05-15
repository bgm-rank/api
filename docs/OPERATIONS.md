# Bangumi Rank API 操作手册

所有 Admin 接口均需携带认证头：

```
Authorization: Bearer <ADMIN_TOKEN>
```

`season_id` 格式为 `YYYYMM`，月份只能是 `1`、`4`、`7`、`10`（对应冬/春/夏/秋）。

---

## 场景一：新增一个季度（数据库里没有该季度）

**触发时机**：新季度开播，上游数据源已有番组列表，需要把该季度首次写入数据库。

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
1. 在 `seasons` 表里 upsert 该季度记录
2. 从上游 SeasonDataSource 拉取该季度的番组 ID 列表
3. 调和 `season_subjects` 关联表（新增关联，删除消失的关联）
4. 逐个调用 Bangumi API 拉取每部番组的评分、集数评论数等详细信息并写入 `subjects` 表

**成功响应**（`201 Created`）：

```json
{
  "season_id": 202604,
  "subjects_added": 42,
  "subjects_removed": 0,
  "subjects_updated": 42,
  "subjects_failed": 0
}
```

**注意**：`subjects_failed > 0` 不是致命错误，失败的条目是 Bangumi API 单次拉取超时或限流，数据还在，下次调度器定时 tick 会补全。

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

**它做了什么**（与场景一的新建流程完全相同，只跳过初次写入 season 记录这一步）：
1. 从上游 SeasonDataSource 重新拉取番组 ID 列表
2. 调和 `season_subjects` 关联（上游新增的加进来，上游移除的删掉）
3. 逐个调用 Bangumi API 刷新所有番组的评分、评论数等数据

**调和逻辑**：
- 上游有、数据库没有 → `subjects_added`，新建关联并拉取 Bangumi 数据
- 数据库有、上游没有 → `subjects_removed`，删除 `season_subjects` 关联（番组本体保留在 `subjects` 表）

响应同场景一的成功响应格式。这是同步操作，等待完成后返回。

因此，**「刷新数据」和「重新对齐番组列表」是同一个操作**，无需区分。

**验证结果**：

```
GET /api/seasons/202604/subjects
```

返回该季度当前的番组列表，按 `rank` 升序排列，`rank` 为空的排在最后。

---

## 场景三：手动从季度中移除某部番组

**触发时机**：某番组被错误地归入了本季度，需要单独移除。

先找到番组 ID（可从公开接口的响应里拿）：

```
DELETE /admin/seasons/202604/subjects/123456
```

**注意**：这只是删除「关联关系」，不会删除 `subjects` 表里的番组本体。该番组会变成孤立番组（orphan），不再属于任何季度。

---

## 场景四：清理孤立番组

**触发时机**：删除季度或移除番组关联后，`subjects` 表里存在不属于任何季度的番组，占用空间且无意义。

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

**注意**：`DELETE /admin/seasons/{id}` 删除整个季度时会自动清理孤立番组，无需手动操作这一步。

---

## 场景五：删除整个季度

**触发时机**：某季度数据错误，需要彻底重来；或测试数据需要清除。

```
DELETE /admin/seasons/202604
```

**它做了什么**：
1. 删除 `season_subjects` 关联表中该季度的所有关联
2. 删除 `seasons` 表里该条记录
3. **自动**清理因此产生的孤立番组

之后按场景一重新创建即可。

---

## 场景六：手动修正某部番组的数据

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

## 场景七：修改季度别名

```
PATCH /admin/seasons/202604
Content-Type: application/json

{
  "name": "2026春季（修正版）"
}
```

---

## 场景八：一次性同步全部季度的数据

**触发时机**：服务重启后想立即刷新所有季度，不等调度器。

```
POST /admin/seasons/sync-all
```

**注意**：异步执行（`202 Accepted`），日志里可以看进度。此操作对所有季度重新全量 sync，耗时较长，非必要不要频繁调用。

---

## 场景九：手动触发 Cloudflare Pages 部署

**触发时机**：数据已经更新，但部署 hook 没自动触发（网络异常等情况），需要手动推一次。

```
POST /admin/deploy
```

---

## 附录：接口速查表

| 用途 | 方法 | 路径 |
|------|------|------|
| 健康检查 | GET | `/health` |
| 列出所有季度 | GET | `/api/seasons` |
| 查看季度番组列表 | GET | `/api/seasons/{id}/subjects` |
| **新建季度**（首次） | POST | `/admin/seasons` |
| **重新同步季度数据** | POST | `/admin/seasons/{id}/sync` |
| 同步全部季度 | POST | `/admin/seasons/sync-all` |
| 查看季度详情（Admin） | GET | `/admin/seasons/{id}` |
| 修改季度别名 | PATCH | `/admin/seasons/{id}` |
| 删除整个季度 | DELETE | `/admin/seasons/{id}` |
| 从季度移除番组 | DELETE | `/admin/seasons/{id}/subjects/{sid}` |
| 查看番组详情（Admin） | GET | `/admin/subjects/{id}` |
| 手动修改番组字段 | PATCH | `/admin/subjects/{id}` |
| 删除番组 | DELETE | `/admin/subjects/{id}` |
| 查看孤立番组 | GET | `/admin/subjects/orphans` |
| 清理孤立番组 | DELETE | `/admin/subjects/orphans` |
| 手动触发调度器 | POST | `/admin/scheduler/trigger` |
| 查看调度器状态 | GET | `/admin/scheduler/status` |
| 手动触发部署 | POST | `/admin/deploy` |
