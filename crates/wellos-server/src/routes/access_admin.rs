//! Scheduling administration: open catalogs, tenant scheduling policy,
//! facility hours/location, schedulable resources with their services,
//! weekly availability, dated exceptions, service resource requirements and
//! the tenant operational calendar.
//!
//! Catalog entries are runtime data (never Rust enums): a tenant
//! administrator adds a previously unknown specialty, profession or resource
//! type here and it is immediately usable by the matcher. Nothing in a
//! catalog grants authorization — permissions stay in role assignments.
//! Every mutation runs under a row lock, is bound to the caller's expected
//! `version`, writes an append-only history snapshot and an audit event.

use crate::audit;
use crate::auth::AuthContext;
use crate::error::ApiError;
use crate::policy::{actions, facility_scope, ResourceCtx};
use crate::routes::guard;
use crate::scheduling;
use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::Json;
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::{PgConnection, Row, Transaction};
use uuid::Uuid;
use wellos_domain::access::{is_catalog_kind, is_valid_code, ServiceConfig, CATALOG_KINDS};

const MAX_NAME: usize = 200;
const MAX_TEXT: usize = 500;
const MAX_SYNONYMS: usize = 30;
const MAX_CODINGS: usize = 20;
const MAX_LIST: usize = 30;
const MAX_CONFIG_BYTES: usize = 16 * 1024;
const DEFAULT_LIMIT: i64 = 100;
const MAX_LIMIT: i64 = 200;

// ---------------------------------------------------------------------------
// Shared validation helpers
// ---------------------------------------------------------------------------

fn char_len(s: &str) -> usize {
    s.chars().count()
}

fn clean_required(value: &str, field: &str, max: usize) -> Result<String, ApiError> {
    let t = value.trim();
    if t.is_empty() {
        return Err(ApiError::bad_request(
            "validation_failed",
            format!("{field} is required"),
        ));
    }
    if char_len(t) > max {
        return Err(ApiError::bad_request(
            "validation_failed",
            format!("{field} exceeds {max} characters"),
        ));
    }
    Ok(t.to_string())
}

fn clean_optional(
    value: Option<String>,
    field: &str,
    max: usize,
) -> Result<Option<String>, ApiError> {
    match value {
        None => Ok(None),
        Some(v) if v.trim().is_empty() => Ok(None),
        Some(v) => clean_required(&v, field, max).map(Some),
    }
}

fn require_kind(kind: &str) -> Result<String, ApiError> {
    let k = kind.trim();
    if !is_catalog_kind(k) {
        return Err(ApiError::bad_request(
            "validation_failed",
            format!("kind must be one of {}", CATALOG_KINDS.join(", ")),
        ));
    }
    Ok(k.to_string())
}

fn require_code(code: &str, field: &str) -> Result<String, ApiError> {
    let c = code.trim();
    if !is_valid_code(c) {
        return Err(ApiError::bad_request(
            "validation_failed",
            format!("{field} must be 1-64 lowercase letters, digits, '_', '.' or '-'"),
        ));
    }
    Ok(c.to_string())
}

fn clean_codes(values: Vec<String>, field: &str) -> Result<Vec<String>, ApiError> {
    if values.len() > MAX_LIST {
        return Err(ApiError::bad_request(
            "validation_failed",
            format!("{field} accepts at most {MAX_LIST} entries"),
        ));
    }
    let mut out = Vec::new();
    for v in values {
        let c = require_code(&v, field)?;
        if !out.contains(&c) {
            out.push(c);
        }
    }
    Ok(out)
}

fn clean_synonyms(values: Vec<String>) -> Result<Vec<String>, ApiError> {
    if values.len() > MAX_SYNONYMS {
        return Err(ApiError::bad_request(
            "validation_failed",
            format!("synonyms accepts at most {MAX_SYNONYMS} entries"),
        ));
    }
    let mut out = Vec::new();
    for v in values {
        let s = clean_required(&v, "synonyms", MAX_NAME)?;
        if !out.contains(&s) {
            out.push(s);
        }
    }
    Ok(out)
}

fn clean_codings(value: Option<Value>) -> Result<Value, ApiError> {
    let Some(value) = value else {
        return Ok(json!([]));
    };
    let Some(items) = value.as_array() else {
        return Err(ApiError::bad_request(
            "validation_failed",
            "external_codings must be an array of {system, code, display?}",
        ));
    };
    if items.len() > MAX_CODINGS {
        return Err(ApiError::bad_request(
            "validation_failed",
            format!("external_codings accepts at most {MAX_CODINGS} entries"),
        ));
    }
    let mut out = Vec::new();
    for it in items {
        let system = it
            .get("system")
            .and_then(Value::as_str)
            .map(|s| clean_required(s, "external_codings.system", MAX_TEXT))
            .transpose()?;
        let code = it
            .get("code")
            .and_then(Value::as_str)
            .map(|s| clean_required(s, "external_codings.code", MAX_NAME))
            .transpose()?;
        let (Some(system), Some(code)) = (system, code) else {
            return Err(ApiError::bad_request(
                "validation_failed",
                "each external coding needs a system and a code",
            ));
        };
        let display = it
            .get("display")
            .and_then(Value::as_str)
            .map(|s| clean_required(s, "external_codings.display", MAX_TEXT))
            .transpose()?;
        out.push(json!({ "system": system, "code": code, "display": display }));
    }
    Ok(Value::Array(out))
}

fn validate_tz(tz: &str) -> Result<String, ApiError> {
    let t = tz.trim();
    if t.parse::<chrono_tz::Tz>().is_err() {
        return Err(ApiError::bad_request(
            "validation_failed",
            "time_zone must be a valid IANA time zone",
        ));
    }
    Ok(t.to_string())
}

fn validate_dates(from: Option<NaiveDate>, to: Option<NaiveDate>) -> Result<(), ApiError> {
    if let (Some(f), Some(t)) = (from, to) {
        if t < f {
            return Err(ApiError::bad_request(
                "validation_failed",
                "effective_to must not precede effective_from",
            ));
        }
    }
    Ok(())
}

/// Kind-specific configuration is validated server-side; the UI never
/// decides what is acceptable.
async fn validate_config(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    kind: &str,
    config: Option<Value>,
) -> Result<Value, ApiError> {
    let config = config.unwrap_or_else(|| json!({}));
    if !config.is_object() {
        return Err(ApiError::bad_request(
            "validation_failed",
            "config must be a JSON object",
        ));
    }
    if config.to_string().len() > MAX_CONFIG_BYTES {
        return Err(ApiError::bad_request(
            "validation_failed",
            "config exceeds the allowed size",
        ));
    }
    match kind {
        "clinical_service" => {
            let parsed: ServiceConfig = serde_json::from_value(config.clone())
                .map_err(|e| ApiError::bad_request("validation_failed", format!("config: {e}")))?;
            parsed
                .validate()
                .map_err(|m| ApiError::bad_request("validation_failed", format!("config: {m}")))?;
            scheduling::require_codes(
                conn,
                tenant_id,
                "modality",
                &parsed.modality_codes,
                "config.modality_codes",
            )
            .await?;
            scheduling::require_codes(
                conn,
                tenant_id,
                "resource_type",
                &parsed.required_resource_types,
                "config.required_resource_types",
            )
            .await?;
            serde_json::to_value(parsed).map_err(ApiError::internal)
        }
        "location" => {
            let lat = config.get("latitude").and_then(Value::as_f64);
            let lon = config.get("longitude").and_then(Value::as_f64);
            let radius = config.get("service_radius_km").and_then(Value::as_f64);
            match (lat, lon) {
                (None, None) => {}
                (Some(la), Some(lo))
                    if (-90.0..=90.0).contains(&la) && (-180.0..=180.0).contains(&lo) => {}
                _ => {
                    return Err(ApiError::bad_request(
                        "validation_failed",
                        "config.latitude/longitude must both be present and within range",
                    ))
                }
            }
            if let Some(r) = radius {
                if !(r > 0.0 && r <= 5000.0) {
                    return Err(ApiError::bad_request(
                        "validation_failed",
                        "config.service_radius_km must be between 0 and 5000",
                    ));
                }
            }
            Ok(config)
        }
        _ => Ok(config),
    }
}

async fn tenant_row(
    conn: &mut PgConnection,
    id: Uuid,
    table: &str,
) -> Result<Option<sqlx::postgres::PgRow>, ApiError> {
    let sql = format!("SELECT * FROM {table} WHERE id = $1");
    Ok(sqlx::query(&sql).bind(id).fetch_optional(conn).await?)
}

/// Facilities are trusted tenant data: an unknown or foreign facility is
/// indistinguishable from a missing one (anti-enumeration).
async fn facility_in_tenant(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    facility_id: Uuid,
) -> Result<(), ApiError> {
    let ok: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM facilities WHERE id = $1 AND tenant_id = $2")
            .bind(facility_id)
            .bind(tenant_id)
            .fetch_optional(conn)
            .await?;
    ok.map(|_| ()).ok_or_else(ApiError::not_found)
}

fn require_version(current: i64, expected: i64) -> Result<(), ApiError> {
    if current != expected {
        return Err(ApiError::conflict(
            "version_conflict",
            format!("the record has changed (current version {current}); reload and retry"),
        ));
    }
    Ok(())
}

fn limit_of(limit: Option<i64>) -> i64 {
    limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
}

async fn emit(
    tx: &mut Transaction<'_, sqlx::Postgres>,
    ctx: &AuthContext,
    state: &AppState,
    event: &str,
    refs: Value,
) -> Result<(), ApiError> {
    audit::emit(&mut **tx, ctx, event, &state.cell, refs, None)
        .await
        .map_err(ApiError::internal)
}

// ---------------------------------------------------------------------------
// Catalogs
// ---------------------------------------------------------------------------

fn catalog_json(r: &sqlx::postgres::PgRow) -> Value {
    json!({
        "id": r.get::<Uuid, _>("id"),
        "kind": r.get::<String, _>("kind"),
        "code": r.get::<String, _>("code"),
        "parent_id": r.get::<Option<Uuid>, _>("parent_id"),
        "name_en": r.get::<String, _>("name_en"),
        "name_es": r.get::<String, _>("name_es"),
        "synonyms": r.get::<Vec<String>, _>("synonyms"),
        "external_codings": r.get::<Value, _>("external_codings"),
        "config": r.get::<Value, _>("config"),
        "active": r.get::<bool, _>("active"),
        "effective_from": r.get::<Option<NaiveDate>, _>("effective_from"),
        "effective_to": r.get::<Option<NaiveDate>, _>("effective_to"),
        "version": r.get::<i64, _>("version"),
        "created_at": r.get::<DateTime<Utc>, _>("created_at"),
        "updated_at": r.get::<DateTime<Utc>, _>("updated_at"),
    })
}

async fn catalog_facilities(
    conn: &mut PgConnection,
    entry_id: Uuid,
) -> Result<Vec<Uuid>, ApiError> {
    Ok(sqlx::query_scalar(
        "SELECT facility_id FROM catalog_entry_facilities WHERE entry_id = $1 ORDER BY facility_id",
    )
    .bind(entry_id)
    .fetch_all(conn)
    .await?)
}

async fn load_catalog_entry(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    id: Uuid,
) -> Result<sqlx::postgres::PgRow, ApiError> {
    let row = tenant_row(conn, id, "catalog_entries").await?;
    match row {
        Some(r) if r.get::<Uuid, _>("tenant_id") == tenant_id => Ok(r),
        _ => Err(ApiError::not_found()),
    }
}

