//! Synthetic development seed data. All names, identifiers, and clinical
//! values are clearly synthetic. No real PHI anywhere.

use crate::routes::visits::single_row_vital_facts;
use dmind_gateway::triage::TRIAGE_TEMPLATE;
use dmind_gateway::{ModelGateway, SummaryRequest, TriageRequest};
use rand::RngCore;
use rust_decimal::Decimal;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;
use wellos_domain::ai::ArtifactStatus;
use wellos_domain::rules::{baseline_rules, RuleOutcome};
use wellos_domain::triage::{
    safety_floor, ArrivalKind, Priority, TriageVitals, SAFETY_RULES_VERSION, TRIAGE_PROPOSAL_SCHEMA,
};
use wellos_domain::units::Quantity;

pub struct Seeded {
    pub tenant_a: Uuid,
    pub tenant_b: Uuid,
    pub facility_a: Uuid,
    /// Second facility in tenant A, for intra-tenant facility isolation.
    pub facility_a2: Uuid,
    pub facility_b: Uuid,
    pub patient_a: Uuid,
    /// Patient in tenant A's second facility.
    pub patient_a2: Uuid,
    pub patient_b: Uuid,
    /// Development-only lab-adapter service credential (random per seed run;
    /// only its hash is stored). Real deployments issue credentials through
    /// an operational process, never through seeding.
    pub lab_adapter_token: String,
}

/// Generate a random high-entropy service credential (256 bits).
pub fn generate_service_secret() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    format!("wsk_{}", hex::encode(bytes))
}

/// Seed synthetic development data. Returns `None` when the database is
/// already seeded (concurrent seeders serialize on an advisory lock).
pub async fn seed(pool: &PgPool) -> anyhow::Result<Option<Seeded>> {
    let tenant_a = Uuid::now_v7();
    let tenant_b = Uuid::now_v7();
    let facility_a = Uuid::now_v7();
    let facility_a2 = Uuid::now_v7();
    let facility_b = Uuid::now_v7();

    let mut tx = pool.begin().await?;

    // Serialize concurrent seeders (e.g. parallel test binaries) and make
    // seeding idempotent: whoever wins the lock seeds; everyone else reuses.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('wellos_seed'))")
        .execute(&mut *tx)
        .await?;
    let (existing,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM users")
        .fetch_one(&mut *tx)
        .await?;
    if existing > 0 {
        tx.rollback().await?;
        return Ok(None);
    }

    for (id, name, brand) in [
        (
            tenant_a,
            "Hospital Demo Norte",
            serde_json::json!({
                "theme": "north",
                "primary_color": "#0f6f8f",
                "logo_text": "Hospital Demo Norte",
                "product_name": "WellOS"
            }),
        ),
        (
            tenant_b,
            "Clínica Demo Sur",
            serde_json::json!({
                "theme": "south",
                "primary_color": "#6d28d9",
                "logo_text": "Clínica Demo Sur",
                "product_name": "WellOS"
            }),
        ),
    ] {
        sqlx::query("INSERT INTO tenants (id, cell, name, brand) VALUES ($1,'cell-dev-1',$2,$3)")
            .bind(id)
            .bind(name)
            .bind(brand)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query("INSERT INTO facilities (id, tenant_id, name) VALUES ($1,$2,'Main Campus')")
        .bind(facility_a)
        .bind(tenant_a)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO facilities (id, tenant_id, name) VALUES ($1,$2,'North Annex')")
        .bind(facility_a2)
        .bind(tenant_a)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO facilities (id, tenant_id, name) VALUES ($1,$2,'Campus Sur')")
        .bind(facility_b)
        .bind(tenant_b)
        .execute(&mut *tx)
        .await?;

    // Users and roles for tenant A (all synthetic).
    let users: &[(&str, &str, &str, bool)] = &[
        (
            "reg.rivera",
            "Rosa Rivera (Registration)",
            "registration_staff",
            false,
        ),
        (
            "dr.garcia",
            "Dr. Gabriel García (Physician)",
            "physician",
            false,
        ),
        (
            "dr.lopez",
            "Dr. Lucía López (Physician)",
            "physician",
            false,
        ),
        ("nurse.kim", "Nurse Ana Kim", "nurse", false),
        (
            "lab.chen",
            "Luis Chen (Laboratory)",
            "laboratory_professional",
            false,
        ),
        ("pharm.osei", "Paula Osei (Pharmacist)", "pharmacist", false),
        (
            "admin.silva",
            "Carla Silva (Clinical Admin)",
            "clinical_administrator",
            false,
        ),
        (
            "privacy.wolf",
            "Petra Wolf (Privacy Officer)",
            "privacy_officer",
            false,
        ),
        (
            "audit.stone",
            "Sam Stone (Security Auditor)",
            "security_auditor",
            false,
        ),
        (
            "research.diaz",
            "Rafael Díaz (Research)",
            "research_user",
            false,
        ),
        (
            "portal.patient",
            "Patient Portal Placeholder",
            "patient_representative",
            false,
        ),
        (
            "svc.dmind",
            "dMind Service Agent",
            "dmind_service_agent",
            true,
        ),
        (
            "svc.lab-adapter",
            "Synthetic Lab Adapter",
            "lab_interface_agent",
            true,
        ),
    ];
    let mut lab_adapter_id = None;
    let mut dr_garcia_id = None;
    let mut nurse_kim_id = None;
    let mut reg_rivera_id = None;
    for (username, display, role, is_service) in users {
        let uid = Uuid::now_v7();
        match *username {
            "svc.lab-adapter" => lab_adapter_id = Some(uid),
            "dr.garcia" => dr_garcia_id = Some(uid),
            "nurse.kim" => nurse_kim_id = Some(uid),
            "reg.rivera" => reg_rivera_id = Some(uid),
            _ => {}
        }
        // Human users get a synthetic OIDC subject mapping so the local
        // identity record can be resolved from a validated token's `sub`.
        let oidc_subject = (!is_service).then(|| format!("synthetic|{username}"));
        sqlx::query(
            "INSERT INTO users (id, tenant_id, username, display_name, is_service, oidc_subject) VALUES ($1,$2,$3,$4,$5,$6)",
        )
        .bind(uid)
        .bind(tenant_a)
        .bind(username)
        .bind(display)
        .bind(is_service)
        .bind(oidc_subject)
        .execute(&mut *tx)
        .await?;
        // Administrative, oversight, and machine roles are explicitly
        // tenant-wide (facility_id IS NULL, allowlisted in policy);
        // ordinary clinical roles get explicit facility assignments.
        let assignment_facility =
            (!crate::policy::null_facility_is_tenant_wide(role)).then_some(facility_a);
        sqlx::query(
            "INSERT INTO role_assignments (id, tenant_id, user_id, role, facility_id) VALUES ($1,$2,$3,$4,$5)",
        )
        .bind(Uuid::now_v7())
        .bind(tenant_a)
        .bind(uid)
        .bind(role)
        .bind(assignment_facility)
        .execute(&mut *tx)
        .await?;
        // Dr. García covers both tenant-A facilities (multi-facility clinician).
        if *username == "dr.garcia" {
            sqlx::query(
                "INSERT INTO role_assignments (id, tenant_id, user_id, role, facility_id) VALUES ($1,$2,$3,$4,$5)",
            )
            .bind(Uuid::now_v7())
            .bind(tenant_a)
            .bind(uid)
            .bind(role)
            .bind(facility_a2)
            .execute(&mut *tx)
            .await?;
        }
    }
    // A physician assigned only to the second facility, to prove
    // intra-tenant facility isolation.
    let dr_annex = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO users (id, tenant_id, username, display_name, oidc_subject)
         VALUES ($1,$2,'dr.annex','Dr. Andrea Anexo (Physician, North Annex)','synthetic|dr.annex')",
    )
    .bind(dr_annex)
    .bind(tenant_a)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO role_assignments (id, tenant_id, user_id, role, facility_id) VALUES ($1,$2,$3,'physician',$4)",
    )
    .bind(Uuid::now_v7())
    .bind(tenant_a)
    .bind(dr_annex)
    .bind(facility_a2)
    .execute(&mut *tx)
    .await?;
    // An emergency clinician explicitly authorized for break-glass access
    // (least privilege: ordinary physicians cannot self-assert emergencies).
    let dr_emergency = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO users (id, tenant_id, username, display_name, oidc_subject)
         VALUES ($1,$2,'dr.emergency','Dr. Elena Emergencia (Emergency)','synthetic|dr.emergency')",
    )
    .bind(dr_emergency)
    .bind(tenant_a)
    .execute(&mut *tx)
    .await?;
    for role in ["physician", "break_glass_authorized"] {
        sqlx::query(
            "INSERT INTO role_assignments (id, tenant_id, user_id, role, facility_id) VALUES ($1,$2,$3,$4,$5)",
        )
        .bind(Uuid::now_v7())
        .bind(tenant_a)
        .bind(dr_emergency)
        .bind(role)
        .bind(facility_a)
        .execute(&mut *tx)
        .await?;
    }

    // Development-only service credential for the synthetic lab adapter.
    let lab_adapter_token = generate_service_secret();
    sqlx::query(
        "INSERT INTO service_credentials (id, tenant_id, user_id, name, token_hash, scopes, expires_at)
         VALUES ($1,$2,$3,'synthetic lab adapter (dev)',$4,$5, now() + interval '90 days')",
    )
    .bind(Uuid::now_v7())
    .bind(tenant_a)
    .bind(lab_adapter_id.expect("lab adapter user seeded"))
    .bind(crate::auth::hash_service_secret(&lab_adapter_token))
    .bind(vec!["result.ingest".to_string(), "worklist.read".to_string()])
    .execute(&mut *tx)
    .await?;

    // A physician in tenant B, to prove cross-tenant denial.
    let dr_b = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO users (id, tenant_id, username, display_name) VALUES ($1,$2,'dr.sur','Dr. Sur (Tenant B)')",
    )
    .bind(dr_b)
    .bind(tenant_b)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO role_assignments (id, tenant_id, user_id, role, facility_id) VALUES ($1,$2,$3,'physician',$4)",
    )
    .bind(Uuid::now_v7())
    .bind(tenant_b)
    .bind(dr_b)
    .bind(facility_b)
    .execute(&mut *tx)
    .await?;

    // Synthetic patients.
    let patient_a = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO patients (id, tenant_id, facility_id, family_name, given_name, birth_date, sex, identifier)
         VALUES ($1,$2,$3,'Demopatient','Alba','1980-04-12','female','SYN-0001')",
    )
    .bind(patient_a)
    .bind(tenant_a)
    .bind(facility_a)
    .execute(&mut *tx)
    .await?;
    let patient_a2 = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO patients (id, tenant_id, facility_id, family_name, given_name, birth_date, sex, identifier)
         VALUES ($1,$2,$3,'Demopatient','Anexa','1990-01-15','female','SYN-0002')",
    )
    .bind(patient_a2)
    .bind(tenant_a)
    .bind(facility_a2)
    .execute(&mut *tx)
    .await?;
    let patient_b = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO patients (id, tenant_id, facility_id, family_name, given_name, birth_date, sex, identifier)
         VALUES ($1,$2,$3,'Demopaciente','Bruno','1975-09-30','male','SYN-1001')",
    )
    .bind(patient_b)
    .bind(tenant_b)
    .bind(facility_b)
    .execute(&mut *tx)
    .await?;

    // Clinical context for patient A (synthetic).
    sqlx::query(
        "INSERT INTO allergies (id, tenant_id, patient_id, substance, criticality)
         VALUES ($1,$2,$3,'Penicillin','high')",
    )
    .bind(Uuid::now_v7())
    .bind(tenant_a)
    .bind(patient_a)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO medications (id, tenant_id, patient_id, name)
         VALUES ($1,$2,$3,'Lisinopril 10 mg daily (synthetic)')",
    )
    .bind(Uuid::now_v7())
    .bind(tenant_a)
    .bind(patient_a)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO conditions (id, tenant_id, patient_id, code, display)
         VALUES ($1,$2,$3,'I10','Essential hypertension (synthetic)')",
    )
    .bind(Uuid::now_v7())
    .bind(tenant_a)
    .bind(patient_a)
    .execute(&mut *tx)
    .await?;

    // Consents: care delivery active; external AI processing revoked by default.
    for (purpose, status) in [
        ("care_delivery", "active"),
        ("ai_external_processing", "revoked"),
    ] {
        sqlx::query(
            "INSERT INTO consents (id, tenant_id, patient_id, purpose, status) VALUES ($1,$2,$3,$4,$5)",
        )
        .bind(Uuid::now_v7())
        .bind(tenant_a)
        .bind(patient_a)
        .bind(purpose)
        .bind(status)
        .execute(&mut *tx)
        .await?;
    }

    // Demo clinical states: enough synthetic loops to show the workspace in
    // every workflow stage without hand-driving the lab adapter first.
    let dr_garcia = dr_garcia_id.expect("dr.garcia seeded");
    let demo = seed_demo_states(&mut tx, tenant_a, facility_a, patient_a, dr_garcia).await?;

    // Service queues exist for every facility so arrivals always route
    // somewhere explicit, even where no demo visit is seeded.
    for (facility, tenant, codes) in [
        (
            facility_a,
            tenant_a,
            &["general_medicine", "emergency", "nursing", "telehealth"][..],
        ),
        (facility_a2, tenant_a, &["general_medicine", "nursing"][..]),
        (facility_b, tenant_b, &["general_medicine"][..]),
    ] {
        for code in codes {
            sqlx::query(
                "INSERT INTO service_queues (id, tenant_id, facility_id, code, name) VALUES ($1,$2,$3,$4,$5)",
            )
            .bind(Uuid::now_v7())
            .bind(tenant)
            .bind(facility)
            .bind(code)
            .bind(queue_name(code))
            .execute(&mut *tx)
            .await?;
        }
    }
    seed_access_triage(
        &mut tx,
        AccessSeed {
            tenant: tenant_a,
            facility: facility_a,
            annex: facility_a2,
            registration: reg_rivera_id.expect("reg.rivera seeded"),
            nurse: nurse_kim_id.expect("nurse.kim seeded"),
            physician: dr_garcia,
            alba: patient_a,
            anexa: patient_a2,
            carlos: demo.carlos,
            marta: demo.marta,
            jonas: demo.jonas,
            alba_draft_encounter: demo.alba_draft_encounter,
        },
    )
    .await?;

    tx.commit().await?;
    Ok(Some(Seeded {
        tenant_a,
        tenant_b,
        facility_a,
        facility_a2,
        facility_b,
        patient_a,
        patient_a2,
        patient_b,
        lab_adapter_token,
    }))
}

