//! Bounded FHIR R4 facade (ADR-0005).
//!
//! Internal resources are mapped to FHIR R4 JSON at the edge for the subset
//! WellOS exchanges: `Patient`, `ServiceRequest`, `Specimen`, `Observation`,
//! `DiagnosticReport`, `DocumentReference`, `ImagingStudy`. Every emitted
//! resource satisfies `wellos_domain::fhir::validate`; inbound
//! `DiagnosticReport` deliveries are validated the same way before they are
//! issued through the generalized typed result path. This is a facade, not a
//! FHIR server: no search, no Bundles, no conformance claim.

use crate::auth::AuthContext;
use crate::error::ApiError;
use crate::policy::{actions, ResourceCtx};
use crate::routes::diagnostics::documents::{load_document, DocumentRow};
use crate::routes::diagnostics::reports::{
    components_of, issue_in, load_component, load_report, ComponentInput, ComponentRow,
    ConclusionCode, IssueInput, ReportRow,
};
use crate::routes::diagnostics::specimens::SpecimenRow;
use crate::routes::diagnostics::{load_order, lock_order, OrderRow};
use crate::routes::guard;
use crate::state::AppState;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde_json::{json, Map, Value};
use sqlx::{PgConnection, Row};
use std::str::FromStr;
use uuid::Uuid;
use wellos_domain::diagnostics::{
    Interpretation, OrderPriority, OrderStatus, ReportStatus, ResultValue, SpecimenStatus,
};
use wellos_domain::fhir as profile;

const LOINC: &str = "http://loinc.org";
const UCUM: &str = "http://unitsofmeasure.org";
const DCM: &str = "http://dicom.nema.org/resources/ontology/DCM";
const INTERPRETATION_SYSTEM: &str =
    "http://terminology.hl7.org/CodeSystem/v3-ObservationInterpretation";
const IDEMPOTENCY_SYSTEM: &str = "urn:wellos:idempotency";
const CHANGE_REASON_EXTENSION: &str = "urn:wellos:change-reason";
const NARRATIVE_CATEGORY: &str = "urn:wellos:result-type";

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Tenant of a row looked up by id only, so that cross-tenant reads are
/// refused by policy (403) exactly like the rest of the facade rather than
/// leaking existence through 404/403 differences per resource.
async fn locate(conn: &mut PgConnection, table: &str, id: Uuid) -> Result<Uuid, ApiError> {
    let row = sqlx::query(&format!("SELECT tenant_id FROM {table} WHERE id = $1"))
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(ApiError::not_found)?;
    Ok(row.get("tenant_id"))
}

async fn patient_facility(conn: &mut PgConnection, patient_id: Uuid) -> Result<Uuid, ApiError> {
    let row = sqlx::query("SELECT facility_id FROM patients WHERE id = $1")
        .bind(patient_id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(ApiError::not_found)?;
    Ok(row.get("facility_id"))
}

async fn read_guard(
    state: &AppState,
    ctx: &AuthContext,
    action: &str,
    resource: &str,
    tenant_id: Uuid,
    patient_id: Uuid,
    facility_id: Uuid,
) -> Result<(), ApiError> {
    guard(
        state,
        ctx,
        action,
        resource,
        Some(ResourceCtx {
            tenant_id,
            patient_id: Some(patient_id),
            facility_id: Some(facility_id),
        }),
    )
    .await?
    .record_on_pool(state, ctx)
    .await
}

fn reference(ty: &str, id: impl std::fmt::Display) -> Value {
    json!({ "reference": format!("{ty}/{id}") })
}

fn coding(system: &str, code: &str, display: Option<&str>) -> Value {
    let mut c = json!({ "system": system, "code": code });
    if let Some(d) = display {
        c["display"] = json!(d);
    }
    c
}

fn rfc3339(t: DateTime<Utc>) -> Value {
    json!(t.to_rfc3339_opts(chrono::SecondsFormat::Micros, true))
}

fn decimal_number(d: Decimal) -> Result<Value, ApiError> {
    // Exact decimal: never round clinical values through f64.
    d.normalize()
        .to_string()
        .parse::<serde_json::Number>()
        .map(Value::Number)
        .map_err(|_| ApiError::internal("invalid numeric observation value"))
}

/// The emitted resource must satisfy the boundary profile; a drift here is a
/// server defect, never something the caller can fix.
fn checked(resource: Value) -> Result<Json<Value>, ApiError> {
    if let Err(issues) = profile::validate(&resource, true) {
        tracing::error!(?issues, "FHIR facade emitted a non-conformant resource");
        return Err(ApiError::internal(
            "FHIR mapping produced an invalid resource",
        ));
    }
    Ok(Json(resource))
}

fn order_fhir_status(s: OrderStatus) -> &'static str {
    match s {
        OrderStatus::Placed
        | OrderStatus::Accepted
        | OrderStatus::Scheduled
        | OrderStatus::InProgress => "active",
        OrderStatus::Completed => "completed",
        OrderStatus::OnHold => "on-hold",
        OrderStatus::Cancelled | OrderStatus::Rejected => "revoked",
        OrderStatus::EnteredInError => "entered-in-error",
    }
}

