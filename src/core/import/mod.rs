use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDateTime, Utc};
use serde::Deserialize;
use sqlx::{
    FromRow, Sqlite, SqlitePool, Transaction,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};

use crate::core::sync::season_key_to_id;
use crate::dal::Database;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportSummary {
    pub seasons_inserted: usize,
    pub seasons_skipped: usize,
    pub subjects_inserted: usize,
    pub mal_anime_upserted: usize,
    pub rating_fallback: usize,
    pub season_mal_inserted: usize,
    pub season_mal_removed: usize,
    pub season_mal_skipped: usize,
    pub matches_upserted: usize,
    pub matches_skipped: usize,
}

impl fmt::Display for ImportSummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "seasons: inserted={} skipped={}; subjects_skeleton_inserted={}; mal_anime: upserted={} rating_fallback={}; season_mal: inserted={} removed={} skipped={}; matches: upserted={} skipped={}",
            self.seasons_inserted,
            self.seasons_skipped,
            self.subjects_inserted,
            self.mal_anime_upserted,
            self.rating_fallback,
            self.season_mal_inserted,
            self.season_mal_removed,
            self.season_mal_skipped,
            self.matches_upserted,
            self.matches_skipped,
        )
    }
}

#[derive(Debug, FromRow)]
struct SourceSeason {
    id: String,
}

#[derive(Debug, FromRow)]
struct SourceBgmSubject {
    bgm_id: i32,
    name: Option<String>,
    name_cn: Option<String>,
    air_date: Option<String>,
    platform: Option<String>,
    summary: Option<String>,
    tags: Option<String>,
    nsfw: Option<bool>,
}

#[derive(Debug, FromRow)]
struct SourceMalAnime {
    mal_id: i32,
    title: String,
    title_ja: Option<String>,
    title_en: Option<String>,
    media_type: String,
    rating: Option<String>,
    start_date: Option<String>,
    end_date: Option<String>,
    num_episodes: Option<i32>,
    source: Option<String>,
    studios: Option<String>,
    synopsis: Option<String>,
    start_season_year: Option<i32>,
    start_season: Option<String>,
    fetched_at: Option<String>,
}

#[derive(Debug, FromRow)]
struct SourceSeasonItem {
    season_id: String,
    mal_id: i32,
    status: Option<String>,
    source: Option<String>,
    confidence: Option<f64>,
    bgm_id: Option<i32>,
    candidates: Option<String>,
    error: Option<String>,
    origin: Option<String>,
    updated_at: Option<String>,
    reason: Option<String>,
    note: Option<String>,
}

#[derive(Debug, FromRow)]
struct SourceOverride {
    mal_id: i32,
    season_id: String,
    action: Option<String>,
    bgm_id: Option<i32>,
    reason: Option<String>,
    note: Option<String>,
}

#[derive(Deserialize)]
struct RawMalItem {
    id: i32,
    #[serde(default)]
    rating: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum MalJsonPayload {
    Envelope { items: Vec<RawMalItem> },
    List(Vec<RawMalItem>),
}

/// One-shot import from legacy `season.db` (and optional raw MAL JSON cache dir)
/// into the target database inside a single transaction.
pub async fn import_season_data(
    db: &Database,
    season_db_path: &Path,
    mal_json_dir: Option<&Path>,
) -> Result<ImportSummary> {
    if !season_db_path.exists() {
        anyhow::bail!(
            "source season db does not exist: {}",
            season_db_path.display()
        );
    }

    let raw_ratings = match mal_json_dir {
        Some(dir) if !dir.exists() => {
            tracing::warn!(
                mal_json_dir = %dir.display(),
                "mal-json-dir does not exist; falling back without raw MAL JSON ratings"
            );
            HashMap::new()
        }
        Some(dir) => load_raw_mal_ratings(dir)?,
        None => HashMap::new(),
    };

    let opts = SqliteConnectOptions::new()
        .filename(season_db_path)
        .read_only(true);
    let source_pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .with_context(|| {
            format!(
                "failed to open source season db read-only: {}",
                season_db_path.display()
            )
        })?;

    let source_seasons = read_source_seasons(&source_pool).await?;
    let source_bgm_subjects = read_source_bgm_subjects(&source_pool).await?;
    let source_mal_anime = read_source_mal_anime(&source_pool).await?;
    let source_season_items = read_source_season_items(&source_pool).await?;
    let source_overrides = read_source_overrides(&source_pool).await?;
    source_pool.close().await;

    let now_str = Utc::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string();
    let mut summary = ImportSummary::default();

    let mut tx = db
        .pool()
        .begin()
        .await
        .context("failed to begin target import transaction")?;

    let known_seasons = import_seasons(&mut tx, &source_seasons, &mut summary).await?;

    import_subject_skeletons(
        &mut tx,
        &source_season_items,
        &source_overrides,
        &source_bgm_subjects,
        &mut summary,
    )
    .await?;

    let known_mal_ids = import_mal_anime(
        &mut tx,
        &source_mal_anime,
        &raw_ratings,
        &now_str,
        &mut summary,
    )
    .await?;

    import_season_mal(
        &mut tx,
        &source_season_items,
        &source_overrides,
        &known_seasons,
        &known_mal_ids,
        &mut summary,
    )
    .await?;

    import_matches(
        &mut tx,
        &source_season_items,
        &known_mal_ids,
        &now_str,
        &mut summary,
    )
    .await?;

    tx.commit()
        .await
        .context("failed to commit import transaction")?;

    tracing::info!(
        seasons_inserted = summary.seasons_inserted,
        seasons_skipped = summary.seasons_skipped,
        subjects_inserted = summary.subjects_inserted,
        mal_anime_upserted = summary.mal_anime_upserted,
        rating_fallback = summary.rating_fallback,
        season_mal_inserted = summary.season_mal_inserted,
        season_mal_removed = summary.season_mal_removed,
        season_mal_skipped = summary.season_mal_skipped,
        matches_upserted = summary.matches_upserted,
        matches_skipped = summary.matches_skipped,
        "season-data import completed"
    );

    Ok(summary)
}

fn load_raw_mal_ratings(dir: &Path) -> Result<HashMap<i32, String>> {
    if !dir.is_dir() {
        anyhow::bail!("mal-json-dir is not a directory: {}", dir.display());
    }

    let mut json_paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("failed to read mal-json-dir: {}", dir.display()))?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
        })
        .collect();
    json_paths.sort();

    let mut map = HashMap::new();
    for path in json_paths {
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read MAL JSON file: {}", path.display()))?;
        let payload: MalJsonPayload = serde_json::from_str(&content)
            .with_context(|| format!("failed to parse MAL JSON file: {}", path.display()))?;
        let items = match payload {
            MalJsonPayload::Envelope { items } => items,
            MalJsonPayload::List(items) => items,
        };
        for item in items {
            if let Some(rating) = item.rating.map(|r| r.trim().to_string())
                && !rating.is_empty()
            {
                map.insert(item.id, rating);
            }
        }
    }

    Ok(map)
}

