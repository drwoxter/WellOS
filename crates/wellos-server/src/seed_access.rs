//! dMind Access synthetic fixtures (compiled only with `dev-fixtures`).
//!
//! Everything here goes through the same transaction-scoped production
//! cores the HTTP routes use (`create_catalog_in`, `create_resource_in`,
//! `create_request_in`, `run_matcher_in`, `accept_offer_in`, `join_for`,
//! `transport::create`, `capacity::create_forecast`, ...) so the seeded
//! state is exactly what the product would have produced. There are no
//! demo shortcuts: catalogs are added at runtime like a tenant
//! administrator would, appointments come from matcher offers, the legacy
//! scheduled visits are linked through `direct_appointment_for_visit`, and
//! the three AI states (ready / disabled / degraded) are real matcher runs
//! against the fake, disabled and unavailable gateways.

use std::sync::Arc;

use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveTime, TimeZone, Utc};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;
use wellos_domain::access::AppointmentTransition;

use crate::auth::{AuthContext, RoleAssignment};
use crate::policy::{roles, Purpose};
use crate::routes::access::{
    self, AcceptInput, ConstraintsInput, CreateRequestBody, InterpretBody, MatchBody,
};
use crate::routes::access_admin::{
    self, CreateCalendarEvent, CreateCatalogEntry, CreateException, CreateResource, Deactivate,
    ReplaceAvailability, ReplaceRequirements, UpdateFacilityScheduling, UpdatePolicy,
};
use crate::routes::grants::{self, CreateGrantBody, GrantRow};
use crate::routes::me::{self, PreferencesBody};
use crate::routes::waitlist::{self, JoinInput};
use crate::runtime::RuntimeConfig;
use crate::scheduling::{self, CloseInput};
use crate::state::AppState;
use crate::state::AuthConfig;
use crate::{capacity, notify, recovery, transport};

type Tx<'a> = Transaction<'a, Postgres>;

/// Identifiers the access fixtures are built on top of.
pub(crate) struct AccessFixtureInput {
    pub tenant: Uuid,
    pub facility: Uuid,
    pub annex: Uuid,
    pub tenant_b: Uuid,
    pub facility_b: Uuid,
    pub admin: Uuid,
    pub registration: Uuid,
    pub dr_garcia: Uuid,
    pub nurse_kim: Uuid,
    pub pharmacist: Option<Uuid>,
    pub alba: Uuid,
    pub anexa: Uuid,
    pub carlos: Uuid,
    pub marta: Uuid,
    pub jonas: Uuid,
    pub sofia: Uuid,
    pub diego: Uuid,
    pub carlos_scheduled_visit: Uuid,
    pub anexa_scheduled_visit: Uuid,
    pub carlos_scheduled_at: DateTime<Utc>,
    pub anexa_scheduled_at: DateTime<Utc>,
}

/// What the post-commit phase needs: the three access requests whose
/// matcher runs exercise the AI-ready, AI-disabled and AI-degraded paths.
pub(crate) struct AccessFixtures {
    pub tenant: Uuid,
    pub staff: Uuid,
    pub facility: Uuid,
    pub ai_ready_request: Uuid,
    pub ai_disabled_request: Uuid,
    pub ai_degraded_request: Uuid,
}

fn body<T: DeserializeOwned>(v: Value) -> anyhow::Result<T> {
    Ok(serde_json::from_value(v)?)
}

/// Staff fixture identity: the same shape the HTTP layer builds after
/// authentication, with tenant-wide administrative and scheduling roles.
fn staff_ctx(tenant: Uuid, user: Uuid, facility: Uuid, purpose: Purpose) -> AuthContext {
    AuthContext {
        user_id: user,
        tenant_id: tenant,
        username: "fixture.scheduler".into(),
        display_name: "Synthetic scheduling fixture".into(),
        is_service: false,
        roles: vec![roles::CLINICAL_ADMIN.into(), roles::REGISTRATION.into()],
        assignments: vec![
            RoleAssignment {
                role: roles::CLINICAL_ADMIN.into(),
                facility_id: None,
            },
            RoleAssignment {
                role: roles::REGISTRATION.into(),
                facility_id: Some(facility),
            },
        ],
        scopes: vec![],
        purpose_of_use: purpose,
        break_glass_reason: None,
        web_session_id: None,
        correlation_id: Uuid::now_v7(),
    }
}

/// Patient/representative fixture identity (self-service paths).
fn rep_ctx(tenant: Uuid, user: Uuid, username: &str) -> AuthContext {
    AuthContext {
        user_id: user,
        tenant_id: tenant,
        username: username.into(),
        display_name: username.into(),
        is_service: false,
        roles: vec![roles::PATIENT_REP.into()],
        assignments: vec![RoleAssignment {
            role: roles::PATIENT_REP.into(),
            facility_id: None,
        }],
        scopes: vec![],
        purpose_of_use: Purpose::Treatment,
        break_glass_reason: None,
        web_session_id: None,
        correlation_id: Uuid::now_v7(),
    }
}

pub(crate) fn fixture_state(pool: PgPool, runtime: &RuntimeConfig) -> AppState {
    AppState::from_runtime(
        pool,
        Arc::new(dmind_gateway::fake::FakeProvider::new()),
        Arc::new(dmind_gateway::scribe::FakeTranscription::new()),
        AuthConfig::development(),
        runtime.clone(),
    )
}

async fn insert_user(
    tx: &mut Tx<'_>,
    tenant: Uuid,
    username: &str,
    display: &str,
    role: &str,
    facility: Option<Uuid>,
) -> anyhow::Result<Uuid> {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO users (id, tenant_id, username, display_name, oidc_subject)
         VALUES ($1,$2,$3,$4,$5)",
    )
    .bind(id)
    .bind(tenant)
    .bind(username)
    .bind(display)
    .bind(format!("synthetic|{username}"))
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "INSERT INTO role_assignments (id, tenant_id, user_id, role, facility_id) VALUES ($1,$2,$3,$4,$5)",
    )
    .bind(Uuid::now_v7())
    .bind(tenant)
    .bind(id)
    .bind(role)
    .bind(facility)
    .execute(&mut **tx)
    .await?;
    Ok(id)
}

#[allow(clippy::too_many_arguments)]
async fn insert_patient(
    tx: &mut Tx<'_>,
    tenant: Uuid,
    facility: Uuid,
    family: &str,
    given: &str,
    birth: &str,
    sex: &str,
    mrn: &str,
) -> anyhow::Result<Uuid> {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO patients (id, tenant_id, facility_id, family_name, given_name, birth_date, sex, identifier)
         VALUES ($1,$2,$3,$4,$5,$6::date,$7,$8)",
    )
    .bind(id)
    .bind(tenant)
    .bind(facility)
    .bind(family)
    .bind(given)
    .bind(birth)
    .bind(sex)
    .bind(mrn)
    .execute(&mut **tx)
    .await?;
    consent(tx, tenant, id, "care_delivery").await?;
    Ok(id)
}

async fn consent(
    tx: &mut Tx<'_>,
    tenant: Uuid,
    patient: Uuid,
    purpose: &str,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO consents (id, tenant_id, patient_id, purpose, status) VALUES ($1,$2,$3,$4,'active')",
    )
    .bind(Uuid::now_v7())
    .bind(tenant)
    .bind(patient)
    .bind(purpose)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Catalog entries added at runtime through the administration core, the
/// way a tenant administrator extends the catalog without a deployment.
/// (kind, code, parent code, EN, ES, synonyms, external codings, config).
type CatalogSpec = (
    &'static str,
    &'static str,
    Option<&'static str>,
    &'static str,
    &'static str,
    &'static [&'static str],
    Value,
    Value,
);

fn service(duration: i32, modalities: &[&str], required: &[&str], extra: Value) -> Value {
    let mut v = json!({
        "duration_minutes": duration,
        "modality_codes": modalities,
        "required_resource_types": required,
    });
    if let (Some(base), Some(more)) = (v.as_object_mut(), extra.as_object()) {
        for (k, val) in more {
            if k == "preparation" {
                base.insert("preparation_en".into(), val["en"].clone());
                base.insert("preparation_es".into(), val["es"].clone());
            } else {
                base.insert(k.clone(), val.clone());
            }
        }
    }
    v
}

