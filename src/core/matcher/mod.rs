//! MAL -> Bangumi 四层匹配管线（规则 / 搜索 / 精确 / LLM）。
//!
//! 只通过 `MalBgmMatchRepository` 写匹配列；已 included/excluded 的决策绝不覆盖。

use std::fmt;
use std::sync::{Arc, LazyLock};

use anyhow::{Context, Result, anyhow};
use regex::Regex;
use unicode_normalization::UnicodeNormalization;

use crate::core::fetch::parse_season_id;
use crate::core::sync::SyncService;
use crate::dal::Database;
use crate::dal::dto::{CreateSubject, MalAnime, MalBgmMatch, UpdateMalBgmMatch};
use crate::dal::repositories::{
    MalAnimeRepository, MalBgmMatchRepository, SeasonMalRepository, SubjectRepository,
};
use crate::services::bangumi::{BangumiClient, Subject as BgmSubject};
use crate::services::llm::{BgmCandidate, LlmMatch, LlmMatcher, MalBrief, Suggest};

pub const LLM_CONFIDENCE_THRESHOLD: f64 = 0.85;
const DATE_CONFLICT_MONTHS: i32 = 6;

pub const CONFLICT_DATE: &str = "date";
pub const CONFLICT_PLATFORM: &str = "platform";

/// 直接排除的 MAL media_type（`unknown` 在源头拦截）。
const SKIP_MEDIA_TYPES: [&str; 6] = ["pv", "cm", "music", "special", "tv_special", "unknown"];

// ───────────────────────────── 纯函数 ─────────────────────────────

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("valid regex")
}

static SUFFIX_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    const CJK: &str = "一二三四五六七八九十百";
    vec![
        re(&format!(r"\s*第[\d{CJK}]+[期季]$")),
        re(r"\s*第\d+クール$"),
        re(r"(?i)\s+Season\s*\d+$"),
        re(r"(?i)\s+\d+(?:st|nd|rd|th)\s+Season$"),
        re(r"\s+最終章$"),
        re(r"\s*[（(]新章[）)]$"),
        re(r"\s*「[^」]+」$"),
        re(r"\s*～[^～]+～$"),
        re(r"\s*〜[^〜]+〜$"),
        re(r"[\s　]+-[^-]+-$"),
        re(r"\s+劇場版$"),
        re(r"^劇場版\s+"),
        re(r"^映画\s+"),
        re(r"^新劇場版\s+"),
        re(&format!(r"\s+[\d{CJK}]+期$")),
        re(r"\d+$"),
        re(r"(?i)\s+Part\s*\d+$"),
        re(r"\s+[IVX]+$"),
    ]
});

static HANGUL_RE: LazyLock<Regex> =
    LazyLock::new(|| re("[\u{AC00}-\u{D7A3}\u{1100}-\u{11FF}\u{3130}-\u{318F}]"));
static KANA_RE: LazyLock<Regex> = LazyLock::new(|| re("[\u{3040}-\u{30FF}\u{FF66}-\u{FF9F}]"));
static YM_RE: LazyLock<Regex> = LazyLock::new(|| re(r"^(\d{4})(?:[-/](\d{1,2}))?"));

/// NFKC 归一化 + trim，用于标题精确比较。
pub fn normalize_title(s: &str) -> String {
    s.nfkc().collect::<String>().trim().to_string()
}

/// 谚文标题：含谚文且不含假名（含假名的是带韩文副标的日本动画）。
pub fn is_korean_title(title: Option<&str>) -> bool {
    match title {
        Some(t) if !t.is_empty() => HANGUL_RE.is_match(t) && !KANA_RE.is_match(t),
        _ => false,
    }
}

/// 需要在规则层直接排除的 media_type。
pub fn should_skip_media_type(media_type: &str) -> bool {
    SKIP_MEDIA_TYPES.contains(&media_type.trim().to_lowercase().as_str())
}

/// ona + g/pg：匹配全败时 BGM 上基本不存在对应条目。仅限收尾兜底，不可前置。
pub fn is_hopeless_ona_kids(media_type: &str, rating: Option<&str>) -> bool {
    media_type == "ona" && matches!(rating, Some("g") | Some("pg"))
}

fn parse_ym(date: Option<&str>) -> Option<(i32, Option<i32>)> {
    let caps = YM_RE.captures(date?.trim())?;
    let year: i32 = caps[1].parse().ok()?;
    let month = caps
        .get(2)
        .and_then(|m| m.as_str().parse::<i32>().ok())
        .filter(|m| (1..=12).contains(m));
    Some((year, month))
}

/// 代码侧硬规则：日期差 > 6 个月、MAL media_type 与 BGM platform 明显互斥。
/// 返回冲突代码（`date` / `platform`），写入 candidates JSON 供前端展示。
pub fn match_conflicts(
    mal_start_date: Option<&str>,
    mal_media_type: Option<&str>,
    bgm_date: Option<&str>,
    bgm_platform: Option<&str>,
) -> Vec<&'static str> {
    let mut conflicts = Vec::new();

    if let (Some((my, mm)), Some((by, bm))) = (parse_ym(mal_start_date), parse_ym(bgm_date)) {
        match (mm, bm) {
            (Some(mm), Some(bm)) => {
                if ((my * 12 + mm) - (by * 12 + bm)).abs() > DATE_CONFLICT_MONTHS {
                    conflicts.push(CONFLICT_DATE);
                }
            }
            // 只精确到年时差 1 年可能只差 1 个月，差 >= 2 年才确定超阈值
            _ => {
                if (my - by).abs() >= 2 {
                    conflicts.push(CONFLICT_DATE);
                }
            }
        }
    }

    if let (Some(mt), Some(pf)) = (mal_media_type, bgm_platform) {
        let mt = mt.trim().to_lowercase();
        let pf = pf.trim();
        let hit = (mt == "movie" && matches!(pf, "TV" | "WEB"))
            || ((mt == "tv" || mt == "ona") && matches!(pf, "剧场版" | "劇場版"));
        if hit {
            conflicts.push(CONFLICT_PLATFORM);
        }
    }

    conflicts
}

/// 剥离后缀 / 取空格后半段生成备选搜索词。
pub fn alternative_keywords(title: &str) -> Vec<String> {
    let mut results: Vec<String> = Vec::new();
    let mut seen: Vec<String> = vec![title.to_string()];
    let mut add = |kw: &str, results: &mut Vec<String>| {
        let kw = kw.trim();
        if kw.chars().count() >= 2 && !seen.iter().any(|s| s == kw) {
            results.push(kw.to_string());
            seen.push(kw.to_string());
        }
    };

    let mut stripped = title.to_string();
    loop {
        let mut changed = false;
        for pat in SUFFIX_PATTERNS.iter() {
            let new = pat.replace(&stripped, "").trim().to_string();
            if new != stripped && new.chars().count() >= 2 {
                stripped = new;
                changed = true;
                break;
            }
        }
        if !changed {
            break;
        }
    }
    add(&stripped, &mut results);

    for sep in [' ', '　'] {
        if let Some(idx) = title.find(sep) {
            // Python 以字符数判断 idx >= 2
            if title[..idx].chars().count() >= 2 {
                let tail = title[idx + sep.len_utf8()..].trim();
                if tail.chars().count() >= 4 {
                    add(tail, &mut results);
                }
            }
            break;
        }
    }

    results
}

/// 季度的 BGM 搜索日期范围 `(start, end)`（`>=start` 且 `<end`，与 Python 一致）。
pub fn season_date_range(season_id: i32) -> Result<(String, String)> {
    let (year, _, season) = parse_season_id(season_id)?;
    Ok(match season {
        "winter" => (format!("{}-12-01", year - 1), format!("{year}-03-31")),
        "spring" => (format!("{year}-03-01"), format!("{year}-06-30")),
        "summer" => (format!("{year}-06-01"), format!("{year}-09-30")),
        _ => (format!("{year}-09-01"), format!("{year}-12-31")),
    })
}

