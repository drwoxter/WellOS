//! Patient 360, the explainable deterministic risk engine and the governed
//! dMind Risk Agent.
//!
//! Risk is computed by `wellos_domain::risk` from the patient's existing
//! records and stored as append-only snapshots (`risk_assessments`); the
//! newest snapshot is `is_current`. Review state (`risk_reviews`) is bound to
//! the level that was reviewed, so a level that rises afterwards is shown as
//! unreviewed again. dMind summaries are AI artifacts bound to one snapshot
//! and follow the same review lifecycle as every other dMind proposal; a
//! suggestion becomes a follow-up task only through an explicit confirmation
//! by a professional. The insurer projection exposes levels, versions,
//! provenance and review state only — never notes — and every access is an
//! audited event.

use super::{brief, guard, patients};
use crate::aigov;
use crate::audit;
use crate::auth::AuthContext;
use crate::error::ApiError;
use crate::policy::{actions, facility_scope, roles, ResourceCtx};
use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Duration, NaiveDate, Utc};
use dmind_gateway::risk::{RiskSummaryRequest, RISK_TEMPLATE};
use dmind_gateway::{GatewayError, Operation};
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::{PgConnection, Row};
use std::collections::BTreeMap;
use uuid::Uuid;
use wellos_domain::ai::{ArtifactStatus, ProviderInfo, ReviewDecision};
use wellos_domain::risk::{
    self as risk_rules, AlertFact, AllergyFact, CareTeamFact, ConditionFact, EncounterFact,
    MedicationFact, PreviousAssessment, ResultFact, RiskAssessment, RiskDomain, RiskInput,
    RiskLevel, RiskReviewStatus, RiskSummaryV1, RiskTrend, ServiceRequestFact, TaskFact, VisitFact,
    VitalsFact, RISK_RULES_VERSION, RISK_SUMMARY_PROMPT_VERSION, RISK_SUMMARY_SCHEMA,
};
use wellos_domain::triage::TriageVitals;

const MAX_NOTE: usize = 2000;
const HISTORY_LIMIT: i64 = 12;
const WORKLIST_LIMIT: i64 = 200;
/// Cap on *closed* historical records (used only for frequency and freshness
/// rules). Currently actionable records — open alerts, open visits, open
/// result loops, pending requests, in-progress encounters, open tasks — are
/// always loaded in full so a critical signal can never fall outside the cap.
const HISTORY_FACT_LIMIT: i64 = 200;
const VISIT_LOOKBACK_DAYS: i64 = 365;
/// Trend compares the new levels with the most recent snapshot at least this
/// old (falling back to the oldest snapshot), so that recalculating twice in a
/// row does not erase a real change.
const TREND_BASELINE_DAYS: i64 = 7;
/// Care-team function of the professional who owns risk follow-up for a
/// patient (assigned from the worklist).
pub const RISK_FOLLOW_UP_FUNCTION: &str = "risk_follow_up";
/// Consent purpose the insurer projection requires before sharing levels.
pub const PROJECTION_CONSENT_PURPOSE: &str = "insurer_risk_sharing";
const OVERALL_DOMAIN: &str = "overall";
const ARTIFACT_TYPE: &str = "risk_summary";

// ---------------------------------------------------------------------------
// Patient loading and authorization context
// ---------------------------------------------------------------------------

struct PatientRow {
    id: Uuid,
    tenant_id: Uuid,
    facility_id: Uuid,
    row: sqlx::postgres::PgRow,
}

async fn load_patient(state: &AppState, id: Uuid) -> Result<PatientRow, ApiError> {
    let row = sqlx::query(
        "SELECT id, tenant_id, facility_id, family_name, given_name, birth_date, sex, identifier
         FROM patients WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(ApiError::not_found)?;
    Ok(PatientRow {
        id: row.get("id"),
        tenant_id: row.get("tenant_id"),
        facility_id: row.get("facility_id"),
        row,
    })
}

fn resource_ctx(p: &PatientRow) -> ResourceCtx {
    ResourceCtx {
        tenant_id: p.tenant_id,
        patient_id: Some(p.id),
        facility_id: Some(p.facility_id),
    }
}

fn clean_note(value: Option<String>) -> Result<Option<String>, ApiError> {
    let Some(v) = value else { return Ok(None) };
    let trimmed = v.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.chars().count() > MAX_NOTE {
        return Err(ApiError::bad_request(
            "validation_failed",
            format!("note exceeds {MAX_NOTE} characters"),
        ));
    }
    Ok(Some(trimmed.to_string()))
}

fn parse_domain_key(s: &str) -> Result<String, ApiError> {
    if s == OVERALL_DOMAIN {
        return Ok(OVERALL_DOMAIN.to_string());
    }
    RiskDomain::parse(s)
        .map(|d| d.as_str().to_string())
        .ok_or_else(|| ApiError::bad_request("validation_failed", "unknown risk domain"))
}

/// Serialize one patient-level advisory lock so concurrent recalculations of
/// the same patient (manual, encounter sign, result review) commit one
/// `is_current` snapshot at a time instead of racing on the unique index.
async fn lock_patient_risk(conn: &mut PgConnection, patient_id: Uuid) -> Result<(), ApiError> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('risk:' || $1::text, 0))")
        .bind(patient_id.to_string())
        .execute(&mut *conn)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Fact collection (read-only; every fact points at its source record)
// ---------------------------------------------------------------------------

const OPEN_VISIT_STATUSES: &str =
    "('scheduled','arrived','triage_in_progress','ready_for_consultation','in_consultation')";

/// Operational precedence when a patient has several open visits: a visit the
/// patient is physically in (consultation, ready, triage, arrived) always wins
/// over a booking, however recent the booking is. Among bookings the one
/// closest to `now` is current.
fn visit_precedence(status: &str) -> u8 {
    match status {
        "in_consultation" => 0,
        "ready_for_consultation" => 1,
        "triage_in_progress" => 2,
        "arrived" => 3,
        "scheduled" => 4,
        _ => u8::MAX,
    }
}

pub(crate) fn select_current_visit(visits: &[VisitFact], now: DateTime<Utc>) -> Option<VisitFact> {
    visits
        .iter()
        .filter(|v| visit_precedence(&v.status) != u8::MAX)
        .min_by_key(|v| {
            let distance = if v.status == "scheduled" {
                (v.occurred_at - now).num_seconds().abs()
            } else {
                (now - v.occurred_at).num_seconds()
            };
            (visit_precedence(&v.status), distance, v.id)
        })
        .cloned()
}