fn catalog_specs() -> Vec<CatalogSpec> {
    let snomed = |code: &str, display: &str| json!([{ "system": "http://snomed.info/sct", "code": code, "display": display }]);
    vec![
        // --- specialties (hierarchy) ---------------------------------------
        (
            "specialty",
            "family_medicine",
            None,
            "Family medicine",
            "Medicina familiar",
            &["primary care", "atención primaria"],
            snomed("419772000", "Family practice"),
            json!({}),
        ),
        (
            "specialty",
            "internal_medicine",
            None,
            "Internal medicine",
            "Medicina interna",
            &[],
            snomed("419192003", "Internal medicine"),
            json!({}),
        ),
        (
            "specialty",
            "cardiology",
            Some("internal_medicine"),
            "Cardiology",
            "Cardiología",
            &["heart"],
            snomed("394579002", "Cardiology"),
            json!({}),
        ),
        (
            "specialty",
            "general_surgery",
            None,
            "General surgery",
            "Cirugía general",
            &[],
            snomed("394609007", "General surgery"),
            json!({}),
        ),
        (
            "specialty",
            "orthopaedics",
            Some("general_surgery"),
            "Orthopaedic surgery",
            "Cirugía ortopédica y traumatología",
            &["trauma"],
            snomed("394801008", "Trauma and orthopedics"),
            json!({}),
        ),
        (
            "specialty",
            "pediatrics",
            None,
            "Pediatrics",
            "Pediatría",
            &["paediatrics", "niños"],
            snomed("394537008", "Pediatric specialty"),
            json!({}),
        ),
        (
            "specialty",
            "obstetrics_gynecology",
            None,
            "Obstetrics and gynecology",
            "Obstetricia y ginecología",
            &["ob/gyn", "ginecología"],
            snomed("394585009", "Obstetrics and gynecology"),
            json!({}),
        ),
        (
            "specialty",
            "psychiatry",
            None,
            "Psychiatry",
            "Psiquiatría",
            &["mental health"],
            snomed("394587001", "Psychiatry"),
            json!({}),
        ),
        (
            "specialty",
            "clinical_psychology",
            None,
            "Clinical psychology",
            "Psicología clínica",
            &["psychology"],
            snomed("394913002", "Psychotherapy"),
            json!({}),
        ),
        (
            "specialty",
            "dentistry",
            None,
            "Dentistry",
            "Odontología",
            &["oral care", "dental"],
            snomed("394812008", "Dental medicine specialties"),
            json!({}),
        ),
        (
            "specialty",
            "emergency_medicine",
            None,
            "Emergency medicine",
            "Medicina de urgencias",
            &["A&E"],
            snomed("773568002", "Emergency medicine"),
            json!({}),
        ),
        (
            "specialty",
            "radiology",
            None,
            "Diagnostic radiology",
            "Radiodiagnóstico",
            &["imaging"],
            snomed("394914008", "Radiology"),
            json!({}),
        ),
        (
            "specialty",
            "clinical_laboratory",
            None,
            "Clinical laboratory",
            "Análisis clínicos",
            &[],
            snomed("708184003", "Clinical laboratory"),
            json!({}),
        ),
        (
            "specialty",
            "rehabilitation_medicine",
            None,
            "Rehabilitation medicine",
            "Medicina física y rehabilitación",
            &[],
            snomed("394602003", "Rehabilitation"),
            json!({}),
        ),
        // --- professions ---------------------------------------------------
        (
            "profession",
            "physician",
            None,
            "Physician",
            "Médico/a",
            &["doctor"],
            snomed("309343006", "Physician"),
            json!({}),
        ),
        (
            "profession",
            "nurse",
            None,
            "Nurse",
            "Enfermero/a",
            &[],
            snomed("224535009", "Registered nurse"),
            json!({}),
        ),
        (
            "profession",
            "midwife",
            Some("nurse"),
            "Midwife",
            "Matrona",
            &[],
            snomed("309453006", "Midwife"),
            json!({}),
        ),
        (
            "profession",
            "psychologist",
            None,
            "Psychologist",
            "Psicólogo/a",
            &[],
            snomed("59944000", "Psychologist"),
            json!({}),
        ),
        (
            "profession",
            "dentist",
            None,
            "Dentist",
            "Dentista",
            &["odontólogo"],
            snomed("106289002", "Dentist"),
            json!({}),
        ),
        (
            "profession",
            "dental_hygienist",
            Some("dentist"),
            "Dental hygienist",
            "Higienista dental",
            &[],
            json!([]),
            json!({}),
        ),
        (
            "profession",
            "physiotherapist",
            None,
            "Physiotherapist",
            "Fisioterapeuta",
            &["physical therapist"],
            snomed("36682004", "Physiotherapist"),
            json!({}),
        ),
        (
            "profession",
            "occupational_therapist",
            None,
            "Occupational therapist",
            "Terapeuta ocupacional",
            &[],
            snomed("80546007", "Occupational therapist"),
            json!({}),
        ),
        (
            "profession",
            "pharmacist",
            None,
            "Pharmacist",
            "Farmacéutico/a",
            &[],
            snomed("46255001", "Pharmacist"),
            json!({}),
        ),
        (
            "profession",
            "laboratory_technician",
            None,
            "Laboratory technician",
            "Técnico/a de laboratorio",
            &["phlebotomist"],
            snomed("159282002", "Medical laboratory technician"),
            json!({}),
        ),
        (
            "profession",
            "radiographer",
            None,
            "Radiographer",
            "Técnico/a en imagen para el diagnóstico",
            &[],
            snomed("3430008", "Radiographer"),
            json!({}),
        ),
        (
            "profession",
            "paramedic",
            None,
            "Paramedic",
            "Técnico/a en emergencias sanitarias",
            &["EMT"],
            snomed("397897005", "Paramedic"),
            json!({}),
        ),
        (
            "profession",
            "rescue_technician",
            Some("paramedic"),
            "Rescue technician",
            "Técnico/a de rescate",
            &["rescuer"],
            json!([]),
            json!({}),
        ),
        (
            "profession",
            "patient_transport_driver",
            None,
            "Patient transport driver",
            "Conductor/a de transporte sanitario",
            &[],
            json!([]),
            json!({}),
        ),
        (
            "profession",
            "home_care_aide",
            Some("nurse"),
            "Home care aide",
            "Auxiliar de atención domiciliaria",
            &[],
            json!([]),
            json!({}),
        ),
        // --- resource types ------------------------------------------------
        (
            "resource_type",
            "dental_chair",
            None,
            "Dental chair",
            "Sillón dental",
            &[],
            json!([]),
            json!({}),
        ),
        (
            "resource_type",
            "procedure_room",
            None,
            "Procedure room",
            "Sala de procedimientos",
            &["operating room"],
            json!([]),
            json!({}),
        ),
        (
            "resource_type",
            "lab_station",
            None,
            "Laboratory station",
            "Puesto de laboratorio",
            &["phlebotomy station"],
            json!([]),
            json!({}),
        ),
        (
            "resource_type",
            "imaging_equipment",
            None,
            "Imaging equipment",
            "Equipo de imagen",
            &["x-ray", "ultrasound"],
            json!([]),
            json!({}),
        ),
        (
            "resource_type",
            "rehabilitation_space",
            None,
            "Rehabilitation space",
            "Espacio de rehabilitación",
            &["gym"],
            json!([]),
            json!({}),
        ),
        (
            "resource_type",
            "home_visit_team",
            None,
            "Home-visit team",
            "Equipo de atención domiciliaria",
            &[],
            json!([]),
            json!({}),
        ),
        (
            "resource_type",
            "emergency_response_unit",
            None,
            "Emergency response unit",
            "Unidad de respuesta a emergencias",
            &["rescue unit"],
            json!([]),
            json!({}),
        ),
        // --- accessibility capabilities ------------------------------------
        (
            "accessibility_capability",
            "wheelchair_access",
            None,
            "Wheelchair access",
            "Acceso en silla de ruedas",
            &["step-free"],
            json!([]),
            json!({}),
        ),
        (
            "accessibility_capability",
            "hearing_loop",
            None,
            "Hearing loop",
            "Bucle magnético",
            &[],
            json!([]),
            json!({}),
        ),
        (
            "accessibility_capability",
            "sign_language",
            None,
            "Sign-language support",
            "Apoyo en lengua de signos",
            &[],
            json!([]),
            json!({}),
        ),
        (
            "accessibility_capability",
            "stretcher",
            None,
            "Stretcher transfer",
            "Traslado en camilla",
            &[],
            json!([]),
            json!({}),
        ),
        // --- locations / service areas -------------------------------------
        (
            "location",
            "area_north",
            None,
            "North service area",
            "Área de servicio norte",
            &[],
            json!([]),
            json!({"service_radius_km": 12}),
        ),
        (
            "location",
            "area_north_coast",
            Some("area_north"),
            "North coast",
            "Costa norte",
            &[],
            json!([]),
            json!({}),
        ),
        (
            "location",
            "area_island_interior",
            None,
            "Island interior",
            "Interior de la isla",
            &["rural"],
            json!([]),
            json!({"service_radius_km": 35}),
        ),
        // --- transport / emergency resources --------------------------------
        (
            "transport_resource",
            "patient_transport_van",
            None,
            "Patient transport van",
            "Furgoneta de transporte sanitario",
            &[],
            json!([]),
            json!({"resource_type_code": "vehicle"}),
        ),
        (
            "transport_resource",
            "wheelchair_accessible_van",
            None,
            "Wheelchair-accessible van",
            "Furgoneta adaptada",
            &[],
            json!([]),
            json!({"resource_type_code": "accessible_vehicle"}),
        ),
        (
            "transport_resource",
            "basic_life_support_ambulance",
            None,
            "Basic life-support ambulance",
            "Ambulancia de soporte vital básico",
            &["BLS"],
            json!([]),
            json!({"resource_type_code": "ambulance", "emergency": true}),
        ),
        (
            "transport_resource",
            "advanced_life_support_ambulance",
            None,
            "Advanced life-support ambulance",
            "Ambulancia de soporte vital avanzado",
            &["ALS", "UVI móvil"],
            json!([]),
            json!({"resource_type_code": "ambulance", "emergency": true}),
        ),
        (
            "transport_resource",
            "rescue_unit",
            None,
            "Rescue unit",
            "Unidad de rescate",
            &[],
            json!([]),
            json!({"resource_type_code": "emergency_response_unit", "emergency": true}),
        ),
        // --- clinical services ---------------------------------------------
        (
            "clinical_service",
            "family_medicine_review",
            Some("general_medicine"),
            "Family medicine review",
            "Revisión de medicina familiar",
            &["GP review"],
            json!([]),
            service(
                20,
                &["in_person", "telehealth"],
                &["professional"],
                json!({"specialty_code": "family_medicine"}),
            ),
        ),
        (
            "clinical_service",
            "cardiology_consult",
            None,
            "Cardiology consultation",
            "Consulta de cardiología",
            &["heart clinic"],
            snomed("718340009", "Cardiology consultation"),
            service(
                30,
                &["in_person"],
                &["professional"],
                json!({"specialty_code": "cardiology", "requires_referral": true, "preparation": {"en": "Bring your current medication list and any previous ECG.", "es": "Traiga su lista de medicación actual y cualquier ECG previo."}}),
            ),
        ),
        (
            "clinical_service",
            "orthopaedic_consult",
            None,
            "Orthopaedic consultation",
            "Consulta de traumatología",
            &[],
            json!([]),
            service(
                20,
                &["in_person"],
                &["professional"],
                json!({"specialty_code": "orthopaedics", "requires_referral": true}),
            ),
        ),
        (
            "clinical_service",
            "pediatric_consult",
            None,
            "Pediatric consultation",
            "Consulta de pediatría",
            &["child check-up"],
            json!([]),
            service(
                20,
                &["in_person", "telehealth"],
                &["professional"],
                json!({"specialty_code": "pediatrics", "max_age_years": 15}),
            ),
        ),
        (
            "clinical_service",
            "antenatal_visit",
            None,
            "Antenatal visit",
            "Visita prenatal",
            &["pregnancy check"],
            json!([]),
            service(
                30,
                &["in_person"],
                &["professional"],
                json!({"specialty_code": "obstetrics_gynecology", "preparation": {"en": "Bring your pregnancy record book.", "es": "Traiga su cartilla de embarazo."}}),
            ),
        ),
        (
            "clinical_service",
            "psychology_session",
            None,
            "Psychology session",
            "Sesión de psicología",
            &["therapy"],
            json!([]),
            service(
                50,
                &["in_person", "telehealth"],
                &["professional"],
                json!({"specialty_code": "clinical_psychology", "min_age_years": 16}),
            ),
        ),
        (
            "clinical_service",
            "dental_checkup",
            None,
            "Dental check-up",
            "Revisión dental",
            &["dental review"],
            json!([]),
            service(
                30,
                &["in_person"],
                &["professional", "dental_chair"],
                json!({"specialty_code": "dentistry", "preparation": {"en": "Brush your teeth before the visit.", "es": "Cepíllese los dientes antes de la visita."}}),
            ),
        ),
        (
            "clinical_service",
            "dental_hygiene",
            Some("dental_checkup"),
            "Dental hygiene",
            "Higiene dental",
            &["cleaning"],
            json!([]),
            service(
                45,
                &["in_person"],
                &["professional", "dental_chair"],
                json!({"specialty_code": "dentistry"}),
            ),
        ),
        (
            "clinical_service",
            "physio_session",
            None,
            "Physiotherapy session",
            "Sesión de fisioterapia",
            &["physio", "rehab"],
            json!([]),
            service(
                45,
                &["in_person"],
                &["professional", "rehabilitation_space"],
                json!({"specialty_code": "rehabilitation_medicine", "preparation": {"en": "Wear comfortable clothing.", "es": "Lleve ropa cómoda."}}),
            ),
        ),
        (
            "clinical_service",
            "medication_review",
            None,
            "Pharmacist medication review",
            "Revisión farmacoterapéutica",
            &["pharmacy review"],
            json!([]),
            service(
                20,
                &["in_person", "telehealth"],
                &["professional"],
                json!({}),
            ),
        ),
        (
            "clinical_service",
            "blood_draw",
            None,
            "Blood sample collection",
            "Extracción de sangre",
            &["phlebotomy", "analítica"],
            snomed("396540005", "Phlebotomy"),
            service(
                10,
                &["in_person"],
                &["lab_station"],
                json!({"preparation": {"en": "Fast for 8 hours; water is allowed.", "es": "Ayuno de 8 horas; puede beber agua."}}),
            ),
        ),
        (
            "clinical_service",
            "xray_exam",
            None,
            "X-ray examination",
            "Radiografía",
            &["imaging"],
            json!([]),
            service(
                15,
                &["in_person"],
                &["imaging_equipment", "professional"],
                json!({"requires_referral": true, "preparation": {"en": "Remove metal objects before the exam.", "es": "Retire los objetos metálicos antes de la prueba."}}),
            ),
        ),
        (
            "clinical_service",
            "home_nursing_visit",
            Some("nursing"),
            "Home nursing visit",
            "Visita de enfermería a domicilio",
            &["home care"],
            json!([]),
            service(45, &["home_visit"], &["home_visit_team"], json!({})),
        ),
        (
            "clinical_service",
            "midwife_visit",
            None,
            "Midwife visit",
            "Visita con matrona",
            &[],
            json!([]),
            service(
                30,
                &["in_person", "home_visit"],
                &["professional"],
                json!({"specialty_code": "obstetrics_gynecology"}),
            ),
        ),
        (
            "clinical_service",
            "emergency_transport",
            None,
            "Emergency transport",
            "Transporte de emergencia",
            &["ambulance"],
            json!([]),
            service(
                60,
                &["in_person"],
                &["ambulance"],
                json!({"emergency": true, "human_dispatch_required": true}),
            ),
        ),
    ]
}

