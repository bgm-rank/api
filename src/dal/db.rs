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

    #[tokio::test]
    async fn test_database_new() {
        dotenvy::dotenv().ok();
        let database_url = std::env::var("DATABASE_URL").unwrap();

        let db = Database::new(&database_url).await;
        assert!(db.is_ok());
    }

    #[tokio::test]
    async fn test_database_ping() {
        dotenvy::dotenv().ok();
        let database_url = std::env::var("DATABASE_URL").unwrap();
        let db = Database::new(&database_url).await.unwrap();

        let result = db.ping().await;
        assert!(result.is_ok());
    }
}
