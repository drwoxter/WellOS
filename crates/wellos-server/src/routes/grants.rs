//! Patient-access grants: the only bridge between an authenticated
//! identity and the patient(s) it may act for through `/api/v1/me/...`.
//!
//! A grant is explicit (created by verified staff with a mandatory
//! verification note), revocable, optionally expiring and audited. The
//! self-service surface derives its patient from an *active* grant of the
//! caller; nothing in a request body can widen that set. Name, birth date
//! or medical-record number never establish identity here.

use crate::audit;
use crate::auth::AuthContext;
use crate::error::ApiError;
use crate::policy::{actions, roles, ResourceCtx};
use crate::routes::guard;
use crate::scheduling;
use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::{PgConnection, Row};
use uuid::Uuid;

pub const RELATIONSHIPS: &[&str] = &["self", "parent_guardian", "authorized_proxy"];
const MAX_NOTE: usize = 500;
const MIN_NOTE: usize = 3;

#[derive(Debug, Clone)]
pub struct GrantRow {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub user_id: Uuid,
    pub patient_id: Uuid,
    pub relationship: String,
    pub status: String,
    pub verified_by: Uuid,
    pub verification_note: Option<String>,
    pub granted_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub revoked_by: Option<Uuid>,
    pub revoke_reason: Option<String>,
    pub version: i64,
    pub patient_given_name: String,
    pub patient_family_name: String,
    pub patient_facility_id: Uuid,
}

const GRANT_SELECT: &str =
    "SELECT g.id, g.tenant_id, g.user_id, g.patient_id, g.relationship, g.status,
        g.verified_by, g.verification_note, g.granted_at, g.expires_at, g.revoked_at, g.revoked_by,
        g.revoke_reason, g.version, p.given_name, p.family_name, p.facility_id
     FROM patient_access_grants g JOIN patients p ON p.id = g.patient_id";

fn grant_from_row(r: &sqlx::postgres::PgRow) -> GrantRow {
    GrantRow {
        id: r.get("id"),
        tenant_id: r.get("tenant_id"),
        user_id: r.get("user_id"),
        patient_id: r.get("patient_id"),
        relationship: r.get("relationship"),
        status: r.get("status"),
        verified_by: r.get("verified_by"),
        verification_note: r.get("verification_note"),
        granted_at: r.get("granted_at"),
        expires_at: r.get("expires_at"),
        revoked_at: r.get("revoked_at"),
        revoked_by: r.get("revoked_by"),
        revoke_reason: r.get("revoke_reason"),
        version: r.get("version"),
        patient_given_name: r.get("given_name"),
        patient_family_name: r.get("family_name"),
        patient_facility_id: r.get("facility_id"),
    }
}

/// Staff view of a grant (includes verification provenance).
pub fn grant_json(g: &GrantRow) -> Value {
    json!({
        "id": g.id,
        "user_id": g.user_id,
        "patient_id": g.patient_id,
        "relationship": g.relationship,
        "status": effective_status(g),
        "verified_by": g.verified_by,
        "verification_note": g.verification_note,
        "granted_at": g.granted_at,
        "expires_at": g.expires_at,
        "revoked_at": g.revoked_at,
        "revoked_by": g.revoked_by,
        "revoke_reason": g.revoke_reason,
        "version": g.version,
    })
}

/// Self-service view: the accessible patient with the minimum identity
/// needed to pick a dependant, no verification internals.
pub fn grant_self_json(g: &GrantRow) -> Value {
    json!({
        "grant_id": g.id,
        "patient_id": g.patient_id,
        "relationship": g.relationship,
        "patient": { "given_name": g.patient_given_name, "family_name": g.patient_family_name },
        "expires_at": g.expires_at,
    })
}

/// A stored `active` grant past its expiry is already unusable; report it
/// as expired even before the sweep rewrites the row.
fn effective_status(g: &GrantRow) -> &str {
    if g.status == "active" && g.expires_at.is_some_and(|e| e <= Utc::now()) {
        "expired"
    } else {
        &g.status
    }
}