async fn install_catalogs(
    tx: &mut Tx<'_>,
    ctx: &AuthContext,
    state: &AppState,
    facilities: &[Uuid],
) -> anyhow::Result<()> {
    let mut ids: std::collections::HashMap<(&str, &str), Uuid> = Default::default();
    for (kind, code, parent, en, es, synonyms, codings, config) in catalog_specs() {
        let parent_id = match parent {
            None => None,
            Some(p) => match ids.get(&(kind, p)) {
                Some(id) => Some(*id),
                None => sqlx::query_scalar::<_, Uuid>(
                    "SELECT id FROM catalog_entries WHERE tenant_id = $1 AND kind = $2 AND code = $3",
                )
                .bind(ctx.tenant_id)
                .bind(kind)
                .bind(p)
                .fetch_optional(&mut **tx)
                .await?,
            },
        };
        let entry: CreateCatalogEntry = body(json!({
            "kind": kind,
            "code": code,
            "parent_id": parent_id,
            "name_en": en,
            "name_es": es,
            "synonyms": synonyms,
            "external_codings": codings,
            "config": config,
            "facility_ids": facilities,
            "change_reason": "synthetic fixture catalog",
        }))?;
        let created = access_admin::create_catalog_in(tx, ctx, state, entry).await?;
        if let Some(id) = created.get("id").and_then(Value::as_str) {
            ids.insert((kind, code), id.parse()?);
        }
    }
    // A retired entry proves the non-destructive lifecycle: still visible
    // in history, no longer offered.
    let retired = ids[&("profession", "rescue_technician")];
    access_admin::deactivate_catalog_in(
        tx,
        ctx,
        state,
        retired,
        Deactivate {
            version: 1,
            change_reason: Some("Role merged into paramedic (synthetic)".into()),
        },
    )
    .await?;
    Ok(())
}

struct ResourceSpec<'a> {
    facility: Uuid,
    rtype: &'a str,
    name: &'a str,
    user: Option<Uuid>,
    profession: Option<&'a str>,
    specialties: &'a [&'a str],
    languages: &'a [&'a str],
    accessibility: &'a [&'a str],
    capacity: i32,
    time_zone: &'a str,
    services: Vec<Value>,
    /// (weekday, start, end, kind, capacity)
    rules: Vec<(u8, &'a str, &'a str, &'a str, Option<i32>)>,
}

fn svc(code: &str) -> Value {
    json!({ "service_code": code, "modality_codes": [] })
}

fn svc_mod(code: &str, modalities: &[&str]) -> Value {
    json!({ "service_code": code, "modality_codes": modalities })
}

fn weekdays<'a>(
    days: &[u8],
    start: &'a str,
    end: &'a str,
    capacity: Option<i32>,
) -> Vec<(u8, &'a str, &'a str, &'a str, Option<i32>)> {
    days.iter()
        .map(|d| (*d, start, end, "available", capacity))
        .collect()
}

async fn create_resource(
    tx: &mut Tx<'_>,
    ctx: &AuthContext,
    state: &AppState,
    spec: ResourceSpec<'_>,
) -> anyhow::Result<Uuid> {
    let b: CreateResource = body(json!({
        "facility_id": spec.facility,
        "resource_type_code": spec.rtype,
        "name": spec.name,
        "user_id": spec.user,
        "profession_code": spec.profession,
        "specialty_codes": spec.specialties,
        "languages": spec.languages,
        "accessibility_codes": spec.accessibility,
        "capacity": spec.capacity,
        "time_zone": spec.time_zone,
        "metadata": { "synthetic": true },
        "services": spec.services,
    }))?;
    let created = access_admin::create_resource_in(tx, ctx, state, b).await?;
    let id: Uuid = created
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("resource id missing"))?
        .parse()?;
    if !spec.rules.is_empty() {
        let rules: Vec<Value> = spec
            .rules
            .iter()
            .map(|(d, s, e, kind, cap)| {
                json!({ "weekday": d, "start_local": s, "end_local": e, "kind": kind, "capacity": cap })
            })
            .collect();
        let avail: ReplaceAvailability = body(json!({ "version": 1, "rules": rules }))?;
        access_admin::replace_availability_in(tx, ctx, state, id, avail).await?;
    }
    Ok(id)
}

#[allow(clippy::too_many_arguments)]
async fn exception(
    tx: &mut Tx<'_>,
    ctx: &AuthContext,
    state: &AppState,
    resource: Uuid,
    kind: &str,
    starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>,
    reason: &str,
    capacity_delta: Option<i32>,
) -> anyhow::Result<()> {
    let e: CreateException = body(json!({
        "kind": kind,
        "starts_at": starts_at,
        "ends_at": ends_at,
        "capacity_delta": capacity_delta,
        "reason_code": reason,
    }))?;
    access_admin::create_exception_in(tx, ctx, state, resource, e).await?;
    Ok(())
}

fn local(tz: chrono_tz::Tz, date: NaiveDate, h: u32, m: u32) -> DateTime<Utc> {
    tz.from_local_datetime(&date.and_time(NaiveTime::from_hms_opt(h, m, 0).expect("valid time")))
        .single()
        .or_else(|| {
            tz.from_local_datetime(
                &date.and_time(NaiveTime::from_hms_opt(h + 1, m, 0).expect("valid time")),
            )
            .single()
        })
        .expect("local time resolves")
        .with_timezone(&Utc)
}

/// Next occurrence of an ISO weekday strictly after `from`.
fn next_weekday(from: NaiveDate, weekday: u32) -> NaiveDate {
    let mut d = from + Duration::days(1);
    while d.weekday().number_from_monday() != weekday {
        d += Duration::days(1);
    }
    d
}

