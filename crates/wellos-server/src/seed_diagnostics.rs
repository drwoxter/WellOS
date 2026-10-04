//! dMind Clinical Orders & Diagnostics synthetic fixtures (`dev-fixtures`).
//!
//! Every catalog entry, order, specimen, report, review, release, document
//! and imaging reference below is produced by the production write paths —
//! the same handler functions the HTTP routes dispatch to — so the fixtures
//! exercise the real safety engine, order/report/specimen state machines,
//! Access linkage (request → matcher → offer → appointment), ObjectStore
//! gating and dMind governance. Nothing is inserted into a diagnostic table
//! directly and there are no demo-only shortcuts.

use std::collections::BTreeMap;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue};
use axum::Json;
use chrono::{Duration, Utc};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use crate::auth::{AuthContext, RoleAssignment};
use crate::objectstore::FIXTURE_ROUTE_PREFIX;
use crate::policy::{roles, Purpose};
use crate::routes::access::{self, AcceptInput, MatchBody};
use crate::routes::access_admin::{self, CreateCatalogEntry, Deactivate};
use crate::routes::diagnostics::{documents, orders, reports, specimens};
use crate::runtime::RuntimeConfig;
use crate::seed_access::{
    self, body, create_resource, insert_user, service, staff_ctx, svc, weekdays, ResourceSpec, Tx,
};
use crate::state::AppState;

/// Identifiers the diagnostics fixtures are built on top of.
pub(crate) struct DiagnosticsFixtureInput {
    pub tenant: Uuid,
    pub facility: Uuid,
    pub annex: Uuid,
    pub admin: Uuid,
    pub dr_garcia: Uuid,
    pub lab_chen: Uuid,
    pub alba_encounter: Uuid,
    pub anexa: Uuid,
    pub carlos: Uuid,
    pub marta: Uuid,
    pub jonas: Uuid,
    pub sofia: Uuid,
    pub diego: Uuid,
}

/// What the post-commit phase needs.
pub(crate) struct DiagnosticsFixtures {
    tenant: Uuid,
    facility: Uuid,
    annex: Uuid,
    admin: Uuid,
    dr_garcia: Uuid,
    lab_chen: Uuid,
    tech_ruiz: Uuid,
    orderables: BTreeMap<String, Uuid>,
    encounters: Encounters,
}

struct Encounters {
    alba: Uuid,
    anexa: Uuid,
    carlos: Uuid,
    marta: Uuid,
    jonas: Uuid,
    sofia: Uuid,
    diego: Uuid,
}

const TZ: &str = "Europe/Madrid";

// ---------------------------------------------------------------------------
// Identities (same shape the HTTP layer builds after authentication)
// ---------------------------------------------------------------------------

fn clinical_ctx(
    tenant: Uuid,
    user: Uuid,
    username: &str,
    display: &str,
    role: &str,
    facilities: &[Uuid],
) -> AuthContext {
    AuthContext {
        user_id: user,
        tenant_id: tenant,
        username: username.into(),
        display_name: display.into(),
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
        purpose_of_use: Purpose::Treatment,
        break_glass_reason: None,
        web_session_id: None,
        correlation_id: Uuid::now_v7(),
    }
}

// ---------------------------------------------------------------------------
// Catalog: clinical services, resources and diagnostic orderables
// ---------------------------------------------------------------------------

type ServiceSpec = (&'static str, &'static str, &'static str, i32, &'static str);

/// Acquisition services the orderables schedule through dMind Access.
/// (code, name_en, name_es, duration, required resource type)
fn service_specs() -> Vec<ServiceSpec> {
    vec![
        (
            "mammography_exam",
            "Mammography",
            "Mamografía",
            20,
            "imaging_equipment",
        ),
        (
            "ultrasound_exam",
            "Ultrasound examination",
            "Ecografía",
            20,
            "imaging_equipment",
        ),
        (
            "ct_mri_exam",
            "CT / MRI examination",
            "TC / RM",
            30,
            "imaging_equipment",
        ),
        (
            "cardiology_diagnostics",
            "Cardiology diagnostics",
            "Diagnóstico cardiológico",
            30,
            "imaging_equipment",
        ),
        (
            "dental_imaging",
            "Dental imaging",
            "Radiología dental",
            10,
            "imaging_equipment",
        ),
        (
            "endoscopy_procedure",
            "Endoscopy",
            "Endoscopia",
            45,
            "procedure_room",
        ),
    ]
}

/// (code, name_en, name_es, synonyms, external codings, config)
type OrderableSpec = (
    &'static str,
    &'static str,
    &'static str,
    Vec<&'static str>,
    Value,
    Value,
);

fn loinc(code: &str, display: &str) -> Value {
    json!([{ "system": "http://loinc.org", "code": code, "display": display }])
}

fn quantity(code: &str, display: &str, unit: &str, range: &str) -> Value {
    json!({ "code": code, "display": display, "result_type": "quantity", "unit": unit, "reference_range": range })
}

fn narrative(code: &str, display: &str) -> Value {
    json!({ "code": code, "display": display, "result_type": "narrative" })
}

fn lab(components: Vec<Value>, specimen: Value, extra: Value) -> Value {
    let mut v = json!({
        "category_code": "laboratory",
        "modality_code": "laboratory",
        "result_type": "quantity",
        "components": components,
        "specimen": specimen,
        "fulfilment_modes": ["immediate", "inpatient", "bedside", "walk_in"],
        "duplicate_window_days": 7,
    });
    merge(&mut v, extra);
    v
}

fn imaging(modality: &str, service_code: &str, components: Vec<Value>, extra: Value) -> Value {
    let mut v = json!({
        "category_code": "imaging",
        "modality_code": modality,
        "result_type": "narrative",
        "components": components,
        "scheduling_service_code": service_code,
        "required_resource_types": ["imaging_equipment"],
        "fulfilment_modes": ["scheduled", "walk_in", "inpatient"],
        "duplicate_window_days": 30,
        "expects_imaging_study": true,
    });
    merge(&mut v, extra);
    v
}

fn merge(base: &mut Value, extra: Value) {
    if let (Some(b), Some(e)) = (base.as_object_mut(), extra.as_object()) {
        for (k, v) in e {
            b.insert(k.clone(), v.clone());
        }
    }
}

fn question(id: &str, en: &str, es: &str, severity: &str) -> Value {
    json!({ "id": id, "kind": "question", "text_en": en, "text_es": es, "severity": severity,
            "satisfied_by_answer": true })
}

fn fact(id: &str, key: &str, en: &str, es: &str, severity: &str) -> Value {
    json!({ "id": id, "kind": "fact", "fact_key": key, "text_en": en, "text_es": es, "severity": severity })
}