/// Loop stages a demo service request can be seeded into. Versions mirror the
/// production transition sequence (ordered=1, received=2, reviewed=3,
/// notified=4, closed=5).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum DemoStage {
    /// Ordered, no result yet (pending in the diagnostic history).
    Ordered,
    Received,
    Reviewed,
    Notified,
    Closed,
}

impl DemoStage {
    fn loop_state(self) -> &'static str {
        match self {
            DemoStage::Ordered => "ordered",
            DemoStage::Received => "received",
            DemoStage::Reviewed => "reviewed",
            DemoStage::Notified => "notified",
            DemoStage::Closed => "closed",
        }
    }

    fn version(self) -> i64 {
        match self {
            DemoStage::Ordered => 1,
            DemoStage::Received => 2,
            DemoStage::Reviewed => 3,
            DemoStage::Notified => 4,
            DemoStage::Closed => 5,
        }
    }
}

struct DemoLoopSpec {
    patient_id: Uuid,
    code_loinc: &'static str,
    display: &'static str,
    value: Decimal,
    unit: &'static str,
    reference_range: &'static str,
    stage: DemoStage,
    hours_ago: i64,
}

/// Seed synthetic patients and closed-loop service requests in every workflow
/// stage so the clinical workspace is demonstrable immediately after setup.
/// Rows mirror what the lab-ingest pipeline writes: deterministic rule
/// evaluations come from the shared versioned rules and AI artifacts from the
/// deterministic development provider.
/// Identifiers of the demo records later seed stages build on.
struct DemoRecords {
    carlos: Uuid,
    marta: Uuid,
    jonas: Uuid,
    alba_draft_encounter: Uuid,
}

