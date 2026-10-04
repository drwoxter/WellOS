//! Patient self-service diagnostics (`/api/v1/me/diagnostics`).
//!
//! Only reports with a current clinician release decision are visible, with
//! the clinician-approved explanation and the released clean documents.
//! Pending orders appear as scheduling facts (what, where, when) without
//! results; a report superseded by an unreleased newer version is shown as
//! "under professional review" without content. Representative access goes
//! through the same patient grants as every other self-service surface.

use super::documents::{self, load_document};
use super::reports::{self, load_report};
use crate::auth::AuthContext;
use crate::error::ApiError;
use crate::policy::{actions, ResourceCtx};
use crate::routes::grants::GrantRow;
use crate::routes::{grants, guard, Allowed};
use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::Json;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::{PgConnection, Row};
use uuid::Uuid;

const DEFAULT_LIMIT: i64 = 50;
const MAX_LIMIT: i64 = 200;

async fn scope(
    state: &AppState,
    ctx: &AuthContext,
    conn: &mut PgConnection,
    resource_type: &str,
    requested: Option<Uuid>,
) -> Result<(GrantRow, Allowed), ApiError> {
    guard(
        state,
        ctx,
        actions::PATIENT_SELF_SERVICE,
        resource_type,
        None,
    )
    .await?;
    let g = grants::resolve_grant(conn, ctx, requested).await?;
    let allowed = guard(
        state,
        ctx,
        actions::PATIENT_SELF_SERVICE,
        resource_type,
        Some(ResourceCtx {
            tenant_id: g.tenant_id,
            patient_id: Some(g.patient_id),
            facility_id: Some(g.patient_facility_id),
        }),
    )
    .await?;
    Ok((g, allowed))
}

#[derive(Debug, Default, Deserialize)]
pub struct ListQuery {
    pub patient_id: Option<Uuid>,
    pub limit: Option<i64>,
}

/// Patient-facing report view: the released version, its components, the
/// approved explanation and released documents. No safety rules, internal
/// identifiers of reviewers or dMind drafts.
async fn released_report_json(
    conn: &mut PgConnection,
    report: &reports::ReportRow,
    release: &sqlx::postgres::PgRow,
    with_components: bool,
) -> Result<Value, ApiError> {
    let order = sqlx::query(
        "SELECT display, category_code, modality_code, priority, requester_id FROM service_requests WHERE id = $1",
    )
    .bind(report.service_request_id)
    .fetch_one(&mut *conn)
    .await?;
    let mut v = json!({
        "id": report.id,
        "service_request_id": report.service_request_id,
        "patient_id": report.patient_id,
        "order_display": order.get::<String, _>("display"),
        "category_code": order.get::<Option<String>, _>("category_code"),
        "modality_code": order.get::<Option<String>, _>("modality_code"),
        "status": report.status.as_str(),
        "version": report.version,
        "criticality": report.criticality.as_str(),
        "conclusion": report.conclusion,
        "issued_at": report.issued_at,
        "effective_at": report.effective_at,
        "released_at": release.get::<DateTime<Utc>, _>("decided_at"),
        "explanation_en": release.get::<Option<String>, _>("explanation_en"),
        "explanation_es": release.get::<Option<String>, _>("explanation_es"),
        "notified": release.get::<bool, _>("notify_patient"),
    });
    if with_components {
        v["components"] = Value::Array(
            reports::components_of(conn, report.id)
                .await?
                .iter()
                .map(|c| {
                    json!({
                        "id": c.id,
                        "code": c.code,
                        "display": c.display,
                        "value": c.value,
                        "value_text": reports::value_text(&c.value),
                        "reference_range": c.reference_range,
                        "interpretation": c.interpretation.as_str(),
                        "effective_at": c.effective_at,
                    })
                })
                .collect(),
        );
        v["documents"] =
            Value::Array(documents::list_for_report_json(conn, report.id, true).await?);
    }
    Ok(v)
}

const RELEASE_SELECT: &str =
    "SELECT d.id, d.diagnostic_report_id, d.decided_at, d.explanation_en, d.explanation_es,
    d.notify_patient FROM result_release_decisions d";

