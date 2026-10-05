//! Diagnostic orderable catalog: search and typed loading of
//! `catalog_entries` rows of kind `diagnostic_orderable`. Administration
//! (add / update / deactivate / facility mapping / history) is the existing
//! tenant catalog administration in `access_admin`; this module only reads.

use crate::auth::AuthContext;
use crate::error::ApiError;
use crate::policy::{actions, facility_scope};
use crate::routes::guard;
use crate::state::AppState;
use axum::extract::{Query, State};
use axum::Json;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::{PgConnection, Row};
use uuid::Uuid;
use wellos_domain::diagnostics::OrderableConfig;

pub const KIND: &str = "diagnostic_orderable";
const MAX_RESULTS: i64 = 50;

#[derive(Debug, Clone)]
pub struct Orderable {
    pub id: Uuid,
    pub code: String,
    pub name_en: String,
    pub name_es: String,
    pub synonyms: Vec<String>,
    pub external_codings: Value,
    pub config: OrderableConfig,
    pub active: bool,
    pub version: i64,
    pub facility_ids: Vec<Uuid>,
}

impl Orderable {
    pub fn name(&self, lang: &str) -> &str {
        if lang.starts_with("es") {
            &self.name_es
        } else {
            &self.name_en
        }
    }

    pub fn preparation(&self, lang: &str) -> Option<&str> {
        if lang.starts_with("es") {
            self.config.preparation_es.as_deref()
        } else {
            self.config.preparation_en.as_deref()
        }
    }

    /// Primary LOINC coding when the orderable has one (laboratory tests);
    /// kept on `service_requests.code_loinc` for the legacy result path.
    pub fn loinc(&self) -> Option<String> {
        self.external_codings
            .as_array()?
            .iter()
            .find(|c| c.get("system").and_then(Value::as_str) == Some("http://loinc.org"))
            .and_then(|c| c.get("code").and_then(Value::as_str))
            .map(str::to_owned)
    }

    pub fn available_at(&self, facility_id: Uuid) -> bool {
        self.facility_ids.is_empty() || self.facility_ids.contains(&facility_id)
    }

    pub fn json(&self, lang: &str) -> Value {
        json!({
            "id": self.id,
            "code": self.code,
            "name": self.name(lang),
            "name_en": self.name_en,
            "name_es": self.name_es,
            "synonyms": self.synonyms,
            "external_codings": self.external_codings,
            "category_code": self.config.category_code,
            "modality_code": self.config.modality_code,
            "result_type": self.config.result_type,
            "components": self.config.components,
            "panel_member_codes": self.config.panel_member_codes,
            "specimen": self.config.specimen,
            "preparation": self.preparation(lang),
            "preparation_en": self.config.preparation_en,
            "preparation_es": self.config.preparation_es,
            "scheduling_service_code": self.config.scheduling_service_code,
            "required_resource_types": self.config.required_resource_types,
            "fulfilment_modes": self.config.allowed_modes(),
            "safety_rules": self.config.safety_rules,
            "duplicate_window_days": self.config.duplicate_window_days,
            "redundant_with_codes": self.config.redundant_with_codes,
            "requires_specimen": self.config.needs_specimen(),
            "expects_imaging_study": self.config.expects_imaging_study,
            "active": self.active,
            "version": self.version,
            "facility_ids": self.facility_ids,
        })
    }
}

fn from_row(r: &sqlx::postgres::PgRow) -> Result<Orderable, ApiError> {
    let config: OrderableConfig = serde_json::from_value(r.get::<Value, _>("config"))
        .map_err(|e| ApiError::internal(format!("invalid orderable config: {e}")))?;
    Ok(Orderable {
        id: r.get("id"),
        code: r.get("code"),
        name_en: r.get("name_en"),
        name_es: r.get("name_es"),
        synonyms: r.get("synonyms"),
        external_codings: r.get("external_codings"),
        config,
        active: r.get("active"),
        version: r.get("version"),
        facility_ids: r.get("facility_ids"),
    })
}

const COLUMNS: &str = "c.id, c.code, c.name_en, c.name_es, c.synonyms, c.external_codings, c.config, c.active, c.version,
    COALESCE((SELECT array_agg(f.facility_id) FROM catalog_entry_facilities f WHERE f.entry_id = c.id), '{}') AS facility_ids";

/// Load orderables by id, active or not (orders keep referencing deactivated
/// entries; only *new* orders require an active one).
pub async fn load_by_ids(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    ids: &[Uuid],
) -> Result<Vec<Orderable>, ApiError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM catalog_entries c
         WHERE c.tenant_id = $1 AND c.kind = $2 AND c.id = ANY($3)"
    ))
    .bind(tenant_id)
    .bind(KIND)
    .bind(ids)
    .fetch_all(&mut *conn)
    .await?;
    rows.iter().map(from_row).collect()
}

