//! Synthetic development seed data. All names, identifiers, and clinical
//! values are clearly synthetic. No real PHI anywhere.

use dmind_gateway::{ModelGateway, SummaryRequest};
use rand::RngCore;
use rust_decimal::Decimal;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;
use wellos_domain::ai::ArtifactStatus;
use wellos_domain::rules::{baseline_rules, RuleOutcome};
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
    for (username, display, role, is_service) in users {
        let uid = Uuid::now_v7();
        if *username == "svc.lab-adapter" {
            lab_adapter_id = Some(uid);
        }
        if *username == "dr.garcia" {
            dr_garcia_id = Some(uid);
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
    seed_demo_states(&mut tx, tenant_a, facility_a, patient_a, dr_garcia).await?;

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
    Received,
    Reviewed,
    Notified,
    Closed,
}

impl DemoStage {
    fn loop_state(self) -> &'static str {
        match self {
            DemoStage::Received => "received",
            DemoStage::Reviewed => "reviewed",
            DemoStage::Notified => "notified",
            DemoStage::Closed => "closed",
        }
    }

    fn version(self) -> i64 {
        match self {
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
async fn seed_demo_states(
    tx: &mut PgConnection,
    tenant: Uuid,
    facility: Uuid,
    patient_alba: Uuid,
    practitioner: Uuid,
) -> anyhow::Result<()> {
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
            value: Decimal::new(42, 0), // 42 mg/dL (critical low)
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
    ];

    for spec in specs {
        seed_demo_loop(&mut *tx, tenant, facility, practitioner, &spec).await?;
    }
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
        "INSERT INTO encounters (id, tenant_id, facility_id, patient_id, practitioner_id, status, started_at)
         VALUES ($1,$2,$3,$4,$5,'in_progress',$6)",
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
         VALUES ($1,$2,$3,$4,$5,'result_summary','A1',$6,$7,$8,$9,$10,$11,$12,'result-summary.v1',$13,$14,$15,$16,$17,$18,$19)",
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
