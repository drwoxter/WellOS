//! Transport and location coordination.
use crate::error::ApiError;
use crate::state::AppState;

pub async fn purge_expired_locations(_state: &AppState, _worker_id: &str) -> Result<u64, ApiError> {
    Ok(0)
}
