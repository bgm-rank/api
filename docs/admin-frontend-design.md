# `bgm-rank` 本地管理工作台前端设计规范 (`admin`)

> 基于 `ui-ux-pro-max` 设计智能系统制定（Style: **Minimalism & Swiss Style** | Density: **8/10 Dense Dashboard** | Motion: **3/10 Subtle** | Variance: **5/10 Balanced Modern**）。
> 定位：**仅在本地运行的极速数据审核与发布工作台**，通过顶栏环境开关一键切换连接本地开发后端 (`localhost:3000`) 与 Fly.io 生产后端。

---

## 1. 工程与技术选型

彻底抛弃旧版 `season-data/web` 笨重的 Astro SSR + 多 Island + 自定义 EventBus 架构，采用纯正、跟手的 **Local-First SPA** 技术栈：

- **构建与运行时**: `Vite 6` + `React 19` + `TypeScript 5.8`（纯客户端 SPA，无需 Node SSR 容器，冷启动 < 200ms）。
- **数据流与缓存**: `@tanstack/react-query v5`
  - 内置乐观更新（Optimistic Updates）：按下通过/排除快捷键的 **0ms 内** UI 立即切往下一条，后台异步落库，失败自动回滚并吐司提示。
  - 自动请求取消（Query Cancellation）：长按 `j/k` 极速浏览时自动 abort 未完成的详情请求，彻底根除旧版手写 `detailSeq` 计数器。
- **UI 原语与样式**: `Tailwind CSS v4` + `shadcn/ui`（Radix UI 原语，重点使用 `<Command>`、`<Table>`、`<Badge>`、`<Tooltip>`、`<ResizablePanelGroup>`）。
- **图标系统**: **Phosphor Icons (`@phosphor-icons/react`)**
  - 严格遵守 `ui-ux-pro-max` 规范：**严禁使用任何 Emoji（如 🚀、⚙️、🔗）充当结构性图标**。
  - 统一使用 `1.5px` (Regular) 描边风格，选中态切换为 `Fill` 变体，尺寸严格锁定在 Token 阶梯：`icon-sm (16px)`、`icon-md (20px)`、`icon-lg (24px)`。

---

## 2. 视觉与设计系统 (Design System Tokens)

采用 **Swiss Grid（瑞士网格）+ Linear 风格高密度暗色优先（Dark-first）** 视觉语言，依靠精准的 `1px` 边框、字重对比和语义色彩传达信息，避免冗余阴影与卡片套娃。

### 2.1 色彩语义 (Color Tokens)

支持 Light / Dark 双模式，默认推荐使用 **IDE Dark 模式** 以降低长时间审番的视觉疲劳：

| 语义角色 | CSS 变量 | Dark Mode (默认) | Light Mode | 用途说明 |
| :--- | :--- | :--- | :--- | :--- |
| **Background** | `--bg-canvas` | `#090D16` (Slate 950+) | `#F8FAFC` | 全局底色 |
| **Surface / Card** | `--bg-surface` | `#0F172A` (Slate 900) | `#FFFFFF` | 面板、分栏容器底色 |
| **Elevated / Hover**| `--bg-elevated` | `#1E293B` (Slate 800) | `#F1F5F9` | 选中行、候选卡悬停、Command 浮层 |
| **Border** | `--border-subtle`| `#1E293B` / `#334155` | `#E2E8F0` | 1px 瑞士网格分割线 |
| **Text Primary** | `--text-primary` | `#F8FAFC` (对比度 >15:1)| `#0F172A` | 主标题、日文原名、核心正文 |
| **Text Muted** | `--text-muted` | `#94A3B8` (对比度 >5.5:1)| `#475569` | 辅助元数据、英文译名、标签 |
| **Included / Pass** | `--accent-pass` | `#22C55E` (Green 500) | `#16A34A` | 确认收录、Exact 匹配、Local 环境指示 |
| **Pending / Warn** | `--accent-warn` | `#F59E0B` (Amber 500) | `#D97706` | 待人工复核、日期/平台硬冲突警告 |
| **Excluded / Danger**| `--accent-danger`| `#EF4444` (Red 500) | `#DC2626` | 排除不收录、错误状态、**Prod 环境警示边框** |
| **Shared BGM Link** | `--accent-info` | `#38BDF8` (Sky 400) | `#0284C7` | 多对一（跨季/拆季共用 `bgm_id`）提示 |

