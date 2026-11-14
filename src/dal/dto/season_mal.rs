use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
#[allow(dead_code)]
pub struct SeasonMal {
    pub season_id: i32,
    pub mal_id: i32,
    pub origin: String,
    pub is_removed: bool,
    pub note: Option<String>,
    pub created_at: NaiveDateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow, PartialEq, Eq)]
pub struct SeasonMalExportRow {
    pub season_id: i32,
    pub mal_id: i32,
    pub origin: String,
    pub is_removed: bool,
    pub note: Option<String>,
}
