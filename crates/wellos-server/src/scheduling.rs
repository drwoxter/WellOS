//! Scheduling engine shared by staff, patient self-service and waitlist
//! routes: catalog lookups, tenant policy, matcher fact assembly, atomic
//! holds and bookings, offer confirmation into an authoritative appointment
//! with its linked operational visit, and the appointment state machine.
//!
//! Everything here runs inside the caller's transaction. Concurrency rests
//! on three layers: per-resource advisory locks serialize slot selection,
//! the `resource_bookings` exclusion constraint rejects any overlap that
//! slips through, and optimistic versions protect every stateful row.

use crate::audit;
use crate::auth::AuthContext;
use crate::error::ApiError;
use crate::notify;
use crate::state::AppState;
use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{PgConnection, Row};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;
use wellos_domain::access::{
    is_valid_code, AppointmentStatus, AppointmentTransition, CandidateResource, OfferStatus,
    OfferTransition, ServiceConfig, Urgency, WeeklyWindow,
};
use wellos_domain::matcher::{
    AvailabilityRule, BookingPlan, Candidate, ExistingBooking, FacilityFact, Interval,
    PatientFacts, PolicyFacts, ResourceException, ResourceFact, ResourceServiceFact,
};
use wellos_domain::triage::{VisitStatus, VisitTransition};

pub const CONSENT_CALENDAR: &str = "scheduling_calendar";
pub const CONSENT_LOCATION: &str = "scheduling_location";
pub const CONSENT_TRANSPORT: &str = "transport_coordination";

/// Upper bound on facts loaded per matcher run (defence against a tenant
/// with thousands of resources turning one request into a full scan).
const MAX_RESOURCES_PER_RUN: i64 = 400;
const MAX_BOOKINGS_PER_RESOURCE: i64 = 2_000;

pub const MAX_NOTE: usize = 500;

// ---------------------------------------------------------------------------
// Consent
// ---------------------------------------------------------------------------

/// Whether the highest-version consent decision for `purpose` is active.
pub async fn consent_active(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    patient_id: Uuid,
    purpose: &str,
) -> Result<bool, ApiError> {
    let status: Option<String> = sqlx::query_scalar(
        "SELECT status FROM consents WHERE tenant_id = $1 AND patient_id = $2 AND purpose = $3
         ORDER BY version DESC LIMIT 1",
    )
    .bind(tenant_id)
    .bind(patient_id)
    .bind(purpose)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(status.as_deref() == Some("active"))
}

pub fn consent_required(purpose: &str) -> ApiError {
    ApiError::new(
        axum::http::StatusCode::FORBIDDEN,
        "consent_required",
        format!("this action requires active '{purpose}' consent"),
    )
}

// ---------------------------------------------------------------------------
// Tenant policy
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct Policy {
    pub tenant_id: Uuid,
    pub time_zone: String,
    pub hold_minutes: i32,
    pub offer_ttl_minutes: i32,
    pub cancellation_window_hours: i32,
    pub reschedule_window_hours: i32,
    pub min_notice_hours: i32,
    pub horizon_days: i32,
    pub patient_confirmation_required: bool,
    pub confirmation_deadline_hours: i32,
    pub reminder_lead_hours: Vec<i32>,
    pub quiet_hours_start: NaiveTime,
    pub quiet_hours_end: NaiveTime,
    pub max_candidates: i32,
    pub version: i64,
}

impl Policy {
    fn defaults(tenant_id: Uuid) -> Self {
        Self {
            tenant_id,
            time_zone: "UTC".into(),
            hold_minutes: 15,
            offer_ttl_minutes: 120,
            cancellation_window_hours: 24,
            reschedule_window_hours: 24,
            min_notice_hours: 2,
            horizon_days: 90,
            patient_confirmation_required: false,
            confirmation_deadline_hours: 48,
            reminder_lead_hours: vec![48, 3],
            quiet_hours_start: NaiveTime::from_hms_opt(21, 0, 0).expect("valid"),
            quiet_hours_end: NaiveTime::from_hms_opt(8, 0, 0).expect("valid"),
            max_candidates: 8,
            version: 0,
        }
    }
}

