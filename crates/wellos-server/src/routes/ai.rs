use crate::audit;
use crate::auth::AuthContext;
use crate::error::ApiError;
use crate::policy::{actions, ResourceCtx};
use crate::routes::guard;
use crate::state::AppState;
use axum::extract::{Path, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;
use wellos_domain::ai::{ArtifactStatus, ReviewDecision};

#[derive(Deserialize)]
pub struct ReviewBody {
    /// "approved" or "rejected"
    pub decision: String,
    pub note: Option<String>,
}

pub async fn review_artifact(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<ReviewBody>,
) -> Result<Json<Value>, ApiError> {
    let decision = match body.decision.as_str() {
        "approved" => ReviewDecision::Approved,
        "rejected" => ReviewDecision::Rejected,
        _ => {
            return Err(ApiError::bad_request(
                "validation_failed",
                "decision must be 'approved' or 'rejected'",
            ))
        }
    };
    let row = sqlx::query(
        "SELECT a.tenant_id, a.patient_id, a.status, p.facility_id
         FROM ai_artifacts a JOIN patients p ON p.id = a.patient_id
         WHERE a.id = $1",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(ApiError::not_found)?;
    let tenant_id: Uuid = row.get("tenant_id");
    let patient_id: Uuid = row.get("patient_id");
    let facility_id: Uuid = row.get("facility_id");
    let status = ArtifactStatus::parse(row.get::<String, _>("status").as_str())
        .ok_or_else(|| ApiError::internal("invalid artifact status"))?;

    if body.note.as_deref().is_some_and(|n| n.len() > 4000) {
        return Err(ApiError::bad_request(
            "validation_failed",
            "note exceeds 4000 characters",
        ));
    }
    let allowed = guard(
        &state,
        &ctx,
        actions::AI_REVIEW,
        "ai_artifact",
        Some(ResourceCtx {
            tenant_id,
            patient_id: Some(patient_id),
            facility_id: Some(facility_id),
        }),
    )
    .await?;

    let next = status
        .review(decision)
        .map_err(|e| ApiError::conflict("invalid_artifact_state", e.to_string()))?;

    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    // Approval is a new provenance event; the AI origin remains recorded.
    // The status predicate makes the review an atomic conditional transition:
    // a concurrent review or supersession loses instead of being overwritten.
    let updated = sqlx::query(
        "UPDATE ai_artifacts SET status=$1, reviewer_id=$2, review_decision=$3,
         review_note=$4, reviewed_at=now() WHERE id=$5 AND status=$6",
    )
    .bind(next.as_str())
    .bind(ctx.user_id)
    .bind(&body.decision)
    .bind(&body.note)
    .bind(id)
    .bind(status.as_str())
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(ApiError::conflict(
            "review_conflict",
            "artifact was reviewed or superseded concurrently",
        ));
    }
    audit::emit(
        &mut *tx,
        &ctx,
        "ai.artifact.reviewed",
        &state.cell,
        json!({ "artifact_id": id, "decision": body.decision }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    tx.commit().await?;
    Ok(Json(json!({ "id": id, "status": next.as_str() })))
}