// ───────────────────────────── 类型 ─────────────────────────────

/// 单条 MAL 的处理结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchOutcome {
    /// 已决策 / 已被处理过而短路
    Skipped,
    Included,
    Excluded,
    Pending,
    /// 管线异常（已写入 `error` 列）
    Failed,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MatchSummary {
    pub total: usize,
    pub skipped: usize,
    pub included: usize,
    pub excluded: usize,
    pub pending: usize,
    pub failed: usize,
}

impl MatchSummary {
    fn record(&mut self, o: MatchOutcome) {
        self.total += 1;
        match o {
            MatchOutcome::Skipped => self.skipped += 1,
            MatchOutcome::Included => self.included += 1,
            MatchOutcome::Excluded => self.excluded += 1,
            MatchOutcome::Pending => self.pending += 1,
            MatchOutcome::Failed => self.failed += 1,
        }
    }
}

impl fmt::Display for MatchSummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "total={} skipped={} included={} excluded={} pending={} failed={}",
            self.total, self.skipped, self.included, self.excluded, self.pending, self.failed
        )
    }
}

/// 搜索结果 + 本地已同步详情补全后的候选。
#[derive(Debug, Clone)]
struct Cand {
    id: i32,
    name: Option<String>,
    name_cn: Option<String>,
    date: Option<String>,
    summary: Option<String>,
    platform: Option<String>,
    tags: Option<Vec<String>>,
    nsfw: Option<bool>,
}

impl From<BgmSubject> for Cand {
    fn from(s: BgmSubject) -> Self {
        Self {
            id: s.id,
            name: s.name,
            name_cn: s.name_cn,
            date: s.date,
            summary: s.summary,
            platform: s.platform,
            tags: s
                .tags
                .map(|t| t.into_iter().filter_map(|t| t.name).collect()),
            nsfw: s.nsfw,
        }
    }
}

#[derive(serde::Serialize)]
struct CandidateJson {
    bgm_id: i32,
    bgm_name: Option<String>,
    air_date: Option<String>,
    confidence: Option<f64>,
    reason: Option<String>,
    conflicts: Vec<&'static str>,
}

fn candidates_json(items: &[CandidateJson]) -> Option<String> {
    if items.is_empty() {
        None
    } else {
        serde_json::to_string(items).ok()
    }
}

#[derive(Default)]
struct PipelineState {
    llm_ran: bool,
    errors: Vec<String>,
    first_subjects: Vec<Cand>,
}

// ───────────────────────────── 服务 ─────────────────────────────

pub struct MatcherService {
    db: Arc<Database>,
    bangumi: BangumiClient,
    sync: Arc<SyncService>,
    llm: Option<Arc<dyn LlmMatcher>>,
}

impl MatcherService {
    /// `llm` 为 None 时跳过 LLM 层（精确匹配未命中则写 pending/source NULL）。
    pub fn new(
        db: Arc<Database>,
        sync: Arc<SyncService>,
        llm: Option<Arc<dyn LlmMatcher>>,
    ) -> Self {
        Self::with_clients(db, BangumiClient::new(), sync, llm)
    }

    /// 注入自定义 Bangumi 客户端（测试指向 mock server）。
    pub fn with_clients(
        db: Arc<Database>,
        bangumi: BangumiClient,
        sync: Arc<SyncService>,
        llm: Option<Arc<dyn LlmMatcher>>,
    ) -> Self {
        Self {
            db,
            bangumi,
            sync,
            llm,
        }
    }

    /// 处理季度内所有未移除的 `season_mal`。单条失败不中断整体。
    pub async fn match_season(&self, season_id: i32, retry: bool) -> Result<MatchSummary> {
        parse_season_id(season_id)?;
        let rows = SeasonMalRepository::new(self.db.pool())
            .list_by_season(season_id, false)
            .await?;
        let mut summary = MatchSummary::default();
        for row in rows {
            let outcome = match self.match_one(row.mal_id, retry, Some(season_id)).await {
                Ok(o) => o,
                Err(e) => {
                    tracing::error!(season_id, mal_id = row.mal_id, error = %format!("{e:#}"), "match failed");
                    MatchOutcome::Failed
                }
            };
            summary.record(outcome);
        }
        Ok(summary)
    }

    /// 处理单个 `mal_id`；季度日期范围取其最新的未移除 `season_mal`。
    pub async fn match_mal(&self, mal_id: i32, retry: bool) -> Result<MatchOutcome> {
        let season = SeasonMalRepository::new(self.db.pool())
            .list_by_mal_id(mal_id)
            .await?
            .into_iter()
            .find(|r| !r.is_removed)
            .map(|r| r.season_id);
        self.match_one(mal_id, retry, season).await
    }

    async fn match_one(
        &self,
        mal_id: i32,
        retry: bool,
        season_id: Option<i32>,
    ) -> Result<MatchOutcome> {
        let pool = self.db.pool();
        let matches = MalBgmMatchRepository::new(pool);
        let mal = MalAnimeRepository::new(pool)
            .get(mal_id)
            .await?
            .ok_or_else(|| anyhow!("mal_id {} not found in mal_anime", mal_id))?;

        matches.insert_pending_ignore(mal_id).await?;
        let existing = matches
            .get(mal_id)
            .await?
            .ok_or_else(|| anyhow!("mal_bgm_matches row missing for {}", mal_id))?;

        // 1. 已决策短路
        match existing.status.as_str() {
            "included" => {
                self.hydrate_if_skeleton(&existing).await;
                return Ok(MatchOutcome::Skipped);
            }
            "excluded" => return Ok(MatchOutcome::Skipped),
            _ => {}
        }
        if existing.source.as_deref() == Some("human") {
            return Ok(MatchOutcome::Skipped);
        }
        if existing.source.is_some() && !retry {
            return Ok(MatchOutcome::Skipped);
        }

        // 2. 规则层：media_type
        if should_skip_media_type(&mal.media_type) {
            self.write(&existing, excluded("rule", None)).await?;
            return Ok(MatchOutcome::Excluded);
        }

        let range = match season_id {
            Some(id) => Some(season_date_range(id)?),
            None => None,
        };
        let korean = is_korean_title(mal.title_ja.as_deref());
        let keyword = mal
            .title_ja
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| mal.title.clone());

        let mut state = PipelineState::default();
        let subjects = match self.search_bgm(&keyword, range.as_ref()).await {
            Ok(s) => s,
            Err(e) => {
                let msg = format!("{e:#}");
                tracing::error!(mal_id, error = %msg, "BGM 搜索失败");
                self.write(
                    &existing,
                    UpdateMalBgmMatch {
                        status: "pending".into(),
                        error: Some(msg),
                        ..Default::default()
                    },
                )
                .await?;
                return Ok(MatchOutcome::Failed);
            }
        };
        let brief = mal_brief(&mal);

        if !subjects.is_empty() {
            state.first_subjects = subjects.clone();
            if let Some(o) = self
                .try_match(&existing, &mal, &brief, subjects, !korean, &mut state)
                .await?
            {
                return Ok(o);
            }
        }

        // 韩文护栏：精确未命中 -> 直接排除，不调用 LLM
        if korean {
            self.write(&existing, excluded("rule", None)).await?;
            return Ok(MatchOutcome::Excluded);
        }

