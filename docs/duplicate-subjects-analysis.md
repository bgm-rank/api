# 跨季度重复番组及 `media_type` / `rating` 覆盖问题分析

## 1. 核心结论

上游 `season-data` 以 **MAL 条目（`mal_id`）** 为粒度导出 `media_type` 和 `rating`，允许不同季度（甚至同季度）的多个 MAL 条目映射到同一个 **Bangumi 条目（`bgm_id`）**；但后端 `bgm-rank-api` 数据库却把 `media_type` 和 `rating` 存成了 **`subjects` 表（以 `bgm_id` 为主键）的全局唯一字段**，导致跨季/同季同步时发生 **“后写覆盖（Last-Write-Wins）”**，进而触发前端默认筛选器误杀正片或导致旧番串季上榜。

---

## 2. 问题产生的三层原因

### 2.1 上游数据源层：MAL 与 Bangumi 条目粒度不一致（N:1 映射）

在 `season-data/scripts/publish_release.py` 中，`season-data.json` 导出的每条记录结构为 `{ bgm_id, media_type, rating }`，但其中的 **`media_type` 和 `rating` 来自 MAL（`mal_anime` 表），而非 Bangumi 条目本身**。

1. **跨季合法合并**：MAL 常把正片 TV、番外 ONA/OVA、总集篇 SP、无修版拆成不同季度的独立 `mal_id`（各自具有不同的 `media_type` 或 `rating`），而 Bangumi 常将番外/SP 附属在正片同一个主条目 `bgm_id` 下。
   - **数据库实例**：
     - `426156`（《至高之牌 第二季》）：同时关联 `202401`（正片 `tv`）和 `202410`（第 25 话番外 `ona`），共用同一 `bgm_id`。
     - `527620`（《章鱼哔的原罪》）：同时关联 `202504` 和 `202507`。
     - `470738`（《极速车魂 第二季》）：同时关联 `202401` 和 `202410`。
2. **匹配撞车**：自动或人工审核时，可能将续作、衍生作、PV/CM 误绑到了正片 `bgm_id`。虽然 `season-data/src/api/quality.py` 提供了 `dup_global` 和 `dup_in_season` 质量提示，但并非数据库硬约束，发布脚本仍会将所有 `included` 条目原样导出。

### 2.2 后端存储与同步层：属性放错表 + 后写覆盖

1. **表结构失配（`migrations/001_create_schema.sql`）**：
   - `subjects` 表以 `id`（即 `bgm_id`）为主键，全局只有一行，却承载了 `media_type` 和 `rating` 字段。
   - `season_subjects` 关联表仅保存 `(season_id, subject_id, added_at)`，**没有记录该番组在当季对应的 `media_type` 和 `rating`**。
2. **同步互相踩踏（`src/core/sync/mod.rs` & `src/dal/repositories/subject_repository.rs`）**：
   - **全量对账 `reconcile_all`**：按 `season_id` **从新到旧倒序**遍历所有季度，逐季调用 `upsert_meta_batch`，通过 `COALESCE(EXCLUDED.media_type, subjects.media_type)` 直接覆盖 `subjects` 表上的全局值（因此全量对账跑完后，往往是**最老季度**的值留在表里；而若新季度后加入并单独同步，又会被新季度覆盖）。
   - **单季同步 `sync_season_data`**：调用 `subject_repo.upsert` 同样直接覆盖 `subjects` 表的 `media_type` 和 `rating`（最后同步的季度胜出；同季存在重复 `bgm_id` 时，数组中靠后的条目胜出）。
3. **附带隐患（详情置空风险）**：
   - `sync_season_data` 在同步开头用 `CreateSubject { id, media_type, rating, ..Default::default() }` 调用 `subject_repo.upsert`，其 `ON CONFLICT DO UPDATE` 会把已有条目的 `name`、`rank`、`score` 等详情字段先清空为 `NULL`，再依赖后续循环调用 `hydrate_subject` 从 Bangumi 重拉补回。若期间 Bangumi 接口超时失败，其他共享该 `bgm_id` 的季度也会一并丢失番组详情。

### 2.3 前端展示层：默认过滤机制放大异常

前端排行榜组件（`web/src/components/sections/AnimeRank.astro`）在客户端默认只勾选：
- `DEFAULT_MEDIA = new Set(["tv"])`
- `DEFAULT_RATINGS = new Set(["general"])`

由此引发两类直接可见的显示故障：

1. **当季正片“凭空消失”**：
   - 例如《至高之牌 第二季》（`426156`）在 `202401` 本是正统 `tv` 新番，但因 `subjects.media_type` 被 `202410` 的番外覆盖成了 `ona`，用户打开 `2024年01月` 默认榜单（仅展示 `TV` + `General`）时，该番组被直接过滤隐藏。
   - 若某正片被关联的 SP/PV 覆盖为 `tv_special`、`special`、`pv`、`cm`、`music`，由于前端顶部筛选栏仅提供 `TV / OVA / ONA / Movie` 四个复选框，用户甚至无法通过勾选分类找回该番组。
2. **跨季“伪新番”串季霸榜与封面污染**：
   - 反过来，若跨季共享条目的全局 `media_type` 处于 `tv` + `general` 状态，那么在仅播出一集番外/OVA 的新季度里，该旧番会带着 Bangumi 主条目的**全站总评分、总收藏和总排名**直接进入新季度默认 TV 榜。
   - 同时后端 `GET /api/seasons/top1`（`src/core/query/mod.rs`）选季度第一名时未限制 `media_type`，跨季高分旧番可能直接占据新季度的季度封面图，并拉偏前端计算的当季前四高亮阈值（收藏数、话均评论、抛弃率）。

---

## 3. 后续修复方向建议

### 3.1 `api` 侧（消除属性覆盖与同步隐患）
1. **字段下沉至关联表**：将 `media_type` 和 `rating` 迁移/冗余到 `season_subjects` 表（按 `(season_id, subject_id)` 存储当季属性），`QueryService::get_season_subjects` 与 `get_all_seasons_top1` 改为读取 `season_subjects` 上的当季 `media_type` / `rating`。
2. **同季重复 `bgm_id` 优先级归并**：同一季度内若出现多条相同 `bgm_id` 的记录，入库对账前按固定优先级合并（例如 `tv > movie > ova > ona > tv_special > special > music > pv > cm`），避免同季先行 PV 或 SP 覆盖正片属性。
3. **修复 `sync_season_data` 写库方式**：将 `sync_season_data` 第 3 步的逐条 `subject_repo.upsert` 替换为 `upsert_meta_batch`，避免在拉取详情前把已有番组详情刷成 `NULL`。

### 3.2 `season-data` 侧（从源头治理跨季附属条目）
1. **明确跨季合并条目的收录规则**：当某季度的 OVA/ONA/SP/PV 在 Bangumi 上没有独立条目、只能映射到**往季已收录的正片主条目 `bgm_id`** 时，由于 Bangumi 侧并无该番外的独立评分与排名，建议在 `season-data` 审核或 `publish_release.py` 导出阶段将其排除（或仅保留首次正片所在季度），从源头避免“旧番借番外在新季度重复上榜”。