async fn seed_demo_states(
    tx: &mut PgConnection,
    tenant: Uuid,
    facility: Uuid,
    patient_alba: Uuid,
    practitioner: Uuid,
) -> anyhow::Result<DemoRecords> {
    let mut demo_patients: Vec<Uuid> = Vec::new();
    for (family, given, birth, sex, mrn) in [
        ("Demopatient", "Carlos", "1962-07-08", "male", "SYN-0003"),
        ("Demopatient", "Marta", "1988-11-21", "female", "SYN-0004"),
        ("Demopatient", "Jonás", "1955-02-03", "male", "SYN-0005"),
    ] {
        let pid = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO patients (id, tenant_id, facility_id, family_name, given_name, birth_date, sex, identifier)
             VALUES ($1,$2,$3,$4,$5,$6::date,$7,$8)",
        )
        .bind(pid)
        .bind(tenant)
        .bind(facility)
        .bind(family)
        .bind(given)
        .bind(birth)
        .bind(sex)
        .bind(mrn)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO consents (id, tenant_id, patient_id, purpose, status) VALUES ($1,$2,$3,'care_delivery','active')",
        )
        .bind(Uuid::now_v7())
        .bind(tenant)
        .bind(pid)
        .execute(&mut *tx)
        .await?;
        demo_patients.push(pid);
    }
    let (carlos, marta, jonas) = (demo_patients[0], demo_patients[1], demo_patients[2]);

    for (pid, substance, criticality) in [(carlos, "Sulfonamides", "high"), (marta, "Latex", "low")]
    {
        sqlx::query(
            "INSERT INTO allergies (id, tenant_id, patient_id, substance, criticality) VALUES ($1,$2,$3,$4,$5)",
        )
        .bind(Uuid::now_v7())
        .bind(tenant)
        .bind(pid)
        .bind(substance)
        .bind(criticality)
        .execute(&mut *tx)
        .await?;
    }
    for (pid, name) in [
        (carlos, "Metformin 850 mg twice daily (synthetic)"),
        (marta, "Levothyroxine 50 µg daily (synthetic)"),
        (jonas, "Atorvastatin 20 mg nightly (synthetic)"),
    ] {
        sqlx::query(
            "INSERT INTO medications (id, tenant_id, patient_id, name) VALUES ($1,$2,$3,$4)",
        )
        .bind(Uuid::now_v7())
        .bind(tenant)
        .bind(pid)
        .bind(name)
        .execute(&mut *tx)
        .await?;
    }
    for (pid, code, display) in [
        (carlos, "E11", "Type 2 diabetes mellitus (synthetic)"),
        (marta, "E03", "Hypothyroidism (synthetic)"),
        (jonas, "I25", "Chronic ischemic heart disease (synthetic)"),
    ] {
        sqlx::query(
            "INSERT INTO conditions (id, tenant_id, patient_id, code, display) VALUES ($1,$2,$3,$4,$5)",
        )
        .bind(Uuid::now_v7())
        .bind(tenant)
        .bind(pid)
        .bind(code)
        .bind(display)
        .execute(&mut *tx)
        .await?;
    }

    let specs = [
        // Critical result awaiting clinician review.
        DemoLoopSpec {
            patient_id: carlos,
            code_loinc: "2823-3",
            display: "Potassium [Moles/volume] in Serum",
            value: Decimal::new(68, 1), // 6.8 mmol/L (critical high)
            unit: "mmol/L",
            reference_range: "3.5-5.1 mmol/L",
            stage: DemoStage::Received,
            hours_ago: 2,
        },
        // Reviewed critical result awaiting patient notification.
        DemoLoopSpec {
            patient_id: marta,
            code_loinc: "2345-7",
            display: "Glucose [Mass/volume] in Serum",
            value: Decimal::new(38, 0), // 38 mg/dL (critical low)
            unit: "mg/dL",
            reference_range: "70-99 mg/dL",
            stage: DemoStage::Reviewed,
            hours_ago: 8,
        },
        // Fully closed critical loop.
        DemoLoopSpec {
            patient_id: jonas,
            code_loinc: "2823-3",
            display: "Potassium [Moles/volume] in Serum",
            value: Decimal::new(72, 1), // 7.2 mmol/L (critical high)
            unit: "mmol/L",
            reference_range: "3.5-5.1 mmol/L",
            stage: DemoStage::Closed,
            hours_ago: 30,
        },
        // Routine laboratory history for Alba (normal, closed).
        DemoLoopSpec {
            patient_id: patient_alba,
            code_loinc: "2823-3",
            display: "Potassium [Moles/volume] in Serum",
            value: Decimal::new(41, 1), // 4.1 mmol/L (normal)
            unit: "mmol/L",
            reference_range: "3.5-5.1 mmol/L",
            stage: DemoStage::Closed,
            hours_ago: 72,
        },
        // A second normal analyte for Alba's history.
        DemoLoopSpec {
            patient_id: patient_alba,
            code_loinc: "2345-7",
            display: "Glucose [Mass/volume] in Serum",
            value: Decimal::new(92, 0), // 92 mg/dL (normal)
            unit: "mg/dL",
            reference_range: "70-99 mg/dL",
            stage: DemoStage::Closed,
            hours_ago: 168,
        },
        // Carlos: rising, above-range (non-critical) glucose series for the
        // diagnostic trend view, plus a repeat glucose still pending.
        DemoLoopSpec {
            patient_id: carlos,
            code_loinc: "2345-7",
            display: "Glucose [Mass/volume] in Serum",
            value: Decimal::new(118, 0),
            unit: "mg/dL",
            reference_range: "70-99 mg/dL",
            stage: DemoStage::Closed,
            hours_ago: 24 * 30,
        },
        DemoLoopSpec {
            patient_id: carlos,
            code_loinc: "2345-7",
            display: "Glucose [Mass/volume] in Serum",
            value: Decimal::new(126, 0),
            unit: "mg/dL",
            reference_range: "70-99 mg/dL",
            stage: DemoStage::Closed,
            hours_ago: 24 * 14,
        },
        DemoLoopSpec {
            patient_id: carlos,
            code_loinc: "2345-7",
            display: "Glucose [Mass/volume] in Serum",
            value: Decimal::new(134, 0),
            unit: "mg/dL",
            reference_range: "70-99 mg/dL",
            stage: DemoStage::Closed,
            hours_ago: 24 * 7,
        },
        DemoLoopSpec {
            patient_id: carlos,
            code_loinc: "2345-7",
            display: "Glucose [Mass/volume] in Serum",
            value: Decimal::ZERO,
            unit: "mg/dL",
            reference_range: "70-99 mg/dL",
            stage: DemoStage::Ordered,
            hours_ago: 20,
        },
        // Jonás: two earlier normal potassium values so the closed critical
        // result above reads as a rising series.
        DemoLoopSpec {
            patient_id: jonas,
            code_loinc: "2823-3",
            display: "Potassium [Moles/volume] in Serum",
            value: Decimal::new(44, 1),
            unit: "mmol/L",
            reference_range: "3.5-5.1 mmol/L",
            stage: DemoStage::Closed,
            hours_ago: 24 * 60,
        },
        DemoLoopSpec {
            patient_id: jonas,
            code_loinc: "2823-3",
            display: "Potassium [Moles/volume] in Serum",
            value: Decimal::new(49, 1),
            unit: "mmol/L",
            reference_range: "3.5-5.1 mmol/L",
            stage: DemoStage::Closed,
            hours_ago: 24 * 20,
        },
    ];

    for spec in specs {
        seed_demo_loop(&mut *tx, tenant, facility, practitioner, &spec).await?;
    }

    let alba_draft_encounter = seed_demo_encounters(
        &mut *tx,
        tenant,
        facility,
        practitioner,
        patient_alba,
        carlos,
        marta,
    )
    .await?;
    Ok(DemoRecords {
        carlos,
        marta,
        jonas,
        alba_draft_encounter,
    })
}

