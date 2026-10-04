//! Diagnostic reports and typed result components.
//!
//! `issue_in` is the single ingestion path for every result source (manual
//! entry, the legacy laboratory adapter, inbound integrations): immutable
//! report rows chained by `replaces`, append-only typed observations,
//! deterministic interpretation and criticality, the legacy result loop,
//! critical alerts/tasks and the responsible-professional notification.
//! dMind synthesis and patient explanations are drafts bound to one exact
//! report version; the professional review and the release decision are
//! human actions recorded against that same version.

use super::catalog::{self, Orderable};
use super::{
    actor_label, apply_transition_in, check_len, guard_order, load_order, lock_order, order_json,
    OrderRow, TransitionInput, MAX_SHORT, MAX_TEXT,
};
use crate::aigov;
use crate::audit;
use crate::auth::AuthContext;
use crate::error::ApiError;
use crate::notify;
use crate::policy::{actions, facility_scope};
use crate::routes::guard;
use crate::scheduling;
use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::Json;
use chrono::{DateTime, Utc};
use dmind_gateway::diagnostics::{
    PatientExplanationRequest, ResultSynthesisRequest, SynthesisComponent,
    PATIENT_EXPLANATION_TEMPLATE, RESULT_SYNTHESIS_TEMPLATE,
};
use dmind_gateway::{hash_json, GatewayError};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use std::collections::HashMap;
use uuid::Uuid;
use wellos_domain::ai::{ArtifactStatus, ProviderInfo};
use wellos_domain::diagnostics::{
    interpret_component, report_criticality, ComponentSpec, Interpretation, OrderStatus,
    OrderTransition, ReportStatus, ResultValue, MAX_NARRATIVE_CHARS, MAX_TEXT_VALUE_CHARS,
};
use wellos_domain::diagnostics_ai::{
    DiagnosticResultSynthesisV1, PatientResultExplanationV1, PATIENT_EXPLANATION_SCHEMA,
    RESULT_SYNTHESIS_SCHEMA,
};
use wellos_domain::result_loop::{LoopState, LoopTransition};
use wellos_domain::rules::{baseline_rules, RuleOutcome};
use wellos_domain::units::Quantity;

pub const MAX_COMPONENTS: usize = 200;
const MIN_REASON_CHARS: usize = 3;
const MIN_ASSESSMENT_CHARS: usize = 3;
const MAX_FOLLOW_UPS: usize = 20;
const DEFAULT_LIMIT: i64 = 50;
const MAX_LIMIT: i64 = 200;