fn orderable_specs() -> Vec<OrderableSpec> {
    let blood = json!({ "type_code": "blood_venous", "container_code": "serum_tube", "minimum_volume_ml": 3 });
    let edta = json!({ "type_code": "blood_venous", "container_code": "edta_tube", "minimum_volume_ml": 2 });
    vec![
        // --- laboratory --------------------------------------------------
        (
            "hemoglobin",
            "Haemoglobin",
            "Hemoglobina",
            vec!["Hb"],
            loinc("718-7", "Hemoglobin [Mass/volume] in Blood"),
            lab(
                vec![quantity("718-7", "Haemoglobin", "g/dL", "12-16")],
                edta.clone(),
                json!({}),
            ),
        ),
        (
            "wbc",
            "White blood cell count",
            "Recuento de leucocitos",
            vec!["WBC", "leukocytes"],
            loinc("6690-2", "Leukocytes [#/volume] in Blood"),
            lab(
                vec![quantity("6690-2", "Leukocytes", "10*3/uL", "4-11")],
                edta.clone(),
                json!({}),
            ),
        ),
        (
            "platelets",
            "Platelet count",
            "Recuento de plaquetas",
            vec!["PLT"],
            loinc("777-3", "Platelets [#/volume] in Blood"),
            lab(
                vec![quantity("777-3", "Platelets", "10*3/uL", "150-400")],
                edta.clone(),
                json!({}),
            ),
        ),
        (
            "cbc_panel",
            "Complete blood count",
            "Hemograma completo",
            vec!["CBC", "FBC", "hemograma"],
            loinc("58410-2", "CBC panel - Blood by Automated count"),
            lab(
                vec![
                    quantity("718-7", "Haemoglobin", "g/dL", "12-16"),
                    quantity("6690-2", "Leukocytes", "10*3/uL", "4-11"),
                    quantity("777-3", "Platelets", "10*3/uL", "150-400"),
                ],
                edta.clone(),
                json!({ "panel_member_codes": ["hemoglobin", "wbc", "platelets"],
                        "redundant_with_codes": ["hemoglobin", "wbc", "platelets"] }),
            ),
        ),
        (
            "lipid_panel",
            "Lipid panel",
            "Perfil lipídico",
            vec!["lipids", "cholesterol"],
            loinc("57698-3", "Lipid panel with direct LDL - Serum or Plasma"),
            lab(
                vec![
                    quantity("2093-3", "Cholesterol, total", "mg/dL", "0-200"),
                    quantity("2085-9", "HDL cholesterol", "mg/dL", "40-100"),
                    quantity("13457-7", "LDL cholesterol", "mg/dL", "0-130"),
                    quantity("2571-8", "Triglycerides", "mg/dL", "0-150"),
                ],
                json!({ "type_code": "blood_venous", "container_code": "serum_tube", "fasting_hours": 12 }),
                json!({ "duplicate_window_days": 30,
                        "preparation_en": "Fast for 12 hours (water allowed).",
                        "preparation_es": "Ayuno de 12 horas (se permite agua).",
                        "safety_rules": [question("fasting", "Has the patient fasted for 12 hours?",
                                                  "¿Ha ayunado el paciente 12 horas?", "warning")] }),
            ),
        ),
        (
            "hba1c",
            "Haemoglobin A1c",
            "Hemoglobina glicosilada (HbA1c)",
            vec!["HbA1c", "glycated haemoglobin"],
            loinc("4548-4", "Hemoglobin A1c/Hemoglobin.total in Blood"),
            lab(
                vec![quantity("4548-4", "HbA1c", "%", "4-5.6")],
                edta.clone(),
                json!({ "duplicate_window_days": 90 }),
            ),
        ),
        (
            "electrolytes",
            "Electrolyte panel",
            "Panel de electrolitos",
            vec!["lytes", "sodium potassium"],
            loinc("24326-1", "Electrolytes 1998 panel - Serum or Plasma"),
            lab(
                vec![
                    quantity("2951-2", "Sodium", "mmol/L", "135-145"),
                    quantity("2823-3", "Potassium", "mmol/L", "3.5-5.1"),
                    quantity("2075-0", "Chloride", "mmol/L", "98-107"),
                ],
                blood.clone(),
                json!({ "duplicate_window_days": 1 }),
            ),
        ),
        (
            "creatinine",
            "Creatinine",
            "Creatinina",
            vec!["renal function"],
            loinc("2160-0", "Creatinine [Mass/volume] in Serum or Plasma"),
            lab(
                vec![quantity("2160-0", "Creatinine", "mg/dL", "0.6-1.2")],
                blood.clone(),
                json!({}),
            ),
        ),
        (
            "vitamin_d_legacy",
            "Vitamin D (legacy method)",
            "Vitamina D (método antiguo)",
            vec![],
            loinc(
                "1989-3",
                "25-Hydroxyvitamin D3 [Mass/volume] in Serum or Plasma",
            ),
            lab(
                vec![quantity("1989-3", "25-OH vitamin D", "ng/mL", "30-100")],
                blood.clone(),
                json!({}),
            ),
        ),
        // --- imaging -----------------------------------------------------
        (
            "chest_xray",
            "Chest radiograph (2 views)",
            "Radiografía de tórax (2 proyecciones)",
            vec!["CXR", "chest x-ray"],
            loinc("36643-5", "XR Chest 2 Views"),
            imaging(
                "radiography",
                "xray_exam",
                vec![narrative("36643-5", "Chest radiograph report")],
                json!({ "fulfilment_modes": ["scheduled", "walk_in", "inpatient", "bedside"],
                        "safety_rules": [fact("pregnancy", "pregnancy_possible",
                            "Patient may be pregnant: confirm indication and shielding.",
                            "La paciente podría estar embarazada: confirme indicación y protección.", "warning")],
                        "critical_conclusion_codes": ["tension_pneumothorax", "pneumothorax_large"],
                        "preparation_en": "Remove metal objects before the exam.",
                        "preparation_es": "Retire los objetos metálicos antes de la prueba." }),
            ),
        ),
        (
            "mammography_screening",
            "Mammography (bilateral)",
            "Mamografía bilateral",
            vec!["mammogram"],
            loinc("24606-6", "MG Breast Screening"),
            imaging(
                "mammography",
                "mammography_exam",
                vec![narrative("24606-6", "Mammography report")],
                json!({ "safety_rules": [
                            question("pregnancy_excluded", "Has pregnancy been excluded?",
                                     "¿Se ha descartado el embarazo?", "warning"),
                            question("implants", "Breast implants declared (adapted technique)?",
                                     "¿Prótesis mamarias declaradas (técnica adaptada)?", "warning")],
                        "critical_conclusion_codes": ["birads-5", "birads-6"] }),
            ),
        ),
        (
            "ultrasound_abdomen",
            "Abdominal ultrasound",
            "Ecografía abdominal",
            vec!["US abdomen"],
            loinc("24558-9", "US Abdomen"),
            imaging(
                "ultrasound",
                "ultrasound_exam",
                vec![narrative("24558-9", "Abdominal ultrasound report")],
                json!({ "preparation_en": "Fast for 6 hours before the examination.",
                        "preparation_es": "Ayuno de 6 horas antes de la exploración.",
                        "safety_rules": [question("fasting", "Fasting for 6 hours confirmed?",
                                                  "¿Ayuno de 6 horas confirmado?", "warning")] }),
            ),
        ),
        (
            "ct_abdomen_contrast",
            "CT abdomen with IV contrast",
            "TC de abdomen con contraste IV",
            vec!["CT abdomen"],
            loinc("79103-8", "CT Abdomen and Pelvis W contrast IV"),
            imaging(
                "ct",
                "ct_mri_exam",
                vec![narrative("79103-8", "CT abdomen report")],
                json!({ "safety_rules": [
                            fact("contrast_allergy", "allergy:contrast",
                                 "Documented contrast-media allergy: premedication or alternative protocol required.",
                                 "Alergia documentada a contraste: requiere premedicación o protocolo alternativo.", "warning"),
                            fact("renal", "condition:n18",
                                 "Chronic kidney disease recorded: check eGFR before contrast.",
                                 "Enfermedad renal crónica registrada: compruebe el FGe antes del contraste.", "warning"),
                            fact("pregnancy", "pregnancy_possible",
                                 "Patient may be pregnant: ionising radiation and contrast.",
                                 "La paciente podría estar embarazada: radiación ionizante y contraste.", "warning")],
                        "preparation_en": "Fast for 4 hours; drink water as instructed.",
                        "preparation_es": "Ayuno de 4 horas; beba agua según indicaciones." }),
            ),
        ),
        (
            "mri_brain",
            "MRI brain",
            "RM cerebral",
            vec!["brain MRI"],
            loinc("24590-2", "MR Brain"),
            imaging(
                "mri",
                "ct_mri_exam",
                vec![narrative("24590-2", "MRI brain report")],
                json!({ "fulfilment_modes": ["scheduled", "inpatient"],
                        "safety_rules": [
                            fact("pacemaker", "condition:z95.0",
                                 "Cardiac pacemaker recorded: MRI is contraindicated unless the device is MR-conditional.",
                                 "Marcapasos registrado: la RM está contraindicada salvo dispositivo RM-condicional.", "hard_stop"),
                            question("metal", "Metal implants and foreign bodies excluded?",
                                     "¿Se han descartado implantes metálicos y cuerpos extraños?", "warning")] }),
            ),
        ),
        (
            "dental_panoramic",
            "Dental panoramic radiograph",
            "Radiografía panorámica dental",
            vec!["orthopantomogram", "OPG"],
            loinc("37050-2", "XR Mandible Panoramic"),
            imaging(
                "dental_radiography",
                "dental_imaging",
                vec![narrative("37050-2", "Panoramic radiograph report")],
                json!({ "fulfilment_modes": ["scheduled", "walk_in"],
                        "safety_rules": [fact("pregnancy", "pregnancy_possible",
                            "Patient may be pregnant: confirm indication.",
                            "La paciente podría estar embarazada: confirme la indicación.", "warning")] }),
            ),
        ),
        // --- cardiology ----------------------------------------------------
        (
            "ecg_12_lead",
            "12-lead ECG",
            "ECG de 12 derivaciones",
            vec!["EKG", "electrocardiogram"],
            loinc("11524-6", "EKG study"),
            json!({
                "category_code": "cardiology",
                "modality_code": "electrocardiography",
                "result_type": "narrative",
                "components": [
                    quantity("8867-4", "Heart rate", "/min", "50-100"),
                    quantity("8625-6", "PR interval", "ms", "120-200"),
                    quantity("8633-0", "QRS duration", "ms", "60-110"),
                    narrative("8601-7", "ECG impression"),
                ],
                "fulfilment_modes": ["immediate", "bedside", "inpatient", "walk_in"],
                "duplicate_window_days": 1,
                "critical_conclusion_codes": ["stemi", "complete_heart_block", "ventricular_tachycardia"],
            }),
        ),
        (
            "holter_24h",
            "24-hour Holter monitoring",
            "Holter de 24 horas",
            vec!["ambulatory ECG"],
            loinc("18752-6", "Exercise stress test study"),
            json!({
                "category_code": "cardiology",
                "modality_code": "ambulatory_ecg",
                "result_type": "narrative",
                "components": [narrative("18752-6", "Holter report")],
                "scheduling_service_code": "cardiology_diagnostics",
                "required_resource_types": ["imaging_equipment"],
                "fulfilment_modes": ["scheduled"],
                "duplicate_window_days": 30,
                "critical_conclusion_codes": ["ventricular_tachycardia", "pause_gt_3s"],
            }),
        ),
        (
            "echocardiogram_tte",
            "Transthoracic echocardiogram",
            "Ecocardiograma transtorácico",
            vec!["echo", "TTE"],
            loinc("34552-0", "US Heart"),
            json!({
                "category_code": "cardiology",
                "modality_code": "echocardiography",
                "result_type": "narrative",
                "components": [
                    quantity("10230-1", "Left ventricular ejection fraction", "%", "55-70"),
                    narrative("34552-0", "Echocardiogram report"),
                ],
                "scheduling_service_code": "cardiology_diagnostics",
                "required_resource_types": ["imaging_equipment"],
                "fulfilment_modes": ["scheduled", "inpatient"],
                "duplicate_window_days": 90,
                "expects_imaging_study": true,
                "critical_conclusion_codes": ["pericardial_tamponade", "lvef_lt_20"],
            }),
        ),
        // --- respiratory -----------------------------------------------------
        (
            "spirometry",
            "Spirometry",
            "Espirometría",
            vec!["lung function"],
            loinc("81459-0", "Spirometry panel"),
            json!({
                "category_code": "pulmonology",
                "modality_code": "spirometry",
                "result_type": "quantity",
                "components": [
                    quantity("20150-9", "FEV1", "L", "2.5-4.5"),
                    quantity("19870-5", "FVC", "L", "3-5.5"),
                    quantity("19926-5", "FEV1/FVC", "%", "70-100"),
                ],
                "fulfilment_modes": ["immediate", "walk_in"],
                "duplicate_window_days": 30,
                "preparation_en": "Do not use short-acting bronchodilators for 4 hours before the test.",
                "preparation_es": "No use broncodilatadores de acción corta en las 4 horas previas.",
                "safety_rules": [question("bronchodilator", "Short-acting bronchodilator withheld for 4 hours?",
                                          "¿Se ha suspendido el broncodilatador de acción corta 4 horas?", "warning")],
            }),
        ),
        // --- pathology -------------------------------------------------------
        (
            "skin_biopsy_histology",
            "Skin biopsy — histopathology",
            "Biopsia cutánea — histopatología",
            vec!["histology", "pathology"],
            loinc("22637-3", "Pathology report final diagnosis Narrative"),
            json!({
                "category_code": "pathology",
                "modality_code": "histopathology",
                "result_type": "narrative",
                "components": [narrative("22637-3", "Histopathology final diagnosis")],
                "specimen": { "type_code": "tissue", "container_code": "formalin_pot" },
                "fulfilment_modes": ["immediate", "inpatient"],
                "duplicate_window_days": 30,
                "critical_conclusion_codes": ["malignant", "melanoma"],
                "safety_rules": [fact("anticoagulant", "medication:apixaban",
                    "Anticoagulant therapy recorded: bleeding risk during biopsy.",
                    "Tratamiento anticoagulante registrado: riesgo de sangrado en la biopsia.", "warning")],
            }),
        ),
        // --- procedures ------------------------------------------------------
        (
            "upper_gi_endoscopy",
            "Upper GI endoscopy",
            "Endoscopia digestiva alta",
            vec!["gastroscopy", "EGD"],
            loinc("18746-8", "Colonoscopy study"),
            json!({
                "category_code": "procedure",
                "modality_code": "endoscopy",
                "result_type": "narrative",
                "components": [narrative("18746-8", "Endoscopy report")],
                "scheduling_service_code": "endoscopy_procedure",
                "required_resource_types": ["procedure_room"],
                "fulfilment_modes": ["scheduled", "inpatient"],
                "duplicate_window_days": 90,
                "preparation_en": "Fast for 8 hours; bring your medication list.",
                "preparation_es": "Ayuno de 8 horas; traiga su lista de medicación.",
                "safety_rules": [
                    question("fasting", "Fasting for 8 hours arranged?", "¿Ayuno de 8 horas organizado?", "warning"),
                    fact("anticoagulant", "medication:apixaban",
                         "Anticoagulant therapy recorded: plan peri-procedural management.",
                         "Tratamiento anticoagulante registrado: planifique el manejo periprocedimiento.", "warning")],
                "critical_conclusion_codes": ["active_bleeding", "perforation"],
            }),
        ),
    ]
}