/// Seed consultation documentation in every lifecycle state: a partially
/// documented draft (Alba), a signed encounter with vitals, a diagnosis and a
/// plan (Carlos), and a signed encounter with a later addendum (Marta). Jonás
/// remains ready for a fresh consultation.
async fn seed_demo_encounters(
    tx: &mut PgConnection,
    tenant: Uuid,
    facility: Uuid,
    practitioner: Uuid,
    alba: Uuid,
    carlos: Uuid,
    marta: Uuid,
) -> anyhow::Result<Uuid> {
    let now = chrono::Utc::now();

    // 1) Partially documented draft consultation, still in progress.
    let draft_enc = Uuid::now_v7();
    let draft_started = now - chrono::Duration::hours(1);
    sqlx::query(
        "INSERT INTO encounters (id, tenant_id, facility_id, patient_id, practitioner_id,
                                 status, encounter_type, started_at)
         VALUES ($1,$2,$3,$4,$5,'in_progress','consultation',$6)",
    )
    .bind(draft_enc)
    .bind(tenant)
    .bind(facility)
    .bind(alba)
    .bind(practitioner)
    .bind(draft_started)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO encounter_notes
         (id, tenant_id, encounter_id, patient_id, author_id, status, version,
          reason_for_encounter, history_present_illness, created_at, updated_at)
         VALUES ($1,$2,$3,$4,$5,'draft',2,
                 'Follow-up of hypertension control (synthetic)',
                 'Reports good adherence; occasional morning headaches over the last two weeks (synthetic).',
                 $6,$6)",
    )
    .bind(Uuid::now_v7())
    .bind(tenant)
    .bind(draft_enc)
    .bind(alba)
    .bind(practitioner)
    .bind(draft_started)
    .execute(&mut *tx)
    .await?;
    seed_vitals(
        &mut *tx,
        tenant,
        draft_enc,
        alba,
        practitioner,
        draft_started,
        (148, 92, 78, 14, "36.8", 98, "82.5", 168),
    )
    .await?;

    // 2) Signed consultation with vitals, diagnosis and plan.
    let signed_enc = Uuid::now_v7();
    let signed_started = now - chrono::Duration::hours(50);
    let signed_at = signed_started + chrono::Duration::minutes(40);
    sqlx::query(
        "INSERT INTO encounters (id, tenant_id, facility_id, patient_id, practitioner_id,
                                 status, encounter_type, started_at, completed_at)
         VALUES ($1,$2,$3,$4,$5,'completed','consultation',$6,$7)",
    )
    .bind(signed_enc)
    .bind(tenant)
    .bind(facility)
    .bind(carlos)
    .bind(practitioner)
    .bind(signed_started)
    .bind(signed_at)
    .execute(&mut *tx)
    .await?;
    let signed_note = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO encounter_notes
         (id, tenant_id, encounter_id, patient_id, author_id, status, version,
          reason_for_encounter, history_present_illness, physical_exam, assessment, plan,
          follow_up, created_at, updated_at, signed_at, signed_by)
         VALUES ($1,$2,$3,$4,$5,'signed',3,
                 'Diabetes review and fatigue (synthetic)',
                 'Three weeks of fatigue and increased thirst; no fever (synthetic).',
                 'Alert, well hydrated; cardiopulmonary examination unremarkable (synthetic).',
                 'Suboptimal glycaemic control in known type 2 diabetes (synthetic).',
                 'Reinforce diet and adherence; repeat glucose and HbA1c; review in 2 weeks (synthetic).',
                 'Return earlier if symptoms worsen (synthetic).',
                 $6,$7,$7,$5)",
    )
    .bind(signed_note)
    .bind(tenant)
    .bind(signed_enc)
    .bind(carlos)
    .bind(practitioner)
    .bind(signed_started)
    .bind(signed_at)
    .execute(&mut *tx)
    .await?;
    seed_vitals(
        &mut *tx,
        tenant,
        signed_enc,
        carlos,
        practitioner,
        signed_started + chrono::Duration::minutes(5),
        (132, 84, 88, 16, "36.6", 97, "91.0", 175),
    )
    .await?;
    sqlx::query(
        "INSERT INTO conditions (id, tenant_id, patient_id, code, display, clinical_status,
                                 encounter_id, recorded_by, recorded_at)
         VALUES ($1,$2,$3,'E11.9','Type 2 diabetes, suboptimal control (synthetic)','active',$4,$5,$6)",
    )
    .bind(Uuid::now_v7())
    .bind(tenant)
    .bind(carlos)
    .bind(signed_enc)
    .bind(practitioner)
    .bind(signed_at)
    .execute(&mut *tx)
    .await?;

    // 3) Signed consultation with a later dated addendum.
    let amended_enc = Uuid::now_v7();
    let amended_started = now - chrono::Duration::hours(120);
    let amended_signed_at = amended_started + chrono::Duration::minutes(35);
    sqlx::query(
        "INSERT INTO encounters (id, tenant_id, facility_id, patient_id, practitioner_id,
                                 status, encounter_type, started_at, completed_at)
         VALUES ($1,$2,$3,$4,$5,'completed','consultation',$6,$7)",
    )
    .bind(amended_enc)
    .bind(tenant)
    .bind(facility)
    .bind(marta)
    .bind(practitioner)
    .bind(amended_started)
    .bind(amended_signed_at)
    .execute(&mut *tx)
    .await?;
    let amended_note = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO encounter_notes
         (id, tenant_id, encounter_id, patient_id, author_id, status, version,
          reason_for_encounter, history_present_illness, assessment, plan,
          created_at, updated_at, signed_at, signed_by)
         VALUES ($1,$2,$3,$4,$5,'signed',2,
                 'Dizziness after skipped meals (synthetic)',
                 'Two episodes of light-headedness before lunch this week (synthetic).',
                 'Probable hypoglycaemic episodes; thyroid replacement stable (synthetic).',
                 'Regular meal schedule; capillary glucose diary; repeat glucose testing (synthetic).',
                 $6,$7,$7,$5)",
    )
    .bind(amended_note)
    .bind(tenant)
    .bind(amended_enc)
    .bind(marta)
    .bind(practitioner)
    .bind(amended_started)
    .bind(amended_signed_at)
    .execute(&mut *tx)
    .await?;
    seed_vitals(
        &mut *tx,
        tenant,
        amended_enc,
        marta,
        practitioner,
        amended_started + chrono::Duration::minutes(5),
        (118, 74, 72, 14, "36.5", 99, "63.0", 165),
    )
    .await?;
    sqlx::query(
        "INSERT INTO encounter_note_addenda (id, tenant_id, note_id, author_id, body, created_at)
         VALUES ($1,$2,$3,$4,
                 'Addendum: laboratory glucose from the same day returned critically low; patient contacted and follow-up arranged (synthetic).',
                 $5)",
    )
    .bind(Uuid::now_v7())
    .bind(tenant)
    .bind(amended_note)
    .bind(practitioner)
    .bind(amended_signed_at + chrono::Duration::hours(6))
    .execute(&mut *tx)
    .await?;
    Ok(draft_enc)
}

fn queue_name(code: &str) -> &'static str {
    match code {
        "emergency" => "Emergency",
        "nursing" => "Nursing",
        "telehealth" => "Telehealth",
        _ => "General medicine",
    }
}

struct AccessSeed {
    tenant: Uuid,
    facility: Uuid,
    annex: Uuid,
    registration: Uuid,
    nurse: Uuid,
    physician: Uuid,
    alba: Uuid,
    anexa: Uuid,
    carlos: Uuid,
    marta: Uuid,
    jonas: Uuid,
    alba_draft_encounter: Uuid,
}

struct VisitSeed<'a> {
    id: Uuid,
    facility: Uuid,
    patient: Uuid,
    status: &'a str,
    arrival_kind: ArrivalKind,
    service: &'a str,
    reason: &'a str,
    scheduled_at: Option<chrono::DateTime<chrono::Utc>>,
    arrived_at: Option<chrono::DateTime<chrono::Utc>>,
    triage_started_at: Option<chrono::DateTime<chrono::Utc>>,
    ready_at: Option<chrono::DateTime<chrono::Utc>>,
    consultation_started_at: Option<chrono::DateTime<chrono::Utc>>,
    closed_at: Option<chrono::DateTime<chrono::Utc>>,
    closed_reason: Option<&'a str>,
    priority: Option<Priority>,
    handoff_summary: Option<&'a str>,
    encounter_id: Option<Uuid>,
    version: i64,
    created_by: Uuid,
}

async fn insert_visit(
    tx: &mut PgConnection,
    tenant: Uuid,
    v: &VisitSeed<'_>,
) -> anyhow::Result<()> {
    let created_at = v
        .arrived_at
        .or(v.scheduled_at.map(|s| s - chrono::Duration::days(1)))
        .unwrap_or_else(chrono::Utc::now);
    sqlx::query(
        "INSERT INTO visits
         (id, tenant_id, facility_id, patient_id, status, arrival_kind, service, reason,
          scheduled_at, arrived_at, triage_started_at, ready_at, consultation_started_at,
          closed_at, closed_reason, priority, handoff_summary, encounter_id, version,
          created_by, created_at, updated_at)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$21)",
    )
    .bind(v.id)
    .bind(tenant)
    .bind(v.facility)
    .bind(v.patient)
    .bind(v.status)
    .bind(v.arrival_kind.as_str())
    .bind(v.service)
    .bind(v.reason)
    .bind(v.scheduled_at)
    .bind(v.arrived_at)
    .bind(v.triage_started_at)
    .bind(v.ready_at)
    .bind(v.consultation_started_at)
    .bind(v.closed_at)
    .bind(v.closed_reason)
    .bind(v.priority.map(|p| p.as_str()))
    .bind(v.handoff_summary)
    .bind(v.encounter_id)
    .bind(v.version)
    .bind(v.created_by)
    .bind(created_at)
    .execute(&mut *tx)
    .await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn insert_care_team(
    tx: &mut PgConnection,
    tenant: Uuid,
    v: &VisitSeed<'_>,
    assignee: Option<Uuid>,
    queue: Option<Uuid>,
    function: &str,
    source: &str,
    assigned_by: Uuid,
    active: bool,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO care_team_assignments
         (id, tenant_id, facility_id, patient_id, visit_id, assignee_user_id, queue_id,
          function, active, ends_at, source, assigned_by)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,CASE WHEN $9 THEN NULL ELSE now() END,$10,$11)",
    )
    .bind(Uuid::now_v7())
    .bind(tenant)
    .bind(v.facility)
    .bind(v.patient)
    .bind(v.id)
    .bind(assignee)
    .bind(queue)
    .bind(function)
    .bind(active)
    .bind(source)
    .bind(assigned_by)
    .execute(&mut *tx)
    .await?;
    Ok(())
}

