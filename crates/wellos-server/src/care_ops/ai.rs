//! Governed dMind Care Operations artifacts. Every operation is built from
//! deterministic facts already persisted by this module, runs through the
//! AIArtifact lifecycle (`aigov::plan_scoped` → gateway → re-validation →
//! `ai_artifacts` row with provenance), and is advisory until a human with
//! `care_ops.review_ai` accepts or rejects it. Nothing here mutates flow,
//! theatre, workforce or notification state.

use crate::aigov;
use crate::audit;
use crate::auth::AuthContext;
use crate::care_ops::{self as ops, forecast, theatre};
use crate::error::ApiError;
use crate::state::AppState;
use chrono::Utc;
use dmind_gateway::care_ops::{
    self as gw, CareFlowBriefRequest, DischargeCoordinationRequest,
    OperationalForecastExplanationRequest, PlacementCandidateFact, PlacementRankingRequest,
    ShiftHandoffDraftRequest, TaskFact, TheatreCaseFact, TheatrePlanExplanationRequest,
    CARE_FLOW_BRIEF_TEMPLATE, DISCHARGE_COORDINATION_TEMPLATE,
    OPERATIONAL_FORECAST_EXPLANATION_TEMPLATE, PLACEMENT_RANKING_TEMPLATE,
    SHIFT_HANDOFF_DRAFT_TEMPLATE, THEATRE_PLAN_EXPLANATION_TEMPLATE,
};
use dmind_gateway::{hash_json, GatewayError};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};
use sqlx::{PgConnection, Row};
use uuid::Uuid;
use wellos_domain::ai::{ArtifactStatus, ProviderInfo, ReviewDecision};
use wellos_domain::care_ops::{OpsForecast, PLACEMENT_MATCHER_VERSION};
use wellos_domain::care_ops_ai::{
    CARE_FLOW_BRIEF_SCHEMA, DISCHARGE_COORDINATION_SCHEMA, OPERATIONAL_FORECAST_EXPLANATION_SCHEMA,
    PLACEMENT_RANKING_SCHEMA, SHIFT_HANDOFF_DRAFT_SCHEMA, THEATRE_PLAN_EXPLANATION_SCHEMA,
};

pub const CARE_OPS_ARTIFACT_TYPES: [&str; 6] = [
    "care_flow_brief",
    "placement_ranking",
    "discharge_coordination",
    "theatre_plan_explanation",
    "operational_forecast_explanation",
    "shift_handoff_draft",
];

/// Result of one governed generation; `mode` is `dmind` when an artifact was
/// produced (fresh or reused) and `skipped` with a reason otherwise.
#[derive(Debug, Serialize)]
pub struct Outcome<T: Serialize> {
    pub mode: &'static str,
    pub artifact_id: Option<Uuid>,
    pub status: Option<&'static str>,
    pub output: Option<T>,
    pub synthetic: Option<bool>,
    pub reused: bool,
    pub reason: Option<&'static str>,
    pub input_hash: String,
}

impl<T: Serialize> Outcome<T> {
    fn skipped(reason: &'static str, hash: &str) -> Self {
        Outcome {
            mode: "skipped",
            artifact_id: None,
            status: None,
            output: None,
            synthetic: None,
            reused: false,
            reason: Some(reason),
            input_hash: hash.to_string(),
        }
    }
}

struct Binding {
    artifact_type: &'static str,
    template: &'static str,
    schema: &'static str,
    /// `ai_artifacts` column that binds the artifact to its resource.
    column: &'static str,
    resource_id: Uuid,
    patient_id: Option<Uuid>,
    scope: aigov::ReuseScope,
    input_refs: Vec<String>,
    audit_rt: &'static str,
}

fn code_of(err: &GatewayError) -> &'static str {
    match err {
        GatewayError::Disabled(_) => "disabled",
        GatewayError::InvalidOutput(_) => "invalid_output",
        GatewayError::PolicyDenied(_) => "policy_denied",
        GatewayError::Unavailable(_) => "unavailable",
    }
}

fn lang(l: Option<&str>) -> String {
    match l {
        Some(l) if l.starts_with("es") => "es".into(),
        _ => "en".into(),
    }
}