async fn read_source_seasons(pool: &SqlitePool) -> Result<Vec<SourceSeason>> {
    sqlx::query_as::<_, SourceSeason>("SELECT id FROM seasons")
        .fetch_all(pool)
        .await
        .context("failed to read source seasons")
}

async fn read_source_bgm_subjects(pool: &SqlitePool) -> Result<HashMap<i32, SourceBgmSubject>> {
    let rows = sqlx::query_as::<_, SourceBgmSubject>(
        "SELECT bgm_id, name, name_cn, air_date, platform, summary, tags, nsfw FROM bgm_subject",
    )
    .fetch_all(pool)
    .await
    .context("failed to read source bgm_subject")?;

    Ok(rows.into_iter().map(|r| (r.bgm_id, r)).collect())
}

async fn read_source_mal_anime(pool: &SqlitePool) -> Result<Vec<SourceMalAnime>> {
    sqlx::query_as::<_, SourceMalAnime>(
        r#"
        SELECT
            mal_id, title, title_ja, title_en, media_type, rating,
            start_date, end_date, num_episodes, source, studios,
            synopsis, start_season_year, start_season, fetched_at
        FROM mal_anime
        "#,
    )
    .fetch_all(pool)
    .await
    .context("failed to read source mal_anime")
}

async fn read_source_season_items(pool: &SqlitePool) -> Result<Vec<SourceSeasonItem>> {
    sqlx::query_as::<_, SourceSeasonItem>(
        r#"
        SELECT
            season_id, mal_id, status, source, confidence, bgm_id,
            candidates, error, origin, updated_at, reason, note
        FROM season_items
        "#,
    )
    .fetch_all(pool)
    .await
    .context("failed to read source season_items")
}

async fn read_source_overrides(pool: &SqlitePool) -> Result<Vec<SourceOverride>> {
    sqlx::query_as::<_, SourceOverride>(
        "SELECT mal_id, season_id, action, bgm_id, reason, note FROM overrides",
    )
    .fetch_all(pool)
    .await
    .context("failed to read source overrides")
}

fn month_to_season_label(month: i32) -> &'static str {
    match month {
        1 => "WINTER",
        4 => "SPRING",
        7 => "SUMMER",
        10 => "FALL",
        _ => unreachable!("season_key_to_id only produces months 1, 4, 7, 10"),
    }
}

async fn import_seasons(
    tx: &mut Transaction<'_, Sqlite>,
    source_seasons: &[SourceSeason],
    summary: &mut ImportSummary,
) -> Result<HashSet<i32>> {
    let existing: Vec<i32> = sqlx::query_scalar("SELECT season_id FROM seasons")
        .fetch_all(&mut **tx)
        .await
        .context("failed to load existing target season ids")?;
    let mut known: HashSet<i32> = existing.into_iter().collect();

    for row in source_seasons {
        let Some(season_id) = season_key_to_id(&row.id) else {
            tracing::warn!(season_key = %row.id, "skipping unparsable season key");
            summary.seasons_skipped += 1;
            continue;
        };
        let year = season_id / 100;
        let season = month_to_season_label(season_id % 100);

        let res = sqlx::query(
            r#"
            INSERT INTO seasons (season_id, year, season, name)
            VALUES (?, ?, ?, NULL)
            ON CONFLICT (season_id) DO NOTHING
            "#,
        )
        .bind(season_id)
        .bind(year)
        .bind(season)
        .execute(&mut **tx)
        .await
        .with_context(|| format!("failed to insert season {}", season_id))?;

        if res.rows_affected() > 0 {
            summary.seasons_inserted += 1;
        }
        known.insert(season_id);
    }

    Ok(known)
}

async fn import_subject_skeletons(
    tx: &mut Transaction<'_, Sqlite>,
    season_items: &[SourceSeasonItem],
    overrides: &[SourceOverride],
    bgm_subjects: &HashMap<i32, SourceBgmSubject>,
    summary: &mut ImportSummary,
) -> Result<()> {
    let referenced_ids: BTreeSet<i32> = season_items
        .iter()
        .filter_map(|item| item.bgm_id)
        .chain(overrides.iter().filter_map(|ov| ov.bgm_id))
        .collect();

    for bgm_id in referenced_ids {
        let res = if let Some(subj) = bgm_subjects.get(&bgm_id) {
            sqlx::query(
                r#"
                INSERT INTO subjects (
                    id, name, name_cn, air_date, platform, summary, tags, nsfw, last_updated_at
                )
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, NULL)
                ON CONFLICT (id) DO NOTHING
                "#,
            )
            .bind(subj.bgm_id)
            .bind(&subj.name)
            .bind(&subj.name_cn)
            .bind(&subj.air_date)
            .bind(&subj.platform)
            .bind(&subj.summary)
            .bind(&subj.tags)
            .bind(subj.nsfw)
            .execute(&mut **tx)
            .await
            .with_context(|| format!("failed to insert subject skeleton {}", bgm_id))?
        } else {
            sqlx::query(
                r#"
                INSERT INTO subjects (id, last_updated_at)
                VALUES (?, NULL)
                ON CONFLICT (id) DO NOTHING
                "#,
            )
            .bind(bgm_id)
            .execute(&mut **tx)
            .await
            .with_context(|| format!("failed to insert id-only subject skeleton {}", bgm_id))?
        };

        if res.rows_affected() > 0 {
            summary.subjects_inserted += 1;
        }
    }

    Ok(())
}

/// Resolves the rating to store in `mal_anime.rating` along with whether it came from
/// raw MAL JSON (`true`) or fallback mapping (`false`).
fn resolve_mal_rating(
    mal_id: i32,
    source_rating: Option<&str>,
    raw_ratings: &HashMap<i32, String>,
    fallback_count: &mut usize,
) -> (Option<String>, bool) {
    if let Some(raw) = raw_ratings.get(&mal_id) {
        return (Some(raw.clone()), true);
    }

    let mapped = match source_rating.map(str::trim) {
        Some("kids") => {
            *fallback_count += 1;
            Some("g".to_string())
        }
        Some("r18") => {
            *fallback_count += 1;
            Some("rx".to_string())
        }
        Some("general") => {
            *fallback_count += 1;
            Some("pg_13".to_string())
        }
        Some("") | None => None,
        Some(other) => Some(other.to_string()),
    };

    (mapped, false)
}