async fn queue_id(
    tx: &mut PgConnection,
    tenant: Uuid,
    facility: Uuid,
    code: &str,
) -> anyhow::Result<Uuid> {
    Ok(sqlx::query_scalar(
        "SELECT id FROM service_queues WHERE tenant_id = $1 AND facility_id = $2 AND code = $3",
    )
    .bind(tenant)
    .bind(facility)
    .bind(code)
    .fetch_one(&mut *tx)
    .await?)
}

/// Patient-access demo states: a scheduled appointment, a remote appointment
/// at the annex, a walk-in awaiting triage, an urgent arrival with an open
/// queue alert, a triage in progress with a dMind proposal awaiting review, a
/// patient ready for consultation with an accepted proposal and a directed
/// alert to the treating physician, the in-progress consultation linked to
/// Alba's draft encounter, and one cancelled visit for history.
async fn seed_access_triage(tx: &mut PgConnection, s: AccessSeed) -> anyhow::Result<()> {
    let now = chrono::Utc::now();
    let general = queue_id(tx, s.tenant, s.facility, "general_medicine").await?;
    let emergency = queue_id(tx, s.tenant, s.facility, "emergency").await?;

    let mut extra: Vec<Uuid> = Vec::new();
    for (given, birth, sex, mrn) in [
        ("Sofía", "1996-03-14", "female", "SYN-0006"),
        ("Diego", "1970-10-02", "male", "SYN-0007"),
    ] {
        let pid = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO patients (id, tenant_id, facility_id, family_name, given_name, birth_date, sex, identifier)
             VALUES ($1,$2,$3,'Demopatient',$4,$5::date,$6,$7)",
        )
        .bind(pid)
        .bind(s.tenant)
        .bind(s.facility)
        .bind(given)
        .bind(birth)
        .bind(sex)
        .bind(mrn)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO consents (id, tenant_id, patient_id, purpose, status) VALUES ($1,$2,$3,'care_delivery','active')",
        )
        .bind(Uuid::now_v7())
        .bind(s.tenant)
        .bind(pid)
        .execute(&mut *tx)
        .await?;
        extra.push(pid);
    }
    let (sofia, diego) = (extra[0], extra[1]);
    sqlx::query(
        "INSERT INTO allergies (id, tenant_id, patient_id, substance, criticality) VALUES ($1,$2,$3,'Aspirin','high')",
    )
    .bind(Uuid::now_v7())
    .bind(s.tenant)
    .bind(sofia)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO conditions (id, tenant_id, patient_id, code, display) VALUES ($1,$2,$3,'J44','Chronic obstructive pulmonary disease (synthetic)')",
    )
    .bind(Uuid::now_v7())
    .bind(s.tenant)
    .bind(diego)
    .execute(&mut *tx)
    .await?;

    // 1) Scheduled appointment later today (Carlos).
    let scheduled = VisitSeed {
        id: Uuid::now_v7(),
        facility: s.facility,
        patient: s.carlos,
        status: "scheduled",
        arrival_kind: ArrivalKind::Scheduled,
        service: "general_medicine",
        reason: "Diabetes follow-up and repeat glucose (synthetic)",
        scheduled_at: Some(now + chrono::Duration::hours(2)),
        arrived_at: None,
        triage_started_at: None,
        ready_at: None,
        consultation_started_at: None,
        closed_at: None,
        closed_reason: None,
        priority: None,
        handoff_summary: None,
        encounter_id: None,
        version: 1,
        created_by: s.registration,
    };
    insert_visit(tx, s.tenant, &scheduled).await?;

    // 2) Remote (teleconsultation) appointment tomorrow at the annex (Anexa).
    let remote = VisitSeed {
        id: Uuid::now_v7(),
        facility: s.annex,
        patient: s.anexa,
        status: "scheduled",
        arrival_kind: ArrivalKind::Remote,
        service: "telehealth",
        reason: "Teleconsultation: medication review (synthetic)",
        scheduled_at: Some(now + chrono::Duration::hours(26)),
        arrived_at: None,
        triage_started_at: None,
        ready_at: None,
        consultation_started_at: None,
        closed_at: None,
        closed_reason: None,
        priority: None,
        handoff_summary: None,
        encounter_id: None,
        version: 1,
        created_by: s.registration,
    };
    insert_visit(tx, s.tenant, &remote).await?;

    // 3) Walk-in arrived 25 minutes ago, awaiting triage (Marta).
    let walk_in = VisitSeed {
        id: Uuid::now_v7(),
        facility: s.facility,
        patient: s.marta,
        status: "arrived",
        arrival_kind: ArrivalKind::WalkIn,
        service: "general_medicine",
        reason: "Persistent cough for a week (synthetic)",
        scheduled_at: None,
        arrived_at: Some(now - chrono::Duration::minutes(25)),
        triage_started_at: None,
        ready_at: None,
        consultation_started_at: None,
        closed_at: None,
        closed_reason: None,
        priority: None,
        handoff_summary: None,
        encounter_id: None,
        version: 1,
        created_by: s.registration,
    };
    insert_visit(tx, s.tenant, &walk_in).await?;
    insert_care_team(
        tx,
        s.tenant,
        &walk_in,
        None,
        Some(general),
        "destination_queue",
        "registration",
        s.registration,
        true,
    )
    .await?;

    // 4) Urgent arrival 10 minutes ago, routed to the emergency queue with an
    //    open urgent-arrival alert (Sofía).
    let urgent = VisitSeed {
        id: Uuid::now_v7(),
        facility: s.facility,
        patient: sofia,
        status: "arrived",
        arrival_kind: ArrivalKind::Urgent,
        service: "emergency",
        reason: "Chest pain since this morning (synthetic)",
        scheduled_at: None,
        arrived_at: Some(now - chrono::Duration::minutes(10)),
        triage_started_at: None,
        ready_at: None,
        consultation_started_at: None,
        closed_at: None,
        closed_reason: None,
        priority: None,
        handoff_summary: None,
        encounter_id: None,
        version: 1,
        created_by: s.registration,
    };
    insert_visit(tx, s.tenant, &urgent).await?;
    insert_care_team(
        tx,
        s.tenant,
        &urgent,
        None,
        Some(emergency),
        "destination_queue",
        "registration",
        s.registration,
        true,
    )
    .await?;
    sqlx::query(
        "INSERT INTO internal_alerts
         (id, tenant_id, facility_id, patient_id, visit_id, kind, priority, target_queue_id, created_by, created_at)
         VALUES ($1,$2,$3,$4,$5,'urgent_arrival','urgent',$6,$7,$8)",
    )
    .bind(Uuid::now_v7())
    .bind(s.tenant)
    .bind(s.facility)
    .bind(sofia)
    .bind(urgent.id)
    .bind(emergency)
    .bind(s.registration)
    .bind(now - chrono::Duration::minutes(10))
    .execute(&mut *tx)
    .await?;

    // 5) Triage in progress (Diego): assessment saved by the nurse with
    //    visit-linked vitals (SpO₂ 93 % → deterministic floor 'urgent') and a
    //    dMind proposal awaiting the nurse's decision.
    let triage_started = now - chrono::Duration::minutes(12);
    let in_triage = VisitSeed {
        id: Uuid::now_v7(),
        facility: s.facility,
        patient: diego,
        status: "triage_in_progress",
        arrival_kind: ArrivalKind::WalkIn,
        service: "general_medicine",
        reason: "Shortness of breath and fever (synthetic)",
        scheduled_at: None,
        arrived_at: Some(now - chrono::Duration::minutes(35)),
        triage_started_at: Some(triage_started),
        ready_at: None,
        consultation_started_at: None,
        closed_at: None,
        closed_reason: None,
        priority: None,
        handoff_summary: None,
        encounter_id: None,
        version: 2,
        created_by: s.registration,
    };
    insert_visit(tx, s.tenant, &in_triage).await?;
    insert_care_team(
        tx,
        s.tenant,
        &in_triage,
        None,
        Some(general),
        "destination_queue",
        "registration",
        s.registration,
        true,
    )
    .await?;
    insert_care_team(
        tx,
        s.tenant,
        &in_triage,
        Some(s.nurse),
        None,
        "triage_nurse",
        "triage",
        s.nurse,
        true,
    )
    .await?;
    let diego_vitals = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO vital_signs
         (id, tenant_id, visit_id, patient_id, recorded_by, systolic_mmhg, diastolic_mmhg,
          heart_rate_bpm, respiratory_rate_bpm, temperature_c, spo2_percent, recorded_at)
         VALUES ($1,$2,$3,$4,$5,138,86,104,24,38.4,93,$6)",
    )
    .bind(diego_vitals)
    .bind(s.tenant)
    .bind(in_triage.id)
    .bind(diego)
    .bind(s.nurse)
    .bind(triage_started)
    .execute(&mut *tx)
    .await?;
    let diego_triage_vitals = TriageVitals {
        systolic_mmhg: Some(Decimal::from(138)),
        heart_rate_bpm: Some(Decimal::from(104)),
        respiratory_rate_bpm: Some(Decimal::from(24)),
        temperature_c: Some(Decimal::new(384, 1)),
        spo2_percent: Some(Decimal::from(93)),
    };
    let diego_concerns = vec!["shortness_of_breath".to_string(), "fever".to_string()];
    let (diego_floor, diego_rules) = safety_floor(ArrivalKind::WalkIn, &[], &diego_triage_vitals);
    sqlx::query(
        "INSERT INTO triage_assessments
         (id, tenant_id, visit_id, patient_id, author_id, version, reason, concerns, onset,
          red_flags, note, vital_signs_id, priority, safety_floor, safety_rules, rules_version,
          requested_service, created_at, updated_at)
         VALUES ($1,$2,$3,$4,$5,1,$6,$7,'2 days','[]'::jsonb,$8,$9,$10,$11,$12,$13,'general_medicine',$14,$14)",
    )
    .bind(Uuid::now_v7())
    .bind(s.tenant)
    .bind(in_triage.id)
    .bind(diego)
    .bind(s.nurse)
    .bind(in_triage.reason)
    .bind(serde_json::to_value(&diego_concerns)?)
    .bind("Known COPD; productive cough, speaking full sentences (synthetic).")
    .bind(diego_vitals)
    .bind(diego_floor.as_str())
    .bind(diego_floor.as_str())
    .bind(serde_json::to_value(&diego_rules)?)
    .bind(SAFETY_RULES_VERSION)
    .bind(triage_started)
    .execute(&mut *tx)
    .await?;
    let diego_proposal = seed_triage_proposal(
        tx,
        s.tenant,
        &in_triage,
        TriageRequest {
            template: TRIAGE_TEMPLATE.to_string(),
            language: "en".to_string(),
            arrival_kind: ArrivalKind::WalkIn,
            age_years: Some(55),
            reason: Some(in_triage.reason.to_string()),
            concerns: diego_concerns,
            onset: Some("2 days".to_string()),
            red_flags: vec![],
            allergies: vec![],
            requested_service: Some("general_medicine".to_string()),
            facts: [
                ("visit.arrival_kind".to_string(), "walk_in".to_string()),
                ("triage.version".to_string(), "1".to_string()),
            ]
            .into_iter()
            .chain(single_row_vital_facts(diego_vitals, &diego_triage_vitals))
            .collect(),
            vitals: diego_triage_vitals,
        },
        1,
        None,
        triage_started + chrono::Duration::minutes(2),
    )
    .await?;
    sqlx::query("UPDATE triage_assessments SET ai_artifact_id = $2 WHERE visit_id = $1")
        .bind(in_triage.id)
        .bind(diego_proposal)
        .execute(&mut *tx)
        .await?;

    // 6) Ready for consultation (Jonás): triage completed with an accepted
    //    dMind proposal, assigned to Dr. García, directed alert open.
    let jonas_arrived = now - chrono::Duration::minutes(40);
    let jonas_triaged = now - chrono::Duration::minutes(28);
    let jonas_ready = now - chrono::Duration::minutes(15);
    let ready = VisitSeed {
        id: Uuid::now_v7(),
        facility: s.facility,
        patient: s.jonas,
        status: "ready_for_consultation",
        arrival_kind: ArrivalKind::Scheduled,
        service: "general_medicine",
        reason: "Exertional chest tightness this week (synthetic)",
        scheduled_at: Some(now - chrono::Duration::minutes(30)),
        arrived_at: Some(jonas_arrived),
        triage_started_at: Some(jonas_triaged),
        ready_at: Some(jonas_ready),
        consultation_started_at: None,
        closed_at: None,
        closed_reason: None,
        priority: Some(Priority::Urgent),
        handoff_summary: Some(
            "Known ischaemic heart disease; exertional chest tightness for one week, none at rest today. Vitals stable, SpO₂ 97 %. Red flag chest pain: urgent by rule. Aspirin allergy not recorded (synthetic).",
        ),
        encounter_id: None,
        version: 4,
        created_by: s.registration,
    };
    insert_visit(tx, s.tenant, &ready).await?;
    insert_care_team(
        tx,
        s.tenant,
        &ready,
        None,
        Some(general),
        "destination_queue",
        "registration",
        s.registration,
        false,
    )
    .await?;
    insert_care_team(
        tx,
        s.tenant,
        &ready,
        Some(s.nurse),
        None,
        "triage_nurse",
        "triage",
        s.nurse,
        true,
    )
    .await?;
    insert_care_team(
        tx,
        s.tenant,
        &ready,
        Some(s.physician),
        None,
        "treating_professional",
        "triage",
        s.nurse,
        true,
    )
    .await?;
    let jonas_vitals = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO vital_signs
         (id, tenant_id, visit_id, patient_id, recorded_by, systolic_mmhg, diastolic_mmhg,
          heart_rate_bpm, respiratory_rate_bpm, temperature_c, spo2_percent, recorded_at)
         VALUES ($1,$2,$3,$4,$5,142,88,82,16,36.7,97,$6)",
    )
    .bind(jonas_vitals)
    .bind(s.tenant)
    .bind(ready.id)
    .bind(s.jonas)
    .bind(s.nurse)
    .bind(jonas_triaged)
    .execute(&mut *tx)
    .await?;
    let jonas_triage_vitals = TriageVitals {
        systolic_mmhg: Some(Decimal::from(142)),
        heart_rate_bpm: Some(Decimal::from(82)),
        respiratory_rate_bpm: Some(Decimal::from(16)),
        temperature_c: Some(Decimal::new(367, 1)),
        spo2_percent: Some(Decimal::from(97)),
    };
    let jonas_flags = vec!["chest_pain".to_string()];
    let jonas_concerns = vec!["chest_pain".to_string()];
    let (jonas_floor, jonas_rules) =
        safety_floor(ArrivalKind::Scheduled, &jonas_flags, &jonas_triage_vitals);
    let jonas_proposal = seed_triage_proposal(
        tx,
        s.tenant,
        &ready,
        TriageRequest {
            template: TRIAGE_TEMPLATE.to_string(),
            language: "en".to_string(),
            arrival_kind: ArrivalKind::Scheduled,
            age_years: Some(71),
            reason: Some(ready.reason.to_string()),
            concerns: jonas_concerns.clone(),
            onset: Some("1 week".to_string()),
            red_flags: jonas_flags.clone(),
            allergies: vec![],
            requested_service: Some("general_medicine".to_string()),
            facts: [
                ("visit.arrival_kind".to_string(), "scheduled".to_string()),
                ("triage.version".to_string(), "1".to_string()),
            ]
            .into_iter()
            .chain(single_row_vital_facts(jonas_vitals, &jonas_triage_vitals))
            .collect(),
            vitals: jonas_triage_vitals,
        },
        1,
        Some((
            s.nurse,
            "accepted",
            jonas_ready - chrono::Duration::minutes(3),
        )),
        jonas_triaged + chrono::Duration::minutes(4),
    )
    .await?;
    sqlx::query(
        "INSERT INTO triage_assessments
         (id, tenant_id, visit_id, patient_id, author_id, version, reason, concerns, onset,
          red_flags, note, vital_signs_id, priority, safety_floor, safety_rules, rules_version,
          requested_service, ai_artifact_id, ai_decision, completed_at, created_at, updated_at)
         VALUES ($1,$2,$3,$4,$5,1,$6,$7,'1 week',$8,$9,$10,'urgent',$11,$12,$13,'general_medicine',$14,'accepted',$15,$16,$15)",
    )
    .bind(Uuid::now_v7())
    .bind(s.tenant)
    .bind(ready.id)
    .bind(s.jonas)
    .bind(s.nurse)
    .bind(ready.reason)
    .bind(serde_json::to_value(&jonas_concerns)?)
    .bind(serde_json::to_value(&jonas_flags)?)
    .bind("Pain-free at rest; no diaphoresis; ECG requested by protocol (synthetic).")
    .bind(jonas_vitals)
    .bind(jonas_floor.as_str())
    .bind(serde_json::to_value(&jonas_rules)?)
    .bind(SAFETY_RULES_VERSION)
    .bind(jonas_proposal)
    .bind(jonas_ready)
    .bind(jonas_triaged)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO internal_alerts
         (id, tenant_id, facility_id, patient_id, visit_id, kind, priority, target_user_id, created_by, created_at)
         VALUES ($1,$2,$3,$4,$5,'patient_ready','urgent',$6,$7,$8)",
    )
    .bind(Uuid::now_v7())
    .bind(s.tenant)
    .bind(s.facility)
    .bind(s.jonas)
    .bind(ready.id)
    .bind(s.physician)
    .bind(s.nurse)
    .bind(jonas_ready)
    .execute(&mut *tx)
    .await?;

    // 7) In consultation (Alba): the access episode behind her draft encounter.
    let alba_started = now - chrono::Duration::hours(1);
    let in_consultation = VisitSeed {
        id: Uuid::now_v7(),
        facility: s.facility,
        patient: s.alba,
        status: "in_consultation",
        arrival_kind: ArrivalKind::Scheduled,
        service: "general_medicine",
        reason: "Follow-up of hypertension control (synthetic)",
        scheduled_at: Some(alba_started - chrono::Duration::minutes(30)),
        arrived_at: Some(alba_started - chrono::Duration::minutes(45)),
        triage_started_at: Some(alba_started - chrono::Duration::minutes(30)),
        ready_at: Some(alba_started - chrono::Duration::minutes(15)),
        consultation_started_at: Some(alba_started),
        closed_at: None,
        closed_reason: None,
        priority: Some(Priority::Standard),
        handoff_summary: Some(
            "Hypertension follow-up; BP 148/92 at triage, asymptomatic. Standard priority (synthetic).",
        ),
        encounter_id: Some(s.alba_draft_encounter),
        version: 5,
        created_by: s.registration,
    };
    insert_visit(tx, s.tenant, &in_consultation).await?;
    insert_care_team(
        tx,
        s.tenant,
        &in_consultation,
        Some(s.nurse),
        None,
        "triage_nurse",
        "triage",
        s.nurse,
        true,
    )
    .await?;
    insert_care_team(
        tx,
        s.tenant,
        &in_consultation,
        Some(s.physician),
        None,
        "treating_professional",
        "handoff",
        s.physician,
        true,
    )
    .await?;
    sqlx::query(
        "INSERT INTO triage_assessments
         (id, tenant_id, visit_id, patient_id, author_id, version, reason, concerns, onset,
          red_flags, priority, safety_floor, safety_rules, rules_version, requested_service,
          completed_at, created_at, updated_at)
         VALUES ($1,$2,$3,$4,$5,1,$6,'[\"follow_up\"]'::jsonb,'2 weeks','[]'::jsonb,'standard',
                 'non_urgent','[]'::jsonb,$7,'general_medicine',$8,$9,$8)",
    )
    .bind(Uuid::now_v7())
    .bind(s.tenant)
    .bind(in_consultation.id)
    .bind(s.alba)
    .bind(s.nurse)
    .bind(in_consultation.reason)
    .bind(SAFETY_RULES_VERSION)
    .bind(alba_started - chrono::Duration::minutes(15))
    .bind(alba_started - chrono::Duration::minutes(30))
    .execute(&mut *tx)
    .await?;

    // 8) A cancelled visit from yesterday (Carlos) for the history view.
    let cancelled = VisitSeed {
        id: Uuid::now_v7(),
        facility: s.facility,
        patient: s.carlos,
        status: "cancelled",
        arrival_kind: ArrivalKind::Scheduled,
        service: "general_medicine",
        reason: "Diabetes follow-up (synthetic)",
        scheduled_at: Some(now - chrono::Duration::hours(26)),
        arrived_at: None,
        triage_started_at: None,
        ready_at: None,
        consultation_started_at: None,
        closed_at: Some(now - chrono::Duration::hours(30)),
        closed_reason: Some("Rescheduled at the patient's request (synthetic)"),
        priority: None,
        handoff_summary: None,
        encounter_id: None,
        version: 2,
        created_by: s.registration,
    };
    insert_visit(tx, s.tenant, &cancelled).await?;
    Ok(())
}