/// Plans, executes (or reuses) and persists one artifact. The caller owns the
/// transaction so resource-side bindings commit atomically with the artifact.
#[allow(clippy::too_many_arguments)]
async fn produce<T, F>(
    state: &AppState,
    ctx: &AuthContext,
    tx: &mut PgConnection,
    tenant_id: Uuid,
    hash: &str,
    call: F,
    validate: impl Fn(&Value) -> Result<T, GatewayError>,
    cites: impl Fn(&T) -> (Vec<String>, Vec<String>),
    b: Binding,
) -> Result<Outcome<T>, ApiError>
where
    T: Serialize + DeserializeOwned,
    F: std::future::Future<Output = Result<gw::CareOpsResponse<T>, GatewayError>>,
{
    let (audit_rt, audit_id, schema, scope, binding_patient) =
        (b.audit_rt, b.resource_id, b.schema, b.scope, b.patient_id);
    let status = state.gateway.status();
    if !status.state.is_callable() {
        return Ok(Outcome::skipped(
            match status.state {
                dmind_gateway::CapabilityState::Disabled => "disabled",
                _ => "unavailable",
            },
            hash,
        ));
    }
    audit::emit(
        &mut *tx,
        ctx,
        "ai.artifact.requested",
        &state.cell,
        json!({ "resource_type": audit_rt, "resource_id": audit_id, "input_hash": hash }),
        None,
    )
    .await?;
    let plan =
        match aigov::plan_scoped(state, tenant_id, binding_patient, scope, hash, schema).await {
            Ok(p) => p,
            Err(err) => {
                let reason = match err.code {
                    "ai_quota_exceeded" => "quota_exceeded",
                    "ai_disabled" => "disabled",
                    _ => "unavailable",
                };
                audit::emit(
                    &mut *tx,
                    ctx,
                    "ai.generation.failed",
                    &state.cell,
                    json!({ "resource_type": audit_rt, "resource_id": audit_id, "reason": reason }),
                    None,
                )
                .await?;
                return Ok(Outcome::skipped(reason, hash));
            }
        };
    let synthetic = plan.model_synthetic();
    let (raw, provider, prompt_version, usage, reused_from, execution_id): (
        Value,
        ProviderInfo,
        String,
        Option<dmind_gateway::Usage>,
        Option<Uuid>,
        Option<Uuid>,
    ) = match plan {
        aigov::ExecutionPlan::Reuse(prior) => (
            prior.output_as()?,
            ProviderInfo {
                provider: prior.route.clone(),
                model: prior.model.clone(),
                model_version: prior.model_version.clone(),
            },
            prior.prompt_version.clone(),
            prior.usage_as(),
            Some(prior.id),
            None,
        ),
        aigov::ExecutionPlan::Execute { execution_id, .. } => match call.await {
            Ok(resp) => (
                serde_json::to_value(&resp.output).map_err(ApiError::internal)?,
                resp.provider,
                resp.prompt_version,
                resp.usage,
                None,
                Some(execution_id),
            ),
            Err(err) => {
                let code = code_of(&err);
                audit::emit(
                    &mut *tx,
                    ctx,
                    "ai.generation.failed",
                    &state.cell,
                    json!({ "resource_type": audit_rt, "resource_id": audit_id, "reason": code }),
                    None,
                )
                .await?;
                return Ok(Outcome::skipped(code, hash));
            }
        },
    };
    // Server-side re-validation against the exact facts the model was given,
    // for reused outputs too: evidence, bounds, autonomy and clearance rules.
    let output = match validate(&raw) {
        Ok(o) => o,
        Err(err) => {
            audit::emit(
                &mut *tx,
                ctx,
                "ai.generation.failed",
                &state.cell,
                json!({
                    "resource_type": audit_rt,
                    "resource_id": audit_id,
                    "reason": format!("invalid_output: {err}"),
                }),
                None,
            )
            .await?;
            return Ok(Outcome::skipped("invalid_output", hash));
        }
    };
    let (cited, limitations) = cites(&output);
    let artifact_id = Uuid::now_v7();
    let sql = format!(
        "INSERT INTO ai_artifacts
         (id, tenant_id, patient_id, {}, artifact_type, autonomy_level, status,
          model, model_version, route, template, input_hash, output, output_schema,
          citations, limitations, generated_at)
         VALUES ($1,$2,$3,$4,$5,'A1',$6,$7,$8,$9,$10,$11,$12,$13,$14,$15, now())",
        b.column
    );
    sqlx::query(&sql)
        .bind(artifact_id)
        .bind(tenant_id)
        .bind(b.patient_id)
        .bind(b.resource_id)
        .bind(b.artifact_type)
        .bind(ArtifactStatus::AwaitingReview.as_str())
        .bind(&provider.model)
        .bind(&provider.model_version)
        .bind(&provider.provider)
        .bind(b.template)
        .bind(hash)
        .bind(&raw)
        .bind(b.schema)
        .bind(serde_json::to_value(&cited).map_err(ApiError::internal)?)
        .bind(serde_json::to_value(&limitations).map_err(ApiError::internal)?)
        .execute(&mut *tx)
        .await?;
    aigov::annotate(
        tx,
        artifact_id,
        &aigov::Provenance {
            scope: b.scope,
            provider: &provider,
            prompt_version: &prompt_version,
            input_refs: &b.input_refs,
            usage: usage.as_ref(),
            synthetic,
            reused_from,
        },
    )
    .await?;
    if let Some(execution_id) = execution_id {
        aigov::bind_execution(tx, execution_id, artifact_id).await?;
    }
    audit::emit(
        &mut *tx,
        ctx,
        "ai.artifact.generated",
        &state.cell,
        json!({
            "artifact_id": artifact_id,
            "artifact_type": b.artifact_type,
            "resource_type": audit_rt,
            "resource_id": audit_id,
            "template": b.template,
            "prompt_version": prompt_version,
            "model": provider.model,
            "input_hash": hash,
            "reused_from": reused_from,
            "synthetic": synthetic,
        }),
        None,
    )
    .await?;
    Ok(Outcome {
        mode: "dmind",
        artifact_id: Some(artifact_id),
        status: Some(ArtifactStatus::AwaitingReview.as_str()),
        output: Some(output),
        synthetic: Some(synthetic),
        reused: reused_from.is_some(),
        reason: None,
        input_hash: hash.to_string(),
    })
}

