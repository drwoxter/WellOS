//! Order composer: deterministic preflight, dMind order suggestions
//! (assistive only), explicit clinician confirmation into `service_requests`,
//! fulfilment transitions and Access scheduling linkage.
//!
//! Nothing here lets AI place, prioritise or confirm an order: suggestions
//! are bounded to server-supplied catalog candidates and stored as artifacts
//! the clinician looks at; the only writer of orders is the confirmation
//! route, which re-runs the safety engine inside the encounter lock.

use super::catalog::{self, Orderable};
use super::{
    actor_label, apply_transition_in, check_len, guard_order, history_json, load_order, lock_order,
    order_detail_json, order_json, record_history, OrderRow, TransitionInput, MAX_TEXT,
};
use crate::aigov;
use crate::audit;
use crate::auth::AuthContext;
use crate::error::ApiError;
use crate::policy::{actions, ResourceCtx};
use crate::routes::access::{self, ConstraintsInput, CreateRequestBody};
use crate::routes::guard;
use crate::state::AppState;
use axum::extract::{Path, State};
use axum::Json;
use chrono::{DateTime, Datelike, Utc};
use dmind_gateway::diagnostics::{
    CandidateOrderable, OrderSuggestionRequest, MAX_CANDIDATES, MAX_FACTS,
    ORDER_SUGGESTION_TEMPLATE,
};
use dmind_gateway::{hash_json, GatewayError};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{PgConnection, Row};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;
use wellos_domain::ai::{ArtifactStatus, ProviderInfo};
use wellos_domain::diagnostics::{
    evaluate_safety, FulfilmentMode, OrderPriority, OrderStatus, OrderTransition,
    PatientSafetyFacts, RecentOrder, SafetyBlock, SafetyCandidate, SafetyEvaluation, SafetyInput,
    DIAGNOSTIC_SAFETY_VERSION,
};
use wellos_domain::diagnostics_ai::{DiagnosticOrderSuggestionV1, ORDER_SUGGESTION_SCHEMA};

const MAX_ITEMS: usize = 25;
const RECENT_ORDER_LOOKBACK_DAYS: i64 = 365;
const RECENT_ORDER_LIMIT: i64 = 500;
const MIN_REASON_CHARS: usize = 3;

// ---------------------------------------------------------------------------
// Encounter context
// ---------------------------------------------------------------------------

pub struct EncounterCtx {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub patient_id: Uuid,
    pub facility_id: Uuid,
    pub status: String,
    pub practitioner_id: Uuid,
}

impl EncounterCtx {
    pub fn resource(&self) -> ResourceCtx {
        ResourceCtx {
            tenant_id: self.tenant_id,
            patient_id: Some(self.patient_id),
            facility_id: Some(self.facility_id),
        }
    }
}

pub async fn load_encounter(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    id: Uuid,
    for_update: bool,
) -> Result<EncounterCtx, ApiError> {
    let lock = if for_update { "FOR UPDATE" } else { "" };
    let r = sqlx::query(&format!(
        "SELECT id, tenant_id, patient_id, facility_id, status, practitioner_id
         FROM encounters WHERE id = $1 AND tenant_id = $2 {lock}"
    ))
    .bind(id)
    .bind(tenant_id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(ApiError::not_found)?;
    Ok(EncounterCtx {
        id: r.get("id"),
        tenant_id: r.get("tenant_id"),
        patient_id: r.get("patient_id"),
        facility_id: r.get("facility_id"),
        status: r.get("status"),
        practitioner_id: r.get("practitioner_id"),
    })
}

/// Orders attach only to the requester's own active encounter.
fn require_order_context(e: &EncounterCtx, ctx: &AuthContext) -> Result<(), ApiError> {
    if e.status != "in_progress" {
        return Err(ApiError::conflict(
            "encounter_not_active",
            "orders require an active (in progress) encounter",
        ));
    }
    if e.practitioner_id != ctx.user_id {
        return Err(ApiError::forbidden(
            "orders require the requester's own encounter",
        ));
    }
    Ok(())
}

fn lang_of(q: Option<&str>) -> String {
    if q.is_some_and(|l| l.starts_with("es")) {
        "es".into()
    } else {
        "en".into()
    }
}

// ---------------------------------------------------------------------------
// Composer input → safety candidates
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct ItemInput {
    pub orderable_id: Uuid,
    pub fulfilment_mode: Option<String>,
    pub priority: Option<String>,
    pub requested_window_start: Option<DateTime<Utc>>,
    pub requested_window_end: Option<DateTime<Utc>>,
}

#[derive(Debug, Default, Deserialize)]
pub struct PreflightBody {
    #[serde(default)]
    pub items: Vec<ItemInput>,
    #[serde(default)]
    pub answers: BTreeMap<String, bool>,
    pub performing_facility_id: Option<Uuid>,
    pub priority: Option<String>,
    pub lang: Option<String>,
}

pub struct Composed {
    pub orderables: Vec<Orderable>,
    pub candidates: Vec<SafetyCandidate>,
    pub input: SafetyInput,
    pub evaluation: SafetyEvaluation,
    pub fingerprint: String,
    pub performing_facility_id: Uuid,
}

fn parse_priority(s: Option<&str>, fallback: OrderPriority) -> Result<OrderPriority, ApiError> {
    match s.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(fallback),
        Some(s) => OrderPriority::parse(s).ok_or_else(|| {
            ApiError::bad_request(
                "validation_failed",
                "priority must be routine, urgent, stat or timed",
            )
        }),
    }
}

fn parse_mode(s: Option<&str>) -> Result<Option<FulfilmentMode>, ApiError> {
    match s.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(s) => FulfilmentMode::parse(s).map(Some).ok_or_else(|| {
            ApiError::bad_request(
                "validation_failed",
                "fulfilment_mode must be scheduled, immediate, inpatient, bedside or walk_in",
            )
        }),
    }
}

/// What the safety evaluation is bound to: the exact candidates, the recent
/// order picture, the patient facts and the answers — not the clock, so a
/// confirmation a few seconds later still matches its preflight.
#[derive(Serialize)]
struct Fingerprint<'a> {
    engine_version: &'static str,
    candidates: &'a [SafetyCandidate],
    recent_orders: &'a [RecentOrder],
    facts: &'a PatientSafetyFacts,
    answers: &'a BTreeMap<String, bool>,
}