fn priority_fhir(p: OrderPriority) -> &'static str {
    match p {
        OrderPriority::Routine | OrderPriority::Timed => "routine",
        OrderPriority::Urgent => "urgent",
        OrderPriority::Stat => "stat",
    }
}

fn report_fhir_status(s: ReportStatus) -> &'static str {
    match s {
        ReportStatus::Preliminary => "preliminary",
        ReportStatus::Final => "final",
        ReportStatus::Amended => "amended",
        ReportStatus::Corrected => "corrected",
        ReportStatus::Cancelled => "cancelled",
        ReportStatus::EnteredInError => "entered-in-error",
    }
}

fn report_status_from_fhir(s: &str) -> Result<ReportStatus, ApiError> {
    Ok(match s {
        "preliminary" => ReportStatus::Preliminary,
        "final" => ReportStatus::Final,
        "amended" => ReportStatus::Amended,
        "corrected" => ReportStatus::Corrected,
        "cancelled" => ReportStatus::Cancelled,
        "entered-in-error" => ReportStatus::EnteredInError,
        other => {
            return Err(ApiError::bad_request(
                "fhir_unsupported_status",
                format!("DiagnosticReport.status '{other}' is not accepted on inbound delivery"),
            ))
        }
    })
}

fn specimen_fhir_status(s: SpecimenStatus) -> &'static str {
    match s {
        SpecimenStatus::Rejected => "unsatisfactory",
        SpecimenStatus::Consumed => "unavailable",
        _ => "available",
    }
}

fn interpretation_concept(i: Interpretation) -> Option<Value> {
    let (code, display) = match i {
        Interpretation::Critical => ("AA", "Critical abnormal"),
        Interpretation::Abnormal => ("A", "Abnormal"),
        Interpretation::Normal => ("N", "Normal"),
        Interpretation::Unknown => return None,
    };
    Some(json!({ "coding": [coding(INTERPRETATION_SYSTEM, code, Some(display))] }))
}

fn component_status(c: &ComponentRow) -> &'static str {
    if c.superseded {
        "amended"
    } else if c.status == "corrected" {
        "corrected"
    } else if c.status == "preliminary" {
        "preliminary"
    } else if c.status == "cancelled" {
        "cancelled"
    } else if c.status == "entered_in_error" {
        "entered-in-error"
    } else {
        "final"
    }
}

fn observation_resource(
    c: &ComponentRow,
    patient_id: Uuid,
    order_id: Uuid,
) -> Result<Value, ApiError> {
    let mut obs = Map::new();
    obs.insert("resourceType".into(), json!("Observation"));
    obs.insert("id".into(), json!(c.id));
    obs.insert("status".into(), json!(component_status(c)));
    obs.insert(
        "code".into(),
        json!({ "coding": [coding(&c.system, &c.code, c.display.as_deref())] }),
    );
    obs.insert("subject".into(), reference("Patient", patient_id));
    obs.insert(
        "basedOn".into(),
        json!([reference("ServiceRequest", order_id)]),
    );
    obs.insert("effectiveDateTime".into(), rfc3339(c.effective_at));
    obs.insert("issued".into(), rfc3339(c.received_at));
    match &c.value {
        ResultValue::Quantity { value, unit } => {
            obs.insert(
                "valueQuantity".into(),
                json!({ "value": decimal_number(*value)?, "unit": unit, "system": UCUM, "code": unit }),
            );
        }
        ResultValue::Text { text } => {
            obs.insert("valueString".into(), json!(text));
        }
        ResultValue::Narrative { text } => {
            obs.insert("valueString".into(), json!(text));
            obs.insert(
                "category".into(),
                json!([{ "coding": [coding(NARRATIVE_CATEGORY, "narrative", Some("Narrative result"))] }]),
            );
        }
        ResultValue::Coded {
            code,
            system,
            display,
        } => {
            obs.insert(
                "valueCodeableConcept".into(),
                json!({ "coding": [coding(system, code, display.as_deref())] }),
            );
        }
        ResultValue::Boolean { value } => {
            obs.insert("valueBoolean".into(), json!(value));
        }
        ResultValue::Datetime { value } => {
            obs.insert("valueDateTime".into(), rfc3339(*value));
        }
    }
    if let Some(r) = &c.reference_range {
        obs.insert("referenceRange".into(), json!([{ "text": r }]));
    }
    if let Some(i) = interpretation_concept(c.interpretation) {
        obs.insert("interpretation".into(), json!([i]));
    }
    if let Some(a) = c.amends {
        obs.insert("derivedFrom".into(), json!([reference("Observation", a)]));
    }
    obs.insert("meta".into(), json!({ "source": c.source_system }));
    Ok(Value::Object(obs))
}

