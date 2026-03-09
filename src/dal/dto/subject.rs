use chrono::{DateTime, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

#[derive(Debug, Serialize, Deserialize, FromRow)]
pub struct Subject {
    pub id: i32,
    pub name: Option<String>,
    pub name_cn: Option<String>,
    pub images_grid: Option<String>,
    pub images_large: Option<String>,
    pub rank: Option<i32>,
    pub score: Option<f64>,
    pub collection_total: Option<i32>,
    pub average_comment: Option<f64>,
    pub drop_rate: Option<f64>,
    pub air_weekday: Option<String>,
    pub meta_tags: Vec<String>,
    #[sqlx(default)]
    pub media_type: Option<String>,
    #[sqlx(default)]
    pub rating: Option<String>,
    pub updated_at: NaiveDateTime,
    #[sqlx(default)]
    pub last_updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
pub struct CreateSubject {
    pub id: i32,
    pub name: Option<String>,
    pub name_cn: Option<String>,
    pub images_grid: Option<String>,
    pub images_large: Option<String>,
    pub rank: Option<i32>,
    pub score: Option<f64>,
    pub collection_total: Option<i32>,
    pub average_comment: Option<f64>,
    pub drop_rate: Option<f64>,
    pub air_weekday: Option<String>,
    pub meta_tags: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSubject {
    pub name: Option<String>,
    pub name_cn: Option<String>,
    pub images_grid: Option<String>,
    pub images_large: Option<String>,
    pub rank: Option<i32>,
    pub score: Option<f64>,
    pub collection_total: Option<i32>,
    pub average_comment: Option<f64>,
    pub drop_rate: Option<f64>,
    pub air_weekday: Option<String>,
    pub meta_tags: Option<Vec<String>>,
}
