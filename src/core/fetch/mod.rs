use std::fmt;
use std::sync::Arc;

use anyhow::{Result, bail};

use crate::dal::Database;
use crate::dal::dto::{CreateSeason, UpsertMalAnime};
use crate::dal::repositories::{
    MalAnimeRepository, MalBgmMatchRepository, SeasonMalRepository, SeasonRepository,
};
use crate::services::mal::{MalAnimeNode, MalClient};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FetchSummary {
    /// 当季开播新番条目数（已过滤续播/老番）
    pub fetched: usize,
    /// 新写入 `season_mal` 的行数（已存在/已移除的行不计）
    pub new: usize,
}

impl fmt::Display for FetchSummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "fetched={} new={}", self.fetched, self.new)
    }
}

impl std::ops::AddAssign for FetchSummary {
    fn add_assign(&mut self, rhs: Self) {
        self.fetched += rhs.fetched;
        self.new += rhs.new;
    }
}

/// 解析 `202601` 形式的 season_id 为 (year, 大写 season 标签, 小写 MAL season)。
pub fn parse_season_id(season_id: i32) -> Result<(i32, &'static str, &'static str)> {
    let year = season_id / 100;
    if !(1900..=2999).contains(&year) {
        bail!("invalid season_id: {}", season_id);
    }
    match season_id % 100 {
        1 => Ok((year, "WINTER", "winter")),
        4 => Ok((year, "SPRING", "spring")),
        7 => Ok((year, "SUMMER", "summer")),
        10 => Ok((year, "FALL", "fall")),
        _ => bail!(
            "invalid season_id: {} (month must be 01, 04, 07 or 10)",
            season_id
        ),
    }
}

/// 列出 [from, to]（含）之间的所有 season_id。
pub fn season_range(from: i32, to: i32) -> Result<Vec<i32>> {
    parse_season_id(from)?;
    parse_season_id(to)?;
    if from > to {
        bail!("--from ({}) must not be after --to ({})", from, to);
    }
    let mut out = Vec::new();
    let mut id = from;
    while id <= to {
        out.push(id);
        id = match id % 100 {
            10 => (id / 100 + 1) * 100 + 1,
            _ => id + 3,
        };
    }
    Ok(out)
}

/// 检查 MAL 条目是否为当季开播新番（过滤跨季续播与常年连载老番）。
pub fn is_new_anime(node: &MalAnimeNode, year: i32, mal_season: &str) -> bool {
    node.start_season
        .as_ref()
        .is_some_and(|s| s.year == year && s.season.trim().eq_ignore_ascii_case(mal_season))
}

pub(crate) fn to_upsert(node: MalAnimeNode) -> UpsertMalAnime {
    let studios = if node.studios.is_empty() {
        "[]".to_string()
    } else {
        let names: Vec<&str> = node.studios.iter().map(|s| s.name.as_str()).collect();
        serde_json::to_string(&names).unwrap_or_else(|_| "[]".to_string())
    };
    let (title_ja, title_en) = match node.alternative_titles {
        Some(a) => (
            a.ja.filter(|s| !s.is_empty()),
            a.en.filter(|s| !s.is_empty()),
        ),
        None => (None, None),
    };
    UpsertMalAnime {
        mal_id: node.id,
        title: node.title,
        title_ja,
        title_en,
        media_type: node
            .media_type
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "unknown".to_string()),
        rating: node.rating,
        start_date: node.start_date,
        end_date: node.end_date,
        num_episodes: node.num_episodes,
        source: node.source,
        studios: Some(studios),
        synopsis: node.synopsis,
        start_season_year: node.start_season.as_ref().map(|s| s.year),
        start_season: node.start_season.map(|s| s.season),
    }
}

pub struct MalFetchService {
    db: Arc<Database>,
    client: MalClient,
}

impl MalFetchService {
    pub fn new(db: Arc<Database>, client: MalClient) -> Self {
        Self { db, client }
    }