fn level(value: f64, warn: f64, high: f64, critical: f64) -> u8 {
    if value >= critical {
        3
    } else if value >= high {
        2
    } else if value >= warn {
        1
    } else {
        0
    }
}

fn num(v: &Value, path: &[&str]) -> f64 {
    let mut cur = v;
    for p in path {
        cur = &cur[*p];
    }
    cur.as_f64().unwrap_or(0.0)
}

fn area(
    code: &str,
    label: (&str, &str),
    es: bool,
    pressure: u8,
    facts: Vec<(String, String)>,
) -> gw::AreaFacts {
    gw::AreaFacts {
        code: code.into(),
        label: if es { label.1 } else { label.0 }.into(),
        pressure,
        facts,
    }
}

/// Persists the deterministic snapshot the brief is built from and generates
/// `care-flow-brief.v1` for it.
pub async fn flow_brief(
    state: &AppState,
    ctx: &AuthContext,
    tx: &mut PgConnection,
    facility_id: Uuid,
    language: Option<&str>,
) -> Result<
    (
        Uuid,
        Value,
        Outcome<wellos_domain::care_ops_ai::CareFlowBriefV1>,
    ),
    ApiError,
> {
    let language = lang(language);
    let es = language == "es";
    let now = Utc::now();
    let snap = ops::flow::snapshot(tx, ctx.tenant_id, facility_id, now).await?;
    let facts_hash = wellos_domain::care_ops::hash_facts(&snap);
    let snapshot_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO care_operations_snapshots (id, tenant_id, facility_id, generated_at, facts, facts_hash, created_by)
         VALUES ($1,$2,$3,$4,$5,$6,$7)",
    )
    .bind(snapshot_id)
    .bind(ctx.tenant_id)
    .bind(facility_id)
    .bind(now)
    .bind(&snap)
    .bind(&facts_hash)
    .bind(ctx.user_id)
    .execute(&mut *tx)
    .await?;
    let facility_label: String =
        sqlx::query_scalar("SELECT name FROM facilities WHERE id = $1 AND tenant_id = $2")
            .bind(facility_id)
            .bind(ctx.tenant_id)
            .fetch_optional(&mut *tx)
            .await?
            .unwrap_or_else(|| "facility".into());
    let f = |k: &str, v: String| (format!("fact:{k}"), v);
    let awaiting_triage = num(&snap, &["emergency", "awaiting_triage"]);
    let awaiting_clinician = num(&snap, &["emergency", "awaiting_clinician"]);
    let boarding = num(&snap, &["placement_queue", "pending"]);
    let boarding_wait = num(&snap, &["placement_queue", "max_wait_minutes"]);
    let occupancy = num(&snap, &["capacity", "occupancy_ratio"]);
    let available = num(&snap, &["capacity", "available"]);
    let barriers = snap["discharge"]["barriers"]
        .as_array()
        .map(|a| a.len())
        .unwrap_or(0) as f64;
    let overdue = num(&snap, &["discharge", "overdue_tasks"]);
    let theatre_conflicts = num(&snap, &["theatre", "blocking_conflicts"]);
    let theatre_missing = num(&snap, &["theatre", "missing_prerequisites"]);
    let gaps = snap["workforce"]["gaps"]
        .as_array()
        .map(|a| a.len())
        .unwrap_or(0) as f64;
    let transport_delayed = num(&snap, &["transport", "delayed"]);
    let transport_open = num(&snap, &["transport", "open"]);
    let alerts = snap["alerts"].as_array().map(|a| a.len()).unwrap_or(0) as f64;
    let escalations = snap["escalations"].as_array().map(|a| a.len()).unwrap_or(0) as f64;
    let areas = vec![
        area(
            "emergency",
            ("Emergency department", "Urgencias"),
            es,
            level(awaiting_triage + awaiting_clinician, 1.0, 5.0, 10.0).max(level(
                escalations,
                1.0,
                3.0,
                6.0,
            )),
            vec![
                f("emergency.awaiting_triage", format!("{awaiting_triage:.0}")),
                f(
                    "emergency.awaiting_clinician",
                    format!("{awaiting_clinician:.0}"),
                ),
                f("escalations.open", format!("{escalations:.0}")),
            ],
        ),
        area(
            "placement",
            ("Placement queue", "Cola de ubicación"),
            es,
            level(boarding, 1.0, 3.0, 6.0).max(level(boarding_wait, 60.0, 180.0, 360.0)),
            vec![
                f("placement_queue.pending", format!("{boarding:.0}")),
                f(
                    "placement_queue.max_wait_minutes",
                    format!("{boarding_wait:.0}"),
                ),
            ],
        ),
        area(
            "capacity",
            ("Capacity", "Capacidad"),
            es,
            level(occupancy, 0.85, 0.92, 0.97),
            vec![
                f(
                    "capacity.occupancy_ratio",
                    format!("{:.0}%", occupancy * 100.0),
                ),
                f("capacity.available", format!("{available:.0}")),
            ],
        ),
        area(
            "discharge",
            ("Discharge preparation", "Preparación del alta"),
            es,
            level(barriers + overdue, 1.0, 4.0, 8.0),
            vec![
                f("discharge.barriers", format!("{barriers:.0}")),
                f("discharge.overdue_tasks", format!("{overdue:.0}")),
            ],
        ),
        area(
            "theatre",
            ("Theatres", "Quirófanos"),
            es,
            level(theatre_conflicts + theatre_missing, 1.0, 2.0, 4.0),
            vec![
                f(
                    "theatre.blocking_conflicts",
                    format!("{theatre_conflicts:.0}"),
                ),
                f(
                    "theatre.missing_prerequisites",
                    format!("{theatre_missing:.0}"),
                ),
            ],
        ),
        area(
            "workforce",
            ("Workforce coverage", "Cobertura de plantilla"),
            es,
            level(gaps, 1.0, 2.0, 4.0),
            vec![f("workforce.gaps", format!("{gaps:.0}"))],
        ),
        area(
            "transport",
            ("Transport", "Transporte"),
            es,
            level(transport_delayed, 1.0, 2.0, 4.0),
            vec![
                f("transport.open", format!("{transport_open:.0}")),
                f("transport.delayed", format!("{transport_delayed:.0}")),
            ],
        ),
        area(
            "alerts",
            ("Unresolved alerts", "Alertas sin resolver"),
            es,
            level(alerts, 1.0, 5.0, 10.0),
            vec![f("alerts.unresolved", format!("{alerts:.0}"))],
        ),
    ];
    let req = CareFlowBriefRequest {
        template: CARE_FLOW_BRIEF_TEMPLATE.into(),
        language,
        facility_label,
        snapshot_generated_at: now,
        stale: false,
        areas,
    };
    let hash = hash_json(&req);
    let refs = req.fact_refs();
    let gateway = state.gateway.clone();
    let req_for_check = req.clone();
    let outcome = produce(
        state,
        ctx,
        tx,
        ctx.tenant_id,
        &hash,
        async move { gateway.brief_care_flow(&req).await },
        |raw| gw::parse_flow_brief(raw, &req_for_check),
        |o| (o.cited_sources.clone(), o.limitations.clone()),
        Binding {
            artifact_type: "care_flow_brief",
            template: CARE_FLOW_BRIEF_TEMPLATE,
            schema: CARE_FLOW_BRIEF_SCHEMA,
            column: "care_operations_snapshot_id",
            resource_id: snapshot_id,
            patient_id: None,
            scope: aigov::ReuseScope::CareFlowBrief {
                care_operations_snapshot_id: snapshot_id,
            },
            input_refs: refs,
            audit_rt: "care_operations_snapshot",
        },
    )
    .await?;
    Ok((snapshot_id, snap, outcome))
}