fn report_resource(r: &ReportRow, components: &[ComponentRow]) -> Result<Value, ApiError> {
    let mut rep = Map::new();
    rep.insert("resourceType".into(), json!("DiagnosticReport"));
    rep.insert("id".into(), json!(r.id));
    rep.insert(
        "meta".into(),
        json!({ "versionId": r.version.to_string(), "source": r.source_system }),
    );
    let mut identifiers = vec![json!({ "system": IDEMPOTENCY_SYSTEM, "value": r.idempotency_key })];
    if let Some(ext) = &r.external_report_id {
        identifiers.push(
            json!({ "system": format!("urn:wellos:source:{}", r.source_system), "value": ext }),
        );
    }
    rep.insert("identifier".into(), json!(identifiers));
    rep.insert("status".into(), json!(report_fhir_status(r.status)));
    if let Some(cat) = &r.category_code {
        rep.insert(
            "category".into(),
            json!([{ "coding": [coding("urn:wellos:diagnostic-category", cat, None)] }]),
        );
    }
    rep.insert(
        "code".into(),
        json!({ "coding": [coding("urn:wellos:diagnostic-report", "diagnostic-report", Some("Diagnostic report"))],
                "text": "Diagnostic report" }),
    );
    rep.insert("subject".into(), reference("Patient", r.patient_id));
    rep.insert(
        "basedOn".into(),
        json!([reference("ServiceRequest", r.service_request_id)]),
    );
    if let Some(e) = r.effective_at {
        rep.insert("effectiveDateTime".into(), rfc3339(e));
    }
    rep.insert("issued".into(), rfc3339(r.issued_at));
    if let Some(p) = r.performer_id {
        rep.insert("performer".into(), json!([reference("Practitioner", p)]));
    }
    if let Some(s) = r.signed_by {
        rep.insert(
            "resultsInterpreter".into(),
            json!([reference("Practitioner", s)]),
        );
    }
    rep.insert(
        "result".into(),
        json!(components
            .iter()
            .map(|c| reference("Observation", c.id))
            .collect::<Vec<_>>()),
    );
    if let Some(c) = &r.conclusion {
        rep.insert("conclusion".into(), json!(c));
    }
    let codes: Vec<Value> = r
        .conclusion_codes
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|c| {
                    let system = c.get("system").and_then(Value::as_str)?;
                    let code = c.get("code").and_then(Value::as_str)?;
                    Some(json!({ "coding": [coding(system, code, c.get("display").and_then(Value::as_str))] }))
                })
                .collect()
        })
        .unwrap_or_default();
    if !codes.is_empty() {
        rep.insert("conclusionCode".into(), json!(codes));
    }
    let mut extensions = vec![json!({
        "url": "urn:wellos:criticality",
        "valueCode": r.criticality.as_str()
    })];
    if let Some(reason) = &r.change_reason {
        extensions.push(json!({ "url": CHANGE_REASON_EXTENSION, "valueString": reason }));
    }
    if let Some(prev) = r.replaces {
        extensions.push(json!({ "url": "urn:wellos:replaces", "valueReference": reference("DiagnosticReport", prev) }));
    }
    rep.insert("extension".into(), json!(extensions));
    Ok(Value::Object(rep))
}