pub(crate) async fn collect_input(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    patient_id: Uuid,
    birth_date: NaiveDate,
    now: DateTime<Utc>,
) -> Result<RiskInput, ApiError> {
    // Only open alerts feed the rules, and every one of them must be present.
    let alerts = sqlx::query(
        "SELECT id, severity, status, message, created_at FROM alerts
         WHERE tenant_id = $1 AND patient_id = $2 AND status = 'open'
         ORDER BY created_at DESC",
    )
    .bind(tenant_id)
    .bind(patient_id)
    .fetch_all(&mut *conn)
    .await?
    .iter()
    .map(|r| AlertFact {
        id: r.get("id"),
        severity: r.get("severity"),
        status: r.get("status"),
        message: r.get("message"),
        created_at: r.get("created_at"),
    })
    .collect();

    // Every open visit is loaded (the current visit is chosen among them);
    // closed visits are bounded history used for frequency rules only.
    let visit_rows = sqlx::query(&format!(
        "WITH v AS (
            SELECT v.id, v.status, v.arrival_kind, v.service, v.priority,
                   (SELECT ta.safety_floor FROM triage_assessments ta
                    WHERE ta.tenant_id = v.tenant_id AND ta.visit_id = v.id) AS safety_floor,
                   COALESCE(v.arrived_at, v.scheduled_at, v.created_at) AS occurred_at
            FROM visits v
            WHERE v.tenant_id = $1 AND v.patient_id = $2
         )
         (SELECT * FROM v WHERE status IN {OPEN_VISIT_STATUSES})
         UNION ALL
         (SELECT * FROM v WHERE status NOT IN {OPEN_VISIT_STATUSES} AND occurred_at >= $3
          ORDER BY occurred_at DESC LIMIT $4)"
    ))
    .bind(tenant_id)
    .bind(patient_id)
    .bind(now - chrono::Duration::days(VISIT_LOOKBACK_DAYS))
    .bind(HISTORY_FACT_LIMIT)
    .fetch_all(&mut *conn)
    .await?;
    let mut visits: Vec<VisitFact> = visit_rows
        .iter()
        .map(|r| VisitFact {
            id: r.get("id"),
            status: r.get("status"),
            arrival_kind: r.get("arrival_kind"),
            service: r.get("service"),
            priority: r.get("priority"),
            safety_floor: r.get("safety_floor"),
            occurred_at: r.get("occurred_at"),
        })
        .collect();
    visits.sort_by(|a, b| b.occurred_at.cmp(&a.occurred_at).then(b.id.cmp(&a.id)));
    let current_visit = select_current_visit(&visits, now);

    let latest_vitals = sqlx::query(
        "SELECT id, recorded_at, systolic_mmhg, heart_rate_bpm, respiratory_rate_bpm,
                temperature_c, spo2_percent
         FROM vital_signs WHERE tenant_id = $1 AND patient_id = $2
         ORDER BY recorded_at DESC, id DESC LIMIT 1",
    )
    .bind(tenant_id)
    .bind(patient_id)
    .fetch_optional(&mut *conn)
    .await?
    .map(|r| VitalsFact {
        id: r.get("id"),
        recorded_at: r.get("recorded_at"),
        vitals: TriageVitals {
            systolic_mmhg: r.get::<Option<Decimal>, _>("systolic_mmhg"),
            heart_rate_bpm: r.get::<Option<Decimal>, _>("heart_rate_bpm"),
            respiratory_rate_bpm: r.get::<Option<Decimal>, _>("respiratory_rate_bpm"),
            temperature_c: r.get::<Option<Decimal>, _>("temperature_c"),
            spo2_percent: r.get::<Option<Decimal>, _>("spo2_percent"),
        },
    });

    let conditions = sqlx::query(
        "SELECT id, code, display, clinical_status, recorded_at FROM conditions
         WHERE tenant_id = $1 AND patient_id = $2 ORDER BY recorded_at",
    )
    .bind(tenant_id)
    .bind(patient_id)
    .fetch_all(&mut *conn)
    .await?
    .iter()
    .map(|r| ConditionFact {
        id: r.get("id"),
        code: r.get("code"),
        display: r.get("display"),
        clinical_status: r.get("clinical_status"),
        recorded_at: r.get("recorded_at"),
    })
    .collect();
    let medications = sqlx::query(
        "SELECT id, name, status, recorded_at FROM medications
         WHERE tenant_id = $1 AND patient_id = $2 ORDER BY recorded_at",
    )
    .bind(tenant_id)
    .bind(patient_id)
    .fetch_all(&mut *conn)
    .await?
    .iter()
    .map(|r| MedicationFact {
        id: r.get("id"),
        name: r.get("name"),
        status: r.get("status"),
        recorded_at: r.get("recorded_at"),
    })
    .collect();
    let allergies = sqlx::query(
        "SELECT id, substance, criticality, recorded_at FROM allergies
         WHERE tenant_id = $1 AND patient_id = $2 ORDER BY recorded_at",
    )
    .bind(tenant_id)
    .bind(patient_id)
    .fetch_all(&mut *conn)
    .await?
    .iter()
    .map(|r| AllergyFact {
        id: r.get("id"),
        substance: r.get("substance"),
        criticality: r.get("criticality"),
        recorded_at: r.get("recorded_at"),
    })
    .collect();

    // Non-superseded observations; abnormality uses the same reference-range
    // classification as the Patient Brief. Every observation whose result loop
    // is still open is loaded; closed loops contribute the most recent result
    // per analyte (preventive intervals) plus bounded recent history.
    let results: Vec<ResultFact> = sqlx::query(
        "WITH o AS (
            SELECT o.id, o.service_request_id, o.code_loinc, sr.display, o.value_num, o.unit,
                   o.reference_range, sr.loop_state, o.effective_at,
                   EXISTS (SELECT 1 FROM rule_evaluations re
                           WHERE re.observation_id = o.id
                             AND re.outcome->>'outcome' = 'critical') AS critical
            FROM observations o JOIN service_requests sr ON sr.id = o.service_request_id
            WHERE o.tenant_id = $1 AND o.patient_id = $2
              AND NOT EXISTS (SELECT 1 FROM observations x WHERE x.amends = o.id)
         )
         SELECT * FROM (
            (SELECT * FROM o WHERE loop_state <> 'closed')
            UNION
            (SELECT DISTINCT ON (code_loinc) * FROM o WHERE loop_state = 'closed'
             ORDER BY code_loinc, effective_at DESC, id DESC)
            UNION
            (SELECT * FROM o WHERE loop_state = 'closed'
             ORDER BY effective_at DESC, id DESC LIMIT $3)
         ) r ORDER BY effective_at DESC, id DESC",
    )
    .bind(tenant_id)
    .bind(patient_id)
    .bind(HISTORY_FACT_LIMIT)
    .fetch_all(&mut *conn)
    .await?
    .iter()
    .map(|r| {
        let value: Decimal = r.get("value_num");
        let range: Option<String> = r.get("reference_range");
        ResultFact {
            observation_id: r.get("id"),
            service_request_id: r.get("service_request_id"),
            code_loinc: r.get("code_loinc"),
            display: r.get("display"),
            value: Some(value.to_string()),
            unit: r.get::<Option<String>, _>("unit"),
            critical: r.get("critical"),
            abnormal: brief::abnormal_flag(value, range.as_deref()).is_some(),
            loop_state: r.get("loop_state"),
            effective_at: r.get("effective_at"),
        }
    })
    .collect();

    let open_requests = sqlx::query(
        "SELECT id, display, loop_state, created_at FROM service_requests
         WHERE tenant_id = $1 AND patient_id = $2 AND loop_state <> 'closed'
         ORDER BY created_at DESC",
    )
    .bind(tenant_id)
    .bind(patient_id)
    .fetch_all(&mut *conn)
    .await?
    .iter()
    .map(|r| ServiceRequestFact {
        id: r.get("id"),
        display: r.get("display"),
        loop_state: r.get("loop_state"),
        created_at: r.get("created_at"),
    })
    .collect();

    // All in-progress encounters plus bounded closed history.
    let encounters = sqlx::query(
        "WITH e AS (
            SELECT id, status, encounter_type, started_at, completed_at FROM encounters
            WHERE tenant_id = $1 AND patient_id = $2
         )
         SELECT * FROM (
            (SELECT * FROM e WHERE status = 'in_progress')
            UNION ALL
            (SELECT * FROM e WHERE status <> 'in_progress'
             ORDER BY started_at DESC, id DESC LIMIT $3)
         ) r ORDER BY started_at DESC, id DESC",
    )
    .bind(tenant_id)
    .bind(patient_id)
    .bind(HISTORY_FACT_LIMIT)
    .fetch_all(&mut *conn)
    .await?
    .iter()
    .map(|r| EncounterFact {
        id: r.get("id"),
        status: r.get("status"),
        encounter_type: r.get("encounter_type"),
        started_at: r.get("started_at"),
        completed_at: r.get("completed_at"),
    })
    .collect();

    // Only actionable tasks feed the rules (same statuses as the Patient Brief).
    let tasks = sqlx::query(&format!(
        "SELECT id, description, status, priority, due_at, created_at FROM follow_up_tasks
         WHERE tenant_id = $1 AND patient_id = $2
           AND status IN {}
         ORDER BY created_at DESC",
        brief::ACTIONABLE_TASK_STATUSES
    ))
    .bind(tenant_id)
    .bind(patient_id)
    .fetch_all(&mut *conn)
    .await?
    .iter()
    .map(|r| TaskFact {
        id: r.get("id"),
        description: r.get("description"),
        status: r.get("status"),
        priority: r.get("priority"),
        due_at: r.get("due_at"),
        created_at: r.get("created_at"),
    })
    .collect();

    let care_team = sqlx::query(
        "SELECT id, function, assignee_user_id IS NOT NULL AS has_professional
         FROM care_team_assignments
         WHERE tenant_id = $1 AND patient_id = $2 AND active
           AND starts_at <= now() AND (ends_at IS NULL OR ends_at > now())",
    )
    .bind(tenant_id)
    .bind(patient_id)
    .fetch_all(&mut *conn)
    .await?
    .iter()
    .map(|r| CareTeamFact {
        id: r.get("id"),
        function: r.get("function"),
        has_professional: r.get("has_professional"),
    })
    .collect();

    let previous = sqlx::query(
        "SELECT calculated_at, domain_levels FROM (
           (SELECT calculated_at, domain_levels, 0 AS pref FROM risk_assessments
             WHERE tenant_id = $1 AND patient_id = $2 AND calculated_at <= $3
             ORDER BY calculated_at DESC LIMIT 1)
           UNION ALL
           (SELECT calculated_at, domain_levels, 1 AS pref FROM risk_assessments
             WHERE tenant_id = $1 AND patient_id = $2
             ORDER BY calculated_at ASC LIMIT 1)
         ) b ORDER BY pref LIMIT 1",
    )
    .bind(tenant_id)
    .bind(patient_id)
    .bind(now - Duration::days(TREND_BASELINE_DAYS))
    .fetch_optional(&mut *conn)
    .await?
    .map(|r| {
        let levels: BTreeMap<String, String> =
            serde_json::from_value(r.get::<Value, _>("domain_levels")).unwrap_or_default();
        PreviousAssessment {
            calculated_at: r.get("calculated_at"),
            levels: levels
                .iter()
                .filter_map(|(d, l)| Some((RiskDomain::parse(d)?, RiskLevel::parse(l)?)))
                .collect(),
        }
    });

    Ok(RiskInput {
        now,
        birth_date,
        alerts,
        current_visit,
        visits,
        latest_vitals,
        conditions,
        medications,
        allergies,
        results,
        open_requests,
        encounters,
        tasks,
        care_team,
        previous,
    })
}

// ---------------------------------------------------------------------------
// Snapshot persistence
// ---------------------------------------------------------------------------

pub(crate) struct StoredAssessment {
    pub id: Uuid,
    pub assessment: RiskAssessment,
    pub trigger: String,
    pub calculated_by: Option<Uuid>,
}

fn domain_levels(a: &RiskAssessment) -> Value {
    let mut m = serde_json::Map::new();
    for d in &a.domains {
        m.insert(d.domain.as_str().to_string(), json!(d.level.as_str()));
    }
    Value::Object(m)
}