fn title(es: bool, en: &str, es_text: &str) -> String {
    if es { es_text } else { en }.to_string()
}

/// `placement-ranking.v1` over the latest feasible candidate generation of a
/// request. Stale when the request is no longer open for a decision.
pub async fn placement_ranking(
    state: &AppState,
    ctx: &AuthContext,
    tx: &mut PgConnection,
    req_row: &ops::PlacementRequestRow,
    language: Option<&str>,
) -> Result<Outcome<wellos_domain::care_ops_ai::PlacementRankingV1>, ApiError> {
    let language = lang(language);
    let es = language == "es";
    let rows = sqlx::query(
        "SELECT c.id, c.rank, c.score, c.score_factors, c.facts_hash, l.name_en, l.name_es
         FROM placement_candidates c JOIN operational_locations l ON l.id = c.location_id
         WHERE c.tenant_id = $1 AND c.request_id = $2 AND c.feasible
           AND c.generation = (SELECT max(generation) FROM placement_candidates WHERE request_id = $2)
         ORDER BY c.rank NULLS LAST, c.id LIMIT 20",
    )
    .bind(ctx.tenant_id)
    .bind(req_row.id)
    .fetch_all(&mut *tx)
    .await?;
    let generation_hash = rows
        .first()
        .map(|r| r.get::<String, _>("facts_hash"))
        .unwrap_or_default();
    let candidates: Vec<PlacementCandidateFact> = rows
        .iter()
        .map(|r| {
            let factors: Vec<wellos_domain::care_ops::PlacementScoreFactor> =
                serde_json::from_value(r.get::<Value, _>("score_factors")).unwrap_or_default();
            PlacementCandidateFact {
                candidate_ref: format!("candidate:{}", r.get::<Uuid, _>("id")),
                label: if es {
                    r.get::<String, _>("name_es")
                } else {
                    r.get::<String, _>("name_en")
                },
                score: r.get::<Option<f64>, _>("score").unwrap_or(0.0),
                factors: factors.iter().map(|f| (f.code.clone(), f.points)).collect(),
                reasons: factors
                    .iter()
                    .map(|f| f.detail.clone())
                    .filter(|d| !d.is_empty())
                    .take(5)
                    .collect(),
            }
        })
        .collect();
    let open = matches!(req_row.status.as_str(), "candidates_ready" | "held");
    let current_hash = if open {
        req_row.facts_hash.clone().unwrap_or_default()
    } else {
        format!("closed:{}", req_row.status)
    };
    let req = PlacementRankingRequest {
        template: PLACEMENT_RANKING_TEMPLATE.into(),
        language,
        matcher_version: req_row
            .matcher_version
            .clone()
            .unwrap_or_else(|| PLACEMENT_MATCHER_VERSION.into()),
        facts_hash: generation_hash,
        current_facts_hash: current_hash,
        request_facts: vec![
            ("request:kind".into(), req_row.kind.clone()),
            ("request:priority".into(), req_row.priority.clone()),
            ("request:status".into(), req_row.status.clone()),
            (
                "request:requirements".into(),
                serde_json::to_string(&req_row.requirements).unwrap_or_default(),
            ),
        ],
        candidates,
    };
    req.check().map_err(|e| match e {
        GatewayError::PolicyDenied(m) if m.contains("stale") => ApiError::conflict(
            "facts_stale",
            "the candidate list no longer matches the request; regenerate candidates",
        ),
        GatewayError::PolicyDenied(m) => ApiError::conflict("ranking_unavailable", m),
        other => ApiError::internal(other.to_string()),
    })?;
    let hash = hash_json(&req);
    let refs = req.fact_refs();
    let gateway = state.gateway.clone();
    let check = req.clone();
    let outcome = produce(
        state,
        ctx,
        tx,
        ctx.tenant_id,
        &hash,
        async move { gateway.rank_placements(&req).await },
        |raw| gw::parse_placement_ranking(raw, &check),
        |o| (o.cited_sources.clone(), o.limitations.clone()),
        Binding {
            artifact_type: "placement_ranking",
            template: PLACEMENT_RANKING_TEMPLATE,
            schema: PLACEMENT_RANKING_SCHEMA,
            column: "placement_request_id",
            resource_id: req_row.id,
            patient_id: Some(req_row.patient_id),
            scope: aigov::ReuseScope::PlacementRanking {
                placement_request_id: req_row.id,
            },
            input_refs: refs,
            audit_rt: "placement_request",
        },
    )
    .await?;
    if let Some(id) = outcome.artifact_id {
        sqlx::query("UPDATE placement_requests SET ranking_artifact_id = $2 WHERE id = $1 AND tenant_id = $3")
            .bind(req_row.id)
            .bind(id)
            .bind(ctx.tenant_id)
            .execute(&mut *tx)
            .await?;
    }
    Ok(outcome)
}