pub async fn load_by_codes(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    codes: &[String],
) -> Result<Vec<Orderable>, ApiError> {
    if codes.is_empty() {
        return Ok(Vec::new());
    }
    let rows = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM catalog_entries c
         WHERE c.tenant_id = $1 AND c.kind = $2 AND c.code = ANY($3) AND c.active"
    ))
    .bind(tenant_id)
    .bind(KIND)
    .bind(codes)
    .fetch_all(&mut *conn)
    .await?;
    rows.iter().map(from_row).collect()
}

/// Whether an orderable can be ordered now: active and inside its effective
/// window.
pub async fn is_orderable_now(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    id: Uuid,
    now: DateTime<Utc>,
) -> Result<bool, ApiError> {
    let ok: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM catalog_entries
         WHERE tenant_id = $1 AND kind = $2 AND id = $3 AND active
           AND (effective_from IS NULL OR effective_from <= $4::date)
           AND (effective_to IS NULL OR effective_to >= $4::date))",
    )
    .bind(tenant_id)
    .bind(KIND)
    .bind(id)
    .bind(now.date_naive())
    .fetch_one(&mut *conn)
    .await?;
    Ok(ok)
}

#[derive(Debug, Default, Deserialize)]
pub struct SearchQuery {
    pub q: Option<String>,
    pub category: Option<String>,
    pub modality: Option<String>,
    pub facility_id: Option<Uuid>,
    pub lang: Option<String>,
    pub include_inactive: Option<bool>,
    pub limit: Option<i64>,
}

pub async fn search_orderables(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    q: &SearchQuery,
    facility_filter: Option<&[Uuid]>,
) -> Result<Vec<Orderable>, ApiError> {
    let term =
        q.q.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.chars().take(120).collect::<String>());
    let pattern = term.as_deref().map(|s| {
        let escaped = s
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        format!("%{escaped}%")
    });
    let limit = q.limit.unwrap_or(MAX_RESULTS).clamp(1, MAX_RESULTS);
    let today = Utc::now().date_naive();
    let rows = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM catalog_entries c
         WHERE c.tenant_id = $1 AND c.kind = $2
           AND ($3::boolean OR (c.active
                AND (c.effective_from IS NULL OR c.effective_from <= $9)
                AND (c.effective_to IS NULL OR c.effective_to >= $9)))
           AND ($4::text IS NULL OR c.name_en ILIKE $4 OR c.name_es ILIKE $4 OR c.code ILIKE $4
                OR EXISTS (SELECT 1 FROM unnest(c.synonyms) s WHERE s ILIKE $4)
                OR EXISTS (SELECT 1 FROM jsonb_array_elements(c.external_codings) e
                           WHERE e->>'code' ILIKE $4))
           AND ($5::text IS NULL OR c.config->>'category_code' = $5)
           AND ($6::text IS NULL OR c.config->>'modality_code' = $6)
           AND ($7::uuid IS NULL OR NOT EXISTS (SELECT 1 FROM catalog_entry_facilities f WHERE f.entry_id = c.id)
                OR EXISTS (SELECT 1 FROM catalog_entry_facilities f WHERE f.entry_id = c.id AND f.facility_id = $7))
           AND ($8::uuid[] IS NULL OR NOT EXISTS (SELECT 1 FROM catalog_entry_facilities f WHERE f.entry_id = c.id)
                OR EXISTS (SELECT 1 FROM catalog_entry_facilities f WHERE f.entry_id = c.id AND f.facility_id = ANY($8)))
         ORDER BY (c.code ILIKE $4 OR c.name_en ILIKE $4 OR c.name_es ILIKE $4) DESC, c.name_en
         LIMIT $10"
    ))
    .bind(tenant_id)
    .bind(KIND)
    .bind(q.include_inactive.unwrap_or(false))
    .bind(pattern)
    .bind(q.category.as_deref().filter(|s| !s.is_empty()))
    .bind(q.modality.as_deref().filter(|s| !s.is_empty()))
    .bind(q.facility_id)
    .bind(facility_filter)
    .bind(today)
    .bind(limit)
    .fetch_all(&mut *conn)
    .await?;
    rows.iter().map(from_row).collect()
}

/// `GET /api/v1/diagnostics/catalog` — clinician search by name, synonym,
/// code, category, modality and facility. Facility-scoped staff only see
/// orderables available at one of their facilities.
pub async fn search(
    State(state): State<AppState>,
    ctx: AuthContext,
    Query(q): Query<SearchQuery>,
) -> Result<Json<Value>, ApiError> {
    guard(
        &state,
        &ctx,
        actions::DIAGNOSTIC_READ,
        "diagnostic_catalog",
        None,
    )
    .await?
    .record_on_pool(&state, &ctx)
    .await?;
    let scope = facility_scope(&ctx, actions::DIAGNOSTIC_READ);
    if matches!(&scope, Some(ids) if ids.is_empty()) {
        return Ok(Json(json!({ "items": [] })));
    }
    let lang = q.lang.clone().unwrap_or_else(|| "en".into());
    let mut conn = state.pool.acquire().await?;
    let items = search_orderables(&mut conn, ctx.tenant_id, &q, scope.as_deref()).await?;
    Ok(Json(json!({
        "items": items.iter().map(|o| o.json(&lang)).collect::<Vec<_>>(),
    })))
}