/// Compute and store a new current snapshot for the patient inside the
/// caller's transaction. Awaiting dMind summaries of the previous snapshot are
/// superseded: their cited facts no longer describe the current assessment.
pub(crate) async fn recalculate(
    tx: &mut PgConnection,
    ctx: &AuthContext,
    cell: &str,
    tenant_id: Uuid,
    patient_id: Uuid,
    trigger: &str,
) -> Result<StoredAssessment, ApiError> {
    lock_patient_risk(tx, patient_id).await?;
    let p = sqlx::query(
        "SELECT facility_id, birth_date FROM patients WHERE id = $1 AND tenant_id = $2",
    )
    .bind(patient_id)
    .bind(tenant_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(ApiError::not_found)?;
    let facility_id: Uuid = p.get("facility_id");
    let input = collect_input(tx, tenant_id, patient_id, p.get("birth_date"), Utc::now()).await?;
    let calculated_by = if ctx.is_service {
        None
    } else {
        Some(ctx.user_id)
    };
    let stored = store_snapshot(
        tx,
        tenant_id,
        patient_id,
        facility_id,
        &input,
        trigger,
        calculated_by,
    )
    .await?;
    audit::emit(
        &mut *tx,
        ctx,
        "risk.assessment.calculated",
        cell,
        json!({
            "assessment_id": stored.id,
            "patient_id": patient_id,
            "trigger": trigger,
            "rules_version": stored.assessment.rules_version,
            "overall_level": stored.assessment.overall_level.as_str(),
            "trend": stored.assessment.trend.as_str(),
        }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    Ok(stored)
}

/// Run the deterministic rules over `input` and append the result as the
/// patient's current snapshot (the previous one is kept as history).
pub(crate) async fn store_snapshot(
    tx: &mut PgConnection,
    tenant_id: Uuid,
    patient_id: Uuid,
    facility_id: Uuid,
    input: &RiskInput,
    trigger: &str,
    calculated_by: Option<Uuid>,
) -> Result<StoredAssessment, ApiError> {
    let assessment = risk_rules::assess(input);
    let input_hash = dmind_gateway::hash_json(input);
    let id = Uuid::now_v7();
    sqlx::query(
        "UPDATE risk_assessments SET is_current = false
         WHERE tenant_id = $1 AND patient_id = $2 AND is_current",
    )
    .bind(tenant_id)
    .bind(patient_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO risk_assessments
         (id, tenant_id, facility_id, patient_id, rules_version, overall_level, safety_floor,
          overall_trend, domain_levels, assessment, input_hash, trigger, calculated_by,
          calculated_at, is_current)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,true)",
    )
    .bind(id)
    .bind(tenant_id)
    .bind(facility_id)
    .bind(patient_id)
    .bind(&assessment.rules_version)
    .bind(assessment.overall_level.as_str())
    .bind(assessment.safety_floor.as_str())
    .bind(assessment.trend.as_str())
    .bind(domain_levels(&assessment))
    .bind(serde_json::to_value(&assessment).map_err(ApiError::internal)?)
    .bind(&input_hash)
    .bind(trigger)
    .bind(calculated_by)
    .bind(assessment.calculated_at)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE ai_artifacts SET status = $1
         WHERE tenant_id = $2 AND patient_id = $3 AND artifact_type = $4 AND status = $5",
    )
    .bind(ArtifactStatus::Superseded.as_str())
    .bind(tenant_id)
    .bind(patient_id)
    .bind(ARTIFACT_TYPE)
    .bind(ArtifactStatus::AwaitingReview.as_str())
    .execute(&mut *tx)
    .await?;
    Ok(StoredAssessment {
        id,
        assessment,
        trigger: trigger.to_string(),
        calculated_by,
    })
}

/// Hook for clinically relevant confirmed changes (signed notes, reviewed
/// results, received results). Runs inside the caller's transaction so the
/// snapshot reflects exactly the committed change; it never touches the
/// change itself.
pub(crate) async fn recalculate_after_change(
    tx: &mut PgConnection,
    ctx: &AuthContext,
    state: &AppState,
    tenant_id: Uuid,
    patient_id: Uuid,
    trigger: &str,
) -> Result<(), ApiError> {
    recalculate(tx, ctx, &state.cell, tenant_id, patient_id, trigger).await?;
    Ok(())
}

async fn load_current(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    patient_id: Uuid,
) -> Result<Option<StoredAssessment>, ApiError> {
    let row = sqlx::query(
        "SELECT id, assessment, trigger, calculated_by FROM risk_assessments
         WHERE tenant_id = $1 AND patient_id = $2 AND is_current",
    )
    .bind(tenant_id)
    .bind(patient_id)
    .fetch_optional(&mut *conn)
    .await?;
    row.map(|r| {
        Ok(StoredAssessment {
            id: r.get("id"),
            assessment: serde_json::from_value(r.get::<Value, _>("assessment"))
                .map_err(ApiError::internal)?,
            trigger: r.get("trigger"),
            calculated_by: r.get("calculated_by"),
        })
    })
    .transpose()
}

// ---------------------------------------------------------------------------
// Review state
// ---------------------------------------------------------------------------

struct ReviewRow {
    domain: String,
    status: RiskReviewStatus,
    reviewer_id: Uuid,
    reviewer_name: String,
    note: Option<String>,
    level_at_review: RiskLevel,
    reviewed_at: DateTime<Utc>,
    assessment_id: Uuid,
}

async fn load_reviews(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    patient_id: Uuid,
) -> Result<Vec<ReviewRow>, ApiError> {
    let rows = sqlx::query(
        "SELECT r.domain, r.status, r.reviewer_id, u.display_name, r.note, r.level_at_review,
                r.reviewed_at, r.assessment_id
         FROM risk_reviews r JOIN users u ON u.id = r.reviewer_id
         WHERE r.tenant_id = $1 AND r.patient_id = $2",
    )
    .bind(tenant_id)
    .bind(patient_id)
    .fetch_all(&mut *conn)
    .await?;
    rows.iter()
        .map(|r| {
            Ok(ReviewRow {
                domain: r.get("domain"),
                status: RiskReviewStatus::parse(r.get::<String, _>("status").as_str())
                    .ok_or_else(|| ApiError::internal("invalid review status"))?,
                reviewer_id: r.get("reviewer_id"),
                reviewer_name: r.get("display_name"),
                note: r.get("note"),
                level_at_review: RiskLevel::parse(r.get::<String, _>("level_at_review").as_str())
                    .ok_or_else(|| ApiError::internal("invalid review level"))?,
                reviewed_at: r.get("reviewed_at"),
                assessment_id: r.get("assessment_id"),
            })
        })
        .collect()
}

/// A review stands while the level has not risen above the level that was
/// reviewed; a higher level is presented as unreviewed again, keeping the
/// earlier review visible as history.
fn effective_review(reviews: &[ReviewRow], domain: &str, current: RiskLevel) -> Value {
    let Some(r) = reviews.iter().find(|r| r.domain == domain) else {
        return json!({ "status": RiskReviewStatus::Unreviewed.as_str() });
    };
    let stands = r.level_at_review >= current;
    json!({
        "status": if stands { r.status.as_str() } else { RiskReviewStatus::Unreviewed.as_str() },
        "reviewer_id": r.reviewer_id,
        "reviewer": r.reviewer_name,
        "note": r.note,
        "reviewed_at": r.reviewed_at,
        "level_at_review": r.level_at_review.as_str(),
        "assessment_id": r.assessment_id,
        "superseded_by_higher_level": !stands,
    })
}

fn present_assessment(stored: &StoredAssessment, reviews: &[ReviewRow]) -> Result<Value, ApiError> {
    let a = &stored.assessment;
    let mut v = serde_json::to_value(a).map_err(ApiError::internal)?;
    let obj = v
        .as_object_mut()
        .ok_or_else(|| ApiError::internal("assessment is not an object"))?;
    obj.insert("id".into(), json!(stored.id));
    obj.insert("trigger".into(), json!(stored.trigger));
    obj.insert("calculated_by".into(), json!(stored.calculated_by));
    obj.insert(
        "review".into(),
        effective_review(reviews, OVERALL_DOMAIN, a.overall_level),
    );
    if let Some(Value::Array(domains)) = obj.get_mut("domains") {
        for (d, dv) in a.domains.iter().zip(domains.iter_mut()) {
            if let Some(m) = dv.as_object_mut() {
                m.insert(
                    "review".into(),
                    effective_review(reviews, d.domain.as_str(), d.level),
                );
            }
        }
    }
    Ok(v)
}

async fn load_history(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    patient_id: Uuid,
) -> Result<Vec<Value>, ApiError> {
    Ok(sqlx::query(
        "SELECT id, calculated_at, overall_level, overall_trend, domain_levels, trigger, rules_version,
                is_current
         FROM risk_assessments WHERE tenant_id = $1 AND patient_id = $2
         ORDER BY calculated_at DESC, id DESC LIMIT $3",
    )
    .bind(tenant_id)
    .bind(patient_id)
    .bind(HISTORY_LIMIT)
    .fetch_all(&mut *conn)
    .await?
    .iter()
    .map(|r| {
        json!({
            "id": r.get::<Uuid,_>("id"),
            "calculated_at": r.get::<DateTime<Utc>,_>("calculated_at"),
            "overall_level": r.get::<String,_>("overall_level"),
            "trend": r.get::<String,_>("overall_trend"),
            "domain_levels": r.get::<Value,_>("domain_levels"),
            "trigger": r.get::<String,_>("trigger"),
            "rules_version": r.get::<String,_>("rules_version"),
            "is_current": r.get::<bool,_>("is_current"),
        })
    })
    .collect())
}

const SUMMARY_ARTIFACT_COLUMNS: &str = "a.id, a.status, a.output, a.model, a.model_version, a.route, a.template,
            a.prompt_version, a.risk_assessment_id, a.generated_at, a.reviewed_at, a.review_decision, a.review_note,
            a.review_detail, u.display_name AS reviewer,
            (SELECT count(*) FROM follow_up_tasks t WHERE t.ai_artifact_id = a.id) AS confirmed_tasks";

/// The summary bound to `assessment_id`, if any.
async fn load_summary_artifact(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    assessment_id: Uuid,
) -> Result<Option<Value>, ApiError> {
    let row = sqlx::query(&format!(
        "SELECT {SUMMARY_ARTIFACT_COLUMNS}
         FROM ai_artifacts a LEFT JOIN users u ON u.id = a.reviewer_id
         WHERE a.tenant_id = $1 AND a.risk_assessment_id = $2 AND a.artifact_type = $3
         ORDER BY a.generated_at DESC, a.id DESC LIMIT 1"
    ))
    .bind(tenant_id)
    .bind(assessment_id)
    .bind(ARTIFACT_TYPE)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.map(|r| summary_json(&r, assessment_id)))
}

/// The summary to show for a patient: the one bound to the current
/// assessment, otherwise the most recent *approved* summary of an earlier
/// assessment (an approved summary keeps its suggestions actionable, and a
/// task confirmed from it recalculates risk, which must not hide it).
/// Awaiting summaries of older assessments are superseded, never shown.
async fn load_patient_summary(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    patient_id: Uuid,
    current_assessment_id: Uuid,
) -> Result<Option<Value>, ApiError> {
    let row = sqlx::query(&format!(
        "SELECT {SUMMARY_ARTIFACT_COLUMNS}
         FROM ai_artifacts a LEFT JOIN users u ON u.id = a.reviewer_id
         WHERE a.tenant_id = $1 AND a.patient_id = $2 AND a.artifact_type = $3
           AND (a.risk_assessment_id = $4 OR a.status = 'approved')
         ORDER BY (a.risk_assessment_id = $4) DESC, a.generated_at DESC, a.id DESC LIMIT 1"
    ))
    .bind(tenant_id)
    .bind(patient_id)
    .bind(ARTIFACT_TYPE)
    .bind(current_assessment_id)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.map(|r| summary_json(&r, current_assessment_id)))
}