/// Persist a deterministic dMind triage proposal for a seeded visit, bound
/// to the triage version it cites, optionally with a recorded human decision.
async fn seed_triage_proposal(
    tx: &mut PgConnection,
    tenant: Uuid,
    v: &VisitSeed<'_>,
    req: TriageRequest,
    triage_version: i64,
    decision: Option<(Uuid, &str, chrono::DateTime<chrono::Utc>)>,
    generated_at: chrono::DateTime<chrono::Utc>,
) -> anyhow::Result<Uuid> {
    let resp = dmind_gateway::fake::FakeProvider::new()
        .propose_triage(&req)
        .await?;
    let (floor, _) = safety_floor(req.arrival_kind, &req.red_flags, &req.vitals);
    let output = resp.output.clamp_to_floor(floor);
    let artifact_id = Uuid::now_v7();
    let status = if decision.is_some() {
        ArtifactStatus::Approved
    } else {
        ArtifactStatus::AwaitingReview
    };
    sqlx::query(
        "INSERT INTO ai_artifacts
         (id, tenant_id, patient_id, visit_id, artifact_type, autonomy_level, status,
          model, model_version, route, template, input_hash, output, output_schema,
          citations, limitations, triage_version, reviewer_id, review_decision, review_note,
          review_detail, reviewed_at, generated_at)
         VALUES ($1,$2,$3,$4,'triage_proposal','A2',$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21)",
    )
    .bind(artifact_id)
    .bind(tenant)
    .bind(v.patient)
    .bind(v.id)
    .bind(status.as_str())
    .bind(&resp.model)
    .bind(&resp.model_version)
    .bind(&resp.route)
    .bind(&req.template)
    .bind(&resp.input_hash)
    .bind(serde_json::to_value(&output)?)
    .bind(TRIAGE_PROPOSAL_SCHEMA)
    .bind(serde_json::to_value(&output.cited_sources)?)
    .bind(serde_json::to_value(&output.limitations)?)
    .bind(triage_version)
    .bind(decision.map(|(reviewer, _, _)| reviewer))
    .bind(decision.map(|_| "approved"))
    .bind(decision.map(|_| "Accepted after bedside review (synthetic demo)."))
    .bind(serde_json::json!({
        "decision": decision.map(|(_, d, _)| d),
        "priority": output.proposed_priority.as_str(),
        "service": output.proposed_service,
    }))
    .bind(decision.map(|(_, _, at)| at))
    .bind(generated_at)
    .execute(&mut *tx)
    .await?;
    if let Some((reviewer, _, at)) = decision {
        sqlx::query(
            "INSERT INTO audit_events
             (id, tenant_id, actor, action, resource_type, resource_id,
              decision, reason, purpose_of_use, recorded_at)
             VALUES ($1,$2,(SELECT username FROM users WHERE id = $3),
                     'ai.artifact.reviewed','ai_artifact',$4,'allow',
                     'synthetic demo seed','treatment',$5)",
        )
        .bind(Uuid::now_v7())
        .bind(tenant)
        .bind(reviewer)
        .bind(artifact_id.to_string())
        .bind(at)
        .execute(&mut *tx)
        .await?;
    }
    Ok(artifact_id)
}

