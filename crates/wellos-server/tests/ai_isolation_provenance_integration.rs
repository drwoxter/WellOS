//! Integration tests for AI artifact isolation and synthetic provenance:
//! reuse never crosses patients, clinical resources, tenants or provider
//! versions; cross-patient `reused_from` links are refused by the database
//! and by the transactional guard; the latest professional decision still
//! governs reuse; and the `synthetic` flag is derived from the actual
//! execution path (transcription OR model OR reused artifact) and reported
//! identically by the API, the persisted row and the audit provenance.
//!
//! Every provider used here is an offline test double; no external vendor
//! is ever contacted.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine;
use chrono::{DateTime, Duration, Utc};
use dmind_gateway::scribe::{
    FakeTranscription, ScribeError, Transcription, TranscriptionProvider, TranscriptionRequest,
};
use dmind_gateway::{CapabilityStatus, ModelGateway, Operation};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;
use wellos_domain::ai::ProviderInfo;
use wellos_server::aigov::{self, ExecutionPlan, Provenance, ReuseScope};
use wellos_server::runtime::RuntimeConfig;
use wellos_server::state::AppState;

fn database_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://wellos:wellos_dev@localhost:5432/wellos".to_string())
}

async fn pool() -> sqlx::PgPool {
    let pool = wellos_server::connect_pool(&database_url()).await.unwrap();
    wellos_server::run_migrations(&pool).await.unwrap();
    let (users,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM users")
        .fetch_one(&pool)
        .await
        .unwrap();
    if users == 0 {
        wellos_server::seeddata::seed(&pool, &RuntimeConfig::test_fixtures())
            .await
            .unwrap();
    }
    pool
}

async fn fake_state() -> AppState {
    AppState::new(
        pool().await,
        Arc::new(dmind_gateway::fake::FakeProvider::new()),
    )
}

async fn call(
    state: &AppState,
    method: &str,
    path: &str,
    token: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {token}"))
        .body(match body {
            Some(v) => Body::from(v.to_string()),
            None => Body::empty(),
        })
        .unwrap();
    let res = wellos_server::app(state.clone())
        .oneshot(req)
        .await
        .unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

const GARCIA: &str = "dev-dr.garcia";

fn uniq(prefix: &str) -> String {
    format!("{prefix}-{}", Uuid::now_v7().simple())
}

// ---------------------------------------------------------------------------
// Offline provider doubles
// ---------------------------------------------------------------------------

/// Provider identity of the "real" model double. It never touches the
/// network: it delegates to the deterministic fixture but reports itself as a
/// non-synthetic, in-cell provider, exactly as a configured real adapter
/// would for provenance purposes.
fn real_model_info() -> ProviderInfo {
    ProviderInfo {
        provider: "test-real-model".into(),
        model: "test-real-model".into(),
        model_version: "2026.1-test".into(),
    }
}

fn real_transcribe_info() -> ProviderInfo {
    ProviderInfo {
        provider: "test-real-transcribe".into(),
        model: "test-real-transcribe".into(),
        model_version: "2026.1-test".into(),
    }
}

fn as_real(mut status: CapabilityStatus, info: &ProviderInfo) -> CapabilityStatus {
    status.provider = info.provider.clone();
    status.model = Some(info.model.clone());
    status.reason = None;
    status.external = false;
    status.synthetic = false;
    status
}

struct RealLikeGateway {
    inner: dmind_gateway::fake::FakeProvider,
}

#[async_trait::async_trait]
impl ModelGateway for RealLikeGateway {
    fn info(&self) -> ProviderInfo {
        real_model_info()
    }
    fn status(&self) -> CapabilityStatus {
        as_real(self.inner.status(), &real_model_info())
    }
    fn prompt_version(&self, op: Operation) -> String {
        format!("{}+real-test", self.inner.prompt_version(op))
    }
    async fn summarize_result(
        &self,
        req: &dmind_gateway::SummaryRequest,
    ) -> Result<dmind_gateway::GatewayResponse, dmind_gateway::GatewayError> {
        let mut r = self.inner.summarize_result(req).await?;
        let info = real_model_info();
        r.route = info.provider;
        r.model = info.model;
        r.model_version = info.model_version;
        r.prompt_version = self.prompt_version(Operation::ResultSummary);
        Ok(r)
    }
    async fn propose_triage(
        &self,
        req: &dmind_gateway::TriageRequest,
    ) -> Result<dmind_gateway::TriageResponse, dmind_gateway::GatewayError> {
        let mut r = self.inner.propose_triage(req).await?;
        let info = real_model_info();
        r.route = info.provider;
        r.model = info.model;
        r.model_version = info.model_version;
        r.prompt_version = self.prompt_version(Operation::TriageProposal);
        Ok(r)
    }
    async fn summarize_risk(
        &self,
        req: &dmind_gateway::RiskSummaryRequest,
    ) -> Result<dmind_gateway::RiskSummaryResponse, dmind_gateway::GatewayError> {
        let mut r = self.inner.summarize_risk(req).await?;
        let info = real_model_info();
        r.route = info.provider;
        r.model = info.model;
        r.model_version = info.model_version;
        r.prompt_version = self.prompt_version(Operation::RiskSummary);
        Ok(r)
    }
    async fn draft_note(
        &self,
        req: &dmind_gateway::notes::NoteDraftRequest,
    ) -> Result<dmind_gateway::notes::NoteDraftResponse, dmind_gateway::GatewayError> {
        let mut r = self.inner.draft_note(req).await?;
        r.provider = real_model_info();
        r.prompt_version = self.prompt_version(Operation::NoteDraft);
        Ok(r)
    }
}

struct RealLikeTranscription {
    inner: FakeTranscription,
}

#[async_trait::async_trait]
impl TranscriptionProvider for RealLikeTranscription {
    fn info(&self) -> ProviderInfo {
        real_transcribe_info()
    }
    fn status(&self) -> CapabilityStatus {
        as_real(self.inner.status(), &real_transcribe_info())
    }
    async fn transcribe(&self, req: &TranscriptionRequest) -> Result<Transcription, ScribeError> {
        let mut t = self.inner.transcribe(req).await?;
        t.provider = real_transcribe_info();
        Ok(t)
    }
}

fn real_gateway() -> Arc<dyn ModelGateway> {
    Arc::new(RealLikeGateway {
        inner: dmind_gateway::fake::FakeProvider::new(),
    })
}

fn real_transcription() -> Arc<dyn TranscriptionProvider> {
    Arc::new(RealLikeTranscription {
        inner: FakeTranscription::new(),
    })
}

/// Every provider in a test state is in-cell and offline.
fn assert_offline(state: &AppState) {
    assert!(!state.gateway.status().external, "model double is in-cell");
    assert!(
        !state.scribe.status().external,
        "transcription double is in-cell"
    );
}

// ---------------------------------------------------------------------------
// Fixtures (synthetic tenants, patients and clinical resources)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Tenant {
    id: Uuid,
    facility: Uuid,
    practitioner: Uuid,
}

async fn seeded_tenant(pool: &sqlx::PgPool) -> Tenant {
    let (practitioner, id): (Uuid, Uuid) =
        sqlx::query_as("SELECT id, tenant_id FROM users WHERE username = 'dr.garcia'")
            .fetch_one(pool)
            .await
            .unwrap();
    let (facility,): (Uuid,) = sqlx::query_as(
        "SELECT id FROM facilities WHERE tenant_id = $1 ORDER BY created_at LIMIT 1",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .unwrap();
    Tenant {
        id,
        facility,
        practitioner,
    }
}

async fn isolated_tenant(pool: &sqlx::PgPool, practitioner: Uuid) -> Tenant {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO tenants (id, cell, name, data_class)
         VALUES ($1, 'cell-test', $2, 'synthetic')",
    )
    .bind(id)
    .bind(uniq("Isolation fixture"))
    .execute(pool)
    .await
    .unwrap();
    let facility = Uuid::now_v7();
    sqlx::query("INSERT INTO facilities (id, tenant_id, name) VALUES ($1, $2, 'Fixture facility')")
        .bind(facility)
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
    Tenant {
        id,
        facility,
        practitioner,
    }
}

async fn new_patient(pool: &sqlx::PgPool, t: Tenant) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO patients (id, tenant_id, facility_id, family_name, given_name, birth_date, sex, identifier)
         VALUES ($1, $2, $3, 'Isolation', 'Synthetic', '1980-01-01', 'female', $4)",
    )
    .bind(id)
    .bind(t.id)
    .bind(t.facility)
    .bind(uniq("SYN-ISO"))
    .execute(pool)
    .await
    .unwrap();
    id
}