/// Historical appointments the capacity model learns from: a realistic
/// weekday demand pattern over the last twelve weeks with cancellations
/// (and their lead time), no-shows and a visible summer/holiday bump.
/// These are legacy records (`booked_via = 'migration'`) — the product has
/// no path that books into the past, so they are the one place the fixture
/// writes appointments directly.
#[allow(clippy::too_many_arguments)]
async fn seed_history(
    tx: &mut Tx<'_>,
    tenant: Uuid,
    facility: Uuid,
    tz: chrono_tz::Tz,
    service_code: &str,
    resource: Uuid,
    patients: &[Uuid],
    booked_by: Uuid,
    today: NaiveDate,
) -> anyhow::Result<()> {
    let mut n = 0usize;
    for back in 1..=84i64 {
        let day = today - Duration::days(back);
        let wd = day.weekday().number_from_monday();
        if wd > 5 {
            continue;
        }
        // Mondays and Fridays are busier; August-like weeks (the 4 weeks
        // ending 8 weeks ago) carry seasonal pressure.
        let mut demand = match wd {
            1 | 5 => 6,
            _ => 4,
        };
        if (56..84).contains(&back) {
            demand += 3;
        }
        for k in 0..demand {
            let starts = local(tz, day, 9 + (k as u32 % 5), if k % 2 == 0 { 0 } else { 30 });
            let ends = starts + Duration::minutes(20);
            let (status, cancelled_at, fulfilled_at, no_show_at) = match n % 9 {
                7 => (
                    "cancelled",
                    Some(starts - Duration::hours(if n.is_multiple_of(2) { 30 } else { 4 })),
                    None,
                    None,
                ),
                8 => ("no_show", None, None, Some(ends)),
                _ => ("fulfilled", None, Some(ends), None),
            };
            let id = Uuid::now_v7();
            sqlx::query(
                "INSERT INTO appointments (id, tenant_id, facility_id, patient_id, service_code, modality_code,
                     status, starts_at, ends_at, time_zone, reason, primary_resource_id, booked_via, booked_by,
                     cancellation_reason, cancelled_by, cancelled_at, fulfilled_at, no_show_at, created_at, updated_at)
                 VALUES ($1,$2,$3,$4,$5,'in_person',$6,$7,$8,$9,'synthetic history',$10,'migration',$11,
                     $12,$13,$14,$15,$16,$17,$17)",
            )
            .bind(id)
            .bind(tenant)
            .bind(facility)
            .bind(patients[n % patients.len()])
            .bind(service_code)
            .bind(status)
            .bind(starts)
            .bind(ends)
            .bind(tz.name())
            .bind(resource)
            .bind(booked_by)
            .bind(cancelled_at.map(|_| "patient_request"))
            .bind(cancelled_at.map(|_| booked_by))
            .bind(cancelled_at)
            .bind(fulfilled_at)
            .bind(no_show_at)
            .bind(starts - Duration::days(7))
            .execute(&mut **tx)
            .await?;
            sqlx::query(
                "INSERT INTO appointment_history (id, tenant_id, appointment_id, from_status, to_status, starts_at_after, actor, version, recorded_at)
                 VALUES ($1,$2,$3,'confirmed',$4,$5,'migration:synthetic-history',1,$6)",
            )
            .bind(Uuid::now_v7())
            .bind(tenant)
            .bind(id)
            .bind(status)
            .bind(starts)
            .bind(cancelled_at.or(fulfilled_at).or(no_show_at).unwrap_or(ends))
            .execute(&mut **tx)
            .await?;
            n += 1;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn grant(
    tx: &mut Tx<'_>,
    ctx: &AuthContext,
    state: &AppState,
    user: Uuid,
    patient: Uuid,
    relationship: &str,
    expires_at: Option<DateTime<Utc>>,
    note: &str,
) -> anyhow::Result<GrantRow> {
    let b: CreateGrantBody = body(json!({
        "user_id": user,
        "patient_id": patient,
        "relationship": relationship,
        "expires_at": expires_at,
    }))?;
    Ok(grants::create_grant_in(tx, ctx, state, b, note.to_string()).await?)
}

struct Request<'a> {
    patient: Uuid,
    facility: Option<Uuid>,
    free_text: Option<&'a str>,
    constraints: Value,
    urgency: Option<&'a str>,
    channel: &'a str,
}

async fn request(
    tx: &mut Tx<'_>,
    ctx: &AuthContext,
    state: &AppState,
    r: Request<'_>,
) -> anyhow::Result<access::RequestRow> {
    let b: CreateRequestBody = body(json!({
        "patient_id": r.patient,
        "facility_id": r.facility,
        "free_text": r.free_text,
        "constraints": r.constraints,
        "urgency": r.urgency,
        "submit": true,
        "idempotency_key": Uuid::now_v7().to_string(),
    }))?;
    Ok(access::create_request_in(tx, ctx, state, r.patient, r.channel, b).await?)
}

async fn match_and_book(
    tx: &mut Tx<'_>,
    ctx: &AuthContext,
    state: &AppState,
    r: &access::RequestRow,
    offered_to: &str,
    booked_via: &str,
    pick: usize,
) -> anyhow::Result<Option<scheduling::AppointmentRow>> {
    let run =
        access::run_matcher_in(tx, ctx, state, r.tenant_id, r.id, None, None, offered_to).await?;
    let Some(offer) = run.offers.get(pick).or_else(|| run.offers.first()) else {
        tracing::warn!(request = %r.id, rejected = ?run.output.rejected, "fixture matcher produced no candidates");
        return Ok(None);
    };
    let a = access::accept_offer_in(
        tx,
        ctx,
        state,
        AcceptInput {
            offer_id: offer.id,
            version: None,
            reason: None,
            override_reason: None,
            idempotency_key: Some(Uuid::now_v7().to_string()),
            reschedule_of: None,
            reschedule_reason: None,
            booked_via,
        },
    )
    .await?;
    Ok(Some(a))
}

/// Build the whole access fixture inside the seed transaction.
pub(crate) async fn seed(
    tx: &mut Tx<'_>,
    state: &AppState,
    input: AccessFixtureInput,
) -> anyhow::Result<AccessFixtures> {
    let AccessFixtureInput {
        tenant,
        facility,
        annex,
        tenant_b,
        facility_b,
        admin,
        registration,
        dr_garcia,
        nurse_kim,
        pharmacist,
        alba,
        anexa,
        carlos,
        marta,
        jonas,
        sofia,
        diego,
        carlos_scheduled_visit,
        anexa_scheduled_visit,
        carlos_scheduled_at,
        anexa_scheduled_at,
    } = input;
    let now = Utc::now();
    let madrid: chrono_tz::Tz = chrono_tz::Europe::Madrid;
    let today = now.with_timezone(&madrid).date_naive();
    let admin_ctx = staff_ctx(tenant, admin, facility, Purpose::Operations);
    let sched = staff_ctx(tenant, registration, facility, Purpose::Treatment);

    // ------------------------------------------------------------------
    // 1. Tenant policy, facility scheduling (time zones, hours, coordinates)
    // ------------------------------------------------------------------
    let policy: UpdatePolicy = body(json!({
        "version": 1,
        "time_zone": "Europe/Madrid",
        "hold_minutes": 15,
        "offer_ttl_minutes": 240,
        "cancellation_window_hours": 24,
        "reschedule_window_hours": 24,
        "min_notice_hours": 2,
        "horizon_days": 60,
        "patient_confirmation_required": true,
        "confirmation_deadline_hours": 48,
        "reminder_lead_hours": [48, 3],
        "quiet_hours_start": "22:00:00",
        "quiet_hours_end": "08:00:00",
        "max_candidates": 12,
    }))?;
    access_admin::update_policy_in(tx, &admin_ctx, state, policy).await?;
    let hours = |open: &str, close: &str| -> Vec<Value> {
        (1..=5u8)
            .map(|d| json!({ "weekday": d, "open": open, "close": close }))
            .collect()
    };
    let main_sched: UpdateFacilityScheduling = body(json!({
        "version": 1,
        "time_zone": "Europe/Madrid",
        "opening_hours": hours("08:00:00", "20:00:00"),
        "latitude": 38.9089,
        "longitude": 1.4324,
        "service_radius_km": 25,
        "address_line": "Synthetic Hospital Demo Norte, Avenida Demo 1",
    }))?;
    access_admin::update_facility_scheduling_in(tx, &admin_ctx, state, facility, main_sched)
        .await?;
    let annex_sched: UpdateFacilityScheduling = body(json!({
        "version": 1,
        "time_zone": "Atlantic/Canary",
        "opening_hours": hours("09:00:00", "17:00:00"),
        "latitude": 28.4636,
        "longitude": -16.2518,
        "service_radius_km": 40,
        "address_line": "Synthetic North Annex, Calle Demo 22",
    }))?;
    access_admin::update_facility_scheduling_in(tx, &admin_ctx, state, annex, annex_sched).await?;

    // ------------------------------------------------------------------
    // 2. Catalogs added at runtime + service resource requirements
    // ------------------------------------------------------------------
    install_catalogs(tx, &admin_ctx, state, &[facility, annex]).await?;
    for (service_code, reqs) in [
        (
            "dental_checkup",
            json!([{ "resource_type_code": "dental_chair", "quantity": 1 }]),
        ),
        (
            "dental_hygiene",
            json!([{ "resource_type_code": "dental_chair", "quantity": 1 }]),
        ),
        (
            "physio_session",
            json!([{ "resource_type_code": "rehabilitation_space", "quantity": 1 }]),
        ),
        (
            "xray_exam",
            json!([{ "resource_type_code": "professional", "quantity": 1 }]),
        ),
    ] {
        let r: ReplaceRequirements = body(json!({ "requirements": reqs }))?;
        access_admin::replace_requirements_in(tx, &admin_ctx, state, service_code, r).await?;
    }
    // Tenant B gets the baseline only: proves catalogs are tenant-owned.
    let _ = (tenant_b, facility_b);

    // ------------------------------------------------------------------
    // 3. Staff accounts the fixtures need (transport, representatives)
    // ------------------------------------------------------------------
    let transport_ruiz = insert_user(
        tx,
        tenant,
        "transport.ruiz",
        "Tomás Ruiz (Transport coordinator)",
        roles::TRANSPORT_COORDINATOR,
        Some(facility),
    )
    .await?;
    let rep_alba = insert_user(
        tx,
        tenant,
        "rep.alba",
        "Alba Demopatient (Patient)",
        roles::PATIENT_REP,
        None,
    )
    .await?;
    let rep_ortiz = insert_user(
        tx,
        tenant,
        "rep.ortiz",
        "Lucía Ortiz (Parent / guardian)",
        roles::PATIENT_REP,
        None,
    )
    .await?;
    let rep_sofia = insert_user(
        tx,
        tenant,
        "rep.sofia",
        "Sofía Demopatient (Patient)",
        roles::PATIENT_REP,
        None,
    )
    .await?;

    // Extra synthetic patients: a child (pediatrics, managed by a parent),
    // the parent's elderly father (second dependant, wheelchair user), a
    // pregnant patient and a Canary-resident telehealth patient.
    let leo = insert_patient(
        tx,
        tenant,
        facility,
        "Ortiz",
        "Leo",
        "2020-03-14",
        "male",
        "SYN-0201",
    )
    .await?;
    let abuelo = insert_patient(
        tx,
        tenant,
        facility,
        "Ortiz",
        "Ramón",
        "1941-09-02",
        "male",
        "SYN-0202",
    )
    .await?;
    let nadia = insert_patient(
        tx,
        tenant,
        facility,
        "Demopatient",
        "Nadia",
        "1993-05-30",
        "female",
        "SYN-0203",
    )
    .await?;
    let teo = insert_patient(
        tx,
        tenant,
        annex,
        "Demopatient",
        "Teo",
        "1979-12-12",
        "male",
        "SYN-0204",
    )
    .await?;
    for (p, purposes) in [
        (
            alba,
            &[
                "scheduling_calendar",
                "waitlist_offers",
                "scheduling_location",
                "transport_coordination",
            ][..],
        ),
        (sofia, &["waitlist_offers", "scheduling_calendar"][..]),
        (diego, &["waitlist_offers"][..]),
        (
            carlos,
            &[
                "waitlist_offers",
                "transport_coordination",
                "scheduling_location",
            ][..],
        ),
        (marta, &["waitlist_offers"][..]),
        (
            jonas,
            &["transport_coordination", "scheduling_location"][..],
        ),
        (leo, &["waitlist_offers"][..]),
        (
            abuelo,
            &[
                "transport_coordination",
                "scheduling_location",
                "waitlist_offers",
            ][..],
        ),
        (nadia, &["scheduling_calendar"][..]),
        (teo, &["scheduling_calendar"][..]),
    ] {
        for purpose in purposes {
            consent(tx, tenant, p, purpose).await?;
        }
    }

    // ------------------------------------------------------------------
    // 4. Schedulable resources, availability, breaks, exceptions
    // ------------------------------------------------------------------
    let mz = "Europe/Madrid";
    let cz = "Atlantic/Canary";
    let mut garcia_rules = weekdays(&[1, 2, 3, 4, 5], "09:00:00", "14:00:00", None);
    garcia_rules.push((1, "11:00:00", "11:30:00", "break", None));
    garcia_rules.push((3, "11:00:00", "11:30:00", "break", None));
    let garcia = create_resource(
        tx,
        &admin_ctx,
        state,
        ResourceSpec {
            facility,
            rtype: "professional",
            name: "Dr. Ana García (Family medicine)",
            user: Some(dr_garcia),
            profession: Some("physician"),
            specialties: &["family_medicine"],
            languages: &["es", "en"],
            accessibility: &["wheelchair_access", "hearing_loop"],
            capacity: 1,
            time_zone: mz,
            services: vec![
                svc("general_medicine"),
                svc("family_medicine_review"),
                svc_mod("telehealth", &["telehealth"]),
            ],
            rules: garcia_rules,
        },
    )
    .await?;
    let ivan_user = insert_user(
        tx,
        tenant,
        "dr.serra",
        "Dr. Iván Serra (Cardiology)",
        roles::PHYSICIAN,
        Some(facility),
    )
    .await?;
    let ivan = create_resource(
        tx,
        &admin_ctx,
        state,
        ResourceSpec {
            facility,
            rtype: "professional",
            name: "Dr. Iván Serra (Cardiology)",
            user: Some(ivan_user),
            profession: Some("physician"),
            specialties: &["internal_medicine", "cardiology"],
            languages: &["es"],
            accessibility: &["wheelchair_access"],
            capacity: 1,
            time_zone: mz,
            services: vec![svc("cardiology_consult"), svc("general_medicine")],
            rules: weekdays(&[1, 3, 5], "15:00:00", "19:00:00", None),
        },
    )
    .await?;
    let kim = create_resource(
        tx,
        &admin_ctx,
        state,
        ResourceSpec {
            facility,
            rtype: "professional",
            name: "Nurse Joon Kim (Nursing)",
            user: Some(nurse_kim),
            profession: Some("nurse"),
            specialties: &[],
            languages: &["en", "es", "ko"],
            accessibility: &["wheelchair_access"],
            capacity: 2,
            time_zone: mz,
            services: vec![svc("nursing"), svc("blood_draw")],
            rules: weekdays(&[1, 2, 3, 4, 5], "08:00:00", "15:00:00", Some(2)),
        },
    )
    .await?;
    let pons = create_resource(
        tx,
        &admin_ctx,
        state,
        ResourceSpec {
            facility,
            rtype: "professional",
            name: "Dra. Pilar Pons (Pediatrics)",
            user: None,
            profession: Some("physician"),
            specialties: &["pediatrics"],
            languages: &["es", "en", "ca"],
            accessibility: &["wheelchair_access"],
            capacity: 1,
            time_zone: mz,
            services: vec![svc("pediatric_consult")],
            rules: weekdays(&[1, 2, 3, 4], "09:00:00", "13:00:00", None),
        },
    )
    .await?;
    let vidal = create_resource(
        tx,
        &admin_ctx,
        state,
        ResourceSpec {
            facility,
            rtype: "professional",
            name: "Dra. Rocío Vidal (Obstetrics & gynecology)",
            user: None,
            profession: Some("physician"),
            specialties: &["obstetrics_gynecology"],
            languages: &["es"],
            accessibility: &["wheelchair_access"],
            capacity: 1,
            time_zone: mz,
            services: vec![svc("antenatal_visit")],
            rules: weekdays(&[2, 4], "09:00:00", "14:00:00", None),
        },
    )
    .await?;
    let midwife = create_resource(
        tx,
        &admin_ctx,
        state,
        ResourceSpec {
            facility,
            rtype: "professional",
            name: "Carmen Roig (Midwife)",
            user: None,
            profession: Some("midwife"),
            specialties: &["obstetrics_gynecology"],
            languages: &["es", "ca"],
            accessibility: &["wheelchair_access"],
            capacity: 1,
            time_zone: mz,
            services: vec![svc("midwife_visit"), svc("antenatal_visit")],
            rules: weekdays(&[1, 3, 5], "09:00:00", "13:00:00", None),
        },
    )
    .await?;
    let serra = create_resource(
        tx,
        &admin_ctx,
        state,
        ResourceSpec {
            facility,
            rtype: "professional",
            name: "Marc Serra (Clinical psychologist)",
            user: None,
            profession: Some("psychologist"),
            specialties: &["clinical_psychology"],
            languages: &["es", "ca", "en"],
            accessibility: &["wheelchair_access", "sign_language"],
            capacity: 1,
            time_zone: mz,
            services: vec![svc("psychology_session")],
            rules: weekdays(&[2, 3, 4], "15:00:00", "20:00:00", None),
        },
    )
    .await?;
    let roth = create_resource(
        tx,
        &admin_ctx,
        state,
        ResourceSpec {
            facility,
            rtype: "professional",
            name: "Dr. Ilan Roth (Dentist)",
            user: None,
            profession: Some("dentist"),
            specialties: &["dentistry"],
            languages: &["en", "es", "he"],
            accessibility: &["wheelchair_access"],
            capacity: 1,
            time_zone: mz,
            services: vec![svc("dental_checkup")],
            rules: weekdays(&[1, 2, 4, 5], "09:00:00", "14:00:00", None),
        },
    )
    .await?;
    let hygienist = create_resource(
        tx,
        &admin_ctx,
        state,
        ResourceSpec {
            facility,
            rtype: "professional",
            name: "Noa Berg (Dental hygienist)",
            user: None,
            profession: Some("dental_hygienist"),
            specialties: &["dentistry"],
            languages: &["es", "en"],
            accessibility: &["wheelchair_access"],
            capacity: 1,
            time_zone: mz,
            services: vec![svc("dental_hygiene")],
            rules: weekdays(&[1, 2, 3], "14:00:00", "19:00:00", None),
        },
    )
    .await?;
    let dental_chair = create_resource(
        tx,
        &admin_ctx,
        state,
        ResourceSpec {
            facility,
            rtype: "dental_chair",
            name: "Dental chair 1",
            user: None,
            profession: None,
            specialties: &[],
            languages: &[],
            accessibility: &["wheelchair_access"],
            capacity: 1,
            time_zone: mz,
            services: vec![],
            rules: weekdays(&[1, 2, 3, 4, 5], "08:00:00", "20:00:00", None),
        },
    )
    .await?;
    let ferrer = create_resource(
        tx,
        &admin_ctx,
        state,
        ResourceSpec {
            facility,
            rtype: "professional",
            name: "Núria Ferrer (Physiotherapist)",
            user: None,
            profession: Some("physiotherapist"),
            specialties: &["rehabilitation_medicine"],
            languages: &["es", "ca", "en"],
            accessibility: &["wheelchair_access"],
            capacity: 1,
            time_zone: mz,
            services: vec![svc("physio_session")],
            rules: weekdays(&[1, 2, 3, 4, 5], "08:00:00", "14:00:00", None),
        },
    )
    .await?;
    let rehab_gym = create_resource(
        tx,
        &admin_ctx,
        state,
        ResourceSpec {
            facility,
            rtype: "rehabilitation_space",
            name: "Rehabilitation gym",
            user: None,
            profession: None,
            specialties: &[],
            languages: &[],
            accessibility: &["wheelchair_access"],
            capacity: 3,
            time_zone: mz,
            services: vec![],
            rules: weekdays(&[1, 2, 3, 4, 5], "08:00:00", "20:00:00", Some(3)),
        },
    )
    .await?;
    let pharm = create_resource(
        tx,
        &admin_ctx,
        state,
        ResourceSpec {
            facility,
            rtype: "professional",
            name: "Pharmacist Amara Osei (Medication review)",
            user: pharmacist,
            profession: Some("pharmacist"),
            specialties: &[],
            languages: &["en", "es"],
            accessibility: &["wheelchair_access"],
            capacity: 1,
            time_zone: mz,
            services: vec![svc("medication_review")],
            rules: weekdays(&[2, 4], "10:00:00", "13:00:00", None),
        },
    )
    .await?;
    let phlebotomy = create_resource(
        tx,
        &admin_ctx,
        state,
        ResourceSpec {
            facility,
            rtype: "lab_station",
            name: "Phlebotomy station 1",
            user: None,
            profession: None,
            specialties: &["clinical_laboratory"],
            languages: &[],
            accessibility: &["wheelchair_access"],
            capacity: 2,
            time_zone: mz,
            services: vec![svc("blood_draw")],
            rules: weekdays(&[1, 2, 3, 4, 5], "08:00:00", "11:00:00", Some(2)),
        },
    )
    .await?;
    let xray = create_resource(
        tx,
        &admin_ctx,
        state,
        ResourceSpec {
            facility,
            rtype: "imaging_equipment",
            name: "X-ray room 1",
            user: None,
            profession: None,
            specialties: &["radiology"],
            languages: &[],
            accessibility: &["wheelchair_access", "stretcher"],
            capacity: 1,
            time_zone: mz,
            services: vec![svc("xray_exam")],
            rules: weekdays(&[1, 2, 3, 4, 5], "08:00:00", "15:00:00", None),
        },
    )
    .await?;
    let radiographer = create_resource(
        tx,
        &admin_ctx,
        state,
        ResourceSpec {
            facility,
            rtype: "professional",
            name: "Sara Ibarra (Radiographer)",
            user: None,
            profession: Some("radiographer"),
            specialties: &["radiology"],
            languages: &["es", "en"],
            accessibility: &[],
            capacity: 1,
            time_zone: mz,
            services: vec![],
            rules: weekdays(&[1, 2, 3, 4, 5], "08:00:00", "15:00:00", None),
        },
    )
    .await?;
    let home_team = create_resource(
        tx,
        &admin_ctx,
        state,
        ResourceSpec {
            facility,
            rtype: "home_visit_team",
            name: "Home-care team North",
            user: None,
            profession: Some("home_care_aide"),
            specialties: &[],
            languages: &["es", "en"],
            accessibility: &["wheelchair_access", "stretcher"],
            capacity: 1,
            time_zone: mz,
            services: vec![
                svc_mod("home_nursing_visit", &["home_visit"]),
                svc_mod("nursing", &["home_visit"]),
            ],
            rules: weekdays(&[1, 2, 3, 4, 5], "09:00:00", "17:00:00", None),
        },
    )
    .await?;
    let procedure_room = create_resource(
        tx,
        &admin_ctx,
        state,
        ResourceSpec {
            facility,
            rtype: "procedure_room",
            name: "Procedure room A",
            user: None,
            profession: None,
            specialties: &["general_surgery"],
            languages: &[],
            accessibility: &["wheelchair_access", "stretcher"],
            capacity: 1,
            time_zone: mz,
            services: vec![svc("orthopaedic_consult")],
            rules: weekdays(&[2, 4], "08:00:00", "14:00:00", None),
        },
    )
    .await?;
    // Transport and emergency resources (vehicles/teams are schedulable).
    let van = create_resource(
        tx,
        &admin_ctx,
        state,
        ResourceSpec {
            facility,
            rtype: "vehicle",
            name: "Patient transport van 1",
            user: None,
            profession: Some("patient_transport_driver"),
            specialties: &[],
            languages: &[],
            accessibility: &[],
            capacity: 1,
            time_zone: mz,
            services: vec![],
            rules: weekdays(&[1, 2, 3, 4, 5], "07:00:00", "21:00:00", None),
        },
    )
    .await?;
    let accessible_van = create_resource(
        tx,
        &admin_ctx,
        state,
        ResourceSpec {
            facility,
            rtype: "accessible_vehicle",
            name: "Wheelchair-accessible van 1",
            user: None,
            profession: Some("patient_transport_driver"),
            specialties: &[],
            languages: &[],
            accessibility: &["wheelchair_access"],
            capacity: 1,
            time_zone: mz,
            services: vec![],
            rules: weekdays(&[1, 2, 3, 4, 5, 6], "07:00:00", "21:00:00", None),
        },
    )
    .await?;
    let ambulance = create_resource(
        tx,
        &admin_ctx,
        state,
        ResourceSpec {
            facility,
            rtype: "ambulance",
            name: "ALS ambulance 1",
            user: None,
            profession: Some("paramedic"),
            specialties: &["emergency_medicine"],
            languages: &[],
            accessibility: &["stretcher"],
            capacity: 1,
            time_zone: mz,
            services: vec![svc("emergency_transport")],
            rules: weekdays(&[1, 2, 3, 4, 5, 6, 7], "00:00:00", "23:59:00", None),
        },
    )
    .await?;
    let rescue = create_resource(
        tx,
        &admin_ctx,
        state,
        ResourceSpec {
            facility,
            rtype: "emergency_response_unit",
            name: "Rescue unit North",
            user: None,
            profession: Some("paramedic"),
            specialties: &["emergency_medicine"],
            languages: &[],
            accessibility: &["stretcher"],
            capacity: 1,
            time_zone: mz,
            services: vec![],
            rules: weekdays(&[1, 2, 3, 4, 5, 6, 7], "08:00:00", "20:00:00", None),
        },
    )
    .await?;
    // Annex resources in a different time zone.
    let annex_user = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM users WHERE tenant_id = $1 AND username = 'dr.annex'",
    )
    .bind(tenant)
    .fetch_one(&mut **tx)
    .await?;
    let annex_doc = create_resource(
        tx,
        &admin_ctx,
        state,
        ResourceSpec {
            facility: annex,
            rtype: "professional",
            name: "Dr. Andrea Anexo (General medicine, Annex)",
            user: Some(annex_user),
            profession: Some("physician"),
            specialties: &["family_medicine"],
            languages: &["es", "en"],
            accessibility: &["wheelchair_access"],
            capacity: 1,
            time_zone: cz,
            services: vec![
                svc("general_medicine"),
                svc_mod("telehealth", &["telehealth"]),
            ],
            rules: weekdays(&[1, 2, 3, 4, 5], "09:00:00", "16:00:00", None),
        },
    )
    .await?;
    let telehealth_channel = create_resource(
        tx,
        &admin_ctx,
        state,
        ResourceSpec {
            facility: annex,
            rtype: "telehealth_channel",
            name: "Telehealth channel Annex",
            user: None,
            profession: None,
            specialties: &[],
            languages: &["es", "en"],
            accessibility: &[],
            capacity: 4,
            time_zone: cz,
            services: vec![svc_mod("telehealth", &["telehealth"])],
            rules: weekdays(&[1, 2, 3, 4, 5], "08:00:00", "20:00:00", Some(4)),
        },
    )
    .await?;
    let _ = (
        dental_chair,
        rehab_gym,
        radiographer,
        xray,
        procedure_room,
        home_team,
        van,
        rescue,
        telehealth_channel,
        midwife,
        hygienist,
        vidal,
        pons,
        ivan,
        roth,
        pharm,
        phlebotomy,
        annex_doc,
        kim,
    );

    // Exceptions: leave, sickness, a training block, a closure and a
    // temporary extra-capacity day.
    let next_mon = next_weekday(today, 1);
    exception(
        tx,
        &admin_ctx,
        state,
        ivan,
        "leave",
        local(madrid, next_mon + Duration::days(7), 0, 0),
        local(madrid, next_mon + Duration::days(12), 0, 0),
        "annual_leave",
        None,
    )
    .await?;
    exception(
        tx,
        &admin_ctx,
        state,
        garcia,
        "blocked",
        local(madrid, next_mon + Duration::days(2), 12, 0),
        local(madrid, next_mon + Duration::days(2), 14, 0),
        "training",
        None,
    )
    .await?;
    exception(
        tx,
        &admin_ctx,
        state,
        ferrer,
        "sickness",
        local(madrid, next_mon, 0, 0),
        local(madrid, next_mon + Duration::days(2), 0, 0),
        "sick_leave",
        None,
    )
    .await?;
    exception(
        tx,
        &admin_ctx,
        state,
        xray,
        "closure",
        local(madrid, next_mon + Duration::days(3), 0, 0),
        local(madrid, next_mon + Duration::days(4), 0, 0),
        "maintenance",
        None,
    )
    .await?;
    exception(
        tx,
        &admin_ctx,
        state,
        kim,
        "extra_capacity",
        local(madrid, next_mon + Duration::days(1), 8, 0),
        local(madrid, next_mon + Duration::days(1), 15, 0),
        "vaccination_campaign",
        Some(2),
    )
    .await?;

    // Operational calendar: tenant-configured holidays, a school break,
    // a local event and the seasonal summer pressure (fixture-only
    // geography; production logic is geography-neutral).
    let year = today.year();
    let ev = |kind: &str,
              name: &str,
              from: NaiveDate,
              to: NaiveDate,
              demand: f64,
              capacity: f64,
              fac: Option<Uuid>| {
        json!({ "facility_id": fac, "kind": kind, "name": name, "starts_on": from, "ends_on": to,
                "demand_multiplier": demand, "capacity_multiplier": capacity })
    };
    let events = [
        ev(
            "holiday",
            "Public holiday (synthetic)",
            next_mon + Duration::days(4),
            next_mon + Duration::days(4),
            0.0,
            0.0,
            None,
        ),
        ev(
            "local_event",
            "Medieval fair (synthetic local event)",
            next_mon + Duration::days(8),
            next_mon + Duration::days(10),
            1.25,
            1.0,
            Some(facility),
        ),
        ev(
            "school_break",
            "Easter school break (synthetic)",
            next_mon + Duration::days(14),
            next_mon + Duration::days(23),
            1.15,
            0.9,
            None,
        ),
        ev(
            "seasonal_period",
            "Ibiza summer pressure (synthetic)",
            NaiveDate::from_ymd_opt(year, 6, 15).expect("date"),
            NaiveDate::from_ymd_opt(year, 9, 15).expect("date"),
            1.4,
            0.85,
            Some(facility),
        ),
        ev(
            "closure",
            "Annex maintenance closure (synthetic)",
            next_mon + Duration::days(9),
            next_mon + Duration::days(9),
            1.0,
            0.0,
            Some(annex),
        ),
    ];
    for e in events {
        let e: CreateCalendarEvent = body(e)?;
        access_admin::create_calendar_event_in(tx, &admin_ctx, state, e).await?;
    }

    // ------------------------------------------------------------------
    // 5. Legacy scheduled visits -> authoritative appointments (linked,
    //    no duplicate visit, history preserved).
    // ------------------------------------------------------------------
    let general_medicine = scheduling::load_service(tx, tenant, "general_medicine").await?;
    let telehealth = scheduling::load_service(tx, tenant, "telehealth").await?;
    let carlos_appt = scheduling::direct_appointment_for_visit(
        tx,
        &sched,
        state,
        carlos_scheduled_visit,
        facility,
        carlos,
        &general_medicine,
        carlos_scheduled_at,
        Some("Follow-up of hypertension (synthetic)"),
    )
    .await?;
    scheduling::direct_appointment_for_visit(
        tx,
        &sched,
        state,
        anexa_scheduled_visit,
        annex,
        anexa,
        &telehealth,
        anexa_scheduled_at,
        Some("Remote follow-up (synthetic)"),
    )
    .await?;

    // ------------------------------------------------------------------
    // 6. Historical demand for the capacity model
    // ------------------------------------------------------------------
    seed_history(
        tx,
        tenant,
        facility,
        madrid,
        "general_medicine",
        garcia,
        &[carlos, marta, jonas, sofia, diego, alba],
        registration,
        today,
    )
    .await?;

    // ------------------------------------------------------------------
    // 7. Patient-access grants (self, parent/guardian with two dependants,
    //    an expired grant and a revoked one for history).
    // ------------------------------------------------------------------
    let grant_ctx = staff_ctx(tenant, registration, facility, Purpose::Operations);
    let g_alba = grant(
        tx,
        &grant_ctx,
        state,
        rep_alba,
        alba,
        "self",
        None,
        "Identity document checked in person at registration (synthetic)",
    )
    .await?;
    let g_leo = grant(
        tx,
        &grant_ctx,
        state,
        rep_ortiz,
        leo,
        "parent_guardian",
        Some(now + Duration::days(365)),
        "Family book and parent ID checked at registration (synthetic)",
    )
    .await?;
    let g_abuelo = grant(
        tx,
        &grant_ctx,
        state,
        rep_ortiz,
        abuelo,
        "authorized_proxy",
        Some(now + Duration::days(180)),
        "Signed proxy authorization verified (synthetic)",
    )
    .await?;
    let g_sofia = grant(
        tx,
        &grant_ctx,
        state,
        rep_sofia,
        sofia,
        "self",
        None,
        "Identity document checked in person (synthetic)",
    )
    .await?;
    // Expired grant: Lucía used to manage Marta's appointments.
    let g_old = grant(
        tx,
        &grant_ctx,
        state,
        rep_ortiz,
        marta,
        "authorized_proxy",
        Some(now + Duration::days(30)),
        "Temporary proxy during recovery (synthetic)",
    )
    .await?;
    sqlx::query(
        "UPDATE patient_access_grants SET expires_at = $2, version = version + 1 WHERE id = $1",
    )
    .bind(g_old.id)
    .bind(now - Duration::days(3))
    .execute(&mut **tx)
    .await?;
    let g_revoked = grant(
        tx,
        &grant_ctx,
        state,
        rep_sofia,
        diego,
        "authorized_proxy",
        None,
        "Proxy authorization presented (synthetic)",
    )
    .await?;
    sqlx::query(
        "UPDATE patient_access_grants SET status = 'revoked', revoked_at = now(), revoked_by = $2,
             revoke_reason = 'Authorization withdrawn by the patient (synthetic)', version = version + 1
         WHERE id = $1",
    )
    .bind(g_revoked.id)
    .bind(registration)
    .execute(&mut **tx)
    .await?;

    // ------------------------------------------------------------------
    // 8. Patient preferences and calendar conflicts (privacy-preserving
    //    ICS import: only busy intervals survive).
    // ------------------------------------------------------------------
    let alba_ctx = rep_ctx(tenant, rep_alba, "rep.alba");
    let prefs: PreferencesBody = body(json!({
        "patient_id": alba,
        "available_windows": [
            { "weekday": 1, "start": "09:00:00", "end": "13:00:00" },
            { "weekday": 2, "start": "09:00:00", "end": "13:00:00" },
            { "weekday": 3, "start": "09:00:00", "end": "14:00:00" },
            { "weekday": 4, "start": "09:00:00", "end": "13:00:00" },
            { "weekday": 5, "start": "09:00:00", "end": "13:00:00" }
        ],
        "unavailable_windows": [{ "weekday": 3, "start": "12:00:00", "end": "14:00:00" }],
        "preferred_modalities": ["in_person", "telehealth"],
        "preferred_facility_ids": [facility],
        "language": "es",
        "accessibility_needs": [],
        "time_zone": "Europe/Madrid",
        "channels": ["in_app", "email"],
        "quiet_hours_start": "21:00:00",
        "quiet_hours_end": "08:00:00",
        "contact_email": "alba.synthetic@example.invalid",
    }))?;
    me::update_preferences_in(tx, &alba_ctx, state, &g_alba, prefs).await?;
    // Alba's personal calendar blocks Tuesday morning next week (the ICS
    // carries a title, a description and an attendee that must never be
    // stored).
    let busy_start = local(madrid, next_weekday(today, 2), 9, 0);
    let fmt = |t: DateTime<Utc>| t.format("%Y%m%dT%H%M%SZ").to_string();
    let ics = format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//synthetic//EN\r\nBEGIN:VEVENT\r\nUID:synthetic-1\r\n\
         DTSTART:{}\r\nDTEND:{}\r\nSUMMARY:Private appointment (must not be stored)\r\n\
         DESCRIPTION:Synthetic private detail https://meet.example.invalid/x\r\nATTENDEE:mailto:friend@example.invalid\r\n\
         RRULE:FREQ=WEEKLY;COUNT=4\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
        fmt(busy_start),
        fmt(busy_start + Duration::hours(3))
    );
    me::import_ics_in(
        tx,
        state,
        &alba_ctx,
        &g_alba,
        ics.into(),
        "Europe/Madrid",
        Some(90),
        None,
    )
    .await?;

    let ortiz_ctx = rep_ctx(tenant, rep_ortiz, "rep.ortiz");
    let abuelo_prefs: PreferencesBody = body(json!({
        "patient_id": abuelo,
        "available_windows": [
            { "weekday": 1, "start": "10:00:00", "end": "13:00:00" },
            { "weekday": 3, "start": "10:00:00", "end": "13:00:00" },
            { "weekday": 5, "start": "10:00:00", "end": "13:00:00" }
        ],
        "unavailable_windows": [],
        "preferred_modalities": ["in_person"],
        "preferred_facility_ids": [facility],
        "language": "es",
        "accessibility_needs": ["wheelchair_access"],
        "time_zone": "Europe/Madrid",
        "channels": ["in_app"],
        "contact_email": null,
        "push_endpoint": null,
    }))?;
    me::update_preferences_in(tx, &ortiz_ctx, state, &g_abuelo, abuelo_prefs).await?;
    let leo_prefs: PreferencesBody = body(json!({
        "patient_id": leo,
        "available_windows": [
            { "weekday": 1, "start": "09:00:00", "end": "12:00:00" },
            { "weekday": 2, "start": "09:00:00", "end": "12:00:00" },
            { "weekday": 3, "start": "09:00:00", "end": "12:00:00" },
            { "weekday": 4, "start": "09:00:00", "end": "12:00:00" }
        ],
        "unavailable_windows": [],
        "preferred_modalities": ["in_person"],
        "preferred_facility_ids": [],
        "language": "es",
        "accessibility_needs": [],
        "time_zone": "Europe/Madrid",
        "channels": ["in_app"],
    }))?;
    me::update_preferences_in(tx, &ortiz_ctx, state, &g_leo, leo_prefs).await?;
    let sofia_ctx = rep_ctx(tenant, rep_sofia, "rep.sofia");
    let sofia_prefs: PreferencesBody = body(json!({
        "patient_id": sofia,
        "available_windows": [],
        "unavailable_windows": [{ "weekday": 5, "start": "08:00:00", "end": "20:00:00" }],
        "preferred_modalities": ["in_person"],
        "preferred_facility_ids": [facility],
        "language": "en",
        "accessibility_needs": [],
        "time_zone": "Europe/Madrid",
        "channels": ["in_app"],
    }))?;
    me::update_preferences_in(tx, &sofia_ctx, state, &g_sofia, sofia_prefs).await?;

    // ------------------------------------------------------------------
    // 9. Access requests, deterministic matching, holds, confirmations
    // ------------------------------------------------------------------
    let horizon = |from_days: i64, to_days: i64| json!({ "earliest": now + Duration::days(from_days), "latest": now + Duration::days(to_days) });
    let with = |mut base: Value, extra: Value| {
        if let (Some(b), Some(e)) = (base.as_object_mut(), extra.as_object()) {
            for (k, v) in e {
                b.insert(k.clone(), v.clone());
            }
        }
        base
    };

    // Alba (self-service): family medicine, in person, continuity with her
    // care team (Dr. García). Confirmed by the patient herself.
    let alba_req = request(
        tx,
        &alba_ctx,
        state,
        Request {
            patient: alba,
            facility: Some(facility),
            free_text: Some("Necesito una revisión con mi médica de familia, mejor por la mañana."),
            constraints: with(
                horizon(1, 14),
                json!({ "service_code": "family_medicine_review", "modality_codes": ["in_person"],
            "facility_ids": [facility], "continuity_required": true, "language": "es" }),
            ),
            urgency: None,
            channel: "patient",
        },
    )
    .await?;
    let alba_appt =
        match_and_book(tx, &alba_ctx, state, &alba_req, "patient", "patient", 0).await?;

    // Leo (child, booked by his parent): pediatrics — the age restriction
    // admits him; the same service rejects adults.
    let leo_req = request(tx, &ortiz_ctx, state, Request {
        patient: leo, facility: Some(facility), free_text: Some("Revisión pediátrica anual de Leo."),
        constraints: with(horizon(2, 21), json!({ "service_code": "pediatric_consult", "modality_codes": ["in_person"], "language": "es" })),
        urgency: None, channel: "representative",
    }).await?;
    let leo_appt = match_and_book(
        tx,
        &ortiz_ctx,
        state,
        &leo_req,
        "patient",
        "representative",
        1,
    )
    .await?;

    // Ramón (wheelchair user, proxy-managed): blood draw with transport.
    let abuelo_req = request(
        tx,
        &ortiz_ctx,
        state,
        Request {
            patient: abuelo,
            facility: Some(facility),
            free_text: Some("Analítica de control; necesita silla de ruedas y transporte."),
            constraints: with(
                horizon(2, 14),
                json!({ "service_code": "blood_draw", "accessibility_codes": ["wheelchair_access"],
            "transport_requested": true, "language": "es" }),
            ),
            urgency: None,
            channel: "representative",
        },
    )
    .await?;
    let abuelo_appt = match_and_book(
        tx,
        &ortiz_ctx,
        state,
        &abuelo_req,
        "patient",
        "representative",
        0,
    )
    .await?;

    // Sofia (staff-created, priority): physiotherapy needs the gym (multi-
    // resource). Ferrer is off sick Monday/Tuesday next week.
    let sofia_req = request(
        tx,
        &sched,
        state,
        Request {
            patient: sofia,
            facility: Some(facility),
            free_text: Some("Post-operative knee rehabilitation, weekly sessions."),
            constraints: with(
                horizon(1, 14),
                json!({ "service_code": "physio_session", "language": "en" }),
            ),
            urgency: Some("priority"),
            channel: "staff",
        },
    )
    .await?;
    let sofia_appt = match_and_book(tx, &sched, state, &sofia_req, "staff", "staff", 0).await?;

    // Nadia: antenatal visit (staff-created, held but not yet confirmed —
    // an active hold that must not create a visit).
    let nadia_req = request(
        tx,
        &sched,
        state,
        Request {
            patient: nadia,
            facility: Some(facility),
            free_text: Some("Second-trimester antenatal visit."),
            constraints: with(horizon(3, 21), json!({ "service_code": "antenatal_visit" })),
            urgency: None,
            channel: "staff",
        },
    )
    .await?;
    let nadia_run =
        access::run_matcher_in(tx, &sched, state, tenant, nadia_req.id, None, None, "staff")
            .await?;
    if let Some(o) = nadia_run.offers.first() {
        access::hold_offer_in(tx, &sched, state, o.id, None).await?;
    }

    // Diego: dental check-up requires the dental chair (required
    // combination) — confirmed by staff with a documented override reason.
    let diego_req = request(
        tx,
        &sched,
        state,
        Request {
            patient: diego,
            facility: Some(facility),
            free_text: Some("Dental check-up; prefers afternoons."),
            constraints: with(
                horizon(1, 21),
                json!({ "service_code": "dental_checkup",
            "preferred_windows": [{ "weekday": 1, "start": "12:00:00", "end": "14:00:00" }] }),
            ),
            urgency: None,
            channel: "staff",
        },
    )
    .await?;
    let diego_run =
        access::run_matcher_in(tx, &sched, state, tenant, diego_req.id, None, None, "staff")
            .await?;
    let diego_appt = match diego_run.offers.first() {
        Some(o) => Some(
            access::accept_offer_in(
                tx,
                &sched,
                state,
                AcceptInput {
                    offer_id: o.id,
                    version: None,
                    reason: Some("patient_request".into()),
                    override_reason: Some(
                        "Patient confirmed by phone; booking on their behalf (synthetic)".into(),
                    ),
                    idempotency_key: Some(Uuid::now_v7().to_string()),
                    reschedule_of: None,
                    reschedule_reason: None,
                    booked_via: "staff",
                },
            )
            .await?,
        ),
        None => None,
    };

    // Teo (annex, Atlantic/Canary): telehealth — different time zone.
    let teo_req = request(tx, &sched, state, Request {
        patient: teo, facility: Some(annex), free_text: Some("Telehealth follow-up from home."),
        constraints: with(horizon(1, 14), json!({ "service_code": "telehealth", "modality_codes": ["telehealth"], "facility_ids": [annex] })),
        urgency: None, channel: "staff",
    }).await?;
    match_and_book(tx, &sched, state, &teo_req, "staff", "staff", 0).await?;

    // Jonás: cardiology needs a referral — the request stays open with the
    // missing referral until staff supply it; a second request with a
    // referral books him.
    request(
        tx,
        &sched,
        state,
        Request {
            patient: jonas,
            facility: Some(facility),
            free_text: Some("Cardiology review after abnormal ECG."),
            constraints: with(
                horizon(1, 30),
                json!({ "service_code": "cardiology_consult", "has_referral": false }),
            ),
            urgency: None,
            channel: "staff",
        },
    )
    .await?;
    let jonas_req = request(
        tx,
        &sched,
        state,
        Request {
            patient: jonas,
            facility: Some(facility),
            free_text: Some("Cardiology review after abnormal ECG (referral attached)."),
            constraints: with(
                horizon(1, 30),
                json!({ "service_code": "cardiology_consult", "has_referral": true,
            "transport_requested": true }),
            ),
            urgency: Some("priority"),
            channel: "staff",
        },
    )
    .await?;
    let jonas_appt = match_and_book(tx, &sched, state, &jonas_req, "staff", "staff", 0).await?;

    // Pending requests for the staff console: one routed to clinical
    // triage by the deterministic floor (free text with a red flag), one
    // with missing information, one with options ready.
    request(
        tx,
        &alba_ctx,
        state,
        Request {
            patient: alba,
            facility: Some(facility),
            free_text: Some("Dolor en el pecho desde esta mañana y me falta el aire."),
            constraints: horizon(0, 3),
            urgency: None,
            channel: "patient",
        },
    )
    .await?;
    request(
        tx,
        &sofia_ctx,
        state,
        Request {
            patient: sofia,
            facility: None,
            free_text: Some("I would like to see someone about my knee, not sure which service."),
            constraints: horizon(1, 30),
            urgency: None,
            channel: "patient",
        },
    )
    .await?;
    let marta_req = request(
        tx,
        &sched,
        state,
        Request {
            patient: marta,
            facility: Some(facility),
            free_text: Some("Psychology follow-up, afternoons only."),
            constraints: with(
                horizon(1, 21),
                json!({ "service_code": "psychology_session", "language": "es" }),
            ),
            urgency: None,
            channel: "staff",
        },
    )
    .await?;
    access::run_matcher_in(tx, &sched, state, tenant, marta_req.id, None, None, "staff").await?;

    // ------------------------------------------------------------------
    // 10. Waitlist, cancellation and recovery
    // ------------------------------------------------------------------
    let join = |service: &str, windows: Value, notice: i32| -> anyhow::Result<JoinInput> {
        body(json!({
            "service_code": service, "facility_ids": [facility], "modality_codes": ["in_person"],
            "acceptable_windows": windows, "earliest": now, "latest": now + Duration::days(45),
            "min_notice_hours": notice,
        }))
    };
    let all_week = json!([
        { "weekday": 1, "start": "08:00:00", "end": "20:00:00" }, { "weekday": 2, "start": "08:00:00", "end": "20:00:00" },
        { "weekday": 3, "start": "08:00:00", "end": "20:00:00" }, { "weekday": 4, "start": "08:00:00", "end": "20:00:00" },
        { "weekday": 5, "start": "08:00:00", "end": "20:00:00" }
    ]);
    // Three consented patients wait for earlier family-medicine slots;
    // Carlos has been waiting longest, Marta pauses.
    let carlos_entry = waitlist::join_for(
        tx,
        &sched,
        state,
        carlos,
        join("family_medicine_review", all_week.clone(), 2)?,
        false,
    )
    .await?;
    sqlx::query("UPDATE waitlist_entries SET joined_at = $2 WHERE id = $1")
        .bind(carlos_entry.id)
        .bind(now - Duration::days(9))
        .execute(&mut **tx)
        .await?;
    let marta_entry = waitlist::join_for(
        tx,
        &sched,
        state,
        marta,
        join("family_medicine_review", all_week.clone(), 24)?,
        false,
    )
    .await?;
    sqlx::query("UPDATE waitlist_entries SET joined_at = $2, status = 'paused', version = version + 1 WHERE id = $1")
        .bind(marta_entry.id)
        .bind(now - Duration::days(6))
        .execute(&mut **tx)
        .await?;
    waitlist::join_for(
        tx,
        &sofia_ctx,
        state,
        sofia,
        join("family_medicine_review", all_week.clone(), 6)?,
        true,
    )
    .await?;
    let diego_entry = waitlist::join_for(
        tx,
        &sched,
        state,
        diego,
        join("family_medicine_review", all_week.clone(), 4)?,
        false,
    )
    .await?;
    sqlx::query("UPDATE waitlist_entries SET joined_at = $2 WHERE id = $1")
        .bind(diego_entry.id)
        .bind(now - Duration::days(3))
        .execute(&mut **tx)
        .await?;
    waitlist::join_for(
        tx,
        &ortiz_ctx,
        state,
        leo,
        join("pediatric_consult", all_week, 12)?,
        true,
    )
    .await?;

    // Alba's confirmed family-medicine appointment is cancelled by staff
    // (with a reason): the freed slot opens a recovery event and the first
    // deterministic-eligible waitlisted patient receives a time-limited
    // offer, in the same transaction.
    if let Some(a) = &alba_appt {
        access::close_appointment_in(
            tx,
            &sched,
            state,
            a.id,
            None,
            AppointmentTransition::Cancel,
            CloseInput {
                reason_code: Some("clinician_unavailable".into()),
                note: Some("Rescheduling requested by the clinic (synthetic)".into()),
                override_reason: None,
                by_patient: false,
            },
        )
        .await?;
        // The original request keeps its terminal `booked` state (history);
        // she re-books through a new self-service request and takes the
        // second-ranked option.
        let again = request(tx, &alba_ctx, state, Request {
            patient: alba, facility: Some(facility),
            free_text: Some("Me han cancelado la cita; necesito otra con mi médica de familia."),
            constraints: with(horizon(2, 21), json!({ "service_code": "family_medicine_review", "modality_codes": ["in_person"],
                "facility_ids": [facility], "continuity_required": true, "language": "es" })),
            urgency: None, channel: "patient",
        }).await?;
        match_and_book(tx, &alba_ctx, state, &again, "patient", "patient", 1).await?;
    }
    let _ = (leo_appt, alba_req);

    // Reschedule: Sofia moves her physiotherapy session to a later offer
    // — the prior appointment is preserved as `rescheduled` history.
    if let Some(a) = &sofia_appt {
        let locked = scheduling::lock_appointment(tx, a.id).await?;
        let resched = access::open_reschedule_request_in(
            tx,
            &sched,
            state,
            &locked,
            "staff",
            Some("patient_request".into()),
            ConstraintsInput::default(),
        )
        .await?;
        let run =
            access::run_matcher_in(tx, &sched, state, tenant, resched.id, None, None, "staff")
                .await?;
        if let Some(o) = run
            .offers
            .iter()
            .find(|o| o.starts_at > a.starts_at + Duration::days(1))
        {
            access::accept_offer_in(
                tx,
                &sched,
                state,
                AcceptInput {
                    offer_id: o.id,
                    version: None,
                    reason: None,
                    override_reason: None,
                    idempotency_key: Some(Uuid::now_v7().to_string()),
                    reschedule_of: Some(a.id),
                    reschedule_reason: Some("patient_request".into()),
                    booked_via: "staff",
                },
            )
            .await?;
        }
    }

    // ------------------------------------------------------------------
    // 11. Transport linked to appointments (consented; encrypted address)
    // ------------------------------------------------------------------
    let transport_ctx = staff_ctx(tenant, transport_ruiz, facility, Purpose::Operations);
    if let Some(a) = &abuelo_appt {
        let t = transport::create(
            tx,
            &transport_ctx,
            state,
            transport::CreateInput {
                appointment_id: a.id,
                requirements: vec!["wheelchair_access".into()],
                emergency: false,
                origin_area_code: Some("area_north_coast".into()),
                pickup_address: Some("Carrer Sintètic 12, 2n (synthetic address)".into()),
                pickup_window_start: Some(a.starts_at - Duration::minutes(75)),
                pickup_window_end: Some(a.starts_at - Duration::minutes(45)),
                note: Some("Needs help with the wheelchair ramp".into()),
            },
            true,
        )
        .await?;
        transport::transition(
            tx,
            &transport_ctx,
            state,
            t.id,
            transport::TransitionInput {
                status: "scheduled".into(),
                version: Some(t.version),
                vehicle_resource_id: Some(accessible_van),
                operator_user_id: Some(transport_ruiz),
                pickup_window_start: None,
                pickup_window_end: None,
                reason: None,
                note: Some("Accessible van assigned (synthetic)".into()),
            },
            true,
        )
        .await?;
    }
    if let Some(a) = &jonas_appt {
        transport::create(
            tx,
            &transport_ctx,
            state,
            transport::CreateInput {
                appointment_id: a.id,
                requirements: vec![],
                emergency: false,
                origin_area_code: Some("area_island_interior".into()),
                pickup_address: None,
                pickup_window_start: None,
                pickup_window_end: None,
                note: Some("Patient lives in the interior; no own vehicle (synthetic)".into()),
            },
            true,
        )
        .await?;
    }
    let _ = (ambulance, diego_appt, carlos_appt, serra);

    // ------------------------------------------------------------------
    // 12. Capacity forecasts (deterministic; explanation is post-commit)
    // ------------------------------------------------------------------
    let cap_ctx = staff_ctx(tenant, admin, facility, Purpose::Operations);
    capacity::create_forecast(
        tx,
        &cap_ctx,
        state,
        facility,
        "general_medicine",
        today + Duration::days(1),
        28,
    )
    .await?;
    // Insufficient history: a service with no past appointments.
    capacity::create_forecast(
        tx,
        &cap_ctx,
        state,
        facility,
        "dental_checkup",
        today + Duration::days(1),
        14,
    )
    .await?;

    // ------------------------------------------------------------------
    // 13. Requests reserved for the post-commit AI-state runs (they need
    //     pool-scoped governed gateway calls, outside this transaction).
    // ------------------------------------------------------------------
    let ai_ready = request(
        tx,
        &sched,
        state,
        Request {
            patient: carlos,
            facility: Some(facility),
            free_text: Some("General medicine review; any weekday morning."),
            constraints: with(
                horizon(1, 14),
                json!({ "service_code": "general_medicine", "modality_codes": ["in_person"] }),
            ),
            urgency: None,
            channel: "staff",
        },
    )
    .await?;
    let ai_disabled = request(
        tx,
        &sched,
        state,
        Request {
            patient: marta,
            facility: Some(facility),
            free_text: Some("Nursing appointment for wound check."),
            constraints: with(
                horizon(1, 14),
                json!({ "service_code": "nursing", "modality_codes": ["in_person"] }),
            ),
            urgency: None,
            channel: "staff",
        },
    )
    .await?;
    let ai_degraded = request(
        tx,
        &sched,
        state,
        Request {
            patient: diego,
            facility: Some(facility),
            free_text: Some("Medication review with the pharmacist."),
            constraints: with(
                horizon(1, 21),
                json!({ "service_code": "medication_review" }),
            ),
            urgency: None,
            channel: "staff",
        },
    )
    .await?;

    let _ = g_leo;
    Ok(AccessFixtures {
        tenant,
        staff: registration,
        facility,
        ai_ready_request: ai_ready.id,
        ai_disabled_request: ai_disabled.id,
        ai_degraded_request: ai_degraded.id,
    })
}