pub const DISPOSITIONS: &[&str] = &[
    "no_action",
    "routine_follow_up",
    "urgent_follow_up",
    "repeat_test",
    "referral",
    "immediate_contact",
    "other",
];

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ReportRow {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub patient_id: Uuid,
    pub service_request_id: Uuid,
    pub status: ReportStatus,
    pub version: i64,
    pub replaces: Option<Uuid>,
    pub category_code: Option<String>,
    pub conclusion: Option<String>,
    pub conclusion_codes: Value,
    pub criticality: Interpretation,
    pub criticality_rules: Value,
    pub performer_id: Option<Uuid>,
    pub performing_facility_id: Option<Uuid>,
    pub performing_service_code: Option<String>,
    pub signed_by: Option<Uuid>,
    pub signed_at: Option<DateTime<Utc>>,
    pub issued_at: DateTime<Utc>,
    pub effective_at: Option<DateTime<Utc>>,
    pub source_system: String,
    pub external_report_id: Option<String>,
    pub idempotency_key: String,
    pub payload_hash: Option<String>,
    pub change_reason: Option<String>,
    pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

pub const REPORT_COLUMNS: &str = "r.id, r.tenant_id, r.patient_id, r.service_request_id, r.status, r.version, r.replaces,
    r.category_code, r.conclusion, r.conclusion_codes, r.criticality, r.criticality_rules, r.performer_id,
    r.performing_facility_id, r.performing_service_code, r.signed_by, r.signed_at, r.issued_at, r.effective_at,
    r.source_system, r.external_report_id, r.idempotency_key, r.payload_hash, r.change_reason, r.created_by, r.created_at";

pub fn report_from_row(r: &PgRow) -> Result<ReportRow, ApiError> {
    let status: String = r.get("status");
    let criticality: String = r.get("criticality");
    Ok(ReportRow {
        id: r.get("id"),
        tenant_id: r.get("tenant_id"),
        patient_id: r.get("patient_id"),
        service_request_id: r.get("service_request_id"),
        status: ReportStatus::parse(&status)
            .ok_or_else(|| ApiError::internal("invalid report status"))?,
        version: r.get("version"),
        replaces: r.get("replaces"),
        category_code: r.get("category_code"),
        conclusion: r.get("conclusion"),
        conclusion_codes: r.get("conclusion_codes"),
        criticality: Interpretation::parse(&criticality)
            .ok_or_else(|| ApiError::internal("invalid report criticality"))?,
        criticality_rules: r.get("criticality_rules"),
        performer_id: r.get("performer_id"),
        performing_facility_id: r.get("performing_facility_id"),
        performing_service_code: r.get("performing_service_code"),
        signed_by: r.get("signed_by"),
        signed_at: r.get("signed_at"),
        issued_at: r.get("issued_at"),
        effective_at: r.get("effective_at"),
        source_system: r.get("source_system"),
        external_report_id: r.get("external_report_id"),
        idempotency_key: r.get("idempotency_key"),
        payload_hash: r.get("payload_hash"),
        change_reason: r.get("change_reason"),
        created_by: r.get("created_by"),
        created_at: r.get("created_at"),
    })
}

pub fn report_json(r: &ReportRow) -> Value {
    json!({
        "id": r.id,
        "service_request_id": r.service_request_id,
        "patient_id": r.patient_id,
        "status": r.status.as_str(),
        "version": r.version,
        "replaces": r.replaces,
        "category_code": r.category_code,
        "conclusion": r.conclusion,
        "conclusion_codes": r.conclusion_codes,
        "criticality": r.criticality.as_str(),
        "criticality_rules": r.criticality_rules,
        "performer_id": r.performer_id,
        "performing_facility_id": r.performing_facility_id,
        "performing_service_code": r.performing_service_code,
        "signed_by": r.signed_by,
        "signed_at": r.signed_at,
        "issued_at": r.issued_at,
        "effective_at": r.effective_at,
        "source_system": r.source_system,
        "external_report_id": r.external_report_id,
        "change_reason": r.change_reason,
        "created_at": r.created_at,
        "reviewable": r.status.is_reviewable(),
    })
}

pub async fn load_report(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    id: Uuid,
    for_update: bool,
) -> Result<ReportRow, ApiError> {
    let lock = if for_update { "FOR UPDATE" } else { "" };
    let row = sqlx::query(&format!(
        "SELECT {REPORT_COLUMNS} FROM diagnostic_reports r WHERE r.id = $1 AND r.tenant_id = $2 {lock}"
    ))
    .bind(id)
    .bind(tenant_id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(ApiError::not_found)?;
    report_from_row(&row)
}

async fn latest_report(
    conn: &mut PgConnection,
    order_id: Uuid,
) -> Result<Option<ReportRow>, ApiError> {
    let row = sqlx::query(&format!(
        "SELECT {REPORT_COLUMNS} FROM diagnostic_reports r WHERE r.service_request_id = $1
         ORDER BY r.version DESC LIMIT 1"
    ))
    .bind(order_id)
    .fetch_optional(&mut *conn)
    .await?;
    row.as_ref().map(report_from_row).transpose()
}

async fn replaced_by(conn: &mut PgConnection, report_id: Uuid) -> Result<Option<Uuid>, ApiError> {
    Ok(
        sqlx::query_scalar("SELECT id FROM diagnostic_reports WHERE replaces = $1 LIMIT 1")
            .bind(report_id)
            .fetch_optional(&mut *conn)
            .await?,
    )
}

/// A report is "current" when no later report of its order replaces it.
async fn require_current(conn: &mut PgConnection, r: &ReportRow) -> Result<(), ApiError> {
    if replaced_by(conn, r.id).await?.is_some() {
        return Err(ApiError::conflict(
            "report_superseded",
            "a newer version of this report exists; review the current one",
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Typed components
// ---------------------------------------------------------------------------

pub fn value_text(v: &ResultValue) -> String {
    match v {
        ResultValue::Quantity { value, unit } => format!("{value} {unit}"),
        ResultValue::Text { text } | ResultValue::Narrative { text } => text.clone(),
        ResultValue::Coded { code, display, .. } => display.clone().unwrap_or_else(|| code.clone()),
        ResultValue::Boolean { value } => value.to_string(),
        ResultValue::Datetime { value } => value.to_rfc3339(),
    }
}

fn value_from_row(r: &PgRow) -> Result<ResultValue, ApiError> {
    let t: String = r.get("value_type");
    Ok(match t.as_str() {
        "quantity" => ResultValue::Quantity {
            value: r
                .get::<Option<rust_decimal::Decimal>, _>("value_num")
                .ok_or_else(|| ApiError::internal("quantity without value"))?,
            unit: r.get::<Option<String>, _>("unit").unwrap_or_default(),
        },
        "text" => ResultValue::Text {
            text: r.get::<Option<String>, _>("value_text").unwrap_or_default(),
        },
        "coded" => ResultValue::Coded {
            code: r.get::<Option<String>, _>("value_code").unwrap_or_default(),
            system: r
                .get::<Option<String>, _>("value_code_system")
                .unwrap_or_default(),
            display: r.get("value_code_display"),
        },
        "boolean" => ResultValue::Boolean {
            value: r.get::<Option<bool>, _>("value_bool").unwrap_or(false),
        },
        "datetime" => ResultValue::Datetime {
            value: r
                .get::<Option<DateTime<Utc>>, _>("value_datetime")
                .ok_or_else(|| ApiError::internal("datetime without value"))?,
        },
        "narrative" => ResultValue::Narrative {
            text: r
                .get::<Option<String>, _>("value_narrative")
                .unwrap_or_default(),
        },
        other => return Err(ApiError::internal(format!("unknown value type {other}"))),
    })
}

pub(crate) const COMPONENT_COLUMNS: &str = "o.id, o.code_loinc, o.code_system, o.display, o.value_type, o.value_num, o.unit, o.value_text,
    o.value_code, o.value_code_system, o.value_code_display, o.value_bool, o.value_datetime, o.value_narrative,
    o.reference_range, o.interpretation, o.status, o.amends, o.sequence, o.effective_at, o.received_at,
    o.source_system, o.diagnostic_report_id,
    EXISTS (SELECT 1 FROM observations n WHERE n.amends = o.id) AS superseded";

#[derive(Debug, Clone)]
pub struct ComponentRow {
    pub id: Uuid,
    pub code: String,
    pub system: String,
    pub display: Option<String>,
    pub value: ResultValue,
    pub reference_range: Option<String>,
    pub interpretation: Interpretation,
    pub status: String,
    pub amends: Option<Uuid>,
    pub sequence: i32,
    pub effective_at: DateTime<Utc>,
    pub received_at: DateTime<Utc>,
    pub source_system: String,
    pub superseded: bool,
}

pub(crate) fn component_from_row(r: &PgRow) -> Result<ComponentRow, ApiError> {
    let interp: String = r.get("interpretation");
    Ok(ComponentRow {
        id: r.get("id"),
        code: r.get("code_loinc"),
        system: r.get("code_system"),
        display: r.get("display"),
        value: value_from_row(r)?,
        reference_range: r.get("reference_range"),
        interpretation: Interpretation::parse(&interp)
            .ok_or_else(|| ApiError::internal("invalid interpretation"))?,
        status: r.get("status"),
        amends: r.get("amends"),
        sequence: r.get("sequence"),
        effective_at: r.get("effective_at"),
        received_at: r.get("received_at"),
        source_system: r.get("source_system"),
        superseded: r.get("superseded"),
    })
}

pub fn component_json(c: &ComponentRow) -> Value {
    json!({
        "id": c.id,
        "code": c.code,
        "system": c.system,
        "display": c.display,
        "value": c.value,
        "value_text": value_text(&c.value),
        "reference_range": c.reference_range,
        "interpretation": c.interpretation.as_str(),
        "status": c.status,
        "amends": c.amends,
        "sequence": c.sequence,
        "effective_at": c.effective_at,
        "received_at": c.received_at,
        "source_system": c.source_system,
        "superseded": c.superseded,
    })
}

/// One typed observation by id (tenant scoped), with its patient and the
/// report it belongs to when it was issued through the generalized path.
pub async fn load_component(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    id: Uuid,
) -> Result<(ComponentRow, Uuid, Uuid, Option<Uuid>), ApiError> {
    let row = sqlx::query(&format!(
        "SELECT {COMPONENT_COLUMNS}, o.patient_id, o.service_request_id, o.diagnostic_report_id
         FROM observations o WHERE o.id = $1 AND o.tenant_id = $2"
    ))
    .bind(id)
    .bind(tenant_id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(ApiError::not_found)?;
    Ok((
        component_from_row(&row)?,
        row.get("patient_id"),
        row.get("service_request_id"),
        row.get("diagnostic_report_id"),
    ))
}

pub async fn components_of(
    conn: &mut PgConnection,
    report_id: Uuid,
) -> Result<Vec<ComponentRow>, ApiError> {
    let rows = sqlx::query(&format!(
        "SELECT {COMPONENT_COLUMNS} FROM observations o WHERE o.diagnostic_report_id = $1
         ORDER BY o.sequence, o.id"
    ))
    .bind(report_id)
    .fetch_all(&mut *conn)
    .await?;
    rows.iter().map(component_from_row).collect()
}

/// The most recent earlier value of `code` for the patient (trend input).
async fn prior_component(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    patient_id: Uuid,
    code: &str,
    before: DateTime<Utc>,
    exclude_report: Uuid,
) -> Result<Option<ComponentRow>, ApiError> {
    let row = sqlx::query(&format!(
        "SELECT {COMPONENT_COLUMNS} FROM observations o
         WHERE o.tenant_id = $1 AND o.patient_id = $2 AND o.code_loinc = $3 AND o.effective_at < $4
           AND (o.diagnostic_report_id IS NULL OR o.diagnostic_report_id <> $5)
           AND NOT EXISTS (SELECT 1 FROM observations n WHERE n.amends = o.id)
         ORDER BY o.effective_at DESC, o.id DESC LIMIT 1"
    ))
    .bind(tenant_id)
    .bind(patient_id)
    .bind(code)
    .bind(before)
    .bind(exclude_report)
    .fetch_optional(&mut *conn)
    .await?;
    row.as_ref().map(component_from_row).transpose()
}

// ---------------------------------------------------------------------------
// Issuing a report (the single ingestion path)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct ComponentInput {
    pub code: String,
    pub system: Option<String>,
    pub display: Option<String>,
    pub value: ResultValue,
    pub reference_range: Option<String>,
    pub effective_at: Option<DateTime<Utc>>,
    /// Observation this component corrects (same order). Defaults to the
    /// replaced report's component with the same code.
    pub amends_observation_id: Option<Uuid>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ConclusionCode {
    pub system: String,
    pub code: String,
    pub display: Option<String>,
}

#[derive(Debug, Clone)]
pub struct IssueInput {
    pub status: ReportStatus,
    pub components: Vec<ComponentInput>,
    pub conclusion: Option<String>,
    pub conclusion_codes: Vec<ConclusionCode>,
    pub change_reason: Option<String>,
    pub idempotency_key: String,
    pub source_system: String,
    pub effective_at: Option<DateTime<Utc>>,
    pub external_report_id: Option<String>,
    pub sign: bool,
    pub performer_id: Option<Uuid>,
    /// Leave the observation idempotency key of the first component equal
    /// to the report key (legacy laboratory deliveries are keyed per
    /// observation).
    pub legacy_observation_key: bool,
}

/// Canonical SHA-256 of a delivery so a replayed idempotency key is
/// honoured only for the identical payload.
fn issue_fingerprint(input: &IssueInput) -> String {
    use sha2::Digest;
    let components: Vec<Value> = input
        .components
        .iter()
        .map(|c| {
            json!({
                "code": c.code,
                "system": c.system,
                "display": c.display,
                "value": c.value,
                "reference_range": c.reference_range,
                "effective_at": c.effective_at,
                "amends_observation_id": c.amends_observation_id,
            })
        })
        .collect();
    let codes: Vec<Value> = input
        .conclusion_codes
        .iter()
        .map(|c| json!({ "system": c.system, "code": c.code, "display": c.display }))
        .collect();
    let canonical = json!({
        "status": input.status.as_str(),
        "components": components,
        "conclusion": input.conclusion,
        "conclusion_codes": codes,
        "change_reason": input.change_reason,
        "source_system": input.source_system,
        "effective_at": input.effective_at,
        "external_report_id": input.external_report_id,
        "sign": input.sign,
    });
    hex::encode(sha2::Sha256::digest(canonical.to_string().as_bytes()))
}

#[derive(Debug, Clone)]
pub struct IssueOutcome {
    pub report: ReportRow,
    pub order: OrderRow,
    pub observation_ids: Vec<Uuid>,
    pub critical: bool,
    pub unit_mismatch: bool,
    pub duplicate: bool,
}

fn quantity_rule_outcomes(code: &str, value: &ResultValue) -> Vec<(String, String, RuleOutcome)> {
    let ResultValue::Quantity { value, unit } = value else {
        return Vec::new();
    };
    let observed = Quantity {
        value: *value,
        unit: unit.clone(),
    };
    baseline_rules()
        .into_iter()
        .filter_map(|rule| {
            let outcome = rule.evaluate(code, &observed);
            (!matches!(outcome, RuleOutcome::NotApplicable))
                .then(|| (rule.rule_id.clone(), rule.version.clone(), outcome))
        })
        .collect()
}

fn validate_value(v: &ResultValue) -> Result<(), ApiError> {
    let too_long = match v {
        ResultValue::Text { text } => text.chars().count() > MAX_TEXT_VALUE_CHARS,
        ResultValue::Narrative { text } => text.chars().count() > MAX_NARRATIVE_CHARS,
        ResultValue::Coded {
            code,
            system,
            display,
        } => {
            code.trim().is_empty()
                || system.trim().is_empty()
                || code.chars().count() > MAX_SHORT
                || system.chars().count() > MAX_SHORT
                || display
                    .as_ref()
                    .is_some_and(|d| d.chars().count() > MAX_SHORT)
        }
        ResultValue::Quantity { unit, .. } => unit.chars().count() > 64,
        _ => false,
    };
    if too_long {
        return Err(ApiError::bad_request(
            "validation_failed",
            "a component value is empty or exceeds its maximum length",
        ));
    }
    Ok(())
}

async fn orderable_of(
    conn: &mut PgConnection,
    o: &OrderRow,
) -> Result<Option<Orderable>, ApiError> {
    let Some(id) = o.orderable_id else {
        return Ok(None);
    };
    Ok(catalog::load_by_ids(conn, o.tenant_id, &[id])
        .await?
        .into_iter()
        .next())
}

fn find_spec<'a>(orderable: Option<&'a Orderable>, code: &str) -> Option<&'a ComponentSpec> {
    orderable?.config.components.iter().find(|c| c.code == code)
}

/// Issue one report for the locked `o` inside the caller's transaction.
pub async fn issue_in(
    tx: &mut PgConnection,
    ctx: &AuthContext,
    state: &AppState,
    o: &OrderRow,
    input: IssueInput,
) -> Result<IssueOutcome, ApiError> {
    if input.idempotency_key.trim().is_empty() || input.idempotency_key.len() > 128 {
        return Err(ApiError::bad_request(
            "validation_failed",
            "idempotency_key is required (at most 128 characters)",
        ));
    }
    if input.source_system.trim().is_empty() || input.source_system.len() > 128 {
        return Err(ApiError::bad_request(
            "validation_failed",
            "source_system is required (at most 128 characters)",
        ));
    }
    // Idempotency: the same key returns the stored report (same order only).
    let existing = sqlx::query(&format!(
        "SELECT {REPORT_COLUMNS} FROM diagnostic_reports r WHERE r.tenant_id = $1 AND r.idempotency_key = $2"
    ))
    .bind(o.tenant_id)
    .bind(&input.idempotency_key)
    .fetch_optional(&mut *tx)
    .await?;
    let payload_hash = issue_fingerprint(&input);
    if let Some(row) = existing {
        let r = report_from_row(&row)?;
        if r.service_request_id != o.id
            || r.status != input.status
            || r.payload_hash.as_deref() != Some(&*payload_hash)
        {
            return Err(ApiError::conflict(
                "idempotency_key_reuse",
                "idempotency_key was already used for a different delivery",
            ));
        }
        let ids = components_of(tx, r.id)
            .await?
            .into_iter()
            .map(|c| c.id)
            .collect();
        return Ok(IssueOutcome {
            critical: r.criticality == Interpretation::Critical,
            report: r,
            order: o.clone(),
            observation_ids: ids,
            unit_mismatch: false,
            duplicate: true,
        });
    }

    if matches!(
        o.order_status,
        OrderStatus::Cancelled | OrderStatus::Rejected | OrderStatus::EnteredInError
    ) {
        return Err(ApiError::conflict(
            "order_closed",
            "results cannot be issued for a cancelled, rejected or erroneous order",
        ));
    }
    let prev = latest_report(tx, o.id).await?;
    if !ReportStatus::can_be_replaced_by(prev.as_ref().map(|p| p.status), input.status) {
        return Err(ApiError::conflict(
            "invalid_report_transition",
            format!(
                "a {} report cannot follow {}",
                input.status.as_str(),
                prev.as_ref()
                    .map(|p| p.status.as_str())
                    .unwrap_or("no report")
            ),
        ));
    }
    let needs_reason = matches!(
        input.status,
        ReportStatus::Amended
            | ReportStatus::Corrected
            | ReportStatus::Cancelled
            | ReportStatus::EnteredInError
    );
    let change_reason = check_len("change_reason", input.change_reason.as_deref(), MAX_TEXT)?;
    if needs_reason && change_reason.as_deref().map_or(0, |r| r.chars().count()) < MIN_REASON_CHARS
    {
        return Err(ApiError::bad_request(
            "validation_failed",
            "change_reason is required for amendments, corrections, cancellations and errors",
        ));
    }
    let conclusion = check_len(
        "conclusion",
        input.conclusion.as_deref(),
        MAX_NARRATIVE_CHARS,
    )?;
    let external_report_id = check_len(
        "external_report_id",
        input.external_report_id.as_deref(),
        MAX_SHORT,
    )?;
    if input.components.len() > MAX_COMPONENTS {
        return Err(ApiError::bad_request(
            "validation_failed",
            format!("at most {MAX_COMPONENTS} components per report"),
        ));
    }
    let carries_results = !matches!(
        input.status,
        ReportStatus::Cancelled | ReportStatus::EnteredInError
    );
    if carries_results && input.components.is_empty() && conclusion.is_none() {
        return Err(ApiError::bad_request(
            "validation_failed",
            "a report needs at least one component or a conclusion",
        ));
    }
    for c in &input.conclusion_codes {
        if c.code.trim().is_empty() || c.system.trim().is_empty() {
            return Err(ApiError::bad_request(
                "validation_failed",
                "conclusion codes need a system and a code",
            ));
        }
    }
    let orderable = orderable_of(tx, o).await?;
    let has_specs = orderable
        .as_ref()
        .is_some_and(|ob| !ob.config.components.is_empty());

    // Validate components against the orderable before anything is written.
    let mut prepared = Vec::with_capacity(input.components.len());
    for (seq, c) in input.components.iter().enumerate() {
        let code = c.code.trim();
        if code.is_empty() || code.len() > 64 {
            return Err(ApiError::bad_request(
                "validation_failed",
                "component code is required",
            ));
        }
        validate_value(&c.value)?;
        let spec = find_spec(orderable.as_ref(), code);
        let ordered_code = o.code_loinc.as_deref() == Some(code)
            || orderable
                .as_ref()
                .is_some_and(|ob| ob.code == code || ob.loinc().as_deref() == Some(code));
        if has_specs && spec.is_none() && !ordered_code {
            return Err(ApiError::bad_request(
                "code_mismatch",
                format!("component {code} is not part of the ordered test"),
            ));
        }
        if !has_specs && orderable.is_none() && !ordered_code {
            return Err(ApiError::bad_request(
                "code_mismatch",
                "result code_loinc does not match the ordered test",
            ));
        }
        if let Some(s) = spec {
            if s.result_type != c.value.result_type() {
                return Err(ApiError::bad_request(
                    "type_mismatch",
                    format!(
                        "component {code} expects a {} value",
                        s.result_type.as_str()
                    ),
                ));
            }
        }
        let reference_range = check_len("reference_range", c.reference_range.as_deref(), 128)?
            .or_else(|| spec.and_then(|s| s.reference_range.clone()));
        let display = check_len("display", c.display.as_deref(), MAX_SHORT)?
            .or_else(|| spec.map(|s| s.display.clone()));
        let system = check_len("system", c.system.as_deref(), MAX_SHORT)?
            .or_else(|| spec.map(|s| s.system.clone()))
            .unwrap_or_else(|| "http://loinc.org".to_string());
        let mut amends = c.amends_observation_id;
        if let Some(a) = amends {
            let ok: Option<Uuid> = sqlx::query_scalar(
                "SELECT id FROM observations WHERE id = $1 AND tenant_id = $2 AND service_request_id = $3",
            )
            .bind(a)
            .bind(o.tenant_id)
            .bind(o.id)
            .fetch_optional(&mut *tx)
            .await?;
            if ok.is_none() {
                return Err(ApiError::bad_request(
                    "unknown_amended_observation",
                    "amends_observation_id does not match an observation of this request",
                ));
            }
        } else if matches!(
            input.status,
            ReportStatus::Amended | ReportStatus::Corrected
        ) {
            if let Some(p) = &prev {
                amends = sqlx::query_scalar(
                    "SELECT id FROM observations WHERE diagnostic_report_id = $1 AND code_loinc = $2
                     ORDER BY sequence DESC LIMIT 1",
                )
                .bind(p.id)
                .bind(code)
                .fetch_optional(&mut *tx)
                .await?;
            }
        }
        let rules = quantity_rule_outcomes(code, &c.value);
        let critical_by_rule = rules
            .iter()
            .any(|(_, _, r)| matches!(r, RuleOutcome::Critical { .. }));
        let interpretation = interpret_component(
            &c.value,
            reference_range.as_deref(),
            critical_by_rule,
            spec.and_then(|s| s.interpretation.as_ref()),
        );
        let source = if critical_by_rule {
            "baseline_rule"
        } else {
            match (&c.value, spec.and_then(|s| s.interpretation.as_ref())) {
                (ResultValue::Quantity { .. }, _) if reference_range.is_some() => "reference_range",
                (ResultValue::Coded { .. } | ResultValue::Boolean { .. }, Some(_)) => "coded_rule",
                _ => "none",
            }
        };
        prepared.push((
            seq as i32,
            code.to_string(),
            system,
            display,
            reference_range,
            amends,
            rules,
            interpretation,
            source,
            c.effective_at
                .or(input.effective_at)
                .unwrap_or_else(Utc::now),
        ));
    }
    let interps: Vec<Interpretation> = prepared.iter().map(|p| p.7).collect();
    let conclusion_code_strs: Vec<String> = input
        .conclusion_codes
        .iter()
        .map(|c| c.code.clone())
        .collect();
    let critical_conclusion_codes: Vec<String> = orderable
        .as_ref()
        .map(|ob| ob.config.critical_conclusion_codes.clone())
        .unwrap_or_default();
    let criticality = if carries_results {
        report_criticality(&interps, &conclusion_code_strs, &critical_conclusion_codes)
    } else {
        Interpretation::Unknown
    };
    let mut criticality_rules: Vec<Value> = prepared
        .iter()
        .map(|p| {
            json!({ "component_ref": format!("component:seq:{}", p.0), "code": p.1,
                    "interpretation": p.7.as_str(), "reference_range": p.4, "source": p.8 })
        })
        .collect();
    for c in &conclusion_code_strs {
        if critical_conclusion_codes.iter().any(|k| k == c) {
            criticality_rules.push(json!({ "conclusion_code": c, "interpretation": "critical",
                                           "source": "critical_conclusion_code" }));
        }
    }

    // Everything the replaced report raised that is still pending no longer
    // describes the current picture.
    if let Some(p) = &prev {
        sqlx::query(
            "UPDATE ai_artifacts SET status = 'superseded'
             WHERE tenant_id = $1 AND status IN ('draft','awaiting_review','unavailable')
               AND (diagnostic_report_id = $2
                    OR observation_id IN (SELECT id FROM observations WHERE diagnostic_report_id = $2))",
        )
        .bind(o.tenant_id)
        .bind(p.id)
        .execute(&mut *tx)
        .await?;
    }
    for (_, _, _, _, _, amends, ..) in &prepared {
        if let Some(a) = amends {
            sqlx::query(
                "UPDATE ai_artifacts SET status='superseded'
                 WHERE tenant_id=$1 AND observation_id=$2
                   AND status IN ('draft','awaiting_review','unavailable')",
            )
            .bind(o.tenant_id)
            .bind(a)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "UPDATE alerts SET status='superseded', closed_at=now()
                 WHERE tenant_id=$1 AND observation_id=$2 AND status='open'",
            )
            .bind(o.tenant_id)
            .bind(a)
            .execute(&mut *tx)
            .await?;
        }
    }
    let is_amendment = prev.as_ref().is_some_and(|p| p.status.is_reviewable()) && carries_results;
    if is_amendment || !carries_results {
        if let Some(p) = &prev {
            sqlx::query(
                "UPDATE alerts SET status='superseded', closed_at=now()
                 WHERE tenant_id=$1 AND status='open'
                   AND (diagnostic_report_id=$2
                        OR observation_id IN (SELECT id FROM observations WHERE diagnostic_report_id=$2))",
            )
            .bind(o.tenant_id)
            .bind(p.id)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query(
            "UPDATE follow_up_tasks SET status='superseded'
             WHERE tenant_id=$1 AND service_request_id=$2 AND status IN ('open','overdue')",
        )
        .bind(o.tenant_id)
        .bind(o.id)
        .execute(&mut *tx)
        .await?;
    }

    let report_id = Uuid::now_v7();
    let version = prev.as_ref().map(|p| p.version + 1).unwrap_or(1);
    let signed = input.sign && input.status.is_reviewable();
    let inserted = sqlx::query(
        "INSERT INTO diagnostic_reports
         (id, tenant_id, patient_id, service_request_id, status, version, replaces, category_code, conclusion,
          conclusion_codes, criticality, criticality_rules, performer_id, performing_facility_id,
          performing_service_code, signed_by, signed_at, effective_at, source_system, external_report_id,
          idempotency_key, change_reason, created_by, payload_hash)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,
                 CASE WHEN $16::uuid IS NULL THEN NULL ELSE now() END,$17,$18,$19,$20,$21,$22,$23)",
    )
    .bind(report_id)
    .bind(o.tenant_id)
    .bind(o.patient_id)
    .bind(o.id)
    .bind(input.status.as_str())
    .bind(version)
    .bind(prev.as_ref().map(|p| p.id))
    .bind(&o.category_code)
    .bind(&conclusion)
    .bind(serde_json::to_value(
        input
            .conclusion_codes
            .iter()
            .map(|c| json!({ "system": c.system, "code": c.code, "display": c.display }))
            .collect::<Vec<_>>(),
    )
    .map_err(ApiError::internal)?)
    .bind(criticality.as_str())
    .bind(Value::Array(criticality_rules))
    .bind(input.performer_id.or(Some(ctx.user_id)))
    .bind(o.performing_facility_id)
    .bind(&o.performing_service_code)
    .bind(signed.then_some(ctx.user_id))
    .bind(input.effective_at)
    .bind(&input.source_system)
    .bind(&external_report_id)
    .bind(&input.idempotency_key)
    .bind(&change_reason)
    .bind(ctx.user_id)
    .bind(&payload_hash)
    .execute(&mut *tx)
    .await;
    if let Err(e) = inserted {
        if matches!(&e, sqlx::Error::Database(db) if db.is_unique_violation()) {
            return Err(ApiError::conflict(
                "report_conflict",
                "a report with this key or version was recorded concurrently",
            ));
        }
        return Err(e.into());
    }

    let mut observation_ids = Vec::with_capacity(prepared.len());
    let mut unit_mismatch = false;
    let mut first_critical: Option<Uuid> = None;
    for (
        seq,
        code,
        system,
        display,
        reference_range,
        amends,
        rules,
        interpretation,
        _,
        effective_at,
    ) in &prepared
    {
        let c = &input.components[*seq as usize];
        let obs_id = Uuid::now_v7();
        let key = if *seq == 0 && input.legacy_observation_key {
            input.idempotency_key.clone()
        } else {
            format!("{}#{seq}", input.idempotency_key)
        };
        let (
            value_num,
            unit,
            value_text,
            value_code,
            value_code_system,
            value_code_display,
            value_bool,
            value_datetime,
            value_narrative,
        ) = match &c.value {
            ResultValue::Quantity { value, unit } => (
                Some(*value),
                Some(unit.clone()),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            ),
            ResultValue::Text { text } => (
                None,
                None,
                Some(text.clone()),
                None,
                None,
                None,
                None,
                None,
                None,
            ),
            ResultValue::Coded {
                code,
                system,
                display,
            } => (
                None,
                None,
                None,
                Some(code.clone()),
                Some(system.clone()),
                display.clone(),
                None,
                None,
                None,
            ),
            ResultValue::Boolean { value } => {
                (None, None, None, None, None, None, Some(*value), None, None)
            }
            ResultValue::Datetime { value } => {
                (None, None, None, None, None, None, None, Some(*value), None)
            }
            ResultValue::Narrative { text } => (
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                Some(text.clone()),
            ),
        };
        let inserted = sqlx::query(
            "INSERT INTO observations
             (id, tenant_id, service_request_id, patient_id, code_loinc, code_system, display, value_type,
              value_num, unit, value_text, value_code, value_code_system, value_code_display, value_bool,
              value_datetime, value_narrative, reference_range, interpretation, status, amends, source_system,
              idempotency_key, effective_at, diagnostic_report_id, sequence)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22,$23,$24,$25,$26)",
        )
        .bind(obs_id)
        .bind(o.tenant_id)
        .bind(o.id)
        .bind(o.patient_id)
        .bind(code)
        .bind(system)
        .bind(display)
        .bind(c.value.result_type().as_str())
        .bind(value_num)
        .bind(unit)
        .bind(value_text)
        .bind(value_code)
        .bind(value_code_system)
        .bind(value_code_display)
        .bind(value_bool)
        .bind(value_datetime)
        .bind(value_narrative)
        .bind(reference_range)
        .bind(interpretation.as_str())
        .bind(if amends.is_some() { "corrected" } else { "final" })
        .bind(amends)
        .bind(&input.source_system)
        .bind(&key)
        .bind(effective_at)
        .bind(report_id)
        .bind(seq)
        .execute(&mut *tx)
        .await;
        if let Err(e) = inserted {
            if matches!(&e, sqlx::Error::Database(db) if db.is_unique_violation()) {
                return Err(ApiError::conflict(
                    "idempotency_key_reuse",
                    "idempotency_key was already used for a different delivery",
                ));
            }
            return Err(e.into());
        }
        for (rule_id, rule_version, outcome) in rules {
            sqlx::query(
                "INSERT INTO rule_evaluations (id, tenant_id, observation_id, rule_id, rule_version, outcome)
                 VALUES ($1,$2,$3,$4,$5,$6)",
            )
            .bind(Uuid::now_v7())
            .bind(o.tenant_id)
            .bind(obs_id)
            .bind(rule_id)
            .bind(rule_version)
            .bind(serde_json::to_value(outcome).map_err(ApiError::internal)?)
            .execute(&mut *tx)
            .await?;
            if let RuleOutcome::UnitMismatch { reason } = outcome {
                unit_mismatch = true;
                sqlx::query(
                    "INSERT INTO data_quality_issues (id, tenant_id, resource_type, resource_id, issue)
                     VALUES ($1,$2,'observation',$3,$4)",
                )
                .bind(Uuid::now_v7())
                .bind(o.tenant_id)
                .bind(obs_id)
                .bind(reason)
                .execute(&mut *tx)
                .await?;
            }
        }
        if *interpretation == Interpretation::Critical && first_critical.is_none() {
            first_critical = Some(obs_id);
        }
        observation_ids.push(obs_id);
    }

    let mut order = o.clone();
    if carries_results {
        // Legacy result loop: receipt (or amendment) of a reviewable report.
        if input.status.is_reviewable() {
            let loop_state = LoopState::parse(&order.loop_state)
                .ok_or_else(|| ApiError::internal("invalid loop state in database"))?;
            let transition = if is_amendment && loop_state != LoopState::Ordered {
                LoopTransition::ResultAmended
            } else {
                LoopTransition::ResultReceived
            };
            let next = match loop_state.apply(transition) {
                Ok(n) => n,
                Err(_) if loop_state == LoopState::Closed => {
                    return Err(ApiError::conflict(
                        "invalid_loop_transition",
                        "the result loop of this order is closed; reopen it before amending",
                    ))
                }
                Err(e) => return Err(ApiError::conflict("invalid_loop_transition", e.to_string())),
            };
            let updated = sqlx::query(
                "UPDATE service_requests SET loop_state = $1, version = version + 1, updated_at = now()
                 WHERE id = $2 AND version = $3",
            )
            .bind(next.as_str())
            .bind(order.id)
            .bind(order.version)
            .execute(&mut *tx)
            .await?;
            if updated.rows_affected() == 0 {
                return Err(ApiError::conflict(
                    "version_conflict",
                    "service request was modified concurrently",
                ));
            }
            order = load_order(tx, o.tenant_id, o.id).await?;
        }
        // Fulfilment: a result proves the order was performed. Orders still
        // waiting are moved through acceptance/start by this deterministic
        // consequence; held orders need a human decision first.
        if order.order_status == OrderStatus::OnHold {
            return Err(ApiError::conflict(
                "order_on_hold",
                "resume the order before recording results",
            ));
        }
        let actor = actor_label(ctx);
        let details = json!({ "report_id": report_id, "trigger": "report_issued" });
        if order.order_status == OrderStatus::Placed {
            order = apply_transition_in(
                tx,
                ctx,
                state,
                &order,
                TransitionInput {
                    transition: OrderTransition::Accept,
                    reason: None,
                    actor: &actor,
                    actor_user_id: Some(ctx.user_id),
                    details: details.clone(),
                    event: "diagnostic_order.accepted",
                },
            )
            .await?;
        }
        if matches!(
            order.order_status,
            OrderStatus::Accepted | OrderStatus::Scheduled
        ) {
            if order.order_status == OrderStatus::Accepted
                && order.fulfilment_mode.requires_appointment()
            {
                // Results arrived for an order that was never started: record
                // the performed fulfilment explicitly rather than inventing an
                // appointment.
                sqlx::query(
                    "UPDATE service_requests SET fulfilment_mode = 'immediate', version = version + 1,
                            updated_at = now() WHERE id = $1 AND version = $2",
                )
                .bind(order.id)
                .bind(order.version)
                .execute(&mut *tx)
                .await?;
                order = load_order(tx, o.tenant_id, o.id).await?;
            }
            order = apply_transition_in(
                tx,
                ctx,
                state,
                &order,
                TransitionInput {
                    transition: OrderTransition::Start,
                    reason: None,
                    actor: &actor,
                    actor_user_id: Some(ctx.user_id),
                    details: details.clone(),
                    event: "diagnostic_order.started",
                },
            )
            .await?;
        }
        if input.status.is_reviewable() && order.order_status == OrderStatus::InProgress {
            order = apply_transition_in(
                tx,
                ctx,
                state,
                &order,
                TransitionInput {
                    transition: OrderTransition::Complete,
                    reason: None,
                    actor: &actor,
                    actor_user_id: Some(ctx.user_id),
                    details,
                    event: "diagnostic_order.completed",
                },
            )
            .await?;
        }
    }

    let critical =
        carries_results && criticality == Interpretation::Critical && input.status.is_reviewable();
    if critical {
        let alert_id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO alerts (id, tenant_id, patient_id, observation_id, diagnostic_report_id, severity, message)
             VALUES ($1,$2,$3,$4,$5,'critical','Critical laboratory result requires review')",
        )
        .bind(alert_id)
        .bind(o.tenant_id)
        .bind(o.patient_id)
        .bind(first_critical)
        .bind(report_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO follow_up_tasks (id, tenant_id, patient_id, service_request_id, description, priority, due_at)
             VALUES ($1,$2,$3,$4,'Review critical laboratory result and document follow-up','high', now() + interval '1 hour')",
        )
        .bind(Uuid::now_v7())
        .bind(o.tenant_id)
        .bind(o.patient_id)
        .bind(o.id)
        .execute(&mut *tx)
        .await?;
        audit::emit(
            &mut *tx,
            ctx,
            "result.critical_flagged",
            &state.cell,
            json!({ "observation_id": first_critical, "diagnostic_report_id": report_id, "alert_id": alert_id }),
            None,
        )
        .await
        .map_err(ApiError::internal)?;
        audit::emit(
            &mut *tx,
            ctx,
            "follow_up.created",
            &state.cell,
            json!({ "service_request_id": o.id }),
            None,
        )
        .await
        .map_err(ApiError::internal)?;
    }

    if input.status.is_reviewable() {
        // Earlier release decisions describe a superseded version.
        sqlx::query(
            "UPDATE result_release_decisions SET superseded_at = now()
             WHERE service_request_id = $1 AND superseded_at IS NULL",
        )
        .bind(o.id)
        .execute(&mut *tx)
        .await?;
        notify_responsible(tx, ctx, state, &order, report_id, criticality).await?;
    }

    audit::emit(
        &mut *tx,
        ctx,
        if is_amendment { "result.amended" } else { "result.received" },
        &state.cell,
        json!({ "diagnostic_report_id": report_id, "service_request_id": o.id, "status": input.status.as_str(),
                "version": version, "criticality": criticality.as_str(), "observation_id": observation_ids.first(),
                "components": observation_ids.len() }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    if carries_results {
        super::super::risk::recalculate_after_change(
            tx,
            ctx,
            state,
            o.tenant_id,
            o.patient_id,
            if is_amendment {
                "result.amended"
            } else {
                "result.received"
            },
        )
        .await?;
    }
    let report = load_report(tx, o.tenant_id, report_id, false).await?;
    Ok(IssueOutcome {
        report,
        order,
        observation_ids,
        critical,
        unit_mismatch,
        duplicate: false,
    })
}

fn staff_recipient(policy: &scheduling::Policy) -> notify::Recipient {
    notify::Recipient {
        language: "en".into(),
        time_zone: policy.time_zone.clone(),
        channels: vec!["in_app".into()],
        quiet_start: policy.quiet_hours_start,
        quiet_end: policy.quiet_hours_end,
    }
}

/// In-app notice to the responsible professional (the requester) that a
/// report awaits review. Identifiers only; nothing clinical in the payload.
async fn notify_responsible(
    tx: &mut PgConnection,
    ctx: &AuthContext,
    state: &AppState,
    o: &OrderRow,
    report_id: Uuid,
    criticality: Interpretation,
) -> Result<(), ApiError> {
    let policy = scheduling::load_policy(tx, o.tenant_id).await?;
    let recipient = staff_recipient(&policy);
    let id = notify::enqueue(
        tx,
        ctx,
        &state.cell,
        notify::Enqueue {
            tenant_id: o.tenant_id,
            patient_id: Some(o.patient_id),
            user_id: Some(o.requester_id),
            kind: "diagnostic_report_review",
            appointment_id: None,
            offer_id: None,
            transport_request_id: None,
            payload: json!({ "diagnostic_report_id": report_id, "service_request_id": o.id,
                             "criticality": criticality.as_str(), "priority": o.priority.as_str() }),
            dedupe_key: format!("diagnostic_report_review:{report_id}"),
            scheduled_for: Utc::now(),
            recipient: &recipient,
        },
    )
    .await?;
    if let Some(id) = id {
        sqlx::query(
            "UPDATE notifications SET service_request_id = $2, diagnostic_report_id = $3 WHERE id = $1",
        )
        .bind(id)
        .bind(o.id)
        .bind(report_id)
        .execute(&mut *tx)
        .await?;
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct IssueBody {
    pub status: String,
    #[serde(default)]
    pub components: Vec<ComponentInput>,
    pub conclusion: Option<String>,
    #[serde(default)]
    pub conclusion_codes: Vec<ConclusionCode>,
    pub change_reason: Option<String>,
    pub idempotency_key: Option<String>,
    pub source_system: Option<String>,
    pub effective_at: Option<DateTime<Utc>>,
    pub external_report_id: Option<String>,
    /// Sign the report on issue (final/amended/corrected only). Default true.
    pub sign: Option<bool>,
}

/// `POST /api/v1/diagnostics/orders/:id/reports`
pub async fn issue(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(order_id): Path<Uuid>,
    Json(body): Json<IssueBody>,
) -> Result<Json<Value>, ApiError> {
    let status = super::parse_report_status(&body.status)?;
    let mut conn = state.pool.acquire().await?;
    let o = load_order(&mut conn, ctx.tenant_id, order_id).await?;
    drop(conn);
    let action = if status == ReportStatus::EnteredInError {
        actions::DIAGNOSTIC_REVIEW
    } else {
        actions::DIAGNOSTIC_REPORT_WRITE
    };
    let allowed = guard_order(&state, &ctx, action, &o).await?;
    let mut tx = state.pool.begin().await?;
    let o = lock_order(&mut tx, ctx.tenant_id, order_id).await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let input = IssueInput {
        status,
        components: body.components,
        conclusion: body.conclusion,
        conclusion_codes: body.conclusion_codes,
        change_reason: body.change_reason,
        idempotency_key: body
            .idempotency_key
            .filter(|k| !k.trim().is_empty())
            .unwrap_or_else(|| format!("manual:{}", Uuid::now_v7())),
        source_system: body
            .source_system
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "wellos-manual".to_string()),
        effective_at: body.effective_at,
        external_report_id: body.external_report_id,
        sign: body.sign.unwrap_or(true),
        performer_id: Some(ctx.user_id),
        legacy_observation_key: false,
    };
    let out = issue_in(&mut tx, &ctx, &state, &o, input).await?;
    tx.commit().await?;
    let mut conn = state.pool.acquire().await?;
    let mut v = report_detail_json(&mut conn, &out.report).await?;
    v["order"] = order_json(&out.order);
    v["duplicate"] = json!(out.duplicate);
    v["critical"] = json!(out.critical);
    v["unit_mismatch"] = json!(out.unit_mismatch);
    Ok(Json(v))
}

// ---------------------------------------------------------------------------
// Views
// ---------------------------------------------------------------------------

async fn reviews_json(conn: &mut PgConnection, report_id: Uuid) -> Result<Vec<Value>, ApiError> {
    let rows = sqlx::query(
        "SELECT v.id, v.report_version, v.reviewer_id, u.display_name AS reviewer_name, v.clinical_assessment,
                v.disposition, v.disposition_note, v.follow_up_task_ids, v.synthesis_artifact_id, v.reviewed_at
         FROM diagnostic_reviews v JOIN users u ON u.id = v.reviewer_id
         WHERE v.diagnostic_report_id = $1 ORDER BY v.reviewed_at",
    )
    .bind(report_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .iter()
        .map(|r| {
            json!({
                "id": r.get::<Uuid, _>("id"),
                "report_version": r.get::<i64, _>("report_version"),
                "reviewer_id": r.get::<Uuid, _>("reviewer_id"),
                "reviewer_name": r.get::<String, _>("reviewer_name"),
                "clinical_assessment": r.get::<String, _>("clinical_assessment"),
                "disposition": r.get::<String, _>("disposition"),
                "disposition_note": r.get::<Option<String>, _>("disposition_note"),
                "follow_up_task_ids": r.get::<Vec<Uuid>, _>("follow_up_task_ids"),
                "synthesis_artifact_id": r.get::<Option<Uuid>, _>("synthesis_artifact_id"),
                "reviewed_at": r.get::<DateTime<Utc>, _>("reviewed_at"),
            })
        })
        .collect())
}

async fn releases_json(conn: &mut PgConnection, report_id: Uuid) -> Result<Vec<Value>, ApiError> {
    let rows = sqlx::query(
        "SELECT id, report_version, review_id, decision, withhold_reason, explanation_en, explanation_es,
                explanation_artifact_id, notify_patient, notification_id, decided_by, decided_at, superseded_at
         FROM result_release_decisions WHERE diagnostic_report_id = $1 ORDER BY decided_at",
    )
    .bind(report_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows.iter().map(release_row_json).collect())
}

fn release_row_json(r: &PgRow) -> Value {
    json!({
        "id": r.get::<Uuid, _>("id"),
        "report_version": r.get::<i64, _>("report_version"),
        "review_id": r.get::<Uuid, _>("review_id"),
        "decision": r.get::<String, _>("decision"),
        "withhold_reason": r.get::<Option<String>, _>("withhold_reason"),
        "explanation_en": r.get::<Option<String>, _>("explanation_en"),
        "explanation_es": r.get::<Option<String>, _>("explanation_es"),
        "explanation_artifact_id": r.get::<Option<Uuid>, _>("explanation_artifact_id"),
        "notify_patient": r.get::<bool, _>("notify_patient"),
        "notification_id": r.get::<Option<Uuid>, _>("notification_id"),
        "decided_by": r.get::<Uuid, _>("decided_by"),
        "decided_at": r.get::<DateTime<Utc>, _>("decided_at"),
        "superseded_at": r.get::<Option<DateTime<Utc>>, _>("superseded_at"),
    })
}

pub fn artifact_row_json(r: &PgRow) -> Value {
    json!({
        "id": r.get::<Uuid, _>("id"),
        "artifact_type": r.get::<String, _>("artifact_type"),
        "autonomy_level": r.get::<String, _>("autonomy_level"),
        "status": r.get::<String, _>("status"),
        "model": r.get::<Option<String>, _>("model"),
        "model_version": r.get::<Option<String>, _>("model_version"),
        "route": r.get::<Option<String>, _>("route"),
        "template": r.get::<Option<String>, _>("template"),
        "prompt_version": r.get::<Option<String>, _>("prompt_version"),
        "output": r.get::<Option<Value>, _>("output"),
        "citations": r.get::<Option<Value>, _>("citations"),
        "limitations": r.get::<Option<Value>, _>("limitations"),
        "synthetic": r.get::<bool, _>("synthetic"),
        "generated_at": r.get::<Option<DateTime<Utc>>, _>("generated_at"),
        "reviewer_id": r.get::<Option<Uuid>, _>("reviewer_id"),
        "review_decision": r.get::<Option<String>, _>("review_decision"),
        "review_note": r.get::<Option<String>, _>("review_note"),
        "reviewed_at": r.get::<Option<DateTime<Utc>>, _>("reviewed_at"),
    })
}

const ARTIFACT_COLUMNS: &str = "id, artifact_type, autonomy_level, status, model, model_version, route, template,
    prompt_version, output, citations, limitations, synthetic, generated_at, reviewer_id, review_decision,
    review_note, reviewed_at";

async fn artifacts_json(
    conn: &mut PgConnection,
    report_id: Uuid,
    artifact_type: &str,
) -> Result<Vec<Value>, ApiError> {
    let rows = sqlx::query(&format!(
        "SELECT {ARTIFACT_COLUMNS} FROM ai_artifacts
         WHERE diagnostic_report_id = $1 AND artifact_type = $2 ORDER BY generated_at DESC NULLS LAST, id DESC LIMIT 10"
    ))
    .bind(report_id)
    .bind(artifact_type)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows.iter().map(artifact_row_json).collect())
}

async fn load_artifact(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    report_id: Uuid,
    artifact_id: Uuid,
    artifact_type: &str,
    for_update: bool,
) -> Result<PgRow, ApiError> {
    let lock = if for_update { "FOR UPDATE" } else { "" };
    sqlx::query(&format!(
        "SELECT {ARTIFACT_COLUMNS} FROM ai_artifacts
         WHERE id = $1 AND tenant_id = $2 AND diagnostic_report_id = $3 AND artifact_type = $4 {lock}"
    ))
    .bind(artifact_id)
    .bind(tenant_id)
    .bind(report_id)
    .bind(artifact_type)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(ApiError::not_found)
}

/// Report with components, chain, reviews, release decisions, dMind
/// drafts, documents and imaging references.
pub async fn report_detail_json(conn: &mut PgConnection, r: &ReportRow) -> Result<Value, ApiError> {
    let mut v = report_json(r);
    v["components"] = Value::Array(
        components_of(conn, r.id)
            .await?
            .iter()
            .map(component_json)
            .collect(),
    );
    let successor = replaced_by(conn, r.id).await?;
    v["replaced_by"] = json!(successor);
    v["reviewable"] = json!(r.status.is_reviewable() && successor.is_none());
    v["reviews"] = Value::Array(reviews_json(conn, r.id).await?);
    v["release_decisions"] = Value::Array(releases_json(conn, r.id).await?);
    v["synthesis"] = Value::Array(artifacts_json(conn, r.id, "diagnostic_result_synthesis").await?);
    v["explanations"] =
        Value::Array(artifacts_json(conn, r.id, "patient_result_explanation").await?);
    v["documents"] = Value::Array(super::documents::list_for_report_json(conn, r.id, false).await?);
    v["imaging_studies"] =
        Value::Array(super::documents::imaging_for_report_json(conn, r.id).await?);
    Ok(v)
}

/// All reports of one order, newest first, with components.
pub async fn list_for_order_json(
    conn: &mut PgConnection,
    o: &OrderRow,
) -> Result<Vec<Value>, ApiError> {
    let rows = sqlx::query(&format!(
        "SELECT {REPORT_COLUMNS} FROM diagnostic_reports r WHERE r.service_request_id = $1 ORDER BY r.version DESC"
    ))
    .bind(o.id)
    .fetch_all(&mut *conn)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let r = report_from_row(row)?;
        let mut v = report_json(&r);
        v["components"] = Value::Array(
            components_of(conn, r.id)
                .await?
                .iter()
                .map(component_json)
                .collect(),
        );
        let successor = replaced_by(conn, r.id).await?;
        v["replaced_by"] = json!(successor);
        v["reviewable"] = json!(r.status.is_reviewable() && successor.is_none());
        v["reviews"] = Value::Array(reviews_json(conn, r.id).await?);
        v["release_decisions"] = Value::Array(releases_json(conn, r.id).await?);
        out.push(v);
    }
    Ok(out)
}

/// Adds `latest_report_status`, `latest_report_id`, `latest_report_criticality`
/// and `latest_report_reviewed` to order items (keyed by `id`).
pub async fn attach_latest_report_status(
    conn: &mut PgConnection,
    items: &mut [Value],
) -> Result<(), ApiError> {
    let ids: Vec<Uuid> = items
        .iter()
        .filter_map(|v| v.get("id").and_then(Value::as_str))
        .filter_map(|s| Uuid::parse_str(s).ok())
        .collect();
    if ids.is_empty() {
        return Ok(());
    }
    let rows = sqlx::query(
        "SELECT DISTINCT ON (r.service_request_id) r.service_request_id, r.id, r.status, r.criticality, r.version,
                EXISTS (SELECT 1 FROM diagnostic_reviews v WHERE v.diagnostic_report_id = r.id) AS reviewed,
                EXISTS (SELECT 1 FROM result_release_decisions d WHERE d.diagnostic_report_id = r.id
                        AND d.decision = 'release' AND d.superseded_at IS NULL) AS released
         FROM diagnostic_reports r WHERE r.service_request_id = ANY($1)
         ORDER BY r.service_request_id, r.version DESC",
    )
    .bind(&ids)
    .fetch_all(&mut *conn)
    .await?;
    let by_order: HashMap<Uuid, &PgRow> = rows
        .iter()
        .map(|r| (r.get::<Uuid, _>("service_request_id"), r))
        .collect();
    for item in items.iter_mut() {
        let Some(id) = item
            .get("id")
            .and_then(Value::as_str)
            .and_then(|s| Uuid::parse_str(s).ok())
        else {
            continue;
        };
        match by_order.get(&id) {
            Some(r) => {
                item["latest_report_id"] = json!(r.get::<Uuid, _>("id"));
                item["latest_report_status"] = json!(r.get::<String, _>("status"));
                item["latest_report_criticality"] = json!(r.get::<String, _>("criticality"));
                item["latest_report_version"] = json!(r.get::<i64, _>("version"));
                item["latest_report_reviewed"] = json!(r.get::<bool, _>("reviewed"));
                item["latest_report_released"] = json!(r.get::<bool, _>("released"));
            }
            None => {
                item["latest_report_id"] = Value::Null;
                item["latest_report_status"] = Value::Null;
                item["latest_report_criticality"] = Value::Null;
                item["latest_report_version"] = Value::Null;
                item["latest_report_reviewed"] = json!(false);
                item["latest_report_released"] = json!(false);
            }
        }
    }
    Ok(())
}

/// Latest reviewable report per order for the patient, newest first.
pub async fn recent_for_patient_json(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    patient_id: Uuid,
    limit: i64,
) -> Result<Vec<Value>, ApiError> {
    let rows = sqlx::query(&format!(
        "SELECT {REPORT_COLUMNS}, sr.display AS order_display, sr.category_code AS order_category,
                sr.modality_code, sr.priority,
                EXISTS (SELECT 1 FROM diagnostic_reviews v WHERE v.diagnostic_report_id = r.id) AS reviewed,
                EXISTS (SELECT 1 FROM result_release_decisions d WHERE d.diagnostic_report_id = r.id
                        AND d.decision = 'release' AND d.superseded_at IS NULL) AS released
         FROM diagnostic_reports r JOIN service_requests sr ON sr.id = r.service_request_id
         WHERE r.tenant_id = $1 AND r.patient_id = $2
           AND r.status IN ('final','amended','corrected')
           AND NOT EXISTS (SELECT 1 FROM diagnostic_reports n WHERE n.replaces = r.id)
         ORDER BY r.issued_at DESC, r.id DESC LIMIT $3"
    ))
    .bind(tenant_id)
    .bind(patient_id)
    .bind(limit.clamp(1, MAX_LIMIT))
    .fetch_all(&mut *conn)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let r = report_from_row(row)?;
        let mut v = report_json(&r);
        v["order_display"] = json!(row.get::<String, _>("order_display"));
        v["order_category"] = json!(row.get::<Option<String>, _>("order_category"));
        v["modality_code"] = json!(row.get::<Option<String>, _>("modality_code"));
        v["priority"] = json!(row.get::<String, _>("priority"));
        v["reviewed"] = json!(row.get::<bool, _>("reviewed"));
        v["released"] = json!(row.get::<bool, _>("released"));
        v["components"] = Value::Array(
            components_of(conn, r.id)
                .await?
                .iter()
                .map(component_json)
                .collect(),
        );
        out.push(v);
    }
    Ok(out)
}

/// `GET /api/v1/diagnostics/reports/:id`
pub async fn detail(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let r = load_report(&mut conn, ctx.tenant_id, id, false).await?;
    let o = load_order(&mut conn, ctx.tenant_id, r.service_request_id).await?;
    let allowed = guard_order(&state, &ctx, actions::DIAGNOSTIC_READ, &o).await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    let mut v = report_detail_json(&mut conn, &r).await?;
    let mut items = vec![json!({ "patient_id": o.patient_id })];
    scheduling::attach_patient_summaries(&mut *conn, ctx.tenant_id, &mut items, true).await?;
    v["patient"] = items[0]["patient"].clone();
    v["order"] = order_json(&o);
    Ok(Json(v))
}

// ---------------------------------------------------------------------------
// dMind result synthesis (A1, bound to one exact report)
// ---------------------------------------------------------------------------

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

fn report_display(o: &OrderRow, r: &ReportRow) -> String {
    format!("{} ({} v{})", o.display, r.status.as_str(), r.version)
}

#[derive(Debug, Default, Deserialize)]
pub struct SynthesizeBody {
    pub lang: Option<String>,
}

/// `POST /api/v1/diagnostics/reports/:id/synthesis`
pub async fn synthesize(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    body: Option<Json<SynthesizeBody>>,
) -> Result<Json<Value>, ApiError> {
    let lang = body
        .and_then(|b| b.0.lang)
        .map(|l| if l.starts_with("es") { "es" } else { "en" }.to_string())
        .unwrap_or_else(|| "en".into());
    let mut conn = state.pool.acquire().await?;
    let r = load_report(&mut conn, ctx.tenant_id, id, false).await?;
    let o = load_order(&mut conn, ctx.tenant_id, r.service_request_id).await?;
    let allowed = guard_order(&state, &ctx, actions::DIAGNOSTIC_REVIEW, &o).await?;
    if !r.status.is_reviewable() {
        return Err(ApiError::conflict(
            "report_not_reviewable",
            "only final, amended or corrected reports are synthesized",
        ));
    }
    require_current(&mut conn, &r).await?;

    let components = components_of(&mut conn, r.id).await?;
    let mut synth_components = Vec::with_capacity(components.len());
    for c in &components {
        let prior = prior_component(
            &mut conn,
            r.tenant_id,
            r.patient_id,
            &c.code,
            c.effective_at,
            r.id,
        )
        .await?
        .map(|p| (format!("component:{}", p.id), value_text(&p.value)));
        synth_components.push(SynthesisComponent {
            component_ref: format!("component:{}", c.id),
            display: c.display.clone().unwrap_or_else(|| c.code.clone()),
            value_text: value_text(&c.value),
            interpretation: c.interpretation,
            reference_range: c.reference_range.clone(),
            prior,
        });
    }
    let prior_reports = sqlx::query(
        "SELECT id, status, criticality, issued_at, version FROM diagnostic_reports
         WHERE service_request_id = $1 AND id <> $2 ORDER BY version DESC LIMIT 5",
    )
    .bind(o.id)
    .bind(r.id)
    .fetch_all(&mut *conn)
    .await?;
    let mut facts: Vec<(String, String)> = prior_reports
        .iter()
        .map(|p| {
            (
                format!("report:{}", p.get::<Uuid, _>("id")),
                format!(
                    "earlier report v{} {} criticality {} issued {}",
                    p.get::<i64, _>("version"),
                    p.get::<String, _>("status"),
                    p.get::<String, _>("criticality"),
                    p.get::<DateTime<Utc>, _>("issued_at").to_rfc3339()
                ),
            )
        })
        .collect();
    if let Some(ind) = &o.clinical_indication {
        facts.push((
            format!("order:{}", o.id),
            format!("clinical indication: {ind}"),
        ));
    }
    if let Some(q) = &o.clinical_question {
        facts.push((
            format!("order:{}:question", o.id),
            format!("clinical question: {q}"),
        ));
    }
    drop(conn);
    let req = ResultSynthesisRequest {
        template: RESULT_SYNTHESIS_TEMPLATE.to_string(),
        language: lang,
        report_ref: format!("report:{}", r.id),
        report_display: report_display(&o, &r),
        report_status: r.status.as_str().to_string(),
        criticality: r.criticality,
        conclusion: r.conclusion.clone(),
        components: synth_components,
        facts,
    };
    req.check().map_err(aigov::gateway_error)?;
    let bounds = req.bounds();

    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    audit::emit(
        &mut *tx,
        &ctx,
        "ai.artifact.requested",
        &state.cell,
        json!({ "diagnostic_report_id": r.id, "patient_id": r.patient_id, "template": RESULT_SYNTHESIS_TEMPLATE,
                "components": req.components.len(), "facts": req.facts.len() }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    tx.commit().await?;

    let hash = hash_json(&req);
    let scope = aigov::ReuseScope::DiagnosticResultSynthesis {
        diagnostic_report_id: r.id,
    };
    let plan = aigov::plan(
        &state,
        r.tenant_id,
        r.patient_id,
        scope,
        &hash,
        RESULT_SYNTHESIS_SCHEMA,
    )
    .await?;
    let synthetic = plan.model_synthetic();
    let (output, provider, prompt_version, usage, reused_from, execution_id) = match plan {
        aigov::ExecutionPlan::Reuse(prior) => {
            let output: DiagnosticResultSynthesisV1 = prior.output_as()?;
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
            match state.gateway.synthesize_result(&req).await {
                Ok(resp) => (
                    resp.output,
                    resp.provider,
                    resp.prompt_version,
                    resp.usage,
                    None,
                    Some(execution_id),
                ),
                Err(err) => {
                    record_generation_failure(&state, &ctx, r.patient_id, &err).await?;
                    return Err(aigov::gateway_error(err));
                }
            }
        }
    };
    output
        .validate(&bounds)
        .map_err(|m| ApiError::internal(format!("synthesis validation: {m}")))?;

    let mut tx = state.pool.begin().await?;
    let locked = load_report(&mut tx, ctx.tenant_id, r.id, true).await?;
    require_current(&mut tx, &locked).await?;
    let artifact_id = Uuid::now_v7();
    sqlx::query(
        "UPDATE ai_artifacts SET status = $1
         WHERE tenant_id = $2 AND diagnostic_report_id = $3 AND artifact_type = 'diagnostic_result_synthesis'
           AND status = $4",
    )
    .bind(ArtifactStatus::Superseded.as_str())
    .bind(r.tenant_id)
    .bind(r.id)
    .bind(ArtifactStatus::AwaitingReview.as_str())
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO ai_artifacts
         (id, tenant_id, patient_id, service_request_id, diagnostic_report_id, artifact_type, autonomy_level, status,
          model, model_version, route, template, input_hash, output, output_schema, citations, limitations, generated_at)
         VALUES ($1,$2,$3,$4,$5,'diagnostic_result_synthesis','A1',$6,$7,$8,$9,$10,$11,$12,$13,$14,$15, now())",
    )
    .bind(artifact_id)
    .bind(r.tenant_id)
    .bind(r.patient_id)
    .bind(o.id)
    .bind(r.id)
    .bind(ArtifactStatus::AwaitingReview.as_str())
    .bind(&provider.model)
    .bind(&provider.model_version)
    .bind(&provider.provider)
    .bind(RESULT_SYNTHESIS_TEMPLATE)
    .bind(&hash)
    .bind(serde_json::to_value(&output).map_err(ApiError::internal)?)
    .bind(RESULT_SYNTHESIS_SCHEMA)
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
            input_refs: &bounds.fact_refs,
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
        json!({ "artifact_id": artifact_id, "diagnostic_report_id": r.id, "template": RESULT_SYNTHESIS_TEMPLATE,
                "reused_from": reused_from, "synthetic": synthetic }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    tx.commit().await?;

    let mut conn = state.pool.acquire().await?;
    let row = load_artifact(
        &mut conn,
        r.tenant_id,
        r.id,
        artifact_id,
        "diagnostic_result_synthesis",
        false,
    )
    .await?;
    let mut v = artifact_row_json(&row);
    v["report_version"] = json!(r.version);
    v["reused_from"] = json!(reused_from);
    Ok(Json(v))
}

// ---------------------------------------------------------------------------
// Professional review
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct FollowUpInput {
    pub description: String,
    pub priority: Option<String>,
    pub due_in_hours: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct ReviewBody {
    pub report_version: i64,
    pub clinical_assessment: String,
    pub disposition: String,
    pub disposition_note: Option<String>,
    #[serde(default)]
    pub follow_ups: Vec<FollowUpInput>,
    pub synthesis_artifact_id: Option<Uuid>,
    /// `approved` or `rejected` for the synthesis the reviewer looked at.
    pub synthesis_decision: Option<String>,
    pub synthesis_note: Option<String>,
}

fn parse_review_decision(s: Option<&str>) -> Result<Option<ArtifactStatus>, ApiError> {
    Ok(match s.map(str::trim) {
        None | Some("") => None,
        Some("approved") => Some(ArtifactStatus::Approved),
        Some("rejected") => Some(ArtifactStatus::Rejected),
        Some(_) => {
            return Err(ApiError::bad_request(
                "validation_failed",
                "decision must be approved or rejected",
            ))
        }
    })
}

#[allow(clippy::too_many_arguments)]
async fn review_artifact_in(
    tx: &mut PgConnection,
    ctx: &AuthContext,
    state: &AppState,
    r: &ReportRow,
    artifact_id: Uuid,
    artifact_type: &str,
    decision: ArtifactStatus,
    note: Option<&str>,
) -> Result<(), ApiError> {
    let row = load_artifact(tx, r.tenant_id, r.id, artifact_id, artifact_type, true).await?;
    let status: String = row.get("status");
    if status != ArtifactStatus::AwaitingReview.as_str() {
        return Err(ApiError::conflict(
            "artifact_not_reviewable",
            format!("the dMind draft is {status}; only awaiting_review drafts can be reviewed"),
        ));
    }
    sqlx::query(
        "UPDATE ai_artifacts SET status = $2, reviewer_id = $3, review_decision = $4, review_note = $5, reviewed_at = now()
         WHERE id = $1",
    )
    .bind(artifact_id)
    .bind(decision.as_str())
    .bind(ctx.user_id)
    .bind(decision.as_str())
    .bind(note)
    .execute(&mut *tx)
    .await?;
    audit::emit(
        &mut *tx,
        ctx,
        "ai.artifact.reviewed",
        &state.cell,
        json!({ "artifact_id": artifact_id, "diagnostic_report_id": r.id, "decision": decision.as_str() }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    Ok(())
}

/// `POST /api/v1/diagnostics/reports/:id/review`
pub async fn review(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<ReviewBody>,
) -> Result<Json<Value>, ApiError> {
    let assessment = check_len(
        "clinical_assessment",
        Some(&body.clinical_assessment),
        MAX_TEXT,
    )?
    .filter(|a| a.chars().count() >= MIN_ASSESSMENT_CHARS)
    .ok_or_else(|| ApiError::bad_request("validation_failed", "clinical_assessment is required"))?;
    let disposition = body.disposition.trim();
    if !DISPOSITIONS.contains(&disposition) {
        return Err(ApiError::bad_request(
            "validation_failed",
            "disposition is not one of the supported values",
        ));
    }
    let disposition_note = check_len(
        "disposition_note",
        body.disposition_note.as_deref(),
        MAX_TEXT,
    )?;
    if disposition == "other" && disposition_note.is_none() {
        return Err(ApiError::bad_request(
            "validation_failed",
            "disposition_note is required when disposition is other",
        ));
    }
    if body.follow_ups.len() > MAX_FOLLOW_UPS {
        return Err(ApiError::bad_request(
            "validation_failed",
            format!("at most {MAX_FOLLOW_UPS} follow-up tasks"),
        ));
    }
    for f in &body.follow_ups {
        if f.description.trim().is_empty() || f.description.chars().count() > MAX_SHORT {
            return Err(ApiError::bad_request(
                "validation_failed",
                "follow-up descriptions are required (at most 400 characters)",
            ));
        }
        if let Some(p) = f.priority.as_deref() {
            if !["routine", "high", "urgent"].contains(&p) {
                return Err(ApiError::bad_request(
                    "validation_failed",
                    "follow-up priority must be routine, high or urgent",
                ));
            }
        }
        if f.due_in_hours.is_some_and(|h| !(1..=24 * 365).contains(&h)) {
            return Err(ApiError::bad_request(
                "validation_failed",
                "due_in_hours must be between 1 and 8760",
            ));
        }
    }
    let synthesis_decision = parse_review_decision(body.synthesis_decision.as_deref())?;
    if synthesis_decision.is_some() && body.synthesis_artifact_id.is_none() {
        return Err(ApiError::bad_request(
            "validation_failed",
            "synthesis_artifact_id is required with synthesis_decision",
        ));
    }
    let synthesis_note = check_len("synthesis_note", body.synthesis_note.as_deref(), MAX_TEXT)?;

    let mut conn = state.pool.acquire().await?;
    let r = load_report(&mut conn, ctx.tenant_id, id, false).await?;
    let o = load_order(&mut conn, ctx.tenant_id, r.service_request_id).await?;
    drop(conn);
    let allowed = guard_order(&state, &ctx, actions::DIAGNOSTIC_REVIEW, &o).await?;

    let mut tx = state.pool.begin().await?;
    let o = lock_order(&mut tx, ctx.tenant_id, o.id).await?;
    let r = load_report(&mut tx, ctx.tenant_id, id, true).await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    if !r.status.is_reviewable() {
        return Err(ApiError::conflict(
            "report_not_reviewable",
            "only final, amended or corrected reports are reviewed",
        ));
    }
    if r.version != body.report_version {
        return Err(ApiError::conflict(
            "report_version_mismatch",
            "the report version you reviewed is not the stored version; reload the report",
        ));
    }
    require_current(&mut tx, &r).await?;
    if let Some(aid) = body.synthesis_artifact_id {
        load_artifact(
            &mut tx,
            r.tenant_id,
            r.id,
            aid,
            "diagnostic_result_synthesis",
            true,
        )
        .await?;
        if let Some(decision) = synthesis_decision {
            review_artifact_in(
                &mut tx,
                &ctx,
                &state,
                &r,
                aid,
                "diagnostic_result_synthesis",
                decision,
                synthesis_note.as_deref(),
            )
            .await?;
        }
    }
    let mut task_ids = Vec::with_capacity(body.follow_ups.len());
    for f in &body.follow_ups {
        let task_id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO follow_up_tasks (id, tenant_id, patient_id, service_request_id, description, priority, due_at)
             VALUES ($1,$2,$3,$4,$5,$6, now() + make_interval(hours => $7))",
        )
        .bind(task_id)
        .bind(o.tenant_id)
        .bind(o.patient_id)
        .bind(o.id)
        .bind(f.description.trim())
        .bind(f.priority.as_deref().unwrap_or("routine"))
        .bind(f.due_in_hours.unwrap_or(72) as i32)
        .execute(&mut *tx)
        .await?;
        task_ids.push(task_id);
    }
    let review_id = Uuid::now_v7();
    let inserted = sqlx::query(
        "INSERT INTO diagnostic_reviews
         (id, tenant_id, patient_id, service_request_id, diagnostic_report_id, report_version, reviewer_id,
          clinical_assessment, disposition, disposition_note, follow_up_task_ids, synthesis_artifact_id)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)",
    )
    .bind(review_id)
    .bind(o.tenant_id)
    .bind(o.patient_id)
    .bind(o.id)
    .bind(r.id)
    .bind(r.version)
    .bind(ctx.user_id)
    .bind(&assessment)
    .bind(disposition)
    .bind(&disposition_note)
    .bind(&task_ids)
    .bind(body.synthesis_artifact_id)
    .execute(&mut *tx)
    .await;
    if let Err(e) = inserted {
        if matches!(&e, sqlx::Error::Database(db) if db.is_unique_violation()) {
            return Err(ApiError::conflict(
                "already_reviewed",
                "you already reviewed this report version",
            ));
        }
        return Err(e.into());
    }
    // Legacy result loop: the first professional review moves it on.
    if let Some(LoopState::Received) = LoopState::parse(&o.loop_state) {
        sqlx::query(
            "UPDATE service_requests SET loop_state = $1, version = version + 1, updated_at = now()
             WHERE id = $2 AND version = $3",
        )
        .bind(LoopState::Reviewed.as_str())
        .bind(o.id)
        .bind(o.version)
        .execute(&mut *tx)
        .await?;
    }
    audit::emit(
        &mut *tx,
        &ctx,
        "diagnostic_report.reviewed",
        &state.cell,
        json!({ "review_id": review_id, "diagnostic_report_id": r.id, "report_version": r.version,
                "service_request_id": o.id, "disposition": disposition, "follow_up_tasks": task_ids.len(),
                "criticality": r.criticality.as_str() }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    super::super::risk::recalculate_after_change(
        &mut tx,
        &ctx,
        &state,
        o.tenant_id,
        o.patient_id,
        "result.reviewed",
    )
    .await?;
    tx.commit().await?;
    let mut conn = state.pool.acquire().await?;
    let mut v = report_detail_json(&mut conn, &r).await?;
    v["review_id"] = json!(review_id);
    Ok(Json(v))
}

// ---------------------------------------------------------------------------
// Patient explanation (A1 draft, EN/ES) and its review
// ---------------------------------------------------------------------------

async fn latest_review(
    conn: &mut PgConnection,
    report_id: Uuid,
) -> Result<Option<(Uuid, String, String, Option<String>, i64)>, ApiError> {
    let row = sqlx::query(
        "SELECT id, clinical_assessment, disposition, disposition_note, report_version FROM diagnostic_reviews
         WHERE diagnostic_report_id = $1 ORDER BY reviewed_at DESC LIMIT 1",
    )
    .bind(report_id)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.map(|r| {
        (
            r.get("id"),
            r.get("clinical_assessment"),
            r.get("disposition"),
            r.get("disposition_note"),
            r.get("report_version"),
        )
    }))
}

/// `POST /api/v1/diagnostics/reports/:id/explanation`
pub async fn explain(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let r = load_report(&mut conn, ctx.tenant_id, id, false).await?;
    let o = load_order(&mut conn, ctx.tenant_id, r.service_request_id).await?;
    let allowed = guard_order(&state, &ctx, actions::DIAGNOSTIC_RELEASE, &o).await?;
    if !r.status.is_releasable() {
        return Err(ApiError::conflict(
            "report_not_releasable",
            "only final, amended or corrected reports can be explained to the patient",
        ));
    }
    require_current(&mut conn, &r).await?;
    let (review_id, assessment, disposition, disposition_note, review_version) =
        latest_review(&mut conn, r.id).await?.ok_or_else(|| {
            ApiError::conflict(
            "review_required",
            "a professional review of this report version is required before a patient explanation",
        )
        })?;
    if review_version != r.version {
        return Err(ApiError::conflict(
            "review_required",
            "the professional review refers to another report version",
        ));
    }
    let components = components_of(&mut conn, r.id).await?;
    drop(conn);
    let mut facts = vec![(
        format!("report:{}", r.id),
        format!(
            "{} — {}{}",
            report_display(&o, &r),
            r.criticality.as_str(),
            r.conclusion
                .as_deref()
                .map(|c| format!("; conclusion: {c}"))
                .unwrap_or_default()
        ),
    )];
    for c in &components {
        facts.push((
            format!("component:{}", c.id),
            format!(
                "{}: {} ({}){}",
                c.display.clone().unwrap_or_else(|| c.code.clone()),
                value_text(&c.value),
                c.interpretation.as_str(),
                c.reference_range
                    .as_deref()
                    .map(|rr| format!(", reference {rr}"))
                    .unwrap_or_default()
            ),
        ));
    }
    let review_summary = format!(
        "{assessment} Disposition: {disposition}{}",
        disposition_note
            .as_deref()
            .map(|n| format!(" ({n})"))
            .unwrap_or_default()
    );
    facts.push((format!("review:{review_id}"), review_summary.clone()));
    let req = PatientExplanationRequest {
        template: PATIENT_EXPLANATION_TEMPLATE.to_string(),
        report_ref: format!("report:{}", r.id),
        report_version: r.version,
        report_display: report_display(&o, &r),
        criticality: r.criticality,
        review_summary,
        facts,
    };
    req.check().map_err(aigov::gateway_error)?;
    let bounds = req.bounds();

    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    audit::emit(
        &mut *tx,
        &ctx,
        "ai.artifact.requested",
        &state.cell,
        json!({ "diagnostic_report_id": r.id, "patient_id": r.patient_id, "template": PATIENT_EXPLANATION_TEMPLATE,
                "facts": req.facts.len() }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    tx.commit().await?;

    let hash = hash_json(&req);
    let scope = aigov::ReuseScope::PatientResultExplanation {
        diagnostic_report_id: r.id,
    };
    let plan = aigov::plan(
        &state,
        r.tenant_id,
        r.patient_id,
        scope,
        &hash,
        PATIENT_EXPLANATION_SCHEMA,
    )
    .await?;
    let synthetic = plan.model_synthetic();
    let (output, provider, prompt_version, usage, reused_from, execution_id) = match plan {
        aigov::ExecutionPlan::Reuse(prior) => {
            let output: PatientResultExplanationV1 = prior.output_as()?;
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
            match state.gateway.explain_result_for_patient(&req).await {
                Ok(resp) => (
                    resp.output,
                    resp.provider,
                    resp.prompt_version,
                    resp.usage,
                    None,
                    Some(execution_id),
                ),
                Err(err) => {
                    record_generation_failure(&state, &ctx, r.patient_id, &err).await?;
                    return Err(aigov::gateway_error(err));
                }
            }
        }
    };
    output
        .validate(&bounds)
        .map_err(|m| ApiError::internal(format!("explanation validation: {m}")))?;

    let mut tx = state.pool.begin().await?;
    let locked = load_report(&mut tx, ctx.tenant_id, r.id, true).await?;
    require_current(&mut tx, &locked).await?;
    let artifact_id = Uuid::now_v7();
    sqlx::query(
        "UPDATE ai_artifacts SET status = $1
         WHERE tenant_id = $2 AND diagnostic_report_id = $3 AND artifact_type = 'patient_result_explanation'
           AND status = $4",
    )
    .bind(ArtifactStatus::Superseded.as_str())
    .bind(r.tenant_id)
    .bind(r.id)
    .bind(ArtifactStatus::AwaitingReview.as_str())
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO ai_artifacts
         (id, tenant_id, patient_id, service_request_id, diagnostic_report_id, artifact_type, autonomy_level, status,
          model, model_version, route, template, input_hash, output, output_schema, citations, limitations, generated_at)
         VALUES ($1,$2,$3,$4,$5,'patient_result_explanation','A1',$6,$7,$8,$9,$10,$11,$12,$13,$14,$15, now())",
    )
    .bind(artifact_id)
    .bind(r.tenant_id)
    .bind(r.patient_id)
    .bind(o.id)
    .bind(r.id)
    .bind(ArtifactStatus::AwaitingReview.as_str())
    .bind(&provider.model)
    .bind(&provider.model_version)
    .bind(&provider.provider)
    .bind(PATIENT_EXPLANATION_TEMPLATE)
    .bind(&hash)
    .bind(serde_json::to_value(&output).map_err(ApiError::internal)?)
    .bind(PATIENT_EXPLANATION_SCHEMA)
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
            input_refs: &bounds.fact_refs,
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
        json!({ "artifact_id": artifact_id, "diagnostic_report_id": r.id, "template": PATIENT_EXPLANATION_TEMPLATE,
                "reused_from": reused_from, "synthetic": synthetic }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    tx.commit().await?;

    let mut conn = state.pool.acquire().await?;
    let row = load_artifact(
        &mut conn,
        r.tenant_id,
        r.id,
        artifact_id,
        "patient_result_explanation",
        false,
    )
    .await?;
    let mut v = artifact_row_json(&row);
    v["report_version"] = json!(r.version);
    v["reused_from"] = json!(reused_from);
    Ok(Json(v))
}

#[derive(Debug, Deserialize)]
pub struct ExplanationReviewBody {
    pub decision: String,
    pub note: Option<String>,
}

/// `POST /api/v1/diagnostics/reports/:id/explanation/:artifact_id/review`
pub async fn review_explanation(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path((id, artifact_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<ExplanationReviewBody>,
) -> Result<Json<Value>, ApiError> {
    let decision = parse_review_decision(Some(&body.decision))?.ok_or_else(|| {
        ApiError::bad_request("validation_failed", "decision must be approved or rejected")
    })?;
    let note = check_len("note", body.note.as_deref(), MAX_TEXT)?;
    let mut conn = state.pool.acquire().await?;
    let r = load_report(&mut conn, ctx.tenant_id, id, false).await?;
    let o = load_order(&mut conn, ctx.tenant_id, r.service_request_id).await?;
    drop(conn);
    let allowed = guard_order(&state, &ctx, actions::DIAGNOSTIC_RELEASE, &o).await?;
    let mut tx = state.pool.begin().await?;
    let r = load_report(&mut tx, ctx.tenant_id, id, true).await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    require_current(&mut tx, &r).await?;
    review_artifact_in(
        &mut tx,
        &ctx,
        &state,
        &r,
        artifact_id,
        "patient_result_explanation",
        decision,
        note.as_deref(),
    )
    .await?;
    tx.commit().await?;
    let mut conn = state.pool.acquire().await?;
    let row = load_artifact(
        &mut conn,
        r.tenant_id,
        r.id,
        artifact_id,
        "patient_result_explanation",
        false,
    )
    .await?;
    Ok(Json(artifact_row_json(&row)))
}

// ---------------------------------------------------------------------------
// Release decision
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ReleaseBody {
    pub report_version: i64,
    pub review_id: Uuid,
    pub decision: String,
    pub withhold_reason: Option<String>,
    pub explanation_en: Option<String>,
    pub explanation_es: Option<String>,
    pub explanation_artifact_id: Option<Uuid>,
    #[serde(default)]
    pub notify_patient: bool,
    /// Release the report's clean documents together with the result.
    pub release_documents: Option<bool>,
}

/// `POST /api/v1/diagnostics/reports/:id/release`
pub async fn release(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<ReleaseBody>,
) -> Result<Json<Value>, ApiError> {
    let decision = match body.decision.trim() {
        "release" => "release",
        "withhold" => "withhold",
        _ => {
            return Err(ApiError::bad_request(
                "validation_failed",
                "decision must be release or withhold",
            ))
        }
    };
    let withhold_reason = check_len("withhold_reason", body.withhold_reason.as_deref(), MAX_TEXT)?;
    if decision == "withhold"
        && withhold_reason.as_deref().map_or(0, |r| r.chars().count()) < MIN_REASON_CHARS
    {
        return Err(ApiError::bad_request(
            "validation_failed",
            "withhold_reason is required when withholding",
        ));
    }
    let explanation_en = check_len(
        "explanation_en",
        body.explanation_en.as_deref(),
        MAX_NARRATIVE_CHARS,
    )?;
    let explanation_es = check_len(
        "explanation_es",
        body.explanation_es.as_deref(),
        MAX_NARRATIVE_CHARS,
    )?;
    if body.notify_patient && decision != "release" {
        return Err(ApiError::bad_request(
            "validation_failed",
            "the patient is notified only when results are released",
        ));
    }

    let mut conn = state.pool.acquire().await?;
    let r = load_report(&mut conn, ctx.tenant_id, id, false).await?;
    let o = load_order(&mut conn, ctx.tenant_id, r.service_request_id).await?;
    drop(conn);
    let allowed = guard_order(&state, &ctx, actions::DIAGNOSTIC_RELEASE, &o).await?;

    let mut tx = state.pool.begin().await?;
    let o = lock_order(&mut tx, ctx.tenant_id, o.id).await?;
    let r = load_report(&mut tx, ctx.tenant_id, id, true).await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    if !r.status.is_releasable() {
        return Err(ApiError::conflict(
            "report_not_releasable",
            "only final, amended or corrected reports can be released",
        ));
    }
    if r.version != body.report_version {
        return Err(ApiError::conflict(
            "report_version_mismatch",
            "the report version you are releasing is not the stored version; reload the report",
        ));
    }
    require_current(&mut tx, &r).await?;
    let review_ok: Option<i64> = sqlx::query_scalar(
        "SELECT report_version FROM diagnostic_reviews WHERE id = $1 AND diagnostic_report_id = $2",
    )
    .bind(body.review_id)
    .bind(r.id)
    .fetch_optional(&mut *tx)
    .await?;
    match review_ok {
        Some(v) if v == r.version => {}
        _ => {
            return Err(ApiError::conflict(
                "review_required",
                "a professional review of this exact report version is required before release",
            ))
        }
    }
    if let Some(aid) = body.explanation_artifact_id {
        let row = load_artifact(
            &mut tx,
            r.tenant_id,
            r.id,
            aid,
            "patient_result_explanation",
            true,
        )
        .await?;
        let status: String = row.get("status");
        if status != ArtifactStatus::Approved.as_str() {
            return Err(ApiError::conflict(
                "explanation_not_approved",
                "the dMind explanation must be approved by a clinician before it is released",
            ));
        }
        if decision == "release" && explanation_en.is_none() && explanation_es.is_none() {
            return Err(ApiError::bad_request(
                "validation_failed",
                "provide the explanation text the patient will read (edited or approved as drafted)",
            ));
        }
    }
    sqlx::query(
        "UPDATE result_release_decisions SET superseded_at = now()
         WHERE diagnostic_report_id = $1 AND superseded_at IS NULL",
    )
    .bind(r.id)
    .execute(&mut *tx)
    .await?;
    let decision_id = Uuid::now_v7();
    let mut notification_id = None;
    if body.notify_patient {
        let policy = scheduling::load_policy(&mut tx, o.tenant_id).await?;
        let recipient = notify::recipient_for_patient(
            &mut tx,
            o.tenant_id,
            o.patient_id,
            &policy,
            &policy.time_zone,
        )
        .await?;
        notification_id = notify::enqueue(
            &mut tx,
            &ctx,
            &state.cell,
            notify::Enqueue {
                tenant_id: o.tenant_id,
                patient_id: Some(o.patient_id),
                user_id: None,
                kind: "diagnostic_result_released",
                appointment_id: None,
                offer_id: None,
                transport_request_id: None,
                payload: json!({ "diagnostic_report_id": r.id, "service_request_id": o.id,
                                 "release_decision_id": decision_id }),
                dedupe_key: format!("diagnostic_result_released:{}", decision_id),
                scheduled_for: Utc::now(),
                recipient: &recipient,
            },
        )
        .await?;
        if let Some(nid) = notification_id {
            sqlx::query(
                "UPDATE notifications SET service_request_id = $2, diagnostic_report_id = $3 WHERE id = $1",
            )
            .bind(nid)
            .bind(o.id)
            .bind(r.id)
            .execute(&mut *tx)
            .await?;
        }
    }
    sqlx::query(
        "INSERT INTO result_release_decisions
         (id, tenant_id, patient_id, service_request_id, diagnostic_report_id, report_version, review_id, decision,
          withhold_reason, explanation_en, explanation_es, explanation_artifact_id, notify_patient, notification_id,
          decided_by)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15)",
    )
    .bind(decision_id)
    .bind(o.tenant_id)
    .bind(o.patient_id)
    .bind(o.id)
    .bind(r.id)
    .bind(r.version)
    .bind(body.review_id)
    .bind(decision)
    .bind(&withhold_reason)
    .bind(&explanation_en)
    .bind(&explanation_es)
    .bind(body.explanation_artifact_id)
    .bind(body.notify_patient)
    .bind(notification_id)
    .bind(ctx.user_id)
    .execute(&mut *tx)
    .await?;
    let mut released_documents = 0u64;
    if decision == "release" && body.release_documents.unwrap_or(true) {
        released_documents = sqlx::query(
            "UPDATE clinical_documents SET released = true
             WHERE diagnostic_report_id = $1 AND status = 'clean' AND released = false",
        )
        .bind(r.id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    }
    if decision == "release" && body.notify_patient {
        if let Some(LoopState::Reviewed) = LoopState::parse(&o.loop_state) {
            sqlx::query(
                "UPDATE service_requests SET loop_state = $1, version = version + 1, updated_at = now()
                 WHERE id = $2 AND version = $3",
            )
            .bind(LoopState::Notified.as_str())
            .bind(o.id)
            .bind(o.version)
            .execute(&mut *tx)
            .await?;
        }
    }
    audit::emit(
        &mut *tx,
        &ctx,
        if decision == "release" {
            "diagnostic_result.released"
        } else {
            "diagnostic_result.withheld"
        },
        &state.cell,
        json!({ "release_decision_id": decision_id, "diagnostic_report_id": r.id, "report_version": r.version,
                "service_request_id": o.id, "review_id": body.review_id, "notify_patient": body.notify_patient,
                "notification_id": notification_id, "explanation_artifact_id": body.explanation_artifact_id,
                "released_documents": released_documents, "criticality": r.criticality.as_str() }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    tx.commit().await?;
    let mut conn = state.pool.acquire().await?;
    let mut v = report_detail_json(&mut conn, &r).await?;
    v["release_decision_id"] = json!(decision_id);
    v["notification_id"] = json!(notification_id);
    Ok(Json(v))
}

// ---------------------------------------------------------------------------
// Review worklist
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct ReviewWorklistQuery {
    /// `pending` (default), `reviewed`, `released` or `all`.
    pub state: Option<String>,
    pub criticality: Option<String>,
    pub facility_id: Option<Uuid>,
    pub patient_id: Option<Uuid>,
    pub category: Option<String>,
    /// Only reports of orders the caller requested.
    pub mine: Option<bool>,
    pub limit: Option<i64>,
}

/// `GET /api/v1/diagnostics/reviews` — current reviewable reports, critical
/// first, then order priority, then oldest issued.
pub async fn review_worklist(
    State(state): State<AppState>,
    ctx: AuthContext,
    Query(q): Query<ReviewWorklistQuery>,
) -> Result<Json<Value>, ApiError> {
    guard(
        &state,
        &ctx,
        actions::DIAGNOSTIC_READ,
        "diagnostic_review_worklist",
        None,
    )
    .await?
    .record_on_pool(&state, &ctx)
    .await?;
    let scope = facility_scope(&ctx, actions::DIAGNOSTIC_READ);
    if matches!(&scope, Some(ids) if ids.is_empty()) {
        return Ok(Json(json!({ "items": [] })));
    }
    let state_filter = match q.state.as_deref().unwrap_or("pending") {
        "pending" => "pending",
        "reviewed" => "reviewed",
        "released" => "released",
        "all" => "all",
        _ => {
            return Err(ApiError::bad_request(
                "validation_failed",
                "state must be pending, reviewed, released or all",
            ))
        }
    };
    if let Some(c) = q.criticality.as_deref().filter(|s| !s.is_empty()) {
        if Interpretation::parse(c).is_none() {
            return Err(ApiError::bad_request(
                "validation_failed",
                "unknown criticality filter",
            ));
        }
    }
    let limit = q.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let rows = sqlx::query(&format!(
        "SELECT {REPORT_COLUMNS}, sr.display AS order_display, sr.priority, sr.category_code AS order_category,
                sr.modality_code, sr.requester_id, sr.performing_facility_id AS order_facility_id, p.facility_id,
                EXISTS (SELECT 1 FROM diagnostic_reviews v WHERE v.diagnostic_report_id = r.id) AS reviewed,
                EXISTS (SELECT 1 FROM result_release_decisions d WHERE d.diagnostic_report_id = r.id
                        AND d.decision = 'release' AND d.superseded_at IS NULL) AS released
         FROM diagnostic_reports r
         JOIN service_requests sr ON sr.id = r.service_request_id
         JOIN patients p ON p.id = r.patient_id
         WHERE r.tenant_id = $1
           AND r.status IN ('final','amended','corrected')
           AND NOT EXISTS (SELECT 1 FROM diagnostic_reports n WHERE n.replaces = r.id)
           AND ($2::uuid[] IS NULL OR COALESCE(sr.performing_facility_id, p.facility_id) = ANY($2)
                OR p.facility_id = ANY($2))
           AND ($3::text IS NULL OR r.criticality = $3)
           AND ($4::uuid IS NULL OR COALESCE(sr.performing_facility_id, p.facility_id) = $4)
           AND ($5::uuid IS NULL OR r.patient_id = $5)
           AND ($6::text IS NULL OR sr.category_code = $6)
           AND (NOT $7::boolean OR sr.requester_id = $8)
           AND CASE $9
                 WHEN 'pending' THEN NOT EXISTS (SELECT 1 FROM diagnostic_reviews v WHERE v.diagnostic_report_id = r.id)
                 WHEN 'reviewed' THEN EXISTS (SELECT 1 FROM diagnostic_reviews v WHERE v.diagnostic_report_id = r.id)
                                      AND NOT EXISTS (SELECT 1 FROM result_release_decisions d
                                                      WHERE d.diagnostic_report_id = r.id AND d.superseded_at IS NULL)
                 WHEN 'released' THEN EXISTS (SELECT 1 FROM result_release_decisions d
                                              WHERE d.diagnostic_report_id = r.id AND d.decision = 'release'
                                                AND d.superseded_at IS NULL)
                 ELSE true END
         ORDER BY CASE r.criticality WHEN 'critical' THEN 0 WHEN 'abnormal' THEN 1 WHEN 'unknown' THEN 2 ELSE 3 END,
                  CASE sr.priority WHEN 'stat' THEN 0 WHEN 'urgent' THEN 1 WHEN 'timed' THEN 2 ELSE 3 END,
                  r.issued_at, r.id
         LIMIT $10"
    ))
    .bind(ctx.tenant_id)
    .bind(scope.as_deref())
    .bind(q.criticality.as_deref().filter(|s| !s.is_empty()))
    .bind(q.facility_id)
    .bind(q.patient_id)
    .bind(q.category.as_deref().filter(|s| !s.is_empty()))
    .bind(q.mine.unwrap_or(false))
    .bind(ctx.user_id)
    .bind(state_filter)
    .bind(limit)
    .fetch_all(&state.pool)
    .await?;
    let mut items = Vec::with_capacity(rows.len());
    for row in &rows {
        let r = report_from_row(row)?;
        let mut v = report_json(&r);
        v["order_display"] = json!(row.get::<String, _>("order_display"));
        v["priority"] = json!(row.get::<String, _>("priority"));
        v["order_category"] = json!(row.get::<Option<String>, _>("order_category"));
        v["modality_code"] = json!(row.get::<Option<String>, _>("modality_code"));
        v["requester_id"] = json!(row.get::<Uuid, _>("requester_id"));
        v["facility_id"] = json!(row
            .get::<Option<Uuid>, _>("order_facility_id")
            .unwrap_or_else(|| row.get::<Uuid, _>("facility_id")));
        v["reviewed"] = json!(row.get::<bool, _>("reviewed"));
        v["released"] = json!(row.get::<bool, _>("released"));
        items.push(v);
    }
    let mut conn = state.pool.acquire().await?;
    scheduling::attach_patient_summaries(&mut *conn, ctx.tenant_id, &mut items, true).await?;
    Ok(Json(json!({ "items": items })))
}