pub async fn compose(
    conn: &mut PgConnection,
    e: &EncounterCtx,
    body: &PreflightBody,
    now: DateTime<Utc>,
) -> Result<Composed, ApiError> {
    if body.items.is_empty() {
        return Err(ApiError::bad_request(
            "validation_failed",
            "at least one orderable is required",
        ));
    }
    if body.items.len() > MAX_ITEMS {
        return Err(ApiError::bad_request(
            "validation_failed",
            "too many orderables in one order group",
        ));
    }
    let mut seen = BTreeSet::new();
    for it in &body.items {
        if !seen.insert(it.orderable_id) {
            return Err(ApiError::bad_request(
                "validation_failed",
                "an orderable appears more than once",
            ));
        }
    }
    let performing_facility_id = body.performing_facility_id.unwrap_or(e.facility_id);
    if performing_facility_id != e.facility_id {
        let ok: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM facilities WHERE id = $1 AND tenant_id = $2")
                .bind(performing_facility_id)
                .bind(e.tenant_id)
                .fetch_optional(&mut *conn)
                .await?;
        if ok.is_none() {
            return Err(ApiError::bad_request(
                "validation_failed",
                "performing_facility_id is not a facility of this tenant",
            ));
        }
    }
    let group_priority = parse_priority(body.priority.as_deref(), OrderPriority::Routine)?;
    let ids: Vec<Uuid> = body.items.iter().map(|i| i.orderable_id).collect();
    let loaded = catalog::load_by_ids(conn, e.tenant_id, &ids).await?;
    let mut orderables = Vec::with_capacity(ids.len());
    let mut candidates = Vec::with_capacity(ids.len());
    for it in &body.items {
        let o = loaded
            .iter()
            .find(|o| o.id == it.orderable_id)
            .cloned()
            .ok_or_else(|| {
                ApiError::bad_request(
                    "validation_failed",
                    format!(
                        "orderable {} is not in this tenant's catalog",
                        it.orderable_id
                    ),
                )
            })?;
        if !catalog::is_orderable_now(conn, e.tenant_id, o.id, now).await? {
            return Err(ApiError::conflict(
                "orderable_inactive",
                format!("{} is deactivated or outside its effective dates", o.code),
            ));
        }
        if !o.available_at(performing_facility_id) {
            return Err(ApiError::conflict(
                "orderable_unavailable_at_facility",
                format!("{} is not offered at the selected facility", o.code),
            ));
        }
        let mode = parse_mode(it.fulfilment_mode.as_deref())?.unwrap_or(o.config.default_mode());
        if !o.config.allowed_modes().contains(&mode) {
            return Err(ApiError::bad_request(
                "validation_failed",
                format!("{} cannot be fulfilled as {}", o.code, mode.as_str()),
            ));
        }
        let priority = parse_priority(it.priority.as_deref(), group_priority)?;
        if let (Some(s), Some(en)) = (it.requested_window_start, it.requested_window_end) {
            if en < s {
                return Err(ApiError::bad_request(
                    "validation_failed",
                    "requested window end precedes its start",
                ));
            }
        }
        if priority == OrderPriority::Timed
            && it.requested_window_start.is_none()
            && it.requested_window_end.is_none()
        {
            return Err(ApiError::bad_request(
                "validation_failed",
                "a timed order needs a requested window",
            ));
        }
        candidates.push(SafetyCandidate {
            orderable_id: o.id,
            code: o.code.clone(),
            name_en: o.name_en.clone(),
            name_es: o.name_es.clone(),
            config: o.config.clone(),
            fulfilment_mode: mode,
            priority,
            requested_window_start: it.requested_window_start,
            requested_window_end: it.requested_window_end,
        });
        orderables.push(o);
    }
    let recent_orders = load_recent_orders(conn, e.tenant_id, e.patient_id, now).await?;
    let facts = load_patient_facts(conn, e.tenant_id, e.patient_id, now).await?;
    let mut answers = BTreeMap::new();
    for (k, v) in &body.answers {
        let k = k.trim();
        if k.is_empty() || k.chars().count() > 160 {
            return Err(ApiError::bad_request(
                "validation_failed",
                "invalid safety answer key",
            ));
        }
        answers.insert(k.to_string(), *v);
    }
    let fingerprint = hash_json(&Fingerprint {
        engine_version: DIAGNOSTIC_SAFETY_VERSION,
        candidates: &candidates,
        recent_orders: &recent_orders,
        facts: &facts,
        answers: &answers,
    });
    let input = SafetyInput {
        now,
        candidates: candidates.clone(),
        recent_orders,
        facts,
        answers,
    };
    let evaluation = evaluate_safety(&input);
    Ok(Composed {
        orderables,
        candidates,
        input,
        evaluation,
        fingerprint,
        performing_facility_id,
    })
}

pub async fn load_recent_orders(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    patient_id: Uuid,
    now: DateTime<Utc>,
) -> Result<Vec<RecentOrder>, ApiError> {
    let since = now - chrono::Duration::days(RECENT_ORDER_LOOKBACK_DAYS);
    let rows = sqlx::query(
        "SELECT sr.id, COALESCE(sr.orderable_code, sr.code_loinc) AS code, sr.created_at, sr.order_status,
                (EXISTS (SELECT 1 FROM diagnostic_reports r WHERE r.service_request_id = sr.id
                           AND r.status IN ('final','amended','corrected')
                           AND NOT EXISTS (SELECT 1 FROM diagnostic_reports r2 WHERE r2.replaces = r.id))
                 OR EXISTS (SELECT 1 FROM observations ob WHERE ob.service_request_id = sr.id
                           AND ob.status IN ('final','corrected'))) AS has_result
         FROM service_requests sr
         WHERE sr.tenant_id = $1 AND sr.patient_id = $2 AND sr.created_at >= $3
           AND sr.order_status <> 'entered_in_error'
         ORDER BY sr.created_at DESC, sr.id
         LIMIT $4",
    )
    .bind(tenant_id)
    .bind(patient_id)
    .bind(since)
    .bind(RECENT_ORDER_LIMIT)
    .fetch_all(&mut *conn)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let Some(code) = r.get::<Option<String>, _>("code") else {
            continue;
        };
        let status: String = r.get("order_status");
        out.push(RecentOrder {
            service_request_id: r.get("id"),
            orderable_code: code,
            created_at: r.get("created_at"),
            order_status: OrderStatus::parse(&status).unwrap_or(OrderStatus::Placed),
            has_result: r.get("has_result"),
        });
    }
    Ok(out)
}

/// Deterministic patient facts for the safety engine: recorded allergies,
/// active medications, coded conditions (`condition:<code>`) and demographic
/// facts (`sex:<sex>`, `pregnancy_possible` for female patients of
/// child-bearing age). Tenant rules reference these keys; nothing is
/// inferred by AI.
pub async fn load_patient_facts(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    patient_id: Uuid,
    now: DateTime<Utc>,
) -> Result<PatientSafetyFacts, ApiError> {
    let p = sqlx::query("SELECT birth_date, sex FROM patients WHERE id = $1 AND tenant_id = $2")
        .bind(patient_id)
        .bind(tenant_id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(ApiError::not_found)?;
    let birth: chrono::NaiveDate = p.get("birth_date");
    let sex: String = p.get("sex");
    let today = now.date_naive();
    let mut age = today.year() - birth.year();
    if (today.month(), today.day()) < (birth.month(), birth.day()) {
        age -= 1;
    }
    let mut facts = BTreeSet::new();
    facts.insert(format!("sex:{}", sex.to_lowercase()));
    facts.insert(format!("age_years:{age}"));
    if sex.eq_ignore_ascii_case("female") && (12..=55).contains(&age) {
        facts.insert("pregnancy_possible".to_string());
    }
    if age < 18 {
        facts.insert("paediatric".to_string());
    }
    let allergies: Vec<String> = sqlx::query_scalar(
        "SELECT substance FROM allergies WHERE tenant_id = $1 AND patient_id = $2 ORDER BY substance",
    )
    .bind(tenant_id)
    .bind(patient_id)
    .fetch_all(&mut *conn)
    .await?;
    let medications: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM medications WHERE tenant_id = $1 AND patient_id = $2 AND status = 'active' ORDER BY name",
    )
    .bind(tenant_id)
    .bind(patient_id)
    .fetch_all(&mut *conn)
    .await?;
    let conditions: Vec<String> = sqlx::query_scalar(
        "SELECT code FROM conditions WHERE tenant_id = $1 AND patient_id = $2
           AND COALESCE(clinical_status, 'active') NOT IN ('resolved','inactive','entered-in-error')",
    )
    .bind(tenant_id)
    .bind(patient_id)
    .fetch_all(&mut *conn)
    .await?;
    for c in conditions {
        facts.insert(format!("condition:{}", c.trim().to_lowercase()));
    }
    Ok(PatientSafetyFacts {
        facts,
        allergies,
        medications,
    })
}

fn finding_json(f: &wellos_domain::diagnostics::SafetyFinding, lang: &str) -> Value {
    json!({
        "id": f.id,
        "kind": f.kind,
        "severity": f.severity,
        "orderable_id": f.orderable_id,
        "orderable_code": f.orderable_code,
        "text": if lang == "es" { &f.text_es } else { &f.text_en },
        "text_en": f.text_en,
        "text_es": f.text_es,
        "evidence": f.evidence,
        "answerable": f.answerable,
    })
}

fn evaluation_json(id: Uuid, c: &Composed, lang: &str) -> Value {
    let warnings = c.evaluation.warnings().len();
    let hard_stops = c.evaluation.hard_stops().len();
    json!({
        "id": id,
        "engine_version": c.evaluation.engine_version,
        "input_hash": c.fingerprint,
        "evaluated_at": c.input.now,
        "performing_facility_id": c.performing_facility_id,
        "candidates": c.candidates.iter().map(|k| json!({
            "orderable_id": k.orderable_id,
            "code": k.code,
            "name": if lang == "es" { &k.name_es } else { &k.name_en },
            "fulfilment_mode": k.fulfilment_mode,
            "priority": k.priority,
            "requested_window_start": k.requested_window_start,
            "requested_window_end": k.requested_window_end,
            "requires_appointment": k.fulfilment_mode.requires_appointment(),
            "scheduling_service_code": k.config.scheduling_service_code,
            "needs_specimen": k.config.needs_specimen(),
            "preparation": if lang == "es" { &k.config.preparation_es } else { &k.config.preparation_en },
        })).collect::<Vec<_>>(),
        "findings": c.evaluation.findings.iter().map(|f| finding_json(f, lang)).collect::<Vec<_>>(),
        "warnings": warnings,
        "hard_stops": hard_stops,
        "requires_acknowledgement": warnings > 0,
        "requires_override": hard_stops > 0,
    })
}