/// The tenant's scheduling policy; defaults apply until the tenant saves
/// one (version 0 signals "never configured").
pub async fn load_policy(conn: &mut PgConnection, tenant_id: Uuid) -> Result<Policy, ApiError> {
    let row = sqlx::query(
        "SELECT time_zone, hold_minutes, offer_ttl_minutes, cancellation_window_hours,
                reschedule_window_hours, min_notice_hours, horizon_days,
                patient_confirmation_required, confirmation_deadline_hours,
                reminder_lead_hours, quiet_hours_start, quiet_hours_end, max_candidates, version
         FROM tenant_scheduling_policies WHERE tenant_id = $1",
    )
    .bind(tenant_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(r) = row else {
        return Ok(Policy::defaults(tenant_id));
    };
    Ok(Policy {
        tenant_id,
        time_zone: r.get("time_zone"),
        hold_minutes: r.get("hold_minutes"),
        offer_ttl_minutes: r.get("offer_ttl_minutes"),
        cancellation_window_hours: r.get("cancellation_window_hours"),
        reschedule_window_hours: r.get("reschedule_window_hours"),
        min_notice_hours: r.get("min_notice_hours"),
        horizon_days: r.get("horizon_days"),
        patient_confirmation_required: r.get("patient_confirmation_required"),
        confirmation_deadline_hours: r.get("confirmation_deadline_hours"),
        reminder_lead_hours: r.get("reminder_lead_hours"),
        quiet_hours_start: r.get("quiet_hours_start"),
        quiet_hours_end: r.get("quiet_hours_end"),
        max_candidates: r.get("max_candidates"),
        version: r.get("version"),
    })
}

pub fn is_iana_time_zone(name: &str) -> bool {
    name.parse::<chrono_tz::Tz>().is_ok()
}

// ---------------------------------------------------------------------------
// Catalog lookups
// ---------------------------------------------------------------------------

/// The minimum catalog a tenant needs before anything can be booked: the
/// services the visit API accepted before catalogs existed, the modalities
/// they reference and the resource types the scheduler books. Mirrors the
/// baseline that migration 0015 installs for pre-existing tenants; tenant
/// provisioning calls this for tenants created afterwards. Idempotent.
pub const BASELINE_CATALOG: &[(&str, &str, &str, &str, &str)] = &[
    (
        "clinical_service",
        "general_medicine",
        "General medicine consultation",
        "Consulta de medicina general",
        r#"{"duration_minutes":20,"modality_codes":["in_person","telehealth"],"required_resource_types":["professional"]}"#,
    ),
    (
        "clinical_service",
        "emergency",
        "Emergency attendance",
        "Atención de urgencias",
        r#"{"duration_minutes":30,"modality_codes":["in_person"],"required_resource_types":["professional"]}"#,
    ),
    (
        "clinical_service",
        "nursing",
        "Nursing care",
        "Atención de enfermería",
        r#"{"duration_minutes":15,"modality_codes":["in_person","home_visit"],"required_resource_types":["professional"]}"#,
    ),
    (
        "clinical_service",
        "telehealth",
        "Telehealth consultation",
        "Consulta de telesalud",
        r#"{"duration_minutes":15,"modality_codes":["telehealth"],"required_resource_types":["professional"]}"#,
    ),
    ("modality", "in_person", "In person", "Presencial", "{}"),
    ("modality", "telehealth", "Telehealth", "Telesalud", "{}"),
    ("modality", "home_visit", "Home visit", "Visita domiciliaria", "{}"),
    ("resource_type", "professional", "Professional", "Profesional", "{}"),
    (
        "resource_type",
        "team",
        "Multidisciplinary team",
        "Equipo multidisciplinar",
        "{}",
    ),
    ("resource_type", "room", "Consultation room", "Consulta", "{}"),
    (
        "resource_type",
        "telehealth_channel",
        "Telehealth channel",
        "Canal de telesalud",
        "{}",
    ),
    ("resource_type", "vehicle", "Vehicle", "Vehículo", "{}"),
    (
        "resource_type",
        "accessible_vehicle",
        "Accessible vehicle",
        "Vehículo adaptado",
        "{}",
    ),
    ("resource_type", "ambulance", "Ambulance", "Ambulancia", "{}"),
];

pub async fn install_baseline_catalog(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    created_by: Uuid,
) -> Result<(), sqlx::Error> {
    for (kind, code, name_en, name_es, config) in BASELINE_CATALOG {
        let config: Value = serde_json::from_str(config).expect("baseline catalog config is valid JSON");
        let inserted = sqlx::query(
            "INSERT INTO catalog_entries (id, tenant_id, kind, code, name_en, name_es, config, created_by)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
             ON CONFLICT (tenant_id, kind, code) DO NOTHING
             RETURNING id, version",
        )
        .bind(Uuid::now_v7())
        .bind(tenant_id)
        .bind(kind)
        .bind(code)
        .bind(name_en)
        .bind(name_es)
        .bind(&config)
        .bind(created_by)
        .fetch_optional(&mut *conn)
        .await?;
        if let Some(row) = inserted {
            let id: Uuid = row.get("id");
            let version: i64 = row.get("version");
            sqlx::query(
                "INSERT INTO catalog_entry_history (id, tenant_id, entry_id, version, snapshot, change_reason, changed_by)
                 VALUES ($1, $2, $3, $4, $5, 'baseline_catalog', $6)",
            )
            .bind(Uuid::now_v7())
            .bind(tenant_id)
            .bind(id)
            .bind(version)
            .bind(json!({
                "kind": kind, "code": code, "name_en": name_en, "name_es": name_es,
                "config": config, "active": true,
            }))
            .bind(created_by)
            .execute(&mut *conn)
            .await?;
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
pub struct ServiceEntry {
    pub code: String,
    pub name_en: String,
    pub name_es: String,
    pub config: ServiceConfig,
}

/// The tenant's active `clinical_service` entry for `code`, or 400.
pub async fn load_service(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    code: &str,
) -> Result<ServiceEntry, ApiError> {
    if !is_valid_code(code) {
        return Err(ApiError::bad_request(
            "validation_failed",
            "service_code has an invalid format",
        ));
    }
    let row = sqlx::query(
        "SELECT code, name_en, name_es, config FROM catalog_entries
         WHERE tenant_id = $1 AND kind = 'clinical_service' AND code = $2 AND active
           AND (effective_from IS NULL OR effective_from <= CURRENT_DATE)
           AND (effective_to IS NULL OR effective_to >= CURRENT_DATE)",
    )
    .bind(tenant_id)
    .bind(code)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(|| {
        ApiError::bad_request(
            "unknown_service",
            "service_code is not an active clinical service in this tenant's catalog",
        )
    })?;
    let config: Value = row.get("config");
    let config: ServiceConfig = serde_json::from_value(config).unwrap_or_default();
    Ok(ServiceEntry {
        code: row.get("code"),
        name_en: row.get("name_en"),
        name_es: row.get("name_es"),
        config,
    })
}

/// Active catalog codes of `kind`.
pub async fn active_codes(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    kind: &str,
) -> Result<BTreeSet<String>, ApiError> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT code FROM catalog_entries WHERE tenant_id = $1 AND kind = $2 AND active",
    )
    .bind(tenant_id)
    .bind(kind)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows.into_iter().collect())
}

/// Every code must be an active entry of `kind`; `field` names the input.
pub async fn require_codes(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    kind: &str,
    codes: &[String],
    field: &str,
) -> Result<(), ApiError> {
    if codes.is_empty() {
        return Ok(());
    }
    for c in codes {
        if !is_valid_code(c) {
            return Err(ApiError::bad_request(
                "validation_failed",
                format!("{field} contains an invalid code"),
            ));
        }
    }
    let known = active_codes(conn, tenant_id, kind).await?;
    if let Some(missing) = codes.iter().find(|c| !known.contains(*c)) {
        return Err(ApiError::bad_request(
            "unknown_catalog_code",
            format!("{field}: '{missing}' is not an active {kind} in this tenant's catalog"),
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Facts assembly for access-matcher.v1
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
struct OpeningHour {
    weekday: u8,
    open: NaiveTime,
    close: NaiveTime,
}

pub async fn load_facility_facts(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    only: Option<&[Uuid]>,
) -> Result<Vec<FacilityFact>, ApiError> {
    let rows = sqlx::query(
        "SELECT f.id, fs.time_zone, fs.opening_hours, fs.latitude, fs.longitude
         FROM facilities f
         LEFT JOIN facility_scheduling fs ON fs.facility_id = f.id
         WHERE f.tenant_id = $1 AND ($2::uuid[] IS NULL OR f.id = ANY($2))
         ORDER BY f.id",
    )
    .bind(tenant_id)
    .bind(only)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .iter()
        .map(|r| {
            let hours: Option<Value> = r.get("opening_hours");
            let opening_hours = hours
                .and_then(|v| serde_json::from_value::<Vec<OpeningHour>>(v).ok())
                .unwrap_or_default()
                .into_iter()
                .map(|h| WeeklyWindow {
                    weekday: h.weekday,
                    start: h.open,
                    end: h.close,
                })
                .collect();
            FacilityFact {
                facility_id: r.get("id"),
                time_zone: r
                    .get::<Option<String>, _>("time_zone")
                    .unwrap_or_else(|| "UTC".into()),
                opening_hours,
                latitude: r.get("latitude"),
                longitude: r.get("longitude"),
            }
        })
        .collect())
}

/// Resources able to deliver `service_code` (plus every active resource of
/// the types the service requires), with their rules, exceptions and active
/// bookings inside `[window_start, window_end)`.
pub async fn load_resource_facts(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    facility_ids: Option<&[Uuid]>,
    service_code: &str,
    required_types: &[String],
    window_start: DateTime<Utc>,
    window_end: DateTime<Utc>,
) -> Result<Vec<ResourceFact>, ApiError> {
    let rows = sqlx::query(
        "SELECT r.id, r.facility_id, r.resource_type_code, r.name, r.user_id, r.languages,
                r.accessibility_codes, r.capacity, r.time_zone
         FROM schedulable_resources r
         WHERE r.tenant_id = $1 AND r.active
           AND ($2::uuid[] IS NULL OR r.facility_id = ANY($2))
           AND (EXISTS (SELECT 1 FROM resource_services s
                        WHERE s.resource_id = r.id AND s.service_code = $3)
                OR r.resource_type_code = ANY($4))
         ORDER BY r.id
         LIMIT $5",
    )
    .bind(tenant_id)
    .bind(facility_ids)
    .bind(service_code)
    .bind(required_types)
    .bind(MAX_RESOURCES_PER_RUN)
    .fetch_all(&mut *conn)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let id: Uuid = r.get("id");
        let services = sqlx::query(
            "SELECT service_code, duration_minutes, prep_minutes, cleanup_minutes, modality_codes
             FROM resource_services WHERE resource_id = $1",
        )
        .bind(id)
        .fetch_all(&mut *conn)
        .await?
        .iter()
        .map(|s| ResourceServiceFact {
            service_code: s.get("service_code"),
            duration_minutes: s.get("duration_minutes"),
            prep_minutes: s.get("prep_minutes"),
            cleanup_minutes: s.get("cleanup_minutes"),
            modality_codes: s.get("modality_codes"),
        })
        .collect();
        let rules = sqlx::query(
            "SELECT weekday, start_local, end_local, kind, capacity, effective_from, effective_to
             FROM resource_availability_rules WHERE resource_id = $1",
        )
        .bind(id)
        .fetch_all(&mut *conn)
        .await?
        .iter()
        .map(|a| AvailabilityRule {
            weekday: a.get::<i16, _>("weekday") as u8,
            start: a.get("start_local"),
            end: a.get("end_local"),
            kind: a.get("kind"),
            capacity: a.get("capacity"),
            effective_from: a.get("effective_from"),
            effective_to: a.get("effective_to"),
        })
        .collect();
        let exceptions = sqlx::query(
            "SELECT kind, starts_at, ends_at, capacity_delta FROM resource_exceptions
             WHERE resource_id = $1 AND ends_at > $2 AND starts_at < $3",
        )
        .bind(id)
        .bind(window_start)
        .bind(window_end)
        .fetch_all(&mut *conn)
        .await?
        .iter()
        .map(|e| ResourceException {
            kind: e.get("kind"),
            start: e.get("starts_at"),
            end: e.get("ends_at"),
            capacity_delta: e.get("capacity_delta"),
        })
        .collect();
        let bookings = sqlx::query(
            "SELECT slot_index, starts_at, ends_at FROM resource_bookings
             WHERE resource_id = $1 AND status = 'active' AND ends_at > $2 AND starts_at < $3
               AND (kind <> 'hold' OR expires_at > now())
             ORDER BY starts_at LIMIT $4",
        )
        .bind(id)
        .bind(window_start)
        .bind(window_end)
        .bind(MAX_BOOKINGS_PER_RESOURCE)
        .fetch_all(&mut *conn)
        .await?
        .iter()
        .map(|b| ExistingBooking {
            slot_index: b.get("slot_index"),
            start: b.get("starts_at"),
            end: b.get("ends_at"),
        })
        .collect();
        out.push(ResourceFact {
            resource_id: id,
            facility_id: r.get("facility_id"),
            resource_type_code: r.get("resource_type_code"),
            name: r.get("name"),
            user_id: r.get("user_id"),
            languages: r.get("languages"),
            accessibility_codes: r.get("accessibility_codes"),
            capacity: r.get("capacity"),
            time_zone: r.get("time_zone"),
            services,
            rules,
            exceptions,
            bookings,
        });
    }
    Ok(out)
}

/// Stored patient scheduling preferences (defaults when none saved).
#[derive(Debug, Clone, Serialize, Default)]
pub struct Preferences {
    pub available_windows: Vec<WeeklyWindow>,
    pub unavailable_windows: Vec<WeeklyWindow>,
    pub preferred_modalities: Vec<String>,
    pub preferred_facility_ids: Vec<Uuid>,
    pub language: Option<String>,
    pub accessibility_needs: Vec<String>,
    pub time_zone: Option<String>,
    pub channels: Vec<String>,
    pub quiet_hours_start: Option<NaiveTime>,
    pub quiet_hours_end: Option<NaiveTime>,
    pub version: i64,
}

pub async fn load_preferences(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    patient_id: Uuid,
) -> Result<Preferences, ApiError> {
    let row = sqlx::query(
        "SELECT available_windows, unavailable_windows, preferred_modalities, preferred_facility_ids,
                language, accessibility_needs, time_zone, channels, quiet_hours_start,
                quiet_hours_end, version
         FROM patient_scheduling_preferences WHERE tenant_id = $1 AND patient_id = $2",
    )
    .bind(tenant_id)
    .bind(patient_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(r) = row else {
        return Ok(Preferences {
            channels: vec!["in_app".into()],
            ..Preferences::default()
        });
    };
    let windows = |v: Value| serde_json::from_value::<Vec<WeeklyWindow>>(v).unwrap_or_default();
    Ok(Preferences {
        available_windows: windows(r.get("available_windows")),
        unavailable_windows: windows(r.get("unavailable_windows")),
        preferred_modalities: r.get("preferred_modalities"),
        preferred_facility_ids: r.get("preferred_facility_ids"),
        language: r.get("language"),
        accessibility_needs: r.get("accessibility_needs"),
        time_zone: r.get("time_zone"),
        channels: r.get("channels"),
        quiet_hours_start: r.get("quiet_hours_start"),
        quiet_hours_end: r.get("quiet_hours_end"),
        version: r.get("version"),
    })
}

/// Assemble the patient side of the facts. `origin` is a one-time
/// coordinate supplied for this run only and never persisted here.
#[allow(clippy::too_many_arguments)]
pub async fn load_patient_facts(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    patient_id: Uuid,
    prefs: &Preferences,
    default_tz: &str,
    origin: Option<(f64, f64)>,
    has_referral: bool,
    window_start: DateTime<Utc>,
    window_end: DateTime<Utc>,
) -> Result<PatientFacts, ApiError> {
    let birth: NaiveDate = sqlx::query_scalar("SELECT birth_date FROM patients WHERE id = $1")
        .bind(patient_id)
        .fetch_one(&mut *conn)
        .await?;
    let today = Utc::now().date_naive();
    let mut age = today.year() - birth.year();
    if (today.month(), today.day()) < (birth.month(), birth.day()) {
        age -= 1;
    }
    let busy = if consent_active(conn, tenant_id, patient_id, CONSENT_CALENDAR).await? {
        sqlx::query(
            "SELECT b.starts_at, b.ends_at FROM patient_busy_intervals b
             JOIN patient_calendar_sources s ON s.id = b.source_id
             WHERE b.tenant_id = $1 AND b.patient_id = $2 AND s.status = 'connected'
               AND b.ends_at > $3 AND b.starts_at < $4",
        )
        .bind(tenant_id)
        .bind(patient_id)
        .bind(window_start)
        .bind(window_end)
        .fetch_all(&mut *conn)
        .await?
        .iter()
        .map(|b| Interval::new(b.get("starts_at"), b.get("ends_at")))
        .collect()
    } else {
        Vec::new()
    };
    // The patient's own live appointments are busy time too.
    let own: Vec<Interval> = sqlx::query(
        "SELECT starts_at, ends_at FROM appointments
         WHERE tenant_id = $1 AND patient_id = $2 AND status IN ('confirmed','rescheduled')
           AND ends_at > $3 AND starts_at < $4",
    )
    .bind(tenant_id)
    .bind(patient_id)
    .bind(window_start)
    .bind(window_end)
    .fetch_all(&mut *conn)
    .await?
    .iter()
    .map(|b| Interval::new(b.get("starts_at"), b.get("ends_at")))
    .collect();
    let mut busy_intervals = busy;
    busy_intervals.extend(own);
    let care_team_user_ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT DISTINCT u FROM (
            SELECT assignee_user_id AS u FROM care_team_assignments
             WHERE tenant_id = $1 AND patient_id = $2 AND active AND assignee_user_id IS NOT NULL
            UNION
            SELECT practitioner_id FROM encounters WHERE tenant_id = $1 AND patient_id = $2
            UNION
            SELECT r.user_id FROM appointments a
              JOIN schedulable_resources r ON r.id = a.primary_resource_id
             WHERE a.tenant_id = $1 AND a.patient_id = $2 AND a.status = 'fulfilled'
               AND r.user_id IS NOT NULL
         ) t WHERE u IS NOT NULL LIMIT 50",
    )
    .bind(tenant_id)
    .bind(patient_id)
    .fetch_all(&mut *conn)
    .await?;
    let kept: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM appointments WHERE tenant_id = $1 AND patient_id = $2 AND status = 'fulfilled'",
    )
    .bind(tenant_id)
    .bind(patient_id)
    .fetch_one(&mut *conn)
    .await?;
    let no_shows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM appointments WHERE tenant_id = $1 AND patient_id = $2 AND status = 'no_show'",
    )
    .bind(tenant_id)
    .bind(patient_id)
    .fetch_one(&mut *conn)
    .await?;
    Ok(PatientFacts {
        age_years: Some(age),
        language: prefs.language.clone(),
        accessibility_needs: prefs.accessibility_needs.clone(),
        time_zone: prefs
            .time_zone
            .clone()
            .unwrap_or_else(|| default_tz.to_string()),
        busy_intervals,
        available_windows: prefs.available_windows.clone(),
        unavailable_windows: prefs.unavailable_windows.clone(),
        care_team_user_ids,
        origin,
        kept_appointments: kept as i32,
        no_shows: no_shows as i32,
        has_referral,
    })
}

/// Policy facts: demand multipliers from the operational calendar and the
/// open cancellation gaps worth filling.
pub async fn load_policy_facts(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    policy: &Policy,
    facility_ids: Option<&[Uuid]>,
    window_start: DateTime<Utc>,
    window_end: DateTime<Utc>,
) -> Result<PolicyFacts, ApiError> {
    let events = sqlx::query(
        "SELECT starts_on, ends_on, demand_multiplier::float8 AS demand FROM operational_calendar_events
         WHERE tenant_id = $1 AND active AND ends_on >= $2 AND starts_on <= $3
           AND (facility_id IS NULL OR $4::uuid[] IS NULL OR facility_id = ANY($4))",
    )
    .bind(tenant_id)
    .bind(window_start.date_naive())
    .bind(window_end.date_naive())
    .bind(facility_ids)
    .fetch_all(&mut *conn)
    .await?;
    let mut demand_by_date: BTreeMap<NaiveDate, f64> = BTreeMap::new();
    for e in &events {
        let mut d: NaiveDate = e.get("starts_on");
        let end: NaiveDate = e.get("ends_on");
        let m: f64 = e.get("demand");
        let mut guard = 0;
        while d <= end && guard < 400 {
            let v = demand_by_date.entry(d).or_insert(1.0);
            *v *= m;
            d += Duration::days(1);
            guard += 1;
        }
    }
    let gaps = sqlx::query(
        "SELECT starts_at, ends_at FROM cancellation_events
         WHERE tenant_id = $1 AND status IN ('open','offered','exhausted')
           AND ends_at > $2 AND starts_at < $3 LIMIT 200",
    )
    .bind(tenant_id)
    .bind(window_start)
    .bind(window_end)
    .fetch_all(&mut *conn)
    .await?
    .iter()
    .map(|g| Interval::new(g.get("starts_at"), g.get("ends_at")))
    .collect();
    Ok(PolicyFacts {
        min_notice_hours: policy.min_notice_hours,
        horizon_days: policy.horizon_days,
        max_candidates: policy.max_candidates.max(1) as usize,
        demand_by_date,
        cancellation_gaps: gaps,
    })
}

// ---------------------------------------------------------------------------
// Bookings: holds and releases
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeldResource {
    pub resource_id: Uuid,
    pub booking_id: Uuid,
    pub role: String,
    pub slot_index: i32,
}

pub fn slot_taken() -> ApiError {
    ApiError::conflict(
        "slot_taken",
        "this time was booked by someone else moments ago; choose another option",
    )
}

fn is_exclusion_violation(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(d) if d.code().as_deref() == Some("23P01"))
}