async fn write_catalog_history(
    tx: &mut Transaction<'_, sqlx::Postgres>,
    ctx: &AuthContext,
    entry: &sqlx::postgres::PgRow,
    reason: Option<&str>,
) -> Result<(), ApiError> {
    let mut snapshot = catalog_json(entry);
    let facilities = catalog_facilities(tx, entry.get("id")).await?;
    snapshot["facility_ids"] = json!(facilities);
    sqlx::query(
        "INSERT INTO catalog_entry_history (id, tenant_id, entry_id, version, snapshot, change_reason, changed_by)
         VALUES ($1,$2,$3,$4,$5,$6,$7)",
    )
    .bind(Uuid::now_v7())
    .bind(entry.get::<Uuid, _>("tenant_id"))
    .bind(entry.get::<Uuid, _>("id"))
    .bind(entry.get::<i64, _>("version"))
    .bind(snapshot)
    .bind(reason)
    .bind(ctx.user_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

#[derive(Deserialize)]
pub struct CatalogQuery {
    pub kind: Option<String>,
    pub q: Option<String>,
    pub parent_id: Option<Uuid>,
    pub facility_id: Option<Uuid>,
    pub include_inactive: Option<bool>,
    pub limit: Option<i64>,
    pub after: Option<String>,
}

/// List catalog entries of one kind (or all kinds), optionally filtered by
/// text, parent and facility availability. Keyset-paginated on `code`.
pub async fn list_catalog(
    State(state): State<AppState>,
    ctx: AuthContext,
    Query(q): Query<CatalogQuery>,
) -> Result<Json<Value>, ApiError> {
    guard(
        &state,
        &ctx,
        actions::CATALOG_READ,
        "catalog_entry",
        Some(ResourceCtx {
            tenant_id: ctx.tenant_id,
            patient_id: None,
            facility_id: None,
        }),
    )
    .await?;
    let kind = q.kind.as_deref().map(require_kind).transpose()?;
    let text =
        q.q.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| {
                if char_len(s) > MAX_NAME {
                    Err(ApiError::bad_request("validation_failed", "q is too long"))
                } else {
                    Ok(format!("%{}%", s.to_lowercase()))
                }
            })
            .transpose()?;
    let limit = limit_of(q.limit);
    let include_inactive = q.include_inactive.unwrap_or(false);
    let rows = sqlx::query(
        "SELECT e.* FROM catalog_entries e
         WHERE e.tenant_id = $1
           AND ($2::text IS NULL OR e.kind = $2)
           AND ($3::boolean OR e.active)
           AND ($4::uuid IS NULL OR e.parent_id = $4)
           AND ($5::text IS NULL OR lower(e.name_en) LIKE $5 OR lower(e.name_es) LIKE $5
                OR e.code LIKE $5 OR EXISTS (SELECT 1 FROM unnest(e.synonyms) s WHERE lower(s) LIKE $5))
           AND ($6::uuid IS NULL OR NOT EXISTS (SELECT 1 FROM catalog_entry_facilities f WHERE f.entry_id = e.id)
                OR EXISTS (SELECT 1 FROM catalog_entry_facilities f WHERE f.entry_id = e.id AND f.facility_id = $6))
           AND ($7::text IS NULL OR (e.kind, e.code) > (split_part($7, ':', 1), split_part($7, ':', 2)))
         ORDER BY e.kind, e.code
         LIMIT $8",
    )
    .bind(ctx.tenant_id)
    .bind(&kind)
    .bind(include_inactive)
    .bind(q.parent_id)
    .bind(&text)
    .bind(q.facility_id)
    .bind(q.after.as_deref().filter(|a| a.contains(':')))
    .bind(limit + 1)
    .fetch_all(&state.pool)
    .await?;
    let has_more = rows.len() as i64 > limit;
    let rows: Vec<_> = rows.into_iter().take(limit as usize).collect();
    let next = if has_more {
        rows.last().map(|r| {
            format!(
                "{}:{}",
                r.get::<String, _>("kind"),
                r.get::<String, _>("code")
            )
        })
    } else {
        None
    };
    let items: Vec<Value> = rows.iter().map(catalog_json).collect();
    Ok(Json(json!({
        "items": items,
        "next": next,
        "kinds": CATALOG_KINDS,
    })))
}

pub async fn catalog_detail(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    guard(
        &state,
        &ctx,
        actions::CATALOG_READ,
        "catalog_entry",
        Some(ResourceCtx {
            tenant_id: ctx.tenant_id,
            patient_id: None,
            facility_id: None,
        }),
    )
    .await?;
    let mut conn = state.pool.acquire().await?;
    let row = load_catalog_entry(&mut conn, ctx.tenant_id, id).await?;
    let mut out = catalog_json(&row);
    out["facility_ids"] = json!(catalog_facilities(&mut conn, id).await?);
    let children: Vec<Value> = sqlx::query(
        "SELECT id, code, name_en, name_es, active FROM catalog_entries WHERE parent_id = $1 ORDER BY code LIMIT 200",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await?
    .iter()
    .map(|r| {
        json!({
            "id": r.get::<Uuid, _>("id"),
            "code": r.get::<String, _>("code"),
            "name_en": r.get::<String, _>("name_en"),
            "name_es": r.get::<String, _>("name_es"),
            "active": r.get::<bool, _>("active"),
        })
    })
    .collect();
    out["children"] = json!(children);
    Ok(Json(out))
}

pub async fn catalog_history(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    guard(
        &state,
        &ctx,
        actions::CATALOG_READ,
        "catalog_entry",
        Some(ResourceCtx {
            tenant_id: ctx.tenant_id,
            patient_id: None,
            facility_id: None,
        }),
    )
    .await?;
    let mut conn = state.pool.acquire().await?;
    load_catalog_entry(&mut conn, ctx.tenant_id, id).await?;
    let rows = sqlx::query(
        "SELECT h.version, h.snapshot, h.change_reason, h.recorded_at, u.display_name
         FROM catalog_entry_history h JOIN users u ON u.id = h.changed_by
         WHERE h.entry_id = $1 ORDER BY h.version DESC LIMIT 200",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await?;
    let items: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "version": r.get::<i64, _>("version"),
                "snapshot": r.get::<Value, _>("snapshot"),
                "change_reason": r.get::<Option<String>, _>("change_reason"),
                "changed_by": r.get::<String, _>("display_name"),
                "recorded_at": r.get::<DateTime<Utc>, _>("recorded_at"),
            })
        })
        .collect();
    Ok(Json(json!({ "entry_id": id, "items": items })))
}

#[derive(Deserialize)]
pub struct CreateCatalogEntry {
    pub kind: String,
    pub code: String,
    pub parent_id: Option<Uuid>,
    pub name_en: String,
    pub name_es: String,
    #[serde(default)]
    pub synonyms: Vec<String>,
    pub external_codings: Option<Value>,
    pub config: Option<Value>,
    pub effective_from: Option<NaiveDate>,
    pub effective_to: Option<NaiveDate>,
    #[serde(default)]
    pub facility_ids: Vec<Uuid>,
    pub change_reason: Option<String>,
}

fn manage_ctx(ctx: &AuthContext) -> Option<ResourceCtx> {
    Some(ResourceCtx {
        tenant_id: ctx.tenant_id,
        patient_id: None,
        facility_id: None,
    })
}

async fn check_parent(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    kind: &str,
    parent_id: Option<Uuid>,
    self_id: Option<Uuid>,
) -> Result<(), ApiError> {
    let Some(pid) = parent_id else { return Ok(()) };
    if Some(pid) == self_id {
        return Err(ApiError::bad_request(
            "validation_failed",
            "an entry cannot be its own parent",
        ));
    }
    let parent = load_catalog_entry(conn, tenant_id, pid)
        .await
        .map_err(|_| {
            ApiError::bad_request(
                "validation_failed",
                "parent_id is not a catalog entry of this tenant",
            )
        })?;
    if parent.get::<String, _>("kind") != kind {
        return Err(ApiError::bad_request(
            "validation_failed",
            "parent must be an entry of the same kind",
        ));
    }
    // Walk up to reject cycles (bounded depth).
    let mut cursor = parent.get::<Option<Uuid>, _>("parent_id");
    for _ in 0..32 {
        let Some(c) = cursor else { break };
        if Some(c) == self_id {
            return Err(ApiError::bad_request(
                "validation_failed",
                "parent_id would create a hierarchy cycle",
            ));
        }
        cursor = sqlx::query_scalar("SELECT parent_id FROM catalog_entries WHERE id = $1")
            .bind(c)
            .fetch_optional(&mut *conn)
            .await?
            .flatten();
    }
    Ok(())
}

async fn replace_facilities(
    tx: &mut Transaction<'_, sqlx::Postgres>,
    tenant_id: Uuid,
    entry_id: Uuid,
    facility_ids: &[Uuid],
) -> Result<(), ApiError> {
    if facility_ids.len() > MAX_LIST {
        return Err(ApiError::bad_request(
            "validation_failed",
            format!("facility_ids accepts at most {MAX_LIST} entries"),
        ));
    }
    for f in facility_ids {
        facility_in_tenant(tx, tenant_id, *f).await.map_err(|_| {
            ApiError::bad_request(
                "validation_failed",
                "facility_ids contains an unknown facility",
            )
        })?;
    }
    sqlx::query("DELETE FROM catalog_entry_facilities WHERE entry_id = $1")
        .bind(entry_id)
        .execute(&mut **tx)
        .await?;
    for f in facility_ids {
        sqlx::query(
            "INSERT INTO catalog_entry_facilities (tenant_id, entry_id, facility_id) VALUES ($1,$2,$3)
             ON CONFLICT DO NOTHING",
        )
        .bind(tenant_id)
        .bind(entry_id)
        .bind(f)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

pub async fn create_catalog(
    State(state): State<AppState>,
    ctx: AuthContext,
    Json(body): Json<CreateCatalogEntry>,
) -> Result<Json<Value>, ApiError> {
    let allowed = guard(
        &state,
        &ctx,
        actions::CATALOG_MANAGE,
        "catalog_entry",
        manage_ctx(&ctx),
    )
    .await?;
    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let out = create_catalog_in(&mut tx, &ctx, &state, body).await?;
    tx.commit().await?;
    Ok(Json(out))
}

/// Create a catalog entry inside the caller's transaction (route and
/// synthetic fixtures share this path). Authorization is the caller's job.
pub async fn create_catalog_in(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ctx: &AuthContext,
    state: &AppState,
    body: CreateCatalogEntry,
) -> Result<Value, ApiError> {
    let kind = require_kind(&body.kind)?;
    let code = require_code(&body.code, "code")?;
    let name_en = clean_required(&body.name_en, "name_en", MAX_NAME)?;
    let name_es = clean_required(&body.name_es, "name_es", MAX_NAME)?;
    let synonyms = clean_synonyms(body.synonyms)?;
    let codings = clean_codings(body.external_codings)?;
    validate_dates(body.effective_from, body.effective_to)?;
    let reason = clean_optional(body.change_reason, "change_reason", MAX_TEXT)?;
    let config = validate_config(tx, ctx.tenant_id, &kind, body.config).await?;
    check_parent(tx, ctx.tenant_id, &kind, body.parent_id, None).await?;
    let id = Uuid::now_v7();
    let inserted = sqlx::query(
        "INSERT INTO catalog_entries (id, tenant_id, kind, code, parent_id, name_en, name_es, synonyms,
             external_codings, config, effective_from, effective_to, created_by)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)",
    )
    .bind(id)
    .bind(ctx.tenant_id)
    .bind(&kind)
    .bind(&code)
    .bind(body.parent_id)
    .bind(&name_en)
    .bind(&name_es)
    .bind(&synonyms)
    .bind(&codings)
    .bind(&config)
    .bind(body.effective_from)
    .bind(body.effective_to)
    .bind(ctx.user_id)
    .execute(&mut **tx)
    .await;
    match inserted {
        Ok(_) => {}
        Err(sqlx::Error::Database(d)) if d.code().as_deref() == Some("23505") => {
            return Err(ApiError::conflict(
                "code_exists",
                "an entry with this kind and code already exists in this tenant",
            ));
        }
        Err(e) => return Err(e.into()),
    }
    replace_facilities(tx, ctx.tenant_id, id, &body.facility_ids).await?;
    let row = load_catalog_entry(tx, ctx.tenant_id, id).await?;
    write_catalog_history(tx, ctx, &row, reason.as_deref()).await?;
    emit(
        tx,
        ctx,
        state,
        "catalog.entry.created",
        json!({ "entry_id": id, "kind": kind, "code": code }),
    )
    .await?;
    let mut out = catalog_json(&row);
    out["facility_ids"] = json!(body.facility_ids);
    Ok(out)
}