    /// 抓取单个季度：补建 seasons 行 → upsert mal_anime → season_mal 仅插入缺失行
    /// （`ON CONFLICT DO NOTHING`，已移除行不会复活）→ 补建 pending 匹配行（不触碰审核列）。
    pub async fn fetch_season(&self, season_id: i32) -> Result<FetchSummary> {
        let (year, season_label, mal_season) = parse_season_id(season_id)?;
        let pool = self.db.pool();

        let season_repo = SeasonRepository::new(pool);
        if season_repo.find_by_id(season_id).await?.is_none() {
            season_repo
                .upsert(CreateSeason {
                    season_id,
                    year,
                    season: season_label.to_string(),
                    name: None,
                })
                .await?;
        }

        let nodes = self.client.get_all_seasonal_anime(year, mal_season).await?;

        let anime_repo = MalAnimeRepository::new(pool);
        let season_mal_repo = SeasonMalRepository::new(pool);
        let match_repo = MalBgmMatchRepository::new(pool);

        let mut summary = FetchSummary::default();
        for node in nodes {
            if !is_new_anime(&node, year, mal_season) {
                continue;
            }
            let mal_id = node.id;
            anime_repo.upsert(to_upsert(node)).await?;
            if season_mal_repo.insert_ignore(season_id, mal_id).await? {
                summary.new += 1;
            }
            match_repo.insert_pending_ignore(mal_id).await?;
            summary.fetched += 1;
        }

        tracing::info!(
            season_id,
            fetched = summary.fetched,
            new = summary.new,
            "fetched MAL season"
        );
        Ok(summary)
    }