/// Serialize slot selection per resource for the rest of the transaction.
async fn lock_resources(conn: &mut PgConnection, ids: &[Uuid]) -> Result<(), ApiError> {
    let mut sorted: Vec<Uuid> = ids.to_vec();
    sorted.sort();
    sorted.dedup();
    for id in sorted {
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext('resource_booking'), hashtext($1))")
            .bind(id.to_string())
            .execute(&mut *conn)
            .await?;
    }
    Ok(())
}

/// Insert one active booking per plan, choosing the first free capacity
/// slot. Returns `slot_taken` when a resource has no free slot; the
/// exclusion constraint is the last line of defence under a true race.
#[allow(clippy::too_many_arguments)]
pub async fn book_plans(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    tenant_id: Uuid,
    plans: &[BookingPlan],
    kind: &str,
    offer_id: Option<Uuid>,
    appointment_id: Option<Uuid>,
    expires_at: Option<DateTime<Utc>>,
    created_by: Uuid,
) -> Result<Vec<HeldResource>, ApiError> {
    let ids: Vec<Uuid> = plans.iter().map(|p| p.resource_id).collect();
    lock_resources(tx, &ids).await?;
    let mut held = Vec::with_capacity(plans.len());
    for p in plans {
        let res = sqlx::query(
            "SELECT capacity, active, tenant_id FROM schedulable_resources WHERE id = $1",
        )
        .bind(p.resource_id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(slot_taken)?;
        let capacity: i32 = res.get("capacity");
        let active: bool = res.get("active");
        let owner: Uuid = res.get("tenant_id");
        if !active || owner != tenant_id {
            return Err(slot_taken());
        }
        let used: Vec<i32> = sqlx::query_scalar(
            "SELECT slot_index FROM resource_bookings
             WHERE resource_id = $1 AND status = 'active'
               AND (kind <> 'hold' OR expires_at > now())
               AND tstzrange(starts_at, ends_at, '[)') && tstzrange($2, $3, '[)')",
        )
        .bind(p.resource_id)
        .bind(p.start)
        .bind(p.end)
        .fetch_all(&mut **tx)
        .await?;
        // Expired holds still occupy the exclusion index until released.
        sqlx::query(
            "UPDATE resource_bookings SET status = 'released', released_at = now()
             WHERE resource_id = $1 AND status = 'active' AND kind = 'hold' AND expires_at <= now()",
        )
        .bind(p.resource_id)
        .execute(&mut **tx)
        .await?;
        let slot = (0..capacity)
            .find(|s| !used.contains(s))
            .ok_or_else(slot_taken)?;
        let booking_id = Uuid::now_v7();
        let inserted = sqlx::query(
            "INSERT INTO resource_bookings (id, tenant_id, resource_id, slot_index, starts_at, ends_at,
                                            kind, status, appointment_id, offer_id, expires_at, created_by)
             VALUES ($1,$2,$3,$4,$5,$6,$7,'active',$8,$9,$10,$11)",
        )
        .bind(booking_id)
        .bind(tenant_id)
        .bind(p.resource_id)
        .bind(slot)
        .bind(p.start)
        .bind(p.end)
        .bind(kind)
        .bind(appointment_id)
        .bind(offer_id)
        .bind(expires_at)
        .bind(created_by)
        .execute(&mut **tx)
        .await;
        match inserted {
            Ok(_) => {}
            Err(e) if is_exclusion_violation(&e) => return Err(slot_taken()),
            Err(e) => return Err(e.into()),
        }
        held.push(HeldResource {
            resource_id: p.resource_id,
            booking_id,
            role: p.role.clone(),
            slot_index: slot,
        });
    }
    Ok(held)
}

pub async fn release_offer_bookings(
    conn: &mut PgConnection,
    offer_id: Uuid,
) -> Result<u64, ApiError> {
    let r = sqlx::query(
        "UPDATE resource_bookings SET status = 'released', released_at = now()
         WHERE offer_id = $1 AND status = 'active' AND kind = 'hold'",
    )
    .bind(offer_id)
    .execute(&mut *conn)
    .await?;
    Ok(r.rows_affected())
}