/// Post-commit phase: the governed dMind Access paths run against the
/// pool (they own their transactions and quota accounting). Three matcher
/// runs prove the three AI states without any fabricated fallback, one
/// scheduling worker tick delivers the due in-app notifications, and every
/// artifact produced here is re-marked synthetic.
pub(crate) async fn after_commit(
    pool: &PgPool,
    runtime: &RuntimeConfig,
    fx: &AccessFixtures,
) -> anyhow::Result<()> {
    let sched = staff_ctx(fx.tenant, fx.staff, fx.facility, Purpose::Treatment);
    let body_for = |ranking: bool| MatchBody {
        version: None,
        origin: None,
        ranking: Some(ranking),
        language: Some("es".into()),
    };

    // AI ready: `access-intent.v1` structures the staff free text, then the
    // fake provider ranks the deterministic candidates.
    let ready = fixture_state(pool.clone(), runtime);
    let mut conn = pool.acquire().await?;
    let r = access::load_request(&mut conn, fx.tenant, fx.ai_ready_request).await?;
    drop(conn);
    access::interpret_request_for(
        &ready,
        &sched,
        r.clone(),
        InterpretBody {
            version: None,
            language: Some("en".into()),
        },
    )
    .await?;
    let mut conn = pool.acquire().await?;
    let r = access::load_request(&mut conn, fx.tenant, r.id).await?;
    drop(conn);
    access::run_matcher_for(&ready, &sched, r, body_for(true), "staff").await?;

    // The open cancellation-recovery event gets a governed
    // `cancellation-recovery.v1` explanation of its deterministic order.
    let event = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM cancellation_events
         WHERE tenant_id = $1 AND status IN ('open', 'offered')
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(fx.tenant)
    .fetch_optional(pool)
    .await?;
    if let Some(id) = event {
        recovery::rank_event(&ready, &sched, id, "es").await?;
    }

    // AI disabled: deterministic ranking, stated as such.
    let disabled = AppState::from_runtime(
        pool.clone(),
        Arc::new(dmind_gateway::DisabledGateway::disabled(
            "synthetic fixture: dMind disabled",
        )),
        Arc::new(dmind_gateway::scribe::FakeTranscription::new()),
        AuthConfig::development(),
        runtime.clone(),
    );
    let mut conn = pool.acquire().await?;
    let r = access::load_request(&mut conn, fx.tenant, fx.ai_disabled_request).await?;
    drop(conn);
    access::run_matcher_for(&disabled, &sched, r, body_for(true), "staff").await?;

    // AI degraded: the provider is reachable but failing; booking continues
    // on deterministic ranking.
    let flaky = dmind_gateway::fake::FakeProvider::new();
    flaky.set_unavailable(true);
    let degraded = AppState::from_runtime(
        pool.clone(),
        Arc::new(flaky),
        Arc::new(dmind_gateway::scribe::FakeTranscription::new()),
        AuthConfig::development(),
        runtime.clone(),
    );
    let mut conn = pool.acquire().await?;
    let r = access::load_request(&mut conn, fx.tenant, fx.ai_degraded_request).await?;
    drop(conn);
    access::run_matcher_for(&degraded, &sched, r, body_for(true), "staff").await?;

    // Capacity explanation for the newest forecast (governed artifact).
    let forecast = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM capacity_forecasts WHERE tenant_id = $1 AND status <> 'insufficient_history'
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(fx.tenant)
    .fetch_optional(pool)
    .await?;
    if let Some(id) = forecast {
        let ops = staff_ctx(fx.tenant, fx.staff, fx.facility, Purpose::Operations);
        capacity::explain(&ready, &ops, id, "es").await?;
    }

    // One worker pass: confirmations and offers that are already due are
    // delivered in-app (the fixture sink records the external channels).
    notify::scheduling_tick(&ready, "seed-fixture-worker").await?;

    sqlx::query(
        "UPDATE ai_artifacts SET synthetic = true, provider = COALESCE(provider, route) WHERE tenant_id = $1",
    )
    .bind(fx.tenant)
    .execute(pool)
    .await?;
    Ok(())
}