async fn persist_evaluation(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    e: &EncounterCtx,
    c: &Composed,
) -> Result<Uuid, ApiError> {
    let id = Uuid::now_v7();
    let ids: Vec<Uuid> = c.candidates.iter().map(|k| k.orderable_id).collect();
    sqlx::query(
        "INSERT INTO diagnostic_safety_evaluations
         (id, tenant_id, patient_id, encounter_id, engine_version, input_hash, orderable_ids, findings,
          hard_stops, warnings, evaluated_by, evaluated_at)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)",
    )
    .bind(id)
    .bind(e.tenant_id)
    .bind(e.patient_id)
    .bind(e.id)
    .bind(&c.evaluation.engine_version)
    .bind(&c.fingerprint)
    .bind(&ids)
    .bind(serde_json::to_value(&c.evaluation.findings).map_err(ApiError::internal)?)
    .bind(c.evaluation.hard_stops().len() as i32)
    .bind(c.evaluation.warnings().len() as i32)
    .bind(ctx.user_id)
    .bind(c.input.now)
    .execute(&mut *conn)
    .await?;
    Ok(id)
}

/// `POST /api/v1/encounters/:id/diagnostic-orders/preflight`
pub async fn preflight(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(encounter_id): Path<Uuid>,
    Json(body): Json<PreflightBody>,
) -> Result<Json<Value>, ApiError> {
    let lang = lang_of(body.lang.as_deref());
    let mut conn = state.pool.acquire().await?;
    let e = load_encounter(&mut conn, ctx.tenant_id, encounter_id, false).await?;
    let allowed = guard(
        &state,
        &ctx,
        actions::DIAGNOSTIC_ORDER_MANAGE,
        "diagnostic_order",
        Some(e.resource()),
    )
    .await?;
    require_order_context(&e, &ctx)?;
    drop(conn);
    let now = Utc::now();
    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let composed = compose(&mut tx, &e, &body, now).await?;
    let id = persist_evaluation(&mut tx, &ctx, &e, &composed).await?;
    audit::emit(
        &mut *tx,
        &ctx,
        "diagnostic.safety.evaluated",
        &state.cell,
        json!({
            "safety_evaluation_id": id,
            "encounter_id": e.id,
            "patient_id": e.patient_id,
            "orderable_codes": composed.candidates.iter().map(|k| k.code.clone()).collect::<Vec<_>>(),
            "warnings": composed.evaluation.warnings().len(),
            "hard_stops": composed.evaluation.hard_stops().len(),
        }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    tx.commit().await?;
    Ok(Json(
        json!({ "evaluation": evaluation_json(id, &composed, &lang) }),
    ))
}

// ---------------------------------------------------------------------------
// dMind order suggestion (assistive)
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct SuggestBody {
    pub q: Option<String>,
    pub lang: Option<String>,
    pub facility_id: Option<Uuid>,
}

async fn encounter_facts(
    conn: &mut PgConnection,
    e: &EncounterCtx,
    recent: &[RecentOrder],
) -> Result<Vec<(String, String)>, ApiError> {
    let mut facts: Vec<(String, String)> = Vec::new();
    if let Some(n) = sqlx::query(
        "SELECT id, version, reason_for_encounter, history_present_illness, medical_history,
                review_of_systems, physical_exam, assessment, plan
         FROM encounter_notes WHERE tenant_id = $1 AND encounter_id = $2",
    )
    .bind(e.tenant_id)
    .bind(e.id)
    .fetch_optional(&mut *conn)
    .await?
    {
        let note_id: Uuid = n.get("id");
        let version: i64 = n.get("version");
        for col in [
            "reason_for_encounter",
            "history_present_illness",
            "medical_history",
            "review_of_systems",
            "physical_exam",
            "assessment",
            "plan",
        ] {
            if let Some(text) = n.get::<Option<String>, _>(col) {
                let t = text.trim();
                if !t.is_empty() {
                    facts.push((
                        format!("note:{note_id}@{version}:{col}"),
                        t.chars().take(MAX_TEXT).collect(),
                    ));
                }
            }
        }
    }
    let dx = sqlx::query(
        "SELECT id, code, display FROM conditions
         WHERE tenant_id = $1 AND patient_id = $2
         ORDER BY (encounter_id = $3) DESC, recorded_at DESC LIMIT 20",
    )
    .bind(e.tenant_id)
    .bind(e.patient_id)
    .bind(e.id)
    .fetch_all(&mut *conn)
    .await?;
    for r in dx {
        let id: Uuid = r.get("id");
        let code: String = r.get("code");
        let display: String = r.get("display");
        facts.push((format!("condition:{id}"), format!("{display} ({code})")));
    }
    let al = sqlx::query("SELECT id, substance, criticality FROM allergies WHERE tenant_id = $1 AND patient_id = $2 ORDER BY recorded_at DESC LIMIT 20")
        .bind(e.tenant_id)
        .bind(e.patient_id)
        .fetch_all(&mut *conn)
        .await?;
    for r in al {
        let id: Uuid = r.get("id");
        facts.push((
            format!("allergy:{id}"),
            format!(
                "allergy {} ({})",
                r.get::<String, _>("substance"),
                r.get::<String, _>("criticality")
            ),
        ));
    }
    let meds = sqlx::query("SELECT id, name FROM medications WHERE tenant_id = $1 AND patient_id = $2 AND status = 'active' ORDER BY recorded_at DESC LIMIT 30")
        .bind(e.tenant_id)
        .bind(e.patient_id)
        .fetch_all(&mut *conn)
        .await?;
    for r in meds {
        let id: Uuid = r.get("id");
        facts.push((
            format!("medication:{id}"),
            format!("medication {}", r.get::<String, _>("name")),
        ));
    }
    if let Some(v) = sqlx::query(
        "SELECT id, systolic_mmhg, diastolic_mmhg, heart_rate_bpm, temperature_c, spo2_percent
         FROM vital_signs WHERE tenant_id = $1 AND patient_id = $2 ORDER BY recorded_at DESC LIMIT 1",
    )
    .bind(e.tenant_id)
    .bind(e.patient_id)
    .fetch_optional(&mut *conn)
    .await?
    {
        let id: Uuid = v.get("id");
        let mut parts = Vec::new();
        let num = |col: &str| v.get::<Option<rust_decimal::Decimal>, _>(col);
        if let (Some(s), Some(d)) = (num("systolic_mmhg"), num("diastolic_mmhg")) {
            parts.push(format!("blood pressure {s}/{d} mmHg"));
        }
        if let Some(h) = num("heart_rate_bpm") {
            parts.push(format!("heart rate {h} bpm"));
        }
        if let Some(t) = num("temperature_c") {
            parts.push(format!("temperature {t} C"));
        }
        if let Some(o) = num("spo2_percent") {
            parts.push(format!("SpO2 {o}%"));
        }
        if !parts.is_empty() {
            facts.push((format!("vitals:{id}"), parts.join(", ")));
        }
    }
    for r in recent.iter().take(20) {
        facts.push((
            format!("order:{}", r.service_request_id),
            format!(
                "previous order {} ({}, {}{})",
                r.orderable_code,
                r.order_status.as_str(),
                r.created_at.date_naive(),
                if r.has_result {
                    ", result available"
                } else {
                    ""
                }
            ),
        ));
    }
    facts.truncate(MAX_FACTS);
    Ok(facts)
}

/// `POST /api/v1/encounters/:id/diagnostic-orders/suggest` — one bounded
/// dMind call per encounter snapshot; the output is an artifact for the
/// clinician to look at, never an order.
pub async fn suggest(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(encounter_id): Path<Uuid>,
    body: Option<Json<SuggestBody>>,
) -> Result<Json<Value>, ApiError> {
    let body = body.map(|Json(b)| b).unwrap_or_default();
    let lang = lang_of(body.lang.as_deref());
    let mut conn = state.pool.acquire().await?;
    let e = load_encounter(&mut conn, ctx.tenant_id, encounter_id, false).await?;
    let allowed = guard(
        &state,
        &ctx,
        actions::DIAGNOSTIC_ORDER_MANAGE,
        "diagnostic_order_suggestion",
        Some(e.resource()),
    )
    .await?;
    require_order_context(&e, &ctx)?;
    let now = Utc::now();
    let facility_id = body.facility_id.unwrap_or(e.facility_id);
    let q = catalog::SearchQuery {
        q: body.q.clone(),
        facility_id: Some(facility_id),
        limit: Some(MAX_CANDIDATES as i64),
        ..Default::default()
    };
    let orderables = catalog::search_orderables(&mut conn, e.tenant_id, &q, None).await?;
    if orderables.is_empty() {
        return Err(ApiError::conflict(
            "no_candidates",
            "no active orderables match; dMind can only suggest from the tenant catalog",
        ));
    }
    let recent = load_recent_orders(&mut conn, e.tenant_id, e.patient_id, now).await?;
    let facts = encounter_facts(&mut conn, &e, &recent).await?;
    drop(conn);
    if facts.is_empty() {
        return Err(ApiError::conflict(
            "insufficient_context",
            "the consultation has no documented facts to suggest from",
        ));
    }
    let candidates: Vec<CandidateOrderable> = orderables
        .iter()
        .map(|o| {
            let window = chrono::Duration::days(i64::from(o.config.duplicate_window_days.max(1)));
            let recent_or_pending = recent.iter().any(|r| {
                r.orderable_code == o.code
                    && (!matches!(
                        r.order_status,
                        OrderStatus::Completed
                            | OrderStatus::Cancelled
                            | OrderStatus::Rejected
                            | OrderStatus::EnteredInError
                    ) || (r.has_result && now - r.created_at <= window))
            });
            CandidateOrderable {
                orderable_id: o.id,
                code: o.code.clone(),
                name_en: o.name_en.clone(),
                name_es: o.name_es.clone(),
                synonyms: o.synonyms.clone(),
                category_code: o.config.category_code.clone(),
                indications: Vec::new(),
                preparation: o.preparation(&lang).map(str::to_owned),
                recent_or_pending,
            }
        })
        .collect();
    let req = OrderSuggestionRequest {
        template: ORDER_SUGGESTION_TEMPLATE.to_string(),
        language: lang.clone(),
        facts,
        candidates,
    };
    let candidate_ids = req.candidate_ids();
    let fact_refs = req.fact_refs();

    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    audit::emit(
        &mut *tx,
        &ctx,
        "ai.artifact.requested",
        &state.cell,
        json!({ "encounter_id": e.id, "patient_id": e.patient_id, "template": ORDER_SUGGESTION_TEMPLATE,
                "candidates": candidate_ids.len(), "facts": fact_refs.len() }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    tx.commit().await?;

    let hash = hash_json(&req);
    let scope = aigov::ReuseScope::DiagnosticOrderSuggestion { encounter_id: e.id };
    let plan = aigov::plan(
        &state,
        e.tenant_id,
        e.patient_id,
        scope,
        &hash,
        ORDER_SUGGESTION_SCHEMA,
    )
    .await?;
    let synthetic = plan.model_synthetic();
    let (output, provider, prompt_version, usage, reused_from, execution_id) = match plan {
        aigov::ExecutionPlan::Reuse(prior) => {
            let output: DiagnosticOrderSuggestionV1 = prior.output_as()?;
            (
                output,
                ProviderInfo {
                    provider: prior.route.clone(),
                    model: prior.model.clone(),
                    model_version: prior.model_version.clone(),
                },
                prior.prompt_version.clone(),
                prior.usage_as(),
                Some(prior.id),
                None,
            )
        }
        aigov::ExecutionPlan::Execute { execution_id, .. } => {
            match state.gateway.suggest_orders(&req).await {
                Ok(resp) => (
                    resp.output,
                    resp.provider,
                    resp.prompt_version,
                    resp.usage,
                    None,
                    Some(execution_id),
                ),
                Err(err) => {
                    record_generation_failure(&state, &ctx, e.patient_id, &err).await?;
                    return Err(aigov::gateway_error(err));
                }
            }
        }
    };
    // Defence in depth: only supplied orderables and facts may be cited.
    output
        .validate(&candidate_ids, &fact_refs)
        .map_err(|m| ApiError::internal(format!("suggestion validation: {m}")))?;

    let mut tx = state.pool.begin().await?;
    let locked = load_encounter(&mut tx, ctx.tenant_id, encounter_id, true).await?;
    require_order_context(&locked, &ctx)?;
    let artifact_id = Uuid::now_v7();
    sqlx::query(
        "UPDATE ai_artifacts SET status = $1
         WHERE tenant_id = $2 AND encounter_id = $3 AND artifact_type = 'diagnostic_order_suggestion' AND status = $4",
    )
    .bind(ArtifactStatus::Superseded.as_str())
    .bind(e.tenant_id)
    .bind(e.id)
    .bind(ArtifactStatus::AwaitingReview.as_str())
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO ai_artifacts
         (id, tenant_id, patient_id, encounter_id, artifact_type, autonomy_level, status,
          model, model_version, route, template, input_hash, output, output_schema,
          citations, limitations, generated_at)
         VALUES ($1,$2,$3,$4,'diagnostic_order_suggestion','A2',$5,$6,$7,$8,$9,$10,$11,$12,$13,$14, now())",
    )
    .bind(artifact_id)
    .bind(e.tenant_id)
    .bind(e.patient_id)
    .bind(e.id)
    .bind(ArtifactStatus::AwaitingReview.as_str())
    .bind(&provider.model)
    .bind(&provider.model_version)
    .bind(&provider.provider)
    .bind(ORDER_SUGGESTION_TEMPLATE)
    .bind(&hash)
    .bind(serde_json::to_value(&output).map_err(ApiError::internal)?)
    .bind(ORDER_SUGGESTION_SCHEMA)
    .bind(serde_json::to_value(&output.cited_sources).map_err(ApiError::internal)?)
    .bind(serde_json::to_value(&output.limitations).map_err(ApiError::internal)?)
    .execute(&mut *tx)
    .await?;
    aigov::annotate(
        &mut tx,
        artifact_id,
        &aigov::Provenance {
            scope,
            provider: &provider,
            prompt_version: &prompt_version,
            input_refs: &fact_refs,
            usage: usage.as_ref(),
            synthetic,
            reused_from,
        },
    )
    .await?;
    if let Some(execution_id) = execution_id {
        aigov::bind_execution(&mut tx, execution_id, artifact_id).await?;
    }
    audit::emit(
        &mut *tx,
        &ctx,
        "ai.artifact.generated",
        &state.cell,
        json!({
            "artifact_id": artifact_id, "encounter_id": e.id, "patient_id": e.patient_id,
            "template": ORDER_SUGGESTION_TEMPLATE, "prompt_version": prompt_version,
            "model": provider.model, "input_hash": hash, "reused_from": reused_from,
            "synthetic": synthetic, "suggestions": output.suggestions.len(),
        }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    tx.commit().await?;

    let suggestions: Vec<Value> = output
        .suggestions
        .iter()
        .map(|s| {
            let o = orderables.iter().find(|o| o.id == s.orderable_id);
            json!({
                "orderable_id": s.orderable_id,
                "code": o.map(|o| o.code.clone()),
                "name": o.map(|o| o.name(&lang).to_string()),
                "category_code": o.map(|o| o.config.category_code.clone()),
                "default_fulfilment_mode": o.map(|o| o.config.default_mode()),
                "rationale": s.rationale,
                "cited_sources": s.cited_sources,
                "proposed_timing": s.proposed_timing,
                "preparation_note": s.preparation_note,
            })
        })
        .collect();
    Ok(Json(json!({
        "artifact_id": artifact_id,
        "status": ArtifactStatus::AwaitingReview.as_str(),
        "autonomy_level": "A2",
        "schema_version": output.schema_version,
        "suggestions": suggestions,
        "duplicate_warnings": output.duplicate_warnings,
        "missing_information": output.missing_information,
        "cited_sources": output.cited_sources,
        "confidence": output.confidence,
        "limitations": output.limitations,
        "provider": provider,
        "prompt_version": prompt_version,
        "reused_from": reused_from,
        "synthetic": synthetic,
    })))
}

async fn record_generation_failure(
    state: &AppState,
    ctx: &AuthContext,
    patient_id: Uuid,
    err: &GatewayError,
) -> Result<(), ApiError> {
    audit::record(
        &state.pool,
        ctx,
        "ai.generation.failed",
        Some("patient"),
        Some(patient_id.to_string()),
        "deny",
        Some(match err {
            GatewayError::Unavailable(_) => "provider_unavailable",
            GatewayError::Disabled(_) => "provider_disabled",
            GatewayError::InvalidOutput(_) => "invalid_output",
            GatewayError::PolicyDenied(_) => "policy_denied",
        }),
    )
    .await
    .map_err(ApiError::internal)
}

// ---------------------------------------------------------------------------
// Confirmation
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ConfirmBody {
    #[serde(flatten)]
    pub composition: PreflightBody,
    pub safety_evaluation_id: Uuid,
    #[serde(default)]
    pub acknowledged_ids: Vec<String>,
    pub override_reason: Option<String>,
    pub clinical_indication: Option<String>,
    pub clinical_question: Option<String>,
    pub suggestion_artifact_id: Option<Uuid>,
    pub idempotency_key: Option<String>,
    /// Create Access requests for appointment-based orders (default). `false`
    /// leaves them `placed` for staff to schedule later.
    #[serde(default = "default_true")]
    pub schedule: bool,
}

fn default_true() -> bool {
    true
}

async fn group_orders(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    group_id: Uuid,
) -> Result<Vec<OrderRow>, ApiError> {
    let rows = sqlx::query(&format!(
        "SELECT {} FROM service_requests sr JOIN patients p ON p.id = sr.patient_id
         WHERE sr.tenant_id = $1 AND sr.order_group_id = $2 ORDER BY sr.created_at, sr.id",
        super::ORDER_COLUMNS
    ))
    .bind(tenant_id)
    .bind(group_id)
    .fetch_all(&mut *conn)
    .await?;
    rows.iter().map(super::order_from_row).collect()
}

async fn group_json(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    group_id: Uuid,
    replayed: bool,
) -> Result<Value, ApiError> {
    let g = sqlx::query(
        "SELECT id, patient_id, encounter_id, requester_id, clinical_indication, clinical_question, priority,
                safety_evaluation_id, suggestion_artifact_id, created_at
         FROM diagnostic_order_groups WHERE tenant_id = $1 AND id = $2",
    )
    .bind(tenant_id)
    .bind(group_id)
    .fetch_one(&mut *conn)
    .await?;
    let orders = group_orders(conn, tenant_id, group_id).await?;
    Ok(json!({
        "id": g.get::<Uuid, _>("id"),
        "patient_id": g.get::<Uuid, _>("patient_id"),
        "encounter_id": g.get::<Uuid, _>("encounter_id"),
        "requester_id": g.get::<Uuid, _>("requester_id"),
        "clinical_indication": g.get::<Option<String>, _>("clinical_indication"),
        "clinical_question": g.get::<Option<String>, _>("clinical_question"),
        "priority": g.get::<String, _>("priority"),
        "safety_evaluation_id": g.get::<Option<Uuid>, _>("safety_evaluation_id"),
        "suggestion_artifact_id": g.get::<Option<Uuid>, _>("suggestion_artifact_id"),
        "created_at": g.get::<DateTime<Utc>, _>("created_at"),
        "orders": orders.iter().map(order_json).collect::<Vec<_>>(),
        "replayed": replayed,
    }))
}

/// `POST /api/v1/encounters/:id/diagnostic-orders` — the only writer of
/// diagnostic orders. Re-runs the safety engine under the encounter lock and
/// refuses when the persisted evaluation no longer matches.
pub async fn confirm(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(encounter_id): Path<Uuid>,
    Json(body): Json<ConfirmBody>,
) -> Result<Json<Value>, ApiError> {
    let lang = lang_of(body.composition.lang.as_deref());
    let indication = check_len(
        "clinical_indication",
        body.clinical_indication.as_deref(),
        MAX_TEXT,
    )?;
    let question = check_len(
        "clinical_question",
        body.clinical_question.as_deref(),
        MAX_TEXT,
    )?;
    let override_reason = check_len("override_reason", body.override_reason.as_deref(), MAX_TEXT)?;
    let idempotency_key = check_len("idempotency_key", body.idempotency_key.as_deref(), 128)?;
    let mut conn = state.pool.acquire().await?;
    let e = load_encounter(&mut conn, ctx.tenant_id, encounter_id, false).await?;
    drop(conn);
    let allowed = guard(
        &state,
        &ctx,
        actions::DIAGNOSTIC_ORDER_MANAGE,
        "diagnostic_order",
        Some(e.resource()),
    )
    .await?;
    require_order_context(&e, &ctx)?;

    let mut tx = state.pool.begin().await?;
    let e = load_encounter(&mut tx, ctx.tenant_id, encounter_id, true).await?;
    require_order_context(&e, &ctx)?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;

    if let Some(key) = &idempotency_key {
        if let Some(existing) = sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM diagnostic_order_groups
             WHERE tenant_id = $1 AND idempotency_key = $2 AND requester_id = $3 AND encounter_id = $4",
        )
        .bind(e.tenant_id)
        .bind(key)
        .bind(ctx.user_id)
        .bind(e.id)
        .fetch_optional(&mut *tx)
        .await?
        {
            let out = group_json(&mut tx, e.tenant_id, existing, true).await?;
            tx.commit().await?;
            return Ok(Json(json!({ "group": out })));
        }
        let clash: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM diagnostic_order_groups WHERE tenant_id = $1 AND idempotency_key = $2",
        )
        .bind(e.tenant_id)
        .bind(key)
        .fetch_optional(&mut *tx)
        .await?;
        if clash.is_some() {
            return Err(ApiError::conflict(
                "idempotency_conflict",
                "this idempotency key was used for a different order group",
            ));
        }
    }

    let now = Utc::now();
    let composed = compose(&mut tx, &e, &body.composition, now).await?;
    let ev = sqlx::query(
        "SELECT input_hash, engine_version FROM diagnostic_safety_evaluations
         WHERE id = $1 AND tenant_id = $2 AND encounter_id = $3 AND patient_id = $4 FOR UPDATE",
    )
    .bind(body.safety_evaluation_id)
    .bind(e.tenant_id)
    .bind(e.id)
    .bind(e.patient_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| {
        ApiError::conflict(
            "safety_evaluation_missing",
            "run the safety preflight for this consultation before confirming",
        )
    })?;
    if ev.get::<String, _>("input_hash") != composed.fingerprint
        || ev.get::<String, _>("engine_version") != composed.evaluation.engine_version
    {
        return Err(ApiError::conflict(
            "safety_evaluation_stale",
            "the order, patient facts or answers changed since the safety preflight; run it again",
        ));
    }
    let acknowledged: BTreeSet<String> = body
        .acknowledged_ids
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let has_hard_stops = !composed.evaluation.hard_stops().is_empty();
    if let Err(block) = composed
        .evaluation
        .confirmable(&acknowledged, override_reason.as_deref())
    {
        return Err(match block {
            SafetyBlock::Unacknowledged(ids) => ApiError::conflict(
                "safety_unacknowledged",
                format!("acknowledge the safety warnings first: {}", ids.join(", ")),
            ),
            SafetyBlock::HardStops(ids) => ApiError::conflict(
                "safety_hard_stop",
                format!(
                    "hard stops require correction or an authorized override reason (10+ characters): {}",
                    ids.join(", ")
                ),
            ),
        });
    }
    if has_hard_stops {
        // Override authority is a separate grant, checked here against the
        // same resource and purpose; the reason is persisted and audited.
        guard(
            &state,
            &ctx,
            actions::DIAGNOSTIC_SAFETY_OVERRIDE,
            "diagnostic_safety_evaluation",
            Some(e.resource()),
        )
        .await?
        .record(&mut tx, &ctx, &state.cell)
        .await?;
    }
    let ack_vec: Vec<String> = acknowledged.iter().cloned().collect();
    sqlx::query(
        "UPDATE diagnostic_safety_evaluations
         SET acknowledged_ids = $2,
             override_reason = CASE WHEN $3 THEN $4 ELSE override_reason END,
             overridden_by = CASE WHEN $3 THEN $5 ELSE overridden_by END,
             overridden_at = CASE WHEN $3 THEN now() ELSE overridden_at END
         WHERE id = $1",
    )
    .bind(body.safety_evaluation_id)
    .bind(&ack_vec)
    .bind(has_hard_stops)
    .bind(&override_reason)
    .bind(ctx.user_id)
    .execute(&mut *tx)
    .await?;
    if has_hard_stops {
        audit::emit(
            &mut *tx,
            &ctx,
            "diagnostic.safety.overridden",
            &state.cell,
            json!({
                "safety_evaluation_id": body.safety_evaluation_id,
                "encounter_id": e.id,
                "patient_id": e.patient_id,
                "hard_stops": composed.evaluation.hard_stops().iter().map(|f| f.id.clone()).collect::<Vec<_>>(),
                "override_reason": override_reason,
            }),
            None,
        )
        .await
        .map_err(ApiError::internal)?;
    }

    if let Some(artifact_id) = body.suggestion_artifact_id {
        let ok = sqlx::query(
            "SELECT status FROM ai_artifacts
             WHERE id = $1 AND tenant_id = $2 AND patient_id = $3 AND encounter_id = $4
               AND artifact_type = 'diagnostic_order_suggestion' FOR UPDATE",
        )
        .bind(artifact_id)
        .bind(e.tenant_id)
        .bind(e.patient_id)
        .bind(e.id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| {
            ApiError::bad_request(
                "validation_failed",
                "suggestion_artifact_id is not a suggestion for this consultation",
            )
        })?;
        if ok.get::<String, _>("status") == ArtifactStatus::AwaitingReview.as_str() {
            sqlx::query(
                "UPDATE ai_artifacts SET status = $2, reviewer_id = $3, review_decision = 'accepted',
                        reviewed_at = now(), review_note = 'orders confirmed by clinician'
                 WHERE id = $1",
            )
            .bind(artifact_id)
            .bind(ArtifactStatus::Approved.as_str())
            .bind(ctx.user_id)
            .execute(&mut *tx)
            .await?;
            audit::emit(
                &mut *tx,
                &ctx,
                "ai.artifact.reviewed",
                &state.cell,
                json!({ "artifact_id": artifact_id, "decision": "accepted", "encounter_id": e.id }),
                None,
            )
            .await
            .map_err(ApiError::internal)?;
        }
    }

    let group_priority =
        parse_priority(body.composition.priority.as_deref(), OrderPriority::Routine)?;
    let group_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO diagnostic_order_groups
         (id, tenant_id, patient_id, encounter_id, requester_id, clinical_indication, clinical_question,
          priority, safety_evaluation_id, suggestion_artifact_id, idempotency_key)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
    )
    .bind(group_id)
    .bind(e.tenant_id)
    .bind(e.patient_id)
    .bind(e.id)
    .bind(ctx.user_id)
    .bind(&indication)
    .bind(&question)
    .bind(group_priority.as_str())
    .bind(body.safety_evaluation_id)
    .bind(body.suggestion_artifact_id)
    .bind(&idempotency_key)
    .execute(&mut *tx)
    .await?;

    let actor = actor_label(&ctx);
    let mut placed: Vec<OrderRow> = Vec::with_capacity(composed.candidates.len());
    for (idx, (cand, o)) in composed
        .candidates
        .iter()
        .zip(composed.orderables.iter())
        .enumerate()
    {
        let order_id = Uuid::now_v7();
        let item_key = idempotency_key.as_ref().map(|k| format!("{k}:{idx}"));
        sqlx::query(
            "INSERT INTO service_requests
             (id, tenant_id, patient_id, encounter_id, requester_id, code_loinc, display, loop_state,
              orderable_id, orderable_code, orderable_version, order_group_id, category_code, modality_code,
              expected_result_type, order_status, fulfilment_mode, priority, clinical_indication,
              clinical_question, requested_window_start, requested_window_end, preparation_en, preparation_es,
              performing_facility_id, performing_service_code, idempotency_key, source_system)
             VALUES ($1,$2,$3,$4,$5,$6,$7,'ordered',$8,$9,$10,$11,$12,$13,$14,'placed',$15,$16,$17,$18,$19,$20,
                     $21,$22,$23,$24,$25,'wellos')",
        )
        .bind(order_id)
        .bind(e.tenant_id)
        .bind(e.patient_id)
        .bind(e.id)
        .bind(ctx.user_id)
        .bind(o.loinc())
        .bind(&o.name_en)
        .bind(o.id)
        .bind(&o.code)
        .bind(o.version)
        .bind(group_id)
        .bind(&o.config.category_code)
        .bind(&o.config.modality_code)
        .bind(o.config.result_type.as_str())
        .bind(cand.fulfilment_mode.as_str())
        .bind(cand.priority.as_str())
        .bind(&indication)
        .bind(&question)
        .bind(cand.requested_window_start)
        .bind(cand.requested_window_end)
        .bind(&o.config.preparation_en)
        .bind(&o.config.preparation_es)
        .bind(composed.performing_facility_id)
        .bind(&o.config.scheduling_service_code)
        .bind(&item_key)
        .execute(&mut *tx)
        .await?;
        let row = load_order(&mut tx, e.tenant_id, order_id).await?;
        record_history(
            &mut tx,
            &row,
            None,
            OrderStatus::Placed,
            row.version,
            None,
            &actor,
            Some(ctx.user_id),
            json!({
                "group_id": group_id,
                "safety_evaluation_id": body.safety_evaluation_id,
                "fulfilment_mode": cand.fulfilment_mode.as_str(),
                "priority": cand.priority.as_str(),
            }),
        )
        .await?;
        audit::emit(
            &mut *tx,
            &ctx,
            "diagnostic_order.placed",
            &state.cell,
            json!({
                "service_request_id": order_id, "group_id": group_id, "patient_id": e.patient_id,
                "encounter_id": e.id, "orderable_code": o.code, "fulfilment_mode": cand.fulfilment_mode.as_str(),
                "priority": cand.priority.as_str(), "overridden": has_hard_stops,
            }),
            None,
        )
        .await
        .map_err(ApiError::internal)?;
        placed.push(row);
    }

    if body.schedule {
        for (row, cand) in placed.iter().zip(composed.candidates.iter()) {
            if !cand.fulfilment_mode.requires_appointment() {
                continue;
            }
            let Some(service_code) = cand.config.scheduling_service_code.clone() else {
                continue;
            };
            link_access_request(
                &mut tx,
                &ctx,
                &state,
                row,
                service_code,
                composed.performing_facility_id,
                cand.requested_window_start,
                cand.requested_window_end,
                &lang,
            )
            .await?;
        }
    }

    audit::emit(
        &mut *tx,
        &ctx,
        "diagnostic_order_group.confirmed",
        &state.cell,
        json!({
            "group_id": group_id, "patient_id": e.patient_id, "encounter_id": e.id,
            "orders": placed.len(), "safety_evaluation_id": body.safety_evaluation_id,
            "warnings_acknowledged": ack_vec.len(), "hard_stops_overridden": has_hard_stops,
            "suggestion_artifact_id": body.suggestion_artifact_id,
        }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    let out = group_json(&mut tx, e.tenant_id, group_id, false).await?;
    tx.commit().await?;
    Ok(Json(json!({ "group": out })))
}

/// Create (and submit) the Access request that will book the appointment for
/// one schedulable order, through the ordinary request path: deterministic
/// triage floor, matcher, offers, holds and confirmation all stay in Access.
#[allow(clippy::too_many_arguments)]
async fn link_access_request(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ctx: &AuthContext,
    state: &AppState,
    row: &OrderRow,
    service_code: String,
    facility_id: Uuid,
    earliest: Option<DateTime<Utc>>,
    latest: Option<DateTime<Utc>>,
    lang: &str,
) -> Result<(), ApiError> {
    // The ordering clinician is the accountable human who set the priority,
    // so the Access request carries the highest urgency a human triage
    // decision can assign (`priority`); `urgent` is reserved for untriaged
    // intake and would send the order back to clinical triage.
    let urgency = match row.priority {
        OrderPriority::Stat | OrderPriority::Urgent | OrderPriority::Timed => {
            Some("priority".to_string())
        }
        OrderPriority::Routine => None,
    };
    let free_text = if lang == "es" {
        format!("Orden diagnóstica: {}", row.display)
    } else {
        format!("Diagnostic order: {}", row.display)
    };
    let body = CreateRequestBody {
        patient_id: row.patient_id,
        facility_id: Some(facility_id),
        free_text: Some(free_text),
        constraints: ConstraintsInput {
            service_code: Some(service_code),
            facility_ids: Some(vec![facility_id]),
            modality_codes: None,
            earliest,
            latest,
            has_referral: Some(true),
            ..ConstraintsInput::default()
        },
        urgency,
        submit: true,
        idempotency_key: Some(format!("diagnostic-order:{}", row.id)),
    };
    let request = access::create_request_in(tx, ctx, state, row.patient_id, "staff", body).await?;
    sqlx::query(
        "UPDATE service_requests SET access_request_id = $2, schedule_conflict = NULL, updated_at = now()
         WHERE id = $1",
    )
    .bind(row.id)
    .bind(request.id)
    .execute(&mut **tx)
    .await?;
    record_history(
        tx,
        row,
        Some(row.order_status),
        row.order_status,
        row.version,
        None,
        &actor_label(ctx),
        Some(ctx.user_id),
        json!({ "event": "access_request_linked", "access_request_id": request.id }),
    )
    .await?;
    Ok(())
}

/// Safety evaluation bound to an order group, for the order detail view.
pub async fn group_safety_json(conn: &mut PgConnection, group_id: Uuid) -> Result<Value, ApiError> {
    let r = sqlx::query(
        "SELECT s.id, s.engine_version, s.input_hash, s.findings, s.hard_stops, s.warnings, s.acknowledged_ids,
                s.override_reason, s.overridden_by, s.overridden_at, s.evaluated_by, s.evaluated_at
         FROM diagnostic_order_groups g JOIN diagnostic_safety_evaluations s ON s.id = g.safety_evaluation_id
         WHERE g.id = $1",
    )
    .bind(group_id)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(match r {
        None => Value::Null,
        Some(r) => json!({
            "id": r.get::<Uuid, _>("id"),
            "engine_version": r.get::<String, _>("engine_version"),
            "input_hash": r.get::<String, _>("input_hash"),
            "findings": r.get::<Value, _>("findings"),
            "hard_stops": r.get::<i32, _>("hard_stops"),
            "warnings": r.get::<i32, _>("warnings"),
            "acknowledged_ids": r.get::<Vec<String>, _>("acknowledged_ids"),
            "override_reason": r.get::<Option<String>, _>("override_reason"),
            "overridden_by": r.get::<Option<Uuid>, _>("overridden_by"),
            "overridden_at": r.get::<Option<DateTime<Utc>>, _>("overridden_at"),
            "evaluated_by": r.get::<Uuid, _>("evaluated_by"),
            "evaluated_at": r.get::<DateTime<Utc>, _>("evaluated_at"),
        }),
    })
}

/// `GET /api/v1/encounters/:id/diagnostic-orders`
pub async fn list_for_encounter(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(encounter_id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let e = load_encounter(&mut conn, ctx.tenant_id, encounter_id, false).await?;
    guard(
        &state,
        &ctx,
        actions::DIAGNOSTIC_READ,
        "diagnostic_order",
        Some(e.resource()),
    )
    .await?
    .record(&mut conn, &ctx, &state.cell)
    .await?;
    let rows = sqlx::query(&format!(
        "SELECT {} FROM service_requests sr JOIN patients p ON p.id = sr.patient_id
         WHERE sr.tenant_id = $1 AND sr.encounter_id = $2 AND sr.orderable_id IS NOT NULL
         ORDER BY sr.created_at DESC, sr.id",
        super::ORDER_COLUMNS
    ))
    .bind(e.tenant_id)
    .bind(e.id)
    .fetch_all(&mut *conn)
    .await?;
    let orders = rows
        .iter()
        .map(super::order_from_row)
        .collect::<Result<Vec<_>, _>>()?;
    let groups = sqlx::query(
        "SELECT id, priority, clinical_indication, clinical_question, safety_evaluation_id, suggestion_artifact_id, created_at
         FROM diagnostic_order_groups WHERE tenant_id = $1 AND encounter_id = $2 ORDER BY created_at DESC",
    )
    .bind(e.tenant_id)
    .bind(e.id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(Json(json!({
        "orders": orders.iter().map(order_json).collect::<Vec<_>>(),
        "groups": groups.iter().map(|g| json!({
            "id": g.get::<Uuid, _>("id"),
            "priority": g.get::<String, _>("priority"),
            "clinical_indication": g.get::<Option<String>, _>("clinical_indication"),
            "clinical_question": g.get::<Option<String>, _>("clinical_question"),
            "safety_evaluation_id": g.get::<Option<Uuid>, _>("safety_evaluation_id"),
            "suggestion_artifact_id": g.get::<Option<Uuid>, _>("suggestion_artifact_id"),
            "created_at": g.get::<DateTime<Utc>, _>("created_at"),
        })).collect::<Vec<_>>(),
    })))
}

// ---------------------------------------------------------------------------
// Fulfilment transitions
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct TransitionBody {
    pub transition: String,
    pub version: i64,
    pub reason: Option<String>,
    /// Required to start an `accepted` order without an appointment
    /// (immediate / inpatient / bedside / walk-in), recorded on the order.
    pub fulfilment_mode: Option<String>,
}

fn parse_transition(s: &str) -> Result<(OrderTransition, &'static str, &'static str), ApiError> {
    Ok(match s.trim() {
        "accept" => (
            OrderTransition::Accept,
            actions::DIAGNOSTIC_FULFIL,
            "diagnostic_order.accepted",
        ),
        "start" => (
            OrderTransition::Start,
            actions::DIAGNOSTIC_FULFIL,
            "diagnostic_order.started",
        ),
        "complete" => (
            OrderTransition::Complete,
            actions::DIAGNOSTIC_FULFIL,
            "diagnostic_order.completed",
        ),
        "hold" => (
            OrderTransition::Hold,
            actions::DIAGNOSTIC_FULFIL,
            "diagnostic_order.held",
        ),
        "resume" => (
            OrderTransition::Resume,
            actions::DIAGNOSTIC_FULFIL,
            "diagnostic_order.resumed",
        ),
        "reject" => (
            OrderTransition::Reject,
            actions::DIAGNOSTIC_FULFIL,
            "diagnostic_order.rejected",
        ),
        "cancel" => (
            OrderTransition::Cancel,
            actions::DIAGNOSTIC_ORDER_MANAGE,
            "diagnostic_order.cancelled",
        ),
        "enter_in_error" => (
            OrderTransition::EnterInError,
            actions::DIAGNOSTIC_ORDER_MANAGE,
            "diagnostic_order.entered_in_error",
        ),
        "schedule" | "unschedule" => {
            return Err(ApiError::bad_request(
                "validation_failed",
                "scheduling transitions are driven by Access appointments",
            ))
        }
        _ => {
            return Err(ApiError::bad_request(
                "validation_failed",
                "unknown transition",
            ))
        }
    })
}

/// `POST /api/v1/diagnostics/orders/:id/transition`
pub async fn transition(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<TransitionBody>,
) -> Result<Json<Value>, ApiError> {
    let (t, action, event) = parse_transition(&body.transition)?;
    let reason = check_len("reason", body.reason.as_deref(), MAX_TEXT)?;
    let needs_reason = matches!(
        t,
        OrderTransition::Cancel
            | OrderTransition::Reject
            | OrderTransition::Hold
            | OrderTransition::EnterInError
    );
    if needs_reason && reason.as_deref().map_or(0, |r| r.chars().count()) < MIN_REASON_CHARS {
        return Err(ApiError::bad_request(
            "validation_failed",
            "a reason is required for this transition",
        ));
    }
    let requested_mode = parse_mode(body.fulfilment_mode.as_deref())?;
    let mut conn = state.pool.acquire().await?;
    let o = load_order(&mut conn, ctx.tenant_id, id).await?;
    drop(conn);
    let allowed = guard_order(&state, &ctx, action, &o).await?;
    if t == OrderTransition::Cancel && o.requester_id != ctx.user_id {
        // Cancelling someone else's order is still a clinician decision; the
        // policy grants it, the audit names the actor, and the reason is kept.
        tracing::info!(order = %o.id, actor = %ctx.user_id, "order cancelled by non-requesting clinician");
    }
    let mut tx = state.pool.begin().await?;
    let mut o = lock_order(&mut tx, ctx.tenant_id, id).await?;
    if o.version != body.version {
        return Err(ApiError::conflict(
            "version_conflict",
            "the order changed since it was displayed; reload it",
        ));
    }
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let mut details = json!({});
    if let Some(mode) = requested_mode {
        if mode != o.fulfilment_mode {
            if !matches!(t, OrderTransition::Accept | OrderTransition::Start) {
                return Err(ApiError::bad_request(
                    "validation_failed",
                    "the fulfilment mode can only be recorded when accepting or starting",
                ));
            }
            let allowed_modes = match o.orderable_id {
                Some(oid) => catalog::load_by_ids(&mut tx, o.tenant_id, &[oid])
                    .await?
                    .first()
                    .map(|c| c.config.allowed_modes())
                    .unwrap_or_default(),
                None => vec![FulfilmentMode::Immediate],
            };
            if !allowed_modes.contains(&mode) {
                return Err(ApiError::conflict(
                    "fulfilment_mode_not_allowed",
                    format!("this orderable cannot be fulfilled as {}", mode.as_str()),
                ));
            }
            if o.appointment_id.is_some() && !mode.requires_appointment() {
                return Err(ApiError::conflict(
                    "appointment_linked",
                    "the order has a booked appointment; cancel it in Access before changing the fulfilment mode",
                ));
            }
            sqlx::query(
                "UPDATE service_requests SET fulfilment_mode = $2, updated_at = now() WHERE id = $1",
            )
            .bind(o.id)
            .bind(mode.as_str())
            .execute(&mut *tx)
            .await?;
            details["fulfilment_mode_from"] = json!(o.fulfilment_mode.as_str());
            details["fulfilment_mode"] = json!(mode.as_str());
            o.fulfilment_mode = mode;
        }
    }
    if t == OrderTransition::Start
        && o.order_status == OrderStatus::Accepted
        && requested_mode.is_none()
    {
        return Err(ApiError::conflict(
            "fulfilment_mode_required",
            "starting without an appointment requires recording the fulfilment mode (immediate, inpatient, bedside or walk_in)",
        ));
    }
    if matches!(t, OrderTransition::Cancel | OrderTransition::EnterInError) {
        if let Some(appt) = o.appointment_id {
            details["appointment_id"] = json!(appt);
            details["appointment_action_required"] = json!(true);
        }
        if let Some(req) = o.access_request_id {
            details["access_request_id"] = json!(req);
        }
    }
    let actor = actor_label(&ctx);
    let updated = apply_transition_in(
        &mut tx,
        &ctx,
        &state,
        &o,
        TransitionInput {
            transition: t,
            reason: reason.as_deref(),
            actor: &actor,
            actor_user_id: Some(ctx.user_id),
            details,
            event,
        },
    )
    .await?;
    tx.commit().await?;
    let mut conn = state.pool.acquire().await?;
    Ok(Json(order_detail_json(&mut conn, &updated).await?))
}

// ---------------------------------------------------------------------------
// Staff scheduling of a placed / conflicted order
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct ScheduleBody {
    pub version: Option<i64>,
    pub facility_id: Option<Uuid>,
    pub earliest: Option<DateTime<Utc>>,
    pub latest: Option<DateTime<Utc>>,
    pub lang: Option<String>,
}

/// `POST /api/v1/diagnostics/orders/:id/schedule` — (re)creates the Access
/// request for an appointment-based order that has none open (never
/// scheduled, or whose appointment was cancelled / no-showed / closed).
pub async fn schedule(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    body: Option<Json<ScheduleBody>>,
) -> Result<Json<Value>, ApiError> {
    let body = body.map(|Json(b)| b).unwrap_or_default();
    let lang = lang_of(body.lang.as_deref());
    let mut conn = state.pool.acquire().await?;
    let o = load_order(&mut conn, ctx.tenant_id, id).await?;
    drop(conn);
    let allowed = match guard_order(&state, &ctx, actions::DIAGNOSTIC_FULFIL, &o).await {
        Ok(a) => a,
        Err(_) => guard_order(&state, &ctx, actions::DIAGNOSTIC_ORDER_MANAGE, &o).await?,
    };
    let mut tx = state.pool.begin().await?;
    let o = lock_order(&mut tx, ctx.tenant_id, id).await?;
    if let Some(v) = body.version {
        if v != o.version {
            return Err(ApiError::conflict(
                "version_conflict",
                "the order changed since it was displayed; reload it",
            ));
        }
    }
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    if !matches!(
        o.order_status,
        OrderStatus::Placed | OrderStatus::Accepted | OrderStatus::OnHold
    ) {
        return Err(ApiError::conflict(
            "order_not_schedulable",
            "only placed, accepted or on-hold orders can be scheduled",
        ));
    }
    if !o.fulfilment_mode.requires_appointment() {
        return Err(ApiError::conflict(
            "order_not_schedulable",
            "this order is fulfilled without an appointment",
        ));
    }
    let Some(service_code) = o.performing_service_code.clone() else {
        return Err(ApiError::conflict(
            "scheduling_service_missing",
            "the orderable has no scheduling service mapped in the catalog",
        ));
    };
    if let Some(req) = o.access_request_id {
        let open: Option<String> = sqlx::query_scalar(
            "SELECT status FROM access_requests WHERE id = $1 AND tenant_id = $2",
        )
        .bind(req)
        .bind(o.tenant_id)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(status) = open {
            if o.schedule_conflict.is_none()
                && !matches!(
                    status.as_str(),
                    "cancelled" | "expired" | "closed" | "fulfilled" | "booked"
                )
            {
                return Err(ApiError::conflict(
                    "access_request_open",
                    "an Access request is already open for this order",
                ));
            }
            if status == "booked" && o.schedule_conflict.is_none() && o.appointment_id.is_some() {
                return Err(ApiError::conflict(
                    "appointment_linked",
                    "this order already has a booked appointment",
                ));
            }
        }
    }
    let facility_id = body
        .facility_id
        .or(o.performing_facility_id)
        .unwrap_or(o.patient_facility_id);
    if body.facility_id.is_some() && Some(facility_id) != o.performing_facility_id {
        let ok: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM facilities WHERE id = $1 AND tenant_id = $2")
                .bind(facility_id)
                .bind(o.tenant_id)
                .fetch_optional(&mut *tx)
                .await?;
        if ok.is_none() {
            return Err(ApiError::bad_request(
                "validation_failed",
                "facility_id is not a facility of this tenant",
            ));
        }
        sqlx::query(
            "UPDATE service_requests SET performing_facility_id = $2, updated_at = now() WHERE id = $1",
        )
        .bind(o.id)
        .bind(facility_id)
        .execute(&mut *tx)
        .await?;
    }
    // A fresh request per attempt: the idempotency key carries the order
    // version so a conflicted order can be rebooked after each conflict.
    let mut row = o.clone();
    row.performing_facility_id = Some(facility_id);
    let request = {
        let urgency = match row.priority {
            OrderPriority::Stat | OrderPriority::Urgent => Some("urgent".to_string()),
            OrderPriority::Timed => Some("priority".to_string()),
            OrderPriority::Routine => None,
        };
        let free_text = if lang == "es" {
            format!("Orden diagnóstica: {}", row.display)
        } else {
            format!("Diagnostic order: {}", row.display)
        };
        let body = CreateRequestBody {
            patient_id: row.patient_id,
            facility_id: Some(facility_id),
            free_text: Some(free_text),
            constraints: ConstraintsInput {
                service_code: Some(service_code),
                facility_ids: Some(vec![facility_id]),
                modality_codes: row.modality_code.clone().map(|m| vec![m]),
                earliest: body.earliest.or(row.requested_window_start),
                latest: body.latest.or(row.requested_window_end),
                ..ConstraintsInput::default()
            },
            urgency,
            submit: true,
            idempotency_key: Some(format!("diagnostic-order:{}:v{}", row.id, row.version)),
        };
        access::create_request_in(&mut tx, &ctx, &state, row.patient_id, "staff", body).await?
    };
    sqlx::query(
        "UPDATE service_requests
         SET access_request_id = $2, appointment_id = NULL, schedule_conflict = NULL,
             version = version + 1, updated_at = now()
         WHERE id = $1 AND version = $3",
    )
    .bind(o.id)
    .bind(request.id)
    .bind(o.version)
    .execute(&mut *tx)
    .await?;
    record_history(
        &mut tx,
        &o,
        Some(o.order_status),
        o.order_status,
        o.version + 1,
        None,
        &actor_label(&ctx),
        Some(ctx.user_id),
        json!({ "event": "access_request_linked", "access_request_id": request.id,
                "previous_conflict": o.schedule_conflict, "facility_id": facility_id }),
    )
    .await?;
    audit::emit(
        &mut *tx,
        &ctx,
        "diagnostic_order.scheduling_requested",
        &state.cell,
        json!({ "service_request_id": o.id, "access_request_id": request.id, "patient_id": o.patient_id }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    let updated = load_order(&mut tx, o.tenant_id, o.id).await?;
    tx.commit().await?;
    let mut conn = state.pool.acquire().await?;
    let mut out = order_detail_json(&mut conn, &updated).await?;
    out["history"] = json!(history_json(&mut conn, updated.id).await?);
    Ok(Json(out))
}