#[allow(clippy::too_many_arguments)]
async fn seed_vitals(
    tx: &mut PgConnection,
    tenant: Uuid,
    encounter: Uuid,
    patient: Uuid,
    recorded_by: Uuid,
    recorded_at: chrono::DateTime<chrono::Utc>,
    (sys, dia, hr, rr, temp, spo2, weight, height): (i64, i64, i64, i64, &str, i64, &str, i64),
) -> anyhow::Result<()> {
    let weight: Decimal = weight.parse()?;
    let height = Decimal::from(height);
    let meters = height / Decimal::from(100);
    let bmi = (weight / (meters * meters)).round_dp(1);
    sqlx::query(
        "INSERT INTO vital_signs
         (id, tenant_id, encounter_id, patient_id, recorded_by, systolic_mmhg, diastolic_mmhg,
          heart_rate_bpm, respiratory_rate_bpm, temperature_c, spo2_percent, weight_kg,
          height_cm, bmi, recorded_at)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15)",
    )
    .bind(Uuid::now_v7())
    .bind(tenant)
    .bind(encounter)
    .bind(patient)
    .bind(recorded_by)
    .bind(Decimal::from(sys))
    .bind(Decimal::from(dia))
    .bind(Decimal::from(hr))
    .bind(Decimal::from(rr))
    .bind(temp.parse::<Decimal>()?)
    .bind(Decimal::from(spo2))
    .bind(weight)
    .bind(height)
    .bind(bmi)
    .bind(recorded_at)
    .execute(&mut *tx)
    .await?;
    Ok(())
}

