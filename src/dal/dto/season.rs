use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

#[derive(Debug, Serialize, Deserialize, FromRow)]
pub struct Season {
    pub season_id: i32,
    pub year: i32,
    pub season: String,
    pub name: Option<String>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

#[derive(Debug, Deserialize)]
pub struct CreateSeason {
    pub season_id: i32,
    pub year: i32,
    pub season: String,
    pub name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSeason {
    pub year: Option<i32>,
    pub season: Option<String>,
    pub name: Option<String>,
}