fn service_request_resource(o: &OrderRow) -> Value {
    let mut codings = Vec::new();
    if let Some(l) = &o.code_loinc {
        codings.push(coding(LOINC, l, Some(&o.display)));
    }
    if let Some(c) = &o.orderable_code {
        codings.push(coding(
            "urn:wellos:diagnostic-orderable",
            c,
            Some(&o.display),
        ));
    }
    let mut sr = json!({
        "resourceType": "ServiceRequest",
        "id": o.id,
        "meta": { "versionId": o.version.to_string() },
        "status": order_fhir_status(o.order_status),
        "intent": "order",
        "priority": priority_fhir(o.priority),
        "code": { "coding": codings, "text": o.display },
        "subject": reference("Patient", o.patient_id),
        "encounter": reference("Encounter", o.encounter_id),
        "requester": reference("Practitioner", o.requester_id),
        "authoredOn": rfc3339(o.created_at),
        "extension": [
            { "url": "urn:wellos:order-status", "valueCode": o.order_status.as_str() },
            { "url": "urn:wellos:fulfilment-mode", "valueCode": o.fulfilment_mode.as_str() },
            { "url": "urn:wellos:priority", "valueCode": o.priority.as_str() },
        ],
    });
    if let Some(cat) = &o.category_code {
        sr["category"] =
            json!([{ "coding": [coding("urn:wellos:diagnostic-category", cat, None)] }]);
    }
    if let Some(reason) = &o.clinical_indication {
        sr["reasonCode"] = json!([{ "text": reason }]);
    }
    if let (Some(start), Some(end)) = (o.requested_window_start, o.requested_window_end) {
        sr["occurrencePeriod"] = json!({ "start": rfc3339(start), "end": rfc3339(end) });
    }
    if let Some(g) = o.order_group_id {
        sr["requisition"] = json!({ "system": "urn:wellos:order-group", "value": g });
    }
    if let Some(f) = o.performing_facility_id {
        sr["locationReference"] = json!([reference("Location", f)]);
    }
    if let Some(p) = &o.preparation_en {
        sr["patientInstruction"] = json!(p);
    }
    sr
}

fn specimen_resource(s: &SpecimenRow) -> Value {
    let mut sp = json!({
        "resourceType": "Specimen",
        "id": s.id,
        "meta": { "versionId": s.version.to_string() },
        "identifier": [{ "system": "urn:wellos:specimen", "value": s.identifier }],
        "status": specimen_fhir_status(s.status),
        "type": { "coding": [coding("urn:wellos:specimen-type", &s.specimen_type_code, None)] },
        "subject": reference("Patient", s.patient_id),
        "request": [reference("ServiceRequest", s.service_request_id)],
        "extension": [{ "url": "urn:wellos:custody-status", "valueCode": s.status.as_str() }],
    });
    let mut collection = Map::new();
    if let Some(t) = s.collected_at {
        collection.insert("collectedDateTime".into(), rfc3339(t));
    }
    if let Some(by) = s.collected_by {
        collection.insert("collector".into(), reference("Practitioner", by));
    }
    if let Some(site) = &s.body_site {
        collection.insert("bodySite".into(), json!({ "text": site }));
    }
    if !collection.is_empty() {
        sp["collection"] = Value::Object(collection);
    }
    if let Some(c) = &s.container_code {
        sp["container"] =
            json!([{ "type": { "coding": [coding("urn:wellos:container", c, None)] } }]);
    }
    if let Some(reason) = &s.rejection_reason {
        sp["condition"] = json!([{ "text": reason }]);
    }
    if let Some(prev) = s.recollection_of {
        sp["parent"] = json!([reference("Specimen", prev)]);
    }
    sp
}

fn document_resource(d: &DocumentRow) -> Value {
    let status = match d.status.as_str() {
        "rejected" => "entered-in-error",
        _ => "current",
    };
    let mut doc = json!({
        "resourceType": "DocumentReference",
        "id": d.id,
        "status": status,
        "docStatus": if d.released { "final" } else { "preliminary" },
        "type": { "coding": [coding("urn:wellos:document-kind", &d.kind, None)], "text": d.title },
        "subject": reference("Patient", d.patient_id),
        "date": rfc3339(d.created_at),
        "description": d.title,
        "content": [{
            "attachment": {
                "contentType": d.mime_type,
                "size": d.size_bytes,
                "title": d.title,
                // Bytes are never embedded: the authenticated download
                // endpoint mints a short-lived pre-signed URL on demand.
                "url": format!("/api/v1/diagnostics/documents/{}/download", d.id),
                "creation": rfc3339(d.created_at),
            }
        }],
        "extension": [
            { "url": "urn:wellos:scan-status", "valueCode": d.status },
            { "url": "urn:wellos:released", "valueBoolean": d.released },
            { "url": "urn:wellos:checksum-sha256", "valueString": d.checksum_sha256 },
        ],
    });
    let mut context = Map::new();
    if let Some(o) = d.service_request_id {
        doc["context"] = json!({});
        context.insert("related".into(), json!([reference("ServiceRequest", o)]));
    }
    if let Some(r) = d.diagnostic_report_id {
        let related = context.entry("related").or_insert_with(|| json!([]));
        if let Some(list) = related.as_array_mut() {
            list.push(reference("DiagnosticReport", r));
        }
    }
    if !context.is_empty() {
        doc["context"] = Value::Object(context);
    }
    doc
}