async fn seed_demo_loop(
    tx: &mut PgConnection,
    tenant: Uuid,
    facility: Uuid,
    practitioner: Uuid,
    spec: &DemoLoopSpec,
) -> anyhow::Result<()> {
    let started = chrono::Utc::now() - chrono::Duration::hours(spec.hours_ago);
    let encounter_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO encounters (id, tenant_id, facility_id, patient_id, practitioner_id, status, encounter_type, started_at)
         VALUES ($1,$2,$3,$4,$5,'in_progress','order_only',$6)",
    )
    .bind(encounter_id)
    .bind(tenant)
    .bind(facility)
    .bind(spec.patient_id)
    .bind(practitioner)
    .bind(started)
    .execute(&mut *tx)
    .await?;

    let sr_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO service_requests (id, tenant_id, encounter_id, patient_id, requester_id,
                                       code_loinc, display, loop_state, version, created_at)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
    )
    .bind(sr_id)
    .bind(tenant)
    .bind(encounter_id)
    .bind(spec.patient_id)
    .bind(practitioner)
    .bind(spec.code_loinc)
    .bind(spec.display)
    .bind(spec.stage.loop_state())
    .bind(spec.stage.version())
    .bind(started)
    .execute(&mut *tx)
    .await?;
    if spec.stage == DemoStage::Ordered {
        return Ok(());
    }

    let effective = started + chrono::Duration::minutes(30);
    let obs_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO observations (id, tenant_id, service_request_id, patient_id, code_loinc,
                                   value_num, unit, reference_range, status, source_system,
                                   idempotency_key, effective_at, received_at)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,'final','synthetic-lab-demo',$9,$10,$10)",
    )
    .bind(obs_id)
    .bind(tenant)
    .bind(sr_id)
    .bind(spec.patient_id)
    .bind(spec.code_loinc)
    .bind(spec.value)
    .bind(spec.unit)
    .bind(spec.reference_range)
    .bind(format!("seed-demo-{sr_id}"))
    .bind(effective)
    .execute(&mut *tx)
    .await?;

    // Deterministic evaluation uses the same versioned rules as ingestion.
    let observed = Quantity {
        value: spec.value,
        unit: spec.unit.to_string(),
    };
    let mut critical = false;
    for rule in baseline_rules() {
        let outcome = rule.evaluate(spec.code_loinc, &observed);
        if matches!(outcome, RuleOutcome::NotApplicable) {
            continue;
        }
        if matches!(outcome, RuleOutcome::Critical { .. }) {
            critical = true;
        }
        sqlx::query(
            "INSERT INTO rule_evaluations (id, tenant_id, observation_id, rule_id, rule_version, outcome, evaluated_at)
             VALUES ($1,$2,$3,$4,$5,$6,$7)",
        )
        .bind(Uuid::now_v7())
        .bind(tenant)
        .bind(obs_id)
        .bind(&rule.rule_id)
        .bind(&rule.version)
        .bind(serde_json::to_value(&outcome)?)
        .bind(effective)
        .execute(&mut *tx)
        .await?;
    }

    let closed = spec.stage == DemoStage::Closed;
    if critical {
        sqlx::query(
            "INSERT INTO alerts (id, tenant_id, patient_id, observation_id, severity, message, status, created_at)
             VALUES ($1,$2,$3,$4,'critical','Critical laboratory result requires review',$5,$6)",
        )
        .bind(Uuid::now_v7())
        .bind(tenant)
        .bind(spec.patient_id)
        .bind(obs_id)
        .bind(if closed { "resolved" } else { "open" })
        .bind(effective)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO follow_up_tasks (id, tenant_id, patient_id, service_request_id, description,
                                          priority, status, due_at, completed_by, created_at)
             VALUES ($1,$2,$3,$4,'Review critical laboratory result and document follow-up','high',$5,$6,$7,$8)",
        )
        .bind(Uuid::now_v7())
        .bind(tenant)
        .bind(spec.patient_id)
        .bind(sr_id)
        .bind(if closed { "completed" } else { "open" })
        .bind(effective + chrono::Duration::hours(1))
        .bind(closed.then_some(practitioner))
        .bind(effective)
        .execute(&mut *tx)
        .await?;
    }

    // AI artifact from the deterministic development provider, matching the
    // ingest pipeline's fact construction.
    let mut facts = vec![(
        format!("observation:{obs_id}"),
        format!(
            "{} {} {} (reference range {})",
            spec.display, spec.value, spec.unit, spec.reference_range
        ),
    )];
    if critical {
        facts.push((
            format!("rule_evaluation:observation:{obs_id}"),
            "Deterministic rule flagged this result as CRITICAL".to_string(),
        ));
    }
    let req = SummaryRequest {
        template: "result-summary@1.0.0".into(),
        facts,
        language: "en".into(),
    };
    let resp = dmind_gateway::fake::FakeProvider::new()
        .summarize_result(&req)
        .await?;
    let reviewed = spec.stage >= DemoStage::Reviewed;
    let artifact_status = if reviewed {
        ArtifactStatus::Approved
    } else {
        ArtifactStatus::AwaitingReview
    };
    let artifact_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO ai_artifacts
         (id, tenant_id, patient_id, service_request_id, observation_id, artifact_type,
          autonomy_level, status, model, model_version, route, template, input_hash, output,
          output_schema, citations, limitations, reviewer_id, review_decision, review_note,
          reviewed_at, generated_at)
         VALUES ($1,$2,$3,$4,$5,'result_summary','A2',$6,$7,$8,$9,$10,$11,$12,'result-summary.v1',$13,$14,$15,$16,$17,$18,$19)",
    )
    .bind(artifact_id)
    .bind(tenant)
    .bind(spec.patient_id)
    .bind(sr_id)
    .bind(obs_id)
    .bind(artifact_status.as_str())
    .bind(&resp.model)
    .bind(&resp.model_version)
    .bind(&resp.route)
    .bind(&req.template)
    .bind(&resp.input_hash)
    .bind(serde_json::to_value(&resp.output)?)
    .bind(serde_json::to_value(&resp.output.cited_sources)?)
    .bind(serde_json::to_value(&resp.output.limitations)?)
    .bind(reviewed.then_some(practitioner))
    .bind(reviewed.then_some("approved"))
    .bind(reviewed.then_some("Reviewed with the deterministic result (synthetic demo)."))
    .bind(reviewed.then(|| effective + chrono::Duration::hours(1)))
    .bind(effective)
    .execute(&mut *tx)
    .await?;

    // A seeded approval is its own provenance event, mirroring the audit the
    // interactive AI review endpoint records; it is never derived silently.
    if reviewed {
        sqlx::query(
            "INSERT INTO audit_events
             (id, tenant_id, actor, action, resource_type, resource_id,
              decision, reason, purpose_of_use, recorded_at)
             VALUES ($1,$2,(SELECT username FROM users WHERE id = $3),
                     'ai.artifact.reviewed','ai_artifact',$4,'allow',
                     'synthetic demo seed','treatment',$5)",
        )
        .bind(Uuid::now_v7())
        .bind(tenant)
        .bind(practitioner)
        .bind(artifact_id.to_string())
        .bind(effective + chrono::Duration::hours(1))
        .execute(&mut *tx)
        .await?;
    }

    // Clinical documentation for each completed workflow step.
    let mut notes: Vec<(&str, &str, i64)> = Vec::new();
    if spec.stage >= DemoStage::Reviewed {
        notes.push((
            "review",
            "Result reviewed against prior values and current medications (synthetic demo).",
            1,
        ));
    }
    if spec.stage >= DemoStage::Notified {
        notes.push((
            "notification",
            "Patient notified by phone; verbal understanding confirmed (synthetic demo).",
            2,
        ));
    }
    if spec.stage >= DemoStage::Closed {
        notes.push((
            "closure",
            "Follow-up plan documented; repeat test ordered where indicated (synthetic demo).",
            3,
        ));
    }
    for (kind, note, offset) in notes {
        sqlx::query(
            "INSERT INTO loop_notes (id, tenant_id, service_request_id, kind, note, created_by, created_at)
             VALUES ($1,$2,$3,$4,$5,$6,$7)",
        )
        .bind(Uuid::now_v7())
        .bind(tenant)
        .bind(sr_id)
        .bind(kind)
        .bind(note)
        .bind(practitioner)
        .bind(effective + chrono::Duration::hours(offset))
        .execute(&mut *tx)
        .await?;
    }
    Ok(())
}