### 2.2 字体排印 (Typography — 中日英与等宽混排优化)

针对新番审核高频出现的**日文汉字/假名、中文译名、数字 ID 与放送日期对比**，采用三字体栈：

- **UI 与中日文正文 (`--font-sans`)**: `'Inter', 'Noto Sans JP', 'PingFang SC', system-ui, sans-serif`
  - 保证日文假名、JIS 汉字与简体中文在同一行内基线对齐、字重统一。
- **标题与季度标识 (`--font-display`)**: `'Outfit', 'Noto Sans JP', sans-serif`
- **数据、ID、日期、置信度 (`--font-mono`)**: `'JetBrains Mono', monospace`（开启 `font-variant-numeric: tabular-nums`）
  - 所有 `mal_id`、`bgm_id`、放送日期 `2026-01-04`、置信度 `0.92`、排名 `#12` 强制使用等宽字体，列表纵向扫视时数字严格对齐。

### 2.3 高密度间距阶梯 (Density 8/10 Scale)

摒弃营销页的大留白，采用紧凑的 `4px` 基准网格：
- 行高与内边距：列表单行高度固定 `36px`（紧凑单行）或 `48px`（双行对照），面板内边距 `12px (p-3)` ~ `16px (p-4)`，卡片间距 `8px (gap-2)`。
- 动效规范：所有焦点切换与状态变更过渡控制在 `120ms ease-out`，严禁任何会导致布局抖动（Layout Shift）的缩放动画；支持 `prefers-reduced-motion`。

---

## 3. 全局布局与安全护栏（Local vs. Prod 环境切换）

整个应用采用 **100vh 无页面级滚动条** 的 IDE 级三段式工作区布局：

```text
+---------------------------------------------------------------------------------------------------+
| [BGM-RANK CONSOLE]   Season: [2026 Winter (Pending: 6) v]  |  [Fetch MAL] [Match SSE] [Sync BGM]  |
|                      Tab: (•) Triage Inbox (6)  ( ) Included (48)  ( ) All (215)   | [PROD: fly.io]|
+-----------------------------------+---------------------------------------------------------------+
| Queue (6 Pending)     [Filter /]  |  COMPARISON & DECISION WORKSPACE                              |
|-----------------------------------|  +---------------------------+-----------------------------+  |
| > 58102  葬送のフリーレン 2期      |  | MAL #58102                | BGM #515759 [Candidate #1]  |  |
|   TV · PG-13 · LLM 0.78  [!Date]  |  | 葬送のフリーレン 第2期     | 葬送的芙莉莲 第二季          |  |
|-----------------------------------|  | Frieren Season 2          | 葬送のフリーレン 第2期       |  |
|   59201  俺だけレベルアップ...     |  | 2026-01-10 | TV | 24 eps  | 2026-01-10 | TV | Score 8.4 |  |
|   TV · R · Pending                |  +---------------------------+-----------------------------+  |
|-----------------------------------|  | [i] Shared BGM ID: Linked to 2026-Summer (mal #60112)   |  |
|   60312  謎の韓国アニメ            |  +---------------------------------------------------------+  |
|   ONA · G · No Match              |  | CANDIDATES (1-3) & LIVE BGM SEARCH (/)                  |  |
|                                   |  | [1] #515759 葬送のフリーレン 第2期 (0.94) - "续作标题一致"|  |
|                                   |  | [2] #400602 葬送のフリーレン (0.41) [!Date: 2023-09]     |  |
|                                   |  | [Search BGM keyword or paste bgm.tv/subject/...       ] |  |
|                                   |  +---------------------------------------------------------+  |
|                                   |  | ACTION BAR                                              |  |
|                                   |  | [Enter: Include #515759]  [X: Exclude...]  [M: Move...] |  |
+-----------------------------------+---------------------------------------------------------------+
```