#[derive(Deserialize)]
pub struct UpdateCatalogEntry {
    pub version: i64,
    pub parent_id: Option<Uuid>,
    pub clear_parent: Option<bool>,
    pub name_en: Option<String>,
    pub name_es: Option<String>,
    pub synonyms: Option<Vec<String>>,
    pub external_codings: Option<Value>,
    pub config: Option<Value>,
    pub effective_from: Option<NaiveDate>,
    pub effective_to: Option<NaiveDate>,
    pub clear_effective_to: Option<bool>,
    pub facility_ids: Option<Vec<Uuid>>,
    pub active: Option<bool>,
    pub change_reason: Option<String>,
}

/// Update an entry in place with a new version and history snapshot. The
/// code and kind are immutable (they are the stable identity other rows
/// reference); deactivate and create a new entry to "rename".
pub async fn update_catalog(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateCatalogEntry>,
) -> Result<Json<Value>, ApiError> {
    let allowed = guard(
        &state,
        &ctx,
        actions::CATALOG_MANAGE,
        "catalog_entry",
        manage_ctx(&ctx),
    )
    .await?;
    let reason = clean_optional(body.change_reason, "change_reason", MAX_TEXT)?;
    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let row =
        sqlx::query("SELECT * FROM catalog_entries WHERE id = $1 AND tenant_id = $2 FOR UPDATE")
            .bind(id)
            .bind(ctx.tenant_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(ApiError::not_found)?;
    require_version(row.get("version"), body.version)?;
    let kind: String = row.get("kind");
    let code: String = row.get("code");

    let name_en = match body.name_en {
        Some(n) => clean_required(&n, "name_en", MAX_NAME)?,
        None => row.get("name_en"),
    };
    let name_es = match body.name_es {
        Some(n) => clean_required(&n, "name_es", MAX_NAME)?,
        None => row.get("name_es"),
    };
    let synonyms = match body.synonyms {
        Some(s) => clean_synonyms(s)?,
        None => row.get("synonyms"),
    };
    let codings = match body.external_codings {
        Some(c) => clean_codings(Some(c))?,
        None => row.get("external_codings"),
    };
    let config = match body.config {
        Some(c) => validate_config(&mut tx, ctx.tenant_id, &kind, Some(c)).await?,
        None => row.get("config"),
    };
    let parent_id = if body.clear_parent.unwrap_or(false) {
        None
    } else {
        match body.parent_id {
            Some(p) => {
                check_parent(&mut tx, ctx.tenant_id, &kind, Some(p), Some(id)).await?;
                Some(p)
            }
            None => row.get("parent_id"),
        }
    };
    let effective_from = body.effective_from.or_else(|| row.get("effective_from"));
    let effective_to = if body.clear_effective_to.unwrap_or(false) {
        None
    } else {
        body.effective_to.or_else(|| row.get("effective_to"))
    };
    validate_dates(effective_from, effective_to)?;
    let active = body.active.unwrap_or_else(|| row.get("active"));
    if active && !row.get::<bool, _>("active") {
        // Reactivation must not resurrect an entry under an inactive parent.
        if let Some(p) = parent_id {
            let parent_active: Option<bool> =
                sqlx::query_scalar("SELECT active FROM catalog_entries WHERE id = $1")
                    .bind(p)
                    .fetch_optional(&mut *tx)
                    .await?;
            if parent_active != Some(true) {
                return Err(ApiError::conflict(
                    "parent_inactive",
                    "reactivate the parent entry first",
                ));
            }
        }
    }
    sqlx::query(
        "UPDATE catalog_entries SET parent_id = $2, name_en = $3, name_es = $4, synonyms = $5,
             external_codings = $6, config = $7, effective_from = $8, effective_to = $9, active = $10,
             version = version + 1, updated_at = now()
         WHERE id = $1",
    )
    .bind(id)
    .bind(parent_id)
    .bind(&name_en)
    .bind(&name_es)
    .bind(&synonyms)
    .bind(&codings)
    .bind(&config)
    .bind(effective_from)
    .bind(effective_to)
    .bind(active)
    .execute(&mut *tx)
    .await?;
    if let Some(f) = &body.facility_ids {
        replace_facilities(&mut tx, ctx.tenant_id, id, f).await?;
    }
    let row = load_catalog_entry(&mut tx, ctx.tenant_id, id).await?;
    write_catalog_history(&mut tx, &ctx, &row, reason.as_deref()).await?;
    emit(
        &mut tx,
        &ctx,
        &state,
        if active { "catalog.entry.updated" } else { "catalog.entry.deactivated" },
        json!({ "entry_id": id, "kind": kind, "code": code, "version": row.get::<i64, _>("version") }),
    )
    .await?;
    let facilities = catalog_facilities(&mut tx, id).await?;
    tx.commit().await?;
    let mut out = catalog_json(&row);
    out["facility_ids"] = json!(facilities);
    Ok(Json(out))
}

#[derive(Deserialize)]
pub struct Deactivate {
    pub version: i64,
    pub change_reason: Option<String>,
}

/// Lifecycle end: the entry stays referenceable by historical rows but is no
/// longer offered. Children are deactivated in the same transaction so a
/// hierarchy never has active leaves under an inactive parent.
pub async fn deactivate_catalog(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<Deactivate>,
) -> Result<Json<Value>, ApiError> {
    let allowed = guard(
        &state,
        &ctx,
        actions::CATALOG_MANAGE,
        "catalog_entry",
        manage_ctx(&ctx),
    )
    .await?;
    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let out = deactivate_catalog_in(&mut tx, &ctx, &state, id, body).await?;
    tx.commit().await?;
    Ok(Json(out))
}

/// Transactional core of [`deactivate_catalog`] (authorization already
/// recorded by the caller): deactivates the entry and its descendants,
/// writing one history row per affected entry.
pub async fn deactivate_catalog_in(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ctx: &AuthContext,
    state: &AppState,
    id: Uuid,
    body: Deactivate,
) -> Result<Value, ApiError> {
    let reason = clean_optional(body.change_reason, "change_reason", MAX_TEXT)?;
    let row =
        sqlx::query("SELECT * FROM catalog_entries WHERE id = $1 AND tenant_id = $2 FOR UPDATE")
            .bind(id)
            .bind(ctx.tenant_id)
            .fetch_optional(&mut **tx)
            .await?
            .ok_or_else(ApiError::not_found)?;
    require_version(row.get("version"), body.version)?;
    if !row.get::<bool, _>("active") {
        return Err(ApiError::conflict(
            "already_inactive",
            "this entry is already inactive",
        ));
    }
    let affected = sqlx::query(
        "WITH RECURSIVE tree AS (
             SELECT id FROM catalog_entries WHERE id = $1
             UNION ALL
             SELECT c.id FROM catalog_entries c JOIN tree t ON c.parent_id = t.id
         )
         UPDATE catalog_entries e SET active = false, version = version + 1, updated_at = now()
         FROM tree WHERE e.id = tree.id AND e.active
         RETURNING e.id",
    )
    .bind(id)
    .fetch_all(&mut **tx)
    .await?;
    for r in &affected {
        let eid: Uuid = r.get("id");
        let e = load_catalog_entry(tx, ctx.tenant_id, eid).await?;
        write_catalog_history(tx, ctx, &e, reason.as_deref()).await?;
    }
    emit(
        tx,
        ctx,
        state,
        "catalog.entry.deactivated",
        json!({ "entry_id": id, "kind": row.get::<String, _>("kind"), "code": row.get::<String, _>("code"),
                "cascaded": affected.len().saturating_sub(1) }),
    )
    .await?;
    let row = load_catalog_entry(tx, ctx.tenant_id, id).await?;
    Ok(catalog_json(&row))
}

// ---------------------------------------------------------------------------
// Tenant scheduling policy
// ---------------------------------------------------------------------------

pub async fn get_policy(
    State(state): State<AppState>,
    ctx: AuthContext,
) -> Result<Json<Value>, ApiError> {
    guard(
        &state,
        &ctx,
        actions::SCHEDULING_READ,
        "scheduling_policy",
        manage_ctx(&ctx),
    )
    .await?;
    let mut conn = state.pool.acquire().await?;
    let p = scheduling::load_policy(&mut conn, ctx.tenant_id).await?;
    Ok(Json(serde_json::to_value(p).map_err(ApiError::internal)?))
}

#[derive(Deserialize)]
pub struct UpdatePolicy {
    pub version: i64,
    pub time_zone: Option<String>,
    pub hold_minutes: Option<i32>,
    pub offer_ttl_minutes: Option<i32>,
    pub cancellation_window_hours: Option<i32>,
    pub reschedule_window_hours: Option<i32>,
    pub min_notice_hours: Option<i32>,
    pub horizon_days: Option<i32>,
    pub patient_confirmation_required: Option<bool>,
    pub confirmation_deadline_hours: Option<i32>,
    pub reminder_lead_hours: Option<Vec<i32>>,
    pub quiet_hours_start: Option<NaiveTime>,
    pub quiet_hours_end: Option<NaiveTime>,
    pub max_candidates: Option<i32>,
}

fn range(v: i32, lo: i32, hi: i32, field: &str) -> Result<i32, ApiError> {
    if v < lo || v > hi {
        return Err(ApiError::bad_request(
            "validation_failed",
            format!("{field} must be between {lo} and {hi}"),
        ));
    }
    Ok(v)
}

pub async fn update_policy(
    State(state): State<AppState>,
    ctx: AuthContext,
    Json(body): Json<UpdatePolicy>,
) -> Result<Json<Value>, ApiError> {
    let allowed = guard(
        &state,
        &ctx,
        actions::SCHEDULING_MANAGE,
        "scheduling_policy",
        manage_ctx(&ctx),
    )
    .await?;
    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let p = update_policy_in(&mut tx, &ctx, &state, body).await?;
    tx.commit().await?;
    Ok(Json(serde_json::to_value(p).map_err(ApiError::internal)?))
}