/// All grants the caller may act through right now: active, unexpired and
/// in the caller's tenant. Ordered for a stable dependant picker.
pub async fn active_grants(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    user_id: Uuid,
) -> Result<Vec<GrantRow>, ApiError> {
    let rows = sqlx::query(&format!(
        "{GRANT_SELECT}
         WHERE g.tenant_id = $1 AND g.user_id = $2 AND g.status = 'active'
           AND (g.expires_at IS NULL OR g.expires_at > now())
         ORDER BY (g.relationship <> 'self'), p.family_name, p.given_name, g.granted_at"
    ))
    .bind(tenant_id)
    .bind(user_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows.iter().map(grant_from_row).collect())
}

/// Resolve the patient a self-service call acts for. With one active grant
/// the patient is implied; with several the caller must name one of them.
/// A named patient outside the caller's grants is indistinguishable from a
/// non-existent one (`404`), so patient identifiers cannot be probed.
pub async fn resolve_grant(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    requested: Option<Uuid>,
) -> Result<GrantRow, ApiError> {
    let grants = active_grants(conn, ctx.tenant_id, ctx.user_id).await?;
    match requested {
        Some(id) => grants
            .into_iter()
            .find(|g| g.patient_id == id)
            .ok_or_else(ApiError::not_found),
        None => {
            let mut it = grants.into_iter();
            match (it.next(), it.next()) {
                (Some(g), None) => Ok(g),
                (None, _) => Err(ApiError::new(
                    StatusCode::FORBIDDEN,
                    "no_patient_grant",
                    "no active patient-access grant for this account",
                )),
                (Some(_), Some(_)) => Err(ApiError::bad_request(
                    "patient_required",
                    "this account manages several patients; specify patient_id",
                )),
            }
        }
    }
}

/// The grant covering `patient_id` for the caller, or `404` (anti-probing).
pub async fn grant_for_patient(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    patient_id: Uuid,
) -> Result<GrantRow, ApiError> {
    resolve_grant(conn, ctx, Some(patient_id)).await
}

/// Mark grants whose expiry has passed. Idempotent; run from the periodic
/// sweep so listings and audits reflect the terminal state.
pub async fn expire_grants(conn: &mut PgConnection, tenant_id: Uuid) -> Result<u64, ApiError> {
    let done = sqlx::query(
        "UPDATE patient_access_grants SET status = 'expired', version = version + 1
         WHERE tenant_id = $1 AND status = 'active' AND expires_at IS NOT NULL AND expires_at <= now()",
    )
    .bind(tenant_id)
    .execute(&mut *conn)
    .await?;
    Ok(done.rows_affected())
}

async fn load_grant(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    id: Uuid,
) -> Result<GrantRow, ApiError> {
    let row = sqlx::query(&format!(
        "{GRANT_SELECT} WHERE g.tenant_id = $1 AND g.id = $2"
    ))
    .bind(tenant_id)
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(ApiError::not_found)?;
    Ok(grant_from_row(&row))
}

fn grant_ctx(tenant_id: Uuid, patient_id: Uuid, facility_id: Uuid) -> Option<ResourceCtx> {
    Some(ResourceCtx {
        tenant_id,
        patient_id: Some(patient_id),
        facility_id: Some(facility_id),
    })
}

#[derive(Debug, Deserialize)]
pub struct CreateGrantBody {
    pub user_id: Uuid,
    pub patient_id: Uuid,
    pub relationship: String,
    /// How staff verified the identity and the relationship (document
    /// type, in-person check, ...). Mandatory; no PHI beyond what the
    /// verifier needs to record.
    pub verification_note: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
}