async fn install_catalog(
    tx: &mut Tx<'_>,
    state: &AppState,
    admin: &AuthContext,
    facilities: &[Uuid],
) -> anyhow::Result<BTreeMap<String, Uuid>> {
    for (code, en, es, duration, rtype) in service_specs() {
        let entry: CreateCatalogEntry = body(json!({
            "kind": "clinical_service",
            "code": code,
            "name_en": en,
            "name_es": es,
            "synonyms": ["diagnostics"],
            "external_codings": [],
            "config": service(duration, &["in_person"], &[rtype], json!({"requires_referral": true})),
            "facility_ids": facilities,
            "change_reason": "synthetic diagnostics fixture catalog",
        }))?;
        access_admin::create_catalog_in(tx, admin, state, entry).await?;
    }
    let mut ids = BTreeMap::new();
    for (code, en, es, synonyms, codings, config) in orderable_specs() {
        let entry: CreateCatalogEntry = body(json!({
            "kind": "diagnostic_orderable",
            "code": code,
            "name_en": en,
            "name_es": es,
            "synonyms": synonyms,
            "external_codings": codings,
            "config": config,
            "facility_ids": facilities,
            "change_reason": "synthetic diagnostics fixture catalog",
        }))?;
        let created = access_admin::create_catalog_in(tx, admin, state, entry).await?;
        let id: Uuid = created["id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("catalog entry without id"))?
            .parse()?;
        ids.insert(code.to_string(), id);
    }
    // Deactivation is the only retirement path: history keeps every version.
    access_admin::deactivate_catalog_in(
        tx,
        admin,
        state,
        ids["vitamin_d_legacy"],
        Deactivate {
            version: 1,
            change_reason: Some("Method replaced by LC-MS/MS assay (synthetic)".into()),
        },
    )
    .await?;
    Ok(ids)
}

async fn install_resources(
    tx: &mut Tx<'_>,
    state: &AppState,
    admin: &AuthContext,
    facility: Uuid,
) -> anyhow::Result<()> {
    let specs: Vec<(&str, &str, &str)> = vec![
        ("imaging_equipment", "Mammography unit", "mammography_exam"),
        ("imaging_equipment", "Ultrasound room 1", "ultrasound_exam"),
        ("imaging_equipment", "CT/MRI suite", "ct_mri_exam"),
        (
            "imaging_equipment",
            "Cardiology diagnostics room",
            "cardiology_diagnostics",
        ),
        (
            "imaging_equipment",
            "Panoramic dental imaging unit",
            "dental_imaging",
        ),
        ("procedure_room", "Endoscopy suite", "endoscopy_procedure"),
    ];
    for (rtype, name, service_code) in specs {
        create_resource(
            tx,
            admin,
            state,
            ResourceSpec {
                facility,
                rtype,
                name,
                user: None,
                profession: None,
                specialties: &[],
                languages: &[],
                accessibility: &["wheelchair_access"],
                capacity: 1,
                time_zone: TZ,
                services: vec![svc(service_code)],
                rules: weekdays(&[1, 2, 3, 4, 5], "08:00:00", "17:00:00", None),
            },
        )
        .await?;
    }
    Ok(())
}

async fn order_only_encounter(
    tx: &mut Tx<'_>,
    tenant: Uuid,
    facility: Uuid,
    patient: Uuid,
    practitioner: Uuid,
) -> anyhow::Result<Uuid> {
    // Orders join the practitioner's open order-only context for the patient
    // when one exists (the legacy laboratory fixtures create it).
    let existing: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM encounters
         WHERE tenant_id = $1 AND facility_id = $2 AND patient_id = $3 AND practitioner_id = $4
           AND status = 'in_progress' AND encounter_type = 'order_only'
         ORDER BY started_at DESC LIMIT 1",
    )
    .bind(tenant)
    .bind(facility)
    .bind(patient)
    .bind(practitioner)
    .fetch_optional(&mut **tx)
    .await?;
    if let Some((id,)) = existing {
        return Ok(id);
    }
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO encounters (id, tenant_id, facility_id, patient_id, practitioner_id, status, encounter_type, started_at)
         VALUES ($1,$2,$3,$4,$5,'in_progress','order_only',$6)",
    )
    .bind(id)
    .bind(tenant)
    .bind(facility)
    .bind(patient)
    .bind(practitioner)
    .bind(Utc::now() - Duration::minutes(20))
    .execute(&mut **tx)
    .await?;
    Ok(id)
}