/// Apply a tenant policy update inside the caller's transaction.
pub async fn update_policy_in(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ctx: &AuthContext,
    state: &AppState,
    body: UpdatePolicy,
) -> Result<scheduling::Policy, ApiError> {
    // Policies are created lazily with defaults so the first update works.
    sqlx::query(
        "INSERT INTO tenant_scheduling_policies (tenant_id) VALUES ($1) ON CONFLICT (tenant_id) DO NOTHING",
    )
    .bind(ctx.tenant_id)
    .execute(&mut **tx)
    .await?;
    sqlx::query("SELECT tenant_id FROM tenant_scheduling_policies WHERE tenant_id = $1 FOR UPDATE")
        .bind(ctx.tenant_id)
        .execute(&mut **tx)
        .await?;
    let cur = scheduling::load_policy(tx, ctx.tenant_id).await?;
    require_version(cur.version, body.version)?;
    let time_zone = match body.time_zone {
        Some(t) => validate_tz(&t)?,
        None => cur.time_zone.clone(),
    };
    let hold = range(
        body.hold_minutes.unwrap_or(cur.hold_minutes),
        1,
        1440,
        "hold_minutes",
    )?;
    let ttl = range(
        body.offer_ttl_minutes.unwrap_or(cur.offer_ttl_minutes),
        5,
        10080,
        "offer_ttl_minutes",
    )?;
    let cancel = range(
        body.cancellation_window_hours
            .unwrap_or(cur.cancellation_window_hours),
        0,
        720,
        "cancellation_window_hours",
    )?;
    let resched = range(
        body.reschedule_window_hours
            .unwrap_or(cur.reschedule_window_hours),
        0,
        720,
        "reschedule_window_hours",
    )?;
    let notice = range(
        body.min_notice_hours.unwrap_or(cur.min_notice_hours),
        0,
        720,
        "min_notice_hours",
    )?;
    let horizon = range(
        body.horizon_days.unwrap_or(cur.horizon_days),
        1,
        365,
        "horizon_days",
    )?;
    let confirm_required = body
        .patient_confirmation_required
        .unwrap_or(cur.patient_confirmation_required);
    let confirm_deadline = range(
        body.confirmation_deadline_hours
            .unwrap_or(cur.confirmation_deadline_hours),
        1,
        720,
        "confirmation_deadline_hours",
    )?;
    let reminders = match body.reminder_lead_hours {
        Some(r) => {
            if r.len() > 6 {
                return Err(ApiError::bad_request(
                    "validation_failed",
                    "reminder_lead_hours accepts at most 6 entries",
                ));
            }
            let mut out: Vec<i32> = Vec::new();
            for h in r {
                range(h, 1, 720, "reminder_lead_hours")?;
                if !out.contains(&h) {
                    out.push(h);
                }
            }
            out.sort_unstable_by(|a, b| b.cmp(a));
            out
        }
        None => cur.reminder_lead_hours.clone(),
    };
    let qs = body.quiet_hours_start.unwrap_or(cur.quiet_hours_start);
    let qe = body.quiet_hours_end.unwrap_or(cur.quiet_hours_end);
    let max_candidates = range(
        body.max_candidates.unwrap_or(cur.max_candidates),
        1,
        20,
        "max_candidates",
    )?;
    sqlx::query(
        "UPDATE tenant_scheduling_policies SET time_zone = $2, hold_minutes = $3, offer_ttl_minutes = $4,
             cancellation_window_hours = $5, reschedule_window_hours = $6, min_notice_hours = $7,
             horizon_days = $8, patient_confirmation_required = $9, confirmation_deadline_hours = $10,
             reminder_lead_hours = $11, quiet_hours_start = $12, quiet_hours_end = $13, max_candidates = $14,
             version = version + 1, updated_by = $15, updated_at = now()
         WHERE tenant_id = $1",
    )
    .bind(ctx.tenant_id)
    .bind(&time_zone)
    .bind(hold)
    .bind(ttl)
    .bind(cancel)
    .bind(resched)
    .bind(notice)
    .bind(horizon)
    .bind(confirm_required)
    .bind(confirm_deadline)
    .bind(&reminders)
    .bind(qs)
    .bind(qe)
    .bind(max_candidates)
    .bind(ctx.user_id)
    .execute(&mut **tx)
    .await?;
    let p = scheduling::load_policy(tx, ctx.tenant_id).await?;
    emit(
        tx,
        ctx,
        state,
        "scheduling.policy.updated",
        json!({ "tenant_id": ctx.tenant_id, "version": p.version }),
    )
    .await?;
    Ok(p)
}

// ---------------------------------------------------------------------------
// Facility scheduling (hours, time zone, location)
// ---------------------------------------------------------------------------

fn facility_json(r: &sqlx::postgres::PgRow, name: &str) -> Value {
    json!({
        "facility_id": r.get::<Uuid, _>("facility_id"),
        "name": name,
        "time_zone": r.get::<String, _>("time_zone"),
        "opening_hours": r.get::<Value, _>("opening_hours"),
        "has_coordinates": r.get::<Option<f64>, _>("latitude").is_some(),
        "latitude": r.get::<Option<f64>, _>("latitude"),
        "longitude": r.get::<Option<f64>, _>("longitude"),
        "service_radius_km": r.get::<Option<f64>, _>("service_radius_km"),
        "address_line": r.get::<Option<String>, _>("address_line"),
        "version": r.get::<i64, _>("version"),
        "updated_at": r.get::<DateTime<Utc>, _>("updated_at"),
    })
}

async fn facility_scheduling_row(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    facility_id: Uuid,
) -> Result<(sqlx::postgres::PgRow, String), ApiError> {
    facility_in_tenant(conn, tenant_id, facility_id).await?;
    let name: String = sqlx::query_scalar("SELECT name FROM facilities WHERE id = $1")
        .bind(facility_id)
        .fetch_one(&mut *conn)
        .await?;
    sqlx::query(
        "INSERT INTO facility_scheduling (facility_id, tenant_id) VALUES ($1,$2) ON CONFLICT (facility_id) DO NOTHING",
    )
    .bind(facility_id)
    .bind(tenant_id)
    .execute(&mut *conn)
    .await?;
    let row = sqlx::query("SELECT * FROM facility_scheduling WHERE facility_id = $1")
        .bind(facility_id)
        .fetch_one(conn)
        .await?;
    Ok((row, name))
}

pub async fn list_facility_scheduling(
    State(state): State<AppState>,
    ctx: AuthContext,
) -> Result<Json<Value>, ApiError> {
    guard(
        &state,
        &ctx,
        actions::SCHEDULING_READ,
        "facility_scheduling",
        manage_ctx(&ctx),
    )
    .await?;
    let scope = facility_scope(&ctx, actions::SCHEDULING_READ);
    let rows = sqlx::query(
        "SELECT f.id, f.name, fs.time_zone, fs.opening_hours, fs.latitude, fs.service_radius_km, fs.version
         FROM facilities f LEFT JOIN facility_scheduling fs ON fs.facility_id = f.id
         WHERE f.tenant_id = $1 AND ($2::uuid[] IS NULL OR f.id = ANY($2))
         ORDER BY f.name LIMIT 200",
    )
    .bind(ctx.tenant_id)
    .bind(&scope)
    .fetch_all(&state.pool)
    .await?;
    let items: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "facility_id": r.get::<Uuid, _>("id"),
                "name": r.get::<String, _>("name"),
                "time_zone": r.get::<Option<String>, _>("time_zone").unwrap_or_else(|| "UTC".into()),
                "opening_hours": r.get::<Option<Value>, _>("opening_hours").unwrap_or_else(|| json!([])),
                "has_coordinates": r.get::<Option<f64>, _>("latitude").is_some(),
                "service_radius_km": r.get::<Option<f64>, _>("service_radius_km"),
                "version": r.get::<Option<i64>, _>("version").unwrap_or(1),
            })
        })
        .collect();
    Ok(Json(json!({ "items": items })))
}

pub async fn get_facility_scheduling(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(facility_id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    facility_in_tenant(&mut conn, ctx.tenant_id, facility_id).await?;
    guard(
        &state,
        &ctx,
        actions::SCHEDULING_READ,
        "facility_scheduling",
        Some(ResourceCtx {
            tenant_id: ctx.tenant_id,
            patient_id: None,
            facility_id: Some(facility_id),
        }),
    )
    .await?;
    let (row, name) = facility_scheduling_row(&mut conn, ctx.tenant_id, facility_id).await?;
    Ok(Json(facility_json(&row, &name)))
}

#[derive(Deserialize)]
pub struct UpdateFacilityScheduling {
    pub version: i64,
    pub time_zone: Option<String>,
    pub opening_hours: Option<Vec<OpeningHourInput>>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub clear_coordinates: Option<bool>,
    pub service_radius_km: Option<f64>,
    pub address_line: Option<String>,
}

#[derive(Deserialize)]
pub struct OpeningHourInput {
    pub weekday: u8,
    pub open: NaiveTime,
    pub close: NaiveTime,
}

pub async fn update_facility_scheduling(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(facility_id): Path<Uuid>,
    Json(body): Json<UpdateFacilityScheduling>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    facility_in_tenant(&mut conn, ctx.tenant_id, facility_id).await?;
    drop(conn);
    let allowed = guard(
        &state,
        &ctx,
        actions::SCHEDULING_MANAGE,
        "facility_scheduling",
        Some(ResourceCtx {
            tenant_id: ctx.tenant_id,
            patient_id: None,
            facility_id: Some(facility_id),
        }),
    )
    .await?;
    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let out = update_facility_scheduling_in(&mut tx, &ctx, &state, facility_id, body).await?;
    tx.commit().await?;
    Ok(Json(out))
}

/// Apply a facility scheduling update (hours, zone, location) inside the
/// caller's transaction.
pub async fn update_facility_scheduling_in(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ctx: &AuthContext,
    state: &AppState,
    facility_id: Uuid,
    body: UpdateFacilityScheduling,
) -> Result<Value, ApiError> {
    facility_in_tenant(tx, ctx.tenant_id, facility_id).await?;
    let opening_hours = match &body.opening_hours {
        Some(h) => {
            if h.len() > 21 {
                return Err(ApiError::bad_request(
                    "validation_failed",
                    "opening_hours accepts at most 21 windows",
                ));
            }
            let mut out = Vec::new();
            for w in h {
                if !(1..=7).contains(&w.weekday) || w.close <= w.open {
                    return Err(ApiError::bad_request(
                        "validation_failed",
                        "each opening window needs weekday 1-7 and close after open",
                    ));
                }
                out.push(json!({
                    "weekday": w.weekday,
                    "open": w.open.format("%H:%M").to_string(),
                    "close": w.close.format("%H:%M").to_string(),
                }));
            }
            Some(Value::Array(out))
        }
        None => None,
    };
    let address = clean_optional(body.address_line, "address_line", MAX_TEXT)?;
    let (row, name) = facility_scheduling_row(tx, ctx.tenant_id, facility_id).await?;
    sqlx::query("SELECT facility_id FROM facility_scheduling WHERE facility_id = $1 FOR UPDATE")
        .bind(facility_id)
        .execute(&mut **tx)
        .await?;
    require_version(row.get("version"), body.version)?;
    let time_zone = match body.time_zone {
        Some(t) => validate_tz(&t)?,
        None => row.get("time_zone"),
    };
    let (lat, lon) = if body.clear_coordinates.unwrap_or(false) {
        (None, None)
    } else {
        match (body.latitude, body.longitude) {
            (None, None) => (row.get("latitude"), row.get("longitude")),
            (Some(la), Some(lo))
                if (-90.0..=90.0).contains(&la) && (-180.0..=180.0).contains(&lo) =>
            {
                (Some(la), Some(lo))
            }
            _ => {
                return Err(ApiError::bad_request(
                    "validation_failed",
                    "latitude and longitude must both be present and within range",
                ))
            }
        }
    };
    let radius = match body.service_radius_km {
        Some(r) if r > 0.0 && r <= 5000.0 => Some(r),
        Some(_) => {
            return Err(ApiError::bad_request(
                "validation_failed",
                "service_radius_km must be between 0 and 5000",
            ))
        }
        None => row.get("service_radius_km"),
    };
    let opening_hours = opening_hours.unwrap_or_else(|| row.get("opening_hours"));
    let address = address.or_else(|| row.get("address_line"));
    sqlx::query(
        "UPDATE facility_scheduling SET time_zone = $2, opening_hours = $3, latitude = $4, longitude = $5,
             service_radius_km = $6, address_line = $7, version = version + 1, updated_by = $8, updated_at = now()
         WHERE facility_id = $1",
    )
    .bind(facility_id)
    .bind(&time_zone)
    .bind(&opening_hours)
    .bind(lat)
    .bind(lon)
    .bind(radius)
    .bind(&address)
    .bind(ctx.user_id)
    .execute(&mut **tx)
    .await?;
    let row = sqlx::query("SELECT * FROM facility_scheduling WHERE facility_id = $1")
        .bind(facility_id)
        .fetch_one(&mut **tx)
        .await?;
    emit(
        tx,
        ctx,
        state,
        "scheduling.facility.updated",
        json!({ "facility_id": facility_id, "version": row.get::<i64, _>("version") }),
    )
    .await?;
    Ok(facility_json(&row, &name))
}

// ---------------------------------------------------------------------------
// Schedulable resources
// ---------------------------------------------------------------------------

fn resource_json(r: &sqlx::postgres::PgRow) -> Value {
    json!({
        "id": r.get::<Uuid, _>("id"),
        "facility_id": r.get::<Uuid, _>("facility_id"),
        "resource_type_code": r.get::<String, _>("resource_type_code"),
        "name": r.get::<String, _>("name"),
        "user_id": r.get::<Option<Uuid>, _>("user_id"),
        "profession_code": r.get::<Option<String>, _>("profession_code"),
        "specialty_codes": r.get::<Vec<String>, _>("specialty_codes"),
        "languages": r.get::<Vec<String>, _>("languages"),
        "accessibility_codes": r.get::<Vec<String>, _>("accessibility_codes"),
        "capacity": r.get::<i32, _>("capacity"),
        "time_zone": r.get::<String, _>("time_zone"),
        "active": r.get::<bool, _>("active"),
        "metadata": r.get::<Value, _>("metadata"),
        "version": r.get::<i64, _>("version"),
        "created_at": r.get::<DateTime<Utc>, _>("created_at"),
        "updated_at": r.get::<DateTime<Utc>, _>("updated_at"),
    })
}

async fn load_resource(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    id: Uuid,
    lock: bool,
) -> Result<sqlx::postgres::PgRow, ApiError> {
    let sql = if lock {
        "SELECT * FROM schedulable_resources WHERE id = $1 AND tenant_id = $2 FOR UPDATE"
    } else {
        "SELECT * FROM schedulable_resources WHERE id = $1 AND tenant_id = $2"
    };
    sqlx::query(sql)
        .bind(id)
        .bind(tenant_id)
        .fetch_optional(conn)
        .await?
        .ok_or_else(ApiError::not_found)
}

fn clean_languages(values: Vec<String>) -> Result<Vec<String>, ApiError> {
    if values.len() > MAX_LIST {
        return Err(ApiError::bad_request(
            "validation_failed",
            "languages accepts at most 30 entries",
        ));
    }
    let mut out = Vec::new();
    for v in values {
        let t = v.trim().to_lowercase();
        let ok = (2..=8).contains(&t.len())
            && t.split('-')
                .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric()));
        if !ok {
            return Err(ApiError::bad_request(
                "validation_failed",
                "languages must be BCP-47 tags such as en or es-ES",
            ));
        }
        if !out.contains(&t) {
            out.push(t);
        }
    }
    Ok(out)
}

