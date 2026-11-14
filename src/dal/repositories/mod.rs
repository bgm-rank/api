pub mod mal_anime_repository;
pub mod mal_bgm_match_repository;
pub mod season_mal_repository;
pub mod season_repository;
pub mod subject_repository;

#[allow(unused_imports)]
pub use mal_anime_repository::*;
pub use mal_bgm_match_repository::*;
pub use season_mal_repository::*;
pub use season_repository::*;
pub use subject_repository::*;

pub(super) fn log_db_error(operation: &'static str, table: &'static str, e: &sqlx::Error) {
    match e {
        sqlx::Error::RowNotFound => {
            tracing::debug!(operation, table, error = %e, "db error");
        }
        sqlx::Error::Database(db_err) if db_err.is_unique_violation() => {
            tracing::warn!(operation, table, error = %e, "db error");
        }
        _ => {
            tracing::error!(operation, table, error = %e, "db error");
        }
    }
}
