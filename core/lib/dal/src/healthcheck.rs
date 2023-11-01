use serde::Serialize;
use sqlx::PgPool;

use zksync_health_check::{async_trait, CheckHealth, Health, HealthStatus};

use crate::MainConnectionPool;

#[derive(Debug, Serialize)]
struct MainConnectionPoolHealthDetails {
    pool_size: u32,
}

impl MainConnectionPoolHealthDetails {
    async fn new(pool: &PgPool) -> Self {
        Self {
            pool_size: pool.size(),
        }
    }
}

// HealthCheck used to verify if we can connect to the main database.
// This guarantees that the app can use it's main "communication" channel.
// Used in the /health endpoint
#[derive(Clone, Debug)]
pub struct MainConnectionPoolHealthCheck {
    connection_pool: MainConnectionPool,
}

impl MainConnectionPoolHealthCheck {
    pub fn new(connection_pool: MainConnectionPool) -> MainConnectionPoolHealthCheck {
        Self { connection_pool }
    }
}

#[async_trait]
impl CheckHealth for MainConnectionPoolHealthCheck {
    fn name(&self) -> &'static str {
        "main_connection_pool"
    }

    async fn check_health(&self) -> Health {
        // This check is rather feeble, plan to make reliable here:
        // https://linear.app/matterlabs/issue/PLA-255/revamp-db-connection-health-check
        self.connection_pool.access_storage().await.unwrap();
        let details = MainConnectionPoolHealthDetails::new(&self.connection_pool.0).await;
        Health::from(HealthStatus::Ready).with_details(details)
    }
}