        // LLM 建议搜索词重试
        if let Some(llm) = &self.llm {
            match llm.suggest_search(&keyword, &mal.media_type).await {
                Ok(Suggest::Keywords(kws)) => {
                    state.llm_ran = true;
                    for kw in kws {
                        let found = match self.search_dated_then_plain(&kw, range.as_ref()).await {
                            Ok(f) => f,
                            Err(e) => {
                                state.errors.push(format!("{e:#}"));
                                continue;
                            }
                        };
                        if found.is_empty() {
                            continue;
                        }
                        if state.first_subjects.is_empty() {
                            state.first_subjects = found.clone();
                        }
                        if let Some(o) = self
                            .try_match(&existing, &mal, &brief, found, true, &mut state)
                            .await?
                        {
                            return Ok(o);
                        }
                    }
                }
                Ok(Suggest::Skip) => state.llm_ran = true,
                Err(e) => {
                    tracing::error!(mal_id, error = %format!("{e:#}"), "LLM suggest 失败");
                    state.errors.push(format!("{e:#}"));
                }
            }
        }

        // 收尾兜底
        if is_hopeless_ona_kids(&mal.media_type, mal.rating.as_deref()) {
            self.write(&existing, excluded("rule", Some("kids")))
                .await?;
            return Ok(MatchOutcome::Excluded);
        }

        self.enrich(&mut state.first_subjects).await;
        let cands: Vec<CandidateJson> = state
            .first_subjects
            .iter()
            .map(|s| CandidateJson {
                bgm_id: s.id,
                bgm_name: s.name.clone(),
                air_date: s.date.clone(),
                confidence: None,
                reason: None,
                conflicts: cand_conflicts(&mal, s),
            })
            .collect();
        let error = (!state.errors.is_empty()).then(|| state.errors.join("; "));
        let has_error = error.is_some();
        self.write(
            &existing,
            UpdateMalBgmMatch {
                status: "pending".into(),
                // 仅在 LLM 完整跑过且无异常时落 source，保证短路只拦截「已判定」的条目
                source: (state.llm_ran && !has_error).then(|| "llm".to_string()),
                candidates: candidates_json(&cands),
                error,
                ..Default::default()
            },
        )
        .await?;
        Ok(if has_error {
            MatchOutcome::Failed
        } else {
            MatchOutcome::Pending
        })
    }

    // ── 搜索 ──

    async fn search_dated_then_plain(
        &self,
        keyword: &str,
        range: Option<&(String, String)>,
    ) -> Result<Vec<Cand>> {
        if let Some((s, e)) = range {
            let found = self.bangumi.search_anime(keyword, Some((s, e))).await?;
            if !found.is_empty() {
                return Ok(found.into_iter().map(Cand::from).collect());
            }
        }
        let found = self.bangumi.search_anime(keyword, None).await?;
        Ok(found.into_iter().map(Cand::from).collect())
    }

    /// 带日期范围 -> 不带 -> 剥后缀关键词重试。LLM 建议词在 `match_one` 中作为最后一级。
    async fn search_bgm(
        &self,
        keyword: &str,
        range: Option<&(String, String)>,
    ) -> Result<Vec<Cand>> {
        let found = self.search_dated_then_plain(keyword, range).await?;
        if !found.is_empty() {
            return Ok(found);
        }
        for alt in alternative_keywords(keyword) {
            let found = self.search_dated_then_plain(&alt, range).await?;
            if !found.is_empty() {
                return Ok(found);
            }
        }
        Ok(Vec::new())
    }

    // ── 精确 + LLM ──

    async fn try_match(
        &self,
        existing: &MalBgmMatch,
        mal: &MalAnime,
        brief: &MalBrief,
        mut subjects: Vec<Cand>,
        allow_llm: bool,
        state: &mut PipelineState,
    ) -> Result<Option<MatchOutcome>> {
        self.enrich(&mut subjects).await;

        // 精确层
        if let Some(ja) = mal.title_ja.as_deref().filter(|s| !s.is_empty()) {
            let ja_norm = normalize_title(ja);
            let hit = subjects
                .iter()
                .find(|s| s.name.as_deref().map(normalize_title).as_deref() == Some(&ja_norm));
            if let Some(hit) = hit {
                let conflicts = cand_conflicts(mal, hit);
                if conflicts.is_empty() {
                    self.ensure_skeleton(hit).await?;
                    self.write(
                        existing,
                        UpdateMalBgmMatch {
                            status: "included".into(),
                            source: Some("exact".into()),
                            bgm_id: Some(hit.id),
                            confidence: Some(1.0),
                            ..Default::default()
                        },
                    )
                    .await?;
                    return Ok(Some(MatchOutcome::Included));
                }
                let hit_id = hit.id;
                let cands: Vec<CandidateJson> = subjects
                    .iter()
                    .map(|s| CandidateJson {
                        bgm_id: s.id,
                        bgm_name: s.name.clone(),
                        air_date: s.date.clone(),
                        confidence: (s.id == hit_id).then_some(1.0),
                        reason: None,
                        conflicts: cand_conflicts(mal, s),
                    })
                    .collect();
                self.write(
                    existing,
                    UpdateMalBgmMatch {
                        status: "pending".into(),
                        source: Some("exact".into()),
                        candidates: candidates_json(&cands),
                        ..Default::default()
                    },
                )
                .await?;
                return Ok(Some(MatchOutcome::Pending));
            }
        }

        // LLM 层
        let Some(llm) = self.llm.as_ref().filter(|_| allow_llm) else {
            return Ok(None);
        };
        let llm_cands: Vec<BgmCandidate> = subjects
            .iter()
            .map(|s| BgmCandidate {
                bgm_id: s.id,
                name: s.name.clone().unwrap_or_default(),
                name_cn: s.name_cn.clone(),
                date: s.date.clone(),
                summary: s.summary.clone(),
                platform: s.platform.clone(),
                tags: s.tags.clone().filter(|t| !t.is_empty()),
            })
            .collect();
        let results: Vec<LlmMatch> = match llm.match_anime(brief, &llm_cands).await {
            Ok(r) => {
                state.llm_ran = true;
                r
            }
            Err(e) => {
                tracing::error!(mal_id = mal.mal_id, error = %format!("{e:#}"), "LLM 匹配失败");
                state.errors.push(format!("{e:#}"));
                return Ok(None);
            }
        };
        let known: Vec<&LlmMatch> = results
            .iter()
            .filter(|r| subjects.iter().any(|s| s.id == r.bgm_id))
            .collect();
        let Some(best) = known.iter().copied().max_by(|a, b| {
            a.confidence
                .unwrap_or(-1.0)
                .total_cmp(&b.confidence.unwrap_or(-1.0))
        }) else {
            return Ok(None);
        };
        let best_subject = subjects
            .iter()
            .find(|s| s.id == best.bgm_id)
            .expect("known");
        let conflicts = cand_conflicts(mal, best_subject);
        let cands: Vec<CandidateJson> = known
            .iter()
            .map(|r| {
                let s = subjects.iter().find(|s| s.id == r.bgm_id);
                CandidateJson {
                    bgm_id: r.bgm_id,
                    bgm_name: s.and_then(|s| s.name.clone()),
                    air_date: s.and_then(|s| s.date.clone()),
                    confidence: r.confidence,
                    reason: r.reason.clone(),
                    conflicts: s.map(|s| cand_conflicts(mal, s)).unwrap_or_default(),
                }
            })
            .collect();

        let accept = best
            .confidence
            .is_some_and(|c| c >= LLM_CONFIDENCE_THRESHOLD);
        if accept && conflicts.is_empty() {
            self.ensure_skeleton(best_subject).await?;
            self.write(
                existing,
                UpdateMalBgmMatch {
                    status: "included".into(),
                    source: Some("llm".into()),
                    bgm_id: Some(best.bgm_id),
                    confidence: best.confidence,
                    candidates: candidates_json(&cands),
                    ..Default::default()
                },
            )
            .await?;
            self.hydrate(best.bgm_id).await;
            return Ok(Some(MatchOutcome::Included));
        }

        if accept {
            tracing::warn!(
                mal_id = mal.mal_id,
                bgm_id = best.bgm_id,
                ?conflicts,
                "硬规则冲突，降级为 pending"
            );
        }
        self.write(
            existing,
            UpdateMalBgmMatch {
                status: "pending".into(),
                source: Some("llm".into()),
                confidence: best.confidence,
                candidates: candidates_json(&cands),
                ..Default::default()
            },
        )
        .await?;
        Ok(Some(MatchOutcome::Pending))
    }

    /// 用本地已同步详情补全搜索结果缺失的 platform / summary / tags（只填空字段）。
    async fn enrich(&self, subjects: &mut [Cand]) {
        let missing: Vec<i32> = subjects
            .iter()
            .filter(|s| s.platform.is_none() || s.summary.is_none() || s.tags.is_none())
            .map(|s| s.id)
            .collect();
        if missing.is_empty() {
            return;
        }
        let rows = match SubjectRepository::new(self.db.pool())
            .find_by_ids(&missing)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "enrich subjects 失败");
                return;
            }
        };
        for s in subjects.iter_mut() {
            let Some(row) = rows
                .iter()
                .find(|r| r.id == s.id && r.last_updated_at.is_some())
            else {
                continue;
            };
            if s.platform.is_none() {
                s.platform = row.platform.clone();
            }
            if s.summary.is_none() {
                s.summary = row.summary.clone();
            }
            if s.tags.is_none() {
                s.tags = row
                    .tags
                    .as_deref()
                    .and_then(|t| serde_json::from_str::<Vec<String>>(t).ok());
            }
        }
    }

    // ── 持久化 ──

    /// 写入匹配列（保留人工备注 `note`）。
    async fn write(&self, existing: &MalBgmMatch, mut data: UpdateMalBgmMatch) -> Result<()> {
        data.note = existing.note.clone();
        MalBgmMatchRepository::new(self.db.pool())
            .update_decision(existing.mal_id, data)
            .await
            .context("写入匹配结果失败")?
            .ok_or_else(|| anyhow!("mal_bgm_matches row missing for {}", existing.mal_id))?;
        Ok(())
    }

    /// 满足 `subjects(id)` 外键：行不存在时插入骨架行（不覆盖已有详情）。
    async fn ensure_skeleton(&self, s: &Cand) -> Result<()> {
        let repo = SubjectRepository::new(self.db.pool());
        if repo.find_by_id(s.id).await?.is_some() {
            return Ok(());
        }
        repo.upsert(CreateSubject {
            id: s.id,
            name: s.name.clone(),
            name_cn: s.name_cn.clone(),
            air_date: s.date.clone(),
            platform: s.platform.clone(),
            summary: s.summary.clone(),
            tags: s.tags.as_ref().and_then(|t| serde_json::to_string(t).ok()),
            nsfw: s.nsfw,
            ..Default::default()
        })
        .await?;
        Ok(())
    }

    async fn hydrate(&self, bgm_id: i32) {
        let today = chrono::Utc::now().date_naive();
        if let Err(e) = self.sync.hydrate_subject(bgm_id, today).await {
            tracing::warn!(bgm_id, error = %format!("{e:#}"), "hydrate_subject 失败");
        }
    }

    async fn hydrate_if_skeleton(&self, m: &MalBgmMatch) {
        let Some(bgm_id) = m.bgm_id else { return };
        match SubjectRepository::new(self.db.pool())
            .find_by_id(bgm_id)
            .await
        {
            Ok(Some(s)) if s.last_updated_at.is_none() => self.hydrate(bgm_id).await,
            Ok(_) => {}
            Err(e) => tracing::warn!(bgm_id, error = %e, "查询 subject 失败"),
        }
    }
}

