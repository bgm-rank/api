use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool};
use std::str::FromStr;

pub struct Database {
    pool: SqlitePool,
}

impl Database {
    pub async fn new(database_url: &str) -> Result<Self, sqlx::Error> {
        let opts = SqliteConnectOptions::from_str(database_url)?
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Wal)
            .create_if_missing(true);
        let pool = SqlitePool::connect_with(opts).await?;
        if let Err(e) = sqlx::migrate!().run(&pool).await {
            // ignore the race where two connections both try to insert the same
            // migration record; the constraint failure means it was already applied
            let msg = e.to_string();
            if !msg.contains("UNIQUE constraint failed: _sqlx_migrations") {
                return Err(sqlx::Error::Protocol(msg));
            }
        }
        Ok(Self { pool })
    }

    #[allow(dead_code)]
    pub fn from_pool(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub async fn ping(&self) -> Result<bool, sqlx::Error> {
        match sqlx::query("SELECT 1").execute(&self.pool).await {
            Ok(_) => Ok(true),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::SqlitePool;

    #[sqlx::test]
    async fn test_database_from_pool(pool: SqlitePool) {
        let db = Database::from_pool(pool);
        assert!(db.ping().await.is_ok());
    }

    #[sqlx::test]
    async fn test_database_ping(pool: SqlitePool) {
        let db = Database::from_pool(pool);
        let result = db.ping().await;
        assert!(result.is_ok());
    }
}
