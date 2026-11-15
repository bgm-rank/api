use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
#[allow(dead_code)]
pub struct MalBgmMatch {
    pub mal_id: i32,
    pub status: String,
    pub source: Option<String>,
    pub bgm_id: Option<i32>,
    pub confidence: Option<f64>,
    pub candidates: Option<String>,
    pub reason: Option<String>,
    pub note: Option<String>,
    pub error: Option<String>,
    pub updated_at: NaiveDateTime,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[allow(dead_code)]
pub struct UpdateMalBgmMatch {
    pub status: String,
    pub source: Option<String>,
    pub bgm_id: Option<i32>,
    pub confidence: Option<f64>,
    pub candidates: Option<String>,
    pub reason: Option<String>,
    pub note: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow, PartialEq)]
pub struct MalBgmMatchExportRow {
    pub mal_id: i32,
    pub status: String,
    pub source: Option<String>,
    pub bgm_id: Option<i32>,
    pub confidence: Option<f64>,
    pub reason: Option<String>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct MatchSubjectSummary {
    pub id: i32,
    pub name: Option<String>,
    pub name_cn: Option<String>,
    pub rank: Option<i32>,
    pub score: Option<f64>,
    pub air_date: Option<String>,
    pub platform: Option<String>,
    pub nsfw: Option<bool>,
    pub tags: Option<String>,
    pub summary: Option<String>,
    pub images_grid: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LinkedMalItem {
    pub mal_id: i32,
    pub bgm_id: i32,
    pub title: String,
    pub title_ja: Option<String>,
    pub media_type: String,
    pub status: String,
    pub seasons: Vec<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MalBgmMatchWithDetails {
    #[serde(flatten)]
    pub match_row: MalBgmMatch,
    pub title: String,
    pub title_ja: Option<String>,
    pub title_en: Option<String>,
    pub media_type: String,
    pub rating: Option<String>,
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    pub num_episodes: Option<i32>,
    pub mal_source: Option<String>,
    pub studios: Option<String>,
    pub synopsis: Option<String>,
    pub subject: Option<MatchSubjectSummary>,
}