async fn import_mal_anime(
    tx: &mut Transaction<'_, Sqlite>,
    source_mal: &[SourceMalAnime],
    raw_ratings: &HashMap<i32, String>,
    now_str: &str,
    summary: &mut ImportSummary,
) -> Result<HashSet<i32>> {
    let existing: Vec<i32> = sqlx::query_scalar("SELECT mal_id FROM mal_anime")
        .fetch_all(&mut **tx)
        .await
        .context("failed to load existing target mal_anime ids")?;
    let mut known: HashSet<i32> = existing.into_iter().collect();

    for row in source_mal {
        let (rating, has_raw_rating) = resolve_mal_rating(
            row.mal_id,
            row.rating.as_deref(),
            raw_ratings,
            &mut summary.rating_fallback,
        );
        let fetched_at = row
            .fetched_at
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(now_str);

        let res = sqlx::query(
            r#"
            INSERT INTO mal_anime (
                mal_id, title, title_ja, title_en, media_type, rating,
                start_date, end_date, num_episodes, source, studios,
                synopsis, start_season_year, start_season, fetched_at
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT (mal_id) DO UPDATE SET
                title = EXCLUDED.title,
                title_ja = EXCLUDED.title_ja,
                title_en = EXCLUDED.title_en,
                media_type = EXCLUDED.media_type,
                rating = CASE
                    WHEN ? OR mal_anime.rating IS NULL THEN EXCLUDED.rating
                    ELSE mal_anime.rating
                END,
                start_date = EXCLUDED.start_date,
                end_date = EXCLUDED.end_date,
                num_episodes = EXCLUDED.num_episodes,
                source = EXCLUDED.source,
                studios = EXCLUDED.studios,
                synopsis = EXCLUDED.synopsis,
                start_season_year = EXCLUDED.start_season_year,
                start_season = EXCLUDED.start_season,
                fetched_at = EXCLUDED.fetched_at
            "#,
        )
        .bind(row.mal_id)
        .bind(&row.title)
        .bind(&row.title_ja)
        .bind(&row.title_en)
        .bind(&row.media_type)
        .bind(&rating)
        .bind(&row.start_date)
        .bind(&row.end_date)
        .bind(row.num_episodes)
        .bind(&row.source)
        .bind(&row.studios)
        .bind(&row.synopsis)
        .bind(row.start_season_year)
        .bind(&row.start_season)
        .bind(fetched_at)
        .bind(has_raw_rating)
        .execute(&mut **tx)
        .await
        .with_context(|| format!("failed to upsert mal_anime {}", row.mal_id))?;

        if res.rows_affected() > 0 {
            summary.mal_anime_upserted += 1;
        }
        known.insert(row.mal_id);
    }

    if summary.rating_fallback > 0 {
        tracing::warn!(
            rating_fallback = summary.rating_fallback,
            "used fallback rating mapping for mal_anime rows without raw JSON rating"
        );
    }

    Ok(known)
}

fn validate_season_mal_ref(
    season_key: &str,
    mal_id: i32,
    known_seasons: &HashSet<i32>,
    known_mal_ids: &HashSet<i32>,
    source_table: &'static str,
) -> Option<i32> {
    let Some(season_id) = season_key_to_id(season_key) else {
        tracing::warn!(
            source_table,
            season_key = %season_key,
            mal_id,
            "skipping row with unparsable season_id"
        );
        return None;
    };
    if !known_seasons.contains(&season_id) {
        tracing::warn!(
            source_table,
            season_id,
            mal_id,
            "skipping row: season_id not present in seasons"
        );
        return None;
    }
    if !known_mal_ids.contains(&mal_id) {
        tracing::warn!(
            source_table,
            season_id,
            mal_id,
            "skipping row: mal_id not found in mal_anime"
        );
        return None;
    }
    Some(season_id)
}

async fn import_season_mal(
    tx: &mut Transaction<'_, Sqlite>,
    season_items: &[SourceSeasonItem],
    overrides: &[SourceOverride],
    known_seasons: &HashSet<i32>,
    known_mal_ids: &HashSet<i32>,
    summary: &mut ImportSummary,
) -> Result<()> {
    for item in season_items {
        let Some(season_id) = validate_season_mal_ref(
            &item.season_id,
            item.mal_id,
            known_seasons,
            known_mal_ids,
            "season_items",
        ) else {
            summary.season_mal_skipped += 1;
            continue;
        };

        let origin = if item.origin.as_deref() == Some("override") {
            "manual"
        } else {
            "mal"
        };
        let is_wrong_season = item.reason.as_deref().map(str::trim) == Some("wrong_season");

        let res = if is_wrong_season {
            sqlx::query(
                r#"
                INSERT INTO season_mal (season_id, mal_id, origin, is_removed, note)
                VALUES (?, ?, ?, 1, ?)
                ON CONFLICT (season_id, mal_id) DO UPDATE SET
                    is_removed = 1,
                    note = COALESCE(season_mal.note, EXCLUDED.note)
                "#,
            )
            .bind(season_id)
            .bind(item.mal_id)
            .bind(origin)
            .bind(&item.note)
            .execute(&mut **tx)
            .await
        } else {
            sqlx::query(
                r#"
                INSERT INTO season_mal (season_id, mal_id, origin, is_removed, note)
                VALUES (?, ?, ?, 0, ?)
                ON CONFLICT (season_id, mal_id) DO NOTHING
                "#,
            )
            .bind(season_id)
            .bind(item.mal_id)
            .bind(origin)
            .bind(&item.note)
            .execute(&mut **tx)
            .await
        }
        .with_context(|| {
            format!(
                "failed to insert season_mal ({}, {})",
                season_id, item.mal_id
            )
        })?;

        if res.rows_affected() > 0 {
            if is_wrong_season {
                summary.season_mal_removed += 1;
            } else {
                summary.season_mal_inserted += 1;
            }
        }
    }

    for ov in overrides {
        let is_skip_or_wrong_season = ov.action.as_deref() == Some("skip")
            || ov.reason.as_deref().map(str::trim) == Some("wrong_season");
        let is_add = ov.action.as_deref() == Some("add");
        if !is_skip_or_wrong_season && !is_add {
            continue;
        }

        let Some(season_id) = validate_season_mal_ref(
            &ov.season_id,
            ov.mal_id,
            known_seasons,
            known_mal_ids,
            "overrides",
        ) else {
            summary.season_mal_skipped += 1;
            continue;
        };

        if is_skip_or_wrong_season {
            let res = sqlx::query(
                r#"
                INSERT INTO season_mal (season_id, mal_id, origin, is_removed, note)
                VALUES (?, ?, 'mal', 1, ?)
                ON CONFLICT (season_id, mal_id) DO UPDATE SET
                    is_removed = 1,
                    note = COALESCE(season_mal.note, EXCLUDED.note)
                "#,
            )
            .bind(season_id)
            .bind(ov.mal_id)
            .bind(&ov.note)
            .execute(&mut **tx)
            .await
            .with_context(|| {
                format!(
                    "failed to apply skip/wrong_season override on season_mal ({}, {})",
                    season_id, ov.mal_id
                )
            })?;

            if res.rows_affected() > 0 {
                summary.season_mal_removed += 1;
            }
        } else if is_add {
            let res = sqlx::query(
                r#"
                INSERT INTO season_mal (season_id, mal_id, origin, is_removed, note)
                VALUES (?, ?, 'manual', 0, ?)
                ON CONFLICT (season_id, mal_id) DO UPDATE SET
                    origin = 'manual',
                    is_removed = 0,
                    note = COALESCE(season_mal.note, EXCLUDED.note)
                "#,
            )
            .bind(season_id)
            .bind(ov.mal_id)
            .bind(&ov.note)
            .execute(&mut **tx)
            .await
            .with_context(|| {
                format!(
                    "failed to apply add override on season_mal ({}, {})",
                    season_id, ov.mal_id
                )
            })?;

            if res.rows_affected() > 0 {
                summary.season_mal_inserted += 1;
            }
        }
    }

    Ok(())
}

fn parse_timestamp_for_cmp(ts: Option<&str>) -> Option<DateTime<Utc>> {
    let raw = ts?.trim();
    if raw.is_empty() {
        return None;
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(raw) {
        return Some(dt.with_timezone(&Utc));
    }
    for fmt in [
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
    ] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(raw, fmt) {
            return Some(DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc));
        }
    }
    None
}