#[derive(Clone, Copy, Debug)]
enum Kind {
    Result,
    Encounter,
    Scribe,
    Triage,
    Risk,
}

const KINDS: [Kind; 5] = [
    Kind::Result,
    Kind::Encounter,
    Kind::Scribe,
    Kind::Triage,
    Kind::Risk,
];

async fn new_encounter(pool: &sqlx::PgPool, t: Tenant, patient: Uuid) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO encounters (id, tenant_id, facility_id, patient_id, practitioner_id)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind(t.id)
    .bind(t.facility)
    .bind(patient)
    .bind(t.practitioner)
    .execute(pool)
    .await
    .unwrap();
    id
}

/// Creates one clinical resource of `kind` for `patient` and returns the
/// typed reuse scope bound to it.
async fn new_scope(pool: &sqlx::PgPool, t: Tenant, patient: Uuid, kind: Kind) -> ReuseScope {
    match kind {
        Kind::Encounter => ReuseScope::EncounterSummary {
            encounter_id: new_encounter(pool, t, patient).await,
        },
        Kind::Scribe => ReuseScope::ScribeDraft {
            encounter_id: new_encounter(pool, t, patient).await,
        },
        Kind::Result => {
            let encounter = new_encounter(pool, t, patient).await;
            let sr = Uuid::now_v7();
            sqlx::query(
                "INSERT INTO service_requests
                 (id, tenant_id, encounter_id, patient_id, requester_id, code_loinc, display)
                 VALUES ($1, $2, $3, $4, $5, '2823-3', 'Potassium')",
            )
            .bind(sr)
            .bind(t.id)
            .bind(encounter)
            .bind(patient)
            .bind(t.practitioner)
            .execute(pool)
            .await
            .unwrap();
            let observation_id = Uuid::now_v7();
            sqlx::query(
                "INSERT INTO observations
                 (id, tenant_id, service_request_id, patient_id, code_loinc, value_num, unit,
                  source_system, idempotency_key, effective_at)
                 VALUES ($1, $2, $3, $4, '2823-3', 4.1, 'mmol/L', 'fixture', $5, now())",
            )
            .bind(observation_id)
            .bind(t.id)
            .bind(sr)
            .bind(patient)
            .bind(uniq("iso-key"))
            .execute(pool)
            .await
            .unwrap();
            ReuseScope::ResultSummary { observation_id }
        }
        Kind::Triage => {
            let visit_id = Uuid::now_v7();
            sqlx::query(
                "INSERT INTO visits
                 (id, tenant_id, facility_id, patient_id, status, arrival_kind, service, created_by)
                 VALUES ($1, $2, $3, $4, 'completed', 'walk_in', 'general_medicine', $5)",
            )
            .bind(visit_id)
            .bind(t.id)
            .bind(t.facility)
            .bind(patient)
            .bind(t.practitioner)
            .execute(pool)
            .await
            .unwrap();
            ReuseScope::TriageProposal { visit_id }
        }
        Kind::Risk => {
            let risk_assessment_id = Uuid::now_v7();
            sqlx::query(
                "INSERT INTO risk_assessments
                 (id, tenant_id, facility_id, patient_id, rules_version, overall_level,
                  safety_floor, overall_trend, domain_levels, assessment, input_hash, trigger,
                  is_current)
                 VALUES ($1, $2, $3, $4, 'risk-rules.v1', 'low', 'low', 'unknown', '{}', '{}',
                         $5, 'fixture', false)",
            )
            .bind(risk_assessment_id)
            .bind(t.id)
            .bind(t.facility)
            .bind(patient)
            .bind(uniq("risk-hash"))
            .execute(pool)
            .await
            .unwrap();
            ReuseScope::RiskSummary { risk_assessment_id }
        }
    }
}