/// `GET /api/v1/me/diagnostics`
pub async fn list(
    State(state): State<AppState>,
    ctx: AuthContext,
    Query(q): Query<ListQuery>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let (g, allowed) = scope(&state, &ctx, &mut conn, "diagnostic_result", q.patient_id).await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    let limit = q.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let releases = sqlx::query(&format!(
        "{RELEASE_SELECT} WHERE d.tenant_id = $1 AND d.patient_id = $2 AND d.decision = 'release'
           AND d.superseded_at IS NULL ORDER BY d.decided_at DESC, d.id DESC LIMIT $3"
    ))
    .bind(g.tenant_id)
    .bind(g.patient_id)
    .bind(limit)
    .fetch_all(&mut *conn)
    .await?;
    let mut released = Vec::with_capacity(releases.len());
    for rel in &releases {
        let r = load_report(
            &mut conn,
            g.tenant_id,
            rel.get("diagnostic_report_id"),
            false,
        )
        .await?;
        released.push(released_report_json(&mut conn, &r, rel, false).await?);
    }
    // Results the patient has seen before whose updated version is still with
    // the professionals: visible as a status, never as content.
    let under_review = sqlx::query(
        "SELECT DISTINCT sr.id AS service_request_id, sr.display
         FROM result_release_decisions d
         JOIN service_requests sr ON sr.id = d.service_request_id
         WHERE d.tenant_id = $1 AND d.patient_id = $2 AND d.decision = 'release' AND d.superseded_at IS NOT NULL
           AND NOT EXISTS (SELECT 1 FROM result_release_decisions c
                           WHERE c.service_request_id = sr.id AND c.superseded_at IS NULL)
         ORDER BY sr.display",
    )
    .bind(g.tenant_id)
    .bind(g.patient_id)
    .fetch_all(&mut *conn)
    .await?;
    let pending = sqlx::query(
        "SELECT sr.id, sr.display, sr.order_status, sr.category_code, sr.modality_code, sr.fulfilment_mode,
                sr.appointment_id, a.starts_at, a.ends_at, a.time_zone, a.facility_id, f.name AS facility_name,
                sr.preparation_en, sr.preparation_es
         FROM service_requests sr
         LEFT JOIN appointments a ON a.id = sr.appointment_id
         LEFT JOIN facilities f ON f.id = a.facility_id
         WHERE sr.tenant_id = $1 AND sr.patient_id = $2
           AND sr.order_status IN ('placed','accepted','scheduled','in_progress','on_hold')
         ORDER BY a.starts_at NULLS LAST, sr.created_at DESC LIMIT $3",
    )
    .bind(g.tenant_id)
    .bind(g.patient_id)
    .bind(limit)
    .fetch_all(&mut *conn)
    .await?;
    Ok(Json(json!({
        "patient_id": g.patient_id,
        "relationship": g.relationship,
        "released": released,
        "under_review": under_review.iter().map(|r| json!({
            "service_request_id": r.get::<Uuid, _>("service_request_id"),
            "order_display": r.get::<String, _>("display"),
        })).collect::<Vec<_>>(),
        "pending": pending.iter().map(|r| json!({
            "service_request_id": r.get::<Uuid, _>("id"),
            "order_display": r.get::<String, _>("display"),
            "status": r.get::<String, _>("order_status"),
            "category_code": r.get::<Option<String>, _>("category_code"),
            "modality_code": r.get::<Option<String>, _>("modality_code"),
            "fulfilment_mode": r.get::<String, _>("fulfilment_mode"),
            "appointment_id": r.get::<Option<Uuid>, _>("appointment_id"),
            "starts_at": r.get::<Option<DateTime<Utc>>, _>("starts_at"),
            "ends_at": r.get::<Option<DateTime<Utc>>, _>("ends_at"),
            "time_zone": r.get::<Option<String>, _>("time_zone"),
            "facility_id": r.get::<Option<Uuid>, _>("facility_id"),
            "facility_name": r.get::<Option<String>, _>("facility_name"),
            "preparation_en": r.get::<Option<String>, _>("preparation_en"),
            "preparation_es": r.get::<Option<String>, _>("preparation_es"),
        })).collect::<Vec<_>>(),
    })))
}

async fn current_release(
    conn: &mut PgConnection,
    g: &GrantRow,
    report_id: Uuid,
) -> Result<sqlx::postgres::PgRow, ApiError> {
    sqlx::query(&format!(
        "{RELEASE_SELECT} WHERE d.tenant_id = $1 AND d.patient_id = $2 AND d.diagnostic_report_id = $3
           AND d.decision = 'release' AND d.superseded_at IS NULL"
    ))
    .bind(g.tenant_id)
    .bind(g.patient_id)
    .bind(report_id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(ApiError::not_found)
}

/// `GET /api/v1/me/diagnostics/:report_id`
pub async fn detail(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(report_id): Path<Uuid>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let (g, allowed) = scope(&state, &ctx, &mut conn, "diagnostic_result", q.patient_id).await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    let rel = current_release(&mut conn, &g, report_id).await?;
    let r = load_report(&mut conn, g.tenant_id, report_id, false).await?;
    let mut v = released_report_json(&mut conn, &r, &rel, true).await?;
    v["relationship"] = json!(g.relationship);
    Ok(Json(v))
}

/// `GET /api/v1/me/diagnostics/:report_id/documents/:document_id/download`
pub async fn download(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path((report_id, document_id)): Path<(Uuid, Uuid)>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let (g, allowed) = scope(&state, &ctx, &mut conn, "clinical_document", q.patient_id).await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    current_release(&mut conn, &g, report_id).await?;
    let d = load_document(&mut conn, g.tenant_id, document_id, false).await?;
    if d.patient_id != g.patient_id || d.diagnostic_report_id != Some(report_id) {
        return Err(ApiError::not_found());
    }
    if !d.released || d.status != "clean" {
        return Err(ApiError::not_found());
    }
    let mut v = documents::presign_download_in(&mut conn, &ctx, &state, &d, "patient").await?;
    v["document"] = documents::document_patient_json(&d);
    Ok(Json(v))
}