/// A professional resource may link to a user of the same tenant. This is a
/// scheduling fact only; it never grants the user any permission.
async fn check_resource_user(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    user_id: Option<Uuid>,
) -> Result<(), ApiError> {
    let Some(uid) = user_id else { return Ok(()) };
    let ok: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM users WHERE id = $1 AND tenant_id = $2 AND NOT is_service",
    )
    .bind(uid)
    .bind(tenant_id)
    .fetch_optional(conn)
    .await?;
    ok.map(|_| ()).ok_or_else(|| {
        ApiError::bad_request("validation_failed", "user_id is not a user of this tenant")
    })
}

#[derive(Deserialize)]
pub struct ResourceQuery {
    pub facility_id: Option<Uuid>,
    pub resource_type_code: Option<String>,
    pub service_code: Option<String>,
    pub specialty_code: Option<String>,
    pub profession_code: Option<String>,
    pub include_inactive: Option<bool>,
    pub limit: Option<i64>,
    pub after: Option<Uuid>,
}

pub async fn list_resources(
    State(state): State<AppState>,
    ctx: AuthContext,
    Query(q): Query<ResourceQuery>,
) -> Result<Json<Value>, ApiError> {
    guard(
        &state,
        &ctx,
        actions::SCHEDULING_READ,
        "schedulable_resource",
        manage_ctx(&ctx),
    )
    .await?;
    let scope = facility_scope(&ctx, actions::SCHEDULING_READ);
    let rtype = q
        .resource_type_code
        .as_deref()
        .map(|c| require_code(c, "resource_type_code"))
        .transpose()?;
    let service = q
        .service_code
        .as_deref()
        .map(|c| require_code(c, "service_code"))
        .transpose()?;
    let specialty = q
        .specialty_code
        .as_deref()
        .map(|c| require_code(c, "specialty_code"))
        .transpose()?;
    let profession = q
        .profession_code
        .as_deref()
        .map(|c| require_code(c, "profession_code"))
        .transpose()?;
    let limit = limit_of(q.limit);
    let rows = sqlx::query(
        "SELECT r.* FROM schedulable_resources r
         WHERE r.tenant_id = $1
           AND ($2::uuid[] IS NULL OR r.facility_id = ANY($2))
           AND ($3::uuid IS NULL OR r.facility_id = $3)
           AND ($4::text IS NULL OR r.resource_type_code = $4)
           AND ($5::text IS NULL OR EXISTS (SELECT 1 FROM resource_services s WHERE s.resource_id = r.id AND s.service_code = $5))
           AND ($6::text IS NULL OR $6 = ANY(r.specialty_codes))
           AND ($7::text IS NULL OR r.profession_code = $7)
           AND ($8::boolean OR r.active)
           AND ($9::uuid IS NULL OR r.id > $9)
         ORDER BY r.id LIMIT $10",
    )
    .bind(ctx.tenant_id)
    .bind(&scope)
    .bind(q.facility_id)
    .bind(&rtype)
    .bind(&service)
    .bind(&specialty)
    .bind(&profession)
    .bind(q.include_inactive.unwrap_or(false))
    .bind(q.after)
    .bind(limit + 1)
    .fetch_all(&state.pool)
    .await?;
    let has_more = rows.len() as i64 > limit;
    let rows: Vec<_> = rows.into_iter().take(limit as usize).collect();
    let next = has_more
        .then(|| rows.last().map(|r| r.get::<Uuid, _>("id")))
        .flatten();
    let mut items = Vec::new();
    let mut conn = state.pool.acquire().await?;
    for r in &rows {
        let mut v = resource_json(r);
        v["services"] = json!(resource_services(&mut conn, r.get("id")).await?);
        items.push(v);
    }
    Ok(Json(json!({ "items": items, "next": next })))
}

async fn resource_services(
    conn: &mut PgConnection,
    resource_id: Uuid,
) -> Result<Vec<Value>, ApiError> {
    Ok(sqlx::query(
        "SELECT service_code, duration_minutes, prep_minutes, cleanup_minutes, modality_codes
         FROM resource_services WHERE resource_id = $1 ORDER BY service_code",
    )
    .bind(resource_id)
    .fetch_all(conn)
    .await?
    .iter()
    .map(|s| {
        json!({
            "service_code": s.get::<String, _>("service_code"),
            "duration_minutes": s.get::<Option<i32>, _>("duration_minutes"),
            "prep_minutes": s.get::<i32, _>("prep_minutes"),
            "cleanup_minutes": s.get::<i32, _>("cleanup_minutes"),
            "modality_codes": s.get::<Vec<String>, _>("modality_codes"),
        })
    })
    .collect())
}

pub async fn resource_detail(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let row = load_resource(&mut conn, ctx.tenant_id, id, false).await?;
    guard(
        &state,
        &ctx,
        actions::SCHEDULING_READ,
        "schedulable_resource",
        Some(ResourceCtx {
            tenant_id: ctx.tenant_id,
            patient_id: None,
            facility_id: Some(row.get("facility_id")),
        }),
    )
    .await?;
    let mut out = resource_json(&row);
    out["services"] = json!(resource_services(&mut conn, id).await?);
    let rules: Vec<Value> = sqlx::query(
        "SELECT id, weekday, start_local, end_local, kind, capacity, effective_from, effective_to
         FROM resource_availability_rules WHERE resource_id = $1 ORDER BY weekday, start_local LIMIT 200",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await?
    .iter()
    .map(rule_json)
    .collect();
    out["availability_rules"] = json!(rules);
    let exceptions: Vec<Value> = sqlx::query(
        "SELECT id, kind, starts_at, ends_at, capacity_delta, reason_code, created_at
         FROM resource_exceptions WHERE resource_id = $1 AND ends_at >= now() - interval '30 days'
         ORDER BY starts_at LIMIT 200",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await?
    .iter()
    .map(exception_json)
    .collect();
    out["exceptions"] = json!(exceptions);
    Ok(Json(out))
}

#[derive(Deserialize)]
pub struct CreateResource {
    pub facility_id: Uuid,
    pub resource_type_code: String,
    pub name: String,
    pub user_id: Option<Uuid>,
    pub profession_code: Option<String>,
    #[serde(default)]
    pub specialty_codes: Vec<String>,
    #[serde(default)]
    pub languages: Vec<String>,
    #[serde(default)]
    pub accessibility_codes: Vec<String>,
    pub capacity: Option<i32>,
    pub time_zone: Option<String>,
    pub metadata: Option<Value>,
    #[serde(default)]
    pub services: Vec<ResourceServiceInput>,
}

#[derive(Deserialize)]
pub struct ResourceServiceInput {
    pub service_code: String,
    pub duration_minutes: Option<i32>,
    pub prep_minutes: Option<i32>,
    pub cleanup_minutes: Option<i32>,
    #[serde(default)]
    pub modality_codes: Vec<String>,
}

async fn validate_resource_codes(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    resource_type: &str,
    profession: Option<&str>,
    specialties: &[String],
    accessibility: &[String],
) -> Result<(), ApiError> {
    scheduling::require_codes(
        conn,
        tenant_id,
        "resource_type",
        &[resource_type.to_string()],
        "resource_type_code",
    )
    .await?;
    if let Some(p) = profession {
        scheduling::require_codes(
            conn,
            tenant_id,
            "profession",
            &[p.to_string()],
            "profession_code",
        )
        .await?;
    }
    scheduling::require_codes(conn, tenant_id, "specialty", specialties, "specialty_codes").await?;
    scheduling::require_codes(
        conn,
        tenant_id,
        "accessibility_capability",
        accessibility,
        "accessibility_codes",
    )
    .await?;
    Ok(())
}

async fn replace_resource_services(
    tx: &mut Transaction<'_, sqlx::Postgres>,
    tenant_id: Uuid,
    resource_id: Uuid,
    services: Vec<ResourceServiceInput>,
) -> Result<(), ApiError> {
    if services.len() > MAX_LIST {
        return Err(ApiError::bad_request(
            "validation_failed",
            format!("services accepts at most {MAX_LIST} entries"),
        ));
    }
    sqlx::query("DELETE FROM resource_services WHERE resource_id = $1")
        .bind(resource_id)
        .execute(&mut **tx)
        .await?;
    for s in services {
        let code = require_code(&s.service_code, "services.service_code")?;
        scheduling::load_service(tx, tenant_id, &code).await?;
        let modalities = clean_codes(s.modality_codes, "services.modality_codes")?;
        scheduling::require_codes(
            tx,
            tenant_id,
            "modality",
            &modalities,
            "services.modality_codes",
        )
        .await?;
        let duration = s
            .duration_minutes
            .map(|d| range(d, 5, 720, "services.duration_minutes"))
            .transpose()?;
        let prep = range(s.prep_minutes.unwrap_or(0), 0, 240, "services.prep_minutes")?;
        let cleanup = range(
            s.cleanup_minutes.unwrap_or(0),
            0,
            240,
            "services.cleanup_minutes",
        )?;
        sqlx::query(
            "INSERT INTO resource_services (tenant_id, resource_id, service_code, duration_minutes, prep_minutes,
                 cleanup_minutes, modality_codes)
             VALUES ($1,$2,$3,$4,$5,$6,$7)
             ON CONFLICT (resource_id, service_code) DO UPDATE SET duration_minutes = EXCLUDED.duration_minutes,
                 prep_minutes = EXCLUDED.prep_minutes, cleanup_minutes = EXCLUDED.cleanup_minutes,
                 modality_codes = EXCLUDED.modality_codes",
        )
        .bind(tenant_id)
        .bind(resource_id)
        .bind(&code)
        .bind(duration)
        .bind(prep)
        .bind(cleanup)
        .bind(&modalities)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

fn clean_metadata(m: Option<Value>) -> Result<Value, ApiError> {
    let m = m.unwrap_or_else(|| json!({}));
    if !m.is_object() || m.to_string().len() > MAX_CONFIG_BYTES {
        return Err(ApiError::bad_request(
            "validation_failed",
            "metadata must be a small JSON object",
        ));
    }
    Ok(m)
}

pub async fn create_resource(
    State(state): State<AppState>,
    ctx: AuthContext,
    Json(body): Json<CreateResource>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    facility_in_tenant(&mut conn, ctx.tenant_id, body.facility_id).await?;
    drop(conn);
    let allowed = guard(
        &state,
        &ctx,
        actions::RESOURCE_MANAGE,
        "schedulable_resource",
        Some(ResourceCtx {
            tenant_id: ctx.tenant_id,
            patient_id: None,
            facility_id: Some(body.facility_id),
        }),
    )
    .await?;
    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let out = create_resource_in(&mut tx, &ctx, &state, body).await?;
    tx.commit().await?;
    Ok(Json(out))
}

/// Create a schedulable resource and its service mappings inside the
/// caller's transaction. Returns the resource JSON (with `id`).
pub async fn create_resource_in(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ctx: &AuthContext,
    state: &AppState,
    body: CreateResource,
) -> Result<Value, ApiError> {
    facility_in_tenant(tx, ctx.tenant_id, body.facility_id).await?;
    let rtype = require_code(&body.resource_type_code, "resource_type_code")?;
    let name = clean_required(&body.name, "name", MAX_NAME)?;
    let profession = body
        .profession_code
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|p| require_code(p, "profession_code"))
        .transpose()?;
    let specialties = clean_codes(body.specialty_codes, "specialty_codes")?;
    let accessibility = clean_codes(body.accessibility_codes, "accessibility_codes")?;
    let languages = clean_languages(body.languages)?;
    let capacity = range(body.capacity.unwrap_or(1), 1, 500, "capacity")?;
    let metadata = clean_metadata(body.metadata)?;
    validate_resource_codes(
        tx,
        ctx.tenant_id,
        &rtype,
        profession.as_deref(),
        &specialties,
        &accessibility,
    )
    .await?;
    check_resource_user(tx, ctx.tenant_id, body.user_id).await?;
    let time_zone = match body.time_zone {
        Some(t) => validate_tz(&t)?,
        None => {
            sqlx::query_scalar("SELECT time_zone FROM facility_scheduling WHERE facility_id = $1")
                .bind(body.facility_id)
                .fetch_optional(&mut **tx)
                .await?
                .unwrap_or_else(|| "UTC".to_string())
        }
    };
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO schedulable_resources (id, tenant_id, facility_id, resource_type_code, name, user_id,
             profession_code, specialty_codes, languages, accessibility_codes, capacity, time_zone, metadata, created_by)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)",
    )
    .bind(id)
    .bind(ctx.tenant_id)
    .bind(body.facility_id)
    .bind(&rtype)
    .bind(&name)
    .bind(body.user_id)
    .bind(&profession)
    .bind(&specialties)
    .bind(&languages)
    .bind(&accessibility)
    .bind(capacity)
    .bind(&time_zone)
    .bind(&metadata)
    .bind(ctx.user_id)
    .execute(&mut **tx)
    .await?;
    replace_resource_services(tx, ctx.tenant_id, id, body.services).await?;
    emit(
        tx,
        ctx,
        state,
        "scheduling.resource.created",
        json!({ "resource_id": id, "facility_id": body.facility_id, "resource_type_code": rtype }),
    )
    .await?;
    let row = load_resource(tx, ctx.tenant_id, id, false).await?;
    let mut out = resource_json(&row);
    out["services"] = json!(resource_services(tx, id).await?);
    Ok(out)
}

