use anyhow::{Context, Result, anyhow};
use std::sync::Arc;

use crate::dal::{CreateSeason, CreateSeasonSubject, CreateSubject, Database};
use crate::dal::{SeasonRepository, SeasonSubjectRepository, SubjectRepository};
use crate::services::bangumi::{BangumiClient, Subject as BangumiSubject};
use crate::services::season_data::SeasonDataClient;

pub struct SyncService {
    season_data_client: SeasonDataClient,
    bangumi_client: BangumiClient,
    db: Arc<Database>,
}

impl SyncService {
    pub fn new(db: Arc<Database>) -> Self {
        Self {
            season_data_client: SeasonDataClient::new(),
            bangumi_client: BangumiClient::new(),
            db,
        }
    }

    pub async fn sync_season(&self, key: &str) -> Result<()> {
        let parsed = parse_season_key(key)?;
        let pool = self.db.pool();

        // 1. 获取该季度的番剧列表
        let season_data = self.season_data_client.fetch_all().await?;
        let entries = season_data
            .get(key)
            .ok_or_else(|| anyhow!("Season '{}' not found in season-data", key))?;

        // 2. upsert Season 记录
        let season_id = parsed.season_id;
        SeasonRepository::new(pool)
            .upsert(CreateSeason {
                season_id,
                year: parsed.year,
                season: parsed.season,
                name: None,
            })
            .await
            .context("upsert season 失败")?;

        // 3. 对每个番剧，从 Bangumi API 获取详情并写入 DB
        let subject_repo = SubjectRepository::new(pool);
        let season_subject_repo = SeasonSubjectRepository::new(pool);

        for entry in entries {
            match self.bangumi_client.get_subject(entry.bgm_id).await {
                Ok(bangumi_subject) => {
                    if let Err(e) = subject_repo.upsert(to_create_subject(bangumi_subject)).await {
                        todo!("log: upsert subject {} 失败: {:#}", entry.bgm_id, e);
                    }

                    if let Err(e) = season_subject_repo
                        .insert_or_ignore(CreateSeasonSubject {
                            season_id,
                            subject_id: entry.bgm_id,
                        })
                        .await
                    {
                        todo!("log: 关联 subject {} 到季度失败: {:#}", entry.bgm_id, e);
                    }
                }
                Err(e) => {
                    todo!("log: 拉取 subject {} 失败: {:#}", entry.bgm_id, e);
                }
            }
        }

        Ok(())
    }
}

fn to_create_subject(s: BangumiSubject) -> CreateSubject {
    CreateSubject {
        id: s.id,
        name: s.name,
        name_cn: s.name_cn,
        images_grid: s.images.as_ref().and_then(|i| i.grid.clone()),
        images_large: s.images.and_then(|i| i.large),
        rank: s.rating.as_ref().and_then(|r| r.rank),
        score: s.rating.and_then(|r| r.score),
        collection_total: s.collection.map(|c| c.collect),
        average_comment: None,
        drop_rate: None,
        air_weekday: None,
        meta_tags: s.meta_tags.unwrap_or_default(),
    }
}

struct ParsedSeasonKey {
    pub year: i32,
    pub season: String,
    pub season_id: i32,
}

fn parse_season_key(key: &str) -> Result<ParsedSeasonKey> {
    let (year_str, season_str) = key
        .split_once('-')
        .ok_or_else(|| anyhow!("Invalid format: expected 'year-season', got '{}'", key))?;

    let year: i32 = year_str
        .parse()
        .with_context(|| format!("Failed to parse year from '{}'", year_str))?;

    let (season_upper, month) = match season_str.to_lowercase().as_str() {
        "winter" => ("WINTER", 1),
        "spring" => ("SPRING", 4),
        "summer" => ("SUMMER", 7),
        "autumn" => ("AUTUMN", 10),
        _ => return Err(anyhow!("Unknown season: '{}'", season_str)),
    };

    Ok(ParsedSeasonKey {
        year,
        season: season_upper.to_string(),
        season_id: year * 100 + month,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_season_key_winter() {
        let parsed = parse_season_key("2026-winter").unwrap();
        assert_eq!(parsed.year, 2026);
        assert_eq!(parsed.season, "WINTER");
        assert_eq!(parsed.season_id, 202601);
    }

    #[test]
    fn test_parse_season_key_spring() {
        let parsed = parse_season_key("2025-spring").unwrap();
        assert_eq!(parsed.season_id, 202504);
    }

    #[test]
    fn test_parse_season_key_summer() {
        let parsed = parse_season_key("2025-summer").unwrap();
        assert_eq!(parsed.season_id, 202507);
    }

    #[test]
    fn test_parse_season_key_autumn() {
        let parsed = parse_season_key("2025-autumn").unwrap();
        assert_eq!(parsed.season_id, 202510);
    }

    #[test]
    fn test_parse_season_key_invalid() {
        assert!(parse_season_key("invalid").is_err());
        assert!(parse_season_key("2026-badseason").is_err());
    }
}