fn summary_json(r: &sqlx::postgres::PgRow, current_assessment_id: Uuid) -> Value {
    let assessment_id: Uuid = r.get("risk_assessment_id");
    json!({
            "id": r.get::<Uuid,_>("id"),
            "status": r.get::<String,_>("status"),
            "assessment_id": assessment_id,
            "for_current_assessment": assessment_id == current_assessment_id,
            "output": r.get::<Option<Value>,_>("output"),
            "model": r.get::<Option<String>,_>("model"),
            "model_version": r.get::<Option<String>,_>("model_version"),
            "route": r.get::<Option<String>,_>("route"),
            "template": r.get::<Option<String>,_>("template"),
            // Artifacts generated before prompt provenance was stored all came
            // from the fixture prompt family.
            "prompt_version": r
                .get::<Option<String>, _>("prompt_version")
                .unwrap_or_else(|| RISK_SUMMARY_PROMPT_VERSION.to_string()),
            "generated_at": r.get::<Option<DateTime<Utc>>,_>("generated_at"),
            "reviewed_at": r.get::<Option<DateTime<Utc>>,_>("reviewed_at"),
            "review_decision": r.get::<Option<String>,_>("review_decision"),
            "review_note": r.get::<Option<String>,_>("review_note"),
            "review_detail": r.get::<Value,_>("review_detail"),
            "reviewer": r.get::<Option<String>,_>("reviewer"),
            "confirmed_tasks": r.get::<i64,_>("confirmed_tasks"),
    })
}

async fn load_care_team(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    patient_id: Uuid,
) -> Result<Vec<Value>, ApiError> {
    Ok(sqlx::query(
        "SELECT c.id, c.function, c.source, c.starts_at, c.visit_id,
                c.assignee_user_id, u.display_name AS assignee, q.id AS queue_id, q.name AS queue
         FROM care_team_assignments c
         LEFT JOIN users u ON u.id = c.assignee_user_id
         LEFT JOIN service_queues q ON q.id = c.queue_id
         WHERE c.tenant_id = $1 AND c.patient_id = $2 AND c.active
           AND c.starts_at <= now() AND (c.ends_at IS NULL OR c.ends_at > now())
         ORDER BY c.starts_at DESC, c.id DESC",
    )
    .bind(tenant_id)
    .bind(patient_id)
    .fetch_all(&mut *conn)
    .await?
    .iter()
    .map(|r| {
        json!({
            "id": r.get::<Uuid,_>("id"),
            "function": r.get::<String,_>("function"),
            "source": r.get::<String,_>("source"),
            "since": r.get::<DateTime<Utc>,_>("starts_at"),
            "visit_id": r.get::<Option<Uuid>,_>("visit_id"),
            "assignee_user_id": r.get::<Option<Uuid>,_>("assignee_user_id"),
            "assignee": r.get::<Option<String>,_>("assignee"),
            "queue_id": r.get::<Option<Uuid>,_>("queue_id"),
            "queue": r.get::<Option<String>,_>("queue"),
        })
    })
    .collect())
}

fn covers(ctx: &AuthContext, action: &str, facility_id: Uuid) -> bool {
    match facility_scope(ctx, action) {
        None => true,
        Some(ids) => ids.contains(&facility_id),
    }
}

/// Risk payload for a patient the caller may read: `None` when the caller is
/// not allowed to read risk (the denial is audited by the guard) so the
/// composite view still renders the chart.
async fn risk_section(
    state: &AppState,
    ctx: &AuthContext,
    p: &PatientRow,
) -> Result<Option<Value>, ApiError> {
    let allowed = match guard(
        state,
        ctx,
        actions::RISK_READ,
        "risk",
        Some(resource_ctx(p)),
    )
    .await
    {
        Ok(a) => a,
        Err(e) if e.status == StatusCode::FORBIDDEN => return Ok(None),
        Err(e) => return Err(e),
    };
    allowed.record_on_pool(state, ctx).await?;
    let mut conn = state.pool.acquire().await?;
    Ok(Some(risk_payload(&mut conn, ctx, p).await?))
}

/// Professionals a risk follow-up can be assigned to (clinical roles only,
/// never service principals).
async fn assignable_professionals(
    conn: &mut PgConnection,
    tenant_id: Uuid,
) -> Result<Vec<Value>, ApiError> {
    Ok(sqlx::query(
        "SELECT DISTINCT u.id, u.display_name FROM role_assignments ra
         JOIN users u ON u.id = ra.user_id
         WHERE ra.tenant_id = $1 AND ra.role IN ('physician', 'nurse') AND NOT u.is_service
         ORDER BY u.display_name",
    )
    .bind(tenant_id)
    .fetch_all(&mut *conn)
    .await?
    .iter()
    .map(|r| json!({ "id": r.get::<Uuid,_>("id"), "display_name": r.get::<String,_>("display_name") }))
    .collect())
}

async fn risk_payload(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    p: &PatientRow,
) -> Result<Value, ApiError> {
    let current = load_current(conn, p.tenant_id, p.id).await?;
    let reviews = load_reviews(conn, p.tenant_id, p.id).await?;
    let history = load_history(conn, p.tenant_id, p.id).await?;
    let (assessment, summary) = match &current {
        Some(c) => (
            Some(present_assessment(c, &reviews)?),
            load_patient_summary(conn, p.tenant_id, p.id, c.id).await?,
        ),
        None => (None, None),
    };
    let follow_up_owner = sqlx::query(
        "SELECT c.assignee_user_id, u.display_name FROM care_team_assignments c
         JOIN users u ON u.id = c.assignee_user_id
         WHERE c.tenant_id = $1 AND c.patient_id = $2 AND c.function = $3 AND c.active
           AND c.starts_at <= now() AND (c.ends_at IS NULL OR c.ends_at > now())
         ORDER BY c.starts_at DESC LIMIT 1",
    )
    .bind(p.tenant_id)
    .bind(p.id)
    .bind(RISK_FOLLOW_UP_FUNCTION)
    .fetch_optional(&mut *conn)
    .await?
    .map(|r| {
        json!({
            "user_id": r.get::<Uuid,_>("assignee_user_id"),
            "display_name": r.get::<String,_>("display_name"),
        })
    });
    let can_assign = covers(ctx, actions::RISK_MANAGE, p.facility_id);
    let professionals = if can_assign {
        assignable_professionals(conn, p.tenant_id).await?
    } else {
        Vec::new()
    };
    Ok(json!({
        "rules_version": RISK_RULES_VERSION,
        "current": assessment,
        "history": history,
        "summary": summary,
        "follow_up_owner": follow_up_owner,
        "professionals": professionals,
        "capabilities": {
            "can_recalculate": covers(ctx, actions::RISK_MANAGE, p.facility_id),
            "can_acknowledge": covers(ctx, actions::RISK_MANAGE, p.facility_id),
            "can_assign": can_assign,
            "can_review": covers(ctx, actions::RISK_REVIEW, p.facility_id),
        },
    }))
}

// ---------------------------------------------------------------------------
// GET /api/v1/patients/:id/360
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct LangQuery {
    pub lang: Option<String>,
}

fn language(q: Option<&str>) -> String {
    if q == Some("es") {
        "es".to_string()
    } else {
        "en".to_string()
    }
}

pub async fn patient_360(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Query(q): Query<LangQuery>,
) -> Result<Json<Value>, ApiError> {
    let lang = language(q.lang.as_deref());
    let p = load_patient(&state, id).await?;
    guard(
        &state,
        &ctx,
        actions::PATIENT_READ,
        "patient",
        Some(resource_ctx(&p)),
    )
    .await?
    .record_on_pool(&state, &ctx)
    .await?;

    let chart = patients::chart_payload(&state, &ctx, &p.row).await?;
    let mut conn = state.pool.acquire().await?;
    // The brief is encounter-neutral here: nothing is excluded as "this
    // encounter".
    let brief = brief::patient_brief(&mut conn, p.tenant_id, id, Uuid::nil()).await?;
    let diagnostics = brief::diagnostic_history(&mut conn, p.tenant_id, id, &lang).await?;
    let care_team = load_care_team(&mut conn, p.tenant_id, id).await?;
    let internal_alerts = sqlx::query(
        "SELECT id, kind, priority, status, visit_id, created_at FROM internal_alerts
         WHERE tenant_id = $1 AND patient_id = $2 AND status <> 'resolved'
         ORDER BY created_at DESC LIMIT 20",
    )
    .bind(p.tenant_id)
    .bind(id)
    .fetch_all(&mut *conn)
    .await?
    .iter()
    .map(|r| {
        json!({
            "id": r.get::<Uuid,_>("id"),
            "kind": r.get::<String,_>("kind"),
            "priority": r.get::<String,_>("priority"),
            "status": r.get::<String,_>("status"),
            "visit_id": r.get::<Uuid,_>("visit_id"),
            "created_at": r.get::<DateTime<Utc>,_>("created_at"),
        })
    })
    .collect::<Vec<_>>();
    let open_consultation: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM encounters
         WHERE tenant_id = $1 AND patient_id = $2 AND practitioner_id = $3
           AND status = 'in_progress' AND encounter_type = 'consultation'
         ORDER BY started_at DESC LIMIT 1",
    )
    .bind(p.tenant_id)
    .bind(id)
    .bind(ctx.user_id)
    .fetch_optional(&mut *conn)
    .await?;
    drop(conn);
    let risk = risk_section(&state, &ctx, &p).await?;
    let responsible = care_team
        .iter()
        .find(|c| c["function"] == "treating_professional" && !c["assignee"].is_null())
        .cloned();

    let mut out = chart;
    let obj = out
        .as_object_mut()
        .ok_or_else(|| ApiError::internal("chart is not an object"))?;
    obj.insert("brief".into(), brief);
    obj.insert("diagnostics".into(), diagnostics);
    obj.insert("care_team".into(), Value::Array(care_team));
    obj.insert("responsible_professional".into(), json!(responsible));
    obj.insert("internal_alerts".into(), Value::Array(internal_alerts));
    obj.insert("risk".into(), json!(risk));
    obj.insert(
        "capabilities".into(),
        json!({
            "can_start_consultation": !ctx.is_service
                && ctx.has_role(roles::PHYSICIAN)
                && covers(&ctx, actions::ENCOUNTER_START, p.facility_id),
            "open_consultation_id": open_consultation,
            "can_view_risk": risk.is_some(),
        }),
    );
    obj.insert("generated_at".into(), json!(Utc::now()));
    Ok(Json(out))
}

// ---------------------------------------------------------------------------
// GET /api/v1/patients/:id/risk · POST /api/v1/patients/:id/risk/recalculate
// ---------------------------------------------------------------------------

pub async fn get_risk(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let p = load_patient(&state, id).await?;
    guard(
        &state,
        &ctx,
        actions::RISK_READ,
        "risk",
        Some(resource_ctx(&p)),
    )
    .await?
    .record_on_pool(&state, &ctx)
    .await?;
    let mut conn = state.pool.acquire().await?;
    Ok(Json(risk_payload(&mut conn, &ctx, &p).await?))
}