#[derive(Deserialize)]
pub struct UpdateResource {
    pub version: i64,
    pub name: Option<String>,
    pub user_id: Option<Uuid>,
    pub clear_user: Option<bool>,
    pub profession_code: Option<String>,
    pub specialty_codes: Option<Vec<String>>,
    pub languages: Option<Vec<String>>,
    pub accessibility_codes: Option<Vec<String>>,
    pub capacity: Option<i32>,
    pub time_zone: Option<String>,
    pub metadata: Option<Value>,
    pub services: Option<Vec<ResourceServiceInput>>,
    pub active: Option<bool>,
    pub reason: Option<String>,
}

pub async fn update_resource(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateResource>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let existing = load_resource(&mut conn, ctx.tenant_id, id, false).await?;
    drop(conn);
    let facility_id: Uuid = existing.get("facility_id");
    let allowed = guard(
        &state,
        &ctx,
        actions::RESOURCE_MANAGE,
        "schedulable_resource",
        Some(ResourceCtx {
            tenant_id: ctx.tenant_id,
            patient_id: None,
            facility_id: Some(facility_id),
        }),
    )
    .await?;
    let reason = clean_optional(body.reason, "reason", MAX_TEXT)?;
    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let row = load_resource(&mut tx, ctx.tenant_id, id, true).await?;
    require_version(row.get("version"), body.version)?;
    let name = match body.name {
        Some(n) => clean_required(&n, "name", MAX_NAME)?,
        None => row.get("name"),
    };
    let user_id = if body.clear_user.unwrap_or(false) {
        None
    } else {
        body.user_id.or_else(|| row.get("user_id"))
    };
    check_resource_user(&mut tx, ctx.tenant_id, user_id).await?;
    let profession = match body.profession_code {
        Some(p) if p.trim().is_empty() => None,
        Some(p) => Some(require_code(&p, "profession_code")?),
        None => row.get("profession_code"),
    };
    let specialties = match body.specialty_codes {
        Some(s) => clean_codes(s, "specialty_codes")?,
        None => row.get("specialty_codes"),
    };
    let accessibility = match body.accessibility_codes {
        Some(s) => clean_codes(s, "accessibility_codes")?,
        None => row.get("accessibility_codes"),
    };
    let languages = match body.languages {
        Some(l) => clean_languages(l)?,
        None => row.get("languages"),
    };
    let capacity = range(
        body.capacity.unwrap_or_else(|| row.get("capacity")),
        1,
        500,
        "capacity",
    )?;
    let time_zone = match body.time_zone {
        Some(t) => validate_tz(&t)?,
        None => row.get("time_zone"),
    };
    let metadata = match body.metadata {
        Some(m) => clean_metadata(Some(m))?,
        None => row.get("metadata"),
    };
    let active = body.active.unwrap_or_else(|| row.get("active"));
    let was_active: bool = row.get("active");
    if !active && was_active && reason.is_none() {
        return Err(ApiError::bad_request(
            "validation_failed",
            "reason is required to deactivate a resource",
        ));
    }
    validate_resource_codes(
        &mut tx,
        ctx.tenant_id,
        &row.get::<String, _>("resource_type_code"),
        profession.as_deref(),
        &specialties,
        &accessibility,
    )
    .await?;
    sqlx::query(
        "UPDATE schedulable_resources SET name = $2, user_id = $3, profession_code = $4, specialty_codes = $5,
             languages = $6, accessibility_codes = $7, capacity = $8, time_zone = $9, metadata = $10, active = $11,
             version = version + 1, updated_at = now()
         WHERE id = $1",
    )
    .bind(id)
    .bind(&name)
    .bind(user_id)
    .bind(&profession)
    .bind(&specialties)
    .bind(&languages)
    .bind(&accessibility)
    .bind(capacity)
    .bind(&time_zone)
    .bind(&metadata)
    .bind(active)
    .execute(&mut *tx)
    .await?;
    if let Some(s) = body.services {
        replace_resource_services(&mut tx, ctx.tenant_id, id, s).await?;
    }
    if !active && was_active {
        // Future holds on a deactivated resource are released; confirmed
        // appointments are never touched silently — staff reschedule them.
        let released = sqlx::query(
            "UPDATE resource_bookings SET status = 'released', released_at = now()
             WHERE resource_id = $1 AND status = 'active' AND kind = 'hold' RETURNING offer_id",
        )
        .bind(id)
        .fetch_all(&mut *tx)
        .await?;
        for r in &released {
            if let Some(offer_id) = r.get::<Option<Uuid>, _>("offer_id") {
                scheduling::revoke_offer_for_resource(
                    &mut tx,
                    &ctx,
                    &state,
                    offer_id,
                    "resource_deactivated",
                )
                .await?;
            }
        }
    }
    let event = if !active && was_active {
        "scheduling.resource.deactivated"
    } else {
        "scheduling.resource.updated"
    };
    let row = load_resource(&mut tx, ctx.tenant_id, id, false).await?;
    emit(
        &mut tx,
        &ctx,
        &state,
        event,
        json!({ "resource_id": id, "facility_id": facility_id, "version": row.get::<i64, _>("version"),
                "reason": reason }),
    )
    .await?;
    let mut out = resource_json(&row);
    out["services"] = json!(resource_services(&mut tx, id).await?);
    tx.commit().await?;
    Ok(Json(out))
}

// ---------------------------------------------------------------------------
// Availability rules and exceptions
// ---------------------------------------------------------------------------

fn rule_json(r: &sqlx::postgres::PgRow) -> Value {
    json!({
        "id": r.get::<Uuid, _>("id"),
        "weekday": r.get::<i16, _>("weekday"),
        "start_local": r.get::<NaiveTime, _>("start_local").format("%H:%M").to_string(),
        "end_local": r.get::<NaiveTime, _>("end_local").format("%H:%M").to_string(),
        "kind": r.get::<String, _>("kind"),
        "capacity": r.get::<Option<i32>, _>("capacity"),
        "effective_from": r.get::<Option<NaiveDate>, _>("effective_from"),
        "effective_to": r.get::<Option<NaiveDate>, _>("effective_to"),
    })
}

fn exception_json(r: &sqlx::postgres::PgRow) -> Value {
    json!({
        "id": r.get::<Uuid, _>("id"),
        "kind": r.get::<String, _>("kind"),
        "starts_at": r.get::<DateTime<Utc>, _>("starts_at"),
        "ends_at": r.get::<DateTime<Utc>, _>("ends_at"),
        "capacity_delta": r.get::<i32, _>("capacity_delta"),
        "reason_code": r.get::<Option<String>, _>("reason_code"),
        "created_at": r.get::<DateTime<Utc>, _>("created_at"),
    })
}

async fn resource_manage_guard(
    state: &AppState,
    ctx: &AuthContext,
    resource_id: Uuid,
) -> Result<(crate::routes::Allowed, Uuid, i64), ApiError> {
    let mut conn = state.pool.acquire().await?;
    let row = load_resource(&mut conn, ctx.tenant_id, resource_id, false).await?;
    let facility_id: Uuid = row.get("facility_id");
    let allowed = guard(
        state,
        ctx,
        actions::RESOURCE_MANAGE,
        "schedulable_resource",
        Some(ResourceCtx {
            tenant_id: ctx.tenant_id,
            patient_id: None,
            facility_id: Some(facility_id),
        }),
    )
    .await?;
    Ok((allowed, facility_id, row.get("version")))
}

#[derive(Deserialize)]
pub struct AvailabilityRuleInput {
    pub weekday: i16,
    pub start_local: NaiveTime,
    pub end_local: NaiveTime,
    pub kind: Option<String>,
    pub capacity: Option<i32>,
    pub effective_from: Option<NaiveDate>,
    pub effective_to: Option<NaiveDate>,
}

#[derive(Deserialize)]
pub struct ReplaceAvailability {
    pub version: i64,
    pub rules: Vec<AvailabilityRuleInput>,
}

/// Replace the weekly availability of a resource atomically (rules are
/// small, declarative and read as a set by the matcher). Bumps the
/// resource version so concurrent editors are detected.
pub async fn replace_availability(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<ReplaceAvailability>,
) -> Result<Json<Value>, ApiError> {
    let (allowed, _, _) = resource_manage_guard(&state, &ctx, id).await?;
    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let out = replace_availability_in(&mut tx, &ctx, &state, id, body).await?;
    tx.commit().await?;
    Ok(Json(out))
}

