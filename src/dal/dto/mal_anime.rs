use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
#[allow(dead_code)]
pub struct MalAnime {
    pub mal_id: i32,
    pub title: String,
    pub title_ja: Option<String>,
    pub title_en: Option<String>,
    pub media_type: String,
    pub rating: Option<String>,
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    pub num_episodes: Option<i32>,
    pub source: Option<String>,
    pub studios: Option<String>,
    pub synopsis: Option<String>,
    pub start_season_year: Option<i32>,
    pub start_season: Option<String>,
    pub fetched_at: NaiveDateTime,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[allow(dead_code)]
pub struct UpsertMalAnime {
    pub mal_id: i32,
    pub title: String,
    pub title_ja: Option<String>,
    pub title_en: Option<String>,
    pub media_type: String,
    pub rating: Option<String>,
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    pub num_episodes: Option<i32>,
    pub source: Option<String>,
    pub studios: Option<String>,
    pub synopsis: Option<String>,
    pub start_season_year: Option<i32>,
    pub start_season: Option<String>,
}
