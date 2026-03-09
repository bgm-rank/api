use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

#[derive(Debug, Serialize, Deserialize, FromRow)]
#[allow(dead_code)]
pub struct SeasonSubject {
    pub season_id: i32,
    pub subject_id: i32,
    pub added_at: NaiveDateTime,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct CreateSeasonSubject {
    pub season_id: i32,
    pub subject_id: i32,
}
