//! Synthetic Care Operations fixtures (development/test only, behind
//! `dev-fixtures`). Every scenario is written through the production
//! `care_ops` write paths so state machines, hard constraints, history
//! triggers and audit behave exactly as they do at runtime. Nothing here is
//! real: patients, staff, locations and timings are invented.

use chrono::{Duration, NaiveTime, Utc};
use serde_json::json;
use sqlx::PgConnection;
use uuid::Uuid;
use wellos_domain::care_ops::{PlacementRequirements, Prerequisite};

use crate::auth::{AuthContext, RoleAssignment};
use crate::care_ops::{self as ops, forecast, theatre, workforce};
use crate::policy::{roles, Purpose};
use crate::seed_access::Tx;
use crate::state::AppState;

pub(crate) struct CareOpsFixtureInput {
    pub tenant: Uuid,
    pub facility: Uuid,
    pub annex: Uuid,
    pub ops_manager: Uuid,
    pub bed_coordinator: Uuid,
    pub theatre_coordinator: Uuid,
    pub workforce_coordinator: Uuid,
    pub dr_garcia: Uuid,
    pub nurse_kim: Uuid,
    pub alba: Uuid,
    pub carlos: Uuid,
    pub marta: Uuid,
    pub jonas: Uuid,
    pub sofia: Uuid,
    pub diego: Uuid,
}

fn ctx(tenant: Uuid, user: Uuid, role: &str, facilities: &[Uuid], purpose: Purpose) -> AuthContext {
    AuthContext {
        user_id: user,
        tenant_id: tenant,
        username: format!("fixture.{role}"),
        display_name: "Synthetic care-operations fixture".into(),
        is_service: false,
        roles: vec![role.into()],
        assignments: facilities
            .iter()
            .map(|f| RoleAssignment {
                role: role.into(),
                facility_id: Some(*f),
            })
            .collect(),
        scopes: vec![],
        purpose_of_use: purpose,
        break_glass_reason: None,
        web_session_id: None,
        correlation_id: Uuid::now_v7(),
    }
}

async fn catalog(
    conn: &mut PgConnection,
    tenant: Uuid,
    by: Uuid,
    entries: &[(&str, &str, &str, &str)],
) -> anyhow::Result<()> {
    for (kind, code, en, es) in entries {
        sqlx::query(
            "INSERT INTO catalog_entries (id, tenant_id, kind, code, name_en, name_es, config, created_by)
             VALUES ($1, $2, $3, $4, $5, $6, '{}'::jsonb, $7)
             ON CONFLICT (tenant_id, kind, code) DO NOTHING",
        )
        .bind(Uuid::now_v7())
        .bind(tenant)
        .bind(kind)
        .bind(code)
        .bind(en)
        .bind(es)
        .bind(by)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

struct Loc<'a> {
    parent: Option<Uuid>,
    level: &'a str,
    type_code: &'a str,
    code: &'a str,
    en: &'a str,
    es: &'a str,
    specialties: &'a [&'a str],
    services: &'a [&'a str],
    age_groups: &'a [&'a str],
    isolation: &'a [&'a str],
    accessibility: &'a [&'a str],
    equipment: &'a [&'a str],
    room_policy: Option<&'a str>,
    capacity: Option<i32>,
}

impl Default for Loc<'_> {
    fn default() -> Self {
        Loc {
            parent: None,
            level: "position",
            type_code: "bed",
            code: "",
            en: "",
            es: "",
            specialties: &[],
            services: &[],
            age_groups: &["adult"],
            isolation: &[],
            accessibility: &[],
            equipment: &[],
            room_policy: None,
            capacity: None,
        }
    }
}

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

async fn location(
    conn: &mut PgConnection,
    c: &AuthContext,
    facility: Uuid,
    l: Loc<'_>,
) -> anyhow::Result<Uuid> {
    let existing: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM operational_locations WHERE tenant_id = $1 AND facility_id = $2 AND code = $3",
    )
    .bind(c.tenant_id)
    .bind(facility)
    .bind(l.code)
    .fetch_optional(&mut *conn)
    .await?;
    if let Some(id) = existing {
        return Ok(id);
    }
    let row = ops::create_location(
        conn,
        c,
        &ops::LocationInput {
            facility_id: facility,
            parent_id: l.parent,
            level: l.level.into(),
            type_code: l.type_code.into(),
            code: l.code.into(),
            name_en: l.en.into(),
            name_es: l.es.into(),
            specialty_codes: strs(l.specialties),
            service_line_codes: strs(l.services),
            profession_requirements: json!([]),
            age_group_codes: strs(l.age_groups),
            isolation_codes: strs(l.isolation),
            accessibility_codes: strs(l.accessibility),
            equipment_codes: strs(l.equipment),
            room_policy: l.room_policy.map(Into::into),
            capacity: l.capacity,
            cleaning_minutes: Some(30),
            schedulable_resource_id: None,
            effective_from: None,
            effective_to: None,
            metadata: json!({"synthetic": true}),
        },
    )
    .await?;
    Ok(row.id)
}

#[allow(clippy::too_many_arguments)]
async fn visit(
    conn: &mut PgConnection,
    tenant: Uuid,
    facility: Uuid,
    patient: Uuid,
    by: Uuid,
    status: &str,
    priority: Option<&str>,
    arrived_minutes_ago: i64,
) -> anyhow::Result<Uuid> {
    let open: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM visits WHERE tenant_id = $1 AND patient_id = $2
         AND status NOT IN ('completed', 'cancelled') LIMIT 1",
    )
    .bind(tenant)
    .bind(patient)
    .fetch_optional(&mut *conn)
    .await?;
    if let Some(id) = open {
        return Ok(id);
    }
    let id = Uuid::now_v7();
    let arrived = Utc::now() - Duration::minutes(arrived_minutes_ago);
    sqlx::query(
        "INSERT INTO visits (id, tenant_id, facility_id, patient_id, status, arrival_kind, service, reason,
            arrived_at, triage_started_at, priority, version, created_by, created_at, updated_at)
         VALUES ($1,$2,$3,$4,$5,'urgent','emergency_medicine','Synthetic urgent arrival',$6,$7,$8,1,$9,$6,$6)",
    )
    .bind(id)
    .bind(tenant)
    .bind(facility)
    .bind(patient)
    .bind(status)
    .bind(arrived)
    .bind((status != "arrived").then_some(arrived + Duration::minutes(10)))
    .bind(priority)
    .bind(by)
    .execute(&mut *conn)
    .await?;
    Ok(id)
}

