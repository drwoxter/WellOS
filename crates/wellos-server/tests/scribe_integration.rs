//! Integration tests for the consultation scribe: consent, transcription
//! validation and rate limiting, deterministic structured output bound to the
//! note version, per-section application that never overwrites clinician
//! text, closed-encounter and stale-version rejection, authorization
//! boundaries, the dashboard cockpit, the patient brief / diagnostic history
//! payload, and create-or-resume consultations. Also proves no raw audio is
//! ever persisted.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine;
use dmind_gateway::scribe::FakeTranscription;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;
use wellos_server::state::{AppState, AuthConfig};

fn database_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://wellos:wellos_dev@localhost:5432/wellos".to_string())
}

async fn state_with(auth: AuthConfig) -> (AppState, Arc<FakeTranscription>) {
    let pool = wellos_server::connect_pool(&database_url()).await.unwrap();
    wellos_server::run_migrations(&pool).await.unwrap();
    let seeded: Option<(i64,)> = sqlx::query_as("SELECT COUNT(*) FROM users")
        .fetch_optional(&pool)
        .await
        .unwrap();
    if seeded.map(|(n,)| n).unwrap_or(0) == 0 {
        wellos_server::seeddata::seed(&pool).await.unwrap();
    }
    let gateway = Arc::new(dmind_gateway::fake::FakeProvider::new());
    let scribe = Arc::new(FakeTranscription::new());
    let mut state = AppState::with_auth(pool, gateway, auth);
    state.scribe = scribe.clone();
    (state, scribe)
}

async fn test_state() -> (AppState, Arc<FakeTranscription>) {
    state_with(AuthConfig::development()).await
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
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
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

fn uniq(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::now_v7().simple())
}

/// A recognisable synthetic "recording": never real speech, but bytes that
/// would be trivially findable if anything persisted them.
const SYNTHETIC_AUDIO_MARKER: &[u8] = b"WELLOS-SYNTHETIC-AUDIO-MARKER-";