fn cmp_updated_at(a: Option<&str>, b: Option<&str>) -> Ordering {
    match (parse_timestamp_for_cmp(a), parse_timestamp_for_cmp(b)) {
        (Some(ta), Some(tb)) => ta.cmp(&tb),
        _ => a.unwrap_or("").cmp(b.unwrap_or("")),
    }
}

/// Tiebreak order when merging duplicate `season_items` rows for the same `mal_id`
/// into `mal_bgm_matches`:
/// 1. `status == "included"` wins over non-`included` (`pending` / `excluded`).
/// 2. `source == "human"` wins over automated/empty sources.
/// 3. Latest `updated_at` timestamp wins.
/// 4. Deterministic fallback: having a `bgm_id`, then higher `season_id` string.
fn is_better_match_candidate(candidate: &SourceSeasonItem, current: &SourceSeasonItem) -> bool {
    let cand_included = candidate.status.as_deref() == Some("included");
    let curr_included = current.status.as_deref() == Some("included");
    if cand_included != curr_included {
        return cand_included;
    }

    let cand_human = candidate.source.as_deref() == Some("human");
    let curr_human = current.source.as_deref() == Some("human");
    if cand_human != curr_human {
        return cand_human;
    }

    match cmp_updated_at(
        candidate.updated_at.as_deref(),
        current.updated_at.as_deref(),
    ) {
        Ordering::Greater => return true,
        Ordering::Less => return false,
        Ordering::Equal => {}
    }

    let cand_has_bgm = candidate.bgm_id.is_some();
    let curr_has_bgm = current.bgm_id.is_some();
    if cand_has_bgm != curr_has_bgm {
        return cand_has_bgm;
    }

    candidate.season_id > current.season_id
}