pub async fn recalculate_risk(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let p = load_patient(&state, id).await?;
    let allowed = guard(
        &state,
        &ctx,
        actions::RISK_MANAGE,
        "risk",
        Some(resource_ctx(&p)),
    )
    .await?;
    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    recalculate(&mut tx, &ctx, &state.cell, p.tenant_id, id, "manual").await?;
    tx.commit().await?;
    let mut conn = state.pool.acquire().await?;
    Ok(Json(risk_payload(&mut conn, &ctx, &p).await?))
}

// ---------------------------------------------------------------------------
// Acknowledge · mark reviewed · assign
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct ReviewBody {
    pub assessment_id: Uuid,
    /// A risk domain, or `overall`.
    pub domain: String,
    pub note: Option<String>,
}

pub async fn acknowledge(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<ReviewBody>,
) -> Result<Json<Value>, ApiError> {
    set_review(
        &state,
        &ctx,
        id,
        body,
        actions::RISK_MANAGE,
        RiskReviewStatus::Acknowledged,
        "risk.item.acknowledged",
    )
    .await
}

pub async fn mark_reviewed(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<ReviewBody>,
) -> Result<Json<Value>, ApiError> {
    set_review(
        &state,
        &ctx,
        id,
        body,
        actions::RISK_REVIEW,
        RiskReviewStatus::Reviewed,
        "risk.item.reviewed",
    )
    .await
}

async fn set_review(
    state: &AppState,
    ctx: &AuthContext,
    id: Uuid,
    body: ReviewBody,
    action: &str,
    status: RiskReviewStatus,
    event: &str,
) -> Result<Json<Value>, ApiError> {
    let domain = parse_domain_key(&body.domain)?;
    let note = clean_note(body.note)?;
    let p = load_patient(state, id).await?;
    let allowed = guard(state, ctx, action, "risk", Some(resource_ctx(&p))).await?;
    let mut tx = state.pool.begin().await?;
    lock_patient_risk(&mut tx, id).await?;
    let current = load_current(&mut tx, p.tenant_id, id)
        .await?
        .ok_or_else(|| {
            ApiError::conflict("risk_not_calculated", "no risk assessment exists yet")
        })?;
    // Reviews always refer to the snapshot the professional saw.
    if current.id != body.assessment_id {
        return Err(ApiError::conflict(
            "assessment_stale",
            "the risk assessment changed since it was displayed; reload before reviewing",
        ));
    }
    let level = if domain == OVERALL_DOMAIN {
        current.assessment.overall_level
    } else {
        current
            .assessment
            .domains
            .iter()
            .find(|d| d.domain.as_str() == domain)
            .map(|d| d.level)
            .ok_or_else(|| ApiError::internal("domain missing from assessment"))?
    };
    allowed.record(&mut tx, ctx, &state.cell).await?;
    sqlx::query(
        "INSERT INTO risk_reviews
         (id, tenant_id, patient_id, domain, status, reviewer_id, note, assessment_id,
          level_at_review, reviewed_at)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9, now())
         ON CONFLICT (tenant_id, patient_id, domain) DO UPDATE SET
           status = EXCLUDED.status, reviewer_id = EXCLUDED.reviewer_id, note = EXCLUDED.note,
           assessment_id = EXCLUDED.assessment_id, level_at_review = EXCLUDED.level_at_review,
           reviewed_at = now()",
    )
    .bind(Uuid::now_v7())
    .bind(p.tenant_id)
    .bind(id)
    .bind(&domain)
    .bind(status.as_str())
    .bind(ctx.user_id)
    .bind(&note)
    .bind(current.id)
    .bind(level.as_str())
    .execute(&mut *tx)
    .await?;
    audit::emit(
        &mut *tx,
        ctx,
        event,
        &state.cell,
        json!({
            "patient_id": id,
            "assessment_id": current.id,
            "domain": domain,
            "level": level.as_str(),
            "note_present": note.is_some(),
        }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    tx.commit().await?;
    let mut conn = state.pool.acquire().await?;
    Ok(Json(risk_payload(&mut conn, ctx, &p).await?))
}

#[derive(Deserialize)]
pub struct AssignBody {
    pub assignee_user_id: Uuid,
}

/// Assign the professional who owns risk follow-up. Recorded as a care-team
/// assignment (`risk_follow_up`), so it also establishes the explicit care
/// relationship that review requires.
pub async fn assign(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<AssignBody>,
) -> Result<Json<Value>, ApiError> {
    let p = load_patient(&state, id).await?;
    let allowed = guard(
        &state,
        &ctx,
        actions::RISK_MANAGE,
        "risk",
        Some(resource_ctx(&p)),
    )
    .await?;
    let mut tx = state.pool.begin().await?;
    lock_patient_risk(&mut tx, id).await?;
    let eligible: Option<(Uuid,)> = sqlx::query_as(
        "SELECT DISTINCT u.id FROM role_assignments ra JOIN users u ON u.id = ra.user_id
         WHERE ra.tenant_id = $1 AND ra.user_id = $2 AND NOT u.is_service
           AND ra.role IN ('physician', 'nurse')
           AND (ra.facility_id = $3 OR ra.facility_id IS NULL)",
    )
    .bind(p.tenant_id)
    .bind(body.assignee_user_id)
    .bind(p.facility_id)
    .fetch_optional(&mut *tx)
    .await?;
    if eligible.is_none() {
        return Err(ApiError::bad_request(
            "validation_failed",
            "the assignee must be a clinical professional at the patient's facility",
        ));
    }
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    sqlx::query(
        "UPDATE care_team_assignments SET active = false, ends_at = now(), updated_at = now()
         WHERE tenant_id = $1 AND patient_id = $2 AND function = $3 AND active",
    )
    .bind(p.tenant_id)
    .bind(id)
    .bind(RISK_FOLLOW_UP_FUNCTION)
    .execute(&mut *tx)
    .await?;
    let assignment_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO care_team_assignments
         (id, tenant_id, facility_id, patient_id, assignee_user_id, function, source, assigned_by)
         VALUES ($1,$2,$3,$4,$5,$6,'risk_worklist',$7)",
    )
    .bind(assignment_id)
    .bind(p.tenant_id)
    .bind(p.facility_id)
    .bind(id)
    .bind(body.assignee_user_id)
    .bind(RISK_FOLLOW_UP_FUNCTION)
    .bind(ctx.user_id)
    .execute(&mut *tx)
    .await?;
    audit::emit(
        &mut *tx,
        &ctx,
        "risk.item.assigned",
        &state.cell,
        json!({
            "patient_id": id,
            "assignment_id": assignment_id,
            "assignee_user_id": body.assignee_user_id,
        }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    tx.commit().await?;
    let mut conn = state.pool.acquire().await?;
    Ok(Json(risk_payload(&mut conn, &ctx, &p).await?))
}

// ---------------------------------------------------------------------------
// GET /api/v1/risk/worklist
// ---------------------------------------------------------------------------

#[derive(Deserialize, Default)]
pub struct WorklistQuery {
    pub domain: Option<String>,
    pub service: Option<String>,
    /// A user id, or `unassigned`.
    pub assignee: Option<String>,
    /// unreviewed | acknowledged | reviewed
    pub review: Option<String>,
    pub trend: Option<String>,
    /// Include low-risk patients (default: false).
    pub include_low: Option<bool>,
}

const LEVEL_RANK_SQL: &str = "CASE {col} WHEN 'critical' THEN 0 WHEN 'high' THEN 1 WHEN 'moderate' THEN 2 WHEN 'low' THEN 3 ELSE 4 END";

fn level_rank(col: &str) -> String {
    LEVEL_RANK_SQL.replace("{col}", col)
}

pub async fn worklist(
    State(state): State<AppState>,
    ctx: AuthContext,
    Query(q): Query<WorklistQuery>,
) -> Result<Json<Value>, ApiError> {
    guard(
        &state,
        &ctx,
        actions::RISK_READ,
        "risk_worklist",
        Some(ResourceCtx {
            tenant_id: ctx.tenant_id,
            patient_id: None,
            facility_id: None,
        }),
    )
    .await?
    .record_on_pool(&state, &ctx)
    .await?;
    let domain = q.domain.as_deref().map(parse_domain_key).transpose()?;
    let domain = domain.filter(|d| d != OVERALL_DOMAIN);
    let trend = match q.trend.as_deref() {
        None | Some("") => None,
        Some(t) => Some(
            RiskTrend::parse(t)
                .ok_or_else(|| ApiError::bad_request("validation_failed", "unknown trend"))?,
        ),
    };
    let review =
        match q.review.as_deref() {
            None | Some("") => None,
            Some(r) => Some(RiskReviewStatus::parse(r).ok_or_else(|| {
                ApiError::bad_request("validation_failed", "unknown review status")
            })?),
        };
    let (assignee_id, unassigned) = match q.assignee.as_deref() {
        None | Some("") => (None, false),
        Some("unassigned") => (None, true),
        Some(u) => (
            Some(Uuid::parse_str(u).map_err(|_| {
                ApiError::bad_request("validation_failed", "assignee must be a user id")
            })?),
            false,
        ),
    };
    let service = q
        .service
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let scope = facility_scope(&ctx, actions::RISK_READ);
    if matches!(&scope, Some(ids) if ids.is_empty()) {
        return Ok(Json(json!({
            "items": [],
            "services": [],
            "professionals": [],
            "domains": RiskDomain::ALL.iter().map(|d| d.as_str()).collect::<Vec<_>>(),
            "rules_version": RISK_RULES_VERSION,
            "limit": WORKLIST_LIMIT,
        })));
    }
    let include_low = q.include_low.unwrap_or(false);

    // `focus` is the level the list is ordered by: the selected domain's
    // level, otherwise the overall level. Review state is resolved in SQL so
    // the filter applies before the row limit.
    let focus_expr = match &domain {
        Some(_) => "ra.domain_levels->>$6".to_string(),
        None => "ra.overall_level".to_string(),
    };
    let sql = format!(
        "WITH owner AS (
            SELECT c.tenant_id, c.patient_id, c.assignee_user_id, u.display_name
            FROM care_team_assignments c JOIN users u ON u.id = c.assignee_user_id
            WHERE c.tenant_id = $1 AND c.function = '{owner_fn}' AND c.active
              AND c.starts_at <= now() AND (c.ends_at IS NULL OR c.ends_at > now())
         ), treating AS (
            SELECT DISTINCT ON (c.patient_id) c.tenant_id, c.patient_id, u.display_name
            FROM care_team_assignments c JOIN users u ON u.id = c.assignee_user_id
            WHERE c.tenant_id = $1 AND c.function = 'treating_professional' AND c.active
              AND c.starts_at <= now() AND (c.ends_at IS NULL OR c.ends_at > now())
            ORDER BY c.patient_id, c.starts_at DESC
         ), latest_visit AS (
            SELECT DISTINCT ON (v.patient_id) v.patient_id, v.service
            FROM visits v WHERE v.tenant_id = $1
            ORDER BY v.patient_id, COALESCE(v.arrived_at, v.scheduled_at, v.created_at) DESC
         )
         SELECT ra.id, ra.patient_id, ra.facility_id, ra.overall_level, ra.overall_trend,
                ra.domain_levels, ra.assessment, ra.calculated_at, ra.rules_version,
                {focus} AS focus_level,
                p.family_name, p.given_name, p.identifier, p.birth_date,
                f.name AS facility,
                o.assignee_user_id AS owner_id, o.display_name AS owner,
                t.display_name AS treating,
                lv.service,
                rr.status AS review_status, rr.level_at_review, rr.reviewed_at,
                ru.display_name AS reviewer,
                CASE WHEN rr.id IS NULL THEN 'unreviewed'
                     WHEN {rank_review} <= {rank_focus} THEN rr.status
                     ELSE 'unreviewed' END AS effective_review,
                (EXISTS (SELECT 1 FROM encounters e
                         WHERE e.tenant_id = $1 AND e.patient_id = ra.patient_id
                           AND e.practitioner_id = $11)
                 OR EXISTS (SELECT 1 FROM care_team_assignments c
                            WHERE c.tenant_id = $1 AND c.patient_id = ra.patient_id
                              AND c.assignee_user_id = $11 AND c.active
                              AND c.starts_at <= now()
                              AND (c.ends_at IS NULL OR c.ends_at > now()))) AS related
         FROM risk_assessments ra
         JOIN patients p ON p.id = ra.patient_id
         JOIN facilities f ON f.id = ra.facility_id
         LEFT JOIN owner o ON o.tenant_id = ra.tenant_id AND o.patient_id = ra.patient_id
         LEFT JOIN treating t ON t.tenant_id = ra.tenant_id AND t.patient_id = ra.patient_id
         LEFT JOIN latest_visit lv ON lv.patient_id = ra.patient_id
         LEFT JOIN risk_reviews rr ON rr.tenant_id = ra.tenant_id AND rr.patient_id = ra.patient_id
              AND rr.domain = {review_domain}
         LEFT JOIN users ru ON ru.id = rr.reviewer_id
         WHERE ra.tenant_id = $1 AND ra.is_current
           AND ($2::uuid[] IS NULL OR ra.facility_id = ANY($2))
           AND ($3::text IS NULL OR ra.overall_trend = $3)
           AND ($4::text IS NULL OR lv.service = $4)
           AND ($5::uuid IS NULL OR o.assignee_user_id = $5)
           AND (NOT $7::boolean OR o.assignee_user_id IS NULL)
           AND ($8::boolean OR {focus} <> 'low')
           AND ($9::text IS NULL OR
                (CASE WHEN rr.id IS NULL THEN 'unreviewed'
                      WHEN {rank_review} <= {rank_focus} THEN rr.status
                      ELSE 'unreviewed' END) = $9)
         ORDER BY {rank_focus}, ra.calculated_at DESC, ra.id
         LIMIT $10",
        owner_fn = RISK_FOLLOW_UP_FUNCTION,
        focus = focus_expr,
        rank_focus = level_rank(&focus_expr),
        rank_review = level_rank("rr.level_at_review"),
        review_domain = if domain.is_some() { "$6" } else { "'overall'" },
    );
    let rows = sqlx::query(&sql)
        .bind(ctx.tenant_id)
        .bind(scope)
        .bind(trend.map(|t| t.as_str()))
        .bind(service)
        .bind(assignee_id)
        .bind(domain.clone().unwrap_or_else(|| OVERALL_DOMAIN.to_string()))
        .bind(unassigned)
        .bind(include_low)
        .bind(review.map(|r| r.as_str()))
        .bind(WORKLIST_LIMIT)
        .bind(ctx.user_id)
        .fetch_all(&state.pool)
        .await?;

    let can_open_needs_relationship =
        ctx.has_role(roles::PHYSICIAN) && !ctx.has_role(roles::CLINICAL_ADMIN);
    let manage_scope = facility_scope(&ctx, actions::RISK_MANAGE);
    let review_scope = facility_scope(&ctx, actions::RISK_REVIEW);
    let in_scope = |scope: &Option<Vec<Uuid>>, f: Uuid| match scope {
        None => true,
        Some(ids) => ids.contains(&f),
    };
    let mut items = Vec::with_capacity(rows.len());
    for r in &rows {
        let assessment: RiskAssessment =
            serde_json::from_value(r.get::<Value, _>("assessment")).map_err(ApiError::internal)?;
        let facility_id: Uuid = r.get("facility_id");
        let focus_domain = domain.as_deref().and_then(RiskDomain::parse);
        // Plain-language explanation material: the contributing factors of
        // the elevated domains (or of the focused domain), with evidence.
        let explained: Vec<Value> = assessment
            .domains
            .iter()
            .filter(|d| match focus_domain {
                Some(fd) => d.domain == fd,
                None => d.level >= RiskLevel::Moderate || d.level == RiskLevel::InsufficientData,
            })
            .map(|d| {
                json!({
                    "domain": d.domain.as_str(),
                    "level": d.level.as_str(),
                    "trend": d.trend.as_str(),
                    "detected_at": d.detected_at,
                    "factors": d.factors,
                    "missing_data": d.missing_data,
                    "stale_data": d.stale_data,
                })
            })
            .collect();
        items.push(json!({
            "assessment_id": r.get::<Uuid,_>("id"),
            "patient": {
                "id": r.get::<Uuid,_>("patient_id"),
                "family_name": r.get::<String,_>("family_name"),
                "given_name": r.get::<String,_>("given_name"),
                "identifier": r.get::<String,_>("identifier"),
                "birth_date": r.get::<NaiveDate,_>("birth_date"),
                "facility_id": facility_id,
                "facility": r.get::<String,_>("facility"),
            },
            "overall_level": r.get::<String,_>("overall_level"),
            "focus_level": r.get::<String,_>("focus_level"),
            "focus_domain": domain,
            "trend": r.get::<String,_>("overall_trend"),
            "domain_levels": r.get::<Value,_>("domain_levels"),
            "explained": explained,
            "calculated_at": r.get::<DateTime<Utc>,_>("calculated_at"),
            "rules_version": r.get::<String,_>("rules_version"),
            "service": r.get::<Option<String>,_>("service"),
            "owner": r.get::<Option<Uuid>,_>("owner_id").map(|id| json!({
                "user_id": id,
                "display_name": r.get::<Option<String>,_>("owner"),
            })),
            "treating_professional": r.get::<Option<String>,_>("treating"),
            "review": {
                "status": r.get::<String,_>("effective_review"),
                "recorded_status": r.get::<Option<String>,_>("review_status"),
                "level_at_review": r.get::<Option<String>,_>("level_at_review"),
                "reviewed_at": r.get::<Option<DateTime<Utc>>,_>("reviewed_at"),
                "reviewer": r.get::<Option<String>,_>("reviewer"),
            },
            "capabilities": {
                "can_acknowledge": in_scope(&manage_scope, facility_id),
                "can_assign": in_scope(&manage_scope, facility_id),
                "can_review": in_scope(&review_scope, facility_id),
                // Display hint only; the Patient 360 guard is authoritative.
                "can_open_360": !can_open_needs_relationship || r.get::<bool,_>("related"),
            },
        }));
    }

    // Filter vocabularies for the interface.
    let services: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT service FROM visits WHERE tenant_id = $1 ORDER BY service",
    )
    .bind(ctx.tenant_id)
    .fetch_all(&state.pool)
    .await?;
    let mut conn = state.pool.acquire().await?;
    let professionals = assignable_professionals(&mut conn, ctx.tenant_id).await?;
    drop(conn);
    Ok(Json(json!({
        "items": items,
        "services": services,
        "professionals": professionals,
        "domains": RiskDomain::ALL.iter().map(|d| d.as_str()).collect::<Vec<_>>(),
        "rules_version": RISK_RULES_VERSION,
        "limit": WORKLIST_LIMIT,
    })))
}