/// Staff create a grant after verifying identity and relationship. The
/// account must already exist in the tenant and hold the
/// `patient_representative` role: the grant never adds permissions, it
/// only names which patient the existing self-service permission applies to.
pub async fn create_grant(
    State(state): State<AppState>,
    ctx: AuthContext,
    Json(mut body): Json<CreateGrantBody>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    if !RELATIONSHIPS.contains(&body.relationship.as_str()) {
        return Err(ApiError::bad_request(
            "validation_failed",
            "relationship must be self, parent_guardian or authorized_proxy",
        ));
    }
    let note = scheduling::clean_text(body.verification_note.take(), "verification_note", MAX_NOTE)?
        .filter(|n| n.chars().count() >= MIN_NOTE)
        .ok_or_else(|| {
            ApiError::bad_request(
                "validation_failed",
                "verification_note is required (how identity and relationship were verified)",
            )
        })?;
    if let Some(e) = body.expires_at {
        if e <= Utc::now() {
            return Err(ApiError::bad_request(
                "validation_failed",
                "expires_at must be in the future",
            ));
        }
    }
    let mut conn = state.pool.acquire().await?;
    let patient = sqlx::query("SELECT facility_id FROM patients WHERE id = $1 AND tenant_id = $2")
        .bind(body.patient_id)
        .bind(ctx.tenant_id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(ApiError::not_found)?;
    let facility_id: Uuid = patient.get("facility_id");
    let allowed = guard(
        &state,
        &ctx,
        actions::PATIENT_GRANT_MANAGE,
        "patient_grant",
        grant_ctx(ctx.tenant_id, body.patient_id, facility_id),
    )
    .await?;
    drop(conn);

    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let g = create_grant_in(&mut tx, &ctx, &state, body, note).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(grant_json(&g))))
}