fn synthetic_audio(len: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(len);
    while v.len() < len {
        v.extend_from_slice(SYNTHETIC_AUDIO_MARKER);
    }
    v.truncate(len);
    v
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn transcribe_body(audio: &[u8], duration_ms: u64) -> Value {
    json!({
        "audio_base64": b64(audio),
        "mime_type": "audio/webm;codecs=opus",
        "duration_ms": duration_ms,
        "language": "en",
    })
}

/// Registers a fresh synthetic patient; returns (facility, id, identifier).
async fn register_patient(state: &AppState) -> (String, String, String) {
    let (st, meta) = call(state, "GET", "/api/v1/meta/tenant", "dev-reg.rivera", None).await;
    assert_eq!(st, StatusCode::OK);
    let facility = meta["facilities"][0]["id"].as_str().unwrap().to_string();
    let identifier = uniq("MRN-SCR");
    let (st, patient) = call(
        state,
        "POST",
        "/api/v1/patients",
        "dev-reg.rivera",
        Some(json!({
            "facility_id": facility,
            "family_name": "Scribe",
            "given_name": "Test",
            "birth_date": "1975-05-05",
            "sex": "male",
            "identifier": identifier,
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{patient}");
    (
        facility,
        patient["id"].as_str().unwrap().to_string(),
        identifier,
    )
}

/// Fresh patient + in-progress consultation owned by dr.garcia.
async fn start_consultation(state: &AppState) -> (String, String) {
    let (_, patient_id, _) = register_patient(state).await;
    let (st, enc) = call(
        state,
        "POST",
        "/api/v1/encounters",
        "dev-dr.garcia",
        Some(json!({ "patient_id": patient_id })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{enc}");
    (patient_id, enc["id"].as_str().unwrap().to_string())
}

async fn grant_consent(state: &AppState, enc: &str) {
    let (st, body) = call(
        state,
        "POST",
        &format!("/api/v1/encounters/{enc}/recording-consent"),
        "dev-dr.garcia",
        Some(json!({ "granted": true })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["granted"], json!(true));
}

async fn transcribe(state: &AppState, enc: &str, token: &str) -> (StatusCode, Value) {
    call(
        state,
        "POST",
        &format!("/api/v1/encounters/{enc}/scribe"),
        token,
        Some(transcribe_body(&synthetic_audio(4_096), 90_000)),
    )
    .await
}

// ---------------------------------------------------------------------------
// Consent
// ---------------------------------------------------------------------------

#[tokio::test]
async fn transcription_requires_recorded_consent_and_consent_is_audited() {
    let (state, _) = test_state().await;
    let (_, enc) = start_consultation(&state).await;

    let (st, err) = transcribe(&state, &enc, "dev-dr.garcia").await;
    assert_eq!(st, StatusCode::CONFLICT, "{err}");
    assert_eq!(err["error"]["code"], json!("consent_required"));

    // Declined consent is recorded but does not unlock transcription.
    let (st, body) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/recording-consent"),
        "dev-dr.garcia",
        Some(json!({ "granted": false })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let (st, err) = transcribe(&state, &enc, "dev-dr.garcia").await;
    assert_eq!(st, StatusCode::CONFLICT, "{err}");
    assert_eq!(err["error"]["code"], json!("consent_required"));

    grant_consent(&state, &enc).await;
    let (st, draft) = transcribe(&state, &enc, "dev-dr.garcia").await;
    assert_eq!(st, StatusCode::OK, "{draft}");

    // Consent decisions are append-only and mirrored to the outbox.
    let consents: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM encounter_recording_consents WHERE encounter_id = $1::uuid",
    )
    .bind(&enc)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(consents, 2);
    let events: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM outbox_events
         WHERE event_type = 'encounter.recording.consent_recorded'
           AND resource_refs->>'encounter_id' = $1",
    )
    .bind(&enc)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(events, 2);

    // The workspace reports the latest decision so the UI can skip re-asking.
    let (st, ws) = call(
        &state,
        "GET",
        &format!("/api/v1/encounters/{enc}"),
        "dev-dr.garcia",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(ws["recording_consent"]["granted"], json!(true));
    assert_eq!(ws["capabilities"]["can_record"], json!(true));
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn transcription_validates_mime_size_duration_and_language() {
    let (state, _) = test_state().await;
    let (_, enc) = start_consultation(&state).await;
    grant_consent(&state, &enc).await;
    let path = format!("/api/v1/encounters/{enc}/scribe");
    let audio = synthetic_audio(2_048);

    let cases: Vec<(Value, StatusCode, &str)> = vec![
        (
            json!({ "audio_base64": b64(&audio), "mime_type": "video/mp4", "duration_ms": 5_000 }),
            StatusCode::BAD_REQUEST,
            "unsupported_media_type",
        ),
        (
            json!({ "audio_base64": b64(&audio), "mime_type": "text/plain", "duration_ms": 5_000 }),
            StatusCode::BAD_REQUEST,
            "unsupported_media_type",
        ),
        (
            json!({ "audio_base64": b64(&audio), "mime_type": "audio/webm", "duration_ms": 200 }),
            StatusCode::BAD_REQUEST,
            "recording_too_short",
        ),
        (
            json!({ "audio_base64": b64(&audio), "mime_type": "audio/webm", "duration_ms": 21 * 60 * 1000 }),
            StatusCode::BAD_REQUEST,
            "recording_too_long",
        ),
        (
            json!({ "audio_base64": b64(&audio), "mime_type": "audio/webm", "duration_ms": 5_000, "language": "fr" }),
            StatusCode::BAD_REQUEST,
            "validation_failed",
        ),
        (
            json!({ "audio_base64": "not*base64!", "mime_type": "audio/webm", "duration_ms": 5_000 }),
            StatusCode::BAD_REQUEST,
            "validation_failed",
        ),
        (
            json!({ "audio_base64": "", "mime_type": "audio/webm", "duration_ms": 5_000 }),
            StatusCode::BAD_REQUEST,
            "validation_failed",
        ),
    ];
    for (body, expected, code) in cases {
        let (st, err) = call(&state, "POST", &path, "dev-dr.garcia", Some(body.clone())).await;
        assert_eq!(st, expected, "{body} -> {err}");
        assert_eq!(err["error"]["code"], json!(code), "{body} -> {err}");
    }

    // Oversize recordings are refused before decoding.
    let oversize = synthetic_audio(wellos_server::routes::scribe::MAX_AUDIO_BYTES + 1);
    let (st, err) = call(
        &state,
        "POST",
        &path,
        "dev-dr.garcia",
        Some(json!({ "audio_base64": b64(&oversize), "mime_type": "audio/webm", "duration_ms": 60_000 })),
    )
    .await;
    assert_eq!(st, StatusCode::PAYLOAD_TOO_LARGE, "{err}");
    assert_eq!(err["error"]["code"], json!("recording_too_large"));

    // Nothing above created an artifact.
    let artifacts: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM ai_artifacts WHERE encounter_id = $1::uuid AND artifact_type = 'scribe_draft'",
    )
    .bind(&enc)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(artifacts, 0);
}

#[tokio::test]
async fn transcription_has_its_own_rate_limit_family() {
    // dr.lopez is used here because dr.garcia's scribe window is shared
    // with the other tests in this file running concurrently.
    let mut cfg = AuthConfig::development();
    cfg.rate.scribe_per_min = 3;
    let (state, _) = state_with(cfg).await;
    // Windows are fixed-minute and persisted, so a previous run within the
    // same minute would otherwise leave this principal already exhausted.
    sqlx::query("DELETE FROM rate_limit_windows WHERE key LIKE '%:scribe'")
        .execute(&state.pool)
        .await
        .unwrap();
    let (_, patient_id, _) = register_patient(&state).await;
    let (st, enc) = call(
        &state,
        "POST",
        "/api/v1/encounters",
        "dev-dr.lopez",
        Some(json!({ "patient_id": patient_id })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{enc}");
    let enc = enc["id"].as_str().unwrap().to_string();
    let (st, body) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/recording-consent"),
        "dev-dr.lopez",
        Some(json!({ "granted": true })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");

    let mut statuses = Vec::new();
    for _ in 0..5 {
        let (st, _) = transcribe(&state, &enc, "dev-dr.lopez").await;
        statuses.push(st);
    }
    assert!(statuses.contains(&StatusCode::OK), "{statuses:?}");
    assert!(
        statuses.contains(&StatusCode::TOO_MANY_REQUESTS),
        "{statuses:?}"
    );
    let first_429 = statuses
        .iter()
        .position(|s| *s == StatusCode::TOO_MANY_REQUESTS)
        .unwrap();
    assert!(
        statuses[first_429..]
            .iter()
            .all(|s| *s == StatusCode::TOO_MANY_REQUESTS),
        "{statuses:?}"
    );

    // The general API family is unaffected: the workspace still loads.
    let (st, _) = call(
        &state,
        "GET",
        &format!("/api/v1/encounters/{enc}"),
        "dev-dr.lopez",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
}

// ---------------------------------------------------------------------------
// Deterministic structured output, provenance, no raw audio
// ---------------------------------------------------------------------------

#[tokio::test]
async fn transcription_yields_valid_structured_draft_bound_to_note_version() {
    let (state, _) = test_state().await;
    let (_, enc) = start_consultation(&state).await;
    grant_consent(&state, &enc).await;

    // A note at version 1 exists when the recording is processed.
    let (st, note) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/note"),
        "dev-dr.garcia",
        Some(json!({ "reason_for_encounter": "Follow-up (typed by clinician)" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{note}");
    assert_eq!(note["version"], json!(1));

    let (st, draft) = transcribe(&state, &enc, "dev-dr.garcia").await;
    assert_eq!(st, StatusCode::OK, "{draft}");
    assert_eq!(draft["status"], json!("awaiting_review"));
    assert_eq!(draft["note_version"], json!(1));
    assert_eq!(draft["route"], json!("dmind-fake"));
    let output = &draft["output"];
    assert_eq!(output["schema_version"], json!("scribe-draft.v1"));
    assert_eq!(output["source_note_version"], json!(1));
    assert_eq!(output["language"], json!("en"));

    // The structured contract round-trips through the strict validator.
    let parsed: wellos_domain::ai::ScribeDraftV1 =
        serde_json::from_value(output.clone()).expect("draft parses");
    parsed.validate().expect("draft validates");
    assert!(!parsed.transcript.is_empty());
    assert!(parsed.sections.len() >= 5, "{:?}", parsed.sections.len());
    assert!(
        parsed.sections.iter().any(|s| s.review_needed),
        "low-confidence sections must be marked for review"
    );
    assert!(
        !parsed.flags.is_empty(),
        "the fake script contains a contradiction"
    );
    // Every proposed section is assembled from cited transcript segments,
    // never invented: each section's text is exactly its segments joined.
    for section in &parsed.sections {
        assert!(!section.segments.is_empty(), "{section:?}");
        let quoted: Vec<&str> = section
            .segments
            .iter()
            .map(|i| parsed.transcript[*i as usize].text.trim_end_matches('.'))
            .collect();
        assert_eq!(
            section.text,
            format!("{}.", quoted.join(". ")),
            "{section:?}"
        );
    }
    assert!(!parsed.limitations.is_empty());

    // Determinism: a second recording produces identical structured text.
    let (st, draft2) = transcribe(&state, &enc, "dev-dr.garcia").await;
    assert_eq!(st, StatusCode::OK, "{draft2}");
    assert_eq!(draft2["output"]["sections"], output["sections"]);
    assert_eq!(draft2["output"]["transcript"], output["transcript"]);

    // Only the newest draft is reviewable.
    let statuses: Vec<String> = sqlx::query_scalar(
        "SELECT status FROM ai_artifacts WHERE encounter_id = $1::uuid AND artifact_type = 'scribe_draft'
         ORDER BY created_at",
    )
    .bind(&enc)
    .fetch_all(&state.pool)
    .await
    .unwrap();
    assert_eq!(
        statuses,
        vec!["superseded".to_string(), "awaiting_review".to_string()]
    );

    // Provenance: recording hash + note version, and outbox events.
    let citations: Value =
        sqlx::query_scalar("SELECT citations FROM ai_artifacts WHERE id = $1::uuid")
            .bind(draft["id"].as_str().unwrap())
            .fetch_one(&state.pool)
            .await
            .unwrap();
    let cites: Vec<String> = serde_json::from_value(citations).unwrap();
    assert!(
        cites.iter().any(|c| c.starts_with("recording:sha256:")),
        "{cites:?}"
    );
    assert!(
        cites.contains(&format!("encounter_note:{enc}:v1")),
        "{cites:?}"
    );
    let transcribed: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM outbox_events WHERE event_type = 'encounter.scribe.transcribed'
           AND resource_refs->>'encounter_id' = $1",
    )
    .bind(&enc)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(transcribed, 2);
}

#[tokio::test]
async fn raw_audio_is_never_persisted_anywhere_in_the_database() {
    let (state, _) = test_state().await;
    let (_, enc) = start_consultation(&state).await;
    grant_consent(&state, &enc).await;
    let audio = synthetic_audio(3_000);
    let (st, draft) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/scribe"),
        "dev-dr.garcia",
        Some(transcribe_body(&audio, 30_000)),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{draft}");

    // Neither the raw bytes nor their base64 form appear in any column of
    // any user table: the only trace is a one-way hash.
    let marker = String::from_utf8_lossy(SYNTHETIC_AUDIO_MARKER).to_string();
    let encoded_prefix: String = b64(&audio).chars().take(32).collect();
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT table_name FROM information_schema.tables
         WHERE table_schema = 'public' AND table_type = 'BASE TABLE'",
    )
    .fetch_all(&state.pool)
    .await
    .unwrap();
    assert!(!tables.is_empty());
    for table in &tables {
        let hits: i64 = sqlx::query_scalar(&format!(
            "SELECT COUNT(*) FROM \"{table}\" t WHERE t::text LIKE '%' || $1 || '%' OR t::text LIKE '%' || $2 || '%'"
        ))
        .bind(&marker)
        .bind(&encoded_prefix)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(hits, 0, "raw audio found in table {table}");
    }
    assert!(!tables
        .iter()
        .any(|t| t.contains("audio") || t.contains("recording_blob")));

    let input_hash: String =
        sqlx::query_scalar("SELECT input_hash FROM ai_artifacts WHERE id = $1::uuid")
            .bind(draft["id"].as_str().unwrap())
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(input_hash.len(), 64);
}

#[tokio::test]
async fn provider_failure_is_safe_bounded_and_retryable() {
    let (state, fake) = test_state().await;
    let (_, enc) = start_consultation(&state).await;
    grant_consent(&state, &enc).await;

    fake.set_unavailable(true);
    let (st, err) = transcribe(&state, &enc, "dev-dr.garcia").await;
    assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE, "{err}");
    assert_eq!(err["error"]["code"], json!("scribe_unavailable"));
    let message = err["error"]["message"].as_str().unwrap_or("");
    assert!(
        !message.contains("offline"),
        "provider detail must not leak: {message}"
    );
    let artifacts: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM ai_artifacts WHERE encounter_id = $1::uuid AND artifact_type = 'scribe_draft'",
    )
    .bind(&enc)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(artifacts, 0);
    let failures: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM outbox_events WHERE event_type = 'ai.generation.failed'
           AND resource_refs->>'encounter_id' = $1",
    )
    .bind(&enc)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(failures, 1);

    // The browser keeps the recording; a retry after recovery succeeds.
    fake.set_unavailable(false);
    let (st, draft) = transcribe(&state, &enc, "dev-dr.garcia").await;
    assert_eq!(st, StatusCode::OK, "{draft}");
}

#[derive(Clone, Default)]
struct LogSink(Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for LogSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogSink {
    type Writer = LogSink;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[tokio::test]
async fn logs_never_contain_audio_transcript_note_text_or_credentials() {
    let sink = LogSink::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(sink.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let (state, fake) = test_state().await;
    let (_, enc) = start_consultation(&state).await;
    grant_consent(&state, &enc).await;
    let audio = synthetic_audio(3_000);
    let body = transcribe_body(&audio, 30_000);
    let token = "dev-dr.garcia";

    // Failure, validation rejection and success paths all emit whatever
    // logging they have; none of it may carry payload content.
    fake.set_unavailable(true);
    let (st, _) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/scribe"),
        token,
        Some(body.clone()),
    )
    .await;
    assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE);
    fake.set_unavailable(false);
    let mut bad = body.clone();
    bad["mime_type"] = json!("video/mp4");
    let (st, _) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/scribe"),
        token,
        Some(bad),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, draft) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/scribe"),
        token,
        Some(body),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{draft}");
    let clinician_text = "Clinician typed this sensitive-looking sentence.";
    let (st, applied) = call(
        &state,
        "POST",
        &format!(
            "/api/v1/encounters/{enc}/scribe/{}/review",
            draft["id"].as_str().unwrap()
        ),
        token,
        Some(json!({
            "decision": "apply",
            "version": draft["note_version"],
            "assessment": clinician_text,
            "sections": [{ "section": "reason_for_encounter", "mode": "fill" }],
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{applied}");

    let logs = String::from_utf8_lossy(&sink.0.lock().unwrap()).to_string();
    let marker = String::from_utf8_lossy(SYNTHETIC_AUDIO_MARKER).to_string();
    let encoded_prefix: String = b64(&audio).chars().take(32).collect();
    assert!(!logs.contains(&marker), "raw audio in logs");
    assert!(!logs.contains(&encoded_prefix), "encoded audio in logs");
    assert!(!logs.contains(token), "credential in logs");
    assert!(
        !logs.contains(clinician_text),
        "clinician note text in logs"
    );
    for seg in draft["output"]["transcript"].as_array().unwrap() {
        let text = seg["text"].as_str().unwrap();
        assert!(!logs.contains(text), "transcript text in logs: {text}");
    }
    for sec in draft["output"]["sections"].as_array().unwrap() {
        let text = sec["text"].as_str().unwrap();
        assert!(!logs.contains(text), "generated note text in logs");
    }
}

// ---------------------------------------------------------------------------
// Authorization boundaries
// ---------------------------------------------------------------------------

#[tokio::test]
async fn scribe_requires_own_active_consultation_in_scope() {
    let (state, _) = test_state().await;
    let (patient_id, enc) = start_consultation(&state).await;
    grant_consent(&state, &enc).await;

    // Another physician at the same facility: not their encounter.
    let (st, err) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/recording-consent"),
        "dev-dr.lopez",
        Some(json!({ "granted": true })),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{err}");
    let (st, err) = transcribe(&state, &enc, "dev-dr.lopez").await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{err}");

    // A physician scoped to another facility gets the non-enumerating 404.
    let (st, _) = transcribe(&state, &enc, "dev-dr.annex").await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    // Registration staff have no documentation right at all.
    let (st, _) = transcribe(&state, &enc, "dev-reg.rivera").await;
    assert_ne!(st, StatusCode::OK);

    // Order-only encounters never accept recordings.
    let (st, order_only) = call(
        &state,
        "POST",
        "/api/v1/encounters",
        "dev-dr.garcia",
        Some(json!({ "patient_id": patient_id, "encounter_type": "order_only" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{order_only}");
    let oo = order_only["id"].as_str().unwrap();
    let (st, err) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{oo}/recording-consent"),
        "dev-dr.garcia",
        Some(json!({ "granted": true })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{err}");
    assert_eq!(err["error"]["code"], json!("not_a_consultation"));
}

#[tokio::test]
async fn closed_encounters_reject_transcription_and_application() {
    let (state, _) = test_state().await;
    let (_, enc) = start_consultation(&state).await;
    grant_consent(&state, &enc).await;
    let (st, draft) = transcribe(&state, &enc, "dev-dr.garcia").await;
    assert_eq!(st, StatusCode::OK, "{draft}");
    let artifact = draft["id"].as_str().unwrap().to_string();

    let (st, cancelled) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/cancel"),
        "dev-dr.garcia",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{cancelled}");

    let (st, err) = transcribe(&state, &enc, "dev-dr.garcia").await;
    assert_eq!(st, StatusCode::CONFLICT, "{err}");
    assert_eq!(err["error"]["code"], json!("encounter_not_active"));

    let (st, err) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/scribe/{artifact}/review"),
        "dev-dr.garcia",
        Some(json!({
            "decision": "apply",
            "sections": [{ "section": "plan", "mode": "fill" }],
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{err}");
    assert_eq!(err["error"]["code"], json!("encounter_not_active"));

    // Closing retired the awaiting draft.
    let status: String = sqlx::query_scalar("SELECT status FROM ai_artifacts WHERE id = $1::uuid")
        .bind(&artifact)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(status, "superseded");
}

/// Two tabs: draft A is partially applied (status `approved`, remaining
/// sections still insertable), then a new recording produces draft B. A's
/// remaining suggestions must no longer be applicable, and closing the
/// encounter must retire a partially applied draft just like an awaiting one.
#[tokio::test]
async fn a_new_recording_retires_a_partially_applied_draft() {
    let (state, _) = test_state().await;
    let (_, enc) = start_consultation(&state).await;
    grant_consent(&state, &enc).await;
    let artifact_status = |id: String| {
        let pool = state.pool.clone();
        async move {
            sqlx::query_scalar::<_, String>("SELECT status FROM ai_artifacts WHERE id = $1::uuid")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap()
        }
    };

    // Tab 1: record, apply one section of draft A.
    let (st, draft_a) = transcribe(&state, &enc, "dev-dr.garcia").await;
    assert_eq!(st, StatusCode::OK, "{draft_a}");
    let a = draft_a["id"].as_str().unwrap().to_string();
    let review_a = format!("/api/v1/encounters/{enc}/scribe/{a}/review");
    let (st, applied) = call(
        &state,
        "POST",
        &review_a,
        "dev-dr.garcia",
        Some(json!({
            "decision": "apply",
            "sections": [{ "section": "history_present_illness", "mode": "fill" }],
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{applied}");
    assert_eq!(applied["status"], json!("approved"));
    let version = applied["note"]["version"].as_i64().unwrap();
    assert_eq!(artifact_status(a.clone()).await, "approved");

    // Tab 2: a new recording replaces the current draft.
    let (st, draft_b) = transcribe(&state, &enc, "dev-dr.garcia").await;
    assert_eq!(st, StatusCode::OK, "{draft_b}");
    let b = draft_b["id"].as_str().unwrap().to_string();
    assert_ne!(a, b);
    assert_eq!(artifact_status(a.clone()).await, "superseded");
    assert_eq!(artifact_status(b.clone()).await, "awaiting_review");

    // Tab 1 tries to insert another section from the stale draft A.
    let hpi = draft_a["output"]["sections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["section"] == json!("history_present_illness"))
        .and_then(|s| s["text"].as_str())
        .unwrap();
    let (st, err) = call(
        &state,
        "POST",
        &review_a,
        "dev-dr.garcia",
        Some(json!({
            "decision": "apply",
            "version": version,
            "history_present_illness": hpi,
            "sections": [{ "section": "plan", "mode": "fill" }],
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{err}");
    assert_eq!(err["error"]["code"], json!("artifact_not_reviewable"));
    let (st, ws) = call(
        &state,
        "GET",
        &format!("/api/v1/encounters/{enc}"),
        "dev-dr.garcia",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        ws["note"]["version"],
        json!(version),
        "no text was inserted"
    );
    assert!(ws["note"]["plan"].as_str().unwrap_or("").is_empty());
    assert_eq!(ws["scribe_draft"]["id"], json!(b), "workspace shows B");

    // Partially apply B, then sign: a partially applied draft is retired by
    // closure too.
    let (st, applied) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/scribe/{b}/review"),
        "dev-dr.garcia",
        Some(json!({
            "decision": "apply",
            "version": version,
            "history_present_illness": hpi,
            "sections": [
                { "section": "reason_for_encounter", "mode": "fill" },
                { "section": "plan", "mode": "fill" },
            ],
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{applied}");
    assert_eq!(artifact_status(b.clone()).await, "approved");
    let (st, signed) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/sign"),
        "dev-dr.garcia",
        Some(json!({ "version": applied["note"]["version"] })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{signed}");
    assert_eq!(artifact_status(b).await, "superseded");
}

// ---------------------------------------------------------------------------
// Application: safe merge, per-section, version binding
// ---------------------------------------------------------------------------

#[tokio::test]
async fn application_fills_empty_sections_appends_to_filled_and_never_overwrites() {
    let (state, _) = test_state().await;
    let (_, enc) = start_consultation(&state).await;
    grant_consent(&state, &enc).await;
    let (st, note) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/note"),
        "dev-dr.garcia",
        Some(json!({ "assessment": "Clinician's own assessment." })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{note}");
    let (st, draft) = transcribe(&state, &enc, "dev-dr.garcia").await;
    assert_eq!(st, StatusCode::OK, "{draft}");
    let artifact = draft["id"].as_str().unwrap().to_string();
    let proposed = |section: &str| -> String {
        draft["output"]["sections"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["section"] == json!(section))
            .and_then(|s| s["text"].as_str())
            .unwrap()
            .to_string()
    };
    let review_path = format!("/api/v1/encounters/{enc}/scribe/{artifact}/review");

    // Filling a section the clinician already wrote is refused: their text
    // is never replaced.
    let (st, err) = call(
        &state,
        "POST",
        &review_path,
        "dev-dr.garcia",
        Some(json!({
            "decision": "apply",
            "version": 1,
            "assessment": "Clinician's own assessment.",
            "sections": [{ "section": "assessment", "mode": "fill" }],
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{err}");
    assert_eq!(err["error"]["code"], json!("section_not_empty"));

    // Fill an empty section and append to the filled one in one atomic
    // application; unsaved clinician text in a third section is kept.
    let (st, applied) = call(
        &state,
        "POST",
        &review_path,
        "dev-dr.garcia",
        Some(json!({
            "decision": "apply",
            "version": 1,
            "assessment": "Clinician's own assessment.",
            "follow_up": "Typed while the recording processed (unsaved).",
            "sections": [
                { "section": "history_present_illness", "mode": "fill" },
                { "section": "assessment", "mode": "append" },
            ],
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{applied}");
    assert_eq!(applied["status"], json!("approved"));
    assert_eq!(applied["note"]["version"], json!(2));
    let (st, ws) = call(
        &state,
        "GET",
        &format!("/api/v1/encounters/{enc}"),
        "dev-dr.garcia",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(ws["note"]["version"], json!(2));
    assert_eq!(
        ws["note"]["history_present_illness"],
        json!(proposed("history_present_illness"))
    );
    assert_eq!(
        ws["note"]["assessment"],
        json!(format!(
            "Clinician's own assessment.\n\n{}",
            proposed("assessment")
        ))
    );
    assert_eq!(
        ws["note"]["follow_up"],
        json!("Typed while the recording processed (unsaved).")
    );
    let applied_sections: Vec<&str> = ws["scribe_draft"]["review_detail"]["applied"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["section"].as_str().unwrap())
        .collect();
    assert_eq!(
        applied_sections,
        vec!["history_present_illness", "assessment"]
    );

    // Remaining sections can still be applied one at a time against the new
    // version; a section already applied cannot be applied twice.
    let (st, err) = call(
        &state,
        "POST",
        &review_path,
        "dev-dr.garcia",
        Some(json!({
            "decision": "apply",
            "version": 2,
            "history_present_illness": proposed("history_present_illness"),
            "assessment": format!("Clinician's own assessment.\n\n{}", proposed("assessment")),
            "follow_up": "Typed while the recording processed (unsaved).",
            "sections": [{ "section": "history_present_illness", "mode": "append" }],
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{err}");
    assert_eq!(err["error"]["code"], json!("section_already_applied"));
    let (st, applied) = call(
        &state,
        "POST",
        &review_path,
        "dev-dr.garcia",
        Some(json!({
            "decision": "apply",
            "version": 2,
            "history_present_illness": proposed("history_present_illness"),
            "assessment": format!("Clinician's own assessment.\n\n{}", proposed("assessment")),
            "follow_up": "Typed while the recording processed (unsaved).",
            "sections": [{ "section": "plan", "mode": "fill" }],
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{applied}");
    assert_eq!(applied["note"]["version"], json!(3));

    // Sections the draft never proposed, or unknown modes, are rejected.
    let (st, err) = call(
        &state,
        "POST",
        &review_path,
        "dev-dr.garcia",
        Some(json!({
            "decision": "apply",
            "version": 3,
            "sections": [{ "section": "plan", "mode": "replace" }],
        })),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{err}");
    // An approved (partially applied) draft can no longer be dismissed.
    let (st, err) = call(
        &state,
        "POST",
        &review_path,
        "dev-dr.garcia",
        Some(json!({ "decision": "dismiss" })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{err}");
    assert_eq!(err["error"]["code"], json!("artifact_not_reviewable"));

    // Application and approval are recorded in the outbox.
    let applied_events: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM outbox_events WHERE event_type = 'encounter.scribe.applied'
           AND resource_refs->>'encounter_id' = $1",
    )
    .bind(&enc)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(applied_events, 2);
}

#[tokio::test]
async fn application_against_a_changed_note_version_is_refused() {
    let (state, _) = test_state().await;
    let (_, enc) = start_consultation(&state).await;
    grant_consent(&state, &enc).await;
    let (st, draft) = transcribe(&state, &enc, "dev-dr.garcia").await;
    assert_eq!(st, StatusCode::OK, "{draft}");
    let artifact = draft["id"].as_str().unwrap().to_string();
    assert_eq!(draft["note_version"], Value::Null);

    // The note changes underneath (another tab saved).
    let (st, note) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/note"),
        "dev-dr.garcia",
        Some(json!({ "plan": "Saved elsewhere." })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{note}");

    // Applying with the version the clinician was looking at (none) fails
    // without touching the note.
    let (st, err) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/scribe/{artifact}/review"),
        "dev-dr.garcia",
        Some(json!({
            "decision": "apply",
            "sections": [{ "section": "plan", "mode": "fill" }],
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{err}");
    assert_eq!(err["error"]["code"], json!("version_required"));
    let (st, err) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/scribe/{artifact}/review"),
        "dev-dr.garcia",
        Some(json!({
            "decision": "apply",
            "version": 7,
            "plan": "Saved elsewhere.",
            "sections": [{ "section": "plan", "mode": "append" }],
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{err}");
    assert_eq!(err["error"]["code"], json!("version_conflict"));
    let (_, ws) = call(
        &state,
        "GET",
        &format!("/api/v1/encounters/{enc}"),
        "dev-dr.garcia",
        None,
    )
    .await;
    assert_eq!(ws["note"]["version"], json!(1));
    assert_eq!(ws["note"]["plan"], json!("Saved elsewhere."));
    assert_eq!(ws["scribe_draft"]["status"], json!("awaiting_review"));
    assert_eq!(ws["scribe_draft"]["stale"], json!(true));

    // Reloading does not make the old draft applicable again: it was
    // proposed against a note version that no longer exists, so even the
    // live version is refused and the note stays untouched.
    let (st, err) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/scribe/{artifact}/review"),
        "dev-dr.garcia",
        Some(json!({
            "decision": "apply",
            "version": 1,
            "plan": "Saved elsewhere.",
            "sections": [{ "section": "plan", "mode": "append" }],
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{err}");
    assert_eq!(err["error"]["code"], json!("artifact_stale"));
    let (_, ws) = call(
        &state,
        "GET",
        &format!("/api/v1/encounters/{enc}"),
        "dev-dr.garcia",
        None,
    )
    .await;
    assert_eq!(ws["note"]["version"], json!(1));
    assert_eq!(ws["note"]["plan"], json!("Saved elsewhere."));

    // A fresh recording binds to the live version and applies; the
    // dismissed path stays decision-only even for a stale draft.
    let (st, draft2) = transcribe(&state, &enc, "dev-dr.garcia").await;
    assert_eq!(st, StatusCode::OK, "{draft2}");
    assert_eq!(draft2["note_version"], json!(1));
    let (st, applied) = call(
        &state,
        "POST",
        &format!(
            "/api/v1/encounters/{enc}/scribe/{}/review",
            draft2["id"].as_str().unwrap()
        ),
        "dev-dr.garcia",
        Some(json!({
            "decision": "apply",
            "version": 1,
            "plan": "Saved elsewhere.",
            "sections": [{ "section": "plan", "mode": "append" }],
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{applied}");
    assert_eq!(applied["note"]["version"], json!(2));

    let (st, note) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/note"),
        "dev-dr.garcia",
        Some(json!({ "version": 2, "plan": "Edited after the first insert." })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{note}");
    let (st, draft3) = transcribe(&state, &enc, "dev-dr.garcia").await;
    assert_eq!(st, StatusCode::OK, "{draft3}");
    let (st, note) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/note"),
        "dev-dr.garcia",
        Some(json!({ "version": 3, "plan": "Edited again." })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{note}");
    let (st, dismissed) = call(
        &state,
        "POST",
        &format!(
            "/api/v1/encounters/{enc}/scribe/{}/review",
            draft3["id"].as_str().unwrap()
        ),
        "dev-dr.garcia",
        Some(json!({ "decision": "dismiss" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{dismissed}");
    assert_eq!(dismissed["status"], json!("rejected"));
    let (_, ws) = call(
        &state,
        "GET",
        &format!("/api/v1/encounters/{enc}"),
        "dev-dr.garcia",
        None,
    )
    .await;
    assert_eq!(ws["note"]["version"], json!(4));
    assert_eq!(ws["note"]["plan"], json!("Edited again."));
}

/// A partially applied draft is bound to the version its own application
/// produced, so its remaining sections stay insertable; a clinician save in
/// between makes it stale even when the live version is supplied. The
/// `Some -> None` direction cannot arise (notes are never deleted), and
/// `None -> Some` is the first case above.
#[tokio::test]
async fn a_partially_applied_draft_stays_bound_to_its_own_writes_only() {
    let (state, _) = test_state().await;
    let (_, enc) = start_consultation(&state).await;
    grant_consent(&state, &enc).await;
    let (st, draft) = transcribe(&state, &enc, "dev-dr.garcia").await;
    assert_eq!(st, StatusCode::OK, "{draft}");
    let artifact = draft["id"].as_str().unwrap().to_string();
    let review = format!("/api/v1/encounters/{enc}/scribe/{artifact}/review");

    let (st, applied) = call(
        &state,
        "POST",
        &review,
        "dev-dr.garcia",
        Some(json!({
            "decision": "apply",
            "sections": [{ "section": "plan", "mode": "fill" }],
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{applied}");
    assert_eq!(applied["note"]["version"], json!(1));
    let (_, ws) = call(
        &state,
        "GET",
        &format!("/api/v1/encounters/{enc}"),
        "dev-dr.garcia",
        None,
    )
    .await;
    assert_eq!(ws["scribe_draft"]["status"], json!("approved"));
    assert_eq!(ws["scribe_draft"]["stale"], json!(false));

    // Its own write advanced the binding, so a second section still applies.
    let (st, applied) = call(
        &state,
        "POST",
        &review,
        "dev-dr.garcia",
        Some(json!({
            "decision": "apply",
            "version": 1,
            "plan": ws["note"]["plan"],
            "sections": [{ "section": "reason_for_encounter", "mode": "fill" }],
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{applied}");
    assert_eq!(applied["note"]["version"], json!(2));

    // The clinician saves in between: the draft is now stale even with the
    // live version, and the note is left exactly as saved.
    let (st, note) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/note"),
        "dev-dr.garcia",
        Some(json!({
            "version": 2,
            "reason_for_encounter": applied["note"]["reason_for_encounter"],
            "plan": applied["note"]["plan"],
            "assessment": "Typed by the clinician.",
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{note}");
    assert_eq!(note["version"], json!(3));
    let (_, ws) = call(
        &state,
        "GET",
        &format!("/api/v1/encounters/{enc}"),
        "dev-dr.garcia",
        None,
    )
    .await;
    assert_eq!(ws["scribe_draft"]["stale"], json!(true));
    let (st, err) = call(
        &state,
        "POST",
        &review,
        "dev-dr.garcia",
        Some(json!({
            "decision": "apply",
            "version": 3,
            "reason_for_encounter": ws["note"]["reason_for_encounter"],
            "plan": ws["note"]["plan"],
            "assessment": "Typed by the clinician.",
            "sections": [{ "section": "history_present_illness", "mode": "fill" }],
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{err}");
    assert_eq!(err["error"]["code"], json!("artifact_stale"));
    let (_, after) = call(
        &state,
        "GET",
        &format!("/api/v1/encounters/{enc}"),
        "dev-dr.garcia",
        None,
    )
    .await;
    assert_eq!(after["note"]["version"], json!(3));
    assert_eq!(after["note"]["history_present_illness"], Value::Null);
    assert_eq!(after["scribe_draft"]["status"], json!("approved"));

    // The superseding recording binds to the live version and applies.
    let (st, draft2) = transcribe(&state, &enc, "dev-dr.garcia").await;
    assert_eq!(st, StatusCode::OK, "{draft2}");
    assert_eq!(draft2["note_version"], json!(3));
    let (st, applied) = call(
        &state,
        "POST",
        &format!(
            "/api/v1/encounters/{enc}/scribe/{}/review",
            draft2["id"].as_str().unwrap()
        ),
        "dev-dr.garcia",
        Some(json!({
            "decision": "apply",
            "version": 3,
            "reason_for_encounter": after["note"]["reason_for_encounter"],
            "plan": after["note"]["plan"],
            "assessment": "Typed by the clinician.",
            "sections": [{ "section": "history_present_illness", "mode": "fill" }],
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{applied}");
    assert_eq!(applied["note"]["version"], json!(4));
}

#[tokio::test]
async fn signed_notes_cannot_receive_scribe_text() {
    let (state, _) = test_state().await;
    let (_, enc) = start_consultation(&state).await;
    grant_consent(&state, &enc).await;
    let (st, draft) = transcribe(&state, &enc, "dev-dr.garcia").await;
    assert_eq!(st, StatusCode::OK, "{draft}");
    let artifact = draft["id"].as_str().unwrap().to_string();
    let (st, note) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/note"),
        "dev-dr.garcia",
        Some(json!({ "reason_for_encounter": "Visit", "assessment": "Stable" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{note}");
    let (st, signed) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/sign"),
        "dev-dr.garcia",
        Some(json!({ "version": 1 })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{signed}");

    let (st, err) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/scribe/{artifact}/review"),
        "dev-dr.garcia",
        Some(json!({
            "decision": "apply",
            "version": 1,
            "reason_for_encounter": "Visit",
            "assessment": "Stable",
            "sections": [{ "section": "plan", "mode": "fill" }],
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{err}");
    assert_eq!(err["error"]["code"], json!("encounter_not_active"));
}

// ---------------------------------------------------------------------------
// Workspace brief + diagnostics, dashboard cockpit, create-or-resume
// ---------------------------------------------------------------------------

async fn seeded_patient(state: &AppState, given: &str) -> Value {
    let (st, res) = call(
        state,
        "GET",
        &format!("/api/v1/patients?query={given}"),
        "dev-dr.garcia",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{res}");
    res["patients"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|p| p["given_name"] == json!(given))
                .cloned()
        })
        .unwrap_or_else(|| panic!("seeded patient {given} not found: {res}"))
}

#[tokio::test]
async fn workspace_carries_patient_brief_and_grouped_diagnostic_history() {
    let (state, _) = test_state().await;
    let carlos = seeded_patient(&state, "Carlos").await;
    let carlos_id = carlos["id"].as_str().unwrap().to_string();
    let (st, enc) = call(
        &state,
        "POST",
        "/api/v1/encounters",
        "dev-dr.garcia",
        Some(json!({ "patient_id": carlos_id, "resume": true })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{enc}");
    let enc_id = enc["id"].as_str().unwrap().to_string();

    let (st, ws) = call(
        &state,
        "GET",
        &format!("/api/v1/encounters/{enc_id}?lang=es"),
        "dev-dr.garcia",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{ws}");

    let brief = &ws["brief"];
    assert!(brief["generated_at"].is_string());
    assert!(brief["recent_notes"].is_array());
    assert!(brief["open_tasks"].is_array());
    assert!(brief["open_requests"].is_array(), "{brief}");
    assert!(
        brief["recent_abnormal"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["code"] == json!("2345-7") && o["abnormal"] == json!("high")),
        "{brief}"
    );
    assert!(
        brief["open_requests"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["loop_state"] == json!("ordered")
                && r["display"].as_str().unwrap_or("").starts_with("Glucose")),
        "pending glucose order should be listed: {brief}"
    );

    let dx = &ws["diagnostics"];
    let tests = dx["tests"].as_array().expect("tests grouped");
    let glucose = tests
        .iter()
        .find(|t| t["code"] == json!("2345-7"))
        .unwrap_or_else(|| panic!("glucose group missing: {dx}"));
    assert_eq!(glucose["unit"], json!("mg/dL"));
    assert_eq!(glucose["reference_range"], json!("70-99 mg/dL"));
    assert_eq!(glucose["direction"], json!("rising"), "{glucose}");
    assert_eq!(glucose["latest_abnormal"], json!("high"));
    assert!(glucose["pending_count"].as_u64().unwrap() >= 1, "{glucose}");
    let values: Vec<String> = glucose["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["value"].to_string().trim_matches('"').to_string())
        .collect();
    assert!(
        values.iter().any(|v| v == "118") && values.iter().any(|v| v == "134"),
        "{values:?}"
    );
    for r in glucose["results"].as_array().unwrap() {
        assert!(r["effective_at"].is_string());
        assert!(r["abnormal"].is_string() || r["abnormal"].is_null());
    }
    let potassium = tests.iter().find(|t| t["code"] == json!("2823-3")).unwrap();
    assert_eq!(potassium["latest_abnormal"], json!("high"));
    assert!(potassium["results"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["critical"] == json!(true)));

    // Assistive dMind commentary: Spanish, cites facts, labelled, bounded.
    let analysis = &dx["analysis"];
    assert_eq!(analysis["schema_version"], json!("diagnostic-trends.v1"));
    assert_eq!(analysis["language"], json!("es"));
    assert_eq!(analysis["provider"]["provider"], json!("dmind-fake"));
    let statements = analysis["statements"].as_array().unwrap();
    assert!(!statements.is_empty());
    for s in statements {
        assert!(!s["facts"].as_array().unwrap().is_empty(), "{s}");
        let text = s["text"].as_str().unwrap().to_lowercase();
        for banned in ["recomend", "recommend", "prescrib", "diagnos"] {
            assert!(!text.contains(banned), "{text}");
        }
    }
    assert!(!analysis["limitations"].as_array().unwrap().is_empty());

    // Default language is English.
    let (_, ws_en) = call(
        &state,
        "GET",
        &format!("/api/v1/encounters/{enc_id}"),
        "dev-dr.garcia",
        None,
    )
    .await;
    assert_eq!(ws_en["diagnostics"]["analysis"]["language"], json!("en"));

    // Clean up so the seeded worklist counts stay stable for other tests.
    let (st, _) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc_id}/cancel"),
        "dev-dr.garcia",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
}

/// Deliver one laboratory result for `service_request`, optionally amending
/// an earlier observation of the same request; returns the observation id.
async fn deliver_result(
    state: &AppState,
    service_request: &str,
    code: &str,
    (value, unit, range): (f64, &str, Option<&str>),
    effective_at: chrono::DateTime<chrono::Utc>,
    amends: Option<&str>,
) -> String {
    let (st, res) = call(
        state,
        "POST",
        "/api/v1/lab/results",
        "dev-lab.chen",
        Some(json!({
            "service_request_id": service_request,
            "code_loinc": code,
            "value": value,
            "unit": unit,
            "reference_range": range,
            "source_system": "fake-lab",
            "idempotency_key": uniq("dx-key"),
            "effective_at": effective_at,
            "amends_observation_id": amends,
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{res}");
    res["observation_id"].as_str().unwrap().to_string()
}

/// Order `code` in `enc` and ingest one result for it; returns the
/// (service request, observation) ids.
async fn order_and_ingest(
    state: &AppState,
    enc: &str,
    (code, display): (&str, &str),
    result: (f64, &str, Option<&str>),
    effective_at: chrono::DateTime<chrono::Utc>,
) -> (String, String) {
    let (st, sr) = call(
        state,
        "POST",
        "/api/v1/service-requests",
        "dev-dr.garcia",
        Some(json!({ "encounter_id": enc, "code_loinc": code, "display": display })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{sr}");
    let sr_id = sr["id"].as_str().unwrap().to_string();
    let obs = deliver_result(state, &sr_id, code, result, effective_at, None).await;
    (sr_id, obs)
}

/// Order `code` in `enc` and ingest one result for it; returns the
/// observation id.
async fn ingest_result(
    state: &AppState,
    enc: &str,
    test: (&str, &str),
    result: (f64, &str, Option<&str>),
    effective_at: chrono::DateTime<chrono::Utc>,
) -> String {
    order_and_ingest(state, enc, test, result, effective_at)
        .await
        .1
}

async fn workspace_tests(state: &AppState, enc: &str) -> Vec<Value> {
    let (st, ws) = call(
        state,
        "GET",
        &format!("/api/v1/encounters/{enc}"),
        "dev-dr.garcia",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{ws}");
    ws["diagnostics"]["tests"].as_array().unwrap().clone()
}

#[tokio::test]
async fn diagnostic_history_normalizes_units_and_never_compares_incomparable_values() {
    let (state, _) = test_state().await;
    let (_, enc) = start_consultation(&state).await;
    let t0 = chrono::Utc::now() - chrono::Duration::days(3);

    // Glucose reported first in mg/dL, then twice in mmol/L: the series is
    // expressed in the newest unit and the older value converted into it.
    let first = ingest_result(
        &state,
        &enc,
        ("2345-7", "Glucose"),
        (118.0, "mg/dL", Some("70-99 mg/dL")),
        t0,
    )
    .await;
    ingest_result(
        &state,
        &enc,
        ("2345-7", "Glucose"),
        (7.5, "mmol/L", Some("3.9-5.5 mmol/L")),
        t0 + chrono::Duration::days(1),
    )
    .await;
    ingest_result(
        &state,
        &enc,
        ("2345-7", "Glucose"),
        (8.0, "mmol/L", Some("3.9-5.5 mmol/L")),
        t0 + chrono::Duration::days(2),
    )
    .await;

    // Potassium reported in mmol/L and then in a unit with no known
    // conversion: shown as reported, but never trended against each other.
    ingest_result(
        &state,
        &enc,
        ("2823-3", "Potassium"),
        (5.0, "mmol/L", Some("3.5-5.1 mmol/L")),
        t0,
    )
    .await;
    ingest_result(
        &state,
        &enc,
        ("2823-3", "Potassium"),
        (21.5, "mg/dL", Some("13.7-20 mg/dL")),
        t0 + chrono::Duration::days(1),
    )
    .await;

    let tests = workspace_tests(&state, &enc).await;
    let glucose = tests.iter().find(|t| t["code"] == json!("2345-7")).unwrap();
    assert_eq!(glucose["unit"], json!("mmol/L"), "{glucose}");
    assert_eq!(glucose["reference_range"], json!("3.9-5.5 mmol/L"));
    assert_eq!(glucose["latest_value"], json!("8"), "{glucose}");
    assert_eq!(glucose["latest_abnormal"], json!("high"));
    // 118 mg/dL -> 6.55 mmol/L, 7.5, 8: rising. Raw comparison (118, 7.5,
    // 8) would have reported a fall.
    assert_eq!(glucose["direction"], json!("rising"), "{glucose}");
    assert_eq!(glucose["incomparable_count"], json!(0));
    assert_eq!(glucose["result_count"], json!(3));
    let converted = glucose["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == json!(first))
        .unwrap();
    assert!(
        converted["value"].as_str().unwrap().starts_with("118"),
        "{converted}"
    );
    assert_eq!(converted["unit"], json!("mg/dL"));
    assert_eq!(converted["normalized_value"], json!("6.55"), "{converted}");
    assert_eq!(converted["comparable"], json!(true));
    // Each row is flagged against its own reference range.
    assert_eq!(converted["abnormal"], json!("high"));

    let potassium = tests.iter().find(|t| t["code"] == json!("2823-3")).unwrap();
    assert_eq!(potassium["unit"], json!("mg/dL"), "{potassium}");
    assert_eq!(potassium["latest_value"], json!("21.5"));
    assert_eq!(potassium["direction"], json!("mixed_units"), "{potassium}");
    assert_eq!(potassium["incomparable_count"], json!(1));
    assert_eq!(potassium["result_count"], json!(2));
    let rows = potassium["results"].as_array().unwrap();
    let older = rows.iter().find(|r| r["unit"] == json!("mmol/L")).unwrap();
    assert_eq!(older["comparable"], json!(false), "{older}");
    assert!(older["normalized_value"].is_null());
    let newer = rows.iter().find(|r| r["unit"] == json!("mg/dL")).unwrap();
    assert_eq!(newer["comparable"], json!(true));
    assert_eq!(newer["normalized_value"], json!("21.5"));

    // Commentary states the limitation instead of inventing a direction, and
    // only cites the comparable results.
    let (_, ws) = call(
        &state,
        "GET",
        &format!("/api/v1/encounters/{enc}"),
        "dev-dr.garcia",
        None,
    )
    .await;
    let statements = ws["diagnostics"]["analysis"]["statements"]
        .as_array()
        .unwrap()
        .clone();
    let k = statements
        .iter()
        .find(|s| s["code"] == json!("2823-3"))
        .unwrap();
    let text = k["text"].as_str().unwrap();
    assert!(text.contains("trend not calculated"), "{text}");
    assert!(text.contains("latest 21.5 mg/dL"), "{text}");
    assert!(
        !text.contains("rising") && !text.contains("falling"),
        "{text}"
    );
    assert_eq!(k["facts"].as_array().unwrap().len(), 1, "{k}");
    let g = statements
        .iter()
        .find(|s| s["code"] == json!("2345-7"))
        .unwrap();
    assert!(
        g["text"].as_str().unwrap().contains("latest 8 mmol/L"),
        "{g}"
    );
    assert!(g["text"].as_str().unwrap().contains("rising"), "{g}");
}

/// The brief lists the five most recent abnormal results of the patient's
/// whole history. Normal results, however many and however recent, must not
/// push older abnormal ones out; amended rows are excluded and replaced by
/// their amendment, and the five-item cap applies to abnormal results only.
#[tokio::test]
async fn recent_abnormal_results_are_not_crowded_out_by_newer_normal_ones() {
    let (state, _) = test_state().await;
    let (_, enc) = start_consultation(&state).await;
    let t0 = chrono::Utc::now() - chrono::Duration::days(90);
    let day = chrono::Duration::days(1);

    // Six abnormal results, oldest first (one with a signed range).
    let mut old_abnormal = Vec::new();
    for i in 0..6i64 {
        let (_, obs) = if i == 5 {
            order_and_ingest(
                &state,
                &enc,
                ("11555-0", "Base excess"),
                (-3.0, "mmol/L", Some("-2-2 mmol/L")),
                t0 + day * i as i32,
            )
            .await
        } else {
            order_and_ingest(
                &state,
                &enc,
                ("2823-3", "Potassium"),
                (6.0 + i as f64 / 10.0, "mmol/L", Some("3.5-5.1 mmol/L")),
                t0 + day * i as i32,
            )
            .await
        };
        old_abnormal.push(obs);
    }

    // Then 22 normal glucose results, all newer than every abnormal one.
    let mut normals = Vec::new();
    for i in 0..22i32 {
        normals.push(
            order_and_ingest(
                &state,
                &enc,
                ("2345-7", "Glucose"),
                (85.0, "mg/dL", Some("70-99 mg/dL")),
                t0 + day * (10 + i),
            )
            .await,
        );
    }

    // The newest result is abnormal but is then amended to a normal value:
    // neither row may be listed.
    let (sr_b, obs_b) = order_and_ingest(
        &state,
        &enc,
        ("2345-7", "Glucose"),
        (134.0, "mg/dL", Some("70-99 mg/dL")),
        t0 + day * 40,
    )
    .await;
    deliver_result(
        &state,
        &sr_b,
        "2345-7",
        (90.0, "mg/dL", Some("70-99 mg/dL")),
        t0 + day * 40 + chrono::Duration::hours(1),
        Some(&obs_b),
    )
    .await;

    // One normal result is corrected to an abnormal value: the amendment
    // is listed, the amended row is not.
    let (sr_c, obs_c_original) = &normals[21];
    let obs_c = deliver_result(
        &state,
        sr_c,
        "2345-7",
        (140.0, "mg/dL", Some("70-99 mg/dL")),
        t0 + day * 31 + chrono::Duration::hours(1),
        Some(obs_c_original),
    )
    .await;

    let (st, ws) = call(
        &state,
        "GET",
        &format!("/api/v1/encounters/{enc}"),
        "dev-dr.garcia",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{ws}");
    let listed: Vec<&Value> = ws["brief"]["recent_abnormal"]
        .as_array()
        .unwrap()
        .iter()
        .collect();
    let ids: Vec<&str> = listed.iter().map(|o| o["id"].as_str().unwrap()).collect();
    // Newest abnormal first: the amendment, then the base excess, then the
    // three newest potassium results; the two oldest fall to the cap.
    assert_eq!(
        ids,
        vec![
            obs_c.as_str(),
            old_abnormal[5].as_str(),
            old_abnormal[4].as_str(),
            old_abnormal[3].as_str(),
            old_abnormal[2].as_str(),
        ],
        "{listed:?}"
    );
    assert!(!ids.contains(&obs_b.as_str()));
    assert!(!ids.contains(&obs_c_original.as_str()));
    assert_eq!(listed[0]["abnormal"], json!("high"));
    assert!(
        listed[0]["value"].as_str().unwrap().starts_with("140"),
        "{}",
        listed[0]
    );
    assert_eq!(listed[1]["abnormal"], json!("low"), "{}", listed[1]);
    assert_eq!(listed[1]["reference_range"], json!("-2-2 mmol/L"));
    for w in listed.windows(2) {
        assert!(
            w[0]["effective_at"].as_str().unwrap() > w[1]["effective_at"].as_str().unwrap(),
            "{listed:?}"
        );
    }
}

#[tokio::test]
async fn overdue_tasks_stay_outstanding_and_superseded_tasks_drop_out_everywhere() {
    let (state, _) = test_state().await;
    let (patient_id, enc) = start_consultation(&state).await;
    // A critical potassium creates a high-priority follow-up task.
    ingest_result(
        &state,
        &enc,
        ("2823-3", "Potassium"),
        (7.1, "mmol/L", Some("3.5-5.1 mmol/L")),
        chrono::Utc::now(),
    )
    .await;
    let patient = Uuid::parse_str(&patient_id).unwrap();
    let task_id: Uuid = sqlx::query_scalar(
        "SELECT id FROM follow_up_tasks WHERE patient_id = $1 AND status = 'open'",
    )
    .bind(patient)
    .fetch_one(&state.pool)
    .await
    .unwrap();

    async fn listed(
        state: &AppState,
        enc: &str,
        task_id: Uuid,
    ) -> (Option<Value>, Option<Value>, Option<Value>) {
        let (st, ws) = call(
            state,
            "GET",
            &format!("/api/v1/encounters/{enc}"),
            "dev-dr.garcia",
            None,
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{ws}");
        let brief_task = ws["brief"]["open_tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["id"] == json!(task_id))
            .cloned();
        let (st, cockpit) = call(
            state,
            "GET",
            "/api/v1/dashboard/cockpit",
            "dev-dr.garcia",
            None,
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{cockpit}");
        let dashboard_task = cockpit["pending_tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["id"] == json!(task_id))
            .cloned();
        let patient_id = ws["patient"]["id"].clone();
        let attention = cockpit["attention"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["patient"]["id"] == patient_id)
            .cloned();
        (brief_task, dashboard_task, attention)
    }

    // Open: visible in the brief, on the dashboard and counted for attention.
    let (brief, dash, attention) = listed(&state, &enc, task_id).await;
    assert_eq!(brief.unwrap()["status"], json!("open"));
    assert_eq!(dash.unwrap()["status"], json!("open"));
    assert_eq!(attention.unwrap()["open_tasks"], json!(1));

    // Overdue (as the sweep marks it): still outstanding, and sorted first.
    sqlx::query("UPDATE follow_up_tasks SET status = 'overdue', priority = 'urgent' WHERE id = $1")
        .bind(task_id)
        .execute(&state.pool)
        .await
        .unwrap();
    let (brief, dash, attention) = listed(&state, &enc, task_id).await;
    assert_eq!(brief.unwrap()["status"], json!("overdue"));
    let dash = dash.expect("overdue task must stay on the dashboard");
    assert_eq!(dash["status"], json!("overdue"));
    assert_eq!(attention.unwrap()["open_tasks"], json!(1));
    let (_, cockpit) = call(
        &state,
        "GET",
        "/api/v1/dashboard/cockpit",
        "dev-dr.garcia",
        None,
    )
    .await;
    let pending = cockpit["pending_tasks"].as_array().unwrap();
    let position = pending
        .iter()
        .position(|t| t["id"] == json!(task_id))
        .unwrap();
    assert!(
        pending[..position]
            .iter()
            .all(|t| t["status"] == json!("overdue")),
        "overdue tasks sort before open ones: {cockpit}"
    );

    // Superseded (result amended): no longer outstanding anywhere.
    sqlx::query("UPDATE follow_up_tasks SET status = 'superseded' WHERE id = $1")
        .bind(task_id)
        .execute(&state.pool)
        .await
        .unwrap();
    let (brief, dash, attention) = listed(&state, &enc, task_id).await;
    assert!(brief.is_none(), "superseded task listed in the brief");
    assert!(dash.is_none(), "superseded task listed on the dashboard");
    // The critical alert still draws attention, but the task is not counted.
    assert_eq!(attention.unwrap()["open_tasks"], json!(0));

    // Completed tasks are history as well.
    sqlx::query("UPDATE follow_up_tasks SET status = 'completed' WHERE id = $1")
        .bind(task_id)
        .execute(&state.pool)
        .await
        .unwrap();
    let (brief, dash, _) = listed(&state, &enc, task_id).await;
    assert!(brief.is_none() && dash.is_none());
}

#[tokio::test]
async fn start_consultation_resumes_own_in_progress_consultation() {
    let (state, _) = test_state().await;
    let (_, patient_id, identifier) = register_patient(&state).await;

    let (st, first) = call(
        &state,
        "POST",
        "/api/v1/encounters",
        "dev-dr.garcia",
        Some(json!({ "patient_id": patient_id, "resume": true })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{first}");
    assert_eq!(first["resumed"], json!(false));
    let first_id = first["id"].as_str().unwrap().to_string();

    // Same clinician, same patient: the open consultation is returned.
    let (st, again) = call(
        &state,
        "POST",
        "/api/v1/encounters",
        "dev-dr.garcia",
        Some(json!({ "patient_id": patient_id, "resume": true })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{again}");
    assert_eq!(again["resumed"], json!(true));
    assert_eq!(again["id"], json!(first_id));

    // Search surfaces the same hint without exposing anything else.
    let (st, res) = call(
        &state,
        "GET",
        &format!("/api/v1/patients?query={identifier}"),
        "dev-dr.garcia",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{res}");
    let items = res["patients"].as_array().unwrap();
    let me = items
        .iter()
        .find(|p| p["id"] == json!(patient_id))
        .unwrap_or_else(|| panic!("{res}"));
    assert_eq!(me["open_consultation_id"], json!(first_id));

    // Another clinician does not resume someone else's consultation.
    let (st, other) = call(
        &state,
        "POST",
        "/api/v1/encounters",
        "dev-dr.lopez",
        Some(json!({ "patient_id": patient_id, "resume": true })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{other}");
    assert_eq!(other["resumed"], json!(false));
    assert_ne!(other["id"], json!(first_id));

    // Without the flag, or for order-only contexts, a new encounter opens.
    let (st, fresh) = call(
        &state,
        "POST",
        "/api/v1/encounters",
        "dev-dr.garcia",
        Some(json!({ "patient_id": patient_id })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{fresh}");
    assert_eq!(fresh["resumed"], json!(false));
    assert_ne!(fresh["id"], json!(first_id));

    // After the consultation closes it is no longer resumable.
    for id in [first_id.as_str(), fresh["id"].as_str().unwrap()] {
        let (st, _) = call(
            &state,
            "POST",
            &format!("/api/v1/encounters/{id}/cancel"),
            "dev-dr.garcia",
            None,
        )
        .await;
        assert_eq!(st, StatusCode::OK);
    }
    let (st, after) = call(
        &state,
        "POST",
        "/api/v1/encounters",
        "dev-dr.garcia",
        Some(json!({ "patient_id": patient_id, "resume": true })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{after}");
    assert_eq!(after["resumed"], json!(false));
}

#[tokio::test]
async fn dashboard_cockpit_is_scoped_and_free_of_generated_text() {
    let (state, _) = test_state().await;
    let (_, enc) = start_consultation(&state).await;
    grant_consent(&state, &enc).await;
    let (st, draft) = transcribe(&state, &enc, "dev-dr.garcia").await;
    assert_eq!(st, StatusCode::OK, "{draft}");

    let (st, cockpit) = call(
        &state,
        "GET",
        "/api/v1/dashboard/cockpit",
        "dev-dr.garcia",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{cockpit}");
    assert!(cockpit["generated_at"].is_string());
    let drafts = cockpit["draft_consultations"].as_array().unwrap();
    assert!(
        drafts.iter().any(|d| d["id"] == json!(enc)),
        "own in-progress consultation must be listed: {cockpit}"
    );
    for d in drafts {
        assert!(d["patient"]["given_name"].is_string(), "{d}");
    }
    let attention = cockpit["attention"].as_array().unwrap();
    assert!(
        attention
            .iter()
            .any(|a| a["open_alerts"].as_u64().unwrap_or(0) > 0),
        "seeded critical alert should draw attention: {cockpit}"
    );
    assert!(!cockpit["pending_tasks"].as_array().unwrap().is_empty());
    let activity = cockpit["ai_activity"].as_array().unwrap();
    assert!(activity
        .iter()
        .any(|a| a["artifact_type"] == json!("scribe_draft")));
    for a in activity {
        assert!(
            a.get("output").is_none(),
            "no generated text on the dashboard: {a}"
        );
        assert!(a.get("summary").is_none(), "{a}");
    }

    // Another physician does not see dr.garcia's draft consultation.
    let (st, other) = call(
        &state,
        "GET",
        "/api/v1/dashboard/cockpit",
        "dev-dr.lopez",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{other}");
    assert!(
        !other["draft_consultations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["id"] == json!(enc)),
        "{other}"
    );

    // The out-of-facility physician sees nothing from facility A.
    let (st, annex) = call(
        &state,
        "GET",
        "/api/v1/dashboard/cockpit",
        "dev-dr.annex",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{annex}");
    assert!(annex["attention"].as_array().unwrap().is_empty(), "{annex}");
    assert!(
        !annex["draft_consultations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["id"] == json!(enc)),
        "{annex}"
    );
    assert!(
        annex["pending_tasks"].as_array().unwrap().is_empty(),
        "{annex}"
    );

    // Registration staff have no worklist right.
    let (st, _) = call(
        &state,
        "GET",
        "/api/v1/dashboard/cockpit",
        "dev-reg.rivera",
        None,
    )
    .await;
    assert_ne!(st, StatusCode::OK);

    let (st, _) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/cancel"),
        "dev-dr.garcia",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
}