fn cand_conflicts(mal: &MalAnime, s: &Cand) -> Vec<&'static str> {
    match_conflicts(
        mal.start_date.as_deref(),
        Some(&mal.media_type),
        s.date.as_deref(),
        s.platform.as_deref(),
    )
}

fn excluded(source: &str, reason: Option<&str>) -> UpdateMalBgmMatch {
    UpdateMalBgmMatch {
        status: "excluded".into(),
        source: Some(source.into()),
        reason: reason.map(String::from),
        ..Default::default()
    }
}

fn mal_brief(mal: &MalAnime) -> MalBrief {
    let studios = mal.studios.as_deref().and_then(|raw| {
        let v: serde_json::Value = serde_json::from_str(raw).ok()?;
        let names: Vec<String> = v
            .as_array()?
            .iter()
            .filter_map(|e| match e {
                serde_json::Value::String(s) => Some(s.clone()),
                other => other.get("name")?.as_str().map(String::from),
            })
            .filter(|s| !s.is_empty())
            .collect();
        (!names.is_empty()).then_some(names)
    });
    MalBrief {
        title: mal.title.clone(),
        title_ja: mal.title_ja.clone(),
        media_type: Some(mal.media_type.clone()),
        start_date: mal.start_date.clone(),
        num_episodes: mal.num_episodes,
        source: mal.source.clone(),
        studios,
        synopsis: mal.synopsis.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dal::dto::{CreateSeason, UpsertMalAnime};
    use crate::dal::repositories::SeasonRepository;
    use crate::services::llm::client::BoxFuture;
    use mockito::{Matcher, Mock, Server, ServerGuard};
    use sqlx::SqlitePool;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // ── 纯函数 ──

    #[test]
    fn test_is_korean_title() {
        assert!(is_korean_title(Some("프린세스 캐치! 티니핑")));
        assert!(!is_korean_title(Some("ブルーアーカイブ / 블루 아카이브")));
        assert!(!is_korean_title(Some("葬送のフリーレン")));
        assert!(!is_korean_title(Some("完全中文标题")));
        assert!(!is_korean_title(Some("")));
        assert!(!is_korean_title(None));
    }

    #[test]
    fn test_match_conflicts_date() {
        let c = |a, b| match_conflicts(a, Some("tv"), b, None);
        assert!(c(Some("2024-01-01"), Some("2024-07-01")).is_empty()); // 6 个月
        assert_eq!(
            c(Some("2024-01-01"), Some("2024-08-01")),
            vec![CONFLICT_DATE]
        );
        assert!(c(Some("2023-12"), Some("2024-01-15")).is_empty());
        assert!(c(Some("2024"), Some("2023-12-01")).is_empty()); // 仅年份差 1 不判
        assert_eq!(c(Some("2024"), Some("2021-12-01")), vec![CONFLICT_DATE]);
        assert!(c(None, Some("2021-12-01")).is_empty());
        assert!(c(Some("garbage"), Some("2021-12-01")).is_empty());
    }

    #[test]
    fn test_match_conflicts_platform() {
        let c = |mt, pf| match_conflicts(None, Some(mt), None, Some(pf));
        assert_eq!(c("movie", "TV"), vec![CONFLICT_PLATFORM]);
        assert_eq!(c("movie", "WEB"), vec![CONFLICT_PLATFORM]);
        assert_eq!(c("tv", "劇場版"), vec![CONFLICT_PLATFORM]);
        assert_eq!(c("ona", "剧场版"), vec![CONFLICT_PLATFORM]);
        assert!(c("ova", "TV").is_empty());
        assert!(c("tv", "TV").is_empty());
        assert!(c("movie", "剧场版").is_empty());
    }

    #[test]
    fn test_alternative_keywords() {
        assert_eq!(alternative_keywords("無職転生 第2期"), vec!["無職転生"]);
        assert_eq!(alternative_keywords("Re:ゼロ Season 2")[0], "Re:ゼロ");
        assert_eq!(
            alternative_keywords("劇場版 名探偵コナン"),
            vec!["名探偵コナン"]
        );
        assert_eq!(alternative_keywords("銀魂 ～吉原大炎上～")[0], "銀魂");
        assert_eq!(alternative_keywords("鬼滅の刃 2"), vec!["鬼滅の刃"]);
        // 空格后半段（>=4 字符）
        let alts = alternative_keywords("ABC 長いサブタイトル");
        assert!(alts.contains(&"長いサブタイトル".to_string()));
        assert!(alternative_keywords("単独").is_empty());
    }

    #[test]
    fn test_is_hopeless_ona_kids_and_skip_types() {
        assert!(is_hopeless_ona_kids("ona", Some("g")));
        assert!(is_hopeless_ona_kids("ona", Some("pg")));
        assert!(!is_hopeless_ona_kids("ona", Some("pg_13")));
        assert!(!is_hopeless_ona_kids("ona", None));
        assert!(!is_hopeless_ona_kids("tv", Some("g")));
        for t in ["pv", "cm", "music", "special", "tv_special", "unknown"] {
            assert!(should_skip_media_type(t), "{t}");
        }
        for t in ["tv", "ona", "ova", "movie"] {
            assert!(!should_skip_media_type(t), "{t}");
        }
    }

    #[test]
    fn test_season_date_range() {
        assert_eq!(
            season_date_range(202601).unwrap(),
            ("2025-12-01".into(), "2026-03-31".into())
        );
        assert_eq!(
            season_date_range(202604).unwrap(),
            ("2026-03-01".into(), "2026-06-30".into())
        );
        assert_eq!(
            season_date_range(202607).unwrap(),
            ("2026-06-01".into(), "2026-09-30".into())
        );
        assert_eq!(
            season_date_range(202610).unwrap(),
            ("2026-09-01".into(), "2026-12-31".into())
        );
        assert!(season_date_range(202602).is_err());
    }

    #[test]
    fn test_normalize_title_nfkc() {
        assert_eq!(normalize_title(" ＡＢＣ　１２３ "), "ABC 123");
        assert_eq!(normalize_title("ﾌﾘｰﾚﾝ"), "フリーレン");
    }

    // ── 服务 ──

    struct FakeLlm {
        matches: Mutex<Vec<LlmMatch>>,
        suggest: Mutex<Suggest>,
        match_calls: AtomicUsize,
        suggest_calls: AtomicUsize,
    }

    impl FakeLlm {
        fn new(matches: Vec<LlmMatch>, suggest: Suggest) -> Arc<Self> {
            Arc::new(Self {
                matches: Mutex::new(matches),
                suggest: Mutex::new(suggest),
                match_calls: AtomicUsize::new(0),
                suggest_calls: AtomicUsize::new(0),
            })
        }
        fn calls(&self) -> (usize, usize) {
            (
                self.match_calls.load(Ordering::SeqCst),
                self.suggest_calls.load(Ordering::SeqCst),
            )
        }
    }

    impl LlmMatcher for FakeLlm {
        fn match_anime<'a>(
            &'a self,
            _mal: &'a MalBrief,
            _c: &'a [BgmCandidate],
        ) -> BoxFuture<'a, Result<Vec<LlmMatch>>> {
            self.match_calls.fetch_add(1, Ordering::SeqCst);
            let out = self.matches.lock().unwrap().clone();
            Box::pin(async move { Ok(out) })
        }
        fn suggest_search<'a>(
            &'a self,
            _t: &'a str,
            _m: &'a str,
        ) -> BoxFuture<'a, Result<Suggest>> {
            self.suggest_calls.fetch_add(1, Ordering::SeqCst);
            let out = self.suggest.lock().unwrap().clone();
            Box::pin(async move { Ok(out) })
        }
    }

    fn llm_match(bgm_id: i32, confidence: f64) -> LlmMatch {
        LlmMatch {
            bgm_id,
            confidence: Some(confidence),
            reason: Some("r".into()),
        }
    }

    struct Env {
        db: Arc<Database>,
        server: ServerGuard,
        service: MatcherService,
    }

    async fn env(pool: SqlitePool, llm: Option<Arc<FakeLlm>>) -> Env {
        let server = Server::new_async().await;
        let db = Arc::new(Database::from_pool(pool));
        let url = server.url();
        let sync = Arc::new(SyncService::with_clients(
            Arc::clone(&db),
            BangumiClient::with_base_url(&url),
        ));
        let llm = llm.map(|l| l as Arc<dyn LlmMatcher>);
        let service = MatcherService::with_clients(
            Arc::clone(&db),
            BangumiClient::with_base_url(&url),
            sync,
            llm,
        );
        SeasonRepository::new(db.pool())
            .create(CreateSeason {
                season_id: 202601,
                year: 2026,
                season: "WINTER".into(),
                name: None,
            })
            .await
            .unwrap();
        Env {
            db,
            server,
            service,
        }
    }

    async fn add_mal(
        db: &Database,
        mal_id: i32,
        title_ja: &str,
        media_type: &str,
        rating: Option<&str>,
        start_date: &str,
    ) {
        MalAnimeRepository::new(db.pool())
            .upsert(UpsertMalAnime {
                mal_id,
                title: format!("title {mal_id}"),
                title_ja: Some(title_ja.into()),
                media_type: media_type.into(),
                rating: rating.map(String::from),
                start_date: Some(start_date.into()),
                ..Default::default()
            })
            .await
            .unwrap();
        SeasonMalRepository::new(db.pool())
            .insert_ignore(202601, mal_id)
            .await
            .unwrap();
        MalBgmMatchRepository::new(db.pool())
            .insert_pending_ignore(mal_id)
            .await
            .unwrap();
    }

    async fn set_match(db: &Database, mal_id: i32, data: UpdateMalBgmMatch) {
        MalBgmMatchRepository::new(db.pool())
            .update_decision(mal_id, data)
            .await
            .unwrap()
            .unwrap();
    }

    async fn get_match(db: &Database, mal_id: i32) -> MalBgmMatch {
        MalBgmMatchRepository::new(db.pool())
            .get(mal_id)
            .await
            .unwrap()
            .unwrap()
    }

    fn search_body(subjects: &[(i32, &str, &str)]) -> String {
        let data: Vec<_> = subjects
            .iter()
            .map(|(id, name, date)| {
                serde_json::json!({"id": id, "type": 2, "name": name, "date": date})
            })
            .collect();
        serde_json::json!({"total": data.len(), "limit": 10, "offset": 0, "data": data}).to_string()
    }

    async fn mock_search(server: &mut ServerGuard, subjects: &[(i32, &str, &str)]) -> Mock {
        server
            .mock("POST", "/v0/search/subjects")
            .match_query(Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(search_body(subjects))
            .expect_at_least(1)
            .create_async()
            .await
    }

    /// 搜索接口不应被调用
    async fn forbid_search(server: &mut ServerGuard) -> Mock {
        server
            .mock("POST", "/v0/search/subjects")
            .match_query(Matcher::Any)
            .with_status(500)
            .expect(0)
            .create_async()
            .await
    }

    async fn mock_hydrate(server: &mut ServerGuard, id: i32, hits: usize) -> (Mock, Mock) {
        let subject = server
            .mock("GET", format!("/v0/subjects/{id}").as_str())
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({"id": id, "type": 2, "name": "hydrated", "date": "2026-01-05"})
                    .to_string(),
            )
            .expect(hits)
            .create_async()
            .await;
        let eps = server
            .mock("GET", Matcher::Regex(r"^/v0/episodes".into()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"total":0,"limit":100,"offset":0,"data":[]}"#)
            .expect(hits)
            .create_async()
            .await;
        (subject, eps)
    }

    async fn seed_subject(db: &Database, id: i32, hydrated: bool) {
        let repo = SubjectRepository::new(db.pool());
        repo.upsert(CreateSubject {
            id,
            name: Some("seed".into()),
            ..Default::default()
        })
        .await
        .unwrap();
        if hydrated {
            repo.update_last_updated_at(id).await.unwrap();
        }
    }

    async fn last_updated(db: &Database, id: i32) -> bool {
        SubjectRepository::new(db.pool())
            .find_by_id(id)
            .await
            .unwrap()
            .unwrap()
            .last_updated_at
            .is_some()
    }

    #[sqlx::test]
    async fn test_short_circuit_included_hydrates_skeleton(pool: SqlitePool) {
        let mut e = env(pool, None).await;
        add_mal(&e.db, 1, "葬送のフリーレン", "tv", None, "2026-01-05").await;
        seed_subject(&e.db, 500, false).await;
        set_match(
            &e.db,
            1,
            UpdateMalBgmMatch {
                status: "included".into(),
                source: Some("human".into()),
                bgm_id: Some(500),
                ..Default::default()
            },
        )
        .await;
        let forbid = forbid_search(&mut e.server).await;
        let (s, ep) = mock_hydrate(&mut e.server, 500, 1).await;

        let out = e.service.match_mal(1, false).await.unwrap();
        assert_eq!(out, MatchOutcome::Skipped);
        forbid.assert_async().await;
        s.assert_async().await;
        ep.assert_async().await;
        assert!(last_updated(&e.db, 500).await);
        let m = get_match(&e.db, 1).await;
        assert_eq!(
            (m.status.as_str(), m.source.as_deref()),
            ("included", Some("human"))
        );
    }

    #[sqlx::test]
    async fn test_short_circuit_included_hydrated_no_fetch(pool: SqlitePool) {
        let mut e = env(pool, None).await;
        add_mal(&e.db, 1, "x作品", "tv", None, "2026-01-05").await;
        seed_subject(&e.db, 500, true).await;
        set_match(
            &e.db,
            1,
            UpdateMalBgmMatch {
                status: "included".into(),
                source: Some("exact".into()),
                bgm_id: Some(500),
                ..Default::default()
            },
        )
        .await;
        let (s, ep) = mock_hydrate(&mut e.server, 500, 0).await;
        assert_eq!(
            e.service.match_mal(1, true).await.unwrap(),
            MatchOutcome::Skipped
        );
        s.assert_async().await;
        ep.assert_async().await;
    }

    #[sqlx::test]
    async fn test_short_circuit_excluded_even_with_retry(pool: SqlitePool) {
        let llm = FakeLlm::new(vec![], Suggest::Keywords(vec![]));
        let mut e = env(pool, Some(llm.clone())).await;
        add_mal(&e.db, 1, "x作品", "tv", None, "2026-01-05").await;
        set_match(&e.db, 1, excluded("human", Some("duplicate"))).await;
        let forbid = forbid_search(&mut e.server).await;

        assert_eq!(
            e.service.match_mal(1, true).await.unwrap(),
            MatchOutcome::Skipped
        );
        forbid.assert_async().await;
        assert_eq!(llm.calls(), (0, 0));
        let m = get_match(&e.db, 1).await;
        assert_eq!(
            (m.status.as_str(), m.reason.as_deref()),
            ("excluded", Some("duplicate"))
        );
    }

    #[sqlx::test]
    async fn test_short_circuit_pending_with_source_and_retry_flag(pool: SqlitePool) {
        let llm = FakeLlm::new(vec![llm_match(900, 0.95)], Suggest::Keywords(vec![]));
        let mut e = env(pool, Some(llm.clone())).await;
        add_mal(&e.db, 1, "葬送のフリーレン", "tv", None, "2026-01-05").await;
        set_match(
            &e.db,
            1,
            UpdateMalBgmMatch {
                status: "pending".into(),
                source: Some("llm".into()),
                ..Default::default()
            },
        )
        .await;
        let forbid = forbid_search(&mut e.server).await;
        // retry=false -> 跳过
        assert_eq!(
            e.service.match_mal(1, false).await.unwrap(),
            MatchOutcome::Skipped
        );
        forbid.assert_async().await;
        assert_eq!(llm.calls(), (0, 0));
        drop(forbid);

        // retry=true -> 重新跑
        let _mock_search = mock_search(&mut e.server, &[(900, "別の名前", "2026-01-05")]).await;
        let _mock_hydrate = mock_hydrate(&mut e.server, 900, 1).await;
        assert_eq!(
            e.service.match_mal(1, true).await.unwrap(),
            MatchOutcome::Included
        );
        assert_eq!(llm.calls().0, 1);
        let m = get_match(&e.db, 1).await;
        assert_eq!((m.status.as_str(), m.bgm_id), ("included", Some(900)));
    }

    #[sqlx::test]
    async fn test_pending_without_source_is_processed(pool: SqlitePool) {
        let mut e = env(pool, None).await;
        add_mal(&e.db, 1, "葬送のフリーレン", "tv", None, "2026-01-05").await;
        let _mock_search =
            mock_search(&mut e.server, &[(500, "葬送のフリーレン", "2026-01-05")]).await;
        assert_eq!(
            e.service.match_mal(1, false).await.unwrap(),
            MatchOutcome::Included
        );
    }

    #[sqlx::test]
    async fn test_rule_excludes_skip_media_types(pool: SqlitePool) {
        let llm = FakeLlm::new(vec![], Suggest::Keywords(vec![]));
        let mut e = env(pool, Some(llm.clone())).await;
        let forbid = forbid_search(&mut e.server).await;
        for (i, t) in ["pv", "cm", "music", "special", "tv_special", "unknown"]
            .iter()
            .enumerate()
        {
            let id = i as i32 + 1;
            add_mal(&e.db, id, "葬送のフリーレン", t, None, "2026-01-05").await;
            assert_eq!(
                e.service.match_mal(id, false).await.unwrap(),
                MatchOutcome::Excluded
            );
            let m = get_match(&e.db, id).await;
            assert_eq!(
                (m.status.as_str(), m.source.as_deref()),
                ("excluded", Some("rule"))
            );
        }
        forbid.assert_async().await;
        assert_eq!(llm.calls(), (0, 0));
    }

    #[sqlx::test]
    async fn test_korean_guard_skips_llm(pool: SqlitePool) {
        let llm = FakeLlm::new(
            vec![llm_match(500, 0.99)],
            Suggest::Keywords(vec!["x".into()]),
        );
        let mut e = env(pool, Some(llm.clone())).await;
        add_mal(&e.db, 1, "프린세스 캐치! 티니핑", "tv", None, "2026-01-05").await;
        let _mock_search = mock_search(&mut e.server, &[(500, "全然違う", "2026-01-05")]).await;

        assert_eq!(
            e.service.match_mal(1, false).await.unwrap(),
            MatchOutcome::Excluded
        );
        assert_eq!(llm.calls(), (0, 0));
        let m = get_match(&e.db, 1).await;
        assert_eq!(
            (m.status.as_str(), m.source.as_deref()),
            ("excluded", Some("rule"))
        );
        assert!(m.candidates.is_none());
    }

    #[sqlx::test]
    async fn test_korean_exact_match_allowed(pool: SqlitePool) {
        let llm = FakeLlm::new(vec![], Suggest::Keywords(vec![]));
        let mut e = env(pool, Some(llm.clone())).await;
        add_mal(&e.db, 1, "프린세스 캐치! 티니핑", "tv", None, "2026-01-05").await;
        let _mock_search = mock_search(
            &mut e.server,
            &[(500, "프린세스 캐치! 티니핑", "2026-01-05")],
        )
        .await;

        assert_eq!(
            e.service.match_mal(1, false).await.unwrap(),
            MatchOutcome::Included
        );
        assert_eq!(llm.calls(), (0, 0));
        assert_eq!(get_match(&e.db, 1).await.source.as_deref(), Some("exact"));
    }

    #[sqlx::test]
    async fn test_exact_included_with_nfkc_and_skeleton(pool: SqlitePool) {
        let llm = FakeLlm::new(vec![], Suggest::Keywords(vec![]));
        let mut e = env(pool, Some(llm.clone())).await;
        add_mal(&e.db, 1, "ＡＢＣ　作品", "tv", None, "2026-01-05").await;
        let m = mock_search(
            &mut e.server,
            &[
                (499, "other", "2026-01-05"),
                (500, "ABC 作品", "2026-01-05"),
            ],
        )
        .await;

        assert_eq!(
            e.service.match_mal(1, false).await.unwrap(),
            MatchOutcome::Included
        );
        m.assert_async().await;
        assert_eq!(llm.calls(), (0, 0));
        let row = get_match(&e.db, 1).await;
        assert_eq!(row.status, "included");
        assert_eq!(row.source.as_deref(), Some("exact"));
        assert_eq!(row.bgm_id, Some(500));
        assert_eq!(row.confidence, Some(1.0));
        assert!(row.error.is_none());
        // 骨架行满足外键，且未被标记为已拉取
        assert!(!last_updated(&e.db, 500).await);
    }

    #[sqlx::test]
    async fn test_exact_with_conflict_is_pending(pool: SqlitePool) {
        let llm = FakeLlm::new(vec![llm_match(500, 0.99)], Suggest::Keywords(vec![]));
        let mut e = env(pool, Some(llm.clone())).await;
        add_mal(&e.db, 1, "同名作品", "tv", None, "2026-01-05").await;
        let _mock_search = mock_search(&mut e.server, &[(500, "同名作品", "2015-04-01")]).await;

        assert_eq!(
            e.service.match_mal(1, false).await.unwrap(),
            MatchOutcome::Pending
        );
        assert_eq!(llm.calls(), (0, 0));
        let row = get_match(&e.db, 1).await;
        assert_eq!(
            (row.status.as_str(), row.source.as_deref()),
            ("pending", Some("exact"))
        );
        assert!(row.bgm_id.is_none());
        let cands: serde_json::Value =
            serde_json::from_str(row.candidates.as_deref().unwrap()).unwrap();
        assert_eq!(cands[0]["bgm_id"], 500);
        assert_eq!(cands[0]["conflicts"], serde_json::json!(["date"]));
    }

    #[sqlx::test]
    async fn test_exact_platform_conflict_from_local_subject_is_pending(pool: SqlitePool) {
        let llm = FakeLlm::new(vec![], Suggest::Keywords(vec![]));
        let mut e = env(pool, Some(llm.clone())).await;
        // 本地已同步一条 TV 动画（日期接近，搜索接口本身不返回 platform）
        let repo = SubjectRepository::new(e.db.pool());
        repo.upsert(CreateSubject {
            id: 500,
            name: Some("同名作品".into()),
            air_date: Some("2026-01-05".into()),
            platform: Some("TV".into()),
            ..Default::default()
        })
        .await
        .unwrap();
        repo.update_last_updated_at(500).await.unwrap();

        // MAL 条目为同名 movie，首播日期在半年内，仅靠 enrich 后的 platform 才能发现冲突
        add_mal(&e.db, 1, "同名作品", "movie", None, "2026-03-01").await;
        let _mock_search = mock_search(&mut e.server, &[(500, "同名作品", "2026-01-05")]).await;

        assert_eq!(
            e.service.match_mal(1, false).await.unwrap(),
            MatchOutcome::Pending
        );
        assert_eq!(llm.calls(), (0, 0));
        let row = get_match(&e.db, 1).await;
        assert_eq!(
            (row.status.as_str(), row.source.as_deref()),
            ("pending", Some("exact"))
        );
        let cands: serde_json::Value =
            serde_json::from_str(row.candidates.as_deref().unwrap()).unwrap();
        assert_eq!(cands[0]["conflicts"], serde_json::json!(["platform"]));
    }

    #[sqlx::test]
    async fn test_all_candidates_have_conflicts_computed(pool: SqlitePool) {
        let llm = FakeLlm::new(
            vec![llm_match(500, 0.7), llm_match(501, 0.4)],
            Suggest::Skip,
        );
        let mut e = env(pool, Some(llm)).await;
        add_mal(&e.db, 1, "葬送のフリーレン", "tv", None, "2026-01-05").await;
        let _mock_search = mock_search(
            &mut e.server,
            &[
                (500, "Frieren S2", "2026-01-05"),
                (501, "Frieren S1", "2023-09-29"),
            ],
        )
        .await;

        assert_eq!(
            e.service.match_mal(1, false).await.unwrap(),
            MatchOutcome::Pending
        );
        let row = get_match(&e.db, 1).await;
        let cands: serde_json::Value =
            serde_json::from_str(row.candidates.as_deref().unwrap()).unwrap();
        assert_eq!(cands[0]["bgm_id"], 500);
        assert_eq!(cands[0]["conflicts"], serde_json::json!([]));
        assert_eq!(cands[1]["bgm_id"], 501);
        assert_eq!(cands[1]["conflicts"], serde_json::json!(["date"]));
    }

    #[sqlx::test]
    async fn test_llm_high_confidence_included_and_hydrated(pool: SqlitePool) {
        let llm = FakeLlm::new(
            vec![
                llm_match(501, 0.4),
                llm_match(500, 0.9),
                llm_match(777, 0.99),
            ],
            Suggest::Keywords(vec![]),
        );
        let mut e = env(pool, Some(llm.clone())).await;
        add_mal(&e.db, 1, "葬送のフリーレン", "tv", None, "2026-01-05").await;
        let _mock_search = mock_search(
            &mut e.server,
            &[
                (500, "Sousou no Frieren", "2026-01-05"),
                (501, "別作品", "2026-01-05"),
            ],
        )
        .await;
        let (s, ep) = mock_hydrate(&mut e.server, 500, 1).await;

        assert_eq!(
            e.service.match_mal(1, false).await.unwrap(),
            MatchOutcome::Included
        );
        s.assert_async().await;
        ep.assert_async().await;
        assert_eq!(llm.calls(), (1, 0));
        let row = get_match(&e.db, 1).await;
        assert_eq!(
            (row.status.as_str(), row.source.as_deref()),
            ("included", Some("llm"))
        );
        assert_eq!(row.bgm_id, Some(500));
        assert_eq!(row.confidence, Some(0.9));
        let cands: serde_json::Value =
            serde_json::from_str(row.candidates.as_deref().unwrap()).unwrap();
        // 不在搜索结果内的 777 被丢弃
        assert_eq!(cands.as_array().unwrap().len(), 2);
        assert!(last_updated(&e.db, 500).await);
    }

    #[sqlx::test]
    async fn test_llm_high_confidence_with_conflict_is_pending(pool: SqlitePool) {
        let llm = FakeLlm::new(vec![llm_match(500, 0.95)], Suggest::Keywords(vec![]));
        let mut e = env(pool, Some(llm.clone())).await;
        add_mal(&e.db, 1, "葬送のフリーレン", "tv", None, "2026-01-05").await;
        let _mock_search = mock_search(&mut e.server, &[(500, "Frieren", "2019-01-05")]).await;
        let (s, _ep) = mock_hydrate(&mut e.server, 500, 0).await;

        assert_eq!(
            e.service.match_mal(1, false).await.unwrap(),
            MatchOutcome::Pending
        );
        s.assert_async().await;
        let row = get_match(&e.db, 1).await;
        assert_eq!(
            (row.status.as_str(), row.source.as_deref()),
            ("pending", Some("llm"))
        );
        assert!(row.bgm_id.is_none());
        assert!(row.candidates.unwrap().contains("\"date\""));
    }

    #[sqlx::test]
    async fn test_llm_low_confidence_pending(pool: SqlitePool) {
        let llm = FakeLlm::new(vec![llm_match(500, 0.6)], Suggest::Keywords(vec![]));
        let mut e = env(pool, Some(llm.clone())).await;
        add_mal(&e.db, 1, "葬送のフリーレン", "tv", None, "2026-01-05").await;
        let _mock_search = mock_search(&mut e.server, &[(500, "Frieren", "2026-01-05")]).await;

        assert_eq!(
            e.service.match_mal(1, false).await.unwrap(),
            MatchOutcome::Pending
        );
        let row = get_match(&e.db, 1).await;
        assert_eq!(
            (row.status.as_str(), row.source.as_deref()),
            ("pending", Some("llm"))
        );
        assert_eq!(row.confidence, Some(0.6));
        assert!(row.candidates.is_some());
        assert!(row.error.is_none());
    }

    #[sqlx::test]
    async fn test_llm_no_match_pending_llm(pool: SqlitePool) {
        let llm = FakeLlm::new(vec![], Suggest::Skip);
        let mut e = env(pool, Some(llm.clone())).await;
        add_mal(&e.db, 1, "葬送のフリーレン", "tv", None, "2026-01-05").await;
        let _mock_search = mock_search(&mut e.server, &[(500, "Frieren", "2026-01-05")]).await;

        assert_eq!(
            e.service.match_mal(1, false).await.unwrap(),
            MatchOutcome::Pending
        );
        assert_eq!(llm.calls(), (1, 1));
        let row = get_match(&e.db, 1).await;
        assert_eq!(
            (row.status.as_str(), row.source.as_deref()),
            ("pending", Some("llm"))
        );
    }

    #[sqlx::test]
    async fn test_kids_ona_fallback(pool: SqlitePool) {
        let llm = FakeLlm::new(vec![], Suggest::Keywords(vec!["キッズ".into()]));
        let mut e = env(pool, Some(llm.clone())).await;
        add_mal(&e.db, 1, "子供向けONA", "ona", Some("g"), "2026-01-05").await;
        let _mock_search = mock_search(&mut e.server, &[]).await;

        assert_eq!(
            e.service.match_mal(1, false).await.unwrap(),
            MatchOutcome::Excluded
        );
        let row = get_match(&e.db, 1).await;
        assert_eq!(
            (
                row.status.as_str(),
                row.source.as_deref(),
                row.reason.as_deref()
            ),
            ("excluded", Some("rule"), Some("kids"))
        );
    }

    #[sqlx::test]
    async fn test_ona_non_kids_not_excluded(pool: SqlitePool) {
        let llm = FakeLlm::new(vec![], Suggest::Keywords(vec![]));
        let mut e = env(pool, Some(llm)).await;
        add_mal(&e.db, 1, "普通ONA", "ona", Some("pg_13"), "2026-01-05").await;
        let _mock_search = mock_search(&mut e.server, &[]).await;

        assert_eq!(
            e.service.match_mal(1, false).await.unwrap(),
            MatchOutcome::Pending
        );
        let row = get_match(&e.db, 1).await;
        assert_eq!(
            (row.status.as_str(), row.source.as_deref()),
            ("pending", Some("llm"))
        );
        assert!(row.candidates.is_none());
    }

    #[sqlx::test]
    async fn test_search_failure_records_error_and_allows_rerun(pool: SqlitePool) {
        let llm = FakeLlm::new(vec![], Suggest::Keywords(vec![]));
        let mut e = env(pool, Some(llm.clone())).await;
        add_mal(&e.db, 1, "葬送のフリーレン", "tv", None, "2026-01-05").await;
        e.server
            .mock("POST", "/v0/search/subjects")
            .match_query(Matcher::Any)
            .with_status(500)
            .create_async()
            .await;

        assert_eq!(
            e.service.match_mal(1, false).await.unwrap(),
            MatchOutcome::Failed
        );
        assert_eq!(llm.calls(), (0, 0));
        let row = get_match(&e.db, 1).await;
        assert_eq!(
            (row.status.as_str(), row.source.as_deref()),
            ("pending", None)
        );
        assert!(row.error.is_some());
    }

    #[sqlx::test]
    async fn test_human_note_preserved(pool: SqlitePool) {
        let mut e = env(pool, None).await;
        add_mal(&e.db, 1, "葬送のフリーレン", "tv", None, "2026-01-05").await;
        set_match(
            &e.db,
            1,
            UpdateMalBgmMatch {
                status: "pending".into(),
                note: Some("人工备注".into()),
                ..Default::default()
            },
        )
        .await;
        let _mock_search =
            mock_search(&mut e.server, &[(500, "葬送のフリーレン", "2026-01-05")]).await;
        e.service.match_mal(1, false).await.unwrap();
        assert_eq!(get_match(&e.db, 1).await.note.as_deref(), Some("人工备注"));
    }

    #[sqlx::test]
    async fn test_search_uses_season_range_then_falls_back(pool: SqlitePool) {
        let mut e = env(pool, None).await;
        add_mal(&e.db, 1, "葬送のフリーレン", "tv", None, "2026-01-05").await;
        // 带日期范围 -> 空
        let dated = e
            .server
            .mock("POST", "/v0/search/subjects")
            .match_query(Matcher::Any)
            .match_body(Matcher::Regex("air_date".into()))
            .with_status(200)
            .with_body(search_body(&[]))
            .expect(1)
            .create_async()
            .await;
        // 不带范围 -> 命中
        let plain = e
            .server
            .mock("POST", "/v0/search/subjects")
            .match_query(Matcher::Any)
            .match_request(|r| !String::from_utf8_lossy(r.body().unwrap()).contains("air_date"))
            .with_status(200)
            .with_body(search_body(&[(500, "葬送のフリーレン", "2026-01-05")]))
            .expect(1)
            .create_async()
            .await;
        e.service.match_season(202601, false).await.unwrap();
        dated.assert_async().await;
        plain.assert_async().await;
        assert_eq!(get_match(&e.db, 1).await.status, "included");
    }

    #[sqlx::test]
    async fn test_match_season_only_active_rows_and_summary(pool: SqlitePool) {
        let mut e = env(pool, None).await;
        add_mal(&e.db, 1, "葬送のフリーレン", "tv", None, "2026-01-05").await;
        add_mal(&e.db, 2, "PV作品", "pv", None, "2026-01-05").await;
        add_mal(&e.db, 3, "移除作品", "tv", None, "2026-01-05").await;
        SeasonMalRepository::new(e.db.pool())
            .set_removed(202601, 3, true, None)
            .await
            .unwrap();
        let _mock_search =
            mock_search(&mut e.server, &[(500, "葬送のフリーレン", "2026-01-05")]).await;

        let summary = e.service.match_season(202601, false).await.unwrap();
        assert_eq!(
            summary,
            MatchSummary {
                total: 2,
                included: 1,
                excluded: 1,
                ..Default::default()
            }
        );
        assert_eq!(get_match(&e.db, 3).await.source, None);
        assert_eq!(get_match(&e.db, 3).await.status, "pending");

        // 再跑：均已决策，全部短路
        let again = e.service.match_season(202601, true).await.unwrap();
        assert_eq!(again.skipped, 2);
    }

    #[sqlx::test]
    async fn test_missing_mal_anime_errors(pool: SqlitePool) {
        let e = env(pool, None).await;
        assert!(e.service.match_mal(999, false).await.is_err());
    }
}