fn imaging_resource(row: &sqlx::postgres::PgRow) -> Value {
    let id: Uuid = row.get("id");
    let patient_id: Uuid = row.get("patient_id");
    let order_id: Uuid = row.get("service_request_id");
    let report_id: Option<Uuid> = row.get("diagnostic_report_id");
    let study_uid: String = row.get("study_instance_uid");
    let accession: Option<String> = row.get("accession_number");
    let modality: String = row.get("modality_code");
    let description: Option<String> = row.get("description");
    let series: Value = row.get("series");
    let n_series: i32 = row.get("number_of_series");
    let n_instances: i32 = row.get("number_of_instances");
    let pacs: Option<String> = row.get("pacs_endpoint_code");
    let status: String = row.get("status");
    let started: Option<DateTime<Utc>> = row.get("started_at");
    let source: String = row.get("source_system");

    let mut identifiers =
        vec![json!({ "system": "urn:dicom:uid", "value": format!("urn:oid:{study_uid}") })];
    if let Some(acc) = accession {
        identifiers.push(json!({
            "type": { "coding": [coding("http://terminology.hl7.org/CodeSystem/v2-0203", "ACSN", Some("Accession ID"))] },
            "value": acc
        }));
    }
    let fhir_status = match status.as_str() {
        "available" => "available",
        "cancelled" => "cancelled",
        "entered_in_error" => "entered-in-error",
        "registered" | "in_progress" | "pending" => "registered",
        _ => "unknown",
    };
    let series_json: Vec<Value> = series
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|s| {
                    let uid = s.get("series_instance_uid").or_else(|| s.get("uid")).and_then(Value::as_str)?;
                    let mut item = json!({
                        "uid": uid,
                        "modality": coding(DCM, s.get("modality_code").and_then(Value::as_str).unwrap_or(&modality), None),
                    });
                    if let Some(n) = s.get("number_of_instances").and_then(Value::as_i64) {
                        item["numberOfInstances"] = json!(n);
                    }
                    if let Some(d) = s.get("description").and_then(Value::as_str) {
                        item["description"] = json!(d);
                    }
                    if let Some(n) = s.get("number").and_then(Value::as_i64) {
                        item["number"] = json!(n);
                    }
                    Some(item)
                })
                .collect()
        })
        .unwrap_or_default();
    let mut study = json!({
        "resourceType": "ImagingStudy",
        "id": id,
        "meta": { "source": source },
        "identifier": identifiers,
        "status": fhir_status,
        "modality": [coding(DCM, &modality, None)],
        "subject": reference("Patient", patient_id),
        "basedOn": [reference("ServiceRequest", order_id)],
        "numberOfSeries": n_series,
        "numberOfInstances": n_instances,
        "series": series_json,
    });
    if let Some(d) = description {
        study["description"] = json!(d);
    }
    if let Some(t) = started {
        study["started"] = rfc3339(t);
    }
    if let Some(r) = report_id {
        study["extension"] = json!([{ "url": "urn:wellos:diagnostic-report", "valueReference": reference("DiagnosticReport", r) }]);
    }
    // Only a configured PACS endpoint code is ever exposed, never a caller
    // supplied URL; the Endpoint resource itself is operator configuration.
    if let Some(code) = pacs.filter(|c| profile::id_is_valid(c)) {
        study["endpoint"] = json!([reference("Endpoint", code)]);
    }
    study
}

// ---------------------------------------------------------------------------
// Read facade
// ---------------------------------------------------------------------------

