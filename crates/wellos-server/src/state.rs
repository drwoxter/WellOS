use dmind_gateway::ModelGateway;
use sqlx::PgPool;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub gateway: Arc<dyn ModelGateway>,
    /// Whether routing patient data to external (off-cell) AI providers is
    /// permitted by deployment configuration. Development default: false.
    pub allow_external_ai: bool,
    /// Regional cell identifier for events and provenance.
    pub cell: String,
}

impl AppState {
    pub fn new(pool: PgPool, gateway: Arc<dyn ModelGateway>) -> Self {
        Self {
            pool,
            gateway,
            allow_external_ai: false,
            cell: "cell-dev-1".to_string(),
        }
    }
}