### 3.1 顶栏环境切换器与防误触设计 (Environment Switcher)
因为你需要直接在本地前端连 Fly.io 生产后端操作，**环境感知的视觉显著度必须拉满**：
- **🟢 Local Mode (`http://localhost:3000`)**：顶栏正常深灰底色，显示绿色圆点徽标 `LOCAL`。
- **🔴 Production Mode (`https://<app>.fly.dev`)**：
  - 切换至 Prod 时，**整个窗口顶部出现一条 `2px` 的琥珀红警示顶边 (`border-t-2 border-red-500`)**，环境徽标变为高对比度实心红底 `PROD · FLY.IO`。
  - 配置面板支持分别保存 `LOCAL_ADMIN_TOKEN` 与 `PROD_ADMIN_TOKEN`（持久化在 `localStorage`），请求拦截器自动根据当前激活环境注入对应的 `BaseURL` 与 `Authorization: Bearer <token>`。
  - 顶栏最右侧常驻 **`[Deploy Pages]`** 按钮（带 Phosphor ` RocketLaunch` 图标）：审核完当季数据后一键触发 Cloudflare Pages 构建。

---

## 4. 核心工作流重设计：围绕「三步走」的三大视图

摒弃旧版把所有状态揉在一个列表里的做法，按审核心智拆成三个正交视图（快捷键 `Alt+1` / `Alt+2` / `Alt+3` 切换）：

### 4.1 视图一：Triage Inbox（待审焦点台 —— 95% 时间停留处）

默认只过滤出 **`status = 'pending'`** 或 **包含质量告警（日期冲突、同季撞车）** 的条目。

#### 1. 左侧：待审队列 (`<ResizablePanel defaultSize={28}>`)
- 仅展示紧凑双行卡片：
  - 第一行：`mal_id (mono)` + `日文原名 (truncate)` + 右侧状态角标。
  - 第二行：`media_type` 胶囊 + `原始 rating` + 冲突徽标（如 `Date Δ8m`、`Platform`）。
- 键盘 `j` / `k` 或 `↓` / `↑` 上下移动焦点，选中行左侧亮起 `3px` 主色竖条，滚动条自动平滑跟随 (`scrollIntoView({ block: 'nearest' })`)。

#### 2. 右侧上半区：MAL vs. BGM 对照解剖台 (Side-by-Side Diff Grid)
采用严格的 **4 行对齐网格**，视线左右平移即可完成比对：

| 对比维度 | 左栏：MAL 源数据 (`mal_anime`) | 右栏：当前预览的 BGM 候选 (`subjects`) | 视觉校验辅助 |
| :--- | :--- | :--- | :--- |
| **1. 标题与外链** | `title_ja`（主） / `title`（副）+ 外链图标 | `name`（主） / `name_cn`（副）+ 外链图标 | 完全相同的字符高亮显示 |
| **2. 放送日期** | `start_date` (如 `2026-01-04`) | `air_date` (如 `2026-01-04`) | 相差 $\le 1$ 个月标绿；相差 $>6$ 个月标红并显示 `Δ X个月` |
| **3. 类型与分级** | `media_type` (`tv`) · 原始 `rating` (`pg_13` → `general`) | `platform` (`TV`) · `nsfw` (`false`) | 若 `movie` 对应 `TV` 直接亮起红色互斥徽标 |
| **4. 规模与简介** | `num_episodes` · `source` · `studios` · 折叠简介 | `eps` · `score` · `rank` · `tags` 前 5 项 | 辅助判断是否为总集篇、SP 或剧场版 |