#[allow(clippy::too_many_arguments)]
async fn episode(
    conn: &mut PgConnection,
    c: &AuthContext,
    facility: Uuid,
    patient: Uuid,
    kind: &str,
    service: &str,
    priority: &str,
    links: Vec<ops::LinkInput>,
) -> anyhow::Result<ops::EpisodeRow> {
    Ok(ops::create_episode(
        conn,
        c,
        &ops::EpisodeInput {
            facility_id: facility,
            patient_id: patient,
            episode_type_code: kind.into(),
            responsible_service_code: Some(service.into()),
            responsible_queue_id: None,
            responsible_user_id: None,
            priority: Some(priority.into()),
            expected_end_at: Some(Utc::now() + Duration::days(3)),
            goals: json!([{"code": "stabilise", "text_en": "Stabilise and plan safe discharge", "text_es": "Estabilizar y planificar un alta segura"}]),
            barriers: json!([]),
            links,
            activate: true,
        },
    )
    .await?)
}

async fn request(
    conn: &mut PgConnection,
    c: &AuthContext,
    ep: &ops::EpisodeRow,
    kind: &str,
    req: PlacementRequirements,
) -> anyhow::Result<ops::PlacementRequestRow> {
    Ok(ops::create_request(
        conn,
        c,
        &ops::PlacementRequestInput {
            episode_id: ep.id,
            kind: Some(kind.into()),
            priority: Some(ep.priority.clone()),
            requirements: req,
            reason_code: None,
            note: None,
        },
    )
    .await?)
}

/// Generates candidates and confirms the first feasible one (human
/// confirmation modelled by the fixture coordinator).
async fn place(
    conn: &mut PgConnection,
    c: &AuthContext,
    req: &ops::PlacementRequestRow,
) -> anyhow::Result<(ops::PlacementRequestRow, ops::PlacementRow)> {
    let (req, m, _) = ops::generate_candidates(conn, c, req.id, None).await?;
    let best = m
        .feasible
        .first()
        .ok_or_else(|| anyhow::anyhow!("fixture expected a feasible placement candidate"))?;
    let (req, placement, _) = ops::confirm(
        conn,
        c,
        req.id,
        &ops::ConfirmInput {
            expected_version: req.version,
            location_id: Some(best.location_id),
            facts_hash: Some(m.facts_hash.clone()),
            note: Some("Confirmed by synthetic bed coordinator".into()),
        },
    )
    .await?;
    Ok((req, placement))
}

fn reqs(
    service: &str,
    isolation: &[&str],
    accessibility: &[&str],
    equipment: &[&str],
) -> PlacementRequirements {
    PlacementRequirements {
        service_code: Some(service.into()),
        specialty_codes: vec![],
        age_group_code: Some("adult".into()),
        isolation_codes: strs(isolation),
        accessibility_codes: strs(accessibility),
        equipment_codes: strs(equipment),
        room_policy: None,
        sex: None,
        preferred_unit_ids: vec![],
        required_staffed_professions: vec![],
    }
}