// ---------------------------------------------------------------------------
// dMind Risk Agent: propose · review · confirm suggestion
// ---------------------------------------------------------------------------

#[derive(Deserialize, Default)]
pub struct SummaryRequest {
    pub language: Option<String>,
    pub assessment_id: Option<Uuid>,
}

fn summary_facts(a: &RiskAssessment) -> Vec<(String, String)> {
    let mut facts = Vec::new();
    for d in &a.domains {
        for f in &d.factors {
            for e in &f.evidence {
                let reference = format!("{}:{}", e.record_type, e.record_id);
                if facts.iter().any(|(r, _)| r == &reference) {
                    continue;
                }
                facts.push((reference, e.label.clone()));
            }
        }
    }
    facts
}

pub async fn propose_summary(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    body: Option<Json<SummaryRequest>>,
) -> Result<Json<Value>, ApiError> {
    let body = body.map(|Json(b)| b).unwrap_or_default();
    let lang = language(body.language.as_deref());
    let p = load_patient(&state, id).await?;
    let allowed = guard(
        &state,
        &ctx,
        actions::RISK_REVIEW,
        "risk_summary",
        Some(resource_ctx(&p)),
    )
    .await?;
    let mut tx = state.pool.begin().await?;
    // The patient lock freezes the snapshot the summary is bound to for the
    // whole generation.
    lock_patient_risk(&mut tx, id).await?;
    let current = load_current(&mut tx, p.tenant_id, id)
        .await?
        .ok_or_else(|| {
            ApiError::conflict("risk_not_calculated", "no risk assessment exists yet")
        })?;
    if let Some(expected) = body.assessment_id {
        if expected != current.id {
            return Err(ApiError::conflict(
                "assessment_stale",
                "the risk assessment changed since it was displayed; reload before requesting a summary",
            ));
        }
    }
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let req = RiskSummaryRequest {
        template: RISK_TEMPLATE.to_string(),
        language: lang,
        assessment: current.assessment.clone(),
        facts: summary_facts(&current.assessment),
    };
    audit::emit(
        &mut *tx,
        &ctx,
        "ai.artifact.requested",
        &state.cell,
        json!({ "patient_id": id, "assessment_id": current.id, "template": RISK_TEMPLATE }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    // The summary is bound to `current.id`; releasing the lock here means no
    // patient row stays locked while a (possibly external) provider runs.
    tx.commit().await?;

    let input_refs: Vec<String> = req.facts.iter().map(|(r, _)| r.clone()).collect();
    let hash = dmind_gateway::risk::risk_input_hash(&req);
    let plan = aigov::plan(
        &state,
        p.tenant_id,
        id,
        ARTIFACT_TYPE,
        Operation::RiskSummary,
        &hash,
        RISK_SUMMARY_SCHEMA,
    )
    .await?;
    let (resp, reused_from, execution_id) = match plan {
        aigov::ExecutionPlan::Reuse(prior) => {
            let output: wellos_domain::risk::RiskSummaryV1 = prior.output_as()?;
            (
                dmind_gateway::risk::RiskSummaryResponse {
                    output,
                    model: prior.model.clone(),
                    model_version: prior.model_version.clone(),
                    route: prior.route.clone(),
                    prompt_version: prior.prompt_version.clone(),
                    input_hash: hash.clone(),
                    usage: prior.usage_as(),
                },
                Some(prior.id),
                None,
            )
        }
        aigov::ExecutionPlan::Execute { execution_id } => {
            match state.gateway.summarize_risk(&req).await {
                Ok(r) => (r, None, Some(execution_id)),
                Err(err) => {
                    audit::record(
                        &state.pool,
                        &ctx,
                        "ai.generation.failed",
                        Some("patient"),
                        Some(id.to_string()),
                        "deny",
                        Some(match err {
                            GatewayError::Unavailable(_) => "provider_unavailable",
                            GatewayError::Disabled(_) => "provider_disabled",
                            GatewayError::InvalidOutput(_) => "invalid_output",
                            GatewayError::PolicyDenied(_) => "policy_denied",
                        }),
                    )
                    .await
                    .map_err(ApiError::internal)?;
                    return Err(aigov::gateway_error(err));
                }
            }
        }
    };

    let mut tx = state.pool.begin().await?;
    lock_patient_risk(&mut tx, id).await?;
    let still_current = load_current(&mut tx, p.tenant_id, id)
        .await?
        .is_some_and(|c| c.id == current.id);
    if !still_current {
        return Err(ApiError::conflict(
            "assessment_stale",
            "the risk assessment changed while the summary was generated; request it again",
        ));
    }
    // Defense in depth: the gateway already aligned and validated; the server
    // re-runs the same rules against the exact snapshot it stores.
    let output = resp
        .output
        .clone()
        .align_to_deterministic(&current.assessment);
    output
        .validate(&current.assessment)
        .map_err(ApiError::internal)?;

    let artifact_id = Uuid::now_v7();
    sqlx::query(
        "UPDATE ai_artifacts SET status = $1
         WHERE tenant_id = $2 AND patient_id = $3 AND artifact_type = $4 AND status = $5",
    )
    .bind(ArtifactStatus::Superseded.as_str())
    .bind(p.tenant_id)
    .bind(id)
    .bind(ARTIFACT_TYPE)
    .bind(ArtifactStatus::AwaitingReview.as_str())
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO ai_artifacts
         (id, tenant_id, patient_id, risk_assessment_id, artifact_type, autonomy_level, status,
          model, model_version, route, template, input_hash, output, output_schema,
          citations, limitations, generated_at)
         VALUES ($1,$2,$3,$4,$5,'A2',$6,$7,$8,$9,$10,$11,$12,$13,$14,$15, now())",
    )
    .bind(artifact_id)
    .bind(p.tenant_id)
    .bind(id)
    .bind(current.id)
    .bind(ARTIFACT_TYPE)
    .bind(ArtifactStatus::AwaitingReview.as_str())
    .bind(&resp.model)
    .bind(&resp.model_version)
    .bind(&resp.route)
    .bind(RISK_TEMPLATE)
    .bind(&resp.input_hash)
    .bind(serde_json::to_value(&output).map_err(ApiError::internal)?)
    .bind(RISK_SUMMARY_SCHEMA)
    .bind(serde_json::to_value(&output.cited_sources).map_err(ApiError::internal)?)
    .bind(serde_json::to_value(&output.limitations).map_err(ApiError::internal)?)
    .execute(&mut *tx)
    .await?;
    let provider = ProviderInfo {
        provider: resp.route.clone(),
        model: resp.model.clone(),
        model_version: resp.model_version.clone(),
    };
    aigov::annotate(
        &mut tx,
        artifact_id,
        &aigov::Provenance {
            provider: &provider,
            prompt_version: &resp.prompt_version,
            input_refs: &input_refs,
            usage: resp.usage.as_ref(),
            synthetic: state.runtime.synthetic_output(),
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
            "artifact_id": artifact_id,
            "patient_id": id,
            "assessment_id": current.id,
            "template": RISK_TEMPLATE,
            "prompt_version": resp.prompt_version,
            "model": resp.model,
            "input_hash": resp.input_hash,
            "reused_from": reused_from,
            "raised_to_floor": output.raised_to_floor,
        }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    tx.commit().await?;
    let mut conn = state.pool.acquire().await?;
    let summary = load_summary_artifact(&mut conn, p.tenant_id, current.id).await?;
    Ok(Json(json!({
        "id": artifact_id,
        "assessment_id": current.id,
        "summary": summary,
    })))
}

#[derive(Deserialize)]
pub struct SummaryReview {
    /// approve | reject
    pub decision: String,
    pub note: Option<String>,
}

pub async fn review_summary(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path((id, artifact_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<SummaryReview>,
) -> Result<Json<Value>, ApiError> {
    let decision = match body.decision.as_str() {
        "approve" => ReviewDecision::Approved,
        "reject" => ReviewDecision::Rejected,
        _ => {
            return Err(ApiError::bad_request(
                "validation_failed",
                "decision must be approve or reject",
            ))
        }
    };
    let note = clean_note(body.note)?;
    let p = load_patient(&state, id).await?;
    let allowed = guard(
        &state,
        &ctx,
        actions::RISK_REVIEW,
        "risk_summary",
        Some(resource_ctx(&p)),
    )
    .await?;
    let mut tx = state.pool.begin().await?;
    lock_patient_risk(&mut tx, id).await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let art = sqlx::query(
        "SELECT a.status, a.risk_assessment_id, ra.is_current
         FROM ai_artifacts a JOIN risk_assessments ra ON ra.id = a.risk_assessment_id
         WHERE a.id = $1 AND a.tenant_id = $2 AND a.patient_id = $3 AND a.artifact_type = $4
         FOR UPDATE OF a",
    )
    .bind(artifact_id)
    .bind(p.tenant_id)
    .bind(id)
    .bind(ARTIFACT_TYPE)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(ApiError::not_found)?;
    let status = ArtifactStatus::parse(art.get::<String, _>("status").as_str())
        .ok_or_else(|| ApiError::internal("invalid artifact status"))?;
    if status != ArtifactStatus::AwaitingReview {
        return Err(ApiError::conflict(
            "invalid_artifact_state",
            "this summary has already been reviewed or superseded",
        ));
    }
    if !art.get::<bool, _>("is_current") {
        return Err(ApiError::conflict(
            "artifact_stale",
            "the summary describes an older risk assessment; request a new one",
        ));
    }
    let next = status
        .review(decision)
        .map_err(|e| ApiError::conflict("invalid_artifact_state", e.to_string()))?;
    let decision_str = match decision {
        ReviewDecision::Approved => "approved",
        ReviewDecision::Rejected => "rejected",
    };
    sqlx::query(
        "UPDATE ai_artifacts SET status = $1, reviewer_id = $2, review_decision = $3,
                review_note = $4, reviewed_at = now()
         WHERE id = $5",
    )
    .bind(next.as_str())
    .bind(ctx.user_id)
    .bind(decision_str)
    .bind(&note)
    .bind(artifact_id)
    .execute(&mut *tx)
    .await?;
    audit::emit(
        &mut *tx,
        &ctx,
        "ai.artifact.reviewed",
        &state.cell,
        json!({ "artifact_id": artifact_id, "patient_id": id, "decision": decision_str }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    tx.commit().await?;
    let assessment_id: Uuid = art.get("risk_assessment_id");
    let mut conn = state.pool.acquire().await?;
    let summary = load_summary_artifact(&mut conn, p.tenant_id, assessment_id).await?;
    Ok(Json(json!({
        "id": artifact_id,
        "status": next.as_str(),
        "decision": decision_str,
        "summary": summary,
    })))
}

#[derive(Deserialize)]
pub struct ConfirmSuggestion {
    /// Index into the approved summary's `follow_up_suggestions`.
    pub suggestion_index: usize,
    /// routine | urgent
    pub priority: Option<String>,
    pub due_in_days: Option<i64>,
    pub note: Option<String>,
}

/// Turn one approved dMind suggestion into a follow-up task. This is the only
/// path from a suggestion to clinical work, and it requires an explicit
/// confirmation by a professional with a care relationship.
pub async fn confirm_suggestion(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path((id, artifact_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<ConfirmSuggestion>,
) -> Result<Json<Value>, ApiError> {
    let priority = match body.priority.as_deref() {
        None | Some("routine") => "routine",
        Some("urgent") => "urgent",
        _ => {
            return Err(ApiError::bad_request(
                "validation_failed",
                "priority must be routine or urgent",
            ))
        }
    };
    let due_in_days = body.due_in_days.unwrap_or(14);
    if !(1..=365).contains(&due_in_days) {
        return Err(ApiError::bad_request(
            "validation_failed",
            "due_in_days must be between 1 and 365",
        ));
    }
    let note = clean_note(body.note)?;
    let p = load_patient(&state, id).await?;
    let allowed = guard(
        &state,
        &ctx,
        actions::RISK_REVIEW,
        "risk_summary",
        Some(resource_ctx(&p)),
    )
    .await?;
    let mut tx = state.pool.begin().await?;
    lock_patient_risk(&mut tx, id).await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let art = sqlx::query(
        "SELECT status, output, risk_assessment_id FROM ai_artifacts
         WHERE id = $1 AND tenant_id = $2 AND patient_id = $3 AND artifact_type = $4 FOR UPDATE",
    )
    .bind(artifact_id)
    .bind(p.tenant_id)
    .bind(id)
    .bind(ARTIFACT_TYPE)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(ApiError::not_found)?;
    let status = ArtifactStatus::parse(art.get::<String, _>("status").as_str())
        .ok_or_else(|| ApiError::internal("invalid artifact status"))?;
    if status != ArtifactStatus::Approved {
        return Err(ApiError::conflict(
            "invalid_artifact_state",
            "only suggestions of an approved summary can be confirmed",
        ));
    }
    let output: RiskSummaryV1 =
        serde_json::from_value(art.get::<Value, _>("output")).map_err(ApiError::internal)?;
    let suggestion = output
        .follow_up_suggestions
        .get(body.suggestion_index)
        .ok_or_else(|| ApiError::bad_request("validation_failed", "unknown suggestion"))?;
    if !suggestion.requires_confirmation {
        return Err(ApiError::internal(
            "suggestion without confirmation requirement",
        ));
    }
    let existing: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM follow_up_tasks
         WHERE tenant_id = $1 AND ai_artifact_id = $2 AND description = $3
           AND status NOT IN ('completed', 'superseded')",
    )
    .bind(p.tenant_id)
    .bind(artifact_id)
    .bind(&suggestion.text)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some((task_id,)) = existing {
        return Err(ApiError::conflict(
            "already_confirmed",
            format!("this suggestion was already confirmed as task {task_id}"),
        ));
    }
    let task_id = Uuid::now_v7();
    let due_at = Utc::now() + chrono::Duration::days(due_in_days);
    sqlx::query(
        "INSERT INTO follow_up_tasks
         (id, tenant_id, patient_id, service_request_id, description, priority, status, due_at,
          source, created_by, ai_artifact_id)
         VALUES ($1,$2,$3,NULL,$4,$5,'open',$6,'risk_suggestion',$7,$8)",
    )
    .bind(task_id)
    .bind(p.tenant_id)
    .bind(id)
    .bind(&suggestion.text)
    .bind(priority)
    .bind(due_at)
    .bind(ctx.user_id)
    .bind(artifact_id)
    .execute(&mut *tx)
    .await?;
    let detail = json!({
        "task_id": task_id,
        "suggestion_index": body.suggestion_index,
        "category": suggestion.category.as_str(),
        "domain": suggestion.domain.as_str(),
        "priority": priority,
        "due_at": due_at,
        "note_present": note.is_some(),
    });
    sqlx::query(
        "UPDATE ai_artifacts
         SET review_detail = jsonb_set(review_detail, '{confirmed}',
                COALESCE(review_detail->'confirmed', '[]'::jsonb) || $2::jsonb)
         WHERE id = $1",
    )
    .bind(artifact_id)
    .bind(&detail)
    .execute(&mut *tx)
    .await?;
    audit::emit(
        &mut *tx,
        &ctx,
        "risk.suggestion.confirmed",
        &state.cell,
        json!({
            "artifact_id": artifact_id,
            "patient_id": id,
            "task_id": task_id,
            "category": suggestion.category.as_str(),
            "domain": suggestion.domain.as_str(),
        }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    audit::emit(
        &mut *tx,
        &ctx,
        "follow_up.created",
        &state.cell,
        json!({ "task_id": task_id, "patient_id": id, "source": "risk_suggestion" }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    // A confirmed task is a care-coordination change the rules observe.
    recalculate(
        &mut tx,
        &ctx,
        &state.cell,
        p.tenant_id,
        id,
        "risk.suggestion.confirmed",
    )
    .await?;
    tx.commit().await?;
    Ok(Json(json!({
        "task_id": task_id,
        "description": suggestion.text,
        "priority": priority,
        "due_at": due_at,
        "status": "open",
    })))
}

// ---------------------------------------------------------------------------
// GET /api/v1/patients/:id/risk/projection — minimal governed insurer view
// ---------------------------------------------------------------------------

/// Levels, versions, provenance and review state only. No notes, no free
/// text, no factor details beyond the rule codes and record references. The
/// patient's `insurer_risk_sharing` consent gates the levels; the access
/// itself is always recorded as an event, shared or not.
pub async fn projection(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let p = load_patient(&state, id).await?;
    let allowed = guard(
        &state,
        &ctx,
        actions::RISK_PROJECTION_READ,
        "risk_projection",
        Some(resource_ctx(&p)),
    )
    .await?;
    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let consent: Option<String> = sqlx::query_scalar(
        "SELECT status FROM consents WHERE tenant_id = $1 AND patient_id = $2 AND purpose = $3
         ORDER BY version DESC, recorded_at DESC LIMIT 1",
    )
    .bind(p.tenant_id)
    .bind(id)
    .bind(PROJECTION_CONSENT_PURPOSE)
    .fetch_optional(&mut *tx)
    .await?;
    let consent_active = consent.as_deref() == Some("active");
    let current = load_current(&mut tx, p.tenant_id, id).await?;
    let reviews = load_reviews(&mut tx, p.tenant_id, id).await?;
    let projected = match (&current, consent_active) {
        (Some(c), true) => {
            let a = &c.assessment;
            Some(json!({
                "assessment_id": c.id,
                "rules_version": a.rules_version,
                "calculated_at": a.calculated_at,
                "overall": {
                    "level": a.overall_level.as_str(),
                    "trend": a.trend.as_str(),
                    "review": review_projection(&reviews, OVERALL_DOMAIN, a.overall_level),
                },
                "domains": a.domains.iter().map(|d| json!({
                    "domain": d.domain.as_str(),
                    "level": d.level.as_str(),
                    "trend": d.trend.as_str(),
                    "detected_at": d.detected_at,
                    "rules_version": d.rules_version,
                    "missing_data": d.missing_data.len(),
                    "stale_data": d.stale_data.len(),
                    "review": review_projection(&reviews, d.domain.as_str(), d.level),
                    "evidence": d.factors.iter().map(|f| json!({
                        "rule": f.code,
                        "detected_at": f.detected_at,
                        "records": f.evidence.iter().map(|e| json!({
                            "record_type": e.record_type,
                            "record_id": e.record_id,
                            "observed_at": e.observed_at,
                        })).collect::<Vec<_>>(),
                    })).collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
            }))
        }
        _ => None,
    };
    audit::emit(
        &mut *tx,
        &ctx,
        "risk.projection.accessed",
        &state.cell,
        json!({
            "patient_id": id,
            "assessment_id": current.as_ref().map(|c| c.id),
            "consent_status": consent,
            "shared": projected.is_some(),
            "purpose_of_use": ctx.purpose_of_use.as_str(),
        }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    tx.commit().await?;
    Ok(Json(json!({
        "schema": "risk-projection.v1",
        "patient_id": id,
        "tenant_id": p.tenant_id,
        "facility_id": p.facility_id,
        "authorization": {
            "purpose_of_use": ctx.purpose_of_use.as_str(),
            "consent_purpose": PROJECTION_CONSENT_PURPOSE,
            "consent_status": consent.clone().unwrap_or_else(|| "not_recorded".into()),
            "shared": projected.is_some(),
        },
        "assessment_available": current.is_some(),
        "last_updated_at": current.as_ref().map(|c| c.assessment.calculated_at),
        "risk": projected,
        "non_goals": [
            "no_pricing",
            "no_underwriting",
            "no_coverage_decision",
            "no_authorization_decision",
            "no_automated_denial",
        ],
    })))
}

fn review_projection(reviews: &[ReviewRow], domain: &str, current: RiskLevel) -> Value {
    let v = effective_review(reviews, domain, current);
    json!({
        "status": v["status"],
        "reviewed_at": v.get("reviewed_at").cloned().unwrap_or(Value::Null),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn review(domain: &str, status: RiskReviewStatus, level: RiskLevel) -> ReviewRow {
        ReviewRow {
            domain: domain.into(),
            status,
            reviewer_id: Uuid::nil(),
            reviewer_name: "Dr. Test".into(),
            note: None,
            level_at_review: level,
            reviewed_at: Utc::now(),
            assessment_id: Uuid::nil(),
        }
    }

    #[test]
    fn review_stands_while_level_does_not_rise() {
        let reviews = vec![review(
            "diagnostic_result",
            RiskReviewStatus::Reviewed,
            RiskLevel::High,
        )];
        let same = effective_review(&reviews, "diagnostic_result", RiskLevel::High);
        assert_eq!(same["status"], "reviewed");
        let lower = effective_review(&reviews, "diagnostic_result", RiskLevel::Moderate);
        assert_eq!(lower["status"], "reviewed");
        let higher = effective_review(&reviews, "diagnostic_result", RiskLevel::Critical);
        assert_eq!(higher["status"], "unreviewed");
        assert_eq!(higher["superseded_by_higher_level"], true);
        let other = effective_review(&reviews, "acute_safety", RiskLevel::Low);
        assert_eq!(other["status"], "unreviewed");
    }

    fn visit(status: &str, offset_minutes: i64, now: DateTime<Utc>) -> VisitFact {
        VisitFact {
            id: Uuid::now_v7(),
            status: status.into(),
            arrival_kind: "scheduled".into(),
            service: "general_medicine".into(),
            priority: None,
            safety_floor: None,
            occurred_at: now + Duration::minutes(offset_minutes),
        }
    }

    #[test]
    fn current_visit_prefers_active_states_over_later_bookings() {
        let now = Utc::now();
        let later_booking = visit("scheduled", 180, now);
        let arrived = visit("arrived", -40, now);
        let triaged = visit("triage_in_progress", -90, now);
        let closed = visit("completed", 5, now);
        let visits = vec![
            later_booking.clone(),
            closed,
            arrived.clone(),
            triaged.clone(),
        ];
        assert_eq!(select_current_visit(&visits, now).unwrap().id, triaged.id);
        let visits = vec![later_booking.clone(), arrived.clone()];
        assert_eq!(select_current_visit(&visits, now).unwrap().id, arrived.id);
        // Only bookings: the one nearest to now is current.
        let soon = visit("scheduled", 20, now);
        let visits = vec![later_booking, soon.clone(), visit("scheduled", -600, now)];
        assert_eq!(select_current_visit(&visits, now).unwrap().id, soon.id);
        assert!(select_current_visit(&[visit("no_show", -10, now)], now).is_none());
    }

    #[test]
    fn domain_keys_are_validated() {
        assert_eq!(parse_domain_key("overall").unwrap(), "overall");
        assert_eq!(parse_domain_key("acute_safety").unwrap(), "acute_safety");
        assert!(parse_domain_key("score").is_err());
    }

    #[test]
    fn level_rank_sql_orders_critical_first() {
        let sql = level_rank("x");
        assert!(sql.starts_with("CASE x WHEN 'critical' THEN 0"));
        assert!(sql.contains("ELSE 4 END"));
    }
}