fn episode_facts(ep: &ops::EpisodeRow, es: bool) -> Vec<(String, String)> {
    let mut facts = vec![
        ("episode:status".into(), ep.status.clone()),
        ("episode:type".into(), ep.episode_type_code.clone()),
        ("episode:priority".into(), ep.priority.clone()),
    ];
    if let Some(p) = ep.current_placement_id {
        facts.push(("placement:current".into(), format!("placement {p}")));
    }
    if let Some(t) = ep.expected_end_at {
        facts.push(("episode:expected_discharge".into(), t.to_rfc3339()));
    }
    if let Some(arr) = ep.barriers.as_array() {
        for (i, b) in arr.iter().take(10).enumerate() {
            let text = b
                .get(if es { "text_es" } else { "text_en" })
                .or_else(|| b.get("text"))
                .or_else(|| b.get("category"))
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| b.to_string());
            facts.push((format!("barrier:{i}"), text));
        }
    }
    facts
}

fn task_fact(t: &ops::TaskRow, es: bool, now: chrono::DateTime<Utc>) -> TaskFact {
    TaskFact {
        task_ref: format!("task:{}", t.id),
        title: if es {
            t.title_es.clone()
        } else {
            t.title_en.clone()
        },
        status: t.status.clone(),
        barrier_category: t.barrier_category_code.clone(),
        due_at: t.due_at,
        overdue: t.completed_at.is_none() && t.due_at.is_some_and(|d| d < now),
    }
}

/// `discharge-coordination.v1`: operational barriers and steps for one
/// episode; never a clinical fitness statement.
pub async fn discharge_coordination(
    state: &AppState,
    ctx: &AuthContext,
    tx: &mut PgConnection,
    ep: &ops::EpisodeRow,
    language: Option<&str>,
) -> Result<Outcome<wellos_domain::care_ops_ai::DischargeCoordinationV1>, ApiError> {
    let language = lang(language);
    let es = language == "es";
    let now = Utc::now();
    let tasks = ops::list_tasks(tx, ctx.tenant_id, Some(ep.id), None, true, 50).await?;
    let req = DischargeCoordinationRequest {
        template: DISCHARGE_COORDINATION_TEMPLATE.into(),
        language,
        stale: matches!(ep.status.as_str(), "completed" | "cancelled" | "error"),
        episode_facts: episode_facts(ep, es),
        tasks: tasks.iter().map(|t| task_fact(t, es, now)).collect(),
    };
    req.check().map_err(|e| match e {
        GatewayError::PolicyDenied(m) if m.contains("stale") => ApiError::conflict(
            "episode_closed",
            "the episode is closed; coordination is not applicable",
        ),
        GatewayError::PolicyDenied(m) => ApiError::conflict("coordination_unavailable", m),
        other => ApiError::internal(other.to_string()),
    })?;
    let hash = hash_json(&req);
    let refs = req.fact_refs();
    let gateway = state.gateway.clone();
    let check = req.clone();
    produce(
        state,
        ctx,
        tx,
        ctx.tenant_id,
        &hash,
        async move { gateway.coordinate_discharge(&req).await },
        |raw| gw::parse_discharge_coordination(raw, &check),
        |o| (o.cited_sources.clone(), o.limitations.clone()),
        Binding {
            artifact_type: "discharge_coordination",
            template: DISCHARGE_COORDINATION_TEMPLATE,
            schema: DISCHARGE_COORDINATION_SCHEMA,
            column: "care_episode_id",
            resource_id: ep.id,
            patient_id: Some(ep.patient_id),
            scope: aigov::ReuseScope::DischargeCoordination {
                care_episode_id: ep.id,
            },
            input_refs: refs,
            audit_rt: "care_episode",
        },
    )
    .await
}