pub(crate) async fn seed(
    tx: &mut Tx<'_>,
    state: &AppState,
    input: CareOpsFixtureInput,
) -> anyhow::Result<()> {
    let CareOpsFixtureInput {
        tenant,
        facility,
        annex,
        ops_manager,
        bed_coordinator,
        theatre_coordinator,
        workforce_coordinator,
        dr_garcia,
        nurse_kim,
        alba,
        carlos,
        marta,
        jonas,
        sofia,
        diego,
    } = input;
    let both = [facility, annex];
    let mgr = ctx(
        tenant,
        ops_manager,
        roles::OPERATIONS_MANAGER,
        &both,
        Purpose::Operations,
    );
    let beds = ctx(
        tenant,
        bed_coordinator,
        roles::BED_COORDINATOR,
        &both,
        Purpose::Operations,
    );
    let th = ctx(
        tenant,
        theatre_coordinator,
        roles::THEATRE_COORDINATOR,
        &both,
        Purpose::Operations,
    );
    let wf = ctx(
        tenant,
        workforce_coordinator,
        roles::WORKFORCE_COORDINATOR,
        &both,
        Purpose::Operations,
    );
    let conn: &mut PgConnection = tx;

    // Runtime catalog data: episode types, location types, capabilities,
    // professions and barrier categories are configuration, never code enums.
    catalog(
        conn,
        tenant,
        ops_manager,
        &[
            ("episode_type", "emergency", "Emergency", "Urgencias"),
            ("episode_type", "observation", "Observation", "Observación"),
            ("episode_type", "inpatient", "Inpatient", "Hospitalización"),
            (
                "episode_type",
                "day_case",
                "Day case",
                "Cirugía/atención de día",
            ),
            (
                "episode_type",
                "perioperative",
                "Perioperative",
                "Perioperatorio",
            ),
            ("episode_type", "ambulatory", "Ambulatory", "Ambulatorio"),
            (
                "episode_type",
                "hospital_at_home",
                "Hospital at home",
                "Hospitalización a domicilio",
            ),
            (
                "episode_type",
                "rehabilitation",
                "Rehabilitation",
                "Rehabilitación",
            ),
            (
                "episode_type",
                "mental_health",
                "Mental health",
                "Salud mental",
            ),
            ("episode_type", "maternity", "Maternity", "Maternidad"),
            (
                "episode_type",
                "virtual_care",
                "Virtual care",
                "Atención virtual",
            ),
            (
                "operational_location_type",
                "department",
                "Department",
                "Servicio",
            ),
            ("operational_location_type", "ward", "Ward", "Planta"),
            ("operational_location_type", "room", "Room", "Habitación"),
            ("operational_location_type", "bay", "Bay", "Box"),
            ("operational_location_type", "bed", "Bed", "Cama"),
            ("operational_location_type", "chair", "Chair", "Sillón"),
            (
                "operational_location_type",
                "theatre",
                "Operating theatre",
                "Quirófano",
            ),
            (
                "operational_location_type",
                "treatment_position",
                "Treatment position",
                "Puesto de tratamiento",
            ),
            (
                "operational_location_type",
                "virtual_unit",
                "Virtual unit",
                "Unidad virtual",
            ),
            ("age_group", "adult", "Adult", "Adulto"),
            ("age_group", "paediatric", "Paediatric", "Pediátrico"),
            ("age_group", "neonate", "Neonate", "Neonato"),
            (
                "isolation_capability",
                "negative_pressure",
                "Negative-pressure isolation",
                "Aislamiento con presión negativa",
            ),
            (
                "isolation_capability",
                "contact",
                "Contact isolation",
                "Aislamiento de contacto",
            ),
            (
                "accessibility_capability",
                "wheelchair_access",
                "Wheelchair access",
                "Acceso en silla de ruedas",
            ),
            (
                "accessibility_capability",
                "hoist",
                "Ceiling hoist",
                "Grúa de techo",
            ),
            (
                "equipment_capability",
                "telemetry",
                "Cardiac telemetry",
                "Telemetría cardíaca",
            ),
            (
                "equipment_capability",
                "bariatric_bed",
                "Bariatric bed",
                "Cama bariátrica",
            ),
            (
                "equipment_capability",
                "oxygen",
                "Piped oxygen",
                "Oxígeno canalizado",
            ),
            (
                "equipment_capability",
                "c_arm",
                "C-arm fluoroscopy",
                "Arco en C",
            ),
            (
                "equipment_capability",
                "laparoscopy_tower",
                "Laparoscopy tower",
                "Torre de laparoscopia",
            ),
            (
                "equipment_capability",
                "dental_unit",
                "Dental unit",
                "Unidad dental",
            ),
            (
                "profession",
                "physiotherapist",
                "Physiotherapist",
                "Fisioterapeuta",
            ),
            ("profession", "porter", "Porter", "Celador/a"),
            ("profession", "midwife", "Midwife", "Matrona"),
            ("profession", "dentist", "Dentist", "Dentista"),
            (
                "profession",
                "radiographer",
                "Radiographer",
                "Técnico/a de imagen",
            ),
            (
                "profession",
                "laboratory_scientist",
                "Laboratory scientist",
                "Técnico/a de laboratorio",
            ),
            (
                "profession",
                "mental_health_nurse",
                "Mental health nurse",
                "Enfermero/a de salud mental",
            ),
            (
                "profession",
                "home_care_nurse",
                "Home-care nurse",
                "Enfermero/a de atención domiciliaria",
            ),
            ("profession", "anaesthetist", "Anaesthetist", "Anestesista"),
            ("profession", "surgeon", "Surgeon", "Cirujano/a"),
            (
                "professional_capability",
                "acls",
                "Advanced life support",
                "Soporte vital avanzado",
            ),
            (
                "professional_capability",
                "paediatric_nursing",
                "Paediatric nursing",
                "Enfermería pediátrica",
            ),
            (
                "professional_capability",
                "scrub",
                "Scrub practitioner",
                "Instrumentista",
            ),
            (
                "professional_capability",
                "telemetry_monitoring",
                "Telemetry monitoring",
                "Monitorización de telemetría",
            ),
            (
                "professional_capability",
                "neuro_rehab",
                "Neurological rehabilitation",
                "Rehabilitación neurológica",
            ),
            ("barrier_category", "transport", "Transport", "Transporte"),
            (
                "barrier_category",
                "pharmacy",
                "Pharmacy / medication supply",
                "Farmacia / medicación",
            ),
            (
                "barrier_category",
                "social_care",
                "Social care / home support",
                "Atención social / apoyo domiciliario",
            ),
            (
                "barrier_category",
                "equipment",
                "Equipment at home",
                "Equipamiento domiciliario",
            ),
            (
                "barrier_category",
                "diagnostics",
                "Pending diagnostics",
                "Diagnósticos pendientes",
            ),
            (
                "barrier_category",
                "education",
                "Patient education",
                "Educación al paciente",
            ),
            (
                "procedure",
                "lap_cholecystectomy",
                "Laparoscopic cholecystectomy",
                "Colecistectomía laparoscópica",
            ),
            (
                "procedure",
                "hip_fracture_fixation",
                "Hip fracture fixation",
                "Osteosíntesis de fractura de cadera",
            ),
            (
                "procedure",
                "dental_extraction_sedation",
                "Dental extraction under sedation",
                "Extracción dental bajo sedación",
            ),
            ("shift_pattern", "day", "Day shift", "Turno de día"),
            ("shift_pattern", "night", "Night shift", "Turno de noche"),
            (
                "clinical_service",
                "emergency_medicine",
                "Emergency medicine",
                "Medicina de urgencias",
            ),
            (
                "clinical_service",
                "internal_medicine",
                "Internal medicine",
                "Medicina interna",
            ),
            (
                "clinical_service",
                "general_surgery",
                "General surgery",
                "Cirugía general",
            ),
            (
                "clinical_service",
                "paediatrics",
                "Paediatrics",
                "Pediatría",
            ),
            ("clinical_service", "maternity", "Maternity", "Maternidad"),
            (
                "clinical_service",
                "mental_health",
                "Mental health",
                "Salud mental",
            ),
            (
                "clinical_service",
                "rehabilitation",
                "Rehabilitation",
                "Rehabilitación",
            ),
            ("clinical_service", "dentistry", "Dentistry", "Odontología"),
            (
                "clinical_service",
                "hospital_at_home",
                "Hospital at home",
                "Hospitalización a domicilio",
            ),
        ],
    )
    .await?;

    // --- Operational hierarchy (main facility) ------------------------------
    let ed = location(
        conn,
        &mgr,
        facility,
        Loc {
            level: "department",
            type_code: "department",
            code: "ED",
            en: "Emergency department",
            es: "Servicio de urgencias",
            services: &["emergency_medicine"],
            ..Loc::default()
        },
    )
    .await?;
    let ed_bays = location(
        conn,
        &mgr,
        facility,
        Loc {
            parent: Some(ed),
            level: "unit",
            type_code: "bay",
            code: "ED-MAJ",
            en: "ED majors",
            es: "Urgencias boxes",
            services: &["emergency_medicine"],
            capacity: Some(4),
            ..Loc::default()
        },
    )
    .await?;
    for i in 1..=4 {
        let code = format!("ED-MAJ-{i}");
        location(
            conn,
            &mgr,
            facility,
            Loc {
                parent: Some(ed_bays),
                type_code: "treatment_position",
                code: &code,
                en: &format!("ED majors bay {i}"),
                es: &format!("Box de urgencias {i}"),
                services: &["emergency_medicine"],
                age_groups: &["adult", "paediatric"],
                equipment: &["oxygen"],
                ..Loc::default()
            },
        )
        .await?;
    }

    let med_dept = location(
        conn,
        &mgr,
        facility,
        Loc {
            level: "department",
            type_code: "department",
            code: "MED",
            en: "Medical division",
            es: "División médica",
            services: &["internal_medicine"],
            ..Loc::default()
        },
    )
    .await?;
    let med_ward = location(
        conn,
        &mgr,
        facility,
        Loc {
            parent: Some(med_dept),
            level: "unit",
            type_code: "ward",
            code: "MED-3",
            en: "Medical ward 3",
            es: "Planta médica 3",
            services: &["internal_medicine", "emergency_medicine"],
            capacity: Some(4),
            ..Loc::default()
        },
    )
    .await?;
    let med_room_a = location(
        conn,
        &mgr,
        facility,
        Loc {
            parent: Some(med_ward),
            level: "room",
            type_code: "room",
            code: "MED-3-A",
            en: "Room 3A (2 beds)",
            es: "Habitación 3A (2 camas)",
            services: &["internal_medicine", "emergency_medicine"],
            room_policy: Some("mixed"),
            capacity: Some(2),
            ..Loc::default()
        },
    )
    .await?;
    let med_a1 = location(
        conn,
        &mgr,
        facility,
        Loc {
            parent: Some(med_room_a),
            code: "MED-3-A1",
            en: "Bed 3A-1",
            es: "Cama 3A-1",
            services: &["internal_medicine", "emergency_medicine"],
            equipment: &["oxygen", "telemetry"],
            ..Loc::default()
        },
    )
    .await?;
    let med_a2 = location(
        conn,
        &mgr,
        facility,
        Loc {
            parent: Some(med_room_a),
            code: "MED-3-A2",
            en: "Bed 3A-2",
            es: "Cama 3A-2",
            services: &["internal_medicine", "emergency_medicine"],
            equipment: &["oxygen"],
            ..Loc::default()
        },
    )
    .await?;
    let med_room_iso = location(
        conn,
        &mgr,
        facility,
        Loc {
            parent: Some(med_ward),
            level: "room",
            type_code: "room",
            code: "MED-3-ISO",
            en: "Isolation room 3I",
            es: "Habitación de aislamiento 3I",
            services: &["internal_medicine", "emergency_medicine"],
            isolation: &["negative_pressure", "contact"],
            room_policy: Some("single"),
            capacity: Some(1),
            ..Loc::default()
        },
    )
    .await?;
    let med_iso1 = location(
        conn,
        &mgr,
        facility,
        Loc {
            parent: Some(med_room_iso),
            code: "MED-3-ISO1",
            en: "Isolation bed 3I-1",
            es: "Cama de aislamiento 3I-1",
            services: &["internal_medicine", "emergency_medicine"],
            isolation: &["negative_pressure", "contact"],
            equipment: &["oxygen"],
            ..Loc::default()
        },
    )
    .await?;
    let med_room_c = location(
        conn,
        &mgr,
        facility,
        Loc {
            parent: Some(med_ward),
            level: "room",
            type_code: "room",
            code: "MED-3-C",
            en: "Accessible room 3C",
            es: "Habitación accesible 3C",
            services: &["internal_medicine"],
            accessibility: &["wheelchair_access", "hoist"],
            room_policy: Some("single"),
            capacity: Some(1),
            ..Loc::default()
        },
    )
    .await?;
    let med_c1 = location(
        conn,
        &mgr,
        facility,
        Loc {
            parent: Some(med_room_c),
            code: "MED-3-C1",
            en: "Accessible bed 3C-1",
            es: "Cama accesible 3C-1",
            services: &["internal_medicine"],
            accessibility: &["wheelchair_access", "hoist"],
            equipment: &["bariatric_bed", "oxygen"],
            ..Loc::default()
        },
    )
    .await?;

    let surg_dept = location(
        conn,
        &mgr,
        facility,
        Loc {
            level: "department",
            type_code: "department",
            code: "SURG",
            en: "Surgical division",
            es: "División quirúrgica",
            services: &["general_surgery"],
            ..Loc::default()
        },
    )
    .await?;
    let surg_ward = location(
        conn,
        &mgr,
        facility,
        Loc {
            parent: Some(surg_dept),
            level: "unit",
            type_code: "ward",
            code: "SURG-2",
            en: "Surgical ward 2",
            es: "Planta quirúrgica 2",
            services: &["general_surgery"],
            capacity: Some(2),
            ..Loc::default()
        },
    )
    .await?;
    let surg_room = location(
        conn,
        &mgr,
        facility,
        Loc {
            parent: Some(surg_ward),
            level: "room",
            type_code: "room",
            code: "SURG-2-A",
            en: "Room 2A (2 beds)",
            es: "Habitación 2A (2 camas)",
            services: &["general_surgery"],
            room_policy: Some("mixed"),
            capacity: Some(2),
            ..Loc::default()
        },
    )
    .await?;
    let surg_a1 = location(
        conn,
        &mgr,
        facility,
        Loc {
            parent: Some(surg_room),
            code: "SURG-2-A1",
            en: "Bed 2A-1",
            es: "Cama 2A-1",
            services: &["general_surgery"],
            equipment: &["oxygen"],
            ..Loc::default()
        },
    )
    .await?;
    let surg_a2 = location(
        conn,
        &mgr,
        facility,
        Loc {
            parent: Some(surg_room),
            code: "SURG-2-A2",
            en: "Bed 2A-2",
            es: "Cama 2A-2",
            services: &["general_surgery"],
            equipment: &["oxygen"],
            ..Loc::default()
        },
    )
    .await?;
    let theatres = location(
        conn,
        &mgr,
        facility,
        Loc {
            parent: Some(surg_dept),
            level: "unit",
            type_code: "ward",
            code: "OR",
            en: "Theatre suite",
            es: "Bloque quirúrgico",
            services: &["general_surgery", "dentistry"],
            capacity: Some(2),
            ..Loc::default()
        },
    )
    .await?;
    let theatre_1 = location(
        conn,
        &mgr,
        facility,
        Loc {
            parent: Some(theatres),
            type_code: "theatre",
            code: "OR-1",
            en: "Theatre 1",
            es: "Quirófano 1",
            services: &["general_surgery"],
            equipment: &["c_arm", "laparoscopy_tower"],
            ..Loc::default()
        },
    )
    .await?;
    let theatre_2 = location(
        conn,
        &mgr,
        facility,
        Loc {
            parent: Some(theatres),
            type_code: "theatre",
            code: "OR-2",
            en: "Theatre 2",
            es: "Quirófano 2",
            services: &["general_surgery", "dentistry"],
            equipment: &["dental_unit"],
            ..Loc::default()
        },
    )
    .await?;

    let wch = location(
        conn,
        &mgr,
        facility,
        Loc {
            level: "department",
            type_code: "department",
            code: "WCH",
            en: "Women and children",
            es: "Mujer e infancia",
            services: &["paediatrics", "maternity"],
            ..Loc::default()
        },
    )
    .await?;
    let paeds = location(
        conn,
        &mgr,
        facility,
        Loc {
            parent: Some(wch),
            level: "unit",
            type_code: "ward",
            code: "PAED-1",
            en: "Paediatric unit",
            es: "Unidad de pediatría",
            services: &["paediatrics"],
            age_groups: &["paediatric", "neonate"],
            capacity: Some(2),
            ..Loc::default()
        },
    )
    .await?;
    for i in 1..=2 {
        let code = format!("PAED-1-{i}");
        location(
            conn,
            &mgr,
            facility,
            Loc {
                parent: Some(paeds),
                code: &code,
                en: &format!("Paediatric bed {i}"),
                es: &format!("Cama pediátrica {i}"),
                services: &["paediatrics"],
                age_groups: &["paediatric"],
                equipment: &["oxygen"],
                ..Loc::default()
            },
        )
        .await?;
    }
    let maternity = location(
        conn,
        &mgr,
        facility,
        Loc {
            parent: Some(wch),
            level: "unit",
            type_code: "ward",
            code: "MAT-1",
            en: "Maternity unit",
            es: "Unidad de maternidad",
            services: &["maternity"],
            capacity: Some(2),
            ..Loc::default()
        },
    )
    .await?;
    location(
        conn,
        &mgr,
        facility,
        Loc {
            parent: Some(maternity),
            code: "MAT-1-1",
            en: "Maternity bed 1",
            es: "Cama de maternidad 1",
            services: &["maternity"],
            ..Loc::default()
        },
    )
    .await?;
    let mh_dept = location(
        conn,
        &mgr,
        facility,
        Loc {
            level: "department",
            type_code: "department",
            code: "MHD",
            en: "Mental health division",
            es: "División de salud mental",
            services: &["mental_health"],
            ..Loc::default()
        },
    )
    .await?;
    let mh = location(
        conn,
        &mgr,
        facility,
        Loc {
            parent: Some(mh_dept),
            level: "unit",
            type_code: "ward",
            code: "MH-1",
            en: "Mental health unit",
            es: "Unidad de salud mental",
            services: &["mental_health"],
            capacity: Some(2),
            ..Loc::default()
        },
    )
    .await?;
    location(
        conn,
        &mgr,
        facility,
        Loc {
            parent: Some(mh),
            code: "MH-1-1",
            en: "Mental health bed 1",
            es: "Cama de salud mental 1",
            services: &["mental_health"],
            room_policy: Some("single"),
            ..Loc::default()
        },
    )
    .await?;
    let rehab = location(
        conn,
        &mgr,
        facility,
        Loc {
            parent: Some(med_dept),
            level: "unit",
            type_code: "ward",
            code: "REHAB-1",
            en: "Rehabilitation unit",
            es: "Unidad de rehabilitación",
            services: &["rehabilitation"],
            capacity: Some(2),
            ..Loc::default()
        },
    )
    .await?;
    let rehab_1 = location(
        conn,
        &mgr,
        facility,
        Loc {
            parent: Some(rehab),
            code: "REHAB-1-1",
            en: "Rehabilitation bed 1",
            es: "Cama de rehabilitación 1",
            services: &["rehabilitation"],
            accessibility: &["wheelchair_access"],
            ..Loc::default()
        },
    )
    .await?;
    let day = location(
        conn,
        &mgr,
        facility,
        Loc {
            parent: Some(surg_dept),
            level: "unit",
            type_code: "ward",
            code: "DAY-1",
            en: "Day-case unit",
            es: "Unidad de día",
            services: &["dentistry", "general_surgery"],
            capacity: Some(2),
            ..Loc::default()
        },
    )
    .await?;
    let day_chair = location(
        conn,
        &mgr,
        facility,
        Loc {
            parent: Some(day),
            type_code: "chair",
            code: "DAY-1-C1",
            en: "Day-case chair 1",
            es: "Sillón de día 1",
            services: &["dentistry", "general_surgery"],
            ..Loc::default()
        },
    )
    .await?;
    let hah = location(
        conn,
        &mgr,
        facility,
        Loc {
            parent: Some(med_dept),
            level: "unit",
            type_code: "virtual_unit",
            code: "HAH",
            en: "Hospital at home",
            es: "Hospitalización a domicilio",
            services: &["hospital_at_home"],
            capacity: Some(10),
            ..Loc::default()
        },
    )
    .await?;
    location(
        conn,
        &mgr,
        facility,
        Loc {
            parent: Some(hah),
            type_code: "treatment_position",
            code: "HAH-1",
            en: "Home-care slot 1",
            es: "Plaza domiciliaria 1",
            services: &["hospital_at_home"],
            ..Loc::default()
        },
    )
    .await?;

    // Annex facility: one small ward so cross-facility isolation is observable.
    let annex_dept = location(
        conn,
        &mgr,
        annex,
        Loc {
            level: "department",
            type_code: "department",
            code: "ANX",
            en: "Annex medical department",
            es: "Servicio médico del anexo",
            services: &["internal_medicine"],
            ..Loc::default()
        },
    )
    .await?;
    let annex_ward = location(
        conn,
        &mgr,
        annex,
        Loc {
            parent: Some(annex_dept),
            level: "unit",
            type_code: "ward",
            code: "ANX-1",
            en: "Annex ward 1",
            es: "Planta anexo 1",
            services: &["internal_medicine"],
            capacity: Some(1),
            ..Loc::default()
        },
    )
    .await?;
    location(
        conn,
        &mgr,
        annex,
        Loc {
            parent: Some(annex_ward),
            code: "ANX-1-1",
            en: "Annex bed 1",
            es: "Cama anexo 1",
            services: &["internal_medicine"],
            ..Loc::default()
        },
    )
    .await?;

    // Maintenance: bed 2A-2 out of service until tomorrow (near-capacity ward).
    ops::create_period(
        conn,
        &mgr,
        surg_a2,
        &ops::PeriodInput {
            kind: "maintenance".into(),
            starts_at: Utc::now() - Duration::hours(2),
            ends_at: Some(Utc::now() + Duration::hours(20)),
            reason_code: Some("bed_repair".into()),
        },
    )
    .await?;

    // --- Scenario: completed day-case episode with full history (Alba) ------
    let alba_ep = episode(
        conn,
        &beds,
        facility,
        alba,
        "day_case",
        "dentistry",
        "routine",
        vec![],
    )
    .await?;
    let alba_req = request(
        conn,
        &beds,
        &alba_ep,
        "admission",
        reqs("dentistry", &[], &[], &[]),
    )
    .await?;
    let (_, alba_pl) = place(conn, &beds, &alba_req).await?;
    let alba_ep = ops::load_episode(conn, tenant, alba_ep.id, false).await?;
    let alba_ep = ops::transition_episode(
        conn,
        &beds,
        alba_ep.id,
        &ops::EpisodeTransitionInput {
            transition: "start_discharge_preparation".into(),
            expected_version: alba_ep.version,
            reason_code: None,
            note: None,
        },
    )
    .await?;
    ops::transition_episode(
        conn,
        &beds,
        alba_ep.id,
        &ops::EpisodeTransitionInput {
            transition: "mark_discharge_ready".into(),
            expected_version: alba_ep.version,
            reason_code: None,
            note: Some(
                "Operational readiness only; clinical discharge decided by the clinician".into(),
            ),
        },
    )
    .await?;
    let alba_pl = ops::load_placement(conn, tenant, alba_pl.id, false).await?;
    let (_, alba_ep) = ops::release(
        conn,
        &beds,
        alba_pl.id,
        &ops::ReleaseInput {
            expected_version: alba_pl.version,
            reason_code: Some("discharged".into()),
        },
    )
    .await?;
    let alba_ep = ops::load_episode(conn, tenant, alba_ep.id, false).await?;
    if alba_ep.status != "completed" {
        ops::transition_episode(
            conn,
            &beds,
            alba_ep.id,
            &ops::EpisodeTransitionInput {
                transition: "complete".into(),
                expected_version: alba_ep.version,
                reason_code: Some("discharged_home".into()),
                note: None,
            },
        )
        .await?;
    }
    // The chair is now cleaning; turnaround completes later in the tick job.
    let _ = day_chair;

    // --- Scenario: near-capacity surgical ward + internal transfer (Jonás) --
    let jonas_ep = episode(
        conn,
        &beds,
        facility,
        jonas,
        "inpatient",
        "internal_medicine",
        "priority",
        vec![],
    )
    .await?;
    let jonas_req = request(
        conn,
        &beds,
        &jonas_ep,
        "admission",
        reqs("internal_medicine", &[], &[], &["telemetry"]),
    )
    .await?;
    let (_, _jonas_pl) = place(conn, &beds, &jonas_req).await?;
    // Transfer to the surgical ward once the surgical team takes over.
    let jonas_ep = ops::load_episode(conn, tenant, jonas_ep.id, false).await?;
    let transfer = request(
        conn,
        &beds,
        &jonas_ep,
        "transfer",
        reqs("general_surgery", &[], &[], &[]),
    )
    .await?;
    let (_, _) = place(conn, &beds, &transfer).await?;
    let _ = (surg_a1, med_a1);

    // --- Scenario: emergency admission awaiting an isolation bed (Carlos) ---
    let carlos_visit = visit(
        conn,
        tenant,
        facility,
        carlos,
        bed_coordinator,
        "ready_for_consultation",
        Some("urgent"),
        190,
    )
    .await?;
    let carlos_ep = episode(
        conn,
        &beds,
        facility,
        carlos,
        "emergency",
        "emergency_medicine",
        "urgent",
        vec![ops::LinkInput {
            link_type: "visit".into(),
            link_id: carlos_visit,
        }],
    )
    .await?;
    let carlos_req = request(
        conn,
        &beds,
        &carlos_ep,
        "admission",
        reqs(
            "internal_medicine",
            &["negative_pressure"],
            &[],
            &["oxygen"],
        ),
    )
    .await?;
    let (carlos_req, _, _) = ops::generate_candidates(conn, &beds, carlos_req.id, None).await?;
    ops::arm_escalation(
        conn,
        &beds,
        ops::ArmEscalation {
            facility_id: facility,
            patient_id: carlos,
            episode_id: Some(carlos_ep.id),
            visit_id: Some(carlos_visit),
            placement_request_id: Some(carlos_req.id),
            kind: "boarding",
            priority: "urgent",
            started_at: Utc::now() - Duration::minutes(185),
            threshold_minutes: Some(120),
        },
    )
    .await?;
    let _ = med_iso1;

    // --- Scenario: accessibility requirement held, not yet confirmed (Marta)
    let marta_visit = visit(
        conn,
        tenant,
        facility,
        marta,
        bed_coordinator,
        "arrived",
        Some("standard"),
        40,
    )
    .await?;
    let marta_ep = episode(
        conn,
        &beds,
        facility,
        marta,
        "observation",
        "internal_medicine",
        "routine",
        vec![ops::LinkInput {
            link_type: "visit".into(),
            link_id: marta_visit,
        }],
    )
    .await?;
    let marta_req = request(
        conn,
        &beds,
        &marta_ep,
        "admission",
        reqs(
            "internal_medicine",
            &[],
            &["wheelchair_access", "hoist"],
            &["bariatric_bed"],
        ),
    )
    .await?;
    let (marta_req, m, _) = ops::generate_candidates(conn, &beds, marta_req.id, None).await?;
    if let Some(best) = m.feasible.first() {
        ops::hold(
            conn,
            &beds,
            marta_req.id,
            &ops::HoldInput {
                location_id: best.location_id,
                expected_version: marta_req.version,
                facts_hash: m.facts_hash.clone(),
                hold_minutes: Some(45),
            },
        )
        .await?;
    }
    let _ = (med_c1, med_a2);

    // Waiting-room pressure: two more arrivals without an episode yet.
    visit(
        conn,
        tenant,
        facility,
        diego,
        bed_coordinator,
        "arrived",
        None,
        25,
    )
    .await?;

    // --- Scenario: discharge barriers, delayed transport (Sofía) -----------
    let sofia_ep = episode(
        conn,
        &beds,
        facility,
        sofia,
        "rehabilitation",
        "rehabilitation",
        "routine",
        vec![],
    )
    .await?;
    let sofia_req = request(
        conn,
        &beds,
        &sofia_ep,
        "admission",
        reqs("rehabilitation", &[], &["wheelchair_access"], &[]),
    )
    .await?;
    place(conn, &beds, &sofia_req).await?;
    let sofia_ep = ops::load_episode(conn, tenant, sofia_ep.id, false).await?;
    let sofia_ep = ops::transition_episode(
        conn,
        &beds,
        sofia_ep.id,
        &ops::EpisodeTransitionInput {
            transition: "start_discharge_preparation".into(),
            expected_version: sofia_ep.version,
            reason_code: None,
            note: None,
        },
    )
    .await?;
    let task = |title_en: &str,
                title_es: &str,
                kind: &str,
                profession: Option<&str>,
                barrier: Option<&str>,
                patient_visible: bool,
                due_h: i64| ops::TaskInput {
        episode_id: sofia_ep.id,
        title_en: title_en.into(),
        title_es: title_es.into(),
        kind: Some(kind.into()),
        accountable_user_id: Some(nurse_kim),
        accountable_queue_id: None,
        care_team_assignment_id: None,
        profession_code: profession.map(Into::into),
        capability_codes: vec![],
        due_at: Some(Utc::now() + Duration::hours(due_h)),
        escalate_at: Some(Utc::now() + Duration::hours(due_h + 2)),
        barrier_category_code: barrier.map(Into::into),
        patient_visible,
        patient_text_en: patient_visible
            .then(|| "Your care team is preparing your discharge plan.".to_string()),
        patient_text_es: patient_visible
            .then(|| "Su equipo está preparando su plan de alta.".to_string()),
        depends_on: vec![],
    };
    let pharmacy = ops::create_task(
        conn,
        &beds,
        &task(
            "Reconcile discharge medication",
            "Conciliar medicación de alta",
            "discharge_preparation",
            Some("pharmacist"),
            Some("pharmacy"),
            false,
            -3,
        ),
    )
    .await?;
    let transport_task = ops::create_task(
        conn,
        &beds,
        &task(
            "Confirm wheelchair transport home",
            "Confirmar transporte en silla de ruedas",
            "transport",
            Some("porter"),
            Some("transport"),
            true,
            -1,
        ),
    )
    .await?;
    ops::create_task(
        conn,
        &beds,
        &ops::TaskInput {
            depends_on: vec![pharmacy.id, transport_task.id],
            ..task(
                "Discharge education session",
                "Sesión de educación al alta",
                "education",
                Some("nurse"),
                Some("education"),
                true,
                4,
            )
        },
    )
    .await?;
    ops::create_task(
        conn,
        &beds,
        &task(
            "Home physiotherapy referral",
            "Derivación a fisioterapia domiciliaria",
            "coordination",
            Some("physiotherapist"),
            Some("social_care"),
            false,
            6,
        ),
    )
    .await?;
    ops::arm_escalation(
        conn,
        &beds,
        ops::ArmEscalation {
            facility_id: facility,
            patient_id: sofia,
            episode_id: Some(sofia_ep.id),
            visit_id: None,
            placement_request_id: None,
            kind: "discharge_delay",
            priority: "routine",
            started_at: Utc::now() - Duration::hours(5),
            threshold_minutes: Some(240),
        },
    )
    .await?;
    let _ = rehab_1;

    // --- Scenario: hospital at home + virtual care episodes -----------------
    episode(
        conn,
        &beds,
        facility,
        diego,
        "hospital_at_home",
        "hospital_at_home",
        "routine",
        vec![],
    )
    .await?;

    // --- Theatre scenarios (Diego) ------------------------------------------
    let diego_ep = episode(
        conn,
        &th,
        facility,
        diego,
        "perioperative",
        "general_surgery",
        "priority",
        vec![],
    )
    .await?;
    let now = Utc::now();
    let session_start = now + Duration::hours(1);
    let s1 = theatre::create_session(
        conn,
        &th,
        &theatre::SessionInput {
            theatre_location_id: theatre_1,
            service_code: Some("general_surgery".into()),
            specialty_codes: vec![],
            starts_at: session_start,
            ends_at: session_start + Duration::hours(6),
            turnaround_minutes: Some(30),
        },
    )
    .await?;
    theatre::create_session(
        conn,
        &th,
        &theatre::SessionInput {
            theatre_location_id: theatre_2,
            service_code: Some("dentistry".into()),
            specialty_codes: vec![],
            starts_at: session_start,
            ends_at: session_start + Duration::hours(4),
            turnaround_minutes: Some(20),
        },
    )
    .await?;
    let prereq = |code: &str, status: &str| Prerequisite {
        code: code.into(),
        status: status.into(),
        evidence_ref: None,
    };
    // Case 1: scheduled and ready in Theatre 1.
    let case_ready = theatre::create_case(conn, &th, &theatre::CaseInput {
        episode_id: diego_ep.id,
        procedure_code: "lap_cholecystectomy".into(),
        responsible_service_code: "general_surgery".into(),
        responsible_user_id: Some(dr_garcia),
        priority: Some("priority".into()),
        expected_duration_minutes: 90,
        required_professionals: json!([{"profession_code": "surgeon", "count": 1}, {"profession_code": "anaesthetist", "count": 1}, {"profession_code": "nurse", "capability_codes": ["scrub"], "count": 2}]),
        required_equipment_codes: vec!["laparoscopy_tower".into()],
        prerequisites: vec![prereq("consent", "met"), prereq("anaesthetic_review", "met"), prereq("pre_op_bloods", "met")],
        required_service_request_ids: vec![],
        checklist: json!({"who_sign_in": false}),
        post_procedure_requirements: json!({"placement_service_code": "general_surgery"}),
    }).await?;
    theatre::schedule(
        conn,
        &th,
        case_ready.id,
        &theatre::ScheduleInput {
            expected_version: case_ready.version,
            theatre_location_id: theatre_1,
            expected_theatre_version: 1,
            scheduled_start: session_start + Duration::minutes(30),
            team_user_ids: vec![dr_garcia, nurse_kim],
            override_reason: None,
        },
    )
    .await
    .ok();
    // Case 2: missing externally supplied anaesthetic review — not schedulable.
    theatre::create_case(conn, &th, &theatre::CaseInput {
        episode_id: diego_ep.id,
        procedure_code: "hip_fracture_fixation".into(),
        responsible_service_code: "general_surgery".into(),
        responsible_user_id: Some(dr_garcia),
        priority: Some("urgent".into()),
        expected_duration_minutes: 120,
        required_professionals: json!([{"profession_code": "surgeon", "count": 1}, {"profession_code": "anaesthetist", "count": 1}]),
        required_equipment_codes: vec!["c_arm".into()],
        prerequisites: vec![prereq("consent", "met"), prereq("anaesthetic_review", "pending"), prereq("blood_group_and_save", "met")],
        required_service_request_ids: vec![],
        checklist: json!({}),
        post_procedure_requirements: json!({"placement_service_code": "general_surgery"}),
    }).await?;
    // Case 3: equipment conflict — c-arm case in Theatre 2 (no c-arm).
    let case_eq = theatre::create_case(conn, &th, &theatre::CaseInput {
        episode_id: diego_ep.id,
        procedure_code: "dental_extraction_sedation".into(),
        responsible_service_code: "dentistry".into(),
        responsible_user_id: None,
        priority: Some("routine".into()),
        expected_duration_minutes: 45,
        required_professionals: json!([{"profession_code": "dentist", "count": 1}, {"profession_code": "nurse", "count": 1}]),
        required_equipment_codes: vec!["dental_unit".into(), "c_arm".into()],
        prerequisites: vec![prereq("consent", "met"), prereq("anaesthetic_review", "met")],
        required_service_request_ids: vec![],
        checklist: json!({}),
        post_procedure_requirements: json!({}),
    }).await?;
    let _ = (case_eq, s1);

    // --- Workforce scenarios -----------------------------------------------
    let tz = workforce::facility_tz(conn, tenant, facility).await?;
    let today = now.with_timezone(&tz).date_naive();
    let shift = workforce::create_shift(
        conn,
        &wf,
        &workforce::ShiftInput {
            facility_id: facility,
            unit_location_id: Some(med_ward),
            label: "Medical ward 3 — day".into(),
            local_date: today,
            local_start: NaiveTime::from_hms_opt(7, 0, 0).unwrap(),
            local_end: NaiveTime::from_hms_opt(19, 0, 0).unwrap(),
            requirements: vec![
                workforce::RequirementInput {
                    profession_code: "nurse".into(),
                    capability_codes: vec!["acls".into()],
                    specialty_codes: vec![],
                    minimum_count: 2,
                },
                workforce::RequirementInput {
                    profession_code: "physician".into(),
                    capability_codes: vec![],
                    specialty_codes: vec![],
                    minimum_count: 1,
                },
                workforce::RequirementInput {
                    profession_code: "physiotherapist".into(),
                    capability_codes: vec![],
                    specialty_codes: vec![],
                    minimum_count: 1,
                },
            ],
        },
    )
    .await?;
    workforce::add_assignment(
        conn,
        &wf,
        shift.id,
        &workforce::AssignmentInput {
            user_id: Some(nurse_kim),
            resource_id: None,
            profession_code: "nurse".into(),
            capability_codes: vec!["acls".into(), "telemetry_monitoring".into()],
            specialty_codes: vec![],
            role_in_shift: Some("assigned".into()),
            override_reason: None,
        },
    )
    .await?;
    workforce::add_assignment(
        conn,
        &wf,
        shift.id,
        &workforce::AssignmentInput {
            user_id: Some(dr_garcia),
            resource_id: None,
            profession_code: "physician".into(),
            capability_codes: vec!["acls".into()],
            specialty_codes: vec!["internal_medicine".into()],
            role_in_shift: Some("assigned".into()),
            override_reason: None,
        },
    )
    .await?;
    workforce::add_assignment(
        conn,
        &wf,
        shift.id,
        &workforce::AssignmentInput {
            user_id: Some(ops_manager),
            resource_id: None,
            profession_code: "nurse".into(),
            capability_codes: vec![],
            specialty_codes: vec![],
            role_in_shift: Some("float".into()),
            override_reason: None,
        },
    )
    .await?;
    let night = workforce::create_shift(
        conn,
        &wf,
        &workforce::ShiftInput {
            facility_id: facility,
            unit_location_id: Some(surg_ward),
            label: "Surgical ward 2 — night".into(),
            local_date: today,
            local_start: NaiveTime::from_hms_opt(19, 0, 0).unwrap(),
            local_end: NaiveTime::from_hms_opt(7, 0, 0).unwrap(),
            requirements: vec![workforce::RequirementInput {
                profession_code: "nurse".into(),
                capability_codes: vec![],
                specialty_codes: vec![],
                minimum_count: 1,
            }],
        },
    )
    .await?;
    workforce::add_assignment(
        conn,
        &wf,
        night.id,
        &workforce::AssignmentInput {
            user_id: Some(nurse_kim),
            resource_id: None,
            profession_code: "nurse".into(),
            capability_codes: vec![],
            specialty_codes: vec![],
            role_in_shift: Some("on_call".into()),
            override_reason: None,
        },
    )
    .await?;
    workforce::record_absence(
        conn,
        &wf,
        &workforce::AbsenceInput {
            user_id: dr_garcia,
            starts_at: now + Duration::hours(3),
            ends_at: now + Duration::hours(9),
            reason_code: "training".into(),
        },
    )
    .await?;

    // --- Deterministic forecasts (history is deliberately short) -----------
    for h in ["24h", "7d"] {
        forecast::create_forecast(conn, &mgr, state, facility, h, None).await?;
    }
    forecast::create_forecast(conn, &mgr, state, annex, "24h", None).await?;

    Ok(())
}