- **跨季/拆季共用 `bgm_id` 提示条（解决多对一痛点）**：
  当右侧预览的 `bgm_id` 已经在库里被其他 `mal_id` 绑定过时，对照台正下方自动展开一条蓝色信息横幅（Phosphor `Link` 图标）：
  > `已关联条目：该 BGM ID (#515759) 同时被 [2025-Fall · mal#54321 某某动画 Part 1] 绑定`
  让你一眼分辨这是**「MAL 拆季、BGM 合并的正常多对一」**，还是**「第一季与第二季误撞车的错配」**。

#### 3. 右侧中半区：LLM 候选列表 + 内嵌 `<Command>` 实时搜索
- **候选列表**：展示 `mal_bgm_matches.candidates` 中的候选条目，直接渲染 LLM 给出的判定理由 `reason`（若模型判为全不匹配，明确显示 `LLM 已审：认为候选均不匹配`，不再像旧版那样留空）。按数字键 **`1` / `2` / `3`** 瞬间切换上方右栏预览的候选对象。
- **内嵌Bangumi 搜索与智能粘贴（基于 shadcn `<Command>`）**：
  - 按 **`/`** 键直接聚焦搜索框，输入关键词实时防抖调用 `GET /admin/bgm/search?q=`。
  - **智能粘贴识别**：直接粘贴 `515759` 或完整网址 `https://bgm.tv/subject/515759`，自动提取 `bgm_id` 并拉取预览。

#### 4. 右侧底栏：三向分流动作栏（彻底解耦三种人工意图）
将旧版混杂在 Exclude 里的操作拆为三个语义极其纯粹的动作，**任一动作触发后乐观更新移除当前项，焦点自动落入下一条待审项**：

1. **确认收录 (`[Enter]` 或 `[Space]`)**
   - 绑定当前高亮的 `bgm_id`，写入 `mal_bgm_matches (status='included', source='human')`，后端立即异步拉取该番剧的排行与剧集评论。
2. **不收录 / 排除 (`[X]` 键 → 数字键选原因)**
   - 按 `X` 弹出行内紧凑的排除原因ピッカー（支持直接按 `1`~`6` 秒选并提交）：
     - `[1] not_on_bgm`（BGM 未建条目）
     - `[2] kids`（少儿/子供向）
     - `[3] mini`（泡面/短片/动态漫）
     - `[4] merged_into_ep`（被 BGM 并入本篇 SP 章节）
     - `[5] not_anime`（MV/CM/三次元）
     - `[6] other`（聚焦备注输入框）
3. **MAL 标错季度 · 跨季移番 (`[M]` 键)**
   - 专门对应第一步的 `season_mal` 表操作！
   - 按 `M` 弹出季度快速选择器，置顶显示 **`[← 上一季 2025 Fall]`** 与 **`[→ 下一季 2026 Spring]`**（也可搜索任意季度）。
   - 选中后调用 `POST /admin/seasons/{id}/mal/{mal_id}/move`：本季 `season_mal.is_removed = 1`，目标季插入 `season_mal` 记录。**如果该番已有绑定的 `bgm_id`，匹配关系自动带过去，无需重审！**

---

### 4.2 视图二：Included Preview（本季排行榜实时预览台）

当你清空 Triage Inbox 后，按 `Alt+2` 切换到此视图，所见即公网前端最终渲染的结果：

- 使用语义化 shadcn `<Table>`，直接调用与公开接口同源的预览查询（已按 `bgm_id` 去重合并，并应用了 `map_mal_rating` 字典转换）。
- **多对一聚合展示**：如果同一个季度（或跨季）有多个 `mal_id` 指向这一行的 `bgm_id`，在条目标题旁显示可展开的 `2 MAL IDs` 徽标，点击可查看或解绑单个 `mal_id`。
- **行内微调与补番**：
  - 表格顶部提供 **`[+ 手动补入 MAL ID]`** 按钮：输入 `mal_id` 直接抓入本季 `season_mal`。
  - 单行右侧提供「重刷 BGM 分数」按钮与「移出本季」按钮。

---

### 4.3 视图三：All Season Items（全量审计与批量重跑视图）