/// In-transaction phase: catalog, resources, patient facts the safety rules
/// reference, the order-only encounters the clinician orders from, and the
/// adoption of the legacy laboratory fixtures by the generalized model.
pub(crate) async fn seed(
    tx: &mut Tx<'_>,
    state: &AppState,
    input: DiagnosticsFixtureInput,
) -> anyhow::Result<DiagnosticsFixtures> {
    let admin = staff_ctx(
        input.tenant,
        input.admin,
        input.facility,
        Purpose::Operations,
    );
    let orderables = install_catalog(tx, state, &admin, &[input.facility, input.annex]).await?;
    install_resources(tx, state, &admin, input.facility).await?;
    let tech_ruiz = insert_user(
        tx,
        input.tenant,
        "tech.ruiz",
        "Marina Ruiz (Diagnostic professional)",
        roles::DIAGNOSTIC_PROFESSIONAL,
        Some(input.facility),
    )
    .await?;

    // Patient facts the deterministic rules read (synthetic).
    sqlx::query(
        "INSERT INTO allergies (id, tenant_id, patient_id, substance, criticality)
         VALUES ($1,$2,$3,'Iodinated contrast media (synthetic)','high')",
    )
    .bind(Uuid::now_v7())
    .bind(input.tenant)
    .bind(input.carlos)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "INSERT INTO conditions (id, tenant_id, patient_id, code, display)
         VALUES ($1,$2,$3,'Z95.0','Presence of cardiac pacemaker (synthetic)')",
    )
    .bind(Uuid::now_v7())
    .bind(input.tenant)
    .bind(input.jonas)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "INSERT INTO medications (id, tenant_id, patient_id, name)
         VALUES ($1,$2,$3,'Apixaban 5 mg twice daily (synthetic)')",
    )
    .bind(Uuid::now_v7())
    .bind(input.tenant)
    .bind(input.diego)
    .execute(&mut **tx)
    .await?;

    let encounters = Encounters {
        alba: input.alba_encounter,
        anexa: order_only_encounter(tx, input.tenant, input.annex, input.anexa, input.dr_garcia)
            .await?,
        carlos: order_only_encounter(
            tx,
            input.tenant,
            input.facility,
            input.carlos,
            input.dr_garcia,
        )
        .await?,
        marta: order_only_encounter(
            tx,
            input.tenant,
            input.facility,
            input.marta,
            input.dr_garcia,
        )
        .await?,
        jonas: order_only_encounter(
            tx,
            input.tenant,
            input.facility,
            input.jonas,
            input.dr_garcia,
        )
        .await?,
        sofia: order_only_encounter(
            tx,
            input.tenant,
            input.facility,
            input.sofia,
            input.dr_garcia,
        )
        .await?,
        diego: order_only_encounter(
            tx,
            input.tenant,
            input.facility,
            input.diego,
            input.dr_garcia,
        )
        .await?,
    };

    // The legacy laboratory fixtures seeded above become generalized orders
    // exactly as migration 0016 adopts a real tenant's history.
    sqlx::query("SELECT wellos_adopt_legacy_lab_orders()")
        .execute(&mut **tx)
        .await?;

    Ok(DiagnosticsFixtures {
        tenant: input.tenant,
        facility: input.facility,
        annex: input.annex,
        admin: input.admin,
        dr_garcia: input.dr_garcia,
        lab_chen: input.lab_chen,
        tech_ruiz,
        orderables,
        encounters,
    })
}

// ---------------------------------------------------------------------------
// Post-commit phase: the clinical workflows through the handler functions
// ---------------------------------------------------------------------------

struct Placement<'a> {
    encounter: Uuid,
    items: Vec<Value>,
    answers: Value,
    indication: &'a str,
    question: Option<&'a str>,
    facility: Option<Uuid>,
    override_reason: Option<&'a str>,
    /// Reviewed dMind order suggestion the clinician is confirming from.
    suggestion: Option<Uuid>,
    schedule: bool,
    key: &'a str,
    /// Minimum warnings the preflight must raise; the fixture is wrong if the
    /// deterministic engine stops producing them.
    expect_warnings: usize,
    expect_hard_stops: usize,
}

fn item(orderable: Uuid, mode: &str, priority: &str) -> Value {
    json!({ "orderable_id": orderable, "fulfilment_mode": mode, "priority": priority })
}

/// Preflight → acknowledge every finding → confirm, exactly like the composer.
async fn place(
    state: &AppState,
    ctx: &AuthContext,
    p: Placement<'_>,
) -> anyhow::Result<Vec<Value>> {
    let preflight: orders::PreflightBody = body(json!({
        "items": p.items,
        "answers": p.answers,
        "performing_facility_id": p.facility,
        "lang": "en",
    }))?;
    let Json(pre) = orders::preflight(
        State(state.clone()),
        ctx.clone(),
        Path(p.encounter),
        Json(preflight),
    )
    .await?;
    let eval = &pre["evaluation"];
    let warnings = eval["warnings"].as_u64().unwrap_or(0) as usize;
    let hard_stops = eval["hard_stops"].as_u64().unwrap_or(0) as usize;
    anyhow::ensure!(
        warnings >= p.expect_warnings && hard_stops == p.expect_hard_stops,
        "fixture `{}`: expected >= {} warnings and {} hard stops, engine produced {} / {}",
        p.key,
        p.expect_warnings,
        p.expect_hard_stops,
        warnings,
        hard_stops
    );
    let acknowledged: Vec<Value> = eval["findings"]
        .as_array()
        .map(|f| f.iter().map(|x| x["id"].clone()).collect())
        .unwrap_or_default();
    let confirm: orders::ConfirmBody = body(json!({
        "items": p.items,
        "answers": p.answers,
        "performing_facility_id": p.facility,
        "lang": "en",
        "safety_evaluation_id": eval["id"],
        "acknowledged_ids": acknowledged,
        "override_reason": p.override_reason,
        "suggestion_artifact_id": p.suggestion,
        "clinical_indication": p.indication,
        "clinical_question": p.question,
        "idempotency_key": format!("fixture:{}", p.key),
        "schedule": p.schedule,
    }))?;
    let Json(out) = orders::confirm(
        State(state.clone()),
        ctx.clone(),
        Path(p.encounter),
        Json(confirm),
    )
    .await?;
    Ok(out["group"]["orders"]
        .as_array()
        .cloned()
        .unwrap_or_default())
}

