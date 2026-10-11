//! Persistence into the `dmind_eval_*` tables only.

use crate::error::ApiError;
use sqlx::PgPool;
use uuid::Uuid;

/// The synthetic tenant an evaluation run belongs to. Refuses anything that
/// is not `data_class = 'synthetic'` before any row is written.
pub async fn require_synthetic_tenant(pool: &PgPool, tenant_id: Uuid) -> Result<(), ApiError> {
    let row: Option<(String,)> = sqlx::query_as("SELECT data_class FROM tenants WHERE id = $1")
        .bind(tenant_id)
        .fetch_optional(pool)
        .await?;
    match row {
        Some((class,)) if class == "synthetic" => Ok(()),
        Some((class,)) => Err(ApiError::forbidden(format!(
            "evaluation refuses tenant with data_class {class}"
        ))),
        None => Err(ApiError::not_found()),
    }
}