    /// 依次抓取 [from, to]（含）范围内的季度。
    pub async fn fetch_range(&self, from: i32, to: i32) -> Result<FetchSummary> {
        let mut total = FetchSummary::default();
        let interval = self.client.request_interval();
        for (idx, season_id) in season_range(from, to)?.into_iter().enumerate() {
            if idx > 0 && !interval.is_zero() {
                tokio::time::sleep(interval).await;
            }
            total += self.fetch_season(season_id).await?;
        }
        Ok(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dal::dto::UpdateMalBgmMatch;
    use mockito::{Matcher, Server, ServerGuard};
    use serde_json::json;
    use sqlx::SqlitePool;
    use std::time::Duration;

    fn node(id: i32, media_type: Option<&str>) -> serde_json::Value {
        let mut n = json!({
            "id": id,
            "title": format!("Anime {id}"),
            "alternative_titles": {"ja": format!("アニメ{id}"), "en": format!("Anime EN {id}")},
            "start_date": "2026-01-10",
            "num_episodes": 12,
            "start_season": {"year": 2026, "season": "winter"},
            "source": "manga",
            "studios": [{"id": 1, "name": "Madhouse"}, {"id": 2, "name": "Bones"}],
            "rating": "r+"
        });
        if let Some(m) = media_type {
            n["media_type"] = json!(m);
        }
        n
    }

    fn page(ids: &[i32], next: Option<&str>) -> String {
        let data: Vec<_> = ids
            .iter()
            .map(|i| json!({"node": node(*i, Some("tv"))}))
            .collect();
        let mut v = json!({"data": data, "paging": {}});
        if let Some(n) = next {
            v["paging"]["next"] = json!(n);
        }
        v.to_string()
    }

    fn service(pool: SqlitePool, server: &ServerGuard) -> MalFetchService {
        let client = MalClient::with_base_url(
            &server.url(),
            Duration::from_millis(1),
            Duration::from_millis(1),
        );
        MalFetchService::new(Arc::new(Database::from_pool(pool)), client)
    }

    #[test]
    fn test_season_range() {
        assert_eq!(
            season_range(202510, 202604).unwrap(),
            vec![202510, 202601, 202604]
        );
        assert!(season_range(202604, 202601).is_err());
        assert!(season_range(202602, 202604).is_err());
    }

    #[sqlx::test]
    async fn test_fetch_season_paging_and_fields(pool: SqlitePool) {
        let mut server = Server::new_async().await;
        let first: Vec<i32> = (1..=500).collect();
        let m1 = server
            .mock("GET", "/anime/season/2026/winter")
            .match_header("X-MAL-CLIENT-ID", "test-client-id")
            .match_query(Matcher::AllOf(vec![
                Matcher::UrlEncoded("offset".into(), "0".into()),
                Matcher::UrlEncoded("limit".into(), "500".into()),
                Matcher::UrlEncoded("nsfw".into(), "true".into()),
            ]))
            .with_body(page(&first, Some("http://next")))
            .expect(1)
            .create_async()
            .await;
        let m2 = server
            .mock("GET", "/anime/season/2026/winter")
            .match_query(Matcher::UrlEncoded("offset".into(), "500".into()))
            .with_body(json!({"data": [{"node": node(501, None)}], "paging": {}}).to_string())
            .expect(1)
            .create_async()
            .await;

        let svc = service(pool.clone(), &server);
        let summary = svc.fetch_season(202601).await.unwrap();
        assert_eq!(
            summary,
            FetchSummary {
                fetched: 501,
                new: 501
            }
        );
        m1.assert_async().await;
        m2.assert_async().await;

        let repo = MalAnimeRepository::new(&pool);
        let a = repo.get(1).await.unwrap().unwrap();
        assert_eq!(a.media_type, "tv");
        assert_eq!(a.rating.as_deref(), Some("r+"));
        assert_eq!(a.title_ja.as_deref(), Some("アニメ1"));
        assert_eq!(a.studios.as_deref(), Some(r#"["Madhouse","Bones"]"#));
        assert_eq!(a.start_season_year, Some(2026));
        assert_eq!(repo.get(501).await.unwrap().unwrap().media_type, "unknown");

        let season = SeasonRepository::new(&pool)
            .find_by_id(202601)
            .await
            .unwrap()
            .unwrap();
        assert_eq!((season.year, season.season.as_str()), (2026, "WINTER"));
        let m = MalBgmMatchRepository::new(&pool)
            .get(501)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(m.status, "pending");
    }

    #[sqlx::test]
    async fn test_fetch_season_retries_then_succeeds(pool: SqlitePool) {
        let mut server = Server::new_async().await;
        let fail = server
            .mock("GET", "/anime/season/2026/winter")
            .match_query(Matcher::Any)
            .with_status(429)
            .expect(2)
            .create_async()
            .await;
        let ok = server
            .mock("GET", "/anime/season/2026/winter")
            .match_query(Matcher::Any)
            .with_body(page(&[7], None))
            .expect(1)
            .create_async()
            .await;
        let summary = service(pool, &server).fetch_season(202601).await.unwrap();
        assert_eq!(summary.fetched, 1);
        fail.assert_async().await;
        ok.assert_async().await;
    }

    #[sqlx::test]
    async fn test_fetch_season_fails_after_max_retries(pool: SqlitePool) {
        let mut server = Server::new_async().await;
        let m = server
            .mock("GET", "/anime/season/2026/winter")
            .match_query(Matcher::Any)
            .with_status(503)
            .expect(3)
            .create_async()
            .await;
        assert!(service(pool, &server).fetch_season(202601).await.is_err());
        m.assert_async().await;
    }

    #[sqlx::test]
    async fn test_fetch_season_idempotent(pool: SqlitePool) {
        let mut server = Server::new_async().await;
        server
            .mock("GET", "/anime/season/2026/winter")
            .match_query(Matcher::Any)
            .with_body(page(&[1, 2], None))
            .expect(2)
            .create_async()
            .await;
        let svc = service(pool.clone(), &server);
        assert_eq!(
            svc.fetch_season(202601).await.unwrap(),
            FetchSummary { fetched: 2, new: 2 }
        );
        assert_eq!(
            svc.fetch_season(202601).await.unwrap(),
            FetchSummary { fetched: 2, new: 0 }
        );
        let rows = SeasonMalRepository::new(&pool)
            .list_by_season(202601, true)
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);
    }

    #[sqlx::test]
    async fn test_removed_row_not_resurrected_and_review_preserved(pool: SqlitePool) {
        let mut server = Server::new_async().await;
        server
            .mock("GET", "/anime/season/2026/winter")
            .match_query(Matcher::Any)
            .with_body(page(&[1, 2], None))
            .expect(2)
            .create_async()
            .await;
        let svc = service(pool.clone(), &server);
        svc.fetch_season(202601).await.unwrap();

        let sm = SeasonMalRepository::new(&pool);
        sm.set_removed(202601, 1, true, Some("moved".to_string()))
            .await
            .unwrap();
        crate::dal::SubjectRepository::new(&pool)
            .create(crate::dal::dto::CreateSubject {
                id: 4242,
                ..Default::default()
            })
            .await
            .unwrap();
        MalBgmMatchRepository::new(&pool)
            .upsert(
                2,
                UpdateMalBgmMatch {
                    status: "included".to_string(),
                    source: Some("human".to_string()),
                    bgm_id: Some(4242),
                    confidence: Some(1.0),
                    note: Some("checked".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let summary = svc.fetch_season(202601).await.unwrap();
        assert_eq!(summary, FetchSummary { fetched: 2, new: 0 });

        let row = sm.get(202601, 1).await.unwrap().unwrap();
        assert!(row.is_removed);
        assert_eq!(row.note.as_deref(), Some("moved"));

        let m = MalBgmMatchRepository::new(&pool)
            .get(2)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(m.status, "included");
        assert_eq!(m.source.as_deref(), Some("human"));
        assert_eq!(m.bgm_id, Some(4242));
        assert_eq!(m.confidence, Some(1.0));
        assert_eq!(m.note.as_deref(), Some("checked"));
    }

    #[sqlx::test]
    async fn test_fetch_season_skips_continuing_and_missing_start_season(pool: SqlitePool) {
        let mut server = Server::new_async().await;
        // 预先存在一部上一季（202510）已收录的跨季连播番 mal_id=10
        SeasonRepository::new(&pool)
            .upsert(CreateSeason {
                season_id: 202510,
                year: 2025,
                season: "FALL".into(),
                name: None,
            })
            .await
            .unwrap();
        MalAnimeRepository::new(&pool)
            .upsert(UpsertMalAnime {
                mal_id: 10,
                title: "Two-Cour Show".into(),
                media_type: "tv".into(),
                start_season_year: Some(2025),
                start_season: Some("fall".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        SeasonMalRepository::new(&pool)
            .insert_ignore(202510, 10)
            .await
            .unwrap();
        crate::dal::SubjectRepository::new(&pool)
            .create(crate::dal::dto::CreateSubject {
                id: 8888,
                ..Default::default()
            })
            .await
            .unwrap();
        MalBgmMatchRepository::new(&pool)
            .upsert(
                10,
                UpdateMalBgmMatch {
                    status: "included".into(),
                    source: Some("exact".into()),
                    bgm_id: Some(8888),
                    confidence: Some(1.0),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let mut continuing = node(10, Some("tv"));
        continuing["start_season"] = json!({"year": 2025, "season": "fall"});
        let mut long_running = node(21, Some("tv"));
        long_running["start_season"] = json!({"year": 1999, "season": "fall"});
        let mut no_start_season = node(30, Some("ona"));
        no_start_season
            .as_object_mut()
            .unwrap()
            .remove("start_season");
        let mut new_winter = node(40, Some("tv"));
        new_winter["start_season"] = json!({"year": 2026, "season": "WINTER"});

        let body = json!({
            "data": [
                {"node": continuing},
                {"node": long_running},
                {"node": no_start_season},
                {"node": new_winter},
            ],
            "paging": {}
        })
        .to_string();

        let mock = server
            .mock("GET", "/anime/season/2026/winter")
            .match_query(Matcher::Any)
            .with_body(body)
            .expect(1)
            .create_async()
            .await;

        let summary = service(pool.clone(), &server)
            .fetch_season(202601)
            .await
            .unwrap();
        mock.assert_async().await;

        assert_eq!(summary, FetchSummary { fetched: 1, new: 1 });
        let rows_202601 = SeasonMalRepository::new(&pool)
            .list_by_season(202601, true)
            .await
            .unwrap();
        assert_eq!(rows_202601.len(), 1);
        assert_eq!(rows_202601[0].mal_id, 40);

        // 21 与 30 不应写入 mal_anime 或 mal_bgm_matches
        assert!(
            MalAnimeRepository::new(&pool)
                .get(21)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            MalAnimeRepository::new(&pool)
                .get(30)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[sqlx::test]
    async fn test_fetch_range_iterates_seasons_with_interval(pool: SqlitePool) {
        let mut server = Server::new_async().await;
        let mut fall_node = node(101, Some("tv"));
        fall_node["start_season"] = json!({"year": 2025, "season": "fall"});
        let m1 = server
            .mock("GET", "/anime/season/2025/fall")
            .match_query(Matcher::Any)
            .with_body(json!({"data": [{"node": fall_node}], "paging": {}}).to_string())
            .expect(1)
            .create_async()
            .await;
        let m2 = server
            .mock("GET", "/anime/season/2026/winter")
            .match_query(Matcher::Any)
            .with_body(page(&[202], None))
            .expect(1)
            .create_async()
            .await;

        let summary = service(pool, &server)
            .fetch_range(202510, 202601)
            .await
            .unwrap();
        m1.assert_async().await;
        m2.assert_async().await;
        assert_eq!(summary, FetchSummary { fetched: 2, new: 2 });
    }
}