fn id_of(v: &Value) -> anyhow::Result<Uuid> {
    Ok(v["id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("missing id in {v}"))?
        .parse()?)
}

fn version_of(v: &Value) -> i64 {
    v["version"].as_i64().unwrap_or(1)
}

async fn transition(
    state: &AppState,
    ctx: &AuthContext,
    order: &Value,
    name: &str,
    reason: Option<&str>,
    mode: Option<&str>,
) -> anyhow::Result<Value> {
    let b: orders::TransitionBody = body(json!({
        "transition": name,
        "version": version_of(order),
        "reason": reason,
        "fulfilment_mode": mode,
    }))?;
    let Json(v) = orders::transition(
        State(state.clone()),
        ctx.clone(),
        Path(id_of(order)?),
        Json(b),
    )
    .await?;
    Ok(v)
}

async fn specimen(
    state: &AppState,
    ctx: &AuthContext,
    order: &Value,
    type_code: &str,
    container: &str,
    body_site: Option<&str>,
    facility: Uuid,
) -> anyhow::Result<Value> {
    let b: specimens::RecordBody = body(json!({
        "specimen_type_code": type_code,
        "container_code": container,
        "body_site": body_site,
        "collected": true,
        "collected_at": Utc::now() - Duration::minutes(15),
        "collection_facility_id": facility,
    }))?;
    let Json(v) = specimens::record(
        State(state.clone()),
        ctx.clone(),
        Path(id_of(order)?),
        Json(b),
    )
    .await?;
    Ok(v)
}

async fn custody(
    state: &AppState,
    ctx: &AuthContext,
    spec: &Value,
    events: &[(&str, Option<&str>)],
    facility: Uuid,
) -> anyhow::Result<Value> {
    let mut current = spec.clone();
    for (event, reason) in events {
        let b: specimens::EventBody = body(json!({
            "event": event,
            "version": version_of(&current),
            "facility_id": facility,
            "reason": reason,
        }))?;
        let Json(v) = specimens::event(
            State(state.clone()),
            ctx.clone(),
            Path(id_of(&current)?),
            Json(b),
        )
        .await?;
        current = v;
    }
    Ok(current)
}

async fn issue(
    state: &AppState,
    ctx: &AuthContext,
    order: &Value,
    b: Value,
) -> anyhow::Result<Value> {
    let b: reports::IssueBody = body(b)?;
    let Json(v) = reports::issue(
        State(state.clone()),
        ctx.clone(),
        Path(id_of(order)?),
        Json(b),
    )
    .await?;
    Ok(v)
}

fn qty(code: &str, value: &str, unit: &str, range: &str) -> Value {
    json!({ "code": code, "value": { "type": "quantity", "value": value, "unit": unit }, "reference_range": range })
}

fn text(code: &str, s: &str) -> Value {
    json!({ "code": code, "value": { "type": "narrative", "text": s } })
}

async fn review(
    state: &AppState,
    ctx: &AuthContext,
    report: &Value,
    assessment: &str,
    disposition: &str,
    synthesis: Option<(Uuid, &str)>,
) -> anyhow::Result<Value> {
    let b: reports::ReviewBody = body(json!({
        "report_version": version_of(report),
        "clinical_assessment": assessment,
        "disposition": disposition,
        "follow_ups": [],
        "synthesis_artifact_id": synthesis.map(|s| s.0),
        "synthesis_decision": synthesis.map(|s| s.1),
    }))?;
    let Json(v) = reports::review(
        State(state.clone()),
        ctx.clone(),
        Path(id_of(report)?),
        Json(b),
    )
    .await?;
    Ok(v)
}

/// Book the Access request a confirmed schedulable order opened: matcher →
/// first offer → staff acceptance (the appointment hook schedules the order).
async fn book(state: &AppState, sched: &AuthContext, order: &Value) -> anyhow::Result<()> {
    let request_id: Uuid = order["access_request_id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("order {} has no Access request", order["id"]))?
        .parse()?;
    let mut conn = state.pool.acquire().await?;
    let r = access::load_request(&mut conn, sched.tenant_id, request_id).await?;
    drop(conn);
    let m = access::run_matcher_for(
        state,
        sched,
        r,
        MatchBody {
            version: None,
            origin: None,
            ranking: Some(false),
            language: Some("en".into()),
        },
        "staff",
    )
    .await?;
    let offer = m
        .offers
        .first()
        .ok_or_else(|| anyhow::anyhow!("matcher produced no offer for order {}", order["id"]))?;
    let mut tx = state.pool.begin().await?;
    access::accept_offer_in(
        &mut tx,
        sched,
        state,
        AcceptInput {
            offer_id: offer.id,
            version: None,
            reason: None,
            override_reason: None,
            idempotency_key: None,
            reschedule_of: None,
            reschedule_reason: None,
            booked_via: "staff",
        },
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Register a document, upload it through the pre-signed fixture grant and
/// complete it (checksum/size verification → `clean`).
#[allow(clippy::too_many_arguments)]
async fn document(
    state: &AppState,
    ctx: &AuthContext,
    order: &Value,
    report_id: Option<Uuid>,
    kind: &str,
    title: &str,
    mime: &str,
    payload: &[u8],
) -> anyhow::Result<Option<Uuid>> {
    if state.object_store.fixture().is_none() {
        tracing::warn!(
            title,
            "WELLOS_OBJECT_STORE is not 'fixture': skipping the synthetic document upload"
        );
        return Ok(None);
    }
    let checksum = hex::encode(Sha256::digest(payload));
    let b: documents::RegisterBody = body(json!({
        "kind": kind,
        "title": title,
        "mime_type": mime,
        "size_bytes": payload.len(),
        "checksum_sha256": checksum,
        "diagnostic_report_id": report_id,
        "source_system": "synthetic-fixture",
    }))?;
    let Json(reg) = documents::register(
        State(state.clone()),
        ctx.clone(),
        Path(id_of(order)?),
        Json(b),
    )
    .await?;
    let url = reg["upload"]["url"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("upload grant without url"))?;
    let rest = url
        .strip_prefix(FIXTURE_ROUTE_PREFIX)
        .ok_or_else(|| anyhow::anyhow!("fixture upload url expected, got {url}"))?;
    let (key, query) = rest.split_once('?').unwrap_or((rest, ""));
    let query: BTreeMap<String, String> = query
        .split('&')
        .filter(|kv| !kv.is_empty())
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (k.to_string(), percent_decode(v)))
        .collect();
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_str(mime)?);
    documents::fixture_put(
        State(state.clone()),
        Path(key.trim_start_matches('/').to_string()),
        Query(query),
        headers,
        Bytes::copy_from_slice(payload),
    )
    .await?;
    let doc_id = id_of(&reg["document"])?;
    let Json(_) = documents::complete(State(state.clone()), ctx.clone(), Path(doc_id)).await?;
    Ok(Some(doc_id))
}

async fn imaging_study(
    state: &AppState,
    ctx: &AuthContext,
    order: &Value,
    modality: &str,
    description: &str,
    key: &str,
) -> anyhow::Result<Value> {
    let suffix: u128 = Uuid::now_v7().as_u128() % 1_000_000_000_000;
    let b: documents::ImagingBody = body(json!({
        "study_instance_uid": format!("1.2.826.0.1.3680043.10.9999.{suffix}"),
        "accession_number": format!("SYN-ACC-{}", &key.to_uppercase()),
        "modality_code": modality,
        "description": description,
        "series": [{ "series_instance_uid": format!("1.2.826.0.1.3680043.10.9999.{suffix}.1"),
                     "modality": modality, "number_of_instances": 2, "description": "Series 1 (synthetic)" }],
        "pacs_endpoint_code": "pacs_main",
        "status": "available",
        "started_at": Utc::now() - Duration::minutes(30),
        "source_system": "synthetic-fixture",
        "idempotency_key": format!("fixture-imaging:{key}"),
    }))?;
    let Json(v) = documents::register_imaging_study(
        State(state.clone()),
        ctx.clone(),
        Path(id_of(order)?),
        Json(b),
    )
    .await?;
    Ok(v)
}

/// Mark every dMind artifact produced here as a deterministic synthetic
/// fixture so no view can mistake it for a real model execution.
async fn mark_synthetic(pool: &PgPool, tenant: Uuid) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE ai_artifacts SET synthetic = true, provider = COALESCE(provider, route)
         WHERE tenant_id = $1 AND synthetic = false",
    )
    .bind(tenant)
    .execute(pool)
    .await?;
    Ok(())
}