/// Insert a verified grant inside the caller's transaction (authorization
/// and input validation already done). Shared by the staff route and the
/// synthetic fixtures so both follow the same rule: the grantee must be a
/// person of the tenant holding `patient_representative`.
pub async fn create_grant_in(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ctx: &AuthContext,
    state: &AppState,
    body: CreateGrantBody,
    note: String,
) -> Result<GrantRow, ApiError> {
    let user = sqlx::query(
        "SELECT u.is_service,
                EXISTS (SELECT 1 FROM role_assignments ra WHERE ra.user_id = u.id AND ra.role = $3) AS is_rep
         FROM users u WHERE u.id = $1 AND u.tenant_id = $2",
    )
    .bind(body.user_id)
    .bind(ctx.tenant_id)
    .bind(roles::PATIENT_REP)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| {
        ApiError::bad_request("validation_failed", "user_id is not an account of this tenant")
    })?;
    if user.get::<bool, _>("is_service") || !user.get::<bool, _>("is_rep") {
        return Err(ApiError::bad_request(
            "validation_failed",
            "the account must be a person holding the patient_representative role",
        ));
    }
    let id = Uuid::now_v7();
    let inserted = sqlx::query(
        "INSERT INTO patient_access_grants (id, tenant_id, user_id, patient_id, relationship, verified_by,
             verification_note, expires_at)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
    )
    .bind(id)
    .bind(ctx.tenant_id)
    .bind(body.user_id)
    .bind(body.patient_id)
    .bind(&body.relationship)
    .bind(ctx.user_id)
    .bind(&note)
    .bind(body.expires_at)
    .execute(&mut **tx)
    .await;
    match inserted {
        Ok(_) => {}
        Err(sqlx::Error::Database(d)) if d.code().as_deref() == Some("23505") => {
            return Err(ApiError::conflict(
                "grant_exists",
                "this account already holds an active grant for this patient",
            ));
        }
        Err(e) => return Err(e.into()),
    }
    audit::emit(
        &mut **tx,
        ctx,
        "patient_grant.created",
        &state.cell,
        json!({ "grant_id": id, "grantee_user_id": body.user_id, "patient_id": body.patient_id,
                "relationship": body.relationship, "expires_at": body.expires_at }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    load_grant(tx, ctx.tenant_id, id).await
}

#[derive(Debug, Deserialize)]
pub struct ListGrantsQuery {
    pub patient_id: Option<Uuid>,
    pub user_id: Option<Uuid>,
    /// `active` (default), `revoked`, `expired` or `all`.
    pub status: Option<String>,
}

/// Grants of one patient or one account (one of the two is required so
/// the read is always authorized against a concrete patient context).
pub async fn list_grants(
    State(state): State<AppState>,
    ctx: AuthContext,
    Query(q): Query<ListGrantsQuery>,
) -> Result<Json<Value>, ApiError> {
    if q.patient_id.is_none() && q.user_id.is_none() {
        return Err(ApiError::bad_request(
            "validation_failed",
            "patient_id or user_id is required",
        ));
    }
    let status = q.status.as_deref().unwrap_or("active");
    if !matches!(status, "active" | "revoked" | "expired" | "all") {
        return Err(ApiError::bad_request(
            "validation_failed",
            "status must be active, revoked, expired or all",
        ));
    }
    let mut conn = state.pool.acquire().await?;
    let facility_id = match q.patient_id {
        Some(pid) => Some(
            sqlx::query_scalar::<_, Uuid>(
                "SELECT facility_id FROM patients WHERE id = $1 AND tenant_id = $2",
            )
            .bind(pid)
            .bind(ctx.tenant_id)
            .fetch_optional(&mut *conn)
            .await?
            .ok_or_else(ApiError::not_found)?,
        ),
        None => None,
    };
    let allowed = guard(
        &state,
        &ctx,
        actions::PATIENT_GRANT_MANAGE,
        "patient_grant",
        Some(ResourceCtx {
            tenant_id: ctx.tenant_id,
            patient_id: q.patient_id,
            facility_id,
        }),
    )
    .await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    let rows = sqlx::query(&format!(
        "{GRANT_SELECT}
         WHERE g.tenant_id = $1
           AND ($2::uuid IS NULL OR g.patient_id = $2)
           AND ($3::uuid IS NULL OR g.user_id = $3)
         ORDER BY g.granted_at DESC, g.id
         LIMIT 200"
    ))
    .bind(ctx.tenant_id)
    .bind(q.patient_id)
    .bind(q.user_id)
    .fetch_all(&mut *conn)
    .await?;
    let items: Vec<Value> = rows
        .iter()
        .map(grant_from_row)
        .filter(|g| status == "all" || effective_status(g) == status)
        .map(|g| grant_json(&g))
        .collect();
    Ok(Json(json!({ "items": items })))
}

pub async fn get_grant(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let g = load_grant(&mut conn, ctx.tenant_id, id).await?;
    let allowed = guard(
        &state,
        &ctx,
        actions::PATIENT_GRANT_MANAGE,
        "patient_grant",
        grant_ctx(g.tenant_id, g.patient_id, g.patient_facility_id),
    )
    .await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    Ok(Json(grant_json(&g)))
}

#[derive(Debug, Deserialize)]
pub struct RevokeGrantBody {
    pub version: Option<i64>,
    pub reason: Option<String>,
}

/// Revoke an active grant with a mandatory reason. Takes effect
/// immediately for every `/me` call; nothing already booked is undone.
pub async fn revoke_grant(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<RevokeGrantBody>,
) -> Result<Json<Value>, ApiError> {
    let reason = scheduling::clean_text(body.reason, "reason", MAX_NOTE)?
        .filter(|n| n.chars().count() >= MIN_NOTE)
        .ok_or_else(|| ApiError::bad_request("validation_failed", "reason is required"))?;
    let mut conn = state.pool.acquire().await?;
    let g = load_grant(&mut conn, ctx.tenant_id, id).await?;
    let allowed = guard(
        &state,
        &ctx,
        actions::PATIENT_GRANT_MANAGE,
        "patient_grant",
        grant_ctx(g.tenant_id, g.patient_id, g.patient_facility_id),
    )
    .await?;
    drop(conn);
    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let updated = sqlx::query(
        "UPDATE patient_access_grants
         SET status = 'revoked', revoked_at = now(), revoked_by = $3, revoke_reason = $4, version = version + 1
         WHERE tenant_id = $1 AND id = $2 AND status = 'active' AND ($5::bigint IS NULL OR version = $5)",
    )
    .bind(ctx.tenant_id)
    .bind(id)
    .bind(ctx.user_id)
    .bind(&reason)
    .bind(body.version)
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() == 0 {
        let current = load_grant(&mut tx, ctx.tenant_id, id).await?;
        return Err(if current.status != "active" {
            ApiError::conflict("invalid_transition", "the grant is not active")
        } else {
            ApiError::conflict("version_conflict", "the grant changed; reload and retry")
        });
    }
    audit::emit(
        &mut *tx,
        &ctx,
        "patient_grant.revoked",
        &state.cell,
        json!({ "grant_id": id, "grantee_user_id": g.user_id, "patient_id": g.patient_id }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    let g = load_grant(&mut tx, ctx.tenant_id, id).await?;
    tx.commit().await?;
    Ok(Json(grant_json(&g)))
}