/// Replace a resource's weekly availability inside the caller's transaction.
pub async fn replace_availability_in(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ctx: &AuthContext,
    state: &AppState,
    id: Uuid,
    body: ReplaceAvailability,
) -> Result<Value, ApiError> {
    if body.rules.len() > 70 {
        return Err(ApiError::bad_request(
            "validation_failed",
            "rules accepts at most 70 windows",
        ));
    }
    for r in &body.rules {
        if !(1..=7).contains(&r.weekday) {
            return Err(ApiError::bad_request(
                "validation_failed",
                "weekday must be 1-7",
            ));
        }
        if r.end_local <= r.start_local {
            return Err(ApiError::bad_request(
                "validation_failed",
                "end_local must be after start_local",
            ));
        }
        if let Some(k) = &r.kind {
            if !matches!(k.as_str(), "available" | "break") {
                return Err(ApiError::bad_request(
                    "validation_failed",
                    "kind must be available or break",
                ));
            }
        }
        if let Some(c) = r.capacity {
            range(c, 1, 500, "capacity")?;
        }
        validate_dates(r.effective_from, r.effective_to)?;
    }
    let row = load_resource(tx, ctx.tenant_id, id, true).await?;
    let facility_id: Uuid = row.get("facility_id");
    require_version(row.get("version"), body.version)?;
    sqlx::query("DELETE FROM resource_availability_rules WHERE resource_id = $1")
        .bind(id)
        .execute(&mut **tx)
        .await?;
    for r in &body.rules {
        sqlx::query(
            "INSERT INTO resource_availability_rules (id, tenant_id, resource_id, weekday, start_local, end_local,
                 kind, capacity, effective_from, effective_to, created_by)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
        )
        .bind(Uuid::now_v7())
        .bind(ctx.tenant_id)
        .bind(id)
        .bind(r.weekday)
        .bind(r.start_local)
        .bind(r.end_local)
        .bind(r.kind.as_deref().unwrap_or("available"))
        .bind(r.capacity)
        .bind(r.effective_from)
        .bind(r.effective_to)
        .bind(ctx.user_id)
        .execute(&mut **tx)
        .await?;
    }
    sqlx::query(
        "UPDATE schedulable_resources SET version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .execute(&mut **tx)
    .await?;
    emit(
        tx,
        ctx,
        state,
        "scheduling.resource.availability_replaced",
        json!({ "resource_id": id, "facility_id": facility_id, "rules": body.rules.len() }),
    )
    .await?;
    let rules: Vec<Value> = sqlx::query(
        "SELECT id, weekday, start_local, end_local, kind, capacity, effective_from, effective_to
         FROM resource_availability_rules WHERE resource_id = $1 ORDER BY weekday, start_local",
    )
    .bind(id)
    .fetch_all(&mut **tx)
    .await?
    .iter()
    .map(rule_json)
    .collect();
    let version: i64 =
        sqlx::query_scalar("SELECT version FROM schedulable_resources WHERE id = $1")
            .bind(id)
            .fetch_one(&mut **tx)
            .await?;
    Ok(json!({ "resource_id": id, "version": version, "rules": rules }))
}

#[derive(Deserialize)]
pub struct CreateException {
    pub kind: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub capacity_delta: Option<i32>,
    pub reason_code: Option<String>,
}

const EXCEPTION_KINDS: &[&str] = &[
    "leave",
    "sickness",
    "blocked",
    "closure",
    "extra_capacity",
    "training",
];

/// Dated exceptions (leave, sickness, closures, temporary extra capacity).
/// Live holds inside a capacity-reducing exception are released and their
/// offers revoked; confirmed appointments are reported for staff follow-up,
/// never cancelled automatically.
pub async fn create_exception(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<CreateException>,
) -> Result<Json<Value>, ApiError> {
    let (allowed, _, _) = resource_manage_guard(&state, &ctx, id).await?;
    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let out = create_exception_in(&mut tx, &ctx, &state, id, body).await?;
    tx.commit().await?;
    Ok(Json(out))
}

/// Record a dated resource exception inside the caller's transaction.
pub async fn create_exception_in(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ctx: &AuthContext,
    state: &AppState,
    id: Uuid,
    body: CreateException,
) -> Result<Value, ApiError> {
    let kind = body.kind.trim();
    if !EXCEPTION_KINDS.contains(&kind) {
        return Err(ApiError::bad_request(
            "validation_failed",
            format!("kind must be one of {}", EXCEPTION_KINDS.join(", ")),
        ));
    }
    if body.ends_at <= body.starts_at || body.ends_at - body.starts_at > chrono::Duration::days(400)
    {
        return Err(ApiError::bad_request(
            "validation_failed",
            "ends_at must be after starts_at and within 400 days",
        ));
    }
    let delta = match kind {
        "extra_capacity" => range(body.capacity_delta.unwrap_or(1), 1, 500, "capacity_delta")?,
        _ => 0,
    };
    let reason_code = body
        .reason_code
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| require_code(s, "reason_code"))
        .transpose()?;
    let resource = load_resource(tx, ctx.tenant_id, id, true).await?;
    let facility_id: Uuid = resource.get("facility_id");
    let ex_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO resource_exceptions (id, tenant_id, resource_id, kind, starts_at, ends_at, capacity_delta,
             reason_code, created_by)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)",
    )
    .bind(ex_id)
    .bind(ctx.tenant_id)
    .bind(id)
    .bind(kind)
    .bind(body.starts_at)
    .bind(body.ends_at)
    .bind(delta)
    .bind(&reason_code)
    .bind(ctx.user_id)
    .execute(&mut **tx)
    .await?;
    let mut affected_appointments: Vec<Uuid> = Vec::new();
    if kind != "extra_capacity" {
        let released = sqlx::query(
            "UPDATE resource_bookings SET status = 'released', released_at = now()
             WHERE resource_id = $1 AND status = 'active' AND kind = 'hold'
               AND tstzrange(starts_at, ends_at, '[)') && tstzrange($2, $3, '[)')
             RETURNING offer_id",
        )
        .bind(id)
        .bind(body.starts_at)
        .bind(body.ends_at)
        .fetch_all(&mut **tx)
        .await?;
        for r in &released {
            if let Some(offer_id) = r.get::<Option<Uuid>, _>("offer_id") {
                scheduling::revoke_offer_for_resource(
                    tx,
                    ctx,
                    state,
                    offer_id,
                    "resource_exception",
                )
                .await?;
            }
        }
        affected_appointments = sqlx::query_scalar(
            "SELECT DISTINCT b.appointment_id FROM resource_bookings b
             WHERE b.resource_id = $1 AND b.status = 'active' AND b.kind = 'appointment'
               AND b.appointment_id IS NOT NULL
               AND tstzrange(b.starts_at, b.ends_at, '[)') && tstzrange($2, $3, '[)')
             LIMIT 200",
        )
        .bind(id)
        .bind(body.starts_at)
        .bind(body.ends_at)
        .fetch_all(&mut **tx)
        .await?;
    }
    sqlx::query(
        "UPDATE schedulable_resources SET version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .execute(&mut **tx)
    .await?;
    emit(
        tx,
        ctx,
        state,
        "scheduling.resource.exception_recorded",
        json!({ "resource_id": id, "facility_id": facility_id, "exception_id": ex_id, "kind": kind,
                "affected_appointments": affected_appointments.len() }),
    )
    .await?;
    let row = sqlx::query("SELECT * FROM resource_exceptions WHERE id = $1")
        .bind(ex_id)
        .fetch_one(&mut **tx)
        .await?;
    let mut out = exception_json(&row);
    out["affected_appointment_ids"] = json!(affected_appointments);
    Ok(out)
}

pub async fn delete_exception(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path((id, exception_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<Value>, ApiError> {
    let (allowed, facility_id, _) = resource_manage_guard(&state, &ctx, id).await?;
    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    load_resource(&mut tx, ctx.tenant_id, id, true).await?;
    let deleted = sqlx::query(
        "DELETE FROM resource_exceptions WHERE id = $1 AND resource_id = $2 AND tenant_id = $3 RETURNING kind",
    )
    .bind(exception_id)
    .bind(id)
    .bind(ctx.tenant_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(ApiError::not_found)?;
    sqlx::query(
        "UPDATE schedulable_resources SET version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    emit(
        &mut tx,
        &ctx,
        &state,
        "scheduling.resource.exception_removed",
        json!({ "resource_id": id, "facility_id": facility_id, "exception_id": exception_id,
                "kind": deleted.get::<String, _>("kind") }),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(
        json!({ "resource_id": id, "exception_id": exception_id, "removed": true }),
    ))
}

// ---------------------------------------------------------------------------
// Service resource requirements
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct RequirementInput {
    pub resource_type_code: String,
    pub quantity: Option<i32>,
}

#[derive(Deserialize)]
pub struct ReplaceRequirements {
    pub requirements: Vec<RequirementInput>,
}

pub async fn get_requirements(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(service_code): Path<String>,
) -> Result<Json<Value>, ApiError> {
    guard(
        &state,
        &ctx,
        actions::CATALOG_READ,
        "service_requirement",
        manage_ctx(&ctx),
    )
    .await?;
    let code = require_code(&service_code, "service_code")?;
    let rows = sqlx::query(
        "SELECT resource_type_code, quantity FROM service_resource_requirements
         WHERE tenant_id = $1 AND service_code = $2 ORDER BY resource_type_code",
    )
    .bind(ctx.tenant_id)
    .bind(&code)
    .fetch_all(&state.pool)
    .await?;
    let items: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "resource_type_code": r.get::<String, _>("resource_type_code"),
                "quantity": r.get::<i32, _>("quantity"),
            })
        })
        .collect();
    Ok(Json(json!({ "service_code": code, "requirements": items })))
}

pub async fn replace_requirements(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(service_code): Path<String>,
    Json(body): Json<ReplaceRequirements>,
) -> Result<Json<Value>, ApiError> {
    let allowed = guard(
        &state,
        &ctx,
        actions::CATALOG_MANAGE,
        "service_requirement",
        manage_ctx(&ctx),
    )
    .await?;
    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let out = replace_requirements_in(&mut tx, &ctx, &state, &service_code, body).await?;
    tx.commit().await?;
    Ok(Json(out))
}

/// Replace the required resource combination of a service inside the
/// caller's transaction.
pub async fn replace_requirements_in(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ctx: &AuthContext,
    state: &AppState,
    service_code: &str,
    body: ReplaceRequirements,
) -> Result<Value, ApiError> {
    let code = require_code(service_code, "service_code")?;
    if body.requirements.len() > 10 {
        return Err(ApiError::bad_request(
            "validation_failed",
            "requirements accepts at most 10 resource types",
        ));
    }
    scheduling::load_service(tx, ctx.tenant_id, &code).await?;
    sqlx::query(
        "DELETE FROM service_resource_requirements WHERE tenant_id = $1 AND service_code = $2",
    )
    .bind(ctx.tenant_id)
    .bind(&code)
    .execute(&mut **tx)
    .await?;
    let mut out = Vec::new();
    for r in &body.requirements {
        let rtype = require_code(&r.resource_type_code, "requirements.resource_type_code")?;
        scheduling::require_codes(
            tx,
            ctx.tenant_id,
            "resource_type",
            std::slice::from_ref(&rtype),
            "requirements.resource_type_code",
        )
        .await?;
        let qty = range(r.quantity.unwrap_or(1), 1, 10, "requirements.quantity")?;
        sqlx::query(
            "INSERT INTO service_resource_requirements (tenant_id, service_code, resource_type_code, quantity)
             VALUES ($1,$2,$3,$4)
             ON CONFLICT (tenant_id, service_code, resource_type_code) DO UPDATE SET quantity = EXCLUDED.quantity",
        )
        .bind(ctx.tenant_id)
        .bind(&code)
        .bind(&rtype)
        .bind(qty)
        .execute(&mut **tx)
        .await?;
        out.push(json!({ "resource_type_code": rtype, "quantity": qty }));
    }
    emit(
        tx,
        ctx,
        state,
        "catalog.service_requirements.replaced",
        json!({ "service_code": code, "count": out.len() }),
    )
    .await?;
    Ok(json!({ "service_code": code, "requirements": out }))
}

