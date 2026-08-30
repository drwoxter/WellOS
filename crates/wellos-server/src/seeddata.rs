//! Synthetic development seed data. All names, identifiers, and clinical
//! values are clearly synthetic. No real PHI anywhere.

use rand::RngCore;
use sqlx::PgPool;
use uuid::Uuid;

pub struct Seeded {
    pub tenant_a: Uuid,
    pub tenant_b: Uuid,
    pub facility_a: Uuid,
    pub facility_b: Uuid,
    pub patient_a: Uuid,
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

pub async fn seed(pool: &PgPool) -> anyhow::Result<Seeded> {
    let tenant_a = Uuid::now_v7();
    let tenant_b = Uuid::now_v7();
    let facility_a = Uuid::now_v7();
    let facility_b = Uuid::now_v7();

    let mut tx = pool.begin().await?;

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
    for (username, display, role, is_service) in users {
        let uid = Uuid::now_v7();
        if *username == "svc.lab-adapter" {
            lab_adapter_id = Some(uid);
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
        sqlx::query(
            "INSERT INTO role_assignments (id, tenant_id, user_id, role, facility_id) VALUES ($1,$2,$3,$4,$5)",
        )
        .bind(Uuid::now_v7())
        .bind(tenant_a)
        .bind(uid)
        .bind(role)
        .bind(facility_a)
        .execute(&mut *tx)
        .await?;
    }
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

    tx.commit().await?;
    Ok(Seeded {
        tenant_a,
        tenant_b,
        facility_a,
        facility_b,
        patient_a,
        patient_b,
        lab_adapter_token,
    })
}
