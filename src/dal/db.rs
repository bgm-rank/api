use sqlx::PgPool;

pub struct Database {
    pool: PgPool,
}

impl Database {
    pub async fn new(database_url: &str) -> Result<Self, sqlx::Error> {
        Ok(Self {
            pool: PgPool::connect(database_url).await.unwrap(),
        })
    }

    #[allow(dead_code)]
    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
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
    use sqlx::PgPool;

    #[sqlx::test]
    async fn test_database_from_pool(pool: PgPool) {
        let db = Database::from_pool(pool);
        assert!(db.ping().await.is_ok());
    }

    #[sqlx::test]
    async fn test_database_ping(pool: PgPool) {
        let db = Database::from_pool(pool);
        let result = db.ping().await;
        assert!(result.is_ok());
    }
}