// ---------------------------------------------------------------------------
// Operational calendar
// ---------------------------------------------------------------------------

const CALENDAR_KINDS: &[&str] = &[
    "holiday",
    "school_break",
    "local_event",
    "seasonal_period",
    "closure",
];

fn calendar_json(r: &sqlx::postgres::PgRow) -> Value {
    json!({
        "id": r.get::<Uuid, _>("id"),
        "facility_id": r.get::<Option<Uuid>, _>("facility_id"),
        "kind": r.get::<String, _>("kind"),
        "name": r.get::<String, _>("name"),
        "starts_on": r.get::<NaiveDate, _>("starts_on"),
        "ends_on": r.get::<NaiveDate, _>("ends_on"),
        "demand_multiplier": r.get::<rust_decimal::Decimal, _>("demand_multiplier").to_string(),
        "capacity_multiplier": r.get::<rust_decimal::Decimal, _>("capacity_multiplier").to_string(),
        "active": r.get::<bool, _>("active"),
        "created_at": r.get::<DateTime<Utc>, _>("created_at"),
    })
}

#[derive(Deserialize)]
pub struct CalendarQuery {
    pub facility_id: Option<Uuid>,
    pub from: Option<NaiveDate>,
    pub to: Option<NaiveDate>,
    pub include_inactive: Option<bool>,
    pub limit: Option<i64>,
}

pub async fn list_calendar(
    State(state): State<AppState>,
    ctx: AuthContext,
    Query(q): Query<CalendarQuery>,
) -> Result<Json<Value>, ApiError> {
    guard(
        &state,
        &ctx,
        actions::SCHEDULING_READ,
        "operational_calendar_event",
        manage_ctx(&ctx),
    )
    .await?;
    let today = Utc::now().date_naive();
    let from = q.from.unwrap_or(today - chrono::Duration::days(30));
    let to = q.to.unwrap_or(today + chrono::Duration::days(365));
    if to < from || (to - from).num_days() > 800 {
        return Err(ApiError::bad_request(
            "validation_failed",
            "the date range must be ordered and at most 800 days",
        ));
    }
    let rows = sqlx::query(
        "SELECT * FROM operational_calendar_events
         WHERE tenant_id = $1
           AND ($2::uuid IS NULL OR facility_id IS NULL OR facility_id = $2)
           AND ends_on >= $3 AND starts_on <= $4
           AND ($5::boolean OR active)
         ORDER BY starts_on, name LIMIT $6",
    )
    .bind(ctx.tenant_id)
    .bind(q.facility_id)
    .bind(from)
    .bind(to)
    .bind(q.include_inactive.unwrap_or(false))
    .bind(limit_of(q.limit))
    .fetch_all(&state.pool)
    .await?;
    let items: Vec<Value> = rows.iter().map(calendar_json).collect();
    Ok(Json(json!({ "items": items, "kinds": CALENDAR_KINDS })))
}

#[derive(Deserialize)]
pub struct CreateCalendarEvent {
    pub facility_id: Option<Uuid>,
    pub kind: String,
    pub name: String,
    pub starts_on: NaiveDate,
    pub ends_on: NaiveDate,
    pub demand_multiplier: Option<f64>,
    pub capacity_multiplier: Option<f64>,
}

fn multiplier(
    v: Option<f64>,
    default: f64,
    field: &str,
) -> Result<rust_decimal::Decimal, ApiError> {
    let v = v.unwrap_or(default);
    if !(0.0..=10.0).contains(&v) || !v.is_finite() {
        return Err(ApiError::bad_request(
            "validation_failed",
            format!("{field} must be between 0 and 10"),
        ));
    }
    rust_decimal::Decimal::from_f64_retain(v)
        .map(|d| d.round_dp(2))
        .ok_or_else(|| ApiError::bad_request("validation_failed", format!("{field} is invalid")))
}

pub async fn create_calendar_event(
    State(state): State<AppState>,
    ctx: AuthContext,
    Json(body): Json<CreateCalendarEvent>,
) -> Result<Json<Value>, ApiError> {
    if let Some(f) = body.facility_id {
        let mut conn = state.pool.acquire().await?;
        facility_in_tenant(&mut conn, ctx.tenant_id, f).await?;
    }
    let allowed = guard(
        &state,
        &ctx,
        actions::SCHEDULING_MANAGE,
        "operational_calendar_event",
        Some(ResourceCtx {
            tenant_id: ctx.tenant_id,
            patient_id: None,
            facility_id: body.facility_id,
        }),
    )
    .await?;
    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let out = create_calendar_event_in(&mut tx, &ctx, &state, body).await?;
    tx.commit().await?;
    Ok(Json(out))
}

/// Record an operational calendar event inside the caller's transaction.
pub async fn create_calendar_event_in(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ctx: &AuthContext,
    state: &AppState,
    body: CreateCalendarEvent,
) -> Result<Value, ApiError> {
    if let Some(f) = body.facility_id {
        facility_in_tenant(tx, ctx.tenant_id, f).await?;
    }
    let kind = body.kind.trim();
    if !CALENDAR_KINDS.contains(&kind) {
        return Err(ApiError::bad_request(
            "validation_failed",
            format!("kind must be one of {}", CALENDAR_KINDS.join(", ")),
        ));
    }
    let name = clean_required(&body.name, "name", MAX_NAME)?;
    if body.ends_on < body.starts_on || (body.ends_on - body.starts_on).num_days() > 400 {
        return Err(ApiError::bad_request(
            "validation_failed",
            "ends_on must be on or after starts_on and within 400 days",
        ));
    }
    let demand = multiplier(body.demand_multiplier, 1.0, "demand_multiplier")?;
    let capacity = multiplier(
        body.capacity_multiplier,
        if kind == "closure" { 0.0 } else { 1.0 },
        "capacity_multiplier",
    )?;
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO operational_calendar_events (id, tenant_id, facility_id, kind, name, starts_on, ends_on,
             demand_multiplier, capacity_multiplier, created_by)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
    )
    .bind(id)
    .bind(ctx.tenant_id)
    .bind(body.facility_id)
    .bind(kind)
    .bind(&name)
    .bind(body.starts_on)
    .bind(body.ends_on)
    .bind(demand)
    .bind(capacity)
    .bind(ctx.user_id)
    .execute(&mut **tx)
    .await?;
    // A closure is a hard constraint: live offers and holds inside it are
    // revoked now; confirmed appointments are kept and surfaced to staff.
    let mut revoked_offers: Vec<Uuid> = Vec::new();
    let mut conflicting: Vec<Uuid> = Vec::new();
    if kind == "closure" {
        let policy = scheduling::load_policy(tx, ctx.tenant_id).await?;
        revoked_offers = scheduling::offers_in_closure(
            tx,
            ctx.tenant_id,
            body.facility_id,
            body.starts_on,
            body.ends_on,
            &policy.time_zone,
        )
        .await?;
        for offer_id in &revoked_offers {
            scheduling::revoke_offer_for_resource(tx, ctx, state, *offer_id, "facility_closed")
                .await?;
        }
        conflicting = scheduling::appointments_in_closure(
            tx,
            ctx.tenant_id,
            body.facility_id,
            body.starts_on,
            body.ends_on,
            &policy.time_zone,
        )
        .await?
        .into_iter()
        .map(|a| a.id)
        .collect();
    }
    emit(
        tx,
        ctx,
        state,
        "scheduling.calendar_event.recorded",
        json!({ "event_id": id, "kind": kind, "facility_id": body.facility_id,
                "revoked_offers": revoked_offers.len(),
                "conflicting_appointments": conflicting.len() }),
    )
    .await?;
    let row = sqlx::query("SELECT * FROM operational_calendar_events WHERE id = $1")
        .bind(id)
        .fetch_one(&mut **tx)
        .await?;
    let mut out = calendar_json(&row);
    if kind == "closure" {
        out["revoked_offer_ids"] = json!(revoked_offers);
        out["conflicting_appointment_ids"] = json!(conflicting);
    }
    Ok(out)
}

/// Confirmed appointments that fall inside an active closure. The closure
/// never cancels them: each one is a scheduling conflict for a human to
/// resolve (reschedule, cancel with a reason, or deactivate the closure).
pub async fn closure_conflicts(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let row =
        sqlx::query("SELECT * FROM operational_calendar_events WHERE id = $1 AND tenant_id = $2")
            .bind(id)
            .bind(ctx.tenant_id)
            .fetch_optional(&mut *conn)
            .await?
            .ok_or_else(ApiError::not_found)?;
    let facility_id: Option<Uuid> = row.get("facility_id");
    let allowed = guard(
        &state,
        &ctx,
        actions::SCHEDULING_READ,
        "operational_calendar_event",
        Some(ResourceCtx {
            tenant_id: ctx.tenant_id,
            patient_id: None,
            facility_id,
        }),
    )
    .await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    let kind: String = row.get("kind");
    let active: bool = row.get("active");
    let mut items: Vec<Value> = Vec::new();
    if kind == "closure" && active {
        let policy = scheduling::load_policy(&mut conn, ctx.tenant_id).await?;
        items = scheduling::appointments_in_closure(
            &mut conn,
            ctx.tenant_id,
            facility_id,
            row.get("starts_on"),
            row.get("ends_on"),
            &policy.time_zone,
        )
        .await?
        .iter()
        .map(scheduling::appointment_json)
        .collect();
        scheduling::attach_patient_summaries(&mut *conn, ctx.tenant_id, &mut items, true).await?;
    }
    Ok(Json(json!({
        "event": calendar_json(&row),
        "items": items,
        "requires_human_decision": !items.is_empty(),
    })))
}

pub async fn deactivate_calendar_event(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let row =
        sqlx::query("SELECT * FROM operational_calendar_events WHERE id = $1 AND tenant_id = $2")
            .bind(id)
            .bind(ctx.tenant_id)
            .fetch_optional(&mut *conn)
            .await?
            .ok_or_else(ApiError::not_found)?;
    drop(conn);
    let allowed = guard(
        &state,
        &ctx,
        actions::SCHEDULING_MANAGE,
        "operational_calendar_event",
        Some(ResourceCtx {
            tenant_id: ctx.tenant_id,
            patient_id: None,
            facility_id: row.get("facility_id"),
        }),
    )
    .await?;
    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let updated = sqlx::query(
        "UPDATE operational_calendar_events SET active = false WHERE id = $1 AND tenant_id = $2 AND active
         RETURNING *",
    )
    .bind(id)
    .bind(ctx.tenant_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| ApiError::conflict("already_inactive", "this event is already inactive"))?;
    emit(
        &mut tx,
        &ctx,
        &state,
        "scheduling.calendar_event.deactivated",
        json!({ "event_id": id }),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(calendar_json(&updated)))
}

// ---------------------------------------------------------------------------
// Worker
// ---------------------------------------------------------------------------

/// Run one scheduling worker pass on demand (external schedulers, tests).
/// Claims use `FOR UPDATE SKIP LOCKED`, so a concurrent in-process worker
/// never double-delivers.
pub async fn worker_tick(
    State(state): State<AppState>,
    ctx: AuthContext,
) -> Result<Json<Value>, ApiError> {
    let allowed = guard(
        &state,
        &ctx,
        actions::SCHEDULING_MANAGE,
        "scheduling_worker",
        manage_ctx(&ctx),
    )
    .await?;
    allowed.record_on_pool(&state, &ctx).await?;
    let worker_id = format!("api:{}", ctx.user_id);
    let report = crate::notify::scheduling_tick(&state, &worker_id).await?;
    Ok(Json(report))
}