const SCHEMA: &str = "isolation-test.v1";

async fn plan(
    state: &AppState,
    t: Tenant,
    patient: Uuid,
    scope: ReuseScope,
    hash: &str,
) -> ExecutionPlan {
    aigov::plan(state, t.id, patient, scope, hash, SCHEMA)
        .await
        .unwrap()
}

fn reused_id(plan: &ExecutionPlan) -> Option<Uuid> {
    match plan {
        ExecutionPlan::Reuse(prior) => Some(prior.id),
        ExecutionPlan::Execute { .. } => None,
    }
}

struct Persist<'a> {
    provider: ProviderInfo,
    prompt_version: String,
    status: &'a str,
    generated_at: DateTime<Utc>,
    synthetic: bool,
    reused_from: Option<Uuid>,
}

impl Persist<'_> {
    fn current(state: &AppState, scope: ReuseScope, status: &'static str) -> Persist<'static> {
        Persist {
            provider: state.gateway.info(),
            prompt_version: state.gateway.prompt_version(scope.operation()),
            status,
            generated_at: Utc::now(),
            synthetic: state.gateway.status().synthetic,
            reused_from: None,
        }
    }
}

/// Persists an artifact the way every route does: the typed row first, then
/// `aigov::annotate` for provenance inside the same transaction.
async fn persist(
    state: &AppState,
    t: Tenant,
    patient: Uuid,
    scope: ReuseScope,
    hash: &str,
    p: &Persist<'_>,
) -> Result<Uuid, wellos_server::error::ApiError> {
    let id = Uuid::now_v7();
    let mut tx = state.pool.begin().await?;
    let sql = format!(
        "INSERT INTO ai_artifacts
         (id, tenant_id, patient_id, {resource}, artifact_type, autonomy_level, status,
          model, model_version, route, template, input_hash, output, output_schema, generated_at)
         VALUES ($1, $2, $3, $4, $5, 'A2', $6, $7, $8, $9, 'isolation-test@1', $10,
                 '{{\"summary\":\"synthetic fixture output\"}}', $11, $12)",
        resource = scope.resource_column()
    );
    sqlx::query(&sql)
        .bind(id)
        .bind(t.id)
        .bind(patient)
        .bind(scope.resource_id())
        .bind(scope.artifact_type())
        .bind(p.status)
        .bind(&p.provider.model)
        .bind(&p.provider.model_version)
        .bind(&p.provider.provider)
        .bind(hash)
        .bind(SCHEMA)
        .bind(p.generated_at)
        .execute(&mut *tx)
        .await?;
    aigov::annotate(
        &mut tx,
        id,
        &Provenance {
            scope,
            provider: &p.provider,
            prompt_version: &p.prompt_version,
            input_refs: &[],
            usage: None,
            synthetic: p.synthetic,
            reused_from: p.reused_from,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(id)
}

async fn executions(pool: &sqlx::PgPool, tenant: Uuid) -> i64 {
    let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM ai_executions WHERE tenant_id = $1")
        .bind(tenant)
        .fetch_one(pool)
        .await
        .unwrap();
    n
}

/// Model executions that produced `artifact` (0 for a reused output).
async fn executions_for(pool: &sqlx::PgPool, artifact: &str) -> i64 {
    let id: Uuid = artifact.parse().unwrap();
    let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM ai_executions WHERE artifact_id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap();
    n
}

// ---------------------------------------------------------------------------
// Reuse isolation (planner)
// ---------------------------------------------------------------------------

/// Two patients of one tenant with byte-identical model input: the artifact
/// generated for the first is never handed to the second, for every
/// artifact type. The exact same request for the first patient and resource
/// is reused without a new execution.
#[tokio::test]
async fn identical_input_never_reuses_across_patients_of_one_tenant() {
    let state = fake_state().await;
    // Executions are counted per tenant, so this test owns its own tenant.
    let t = isolated_tenant(&state.pool, seeded_tenant(&state.pool).await.practitioner).await;
    for kind in KINDS {
        let (alice, bob) = (
            new_patient(&state.pool, t).await,
            new_patient(&state.pool, t).await,
        );
        let alice_scope = new_scope(&state.pool, t, alice, kind).await;
        let bob_scope = new_scope(&state.pool, t, bob, kind).await;
        let hash = uniq("shared-input");

        let first = plan(&state, t, alice, alice_scope, &hash).await;
        assert!(
            reused_id(&first).is_none(),
            "{kind:?}: nothing to reuse yet"
        );
        let persisted = Persist::current(&state, alice_scope, "awaiting_review");
        let alice_artifact = persist(&state, t, alice, alice_scope, &hash, &persisted)
            .await
            .unwrap();

        let before = executions(&state.pool, t.id).await;
        let again = plan(&state, t, alice, alice_scope, &hash).await;
        assert_eq!(
            reused_id(&again),
            Some(alice_artifact),
            "{kind:?}: same patient, resource and input reuses"
        );
        assert_eq!(
            executions(&state.pool, t.id).await,
            before,
            "{kind:?}: reuse reserves no execution"
        );

        let cross = plan(&state, t, bob, bob_scope, &hash).await;
        assert!(
            reused_id(&cross).is_none(),
            "{kind:?}: another patient's artifact must never be reused"
        );
        assert_eq!(
            executions(&state.pool, t.id).await,
            before + 1,
            "{kind:?}: the other patient's request executes"
        );
    }
}

/// Same patient, same input, different clinical resource (another encounter,
/// visit, observation or risk assessment): never cross-reused.
#[tokio::test]
async fn identical_input_never_reuses_across_resources_of_one_patient() {
    let state = fake_state().await;
    let t = seeded_tenant(&state.pool).await;
    for kind in KINDS {
        let patient = new_patient(&state.pool, t).await;
        let first_scope = new_scope(&state.pool, t, patient, kind).await;
        let second_scope = new_scope(&state.pool, t, patient, kind).await;
        assert_ne!(first_scope, second_scope);
        let hash = uniq("shared-input");

        assert!(reused_id(&plan(&state, t, patient, first_scope, &hash).await).is_none());
        let persisted = Persist::current(&state, first_scope, "approved");
        let artifact = persist(&state, t, patient, first_scope, &hash, &persisted)
            .await
            .unwrap();

        assert_eq!(
            reused_id(&plan(&state, t, patient, first_scope, &hash).await),
            Some(artifact),
            "{kind:?}: the exact resource reuses"
        );
        assert!(
            reused_id(&plan(&state, t, patient, second_scope, &hash).await).is_none(),
            "{kind:?}: a different {} must not reuse",
            first_scope.resource_column()
        );
    }
}

/// A different tenant with the same input never sees the artifact, even if
/// the row is (illegitimately) bound to the same resource identifier.
#[tokio::test]
async fn identical_input_never_reuses_across_tenants() {
    let state = fake_state().await;
    let home = seeded_tenant(&state.pool).await;
    let other = isolated_tenant(&state.pool, home.practitioner).await;
    let (home_patient, other_patient) = (
        new_patient(&state.pool, home).await,
        new_patient(&state.pool, other).await,
    );
    let scope = new_scope(&state.pool, home, home_patient, Kind::Encounter).await;
    let hash = uniq("shared-input");
    let persisted = Persist::current(&state, scope, "approved");
    let artifact = persist(&state, home, home_patient, scope, &hash, &persisted)
        .await
        .unwrap();
    assert_eq!(
        reused_id(&plan(&state, home, home_patient, scope, &hash).await),
        Some(artifact)
    );

    let other_scope = new_scope(&state.pool, other, other_patient, Kind::Encounter).await;
    assert!(reused_id(&plan(&state, other, other_patient, other_scope, &hash).await).is_none());
    // Even a request that names the home tenant's encounter from the other
    // tenant finds nothing: tenant is part of the key.
    assert!(reused_id(&plan(&state, other, other_patient, scope, &hash).await).is_none());
}

/// Provider identity is part of the reuse key: a different model version or
/// prompt version of the same provider never reuses an older artifact.
#[tokio::test]
async fn reuse_requires_identical_provider_model_version_and_prompt() {
    let state = fake_state().await;
    let t = seeded_tenant(&state.pool).await;
    let patient = new_patient(&state.pool, t).await;
    let scope = new_scope(&state.pool, t, patient, Kind::Risk).await;
    let hash = uniq("shared-input");

    let mut older_model = Persist::current(&state, scope, "approved");
    older_model.provider.model_version = format!("{}-previous", older_model.provider.model_version);
    persist(&state, t, patient, scope, &hash, &older_model)
        .await
        .unwrap();
    let mut older_prompt = Persist::current(&state, scope, "approved");
    older_prompt.prompt_version = format!("{}-previous", older_prompt.prompt_version);
    persist(&state, t, patient, scope, &hash, &older_prompt)
        .await
        .unwrap();
    let mut other_provider = Persist::current(&state, scope, "approved");
    other_provider.provider.provider = "some-other-provider".into();
    persist(&state, t, patient, scope, &hash, &other_provider)
        .await
        .unwrap();
    assert!(
        reused_id(&plan(&state, t, patient, scope, &hash).await).is_none(),
        "artifacts of another provider, model version or prompt version are not reused"
    );

    let current = Persist::current(&state, scope, "approved");
    let artifact = persist(&state, t, patient, scope, &hash, &current)
        .await
        .unwrap();
    assert_eq!(
        reused_id(&plan(&state, t, patient, scope, &hash).await),
        Some(artifact)
    );
}

/// The latest professional decision governs: a newer rejection or
/// withdrawal blocks reuse of an older approval, while an approval newer
/// than a rejection is reused.
#[tokio::test]
async fn newer_rejection_or_withdrawal_blocks_reuse_of_older_approval() {
    let state = fake_state().await;
    let t = seeded_tenant(&state.pool).await;
    for blocking in ["rejected", "withdrawn"] {
        let patient = new_patient(&state.pool, t).await;
        let scope = new_scope(&state.pool, t, patient, Kind::Result).await;
        let hash = uniq("shared-input");
        let now = Utc::now();

        let mut approved = Persist::current(&state, scope, "approved");
        approved.generated_at = now - Duration::minutes(10);
        let approved_id = persist(&state, t, patient, scope, &hash, &approved)
            .await
            .unwrap();
        assert_eq!(
            reused_id(&plan(&state, t, patient, scope, &hash).await),
            Some(approved_id)
        );

        let mut decided = Persist::current(&state, scope, blocking);
        decided.generated_at = now - Duration::minutes(5);
        persist(&state, t, patient, scope, &hash, &decided)
            .await
            .unwrap();
        assert!(
            reused_id(&plan(&state, t, patient, scope, &hash).await).is_none(),
            "a newer {blocking} artifact blocks reuse of the older approval"
        );

        let mut reapproved = Persist::current(&state, scope, "approved");
        reapproved.generated_at = now;
        let reapproved_id = persist(&state, t, patient, scope, &hash, &reapproved)
            .await
            .unwrap();
        assert_eq!(
            reused_id(&plan(&state, t, patient, scope, &hash).await),
            Some(reapproved_id),
            "an approval newer than the {blocking} decision is reused again"
        );
    }
}

// ---------------------------------------------------------------------------
// reused_from defence in depth
// ---------------------------------------------------------------------------

async fn reused_from_of(pool: &sqlx::PgPool, id: Uuid) -> Option<Uuid> {
    sqlx::query_scalar("SELECT reused_from FROM ai_artifacts WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// A `reused_from` link to another patient's or tenant's artifact is refused
/// by the database itself, and the transactional guard refuses a link to an
/// artifact of the same patient bound to a different clinical resource.
#[tokio::test]
async fn cross_patient_reused_from_is_rejected_by_database_and_guard() {
    let state = fake_state().await;
    let home = seeded_tenant(&state.pool).await;
    let other = isolated_tenant(&state.pool, home.practitioner).await;
    let alice = new_patient(&state.pool, home).await;
    let bob = new_patient(&state.pool, home).await;
    let carol = new_patient(&state.pool, other).await;
    let alice_scope = new_scope(&state.pool, home, alice, Kind::Encounter).await;
    let alice_other_scope = new_scope(&state.pool, home, alice, Kind::Encounter).await;
    let bob_scope = new_scope(&state.pool, home, bob, Kind::Encounter).await;
    let carol_scope = new_scope(&state.pool, other, carol, Kind::Encounter).await;
    let hash = uniq("shared-input");
    let approved = Persist::current(&state, alice_scope, "approved");
    let alice_artifact = persist(&state, home, alice, alice_scope, &hash, &approved)
        .await
        .unwrap();
    let bob_artifact = persist(&state, home, bob, bob_scope, &hash, &approved)
        .await
        .unwrap();
    let carol_artifact = persist(&state, other, carol, carol_scope, &hash, &approved)
        .await
        .unwrap();

    // Database: a direct cross-patient / cross-tenant link violates the
    // composite foreign key regardless of application code.
    for (target, prior, label) in [
        (bob_artifact, alice_artifact, "cross-patient"),
        (carol_artifact, alice_artifact, "cross-tenant"),
    ] {
        let err = sqlx::query("UPDATE ai_artifacts SET reused_from = $2 WHERE id = $1")
            .bind(target)
            .bind(prior)
            .execute(&state.pool)
            .await
            .expect_err(&format!("{label} reused_from must be refused"));
        let db = err.as_database_error().expect("database error");
        assert!(
            db.is_foreign_key_violation(),
            "{label}: expected a foreign key violation, got {db}"
        );
        assert_eq!(reused_from_of(&state.pool, target).await, None);
    }

    // Transactional guard: a link to another patient is refused before the
    // database is even asked, and a link to the same patient's artifact for
    // a different encounter is refused too (the database allows that shape).
    for (patient, scope, prior, label) in [
        (bob, bob_scope, alice_artifact, "cross-patient"),
        (alice, alice_other_scope, alice_artifact, "cross-resource"),
    ] {
        let mut linked = Persist::current(&state, scope, "awaiting_review");
        linked.reused_from = Some(prior);
        let err = persist(&state, home, patient, scope, &hash, &linked)
            .await
            .expect_err(&format!("{label} reuse link must be refused"));
        assert_eq!(
            err.status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "{label}: {err:?}"
        );
        let (n,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM ai_artifacts WHERE reused_from = $1 AND patient_id = $2",
        )
        .bind(prior)
        .bind(patient)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(n, 0, "{label}: nothing was persisted");
    }

    // The legitimate link (same tenant, patient and encounter) is accepted.
    let mut legit = Persist::current(&state, alice_scope, "awaiting_review");
    legit.reused_from = Some(alice_artifact);
    let reused = persist(&state, home, alice, alice_scope, &hash, &legit)
        .await
        .unwrap();
    assert_eq!(
        reused_from_of(&state.pool, reused).await,
        Some(alice_artifact)
    );
}

/// The data-repair statement of migration 0014, exactly as shipped.
fn upgrade_repair_statement() -> &'static str {
    let sql = include_str!("../migrations/0014_ai_artifact_isolation.sql");
    let start = sql.find("WITH RECURSIVE crossed").expect("repair CTE");
    let end = sql[start..].find(';').expect("statement end");
    &sql[start..start + end]
}

/// Inserts an artifact row the way the pre-hotfix code left it: no
/// transactional guard, an arbitrary `reused_from`.
async fn legacy_artifact(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    t: Tenant,
    patient: Uuid,
    scope: ReuseScope,
    status: &str,
    reused_from: Option<Uuid>,
) -> Uuid {
    let id = Uuid::now_v7();
    let sql = format!(
        "INSERT INTO ai_artifacts
         (id, tenant_id, patient_id, {resource}, artifact_type, autonomy_level, status,
          model, model_version, route, template, input_hash, output, output_schema,
          generated_at, reused_from)
         VALUES ($1, $2, $3, $4, $5, 'A2', $6, 'dmind-fake', 'v1', 'local-fake',
                 'isolation-test@1', 'legacy-hash', '{{\"summary\":\"legacy output\"}}', $7,
                 now(), $8)",
        resource = scope.resource_column()
    );
    sqlx::query(&sql)
        .bind(id)
        .bind(t.id)
        .bind(patient)
        .bind(scope.resource_id())
        .bind(scope.artifact_type())
        .bind(status)
        .bind(SCHEMA)
        .bind(reused_from)
        .execute(&mut **tx)
        .await
        .unwrap();
    id
}

/// Upgrade repair, for every artifact type: a pre-hotfix link between two
/// clinical resources of the same patient is severed and its undecided
/// target invalidated; an artifact that (correctly scoped) reused such a
/// tainted artifact is repaired the same way, keeping a recorded decision;
/// a legitimate same-resource link is left untouched. Runs the shipped
/// statement inside a rolled-back transaction against fixture rows.
#[tokio::test]
async fn upgrade_repair_severs_cross_resource_and_chained_links_for_every_type() {
    let state = fake_state().await;
    let t = seeded_tenant(&state.pool).await;
    let mut tx = state.pool.begin().await.unwrap();
    // The composite foreign key does not constrain same-patient links, so
    // these rows can only exist from before the migration.
    let mut fixtures = Vec::new();
    for kind in KINDS {
        let patient = new_patient(&state.pool, t).await;
        let first = new_scope(&state.pool, t, patient, kind).await;
        let second = new_scope(&state.pool, t, patient, kind).await;
        let root = legacy_artifact(&mut tx, t, patient, first, "awaiting_review", None).await;
        let crossed =
            legacy_artifact(&mut tx, t, patient, second, "awaiting_review", Some(root)).await;
        let chained = legacy_artifact(&mut tx, t, patient, second, "approved", Some(crossed)).await;
        let legit =
            legacy_artifact(&mut tx, t, patient, first, "awaiting_review", Some(root)).await;
        fixtures.push((kind, root, crossed, chained, legit));
    }

    sqlx::query(upgrade_repair_statement())
        .execute(&mut *tx)
        .await
        .unwrap();

    async fn row(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        id: Uuid,
    ) -> (String, Option<Uuid>, Value) {
        sqlx::query_as("SELECT status, reused_from, output FROM ai_artifacts WHERE id = $1")
            .bind(id)
            .fetch_one(&mut **tx)
            .await
            .unwrap()
    }
    let legacy_output = json!({ "summary": "legacy output" });
    for (kind, root, crossed, chained, legit) in fixtures {
        assert_eq!(
            row(&mut tx, root).await,
            ("awaiting_review".into(), None, legacy_output.clone()),
            "{kind:?}: the root artifact is untouched"
        );
        assert_eq!(
            row(&mut tx, crossed).await,
            ("invalidated".into(), None, legacy_output.clone()),
            "{kind:?}: cross-resource link severed, undecided target invalidated"
        );
        assert_eq!(
            row(&mut tx, chained).await,
            ("approved".into(), None, legacy_output.clone()),
            "{kind:?}: link to a tainted artifact severed, decision preserved"
        );
        assert_eq!(
            row(&mut tx, legit).await,
            ("awaiting_review".into(), Some(root), legacy_output.clone()),
            "{kind:?}: legitimate same-resource link kept"
        );
    }
    tx.rollback().await.unwrap();
}

// ---------------------------------------------------------------------------
// Synthetic provenance (scribe: transcription × model, and reuse)
// ---------------------------------------------------------------------------

const SYNTHETIC_AUDIO_MARKER: &[u8] = b"WELLOS-SYNTHETIC-AUDIO-MARKER-";

fn synthetic_audio(len: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(len);
    while v.len() < len {
        v.extend_from_slice(SYNTHETIC_AUDIO_MARKER);
    }
    v.truncate(len);
    v
}

/// Registers a fresh synthetic patient and opens a consultation with
/// recording consent; returns (patient, encounter).
async fn consented_consultation(state: &AppState) -> (String, String) {
    let (st, meta) = call(state, "GET", "/api/v1/meta/tenant", "dev-reg.rivera", None).await;
    assert_eq!(st, StatusCode::OK, "{meta}");
    let facility = meta["facilities"][0]["id"].as_str().unwrap().to_string();
    let (st, patient) = call(
        state,
        "POST",
        "/api/v1/patients",
        "dev-reg.rivera",
        Some(json!({
            "facility_id": facility,
            "family_name": "Provenance",
            "given_name": "Synthetic",
            "birth_date": "1975-05-05",
            "sex": "male",
            "identifier": uniq("MRN-PROV"),
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{patient}");
    let patient = patient["id"].as_str().unwrap().to_string();
    let (st, enc) = call(
        state,
        "POST",
        "/api/v1/encounters",
        GARCIA,
        Some(json!({ "patient_id": patient })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{enc}");
    let enc = enc["id"].as_str().unwrap().to_string();
    let (st, consent) = call(
        state,
        "POST",
        &format!("/api/v1/encounters/{enc}/recording-consent"),
        GARCIA,
        Some(json!({ "granted": true })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{consent}");
    (patient, enc)
}

async fn transcribe(state: &AppState, enc: &str) -> Value {
    let (st, draft) = call(
        state,
        "POST",
        &format!("/api/v1/encounters/{enc}/scribe"),
        GARCIA,
        Some(json!({
            "audio_base64": base64::engine::general_purpose::STANDARD.encode(synthetic_audio(4_096)),
            "mime_type": "audio/webm;codecs=opus",
            "duration_ms": 90_000,
            "language": "en",
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{draft}");
    draft
}

/// The `synthetic` flag as persisted on the row and as recorded in the
/// `ai.artifact.generated` audit provenance for `artifact`.
async fn stored_provenance(pool: &sqlx::PgPool, artifact: &str) -> (bool, bool, Option<Uuid>) {
    let id: Uuid = artifact.parse().unwrap();
    let (row_synthetic, reused_from): (bool, Option<Uuid>) =
        sqlx::query_as("SELECT synthetic, reused_from FROM ai_artifacts WHERE id = $1")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap();
    let (refs,): (Value,) = sqlx::query_as(
        "SELECT resource_refs FROM outbox_events
         WHERE event_type = 'ai.artifact.generated' AND resource_refs->>'artifact_id' = $1",
    )
    .bind(artifact)
    .fetch_one(pool)
    .await
    .unwrap();
    let audit_synthetic = refs["synthetic"]
        .as_bool()
        .expect("audit provenance carries the synthetic flag");
    (row_synthetic, audit_synthetic, reused_from)
}

/// Asserts the API response, the persisted row and the audit provenance all
/// report `expected`, and returns the artifact id.
async fn assert_scribe_provenance(state: &AppState, draft: &Value, expected: bool) -> String {
    let id = draft["id"].as_str().unwrap().to_string();
    assert_eq!(draft["synthetic"], json!(expected), "API response: {draft}");
    let (row, audit, _) = stored_provenance(&state.pool, &id).await;
    assert_eq!(row, expected, "persisted row");
    assert_eq!(audit, expected, "audit provenance");
    // The workspace reload reports the stored value too.
    let enc = draft["output"]["encounter_id"].as_str().unwrap();
    let (st, ws) = call(
        state,
        "GET",
        &format!("/api/v1/encounters/{enc}"),
        GARCIA,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{ws}");
    assert_eq!(ws["scribe_draft"]["id"], json!(id), "{ws}");
    assert_eq!(ws["scribe_draft"]["synthetic"], json!(expected), "{ws}");
    id
}

#[tokio::test]
async fn fake_transcription_with_real_model_is_synthetic() {
    let mut state = fake_state().await;
    state.gateway = real_gateway();
    state.scribe = Arc::new(FakeTranscription::new());
    assert_offline(&state);
    let (_, enc) = consented_consultation(&state).await;
    let draft = transcribe(&state, &enc).await;
    assert_eq!(draft["output"]["transcription"]["provider"], "dmind-fake");
    assert_eq!(draft["output"]["extraction"]["provider"], "test-real-model");
    assert_scribe_provenance(&state, &draft, true).await;
}

#[tokio::test]
async fn real_transcription_with_fake_model_is_synthetic() {
    let mut state = fake_state().await;
    state.scribe = real_transcription();
    assert_offline(&state);
    let (_, enc) = consented_consultation(&state).await;
    let draft = transcribe(&state, &enc).await;
    assert_eq!(
        draft["output"]["transcription"]["provider"],
        "test-real-transcribe"
    );
    assert_eq!(draft["output"]["extraction"]["provider"], "local-fake");
    assert_scribe_provenance(&state, &draft, true).await;
}

#[tokio::test]
async fn real_transcription_with_real_model_is_not_synthetic() {
    let mut state = fake_state().await;
    state.gateway = real_gateway();
    state.scribe = real_transcription();
    assert_offline(&state);
    let (_, enc) = consented_consultation(&state).await;
    let draft = transcribe(&state, &enc).await;
    assert_eq!(
        draft["output"]["transcription"]["provider"],
        "test-real-transcribe"
    );
    assert_eq!(draft["output"]["extraction"]["provider"], "test-real-model");
    assert_scribe_provenance(&state, &draft, false).await;
}

/// A model artifact first produced from a synthetic transcript stays
/// synthetic when a later real transcription of the same words reuses it;
/// the same words for another patient's consultation are never reused and
/// carry their own (real) provenance.
#[tokio::test]
async fn reusing_a_synthetic_artifact_preserves_synthetic_and_never_crosses_patients() {
    let mut state = fake_state().await;
    state.gateway = real_gateway();
    state.scribe = Arc::new(FakeTranscription::new());
    assert_offline(&state);
    let (_, enc) = consented_consultation(&state).await;
    let first = transcribe(&state, &enc).await;
    let first_id = assert_scribe_provenance(&state, &first, true).await;
    let (_, _, reused) = stored_provenance(&state.pool, &first_id).await;
    assert_eq!(reused, None);
    assert_eq!(executions_for(&state.pool, &first_id).await, 1);

    // Same consultation, same words, now transcribed by the real provider:
    // the model output is reused, and the reused output was synthetic.
    state.scribe = real_transcription();
    let second = transcribe(&state, &enc).await;
    assert_eq!(
        second["output"]["transcription"]["provider"],
        "test-real-transcribe"
    );
    let second_id = assert_scribe_provenance(&state, &second, true).await;
    let (_, _, reused) = stored_provenance(&state.pool, &second_id).await;
    assert_eq!(
        reused,
        Some(first_id.parse().unwrap()),
        "model output reused"
    );
    assert_eq!(
        executions_for(&state.pool, &second_id).await,
        0,
        "no model execution"
    );

    // Another patient's consultation with identical words and context: a
    // fresh execution whose provenance is honestly real.
    let (_, other_enc) = consented_consultation(&state).await;
    let third = transcribe(&state, &other_enc).await;
    let third_id = assert_scribe_provenance(&state, &third, false).await;
    let (_, _, reused) = stored_provenance(&state.pool, &third_id).await;
    assert_eq!(reused, None, "another patient's artifact is never reused");
    assert_eq!(executions_for(&state.pool, &third_id).await, 1);
    assert_eq!(
        third["output"]["sections"], first["output"]["sections"],
        "identical input yields identical (separately executed) output"
    );
}

// ---------------------------------------------------------------------------
// Synthetic provenance (model-only route)
// ---------------------------------------------------------------------------

async fn risk_summary(state: &AppState, patient: &str) -> Value {
    let (st, body) = call(
        state,
        "POST",
        &format!("/api/v1/patients/{patient}/risk/recalculate"),
        GARCIA,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let (st, body) = call(
        state,
        "POST",
        &format!("/api/v1/patients/{patient}/risk/summary"),
        GARCIA,
        Some(json!({ "language": "en" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    body
}

/// Model-only artifacts take the actual model provider's flag, and the API,
/// the persisted row, the reload and the audit provenance agree.
#[tokio::test]
async fn model_only_provenance_follows_the_actual_provider() {
    for (gateway, expected) in [
        (
            Arc::new(dmind_gateway::fake::FakeProvider::new()) as Arc<dyn ModelGateway>,
            true,
        ),
        (real_gateway(), false),
    ] {
        let mut state = fake_state().await;
        state.gateway = gateway;
        assert_offline(&state);
        let (patient, _) = consented_consultation(&state).await;
        let body = risk_summary(&state, &patient).await;
        let id = body["id"].as_str().unwrap().to_string();
        assert_eq!(body["summary"]["synthetic"], json!(expected), "{body}");
        let (row, audit, _) = stored_provenance(&state.pool, &id).await;
        assert_eq!(row, expected, "persisted row");
        assert_eq!(audit, expected, "audit provenance");

        let (st, reload) = call(
            &state,
            "GET",
            &format!("/api/v1/patients/{patient}/risk"),
            GARCIA,
            None,
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{reload}");
        assert_eq!(reload["summary"]["id"], json!(id), "{reload}");
        assert_eq!(reload["summary"]["synthetic"], json!(expected), "{reload}");
    }
}