async fn import_matches(
    tx: &mut Transaction<'_, Sqlite>,
    season_items: &[SourceSeasonItem],
    known_mal_ids: &HashSet<i32>,
    now_str: &str,
    summary: &mut ImportSummary,
) -> Result<()> {
    let mut best_by_mal: BTreeMap<i32, &SourceSeasonItem> = BTreeMap::new();
    let mut wrong_season_by_mal: BTreeMap<i32, &SourceSeasonItem> = BTreeMap::new();
    let mut skipped_mal_ids: HashSet<i32> = HashSet::new();

    for item in season_items {
        if !known_mal_ids.contains(&item.mal_id) {
            if skipped_mal_ids.insert(item.mal_id) {
                tracing::warn!(
                    mal_id = item.mal_id,
                    "skipping mal_bgm_matches import: mal_id not found in mal_anime"
                );
                summary.matches_skipped += 1;
            }
            continue;
        }

        if item.reason.as_deref().map(str::trim) == Some("wrong_season") {
            match wrong_season_by_mal.get(&item.mal_id) {
                Some(current) if !is_better_match_candidate(item, current) => {}
                _ => {
                    wrong_season_by_mal.insert(item.mal_id, item);
                }
            }
            continue;
        }

        match best_by_mal.get(&item.mal_id) {
            Some(current) if !is_better_match_candidate(item, current) => {}
            _ => {
                best_by_mal.insert(item.mal_id, item);
            }
        }
    }

    let all_mal_ids: BTreeSet<i32> = best_by_mal
        .keys()
        .chain(wrong_season_by_mal.keys())
        .copied()
        .collect();

    for mal_id in all_mal_ids {
        let (status, source, bgm_id, confidence, candidates, reason, note, error, updated_at) =
            if let Some(item) = best_by_mal.get(&mal_id) {
                let status = item
                    .status
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .unwrap_or("pending");
                let source = item
                    .source
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty());
                let updated_at = item
                    .updated_at
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .unwrap_or(now_str);
                (
                    status,
                    source,
                    item.bgm_id,
                    item.confidence,
                    item.candidates.as_deref(),
                    item.reason.as_deref(),
                    item.note.as_deref(),
                    item.error.as_deref(),
                    updated_at,
                )
            } else {
                // Only wrong_season row(s) existed for this mal_id: import as pending
                // without propagating excluded/wrong_season globally.
                let item = wrong_season_by_mal[&mal_id];
                let updated_at = item
                    .updated_at
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .unwrap_or(now_str);
                (
                    "pending",
                    None,
                    None,
                    None,
                    item.candidates.as_deref(),
                    None,
                    None,
                    None,
                    updated_at,
                )
            };

        // Guard against downgrading an already curated or included target match.
        let res = sqlx::query(
            r#"
            INSERT INTO mal_bgm_matches (
                mal_id, status, source, bgm_id, confidence,
                candidates, reason, note, error, updated_at
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT (mal_id) DO UPDATE SET
                status = EXCLUDED.status,
                source = EXCLUDED.source,
                bgm_id = EXCLUDED.bgm_id,
                confidence = EXCLUDED.confidence,
                candidates = EXCLUDED.candidates,
                reason = EXCLUDED.reason,
                note = EXCLUDED.note,
                error = EXCLUDED.error,
                updated_at = EXCLUDED.updated_at
            WHERE mal_bgm_matches.status != 'included'
              AND COALESCE(mal_bgm_matches.source, '') != 'human'
            "#,
        )
        .bind(mal_id)
        .bind(status)
        .bind(source)
        .bind(bgm_id)
        .bind(confidence)
        .bind(candidates)
        .bind(reason)
        .bind(note)
        .bind(error)
        .bind(updated_at)
        .execute(&mut **tx)
        .await
        .with_context(|| format!("failed to upsert mal_bgm_matches for mal_id {}", mal_id))?;

        if res.rows_affected() > 0 {
            summary.matches_upserted += 1;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dal::repositories::{
        MalAnimeRepository, MalBgmMatchRepository, SeasonMalRepository, SeasonRepository,
        SubjectRepository,
    };
    use sqlx::SqlitePool;
    use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
    use tracing_test::traced_test;

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(prefix: &str) -> Self {
            let n = TEMP_COUNTER.fetch_add(1, AtomicOrdering::SeqCst);
            let path = std::env::temp_dir().join(format!(
                "bgm-rank-api-{}-{}-{}",
                prefix,
                std::process::id(),
                n
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    async fn create_and_seed_source_db(db_path: &Path) -> SqlitePool {
        let opts = SqliteConnectOptions::new()
            .filename(db_path)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .unwrap();

        sqlx::query(
            r#"
            CREATE TABLE seasons (
                id TEXT PRIMARY KEY
            );
            CREATE TABLE mal_anime (
                mal_id INTEGER PRIMARY KEY,
                title TEXT NOT NULL,
                title_ja TEXT,
                title_en TEXT,
                media_type TEXT NOT NULL,
                rating TEXT,
                start_date TEXT,
                end_date TEXT,
                num_episodes INTEGER,
                source TEXT,
                studios TEXT,
                synopsis TEXT,
                start_season_year INTEGER,
                start_season TEXT,
                fetched_at TEXT
            );
            CREATE TABLE season_items (
                season_id TEXT NOT NULL,
                mal_id INTEGER NOT NULL,
                status TEXT,
                source TEXT,
                confidence REAL,
                bgm_id INTEGER,
                candidates TEXT,
                error TEXT,
                origin TEXT NOT NULL DEFAULT 'mal',
                updated_at TEXT,
                reason TEXT,
                note TEXT,
                PRIMARY KEY (season_id, mal_id)
            );
            CREATE TABLE overrides (
                mal_id INTEGER NOT NULL,
                season_id TEXT NOT NULL,
                action TEXT,
                bgm_id INTEGER,
                reason TEXT,
                target_season_id TEXT,
                note TEXT,
                created_at TEXT
            );
            CREATE TABLE bgm_subject (
                bgm_id INTEGER PRIMARY KEY,
                type INTEGER,
                name TEXT,
                name_cn TEXT,
                air_date TEXT,
                platform TEXT,
                summary TEXT,
                tags TEXT,
                nsfw BOOLEAN,
                fetched_at TEXT
            );
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        // Seasons: two valid + one unparsable
        sqlx::query(
            "INSERT INTO seasons (id) VALUES ('2026-winter'), ('2026-spring'), ('bad-key')",
        )
        .execute(&pool)
        .await
        .unwrap();

        // mal_anime:
        // 101: general + fetched_at NULL (tests fallback timestamp + raw JSON rating override / fallback)
        // 102: kids -> fallback 'g'
        // 103: r18 -> fallback 'rx'
        // 104: general -> fallback 'pg_13'
        // 105: general -> present in JSON as 'r'
        sqlx::query(
            r#"
            INSERT INTO mal_anime (
                mal_id, title, title_ja, title_en, media_type, rating,
                start_date, end_date, num_episodes, source, studios,
                synopsis, start_season_year, start_season, fetched_at
            ) VALUES
                (101, 'Frieren S2', '葬送のフリーレン', 'Frieren', 'tv', 'general', '2026-01-16', NULL, 12, 'manga', '["Madhouse"]', 'Syn 101', 2026, 'winter', NULL),
                (102, 'Kids Show', 'キッズ', NULL, 'tv', 'kids', '2026-01-05', NULL, 24, 'original', '[]', NULL, 2026, 'winter', '2026-01-01 00:00:00'),
                (103, 'Adult OVA', NULL, NULL, 'ova', 'r18', NULL, NULL, 2, 'original', '[]', NULL, 2026, 'spring', '2026-01-02 00:00:00'),
                (104, 'Wrong Season Show', '間違え', NULL, 'tv', 'general', NULL, NULL, 12, 'manga', '[]', NULL, 2026, 'spring', '2026-01-03 00:00:00'),
                (105, 'Human Excluded Show', '除外', NULL, 'ona', 'general', NULL, NULL, 1, 'original', '[]', NULL, 2026, 'spring', '2026-01-04 00:00:00')
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        // bgm_subject: 5001 exists; 5002 referenced by season_items but missing from bgm_subject
        sqlx::query(
            r#"
            INSERT INTO bgm_subject (bgm_id, type, name, name_cn, air_date, platform, summary, tags, nsfw, fetched_at)
            VALUES (5001, 2, '葬送のフリーレン 2期', '葬送的芙莉莲 第二季', '2026-01-16', 'TV', 'Summary 5001', '["奇幻"]', 0, '2026-01-10 00:00:00')
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        // season_items:
        // - mal 101 appears in 2026-winter (pending, newer) AND 2026-spring (included, older) -> included must win!
        // - mal 102 in 2026-winter -> excluded, missing bgm_subject 5002 referenced on override below
        // - mal 104 in 2026-winter (mal) + 2026-spring (origin='override', note='moved here')
        // - mal 105 in 2026-spring (source='human', status='excluded', reason='mini')
        // - mal 999 in 2026-winter (not in mal_anime -> skipped)
        // - mal 101 in 'bad-key' (unparsable season -> skipped)
        sqlx::query(
            r#"
            INSERT INTO season_items (
                season_id, mal_id, status, source, confidence, bgm_id,
                candidates, error, origin, updated_at, reason, note
            ) VALUES
                ('2026-winter', 101, 'pending', 'llm', 0.4, NULL, '[]', NULL, 'mal', '2026-02-01 00:00:00', NULL, NULL),
                ('2026-spring', 101, 'included', 'exact', 1.0, 5001, '[]', NULL, 'mal', '2026-01-15 00:00:00', NULL, 'matched in spring'),
                ('2026-winter', 102, 'excluded', 'rule', NULL, NULL, NULL, NULL, 'mal', '2026-01-10 00:00:00', 'kids', NULL),
                ('2026-winter', 104, 'included', 'llm', 0.9, 5002, '[]', NULL, 'mal', '2026-01-10 00:00:00', NULL, NULL),
                ('2026-spring', 104, 'included', 'llm', 0.9, 5002, '[]', NULL, 'override', '2026-01-20 00:00:00', NULL, 'override add'),
                ('2026-spring', 105, 'excluded', 'human', NULL, NULL, NULL, NULL, 'mal', '2026-01-11 00:00:00', 'mini', 'human note'),
                ('2026-winter', 999, 'pending', NULL, NULL, NULL, NULL, NULL, 'mal', '2026-01-11 00:00:00', NULL, NULL),
                ('bad-key', 101, 'pending', NULL, NULL, NULL, NULL, NULL, 'mal', '2026-01-11 00:00:00', NULL, NULL)
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        // overrides:
        // - skip on (2026-winter, 104) where row exists with note NULL -> sets is_removed=1 and copies override.note
        // - wrong_season on (2026-winter, 103) where season_mal row doesn't exist -> inserts is_removed=1, origin='mal', note='wrong season'
        // - add on (2026-spring, 103) with bgm_id 5003 -> ensures 5003 skeleton is created, does not mark 103 removed in spring
        sqlx::query(
            r#"
            INSERT INTO overrides (mal_id, season_id, action, bgm_id, reason, target_season_id, note, created_at)
            VALUES
                (104, '2026-winter', 'skip', NULL, 'wrong_season', '2026-spring', 'moved to 2026-spring', '2026-01-20 00:00:00'),
                (103, '2026-winter', NULL, NULL, 'wrong_season', NULL, 'not winter', '2026-01-20 00:00:00'),
                (103, '2026-spring', 'add', 5003, NULL, NULL, 'added manually', '2026-01-20 00:00:00')
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        pool
    }

    async fn table_count(pool: &SqlitePool, table: &str) -> i64 {
        let sql = format!("SELECT COUNT(*) FROM {}", table);
        sqlx::query_scalar(&sql).fetch_one(pool).await.unwrap()
    }

    #[sqlx::test]
    #[traced_test]
    async fn test_import_season_data_full_flow_and_idempotency(pool: SqlitePool) {
        let dir = TempDir::new("full");
        let source_db_path = dir.path().join("season.db");
        let json_dir = dir.path().join("mal_json");
        std::fs::create_dir_all(&json_dir).unwrap();

        let source_pool = create_and_seed_source_db(&source_db_path).await;
        source_pool.close().await;

        // Seed raw JSON files: one envelope format, one top-level array format
        std::fs::write(
            json_dir.join("2026-winter.json"),
            r#"{"season":"2026-winter","items":[{"id":101,"rating":"pg_13"}]}"#,
        )
        .unwrap();
        std::fs::write(
            json_dir.join("2026-spring.json"),
            r#"[{"id":105,"rating":"r"}]"#,
        )
        .unwrap();

        let db = Database::from_pool(pool);
        let summary = import_season_data(&db, &source_db_path, Some(&json_dir))
            .await
            .expect("first import should succeed");

        assert_eq!(summary.seasons_inserted, 2);
        assert_eq!(summary.seasons_skipped, 1);
        assert_eq!(summary.subjects_inserted, 3); // 5001, 5002, 5003
        assert_eq!(summary.mal_anime_upserted, 5);
        assert_eq!(summary.rating_fallback, 3); // 102(kids->g), 103(r18->rx), 104(general->pg_13)
        assert!(logs_contain("rating_fallback"));

        // Verify seasons
        let seasons = SeasonRepository::new(db.pool()).find_all().await.unwrap();
        assert_eq!(seasons.len(), 2);
        assert_eq!(seasons[0].season_id, 202604);
        assert_eq!(seasons[0].season, "SPRING");
        assert_eq!(seasons[1].season_id, 202601);
        assert_eq!(seasons[1].season, "WINTER");

        // Verify subject skeletons (last_updated_at NULL)
        let s5001 = SubjectRepository::new(db.pool())
            .find_by_id(5001)
            .await
            .unwrap()
            .expect("5001 skeleton");
        assert_eq!(s5001.name_cn.as_deref(), Some("葬送的芙莉莲 第二季"));
        assert_eq!(s5001.air_date.as_deref(), Some("2026-01-16"));
        assert!(s5001.last_updated_at.is_none());

        let s5002 = SubjectRepository::new(db.pool())
            .find_by_id(5002)
            .await
            .unwrap()
            .expect("5002 id-only skeleton");
        assert!(s5002.name.is_none());
        assert!(s5002.last_updated_at.is_none());

        let s5003 = SubjectRepository::new(db.pool())
            .find_by_id(5003)
            .await
            .unwrap()
            .expect("5003 id-only skeleton from overrides");
        assert!(s5003.last_updated_at.is_none());

        // Verify mal_anime ratings & fallback fetched_at
        let mal_repo = MalAnimeRepository::new(db.pool());
        assert_eq!(
            mal_repo.get(101).await.unwrap().unwrap().rating.as_deref(),
            Some("pg_13")
        );
        assert_eq!(
            mal_repo.get(102).await.unwrap().unwrap().rating.as_deref(),
            Some("g")
        );
        assert_eq!(
            mal_repo.get(103).await.unwrap().unwrap().rating.as_deref(),
            Some("rx")
        );
        assert_eq!(
            mal_repo.get(104).await.unwrap().unwrap().rating.as_deref(),
            Some("pg_13")
        );
        assert_eq!(
            mal_repo.get(105).await.unwrap().unwrap().rating.as_deref(),
            Some("r")
        );

        // Verify season_mal origin mapping & is_removed flags & note copying
        let sm_repo = SeasonMalRepository::new(db.pool());
        let sm_104_winter = sm_repo.get(202601, 104).await.unwrap().unwrap();
        assert_eq!(sm_104_winter.origin, "mal");
        assert!(sm_104_winter.is_removed);
        assert_eq!(sm_104_winter.note.as_deref(), Some("moved to 2026-spring"));

        let sm_104_spring = sm_repo.get(202604, 104).await.unwrap().unwrap();
        assert_eq!(sm_104_spring.origin, "manual");
        assert!(!sm_104_spring.is_removed);
        assert_eq!(sm_104_spring.note.as_deref(), Some("override add"));

        let sm_103_winter = sm_repo.get(202601, 103).await.unwrap().unwrap();
        assert_eq!(sm_103_winter.origin, "mal");
        assert!(sm_103_winter.is_removed);
        assert_eq!(sm_103_winter.note.as_deref(), Some("not winter"));

        let sm_103_spring = sm_repo.get(202604, 103).await.unwrap().unwrap();
        assert_eq!(sm_103_spring.origin, "manual");
        assert!(!sm_103_spring.is_removed);
        assert_eq!(sm_103_spring.note.as_deref(), Some("added manually"));

        // Verify merged mal_bgm_matches
        let match_repo = MalBgmMatchRepository::new(db.pool());
        let m101 = match_repo.get(101).await.unwrap().unwrap();
        assert_eq!(m101.status, "included");
        assert_eq!(m101.source.as_deref(), Some("exact"));
        assert_eq!(m101.bgm_id, Some(5001));

        let m105 = match_repo.get(105).await.unwrap().unwrap();
        assert_eq!(m105.status, "excluded");
        assert_eq!(m105.source.as_deref(), Some("human"));
        assert_eq!(m105.reason.as_deref(), Some("mini"));

        // Capture row counts across all 5 tables
        let counts_before = (
            table_count(db.pool(), "seasons").await,
            table_count(db.pool(), "subjects").await,
            table_count(db.pool(), "mal_anime").await,
            table_count(db.pool(), "season_mal").await,
            table_count(db.pool(), "mal_bgm_matches").await,
        );

        // Re-run import WITHOUT --mal-json-dir: must be idempotent and MUST NOT overwrite raw 'r' on 105
        let second = import_season_data(&db, &source_db_path, None)
            .await
            .expect("second import should be idempotent");
        assert_eq!(second.seasons_inserted, 0);
        assert_eq!(second.subjects_inserted, 0);
        assert_eq!(
            mal_repo.get(105).await.unwrap().unwrap().rating.as_deref(),
            Some("r"),
            "existing non-null rating must not be overwritten when JSON dir is omitted"
        );

        let counts_after = (
            table_count(db.pool(), "seasons").await,
            table_count(db.pool(), "subjects").await,
            table_count(db.pool(), "mal_anime").await,
            table_count(db.pool(), "season_mal").await,
            table_count(db.pool(), "mal_bgm_matches").await,
        );
        assert_eq!(counts_before, counts_after);
    }

    #[sqlx::test]
    async fn test_import_never_downgrades_included_or_human_matches(pool: SqlitePool) {
        let dir = TempDir::new("guard");
        let source_db_path = dir.path().join("season.db");
        let source_pool = create_and_seed_source_db(&source_db_path).await;
        // Add a pending/llm row for 105 (or downgrade 101 to pending in source)
        sqlx::query("UPDATE season_items SET status = 'pending', source = 'llm', bgm_id = NULL WHERE mal_id = 101")
            .execute(&source_pool)
            .await
            .unwrap();
        sqlx::query(
            "UPDATE season_items SET status = 'pending', source = 'llm' WHERE mal_id = 105",
        )
        .execute(&source_pool)
        .await
        .unwrap();
        source_pool.close().await;

        let db = Database::from_pool(pool);
        // Pre-seed target with an 'included' match for 101 and a 'human' excluded match for 105
        sqlx::query("INSERT INTO subjects (id, last_updated_at) VALUES (9001, NULL)")
            .execute(db.pool())
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO mal_anime (mal_id, title, media_type, fetched_at) VALUES (101, 'T', 'tv', '2026-01-01 00:00:00'), (105, 'T2', 'ona', '2026-01-01 00:00:00')",
        )
        .execute(db.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO mal_bgm_matches (mal_id, status, source, bgm_id, updated_at) VALUES (101, 'included', 'exact', 9001, '2026-02-01 00:00:00'), (105, 'excluded', 'human', NULL, '2026-02-01 00:00:00')",
        )
        .execute(db.pool())
        .await
        .unwrap();

        import_season_data(&db, &source_db_path, None)
            .await
            .unwrap();

        let match_repo = MalBgmMatchRepository::new(db.pool());
        let m101 = match_repo.get(101).await.unwrap().unwrap();
        assert_eq!(m101.status, "included");
        assert_eq!(m101.bgm_id, Some(9001));

        let m105 = match_repo.get(105).await.unwrap().unwrap();
        assert_eq!(m105.status, "excluded");
        assert_eq!(m105.source.as_deref(), Some("human"));
    }

    #[test]
    fn test_match_tiebreak_prefers_included_then_human_then_latest_updated_at() {
        let base = SourceSeasonItem {
            season_id: "2026-winter".to_string(),
            mal_id: 1,
            status: Some("pending".to_string()),
            source: Some("llm".to_string()),
            confidence: Some(0.5),
            bgm_id: None,
            candidates: None,
            error: None,
            origin: Some("mal".to_string()),
            updated_at: Some("2026-02-01 00:00:00".to_string()),
            reason: None,
            note: None,
        };
        let included_older = SourceSeasonItem {
            season_id: "2026-spring".to_string(),
            mal_id: 1,
            status: Some("included".to_string()),
            source: Some("exact".to_string()),
            confidence: Some(1.0),
            bgm_id: Some(5001),
            candidates: None,
            error: None,
            origin: Some("mal".to_string()),
            updated_at: Some("2026-01-01 00:00:00".to_string()),
            reason: None,
            note: None,
        };
        let human_excluded = SourceSeasonItem {
            season_id: "2026-spring".to_string(),
            mal_id: 1,
            status: Some("excluded".to_string()),
            source: Some("human".to_string()),
            confidence: None,
            bgm_id: None,
            candidates: None,
            error: None,
            origin: Some("mal".to_string()),
            updated_at: Some("2026-01-01 00:00:00".to_string()),
            reason: Some("mini".to_string()),
            note: None,
        };
        let llm_newer = SourceSeasonItem {
            updated_at: Some("2026-03-01 00:00:00".to_string()),
            ..SourceSeasonItem {
                season_id: base.season_id.clone(),
                mal_id: 1,
                status: base.status.clone(),
                source: base.source.clone(),
                confidence: base.confidence,
                bgm_id: base.bgm_id,
                candidates: None,
                error: None,
                origin: base.origin.clone(),
                updated_at: None,
                reason: None,
                note: None,
            }
        };

        assert!(is_better_match_candidate(&included_older, &human_excluded));
        assert!(!is_better_match_candidate(&human_excluded, &included_older));
        assert!(is_better_match_candidate(&human_excluded, &llm_newer));
        assert!(is_better_match_candidate(&llm_newer, &base));
    }

    #[sqlx::test]
    async fn test_import_season_items_wrong_season_without_overrides(pool: SqlitePool) {
        let dir = TempDir::new("wrong_season");
        let source_db_path = dir.path().join("season.db");
        let source_pool = create_and_seed_source_db(&source_db_path).await;

        // Add mal 201 (only wrong_season in season_items, NO overrides row)
        // and mal 202 (wrong_season in 2026-winter with source='human' and newer updated_at,
        // plus real excluded/mini decision in 2026-spring).
        // Also mal 203 (wrong_season in 2026-winter, included in 2026-spring).
        sqlx::query(
            r#"
            INSERT INTO mal_anime (mal_id, title, media_type, rating) VALUES
                (201, 'Only Wrong Season', 'tv', 'general'),
                (202, 'Wrong Season Then Mini', 'ona', 'general'),
                (203, 'Wrong Season Then Included', 'tv', 'general')
            "#,
        )
        .execute(&source_pool)
        .await
        .unwrap();

        sqlx::query(
            r#"
            INSERT INTO season_items (
                season_id, mal_id, status, source, confidence, bgm_id,
                candidates, error, origin, updated_at, reason, note
            ) VALUES
                ('2026-winter', 201, 'excluded', 'human', NULL, NULL, '[{"bgm_id":7001}]', NULL, 'mal', '2026-02-01 00:00:00', 'wrong_season', 'belongs elsewhere'),
                ('2026-winter', 202, 'excluded', 'human', NULL, NULL, NULL, NULL, 'mal', '2026-03-01 00:00:00', 'wrong_season', 'moved out'),
                ('2026-spring', 202, 'excluded', 'rule', NULL, NULL, NULL, NULL, 'mal', '2026-01-10 00:00:00', 'mini', 'too short'),
                ('2026-winter', 203, 'excluded', 'human', NULL, NULL, NULL, NULL, 'mal', '2026-03-01 00:00:00', 'wrong_season', 'moved out'),
                ('2026-spring', 203, 'included', 'exact', 1.0, 5001, '[]', NULL, 'mal', '2026-01-10 00:00:00', NULL, NULL)
            "#,
        )
        .execute(&source_pool)
        .await
        .unwrap();
        source_pool.close().await;

        let db = Database::from_pool(pool);
        import_season_data(&db, &source_db_path, None)
            .await
            .unwrap();

        let sm_repo = SeasonMalRepository::new(db.pool());
        let sm_201 = sm_repo.get(202601, 201).await.unwrap().unwrap();
        assert!(sm_201.is_removed);
        assert_eq!(sm_201.note.as_deref(), Some("belongs elsewhere"));

        let sm_202_w = sm_repo.get(202601, 202).await.unwrap().unwrap();
        assert!(sm_202_w.is_removed);
        let sm_202_s = sm_repo.get(202604, 202).await.unwrap().unwrap();
        assert!(!sm_202_s.is_removed);

        let match_repo = MalBgmMatchRepository::new(db.pool());
        // 201: only had wrong_season -> must be pending, NOT excluded/wrong_season
        let m201 = match_repo.get(201).await.unwrap().unwrap();
        assert_eq!(m201.status, "pending");
        assert_eq!(m201.source, None);
        assert_eq!(m201.bgm_id, None);
        assert_eq!(m201.reason, None);

        // 202: real decision in 2026-spring must win over wrong_season in 2026-winter
        let m202 = match_repo.get(202).await.unwrap().unwrap();
        assert_eq!(m202.status, "excluded");
        assert_eq!(m202.source.as_deref(), Some("rule"));
        assert_eq!(m202.reason.as_deref(), Some("mini"));

        // 203: included decision in 2026-spring must win
        let m203 = match_repo.get(203).await.unwrap().unwrap();
        assert_eq!(m203.status, "included");
        assert_eq!(m203.bgm_id, Some(5001));
    }

    #[sqlx::test]
    async fn test_import_overrides_add_sets_manual_origin_even_when_season_item_origin_mal(
        pool: SqlitePool,
    ) {
        let dir = TempDir::new("override_add");
        let source_db_path = dir.path().join("season.db");
        let source_pool = create_and_seed_source_db(&source_db_path).await;

        sqlx::query(
            "INSERT INTO mal_anime (mal_id, title, media_type, rating) VALUES (301, 'Override Add Show', 'tv', 'general')",
        )
        .execute(&source_pool)
        .await
        .unwrap();
        sqlx::query(
            r#"
            INSERT INTO season_items (
                season_id, mal_id, status, source, origin, updated_at
            ) VALUES ('2026-spring', 301, 'pending', NULL, 'mal', '2026-01-10 00:00:00')
            "#,
        )
        .execute(&source_pool)
        .await
        .unwrap();
        sqlx::query(
            r#"
            INSERT INTO overrides (mal_id, season_id, action, bgm_id, reason, target_season_id, note, created_at)
            VALUES (301, '2026-spring', 'add', NULL, NULL, NULL, 'manual override add', '2026-01-20 00:00:00')
            "#,
        )
        .execute(&source_pool)
        .await
        .unwrap();
        source_pool.close().await;

        let db = Database::from_pool(pool);
        import_season_data(&db, &source_db_path, None)
            .await
            .unwrap();

        let sm = SeasonMalRepository::new(db.pool())
            .get(202604, 301)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(sm.origin, "manual");
        assert!(!sm.is_removed);
        assert_eq!(sm.note.as_deref(), Some("manual override add"));
    }

    #[sqlx::test]
    #[traced_test]
    async fn test_import_missing_mal_json_dir_warns_and_falls_back(pool: SqlitePool) {
        let dir = TempDir::new("missing_json");
        let source_db_path = dir.path().join("season.db");
        let source_pool = create_and_seed_source_db(&source_db_path).await;
        source_pool.close().await;

        let missing_json_dir = dir.path().join("does-not-exist");
        let db = Database::from_pool(pool);
        let summary = import_season_data(&db, &source_db_path, Some(&missing_json_dir))
            .await
            .expect("missing mal-json-dir should warn and fall back instead of erroring");

        assert!(logs_contain("mal-json-dir does not exist"));
        assert!(logs_contain("rating_fallback"));
        assert_eq!(summary.mal_anime_upserted, 5);
        assert_eq!(summary.rating_fallback, 5);

        let mal_repo = MalAnimeRepository::new(db.pool());
        assert_eq!(
            mal_repo.get(101).await.unwrap().unwrap().rating.as_deref(),
            Some("pg_13")
        );
    }
}