对应旧版的全部条目列表，用于排查历史季度问题（如 `todo.md` §7 的历史清理）：
- 顶部提供正交的多维 Filter Bar：
  - `Status`: `All` | `Included` | `Excluded` | `Pending`
  - `Source`: `All` | `Rule` | `Exact` | `LLM` | `Human`
  - `Issue`: `All` | `Dup in Season` | `Date Mismatch` | `Korean LLM`
- 支持多选（Checkbox + `Shift` 连选）后执行批量操作：**「批量打回 Pending 并重跑 LLM」**。

---

## 5. 全局键盘快捷键体系 (Zero-Mouse Workflow)

底部状态栏常驻显示当前上下文可用的快捷键提示（可通过 `?` 呼出完整快捷键面板）：

| 快捷键 | 作用域 | 动作说明 |
| :--- | :--- | :--- |
| `j` / `↓` | 列表导航 | 选中下一条番剧 |
| `k` / `↑` | 列表导航 | 选中上一条番剧 |
| `1` ~ `5` | 详情候选区 | 切换预览第 N 个 Bangumi 候选条目 |
| `Enter` | 审核决策 | **确认收录 (Include)** 当前预览的候选 `bgm_id`，并跳至下一条 |
| `x` | 审核决策 | 展开 **不收录 (Exclude)** 原因选择（随后按 `1`~`6` 立即提交并跳至下一条） |
| `m` | 季度归属 | 展开 **跨季移番 (Move Season)** 菜单（随后按 `←`/`→` 移至相邻季度） |
| `/` | 搜索 | 聚焦内嵌 Bangumi 实时搜索框 / 粘贴 `bgm_id` |
| `o` / `b` | 外部核验 | 在新标签页打开当前条目的 MAL 页面 (`o`) 或 Bangumi 页面 (`b`) |
| `Cmd/Ctrl + K`| 全局 | 呼出全局 Command Palette（快速切换季度、切换 Local/Prod 环境、触发 Deploy） |

---

## 6. 目录结构规划 (`../admin`)

```text
/home/yui/repos/others/bgm-rank/admin
├── index.html
├── package.json                # Vite + React 19 + TanStack Query + Tailwind v4 + Phosphor Icons
├── vite.config.ts
├── tsconfig.json
└── src/
    ├── main.tsx                # QueryClientProvider + 全局快捷键挂载
    ├── App.tsx                 # 顶栏 + Resizable 三段式主框架
    ├── lib/
    │   ├── env-store.ts        # Local (localhost:3000) / Prod (fly.io) 环境与 Token 切换存储
    │   ├── api-client.ts       # 统一 Fetch 封装（自动注入当前环境的 BaseURL 与 Bearer Token）
    │   └── rating-map.ts       # MAL 原始 rating -> general/kids/r18 预览字典
    ├── hooks/
    │   ├── use-review-queue.ts # TanStack Query 列表查询 + 乐观更新 Mutation
    │   ├── use-sse-task.ts     # 消费后端 /match 与 /sync-bgm 的 SSE 实时进度流
    │   └── use-hotkeys.ts      # 键盘流快捷键绑定
    └── components/
        ├── TopBar.tsx          # 季度选择器、任务触发按钮、Local/Prod 警示切换器、Deploy 按钮
        ├── TriageInbox/
        │   ├── QueueList.tsx   # 左栏紧凑待审列表
        │   ├── DiffGrid.tsx    # 右上：MAL vs BGM 四行对齐解剖台 + 多对一关联提示条
        │   ├── CandidateBox.tsx# 右中：LLM 候选切换 + shadcn <Command> 实时搜索/粘贴
        │   └── ActionBar.tsx   # 右下：[Enter 收录] [X 排除原因] [M 跨季移番]
        ├── IncludedTable.tsx   # Tab 2：按 bgm_id 聚合去重后的本季排行榜预览表
        └── AuditTable.tsx      # Tab 3：全量多维筛选与批量打回重跑表
```
