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
        "SELECT a.tenant_id, a.patient_id, a.status, a.artifact_type, a.encounter_id,
                p.facility_id
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
    let encounter_id: Option<Uuid> = row.get("encounter_id");
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
    // Accepting an encounter summary writes its text into the draft note, so
    // approval must go through the encounter transaction that does both.
    if decision == ReviewDecision::Approved
        && row.get::<String, _>("artifact_type") == "encounter_summary"
    {
        return Err(ApiError::conflict(
            "use_encounter_accept",
            "encounter summaries are accepted via /encounters/:id/ai-draft/accept",
        ));
    }

    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    if let Some(encounter_id) = encounter_id {
        // Encounter-bound artifacts are reviewable only while the encounter
        // is open and the note is still the version they were generated
        // from; the lock serializes this with documentation writes.
        let open: Option<(String,)> = sqlx::query_as(
            "SELECT status FROM encounters WHERE id = $1 AND tenant_id = $2 FOR UPDATE",
        )
        .bind(encounter_id)
        .bind(tenant_id)
        .fetch_optional(&mut *tx)
        .await?;
        if open.is_none_or(|(s,)| s != "in_progress") {
            return Err(ApiError::conflict(
                "encounter_not_active",
                "this encounter is no longer in progress",
            ));
        }
        let bound: Option<i64> =
            sqlx::query_scalar("SELECT note_version FROM ai_artifacts WHERE id = $1")
                .bind(id)
                .fetch_one(&mut *tx)
                .await?;
        let current: Option<i64> = sqlx::query_scalar(
            "SELECT version FROM encounter_notes WHERE tenant_id = $1 AND encounter_id = $2",
        )
        .bind(tenant_id)
        .bind(encounter_id)
        .fetch_optional(&mut *tx)
        .await?;
        if bound != current {
            return Err(ApiError::conflict(
                "artifact_stale",
                "the draft was generated from an older note version; request a new draft",
            ));
        }
    }
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