pub async fn patient(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let row = sqlx::query(
        "SELECT id, tenant_id, facility_id, family_name, given_name, birth_date, sex, identifier
         FROM patients WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(ApiError::not_found)?;
    read_guard(
        &state,
        &ctx,
        actions::PATIENT_READ,
        "fhir_patient",
        row.get("tenant_id"),
        id,
        row.get("facility_id"),
    )
    .await?;
    checked(json!({
        "resourceType": "Patient",
        "id": id,
        "identifier": [{ "system": "urn:wellos:mrn", "value": row.get::<String,_>("identifier") }],
        "name": [{ "family": row.get::<String,_>("family_name"), "given": [row.get::<String,_>("given_name")] }],
        "birthDate": row.get::<chrono::NaiveDate,_>("birth_date").to_string(),
        "gender": row.get::<String,_>("sex"),
    }))
}

pub async fn observation(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let tenant_id = locate(&mut conn, "observations", id).await?;
    let (component, patient_id, order_id, _) = load_component(&mut conn, tenant_id, id).await?;
    let facility_id = patient_facility(&mut conn, patient_id).await?;
    drop(conn);
    read_guard(
        &state,
        &ctx,
        actions::PATIENT_READ,
        "fhir_observation",
        tenant_id,
        patient_id,
        facility_id,
    )
    .await?;
    checked(observation_resource(&component, patient_id, order_id)?)
}

pub async fn service_request(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let tenant_id = locate(&mut conn, "service_requests", id).await?;
    let o = load_order(&mut conn, tenant_id, id).await?;
    drop(conn);
    read_guard(
        &state,
        &ctx,
        actions::PATIENT_READ,
        "fhir_service_request",
        tenant_id,
        o.patient_id,
        o.patient_facility_id,
    )
    .await?;
    checked(service_request_resource(&o))
}

pub async fn specimen(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let tenant_id = locate(&mut conn, "specimens", id).await?;
    let s = crate::routes::diagnostics::specimens::load_specimen(&mut conn, tenant_id, id, false)
        .await?;
    let facility_id = patient_facility(&mut conn, s.patient_id).await?;
    drop(conn);
    read_guard(
        &state,
        &ctx,
        actions::DIAGNOSTIC_READ,
        "fhir_specimen",
        tenant_id,
        s.patient_id,
        facility_id,
    )
    .await?;
    checked(specimen_resource(&s))
}

pub async fn diagnostic_report(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let tenant_id = locate(&mut conn, "diagnostic_reports", id).await?;
    let r = load_report(&mut conn, tenant_id, id, false).await?;
    let components = components_of(&mut conn, r.id).await?;
    let facility_id = patient_facility(&mut conn, r.patient_id).await?;
    drop(conn);
    read_guard(
        &state,
        &ctx,
        actions::DIAGNOSTIC_READ,
        "fhir_diagnostic_report",
        tenant_id,
        r.patient_id,
        facility_id,
    )
    .await?;
    checked(report_resource(&r, &components)?)
}

pub async fn document_reference(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let tenant_id = locate(&mut conn, "clinical_documents", id).await?;
    let d = load_document(&mut conn, tenant_id, id, false).await?;
    let facility_id = patient_facility(&mut conn, d.patient_id).await?;
    drop(conn);
    read_guard(
        &state,
        &ctx,
        actions::DIAGNOSTIC_READ,
        "fhir_document_reference",
        tenant_id,
        d.patient_id,
        facility_id,
    )
    .await?;
    checked(document_resource(&d))
}

pub async fn imaging_study(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let row = sqlx::query(
        "SELECT i.id, i.tenant_id, i.patient_id, i.service_request_id, i.diagnostic_report_id,
                i.study_instance_uid, i.accession_number, i.modality_code, i.description, i.series,
                i.number_of_series, i.number_of_instances, i.pacs_endpoint_code, i.status,
                i.started_at, i.source_system, p.facility_id
         FROM imaging_studies i JOIN patients p ON p.id = i.patient_id
         WHERE i.id = $1",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(ApiError::not_found)?;
    read_guard(
        &state,
        &ctx,
        actions::DIAGNOSTIC_READ,
        "fhir_imaging_study",
        row.get("tenant_id"),
        row.get("patient_id"),
        row.get("facility_id"),
    )
    .await?;
    checked(imaging_resource(&row))
}

// ---------------------------------------------------------------------------
// Inbound: DiagnosticReport with contained Observations
// ---------------------------------------------------------------------------

fn fhir_invalid(issues: Vec<profile::Issue>) -> ApiError {
    let summary = issues
        .iter()
        .take(8)
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ");
    ApiError::bad_request("fhir_invalid", summary)
}

fn uuid_of_reference(reference: &str, ty: &str) -> Option<Uuid> {
    reference
        .strip_prefix(ty)
        .and_then(|rest| rest.strip_prefix('/'))
        .and_then(|id| Uuid::parse_str(id).ok())
}

fn parse_datetime(v: Option<&Value>, path: &str) -> Result<Option<DateTime<Utc>>, ApiError> {
    match v.and_then(Value::as_str) {
        None => Ok(None),
        Some(s) => DateTime::parse_from_rfc3339(s)
            .map(|t| Some(t.with_timezone(&Utc)))
            .map_err(|_| {
                ApiError::bad_request(
                    "fhir_invalid",
                    format!("{path} must be an RFC 3339 dateTime"),
                )
            }),
    }
}

fn first_coding(concept: Option<&Value>) -> Option<(String, Option<String>, Option<String>)> {
    let c = concept?;
    if let Some(first) = c
        .get("coding")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
    {
        let code = first.get("code").and_then(Value::as_str)?.to_string();
        return Some((
            code,
            first
                .get("system")
                .and_then(Value::as_str)
                .map(str::to_owned),
            first
                .get("display")
                .and_then(Value::as_str)
                .or_else(|| c.get("text").and_then(Value::as_str))
                .map(str::to_owned),
        ));
    }
    None
}

fn component_from_observation(obs: &Value) -> Result<ComponentInput, ApiError> {
    let (code, system, display) = first_coding(obs.get("code")).ok_or_else(|| {
        ApiError::bad_request(
            "fhir_invalid",
            "contained Observation.code requires a coding",
        )
    })?;
    let narrative = obs
        .get("category")
        .and_then(Value::as_array)
        .is_some_and(|cats| {
            cats.iter().any(|c| {
                c.get("coding")
                    .and_then(Value::as_array)
                    .is_some_and(|list| {
                        list.iter().any(|x| {
                            x.get("system").and_then(Value::as_str) == Some(NARRATIVE_CATEGORY)
                                && x.get("code").and_then(Value::as_str) == Some("narrative")
                        })
                    })
            })
        });
    let value = if let Some(q) = obs.get("valueQuantity") {
        let number = q.get("value").and_then(|n| n.as_number()).ok_or_else(|| {
            ApiError::bad_request("fhir_invalid", "valueQuantity.value must be a number")
        })?;
        let value = Decimal::from_str(&number.to_string()).map_err(|_| {
            ApiError::bad_request(
                "fhir_invalid",
                "valueQuantity.value is not an exact decimal",
            )
        })?;
        let unit = q
            .get("code")
            .or_else(|| q.get("unit"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                ApiError::bad_request("fhir_invalid", "valueQuantity.unit is required")
            })?;
        ResultValue::Quantity {
            value,
            unit: unit.to_string(),
        }
    } else if let Some(s) = obs.get("valueString").and_then(Value::as_str) {
        if narrative {
            ResultValue::Narrative {
                text: s.to_string(),
            }
        } else {
            ResultValue::Text {
                text: s.to_string(),
            }
        }
    } else if let Some(cc) = obs.get("valueCodeableConcept") {
        let (code, system, display) = first_coding(Some(cc)).ok_or_else(|| {
            ApiError::bad_request("fhir_invalid", "valueCodeableConcept requires a coding")
        })?;
        ResultValue::Coded {
            code,
            system: system.unwrap_or_default(),
            display,
        }
    } else if let Some(b) = obs.get("valueBoolean").and_then(Value::as_bool) {
        ResultValue::Boolean { value: b }
    } else if obs.get("valueDateTime").is_some() {
        let value = parse_datetime(obs.get("valueDateTime"), "Observation.valueDateTime")?
            .ok_or_else(|| ApiError::bad_request("fhir_invalid", "valueDateTime is invalid"))?;
        ResultValue::Datetime { value }
    } else {
        return Err(ApiError::bad_request(
            "fhir_unsupported_value",
            "only valueQuantity, valueString, valueCodeableConcept, valueBoolean and valueDateTime are accepted",
        ));
    };
    let amends_observation_id = obs
        .get("derivedFrom")
        .and_then(Value::as_array)
        .and_then(|list| list.first())
        .and_then(|r| r.get("reference"))
        .and_then(Value::as_str)
        .map(|r| {
            uuid_of_reference(r, "Observation").ok_or_else(|| {
                ApiError::bad_request(
                    "fhir_invalid",
                    "derivedFrom must reference Observation/<uuid>",
                )
            })
        })
        .transpose()?;
    Ok(ComponentInput {
        code,
        system,
        display,
        value,
        reference_range: obs
            .get("referenceRange")
            .and_then(Value::as_array)
            .and_then(|list| list.first())
            .and_then(|r| r.get("text"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        effective_at: parse_datetime(
            obs.get("effectiveDateTime"),
            "Observation.effectiveDateTime",
        )?,
        amends_observation_id,
    })
}

/// `POST /fhir/r4/DiagnosticReport` — idempotent inbound delivery from a
/// laboratory, imaging, cardiology or pathology system holding a scoped
/// service credential. The report must reference its WellOS order
/// (`basedOn`), carry an idempotency identifier (`urn:wellos:idempotency`)
/// and name its source system (`meta.source`); results travel as contained
/// Observations. The delivery is issued through the same typed result path
/// as manual entry: deterministic interpretation, criticality, alerts, order
/// and loop transitions are never left to the sender.
pub async fn ingest_diagnostic_report(
    State(state): State<AppState>,
    ctx: AuthContext,
    Json(body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    if body.get("resourceType").and_then(Value::as_str) != Some("DiagnosticReport") {
        return Err(ApiError::bad_request(
            "fhir_invalid",
            "resourceType must be DiagnosticReport",
        ));
    }
    profile::validate(&body, false).map_err(fhir_invalid)?;
    let status = report_status_from_fhir(body["status"].as_str().unwrap_or_default())?;
    let order_id = body
        .get("basedOn")
        .and_then(Value::as_array)
        .and_then(|list| list.first())
        .and_then(|r| r.get("reference"))
        .and_then(Value::as_str)
        .and_then(|r| uuid_of_reference(r, "ServiceRequest"))
        .ok_or_else(|| {
            ApiError::bad_request(
                "fhir_invalid",
                "basedOn must reference ServiceRequest/<uuid>",
            )
        })?;
    let identifiers = body
        .get("identifier")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let idempotency_key = identifiers
        .iter()
        .find(|i| i.get("system").and_then(Value::as_str) == Some(IDEMPOTENCY_SYSTEM))
        .and_then(|i| i.get("value"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| {
            ApiError::bad_request(
                "fhir_invalid",
                "an identifier with system urn:wellos:idempotency is required",
            )
        })?;
    let external_report_id = identifiers
        .iter()
        .find(|i| i.get("system").and_then(Value::as_str) != Some(IDEMPOTENCY_SYSTEM))
        .and_then(|i| i.get("value"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let source_system = body
        .pointer("/meta/source")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            ApiError::bad_request("fhir_invalid", "meta.source (source system) is required")
        })?
        .to_string();
    let contained = body
        .get("contained")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut components = Vec::new();
    for r in body
        .get("result")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let reference = r
            .get("reference")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let Some(local) = reference.strip_prefix('#') else {
            return Err(ApiError::bad_request(
                "fhir_invalid",
                "result entries must reference contained Observations (#id)",
            ));
        };
        let obs = contained
            .iter()
            .find(|c| c.get("id").and_then(Value::as_str) == Some(local))
            .ok_or_else(|| {
                ApiError::bad_request(
                    "fhir_invalid",
                    format!("result #{local} has no contained Observation"),
                )
            })?;
        components.push(component_from_observation(obs)?);
    }
    let conclusion_codes: Vec<ConclusionCode> = body
        .get("conclusionCode")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|cc| first_coding(Some(cc)))
        .map(|(code, system, display)| ConclusionCode {
            system: system.unwrap_or_default(),
            code,
            display,
        })
        .collect();
    let change_reason = body
        .get("extension")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|e| e.get("url").and_then(Value::as_str) == Some(CHANGE_REASON_EXTENSION))
        .and_then(|e| e.get("valueString"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let effective_at = parse_datetime(
        body.get("effectiveDateTime"),
        "DiagnosticReport.effectiveDateTime",
    )?;

    let mut conn = state.pool.acquire().await?;
    let o = load_order(&mut conn, ctx.tenant_id, order_id).await?;
    drop(conn);
    let allowed = guard(
        &state,
        &ctx,
        actions::RESULT_INGEST,
        "fhir_diagnostic_report",
        Some(ResourceCtx {
            tenant_id: o.tenant_id,
            patient_id: Some(o.patient_id),
            facility_id: Some(o.patient_facility_id),
        }),
    )
    .await?;
    let mut tx = state.pool.begin().await?;
    let o = lock_order(&mut tx, ctx.tenant_id, o.id).await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let input = IssueInput {
        status,
        components,
        conclusion: body
            .get("conclusion")
            .and_then(Value::as_str)
            .map(str::to_owned),
        conclusion_codes,
        change_reason,
        idempotency_key,
        source_system,
        effective_at,
        external_report_id,
        sign: status != ReportStatus::Preliminary,
        performer_id: None,
        legacy_observation_key: false,
    };
    let out = issue_in(&mut tx, &ctx, &state, &o, input).await?;
    tx.commit().await?;
    let mut conn = state.pool.acquire().await?;
    let components = components_of(&mut conn, out.report.id).await?;
    let resource = checked(report_resource(&out.report, &components)?)?;
    Ok((
        if out.duplicate {
            StatusCode::OK
        } else {
            StatusCode::CREATED
        },
        resource,
    ))
}