/// `theatre-plan-explanation.v1` for one session: repeats the deterministic
/// readiness/conflict engine results without resolving or waiving any.
pub async fn theatre_plan(
    state: &AppState,
    ctx: &AuthContext,
    tx: &mut PgConnection,
    session_id: Uuid,
    language: Option<&str>,
) -> Result<
    (
        Uuid,
        Outcome<wellos_domain::care_ops_ai::TheatrePlanExplanationV1>,
    ),
    ApiError,
> {
    let language = lang(language);
    let es = language == "es";
    let s = sqlx::query(
        "SELECT s.facility_id, s.starts_at, s.ends_at, s.status, s.turnaround_minutes, l.name_en, l.name_es
         FROM theatre_sessions s JOIN operational_locations l ON l.id = s.theatre_location_id
         WHERE s.tenant_id = $1 AND s.id = $2",
    )
    .bind(ctx.tenant_id)
    .bind(session_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(ApiError::not_found)?;
    let facility_id: Uuid = s.get("facility_id");
    let theatre_name: String = if es {
        s.get("name_es")
    } else {
        s.get("name_en")
    };
    let status: String = s.get("status");
    let all = theatre::list_cases(tx, ctx.tenant_id, Some(&[facility_id]), None, true, 200).await?;
    let mut cases = Vec::new();
    for c in all
        .iter()
        .filter(|c| c.session_id == Some(session_id))
        .take(20)
    {
        let check = theatre::readiness(tx, c, None, None).await?;
        let (feasible, conflicts, missing) = match check {
            Some(ch) => {
                let missing: Vec<String> = ch
                    .conflicts
                    .iter()
                    .filter(|x| x.code == "missing_prerequisite")
                    .map(|x| x.detail.clone())
                    .collect();
                (
                    ch.feasible,
                    ch.conflicts
                        .iter()
                        .filter(|x| x.blocking)
                        .map(|x| (x.code.clone(), x.detail.clone()))
                        .collect(),
                    missing,
                )
            }
            None => (
                false,
                vec![(
                    "unscheduled".into(),
                    title(es, "not scheduled", "sin programar"),
                )],
                vec![],
            ),
        };
        cases.push(TheatreCaseFact {
            case_ref: format!("case:{}", c.id),
            label: format!("{} · {}", c.procedure_code, c.responsible_service_code),
            status: c.status.clone(),
            feasible,
            conflicts,
            missing_prerequisites: missing,
        });
    }
    let req = TheatrePlanExplanationRequest {
        template: THEATRE_PLAN_EXPLANATION_TEMPLATE.into(),
        language,
        session_label: format!(
            "{theatre_name} {}–{}",
            s.get::<chrono::DateTime<Utc>, _>("starts_at")
                .format("%Y-%m-%d %H:%M"),
            s.get::<chrono::DateTime<Utc>, _>("ends_at")
                .format("%H:%M UTC")
        ),
        stale: status != "planned",
        session_facts: vec![
            ("session:status".into(), status.clone()),
            ("session:theatre".into(), theatre_name.clone()),
            (
                "session:turnaround_minutes".into(),
                s.get::<i32, _>("turnaround_minutes").to_string(),
            ),
        ],
        cases,
    };
    req.check().map_err(|e| match e {
        GatewayError::PolicyDenied(m) if m.contains("stale") => {
            ApiError::conflict("session_closed", "the session is no longer planned")
        }
        GatewayError::PolicyDenied(m) => ApiError::conflict("explanation_unavailable", m),
        other => ApiError::internal(other.to_string()),
    })?;
    let hash = hash_json(&req);
    let refs = req.fact_refs();
    let gateway = state.gateway.clone();
    let check = req.clone();
    let outcome = produce(
        state,
        ctx,
        tx,
        ctx.tenant_id,
        &hash,
        async move { gateway.explain_theatre_plan(&req).await },
        |raw| gw::parse_theatre_plan(raw, &check),
        |o| (o.cited_sources.clone(), o.limitations.clone()),
        Binding {
            artifact_type: "theatre_plan_explanation",
            template: THEATRE_PLAN_EXPLANATION_TEMPLATE,
            schema: THEATRE_PLAN_EXPLANATION_SCHEMA,
            column: "theatre_session_id",
            resource_id: session_id,
            patient_id: None,
            scope: aigov::ReuseScope::TheatrePlanExplanation {
                theatre_session_id: session_id,
            },
            input_refs: refs,
            audit_rt: "theatre_session",
        },
    )
    .await?;
    Ok((facility_id, outcome))
}

/// `operational-forecast-explanation.v1` over a persisted deterministic forecast.
pub async fn forecast_explanation(
    state: &AppState,
    ctx: &AuthContext,
    tx: &mut PgConnection,
    f: &forecast::ForecastRow,
    language: Option<&str>,
) -> Result<Outcome<wellos_domain::care_ops_ai::OperationalForecastExplanationV1>, ApiError> {
    let language = lang(language);
    let facility_name: String =
        sqlx::query_scalar("SELECT name FROM facilities WHERE id = $1 AND tenant_id = $2")
            .bind(f.facility_id)
            .bind(ctx.tenant_id)
            .fetch_optional(&mut *tx)
            .await?
            .unwrap_or_else(|| "facility".into());
    let parsed: OpsForecast =
        serde_json::from_value(f.output.clone()).map_err(ApiError::internal)?;
    let req = OperationalForecastExplanationRequest {
        template: OPERATIONAL_FORECAST_EXPLANATION_TEMPLATE.into(),
        language,
        scope_label: format!("{facility_name} · {}", f.horizon),
        forecast: parsed,
    };
    req.check()
        .map_err(|e| ApiError::conflict("explanation_unavailable", e.to_string()))?;
    let hash = hash_json(&req);
    let refs = req.fact_refs();
    let gateway = state.gateway.clone();
    let check = req.clone();
    let outcome = produce(
        state,
        ctx,
        tx,
        ctx.tenant_id,
        &hash,
        async move { gateway.explain_operational_forecast(&req).await },
        |raw| gw::parse_forecast_explanation(raw, &check),
        |o| (o.cited_sources.clone(), o.limitations.clone()),
        Binding {
            artifact_type: "operational_forecast_explanation",
            template: OPERATIONAL_FORECAST_EXPLANATION_TEMPLATE,
            schema: OPERATIONAL_FORECAST_EXPLANATION_SCHEMA,
            column: "care_operations_forecast_id",
            resource_id: f.id,
            patient_id: None,
            scope: aigov::ReuseScope::OperationalForecastExplanation {
                care_operations_forecast_id: f.id,
            },
            input_refs: refs,
            audit_rt: "care_operations_forecast",
        },
    )
    .await?;
    if let Some(id) = outcome.artifact_id {
        sqlx::query(
            "UPDATE care_operations_forecasts SET explanation_artifact_id = $2 WHERE id = $1 AND tenant_id = $3",
        )
        .bind(f.id)
        .bind(id)
        .bind(ctx.tenant_id)
        .execute(&mut *tx)
        .await?;
    }
    Ok(outcome)
}

/// `shift-handoff-draft.v1`: a durable `care_handoffs` draft (origin
/// `dmind_draft`) that becomes a handoff only when a professional accepts it
/// through the handoff decision route.
pub async fn handoff_draft(
    state: &AppState,
    ctx: &AuthContext,
    tx: &mut PgConnection,
    ep: &ops::EpisodeRow,
    shift_label: &str,
    language: Option<&str>,
) -> Result<
    (
        Outcome<wellos_domain::care_ops_ai::ShiftHandoffDraftV1>,
        Option<Value>,
    ),
    ApiError,
> {
    let language = lang(language);
    let es = language == "es";
    let now = Utc::now();
    let mut facts = episode_facts(ep, es);
    let tasks = ops::list_tasks(tx, ctx.tenant_id, Some(ep.id), None, true, 30).await?;
    for t in &tasks {
        let tf = task_fact(t, es, now);
        facts.push((
            tf.task_ref,
            format!(
                "{} ({}{})",
                tf.title,
                tf.status,
                if tf.overdue { ", overdue" } else { "" }
            ),
        ));
    }
    let escalations = sqlx::query(
        "SELECT id, kind, status, due_at FROM care_escalations
         WHERE tenant_id = $1 AND episode_id = $2 AND status IN ('armed', 'escalated')
         ORDER BY due_at LIMIT 10",
    )
    .bind(ctx.tenant_id)
    .bind(ep.id)
    .fetch_all(&mut *tx)
    .await?;
    for e in &escalations {
        facts.push((
            format!("escalation:{}", e.get::<Uuid, _>("id")),
            format!(
                "{} {} due {}",
                e.get::<String, _>("kind"),
                e.get::<String, _>("status"),
                e.get::<chrono::DateTime<Utc>, _>("due_at")
                    .format("%H:%M UTC")
            ),
        ));
    }
    let req = ShiftHandoffDraftRequest {
        template: SHIFT_HANDOFF_DRAFT_TEMPLATE.into(),
        language: language.clone(),
        shift_label: shift_label.chars().take(80).collect(),
        stale: matches!(ep.status.as_str(), "completed" | "cancelled" | "error"),
        facts,
    };
    req.check().map_err(|e| match e {
        GatewayError::PolicyDenied(m) if m.contains("stale") => {
            ApiError::conflict("episode_closed", "the episode is closed; no handoff draft")
        }
        GatewayError::PolicyDenied(m) => ApiError::bad_request("validation_failed", m),
        other => ApiError::internal(other.to_string()),
    })?;
    let hash = hash_json(&req);
    let refs = req.fact_refs();
    let source_facts = serde_json::to_value(&req.facts).map_err(ApiError::internal)?;
    let gateway = state.gateway.clone();
    let check = req.clone();
    let outcome = produce(
        state,
        ctx,
        tx,
        ctx.tenant_id,
        &hash,
        async move { gateway.draft_shift_handoff(&req).await },
        |raw| gw::parse_handoff_draft(raw, &check),
        |o| (o.cited_sources.clone(), o.limitations.clone()),
        Binding {
            artifact_type: "shift_handoff_draft",
            template: SHIFT_HANDOFF_DRAFT_TEMPLATE,
            schema: SHIFT_HANDOFF_DRAFT_SCHEMA,
            column: "care_episode_id",
            resource_id: ep.id,
            patient_id: Some(ep.patient_id),
            scope: aigov::ReuseScope::ShiftHandoffDraft {
                care_episode_id: ep.id,
            },
            input_refs: refs,
            audit_rt: "care_episode",
        },
    )
    .await?;
    let handoff = match (&outcome.artifact_id, &outcome.output) {
        (Some(artifact_id), Some(draft)) => {
            let content = serde_json::to_value(draft).map_err(ApiError::internal)?;
            let row = ops::create_handoff_draft(
                tx,
                ctx,
                ep,
                &shift_label.chars().take(80).collect::<String>(),
                &language,
                &content,
                &source_facts,
                *artifact_id,
            )
            .await?;
            Some(json!(row))
        }
        _ => None,
    };
    Ok((outcome, handoff))
}

pub struct ReviewTarget {
    pub artifact_type: String,
    pub facility_id: Uuid,
    pub patient_id: Option<Uuid>,
    pub status: ArtifactStatus,
}

/// Loads a care-operations artifact and resolves the facility it belongs to
/// through its resource binding (anti-probing: unknown or foreign → not found).
pub async fn load_review_target(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    id: Uuid,
) -> Result<ReviewTarget, ApiError> {
    let row = sqlx::query(
        "SELECT a.artifact_type, a.status, a.patient_id,
                COALESCE(e.facility_id, pr.facility_id, ts.facility_id, f.facility_id, s.facility_id) AS facility_id
         FROM ai_artifacts a
         LEFT JOIN care_episodes e ON e.id = a.care_episode_id
         LEFT JOIN placement_requests pr ON pr.id = a.placement_request_id
         LEFT JOIN theatre_sessions ts ON ts.id = a.theatre_session_id
         LEFT JOIN care_operations_forecasts f ON f.id = a.care_operations_forecast_id
         LEFT JOIN care_operations_snapshots s ON s.id = a.care_operations_snapshot_id
         WHERE a.id = $1 AND a.tenant_id = $2 AND a.artifact_type = ANY($3)",
    )
    .bind(id)
    .bind(tenant_id)
    .bind(CARE_OPS_ARTIFACT_TYPES.to_vec())
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(ApiError::not_found)?;
    let facility_id: Option<Uuid> = row.get("facility_id");
    Ok(ReviewTarget {
        artifact_type: row.get("artifact_type"),
        facility_id: facility_id.ok_or_else(ApiError::not_found)?,
        patient_id: row.get("patient_id"),
        status: ArtifactStatus::parse(row.get::<String, _>("status").as_str())
            .ok_or_else(|| ApiError::internal("invalid artifact status"))?,
    })
}

/// Human acceptance/rejection: an atomic conditional status transition that a
/// concurrent review or supersession loses instead of overwriting.
pub async fn review(
    tx: &mut PgConnection,
    ctx: &AuthContext,
    cell: &str,
    id: Uuid,
    target: &ReviewTarget,
    decision: ReviewDecision,
    note: Option<&str>,
) -> Result<ArtifactStatus, ApiError> {
    let next = target
        .status
        .review(decision)
        .map_err(|e| ApiError::conflict("invalid_artifact_state", e.to_string()))?;
    let updated = sqlx::query(
        "UPDATE ai_artifacts SET status=$1, reviewer_id=$2, review_decision=$3,
         review_note=$4, reviewed_at=now() WHERE id=$5 AND tenant_id=$6 AND status=$7",
    )
    .bind(next.as_str())
    .bind(ctx.user_id)
    .bind(match decision {
        ReviewDecision::Approved => "approved",
        ReviewDecision::Rejected => "rejected",
    })
    .bind(note)
    .bind(id)
    .bind(ctx.tenant_id)
    .bind(target.status.as_str())
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(ApiError::conflict(
            "artifact_stale",
            "the artifact was reviewed or superseded concurrently",
        ));
    }
    audit::emit(
        &mut *tx,
        ctx,
        "ai.artifact.reviewed",
        cell,
        json!({
            "artifact_id": id,
            "artifact_type": target.artifact_type,
            "decision": next.as_str(),
            "facility_id": target.facility_id,
        }),
        None,
    )
    .await?;
    Ok(next)
}
