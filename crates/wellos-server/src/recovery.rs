//! Waitlist and cancellation recovery.
use crate::auth::AuthContext;
use crate::error::ApiError;
use crate::state::AppState;
use uuid::Uuid;

pub async fn on_offer_closed(
    _tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    _ctx: &AuthContext,
    _state: &AppState,
    _offer_id: Uuid,
    _reason: &str,
) -> Result<usize, ApiError> {
    Ok(0)
}