pub(crate) async fn after_commit(
    pool: &PgPool,
    runtime: &RuntimeConfig,
    fx: &DiagnosticsFixtures,
) -> anyhow::Result<()> {
    let state = seed_access::fixture_state(pool.clone(), runtime);
    let dr = clinical_ctx(
        fx.tenant,
        fx.dr_garcia,
        "dr.garcia",
        "Dr. Ana García",
        roles::PHYSICIAN,
        &[fx.facility, fx.annex],
    );
    let lab = clinical_ctx(
        fx.tenant,
        fx.lab_chen,
        "lab.chen",
        "Wei Chen",
        roles::LAB,
        &[fx.facility],
    );
    let tech = clinical_ctx(
        fx.tenant,
        fx.tech_ruiz,
        "tech.ruiz",
        "Marina Ruiz",
        roles::DIAGNOSTIC_PROFESSIONAL,
        &[fx.facility],
    );
    let sched = staff_ctx(fx.tenant, fx.admin, fx.facility, Purpose::Treatment);
    let o = |code: &str| fx.orderables[code];
    let f = fx.facility;
    let e = &fx.encounters;

    // --- Alba: dMind order suggestion (server-supplied candidates only),
    // then fasting lipid panel + HbA1c, full laboratory loop to release
    let suggest: orders::SuggestBody = body(json!({ "lang": "en", "facility_id": f }))?;
    let Json(suggestion) = orders::suggest(
        State(state.clone()),
        dr.clone(),
        Path(e.alba),
        Some(Json(suggest)),
    )
    .await?;
    let suggestion_id = suggestion["artifact_id"]
        .as_str()
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(|| anyhow::anyhow!("order suggestion without artifact_id"))?;
    let alba = place(
        &state,
        &dr,
        Placement {
            encounter: e.alba,
            items: vec![
                item(o("lipid_panel"), "immediate", "routine"),
                item(o("hba1c"), "immediate", "routine"),
            ],
            answers: json!({ "lipid_panel:fasting": true }),
            indication: "Hypertension follow-up; cardiovascular risk reassessment (synthetic)",
            question: Some("Lipid profile and glycaemic control at annual review"),
            facility: Some(f),
            override_reason: None,
            suggestion: Some(suggestion_id),
            schedule: false,
            key: "alba-lipids",
            expect_warnings: 0,
            expect_hard_stops: 0,
        },
    )
    .await?;
    let lipids = transition(&state, &lab, &alba[0], "accept", None, None).await?;
    let spec = specimen(&state, &lab, &lipids, "blood_venous", "serum_tube", None, f).await?;
    custody(
        &state,
        &lab,
        &spec,
        &[
            ("dispatched", None),
            ("received", None),
            ("processing_started", None),
            ("processed", None),
        ],
        f,
    )
    .await?;
    let lipid_report = issue(
        &state,
        &lab,
        &lipids,
        json!({
            "status": "final",
            "components": [
                qty("2093-3", "212", "mg/dL", "0-200"),
                qty("2085-9", "48", "mg/dL", "40-100"),
                qty("13457-7", "141", "mg/dL", "0-130"),
                qty("2571-8", "118", "mg/dL", "0-150"),
            ],
            "conclusion": "Mild hypercholesterolaemia (synthetic result).",
            "conclusion_codes": [],
            "idempotency_key": "fixture-report:alba-lipids",
            "source_system": "synthetic-lab",
            "sign": true,
        }),
    )
    .await?;
    let Json(synth) = reports::synthesize(
        State(state.clone()),
        dr.clone(),
        Path(id_of(&lipid_report)?),
        Some(Json(reports::SynthesizeBody {
            lang: Some("es".into()),
        })),
    )
    .await?;
    let reviewed = review(
        &state,
        &dr,
        &lipid_report,
        "LDL above target; reinforce lifestyle advice and reassess statin eligibility at next visit.",
        "routine_follow_up",
        Some((id_of(&synth)?, "approved")),
    )
    .await?;
    let review_id: Uuid = reviewed["review_id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("review without id"))?
        .parse()?;
    let Json(explanation) = reports::explain(
        State(state.clone()),
        dr.clone(),
        Path(id_of(&lipid_report)?),
    )
    .await?;
    let explanation_id = id_of(&explanation)?;
    let Json(_) = reports::review_explanation(
        State(state.clone()),
        dr.clone(),
        Path((id_of(&lipid_report)?, explanation_id)),
        Json(reports::ExplanationReviewBody {
            decision: "approved".into(),
            note: Some("Plain-language wording checked by the clinician (synthetic).".into()),
        }),
    )
    .await?;
    let release: reports::ReleaseBody = body(json!({
        "report_version": version_of(&lipid_report),
        "review_id": review_id,
        "decision": "release",
        "explanation_artifact_id": explanation_id,
        "explanation_en": explanation["output"]["explanation_en"],
        "explanation_es": explanation["output"]["explanation_es"],
        "notify_patient": true,
        "release_documents": true,
    }))?;
    let Json(_) = reports::release(
        State(state.clone()),
        dr.clone(),
        Path(id_of(&lipid_report)?),
        Json(release),
    )
    .await?;
    // HbA1c stays accepted and collected: pending result in the worklists.
    let hba1c = transition(&state, &lab, &alba[1], "accept", None, None).await?;
    specimen(&state, &lab, &hba1c, "blood_venous", "edta_tube", None, f).await?;
    // A repeat HbA1c inside the 90-day window raises the duplicate warning,
    // which the clinician acknowledges explicitly.
    place(
        &state,
        &dr,
        Placement {
            encounter: e.alba,
            items: vec![item(o("hba1c"), "immediate", "routine")],
            answers: json!({}),
            indication: "Repeat requested by patient after home-meter readings (synthetic)",
            question: None,
            facility: Some(f),
            override_reason: None,
            suggestion: None,
            schedule: false,
            key: "alba-hba1c-duplicate",
            expect_warnings: 1,
            expect_hard_stops: 0,
        },
    )
    .await?;

    // --- Carlos: scheduled imaging with a contrast-allergy warning, plus a
    // corrected critical potassium.
    let carlos = place(
        &state,
        &dr,
        Placement {
            encounter: e.carlos,
            items: vec![
                item(o("chest_xray"), "scheduled", "routine"),
                item(o("ct_abdomen_contrast"), "scheduled", "urgent"),
            ],
            answers: json!({}),
            indication: "Persistent cough and weight loss; abdominal pain (synthetic)",
            question: Some("Exclude pulmonary and intra-abdominal pathology"),
            facility: Some(f),
            override_reason: None,
            suggestion: None,
            schedule: true,
            key: "carlos-imaging",
            expect_warnings: 1,
            expect_hard_stops: 0,
        },
    )
    .await?;
    for order in &carlos {
        book(&state, &sched, order).await?;
    }
    let lytes = place(
        &state,
        &dr,
        Placement {
            encounter: e.carlos,
            items: vec![item(o("electrolytes"), "immediate", "urgent")],
            answers: json!({}),
            indication: "Diuretic therapy review (synthetic)",
            question: None,
            facility: Some(f),
            override_reason: None,
            suggestion: None,
            schedule: false,
            key: "carlos-electrolytes",
            expect_warnings: 0,
            expect_hard_stops: 0,
        },
    )
    .await?;
    let lytes = transition(&state, &lab, &lytes[0], "accept", None, None).await?;
    let spec = specimen(&state, &lab, &lytes, "blood_venous", "serum_tube", None, f).await?;
    custody(
        &state,
        &lab,
        &spec,
        &[("received", None), ("processing_started", None)],
        f,
    )
    .await?;
    issue(
        &state,
        &lab,
        &lytes,
        json!({
            "status": "final",
            "components": [
                qty("2951-2", "138", "mmol/L", "135-145"),
                qty("2823-3", "4.1", "mmol/L", "3.5-5.1"),
                qty("2075-0", "101", "mmol/L", "98-107"),
            ],
            "idempotency_key": "fixture-report:carlos-lytes-final",
            "source_system": "synthetic-lab",
            "sign": true,
        }),
    )
    .await?;
    // Analyser re-run after a sample-handling review: the potassium was
    // wrong. The correction replaces the report and is critical by rule.
    issue(
        &state,
        &lab,
        &lytes,
        json!({
            "status": "corrected",
            "components": [
                qty("2951-2", "138", "mmol/L", "135-145"),
                qty("2823-3", "6.8", "mmol/L", "3.5-5.1"),
                qty("2075-0", "101", "mmol/L", "98-107"),
            ],
            "change_reason": "Potassium re-measured after haemolysis check; original value transcribed incorrectly (synthetic).",
            "idempotency_key": "fixture-report:carlos-lytes-corrected",
            "source_system": "synthetic-lab",
            "sign": true,
        }),
    )
    .await?;

    // --- Marta: mammography booked, acquired, reported with a PDF and an
    // imaging reference, reviewed and released to the patient.
    let marta = place(
        &state,
        &dr,
        Placement {
            encounter: e.marta,
            items: vec![item(o("mammography_screening"), "scheduled", "routine")],
            answers: json!({ "mammography_screening:pregnancy_excluded": true,
                             "mammography_screening:implants": true }),
            indication: "Screening interval reached; family history (synthetic)",
            question: None,
            facility: Some(f),
            override_reason: None,
            suggestion: None,
            schedule: true,
            key: "marta-mammography",
            expect_warnings: 0,
            expect_hard_stops: 0,
        },
    )
    .await?;
    book(&state, &sched, &marta[0]).await?;
    let mut conn = state.pool.acquire().await?;
    let mammo =
        crate::routes::diagnostics::load_order(&mut conn, fx.tenant, id_of(&marta[0])?).await?;
    drop(conn);
    let mammo = crate::routes::diagnostics::order_json(&mammo);
    let mammo = transition(&state, &tech, &mammo, "start", None, None).await?;
    imaging_study(
        &state,
        &tech,
        &mammo,
        "MG",
        "Bilateral screening mammography (synthetic)",
        "marta-mg",
    )
    .await?;
    let mammo_report = issue(
        &state,
        &tech,
        &mammo,
        json!({
            "status": "final",
            "components": [text("24606-6",
                "Bilateral mammography. Scattered fibroglandular densities. No suspicious mass, calcification or architectural distortion. BI-RADS 2 (synthetic).")],
            "conclusion": "Benign findings; routine screening interval (synthetic).",
            "conclusion_codes": [{ "system": "urn:wellos:birads", "code": "birads-2", "display": "BI-RADS 2 — benign" }],
            "idempotency_key": "fixture-report:marta-mammography",
            "source_system": "synthetic-radiology",
            "sign": true,
        }),
    )
    .await?;
    document(
        &state,
        &tech,
        &mammo,
        Some(id_of(&mammo_report)?),
        "report_pdf",
        "Mammography report (synthetic PDF)",
        "application/pdf",
        b"%PDF-1.4\n% synthetic fixture: mammography report, BI-RADS 2\n%%EOF\n",
    )
    .await?;
    let reviewed = review(
        &state,
        &dr,
        &mammo_report,
        "Benign screening result; continue routine interval screening.",
        "no_action",
        None,
    )
    .await?;
    let review_id: Uuid = reviewed["review_id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("review without id"))?
        .parse()?;
    let release: reports::ReleaseBody = body(json!({
        "report_version": version_of(&mammo_report),
        "review_id": review_id,
        "decision": "release",
        "explanation_en": "Your mammogram shows no signs of cancer. Please continue with routine screening.",
        "explanation_es": "Su mamografía no muestra signos de cáncer. Continúe con el cribado habitual.",
        "notify_patient": true,
        "release_documents": true,
    }))?;
    let Json(_) = reports::release(
        State(state.clone()),
        dr.clone(),
        Path(id_of(&mammo_report)?),
        Json(release),
    )
    .await?;

    // --- Jonas: STAT ECG with a critical conclusion awaiting review, MRI
    // blocked by the pacemaker hard stop and overridden with a reason,
    // Holter + echo scheduled (Holter put on hold), spirometry withheld.
    let ecg = place(
        &state,
        &dr,
        Placement {
            encounter: e.jonas,
            items: vec![item(o("ecg_12_lead"), "immediate", "stat")],
            answers: json!({}),
            indication: "Syncope in the waiting room (synthetic)",
            question: Some("Conduction abnormality?"),
            facility: Some(f),
            override_reason: None,
            suggestion: None,
            schedule: false,
            key: "jonas-ecg",
            expect_warnings: 0,
            expect_hard_stops: 0,
        },
    )
    .await?;
    let ecg = transition(&state, &tech, &ecg[0], "accept", None, None).await?;
    let ecg = transition(&state, &tech, &ecg, "start", None, Some("immediate")).await?;
    let ecg_report = issue(
        &state,
        &tech,
        &ecg,
        json!({
            "status": "final",
            "components": [
                qty("8867-4", "38", "/min", "50-100"),
                qty("8625-6", "0", "ms", "120-200"),
                qty("8633-0", "132", "ms", "60-110"),
                text("8601-7", "Complete atrioventricular dissociation with ventricular escape rhythm at 38/min (synthetic)."),
            ],
            "conclusion": "Complete heart block (synthetic).",
            "conclusion_codes": [{ "system": "urn:wellos:ecg", "code": "complete_heart_block", "display": "Complete heart block" }],
            "idempotency_key": "fixture-report:jonas-ecg",
            "source_system": "synthetic-cardiology",
            "sign": true,
        }),
    )
    .await?;
    document(
        &state,
        &tech,
        &ecg,
        Some(id_of(&ecg_report)?),
        "tracing",
        "12-lead ECG tracing (synthetic PDF)",
        "application/pdf",
        b"%PDF-1.4\n% synthetic fixture: ECG tracing\n%%EOF\n",
    )
    .await?;
    let mri = place(
        &state,
        &dr,
        Placement {
            encounter: e.jonas,
            items: vec![item(o("mri_brain"), "scheduled", "urgent")],
            answers: json!({ "mri_brain:metal": true }),
            indication: "Syncope with focal neurology (synthetic)",
            question: Some("Structural cause?"),
            facility: Some(f),
            override_reason: Some(
                "Device confirmed MR-conditional by cardiology (model and programming checked); scan under cardiology supervision (synthetic).",
            ),
            suggestion: None,
            schedule: true,
            key: "jonas-mri",
            expect_warnings: 0,
            expect_hard_stops: 1,
        },
    )
    .await?;
    book(&state, &sched, &mri[0]).await?;
    let cardio = place(
        &state,
        &dr,
        Placement {
            encounter: e.jonas,
            items: vec![
                item(o("holter_24h"), "scheduled", "routine"),
                item(o("echocardiogram_tte"), "scheduled", "routine"),
            ],
            answers: json!({}),
            indication: "Bradyarrhythmia work-up (synthetic)",
            question: None,
            facility: Some(f),
            override_reason: None,
            suggestion: None,
            schedule: true,
            key: "jonas-cardio",
            expect_warnings: 0,
            expect_hard_stops: 0,
        },
    )
    .await?;
    transition(
        &state,
        &dr,
        &cardio[0],
        "hold",
        Some("Await pacing decision before ambulatory monitoring (synthetic)"),
        None,
    )
    .await?;
    book(&state, &sched, &cardio[1]).await?;
    let spiro = place(
        &state,
        &dr,
        Placement {
            encounter: e.jonas,
            items: vec![item(o("spirometry"), "immediate", "routine")],
            answers: json!({ "spirometry:bronchodilator": true }),
            indication: "Exertional dyspnoea (synthetic)",
            question: None,
            facility: Some(f),
            override_reason: None,
            suggestion: None,
            schedule: false,
            key: "jonas-spirometry",
            expect_warnings: 0,
            expect_hard_stops: 0,
        },
    )
    .await?;
    let spiro = transition(&state, &tech, &spiro[0], "accept", None, None).await?;
    let spiro = transition(&state, &tech, &spiro, "start", None, Some("immediate")).await?;
    let spiro_report = issue(
        &state,
        &tech,
        &spiro,
        json!({
            "status": "final",
            "components": [
                qty("20150-9", "2.1", "L", "2.5-4.5"),
                qty("19870-5", "3.4", "L", "3-5.5"),
                qty("19926-5", "62", "%", "70-100"),
            ],
            "conclusion": "Moderate obstructive pattern (synthetic).",
            "conclusion_codes": [],
            "idempotency_key": "fixture-report:jonas-spirometry",
            "source_system": "synthetic-pulmonology",
            "sign": true,
        }),
    )
    .await?;
    let reviewed = review(
        &state,
        &dr,
        &spiro_report,
        "Obstructive pattern; discuss in person together with the cardiac work-up.",
        "urgent_follow_up",
        None,
    )
    .await?;
    let review_id: Uuid = reviewed["review_id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("review without id"))?
        .parse()?;
    let withhold: reports::ReleaseBody = body(json!({
        "report_version": version_of(&spiro_report),
        "review_id": review_id,
        "decision": "withhold",
        "withhold_reason": "Result to be explained in person at the scheduled consultation (synthetic).",
        "notify_patient": false,
    }))?;
    let Json(_) = reports::release(
        State(state.clone()),
        dr.clone(),
        Path(id_of(&spiro_report)?),
        Json(withhold),
    )
    .await?;

    // --- Sofia: abdominal ultrasound scheduled; skin biopsy with custody
    // chain, preliminary → final → amended pathology report (reopens review).
    let sofia = place(
        &state,
        &dr,
        Placement {
            encounter: e.sofia,
            items: vec![item(o("ultrasound_abdomen"), "scheduled", "routine")],
            answers: json!({ "ultrasound_abdomen:fasting": true }),
            indication: "Right upper quadrant pain (synthetic)",
            question: Some("Gallstones?"),
            facility: Some(f),
            override_reason: None,
            suggestion: None,
            schedule: true,
            key: "sofia-ultrasound",
            expect_warnings: 0,
            expect_hard_stops: 0,
        },
    )
    .await?;
    book(&state, &sched, &sofia[0]).await?;
    let biopsy = place(
        &state,
        &dr,
        Placement {
            encounter: e.sofia,
            items: vec![item(o("skin_biopsy_histology"), "immediate", "routine")],
            answers: json!({}),
            indication: "Changing pigmented lesion, left forearm (synthetic)",
            question: Some("Exclude melanoma"),
            facility: Some(f),
            override_reason: None,
            suggestion: None,
            schedule: false,
            key: "sofia-biopsy",
            expect_warnings: 0,
            expect_hard_stops: 0,
        },
    )
    .await?;
    let biopsy = transition(&state, &lab, &biopsy[0], "accept", None, None).await?;
    let spec = specimen(
        &state,
        &lab,
        &biopsy,
        "tissue",
        "formalin_pot",
        Some("Left forearm"),
        f,
    )
    .await?;
    custody(
        &state,
        &lab,
        &spec,
        &[
            ("dispatched", None),
            ("received", None),
            ("processing_started", None),
            ("processed", None),
        ],
        f,
    )
    .await?;
    issue(
        &state,
        &lab,
        &biopsy,
        json!({
            "status": "preliminary",
            "components": [text("22637-3", "Compound melanocytic naevus; margins under evaluation (synthetic).")],
            "conclusion_codes": [{ "system": "urn:wellos:pathology", "code": "benign", "display": "Benign" }],
            "idempotency_key": "fixture-report:sofia-biopsy-prelim",
            "source_system": "synthetic-pathology",
            "sign": false,
        }),
    )
    .await?;
    let final_biopsy = issue(
        &state,
        &lab,
        &biopsy,
        json!({
            "status": "final",
            "components": [text("22637-3", "Compound melanocytic naevus, completely excised. No atypia (synthetic).")],
            "conclusion": "Benign naevus, completely excised (synthetic).",
            "conclusion_codes": [{ "system": "urn:wellos:pathology", "code": "benign", "display": "Benign" }],
            "idempotency_key": "fixture-report:sofia-biopsy-final",
            "source_system": "synthetic-pathology",
            "sign": true,
        }),
    )
    .await?;
    review(
        &state,
        &dr,
        &final_biopsy,
        "Benign and completely excised; reassure at follow-up.",
        "routine_follow_up",
        None,
    )
    .await?;
    issue(
        &state,
        &lab,
        &biopsy,
        json!({
            "status": "amended",
            "components": [text("22637-3",
                "Compound melanocytic naevus with mild architectural atypia (dysplastic naevus, low grade), completely excised. Addendum after immunohistochemistry (synthetic).")],
            "conclusion": "Low-grade dysplastic naevus, completely excised (synthetic).",
            "conclusion_codes": [{ "system": "urn:wellos:pathology", "code": "dysplastic_low_grade", "display": "Dysplastic naevus, low grade" }],
            "change_reason": "Immunohistochemistry (HMB-45, Ki-67) completed after the final report (synthetic).",
            "idempotency_key": "fixture-report:sofia-biopsy-amended",
            "source_system": "synthetic-pathology",
            "sign": true,
        }),
    )
    .await?;
    // A placed order the clinician withdraws before fulfilment.
    let creat = place(
        &state,
        &dr,
        Placement {
            encounter: e.sofia,
            items: vec![item(o("creatinine"), "immediate", "routine")],
            answers: json!({}),
            indication: "Baseline before contrast (synthetic)",
            question: None,
            facility: Some(f),
            override_reason: None,
            suggestion: None,
            schedule: false,
            key: "sofia-creatinine",
            expect_warnings: 0,
            expect_hard_stops: 0,
        },
    )
    .await?;
    transition(
        &state,
        &dr,
        &creat[0],
        "cancel",
        Some("Result from last week located in external records (synthetic)"),
        None,
    )
    .await?;

    // --- Diego: panoramic dental radiograph and an urgent endoscopy with an
    // anticoagulant warning, both scheduled through Access.
    let diego = place(
        &state,
        &dr,
        Placement {
            encounter: e.diego,
            items: vec![
                item(o("dental_panoramic"), "scheduled", "routine"),
                item(o("upper_gi_endoscopy"), "scheduled", "urgent"),
            ],
            answers: json!({ "upper_gi_endoscopy:fasting": true }),
            indication: "Dysphagia and dental assessment before procedure (synthetic)",
            question: Some("Oesophageal lesion?"),
            facility: Some(f),
            override_reason: None,
            suggestion: None,
            schedule: true,
            key: "diego-procedures",
            expect_warnings: 1,
            expect_hard_stops: 0,
        },
    )
    .await?;
    for order in &diego {
        book(&state, &sched, order).await?;
    }

    // --- Anexa (annex facility): a placed CBC panel only the annex staff can
    // act on — the laboratory professional of the main site is out of scope.
    place(
        &state,
        &dr,
        Placement {
            encounter: e.anexa,
            items: vec![item(o("cbc_panel"), "immediate", "routine")],
            answers: json!({}),
            indication: "Fatigue (synthetic)",
            question: None,
            facility: Some(fx.annex),
            override_reason: None,
            suggestion: None,
            schedule: false,
            key: "anexa-cbc",
            expect_warnings: 0,
            expect_hard_stops: 0,
        },
    )
    .await?;

    mark_synthetic(pool, fx.tenant).await
}