pub async fn release_appointment_bookings(
    conn: &mut PgConnection,
    appointment_id: Uuid,
) -> Result<u64, ApiError> {
    let r = sqlx::query(
        "UPDATE resource_bookings SET status = 'released', released_at = now()
         WHERE appointment_id = $1 AND status = 'active'",
    )
    .bind(appointment_id)
    .execute(&mut *conn)
    .await?;
    Ok(r.rows_affected())
}

/// Release lapsed holds and expire lapsed offers for one tenant. Called at
/// the start of hold/confirm operations and by the scheduled job. Each
/// expired offer is audited under a system actor.
pub async fn sweep_expired(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    cell: &str,
    tenant_id: Uuid,
) -> Result<(u64, Vec<Uuid>), ApiError> {
    let released = sqlx::query(
        "UPDATE resource_bookings SET status = 'released', released_at = now()
         WHERE tenant_id = $1 AND status = 'active' AND kind = 'hold' AND expires_at <= now()",
    )
    .bind(tenant_id)
    .execute(&mut *conn)
    .await?
    .rows_affected();
    // Holds that lapsed but whose offer is still valid go back to 'offered'.
    let unheld = sqlx::query(
        "UPDATE appointment_offers SET status = 'offered', hold_expires_at = NULL,
                version = version + 1, updated_at = now()
         WHERE tenant_id = $1 AND status = 'held' AND hold_expires_at <= now()
           AND offer_expires_at > now()
         RETURNING id",
    )
    .bind(tenant_id)
    .fetch_all(&mut *conn)
    .await?;
    for r in &unheld {
        let id: Uuid = r.get("id");
        offer_history(
            conn,
            tenant_id,
            id,
            Some("held"),
            "offered",
            Some("hold_lapsed"),
            "system:sweep",
        )
        .await?;
        audit::emit(
            &mut *conn,
            ctx,
            "appointment.offer.released",
            cell,
            json!({"offer_id": id}),
            None,
        )
        .await
        .map_err(ApiError::internal)?;
    }
    let expired = sqlx::query(
        "UPDATE appointment_offers SET status = 'expired', hold_expires_at = NULL,
                version = version + 1, updated_at = now()
         WHERE tenant_id = $1 AND status IN ('offered','held') AND offer_expires_at <= now()
         RETURNING id, status",
    )
    .bind(tenant_id)
    .fetch_all(&mut *conn)
    .await?;
    let mut ids = Vec::with_capacity(expired.len());
    for r in &expired {
        let id: Uuid = r.get("id");
        release_offer_bookings(conn, id).await?;
        offer_history(
            conn,
            tenant_id,
            id,
            None,
            "expired",
            Some("offer_ttl_elapsed"),
            "system:sweep",
        )
        .await?;
        audit::emit(
            &mut *conn,
            ctx,
            "appointment.offer.expired",
            cell,
            json!({"offer_id": id}),
            None,
        )
        .await
        .map_err(ApiError::internal)?;
        ids.push(id);
    }
    Ok((released, ids))
}

pub async fn offer_history(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    offer_id: Uuid,
    from: Option<&str>,
    to: &str,
    reason: Option<&str>,
    actor: &str,
) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO appointment_offer_history (id, tenant_id, offer_id, from_status, to_status, reason, actor)
         VALUES ($1,$2,$3,$4,$5,$6,$7)",
    )
    .bind(Uuid::now_v7())
    .bind(tenant_id)
    .bind(offer_id)
    .bind(from)
    .bind(to)
    .bind(reason)
    .bind(actor)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

pub fn actor_label(ctx: &AuthContext) -> String {
    format!("user:{}", ctx.user_id)
}

// ---------------------------------------------------------------------------
// Offers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct OfferRow {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub patient_id: Uuid,
    pub facility_id: Uuid,
    pub access_request_id: Option<Uuid>,
    pub matcher_run_id: Option<Uuid>,
    pub candidate_id: Option<String>,
    pub cancellation_event_id: Option<Uuid>,
    pub waitlist_entry_id: Option<Uuid>,
    pub status: OfferStatus,
    pub service_code: String,
    pub modality_code: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub resources: Vec<BookingPlan>,
    pub score: Option<Value>,
    pub explanation: Option<Value>,
    pub rank: Option<i32>,
    pub offered_to: String,
    pub offer_expires_at: DateTime<Utc>,
    pub hold_expires_at: Option<DateTime<Utc>>,
    pub appointment_id: Option<Uuid>,
    pub version: i64,
}

const OFFER_COLUMNS: &str =
    "id, tenant_id, patient_id, facility_id, access_request_id, matcher_run_id,
    candidate_id, cancellation_event_id, waitlist_entry_id, status, service_code, modality_code,
    starts_at, ends_at, resources, score, explanation, rank, offered_to, offer_expires_at,
    hold_expires_at, appointment_id, version";

fn offer_from_row(r: &sqlx::postgres::PgRow) -> Result<OfferRow, ApiError> {
    let status = OfferStatus::parse(r.get::<String, _>("status").as_str())
        .ok_or_else(|| ApiError::internal("invalid offer status"))?;
    let resources: Value = r.get("resources");
    let resources: Vec<BookingPlan> =
        serde_json::from_value(resources).map_err(ApiError::internal)?;
    Ok(OfferRow {
        id: r.get("id"),
        tenant_id: r.get("tenant_id"),
        patient_id: r.get("patient_id"),
        facility_id: r.get("facility_id"),
        access_request_id: r.get("access_request_id"),
        matcher_run_id: r.get("matcher_run_id"),
        candidate_id: r.get("candidate_id"),
        cancellation_event_id: r.get("cancellation_event_id"),
        waitlist_entry_id: r.get("waitlist_entry_id"),
        status,
        service_code: r.get("service_code"),
        modality_code: r.get("modality_code"),
        starts_at: r.get("starts_at"),
        ends_at: r.get("ends_at"),
        resources,
        score: r.get("score"),
        explanation: r.get("explanation"),
        rank: r.get("rank"),
        offered_to: r.get("offered_to"),
        offer_expires_at: r.get("offer_expires_at"),
        hold_expires_at: r.get("hold_expires_at"),
        appointment_id: r.get("appointment_id"),
        version: r.get("version"),
    })
}

