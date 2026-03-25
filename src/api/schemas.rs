use serde::{Deserialize, Serialize};

use crate::core::scheduler::PublicTickStats;

#[derive(Serialize)]
#[allow(dead_code)]
pub struct MessageResponse {
    pub message: String,
}

#[derive(Serialize)]
pub struct ErrorResponse {
    pub error: String,
}

// T008: 公开查询 schemas
#[derive(Serialize)]
pub struct PublicSeasonResponse {
    pub season_id: i32,
    pub year: i32,
    pub season: String,
    pub name: Option<String>,
    pub updated_at: chrono::NaiveDateTime,
}

#[derive(Serialize)]
pub struct PublicSubjectItem {
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
    pub media_type: Option<String>,
    pub rating: Option<String>,
}

// T018: Admin schemas
#[derive(Deserialize)]
pub struct CreateSeasonRequest {
    pub year: i32,
    pub month: i32,
    pub name: Option<String>,
}

#[derive(Serialize)]
pub struct SyncResultResponse {
    pub season_id: i32,
    pub subjects_added: usize,
    pub subjects_removed: usize,
    pub subjects_updated: usize,
    pub subjects_failed: usize,
}

// T039: 孤立番剧 schemas
#[derive(Serialize)]
pub struct OrphanSubjectItem {
    pub id: i32,
    pub name: Option<String>,
    pub name_cn: Option<String>,
}

#[derive(Serialize)]
pub struct DeleteOrphansResponse {
    pub deleted: u64,
}

#[derive(Serialize)]
pub struct DeleteSeasonResponse {
    pub season_id: i32,
    pub deleted: bool,
}

// T013: New schemas for admin API

#[derive(Serialize)]
pub struct AcceptedResponse {
    pub status: String,
    pub message: String,
}

#[derive(Serialize)]
pub struct SchedulerStatusResponse {
    pub is_running: bool,
    pub last_run_at: Option<chrono::DateTime<chrono::Utc>>,
    pub last_stats: Option<PublicTickStats>,
}

#[derive(Deserialize)]
pub struct EditSubjectRequest {
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

#[derive(Deserialize)]
pub struct EditSeasonRequest {
    pub name: Option<String>,
}

#[derive(Serialize)]
pub struct DeletedResponse {
    pub deleted: bool,
}

#[derive(Serialize)]
pub struct RemovedResponse {
    pub removed: bool,
}