pub async fn load_offer(conn: &mut PgConnection, id: Uuid) -> Result<OfferRow, ApiError> {
    let row = sqlx::query(&format!(
        "SELECT {OFFER_COLUMNS} FROM appointment_offers WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(ApiError::not_found)?;
    offer_from_row(&row)
}

pub async fn lock_offer(conn: &mut PgConnection, id: Uuid) -> Result<OfferRow, ApiError> {
    let row = sqlx::query(&format!(
        "SELECT {OFFER_COLUMNS} FROM appointment_offers WHERE id = $1 FOR UPDATE"
    ))
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(ApiError::not_found)?;
    offer_from_row(&row)
}

pub fn offer_json(o: &OfferRow) -> Value {
    json!({
        "id": o.id,
        "patient_id": o.patient_id,
        "facility_id": o.facility_id,
        "access_request_id": o.access_request_id,
        "matcher_run_id": o.matcher_run_id,
        "candidate_id": o.candidate_id,
        "cancellation_event_id": o.cancellation_event_id,
        "waitlist_entry_id": o.waitlist_entry_id,
        "status": o.status.as_str(),
        "service_code": o.service_code,
        "modality_code": o.modality_code,
        "starts_at": o.starts_at,
        "ends_at": o.ends_at,
        "resources": o.resources,
        "score": o.score,
        "explanation": o.explanation,
        "rank": o.rank,
        "offered_to": o.offered_to,
        "offer_expires_at": o.offer_expires_at,
        "hold_expires_at": o.hold_expires_at,
        "appointment_id": o.appointment_id,
        "version": o.version,
    })
}

fn invalid_offer_transition(o: &OfferRow, t: OfferTransition) -> ApiError {
    ApiError::conflict(
        "invalid_transition",
        format!(
            "an offer in status '{}' cannot {}",
            o.status.as_str(),
            match t {
                OfferTransition::Hold => "be held",
                OfferTransition::ReleaseHold => "release its hold",
                OfferTransition::Accept => "be accepted",
                OfferTransition::Decline => "be declined",
                OfferTransition::Expire => "expire",
                OfferTransition::Revoke => "be revoked",
            }
        ),
    )
}

/// Apply an offer transition with an optimistic version predicate and
/// append its history row.
pub async fn transition_offer(
    conn: &mut PgConnection,
    o: &OfferRow,
    t: OfferTransition,
    hold_expires_at: Option<DateTime<Utc>>,
    reason: Option<&str>,
    actor: &str,
) -> Result<OfferStatus, ApiError> {
    let next = o
        .status
        .apply(t)
        .map_err(|_| invalid_offer_transition(o, t))?;
    let updated = sqlx::query(
        "UPDATE appointment_offers SET status = $1, hold_expires_at = $2, decline_reason = COALESCE($5, decline_reason),
                version = version + 1, updated_at = now()
         WHERE id = $3 AND version = $4",
    )
    .bind(next.as_str())
    .bind(hold_expires_at)
    .bind(o.id)
    .bind(o.version)
    .bind(if t == OfferTransition::Decline { reason } else { None })
    .execute(&mut *conn)
    .await?;
    if updated.rows_affected() != 1 {
        return Err(stale());
    }
    offer_history(
        conn,
        o.tenant_id,
        o.id,
        Some(o.status.as_str()),
        next.as_str(),
        reason,
        actor,
    )
    .await?;
    Ok(next)
}

/// Revoke a live offer whose resource capacity disappeared (deactivation,
/// leave, closure). Bookings are released by the caller; a no-longer-live
/// offer is left untouched.
pub async fn revoke_offer_for_resource(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ctx: &AuthContext,
    state: &AppState,
    offer_id: Uuid,
    reason: &str,
) -> Result<(), ApiError> {
    let o = lock_offer(tx, offer_id).await?;
    if !o.status.is_live() {
        return Ok(());
    }
    release_offer_bookings(tx, o.id).await?;
    transition_offer(
        tx,
        &o,
        OfferTransition::Revoke,
        None,
        Some(reason),
        &actor_label(ctx),
    )
    .await?;
    audit::emit(
        &mut **tx,
        ctx,
        "appointment.offer.revoked",
        &state.cell,
        json!({ "offer_id": o.id, "reason": reason }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    Ok(())
}

pub fn stale() -> ApiError {
    ApiError::conflict(
        "stale_version",
        "this record changed since it was loaded; refresh and try again",
    )
}

/// Place (or re-place) the temporary hold behind an offer: every planned
/// resource is booked atomically for `hold_minutes`.
pub async fn hold_offer(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ctx: &AuthContext,
    state: &AppState,
    o: &OfferRow,
    policy: &Policy,
) -> Result<OfferRow, ApiError> {
    if o.offer_expires_at <= Utc::now() {
        return Err(ApiError::conflict(
            "offer_expired",
            "this offer has expired",
        ));
    }
    let expires = Utc::now() + Duration::minutes(policy.hold_minutes as i64);
    let hold_until = expires.min(o.offer_expires_at);
    let placed = book_plans(
        tx,
        o.tenant_id,
        &o.resources,
        "hold",
        Some(o.id),
        None,
        Some(hold_until),
        ctx.user_id,
    )
    .await;
    match placed {
        Ok(_) => {}
        Err(e) if e.code == "slot_taken" => {
            return Err(e);
        }
        Err(e) => return Err(e),
    }
    transition_offer(
        tx,
        o,
        OfferTransition::Hold,
        Some(hold_until),
        None,
        &actor_label(ctx),
    )
    .await?;
    audit::emit(
        &mut **tx,
        ctx,
        "appointment.offer.held",
        &state.cell,
        json!({ "offer_id": o.id, "patient_id": o.patient_id, "hold_expires_at": hold_until }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    lock_offer(tx, o.id).await
}

/// Audit a lost race outside the failed transaction so the evidence
/// survives the rollback.
pub async fn record_race_lost(state: &AppState, ctx: &AuthContext, offer_id: Uuid, stage: &str) {
    let mut conn = match state.pool.acquire().await {
        Ok(c) => c,
        Err(_) => return,
    };
    let _ = audit::emit(
        &mut *conn,
        ctx,
        "appointment.offer.race_lost",
        &state.cell,
        json!({ "offer_id": offer_id, "stage": stage }),
        None,
    )
    .await;
}

// ---------------------------------------------------------------------------
// Appointments
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct AppointmentRow {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub facility_id: Uuid,
    pub patient_id: Uuid,
    pub service_code: String,
    pub modality_code: String,
    pub status: AppointmentStatus,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub time_zone: String,
    pub reason: Option<String>,
    pub access_request_id: Option<Uuid>,
    pub offer_id: Option<Uuid>,
    pub matcher_run_id: Option<Uuid>,
    pub candidate_id: Option<String>,
    pub score: Option<Value>,
    pub primary_resource_id: Option<Uuid>,
    pub visit_id: Option<Uuid>,
    pub confirmation_required: bool,
    pub patient_confirmed_at: Option<DateTime<Utc>>,
    pub confirmation_due_at: Option<DateTime<Utc>>,
    pub booked_via: String,
    pub booked_by: Uuid,
    pub override_reason: Option<String>,
    pub rescheduled_from: Option<Uuid>,
    pub rescheduled_to: Option<Uuid>,
    pub cancellation_reason: Option<String>,
    pub cancellation_note: Option<String>,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub fulfilled_at: Option<DateTime<Utc>>,
    pub no_show_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub const APPOINTMENT_COLUMNS: &str =
    "id, tenant_id, facility_id, patient_id, service_code, modality_code,
    status, starts_at, ends_at, time_zone, reason, access_request_id, offer_id, matcher_run_id,
    candidate_id, score, primary_resource_id, visit_id, confirmation_required, patient_confirmed_at,
    confirmation_due_at, booked_via, booked_by, override_reason, rescheduled_from, rescheduled_to,
    cancellation_reason, cancellation_note, cancelled_at, fulfilled_at, no_show_at, version,
    created_at, updated_at";

pub fn appointment_from_row(r: &sqlx::postgres::PgRow) -> Result<AppointmentRow, ApiError> {
    let status = AppointmentStatus::parse(r.get::<String, _>("status").as_str())
        .ok_or_else(|| ApiError::internal("invalid appointment status"))?;
    Ok(AppointmentRow {
        id: r.get("id"),
        tenant_id: r.get("tenant_id"),
        facility_id: r.get("facility_id"),
        patient_id: r.get("patient_id"),
        service_code: r.get("service_code"),
        modality_code: r.get("modality_code"),
        status,
        starts_at: r.get("starts_at"),
        ends_at: r.get("ends_at"),
        time_zone: r.get("time_zone"),
        reason: r.get("reason"),
        access_request_id: r.get("access_request_id"),
        offer_id: r.get("offer_id"),
        matcher_run_id: r.get("matcher_run_id"),
        candidate_id: r.get("candidate_id"),
        score: r.get("score"),
        primary_resource_id: r.get("primary_resource_id"),
        visit_id: r.get("visit_id"),
        confirmation_required: r.get("confirmation_required"),
        patient_confirmed_at: r.get("patient_confirmed_at"),
        confirmation_due_at: r.get("confirmation_due_at"),
        booked_via: r.get("booked_via"),
        booked_by: r.get("booked_by"),
        override_reason: r.get("override_reason"),
        rescheduled_from: r.get("rescheduled_from"),
        rescheduled_to: r.get("rescheduled_to"),
        cancellation_reason: r.get("cancellation_reason"),
        cancellation_note: r.get("cancellation_note"),
        cancelled_at: r.get("cancelled_at"),
        fulfilled_at: r.get("fulfilled_at"),
        no_show_at: r.get("no_show_at"),
        version: r.get("version"),
        created_at: r.get("created_at"),
        updated_at: r.get("updated_at"),
    })
}

pub async fn load_appointment(
    conn: &mut PgConnection,
    id: Uuid,
) -> Result<AppointmentRow, ApiError> {
    let row = sqlx::query(&format!(
        "SELECT {APPOINTMENT_COLUMNS} FROM appointments WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(ApiError::not_found)?;
    appointment_from_row(&row)
}

pub async fn lock_appointment(
    conn: &mut PgConnection,
    id: Uuid,
) -> Result<AppointmentRow, ApiError> {
    let row = sqlx::query(&format!(
        "SELECT {APPOINTMENT_COLUMNS} FROM appointments WHERE id = $1 FOR UPDATE"
    ))
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(ApiError::not_found)?;
    appointment_from_row(&row)
}

/// Safe JSON for staff and patients: scheduling facts only, no clinical
/// content beyond the service code and free-text reason the booker wrote.
pub fn appointment_json(a: &AppointmentRow) -> Value {
    json!({
        "id": a.id,
        "facility_id": a.facility_id,
        "patient_id": a.patient_id,
        "service_code": a.service_code,
        "modality_code": a.modality_code,
        "status": a.status.as_str(),
        "starts_at": a.starts_at,
        "ends_at": a.ends_at,
        "time_zone": a.time_zone,
        "reason": a.reason,
        "access_request_id": a.access_request_id,
        "offer_id": a.offer_id,
        "matcher_run_id": a.matcher_run_id,
        "candidate_id": a.candidate_id,
        "score": a.score,
        "primary_resource_id": a.primary_resource_id,
        "visit_id": a.visit_id,
        "confirmation_required": a.confirmation_required,
        "patient_confirmed_at": a.patient_confirmed_at,
        "confirmation_due_at": a.confirmation_due_at,
        "booked_via": a.booked_via,
        "override_reason": a.override_reason,
        "rescheduled_from": a.rescheduled_from,
        "rescheduled_to": a.rescheduled_to,
        "cancellation_reason": a.cancellation_reason,
        "cancellation_note": a.cancellation_note,
        "cancelled_at": a.cancelled_at,
        "fulfilled_at": a.fulfilled_at,
        "no_show_at": a.no_show_at,
        "version": a.version,
        "created_at": a.created_at,
        "updated_at": a.updated_at,
    })
}

#[allow(clippy::too_many_arguments)]
pub async fn appointment_history(
    conn: &mut PgConnection,
    a: &AppointmentRow,
    from: Option<AppointmentStatus>,
    to: AppointmentStatus,
    starts_after: Option<DateTime<Utc>>,
    reason_code: Option<&str>,
    note: Option<&str>,
    override_: bool,
    actor: &str,
    version: i64,
) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO appointment_history (id, tenant_id, appointment_id, from_status, to_status,
             starts_at_before, starts_at_after, reason_code, note, override, actor, version)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)",
    )
    .bind(Uuid::now_v7())
    .bind(a.tenant_id)
    .bind(a.id)
    .bind(from.map(|s| s.as_str()))
    .bind(to.as_str())
    .bind(from.map(|_| a.starts_at))
    .bind(starts_after)
    .bind(reason_code)
    .bind(note)
    .bind(override_)
    .bind(actor)
    .bind(version)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Who confirmed and through which channel.
#[derive(Debug, Clone)]
pub struct ConfirmInput<'a> {
    pub offer: &'a OfferRow,
    pub booked_via: &'a str,
    pub reason: Option<String>,
    pub override_reason: Option<String>,
    pub idempotency_key: Option<String>,
    /// The appointment being replaced when this confirmation is a
    /// reschedule; its visit moves to the new appointment.
    pub reschedule_of: Option<&'a AppointmentRow>,
    pub reschedule_reason: Option<String>,
}

/// Accept a held (or still-offered) offer into a confirmed appointment:
/// converts or places the bookings, creates the appointment and its linked
/// scheduled visit (or moves the visit on reschedule), marks the request
/// booked, revokes sibling offers and schedules notifications — all in the
/// caller's transaction.
pub async fn confirm_offer(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ctx: &AuthContext,
    state: &AppState,
    input: ConfirmInput<'_>,
    policy: &Policy,
    service: &ServiceEntry,
) -> Result<AppointmentRow, ApiError> {
    let o = input.offer;
    if o.offer_expires_at <= Utc::now() {
        return Err(ApiError::conflict(
            "offer_expired",
            "this offer has expired",
        ));
    }
    if o.starts_at <= Utc::now() {
        return Err(ApiError::conflict(
            "slot_in_past",
            "this option is no longer in the future",
        ));
    }
    let appointment_id = Uuid::now_v7();
    // A live hold converts in place; an un-held offer books directly.
    let live_hold: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM resource_bookings WHERE offer_id = $1 AND status = 'active'
           AND kind = 'hold' AND expires_at > now()",
    )
    .bind(o.id)
    .fetch_one(&mut **tx)
    .await?;
    let held: Vec<HeldResource> = if o.status == OfferStatus::Held
        && live_hold as usize == o.resources.len()
    {
        lock_resources(
            tx,
            &o.resources
                .iter()
                .map(|p| p.resource_id)
                .collect::<Vec<_>>(),
        )
        .await?;
        let rows = sqlx::query(
            "UPDATE resource_bookings SET kind = 'appointment', expires_at = NULL, appointment_id = $2
             WHERE offer_id = $1 AND status = 'active' AND kind = 'hold'
             RETURNING id, resource_id, slot_index",
        )
        .bind(o.id)
        .bind(appointment_id)
        .fetch_all(&mut **tx)
        .await?;
        rows.iter()
            .map(|r| {
                let resource_id: Uuid = r.get("resource_id");
                let role = o
                    .resources
                    .iter()
                    .find(|p| p.resource_id == resource_id)
                    .map(|p| p.role.clone())
                    .unwrap_or_else(|| "resource".into());
                HeldResource {
                    resource_id,
                    booking_id: r.get("id"),
                    role,
                    slot_index: r.get("slot_index"),
                }
            })
            .collect()
    } else {
        // Any stale partial hold is released before booking afresh.
        release_offer_bookings(tx, o.id).await?;
        book_plans(
            tx,
            o.tenant_id,
            &o.resources,
            "appointment",
            Some(o.id),
            Some(appointment_id),
            None,
            ctx.user_id,
        )
        .await?
    };
    let primary = o
        .resources
        .iter()
        .find(|p| p.role == "primary")
        .or_else(|| o.resources.first())
        .map(|p| p.resource_id);
    let confirmation_required = service
        .config
        .patient_confirmation_required
        .unwrap_or(policy.patient_confirmation_required);
    let confirmation_due_at = confirmation_required.then(|| {
        let due = o.starts_at - Duration::hours(policy.confirmation_deadline_hours as i64);
        due.max(Utc::now() + Duration::hours(1))
    });
    let facility_tz: Option<String> =
        sqlx::query_scalar("SELECT time_zone FROM facility_scheduling WHERE facility_id = $1")
            .bind(o.facility_id)
            .fetch_optional(&mut **tx)
            .await?;
    let time_zone = facility_tz.unwrap_or_else(|| policy.time_zone.clone());
    let inserted = sqlx::query(
        "INSERT INTO appointments (id, tenant_id, facility_id, patient_id, service_code, modality_code,
             status, starts_at, ends_at, time_zone, reason, access_request_id, offer_id, matcher_run_id,
             candidate_id, score, primary_resource_id, confirmation_required, confirmation_due_at,
             booked_via, booked_by, override_reason, rescheduled_from, idempotency_key)
         VALUES ($1,$2,$3,$4,$5,$6,'confirmed',$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22,$23)",
    )
    .bind(appointment_id)
    .bind(o.tenant_id)
    .bind(o.facility_id)
    .bind(o.patient_id)
    .bind(&o.service_code)
    .bind(&o.modality_code)
    .bind(o.starts_at)
    .bind(o.ends_at)
    .bind(&time_zone)
    .bind(&input.reason)
    .bind(o.access_request_id)
    .bind(o.id)
    .bind(o.matcher_run_id)
    .bind(&o.candidate_id)
    .bind(&o.score)
    .bind(primary)
    .bind(confirmation_required)
    .bind(confirmation_due_at)
    .bind(input.booked_via)
    .bind(ctx.user_id)
    .bind(&input.override_reason)
    .bind(input.reschedule_of.map(|a| a.id))
    .bind(&input.idempotency_key)
    .execute(&mut **tx)
    .await;
    match inserted {
        Ok(_) => {}
        Err(sqlx::Error::Database(d)) if d.code().as_deref() == Some("23505") => {
            return Err(ApiError::conflict(
                "patient_already_booked",
                "this patient already has a live appointment at this time",
            ));
        }
        Err(e) => return Err(e.into()),
    }
    for h in &held {
        sqlx::query(
            "INSERT INTO appointment_resources (appointment_id, resource_id, booking_id, role)
             VALUES ($1,$2,$3,$4) ON CONFLICT DO NOTHING",
        )
        .bind(appointment_id)
        .bind(h.resource_id)
        .bind(h.booking_id)
        .bind(&h.role)
        .execute(&mut **tx)
        .await?;
    }
    let mut appt = lock_appointment(tx, appointment_id).await?;
    let actor = actor_label(ctx);
    appointment_history(
        tx,
        &appt,
        None,
        AppointmentStatus::Confirmed,
        Some(appt.starts_at),
        input.reschedule_of.map(|_| "rescheduled_from_previous"),
        input.reason.as_deref(),
        input.override_reason.is_some(),
        &actor,
        appt.version,
    )
    .await?;

    // Offer accepted; sibling offers of the same request are revoked.
    transition_offer(tx, o, OfferTransition::Accept, None, None, &actor).await?;
    sqlx::query("UPDATE appointment_offers SET appointment_id = $2 WHERE id = $1")
        .bind(o.id)
        .bind(appointment_id)
        .execute(&mut **tx)
        .await?;
    if let Some(req) = o.access_request_id {
        let siblings = sqlx::query(&format!(
            "SELECT {OFFER_COLUMNS} FROM appointment_offers
             WHERE access_request_id = $1 AND id <> $2 AND status IN ('offered','held') FOR UPDATE"
        ))
        .bind(req)
        .bind(o.id)
        .fetch_all(&mut **tx)
        .await?;
        for s in &siblings {
            let s = offer_from_row(s)?;
            release_offer_bookings(tx, s.id).await?;
            transition_offer(
                tx,
                &s,
                OfferTransition::Revoke,
                None,
                Some("sibling_accepted"),
                &actor,
            )
            .await?;
            audit::emit(
                &mut **tx,
                ctx,
                "appointment.offer.revoked",
                &state.cell,
                json!({ "offer_id": s.id, "reason": "sibling_accepted" }),
                None,
            )
            .await
            .map_err(ApiError::internal)?;
        }
        mark_request_booked(tx, ctx, state, req, appointment_id, &actor).await?;
    }

    // Visit: move the previous one on reschedule, otherwise create.
    match input.reschedule_of {
        Some(prev) => {
            let mut moved = false;
            if let Some(visit_id) = prev.visit_id {
                let status: Option<String> =
                    sqlx::query_scalar("SELECT status FROM visits WHERE id = $1 FOR UPDATE")
                        .bind(visit_id)
                        .fetch_optional(&mut **tx)
                        .await?;
                if status.as_deref() == Some("scheduled") {
                    sqlx::query(
                        "UPDATE visits SET scheduled_at = $2, facility_id = $3, service = $4, appointment_id = NULL,
                                version = version + 1, updated_at = now() WHERE id = $1",
                    )
                    .bind(visit_id)
                    .bind(appt.starts_at)
                    .bind(appt.facility_id)
                    .bind(&appt.service_code)
                    .execute(&mut **tx)
                    .await?;
                    sqlx::query("UPDATE appointments SET visit_id = NULL WHERE id = $1")
                        .bind(prev.id)
                        .execute(&mut **tx)
                        .await?;
                    sqlx::query("UPDATE visits SET appointment_id = $2 WHERE id = $1")
                        .bind(visit_id)
                        .bind(appointment_id)
                        .execute(&mut **tx)
                        .await?;
                    sqlx::query("UPDATE appointments SET visit_id = $2 WHERE id = $1")
                        .bind(appointment_id)
                        .bind(visit_id)
                        .execute(&mut **tx)
                        .await?;
                    moved = true;
                }
            }
            if !moved {
                create_visit_for(tx, ctx, &appt).await?;
            }
            // Previous appointment becomes 'rescheduled' and frees its slot.
            let prev = lock_appointment(tx, prev.id).await?;
            let next = prev
                .status
                .apply(AppointmentTransition::Reschedule)
                .map_err(|_| {
                    invalid_appointment_transition(&prev, AppointmentTransition::Reschedule)
                })?;
            let updated = sqlx::query(
                "UPDATE appointments SET status = $1, rescheduled_to = $2, version = version + 1, updated_at = now()
                 WHERE id = $3 AND version = $4",
            )
            .bind(next.as_str())
            .bind(appointment_id)
            .bind(prev.id)
            .bind(prev.version)
            .execute(&mut **tx)
            .await?;
            if updated.rows_affected() != 1 {
                return Err(stale());
            }
            release_appointment_bookings(tx, prev.id).await?;
            appointment_history(
                tx,
                &prev,
                Some(prev.status),
                next,
                Some(appt.starts_at),
                input.reschedule_reason.as_deref(),
                input.reason.as_deref(),
                input.override_reason.is_some(),
                &actor,
                prev.version + 1,
            )
            .await?;
            notify::cancel_pending_for_appointment(tx, prev.tenant_id, prev.id).await?;
            audit::emit(
                &mut **tx,
                ctx,
                "appointment.rescheduled",
                &state.cell,
                json!({
                    "appointment_id": prev.id,
                    "new_appointment_id": appointment_id,
                    "patient_id": prev.patient_id,
                    "reason_code": input.reschedule_reason,
                    "override": input.override_reason.is_some(),
                }),
                None,
            )
            .await
            .map_err(ApiError::internal)?;
        }
        None => {
            create_visit_for(tx, ctx, &appt).await?;
        }
    }
    appt = lock_appointment(tx, appointment_id).await?;
    audit::emit(
        &mut **tx,
        ctx,
        "appointment.confirmed",
        &state.cell,
        json!({
            "appointment_id": appt.id,
            "patient_id": appt.patient_id,
            "facility_id": appt.facility_id,
            "service_code": appt.service_code,
            "offer_id": o.id,
            "booked_via": input.booked_via,
            "override": input.override_reason.is_some(),
        }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    if input.override_reason.is_some() {
        audit::emit(
            &mut **tx,
            ctx,
            "appointment.overridden",
            &state.cell,
            json!({ "appointment_id": appt.id, "stage": "confirm" }),
            None,
        )
        .await
        .map_err(ApiError::internal)?;
    }
    notify::schedule_for_confirmed(
        tx,
        ctx,
        state,
        &appt,
        policy,
        service,
        input.reschedule_of.is_some(),
    )
    .await?;
    Ok(appt)
}

async fn mark_request_booked(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ctx: &AuthContext,
    state: &AppState,
    request_id: Uuid,
    appointment_id: Uuid,
    actor: &str,
) -> Result<(), ApiError> {
    let row = sqlx::query("SELECT status, version FROM access_requests WHERE id = $1 FOR UPDATE")
        .bind(request_id)
        .fetch_optional(&mut **tx)
        .await?;
    let Some(row) = row else { return Ok(()) };
    let status: String = row.get("status");
    let version: i64 = row.get("version");
    let from = wellos_domain::access::AccessRequestStatus::parse(&status)
        .ok_or_else(|| ApiError::internal("invalid access request status"))?;
    let Ok(next) = from.apply(wellos_domain::access::AccessRequestTransition::Book) else {
        // Already booked/closed: the accepted offer still stands; the request
        // simply keeps its terminal state.
        return Ok(());
    };
    sqlx::query(
        "UPDATE access_requests SET status = $1, appointment_id = $2, version = version + 1, updated_at = now()
         WHERE id = $3 AND version = $4",
    )
    .bind(next.as_str())
    .bind(appointment_id)
    .bind(request_id)
    .bind(version)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "INSERT INTO access_request_history (id, tenant_id, access_request_id, from_status, to_status, reason, actor)
         VALUES ($1,$2,$3,$4,$5,$6,$7)",
    )
    .bind(Uuid::now_v7())
    .bind(ctx.tenant_id)
    .bind(request_id)
    .bind(from.as_str())
    .bind(next.as_str())
    .bind("offer_accepted")
    .bind(actor)
    .execute(&mut **tx)
    .await?;
    audit::emit(
        &mut **tx,
        ctx,
        "access_request.transitioned",
        &state.cell,
        json!({ "access_request_id": request_id, "to": next.as_str(), "appointment_id": appointment_id }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    Ok(())
}

/// The operational visit behind a confirmed appointment. The visit stays
/// `scheduled` until arrival; the one-open-visit-per-patient invariant is
/// not affected by scheduled visits.
async fn create_visit_for(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ctx: &AuthContext,
    appt: &AppointmentRow,
) -> Result<Uuid, ApiError> {
    let visit_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO visits (id, tenant_id, facility_id, patient_id, status, arrival_kind, service,
                             reason, scheduled_at, created_by, appointment_id)
         VALUES ($1,$2,$3,$4,'scheduled','scheduled',$5,$6,$7,$8,$9)",
    )
    .bind(visit_id)
    .bind(appt.tenant_id)
    .bind(appt.facility_id)
    .bind(appt.patient_id)
    .bind(&appt.service_code)
    .bind(&appt.reason)
    .bind(appt.starts_at)
    .bind(ctx.user_id)
    .bind(appt.id)
    .execute(&mut **tx)
    .await?;
    sqlx::query("UPDATE appointments SET visit_id = $2 WHERE id = $1")
        .bind(appt.id)
        .bind(visit_id)
        .execute(&mut **tx)
        .await?;
    Ok(visit_id)
}

/// A staff-registered scheduled visit gets its authoritative appointment in
/// the same transaction (`booked_via = 'staff_direct'`, no resource
/// booking), so arrival, cancellation and no-show stay consistent.
#[allow(clippy::too_many_arguments)]
pub async fn direct_appointment_for_visit(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ctx: &AuthContext,
    state: &AppState,
    visit_id: Uuid,
    facility_id: Uuid,
    patient_id: Uuid,
    service: &ServiceEntry,
    starts_at: DateTime<Utc>,
    reason: Option<&str>,
) -> Result<AppointmentRow, ApiError> {
    let policy = load_policy(tx, ctx.tenant_id).await?;
    let facility_tz: Option<String> =
        sqlx::query_scalar("SELECT time_zone FROM facility_scheduling WHERE facility_id = $1")
            .bind(facility_id)
            .fetch_optional(&mut **tx)
            .await?;
    let time_zone = facility_tz.unwrap_or_else(|| policy.time_zone.clone());
    let ends_at = starts_at + Duration::minutes(service.config.duration_minutes.max(5) as i64);
    let modality = service
        .config
        .modality_codes
        .first()
        .cloned()
        .unwrap_or_else(|| "in_person".into());
    let id = Uuid::now_v7();
    let inserted = sqlx::query(
        "INSERT INTO appointments (id, tenant_id, facility_id, patient_id, service_code, modality_code,
             status, starts_at, ends_at, time_zone, reason, visit_id, confirmation_required,
             booked_via, booked_by)
         VALUES ($1,$2,$3,$4,$5,$6,'confirmed',$7,$8,$9,$10,$11,false,'staff_direct',$12)",
    )
    .bind(id)
    .bind(ctx.tenant_id)
    .bind(facility_id)
    .bind(patient_id)
    .bind(&service.code)
    .bind(&modality)
    .bind(starts_at)
    .bind(ends_at)
    .bind(&time_zone)
    .bind(reason)
    .bind(visit_id)
    .bind(ctx.user_id)
    .execute(&mut **tx)
    .await;
    match inserted {
        Ok(_) => {}
        Err(sqlx::Error::Database(d)) if d.code().as_deref() == Some("23505") => {
            return Err(ApiError::conflict(
                "patient_already_booked",
                "this patient already has a live appointment at this time",
            ));
        }
        Err(e) => return Err(e.into()),
    }
    sqlx::query("UPDATE visits SET appointment_id = $2 WHERE id = $1")
        .bind(visit_id)
        .bind(id)
        .execute(&mut **tx)
        .await?;
    let appt = lock_appointment(tx, id).await?;
    let actor = actor_label(ctx);
    appointment_history(
        tx,
        &appt,
        None,
        AppointmentStatus::Confirmed,
        Some(appt.starts_at),
        Some("staff_direct"),
        reason,
        false,
        &actor,
        appt.version,
    )
    .await?;
    audit::emit(
        &mut **tx,
        ctx,
        "appointment.confirmed",
        &state.cell,
        json!({ "appointment_id": id, "visit_id": visit_id, "patient_id": patient_id, "booked_via": "staff_direct" }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    notify::schedule_for_confirmed(tx, ctx, state, &appt, &policy, service, false).await?;
    Ok(appt)
}

fn invalid_appointment_transition(a: &AppointmentRow, t: AppointmentTransition) -> ApiError {
    ApiError::conflict(
        "invalid_transition",
        format!(
            "an appointment in status '{}' cannot {}",
            a.status.as_str(),
            match t {
                AppointmentTransition::Reschedule => "be rescheduled",
                AppointmentTransition::Cancel => "be cancelled",
                AppointmentTransition::Fulfil => "be fulfilled",
                AppointmentTransition::MarkNoShow => "be marked no-show",
            }
        ),
    )
}

/// What the caller may do to a linked visit when closing an appointment.
#[derive(Debug, Clone)]
pub struct CloseInput {
    pub reason_code: Option<String>,
    pub note: Option<String>,
    pub override_reason: Option<String>,
    /// `true` when the patient/representative acts: cancellation windows
    /// apply and no override is possible.
    pub by_patient: bool,
}

/// Cancel or mark no-show from the appointment side, keeping the linked
/// scheduled visit consistent in the same transaction and opening a
/// cancellation-recovery event when the freed slot is still usable.
pub async fn close_appointment(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ctx: &AuthContext,
    state: &AppState,
    a: &AppointmentRow,
    t: AppointmentTransition,
    input: CloseInput,
    policy: &Policy,
) -> Result<AppointmentRow, ApiError> {
    let next = a
        .status
        .apply(t)
        .map_err(|_| invalid_appointment_transition(a, t))?;
    let now = Utc::now();
    if t == AppointmentTransition::Cancel
        && input.by_patient
        && !wellos_domain::access::within_patient_window(
            now,
            a.starts_at,
            policy.cancellation_window_hours as i64,
        )
    {
        return Err(ApiError::conflict(
            "cancellation_window_closed",
            format!(
                "appointments can be cancelled online up to {} hours before they start; contact the facility",
                policy.cancellation_window_hours
            ),
        ));
    }
    let stamp = match next {
        AppointmentStatus::Cancelled => "cancelled_at = now(), cancelled_by = $5, cancellation_reason = $6, cancellation_note = $7,",
        AppointmentStatus::NoShow => "no_show_at = now(),",
        AppointmentStatus::Fulfilled => "fulfilled_at = now(),",
        _ => "",
    };
    let sql = format!(
        "UPDATE appointments SET status = $1, {stamp} override_reason = COALESCE($8, override_reason),
                version = version + 1, updated_at = now()
         WHERE id = $2 AND version = $3 AND $4::bool"
    );
    let updated = sqlx::query(&sql)
        .bind(next.as_str())
        .bind(a.id)
        .bind(a.version)
        .bind(true)
        .bind(ctx.user_id)
        .bind(&input.reason_code)
        .bind(&input.note)
        .bind(&input.override_reason)
        .execute(&mut **tx)
        .await?;
    if updated.rows_affected() != 1 {
        return Err(stale());
    }
    let actor = actor_label(ctx);
    appointment_history(
        tx,
        a,
        Some(a.status),
        next,
        None,
        input.reason_code.as_deref(),
        input.note.as_deref(),
        input.override_reason.is_some(),
        &actor,
        a.version + 1,
    )
    .await?;
    release_appointment_bookings(tx, a.id).await?;
    notify::cancel_pending_for_appointment(tx, a.tenant_id, a.id).await?;

    // Linked visit follows in the same transaction.
    if let Some(visit_id) = a.visit_id {
        let vt = match next {
            AppointmentStatus::Cancelled => Some(VisitTransition::Cancel),
            AppointmentStatus::NoShow => Some(VisitTransition::MarkNoShow),
            _ => None,
        };
        if let Some(vt) = vt {
            crate::routes::visits::close_for_appointment(
                tx,
                ctx,
                state,
                visit_id,
                vt,
                input.reason_code.as_deref(),
            )
            .await?;
        }
    }
    let event = match next {
        AppointmentStatus::Cancelled => "appointment.cancelled",
        AppointmentStatus::NoShow => "appointment.no_show",
        AppointmentStatus::Fulfilled => "appointment.fulfilled",
        _ => "appointment.transitioned",
    };
    audit::emit(
        &mut **tx,
        ctx,
        event,
        &state.cell,
        json!({
            "appointment_id": a.id,
            "patient_id": a.patient_id,
            "reason_code": input.reason_code,
            "override": input.override_reason.is_some(),
            "by_patient": input.by_patient,
        }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    if input.override_reason.is_some() {
        audit::emit(
            &mut **tx,
            ctx,
            "appointment.overridden",
            &state.cell,
            json!({ "appointment_id": a.id, "stage": t_label(t) }),
            None,
        )
        .await
        .map_err(ApiError::internal)?;
    }
    if next == AppointmentStatus::Cancelled {
        notify::schedule_cancellation(tx, ctx, state, a, policy).await?;
        if a.starts_at > now + Duration::minutes(30) {
            open_cancellation_event(tx, ctx, state, a).await?;
        }
    }
    lock_appointment(tx, a.id).await
}

fn t_label(t: AppointmentTransition) -> &'static str {
    match t {
        AppointmentTransition::Reschedule => "reschedule",
        AppointmentTransition::Cancel => "cancel",
        AppointmentTransition::Fulfil => "fulfil",
        AppointmentTransition::MarkNoShow => "no_show",
    }
}

/// A freed future slot becomes a recovery event for the consented waitlist.
async fn open_cancellation_event(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ctx: &AuthContext,
    state: &AppState,
    a: &AppointmentRow,
) -> Result<Uuid, ApiError> {
    let resources = sqlx::query(
        "SELECT ar.resource_id, ar.role, b.slot_index, b.starts_at, b.ends_at
         FROM appointment_resources ar JOIN resource_bookings b ON b.id = ar.booking_id
         WHERE ar.appointment_id = $1",
    )
    .bind(a.id)
    .fetch_all(&mut **tx)
    .await?
    .iter()
    .map(|r| BookingPlan {
        resource_id: r.get("resource_id"),
        role: r.get("role"),
        slot_index: r.get("slot_index"),
        start: r.get("starts_at"),
        end: r.get("ends_at"),
    })
    .collect::<Vec<_>>();
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO cancellation_events (id, tenant_id, appointment_id, facility_id, service_code,
             modality_code, starts_at, ends_at, resources, status)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,'open')",
    )
    .bind(id)
    .bind(a.tenant_id)
    .bind(a.id)
    .bind(a.facility_id)
    .bind(&a.service_code)
    .bind(&a.modality_code)
    .bind(a.starts_at)
    .bind(a.ends_at)
    .bind(serde_json::to_value(&resources).map_err(ApiError::internal)?)
    .execute(&mut **tx)
    .await?;
    audit::emit(
        &mut **tx,
        ctx,
        "waitlist.recovery.opened",
        &state.cell,
        json!({ "cancellation_event_id": id, "appointment_id": a.id, "starts_at": a.starts_at }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    Ok(id)
}

/// Visit → appointment consistency, called inside visit transitions. Only
/// operational closures move the appointment; arrival leaves it confirmed.
pub async fn on_visit_status(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ctx: &AuthContext,
    state: &AppState,
    visit_id: Uuid,
    status: VisitStatus,
    reason: Option<&str>,
) -> Result<(), ApiError> {
    let t = match status {
        VisitStatus::Cancelled => AppointmentTransition::Cancel,
        VisitStatus::NoShow => AppointmentTransition::MarkNoShow,
        VisitStatus::Completed => AppointmentTransition::Fulfil,
        _ => return Ok(()),
    };
    let appointment_id: Option<Uuid> =
        sqlx::query_scalar("SELECT appointment_id FROM visits WHERE id = $1")
            .bind(visit_id)
            .fetch_optional(&mut **tx)
            .await?
            .flatten();
    let Some(appointment_id) = appointment_id else {
        return Ok(());
    };
    let a = lock_appointment(tx, appointment_id).await?;
    if a.status.apply(t).is_err() {
        return Ok(());
    }
    let policy = load_policy(tx, a.tenant_id).await?;
    // The visit already moved; detach it so `close_appointment` does not
    // transition it a second time.
    let detached = AppointmentRow {
        visit_id: None,
        ..a.clone()
    };
    close_appointment(
        tx,
        ctx,
        state,
        &detached,
        t,
        CloseInput {
            reason_code: reason
                .map(|r| r.to_string())
                .or_else(|| Some("visit_closed".into())),
            note: None,
            override_reason: None,
            by_patient: false,
        },
        &policy,
    )
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Candidate → offer materialization
// ---------------------------------------------------------------------------

/// Persist matcher candidates as offers for the patient/request. Offers
/// carry the full score decomposition and the deterministic reasons; a
/// dMind explanation is attached when one was produced.
#[allow(clippy::too_many_arguments)]
pub async fn materialize_offers(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ctx: &AuthContext,
    tenant_id: Uuid,
    patient_id: Uuid,
    access_request_id: Uuid,
    matcher_run_id: Uuid,
    service_code: &str,
    candidates: &[Candidate],
    explanations: &BTreeMap<String, Value>,
    offered_to: &str,
    ttl_minutes: i32,
) -> Result<Vec<OfferRow>, ApiError> {
    let expires = Utc::now() + Duration::minutes(ttl_minutes as i64);
    let mut out = Vec::with_capacity(candidates.len());
    for (rank, c) in candidates.iter().enumerate() {
        let id = Uuid::now_v7();
        let score = json!({
            "score": c.score,
            "factors": c.factors,
            "travel": c.travel,
            "reasons": c.reasons,
            "supportive_actions": c.supportive_actions,
            "resources": c.resources.iter().map(|r: &CandidateResource| json!({
                "resource_id": r.resource_id, "role": r.role, "slot_index": r.slot_index
            })).collect::<Vec<_>>(),
        });
        sqlx::query(
            "INSERT INTO appointment_offers (id, tenant_id, patient_id, facility_id, access_request_id,
                 matcher_run_id, candidate_id, status, service_code, modality_code, starts_at, ends_at,
                 resources, score, explanation, rank, offered_to, offer_expires_at, created_by)
             VALUES ($1,$2,$3,$4,$5,$6,$7,'offered',$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18)",
        )
        .bind(id)
        .bind(tenant_id)
        .bind(patient_id)
        .bind(c.facility_id)
        .bind(access_request_id)
        .bind(matcher_run_id)
        .bind(&c.candidate_id)
        .bind(service_code)
        .bind(&c.modality_code)
        .bind(c.starts_at)
        .bind(c.ends_at)
        .bind(serde_json::to_value(&c.bookings).map_err(ApiError::internal)?)
        .bind(score)
        .bind(explanations.get(&c.candidate_id))
        .bind(rank as i32 + 1)
        .bind(offered_to)
        .bind(expires)
        .bind(ctx.user_id)
        .execute(&mut **tx)
        .await?;
        offer_history(
            tx,
            tenant_id,
            id,
            None,
            "offered",
            Some("matcher"),
            &actor_label(ctx),
        )
        .await?;
        out.push(load_offer(tx, id).await?);
    }
    Ok(out)
}

pub fn urgency_of(s: &str) -> Urgency {
    Urgency::parse(s).unwrap_or(Urgency::Routine)
}

/// Bounded free text: trimmed, non-empty, at most `max` characters.
pub fn clean_text(
    value: Option<String>,
    field: &str,
    max: usize,
) -> Result<Option<String>, ApiError> {
    match value {
        None => Ok(None),
        Some(s) => {
            let t = s.trim();
            if t.is_empty() {
                return Ok(None);
            }
            if t.chars().count() > max {
                return Err(ApiError::bad_request(
                    "validation_failed",
                    format!("{field} must be at most {max} characters"),
                ));
            }
            Ok(Some(t.to_string()))
        }
    }
}

/// Bounded list of catalog-grammar codes.
pub fn clean_codes(values: Vec<String>, field: &str, max: usize) -> Result<Vec<String>, ApiError> {
    if values.len() > max {
        return Err(ApiError::bad_request(
            "validation_failed",
            format!("{field} accepts at most {max} entries"),
        ));
    }
    let mut out: Vec<String> = Vec::with_capacity(values.len());
    for v in values {
        let v = v.trim().to_string();
        if !is_valid_code(&v) {
            return Err(ApiError::bad_request(
                "validation_failed",
                format!("{field} contains an invalid code '{v}'"),
            ));
        }
        if !out.contains(&v) {
            out.push(v);
        }
    }
    Ok(out)
}

pub fn validate_windows(windows: &[WeeklyWindow], field: &str) -> Result<(), ApiError> {
    if windows.len() > 42 {
        return Err(ApiError::bad_request(
            "validation_failed",
            format!("{field} accepts at most 42 windows"),
        ));
    }
    for w in windows {
        w.validate()
            .map_err(|e| ApiError::bad_request("validation_failed", format!("{field}: {e}")))?;
    }
    Ok(())
}
