//! dMind care-operations operations: request contracts, the deterministic
//! offline implementations used by the fixture provider and the shared
//! parse-and-validate step every provider's raw JSON passes through.
//!
//! - `care-flow-brief.v1` explains a persisted command-center snapshot;
//! - `placement-ranking.v1` reorders/explains matcher-feasible candidates;
//! - `discharge-coordination.v1` explains operational discharge barriers;
//! - `theatre-plan-explanation.v1` explains a session plan's readiness;
//! - `operational-forecast-explanation.v1` explains a deterministic forecast;
//! - `shift-handoff-draft.v1` drafts a structured handoff for human acceptance.
//!
//! Every request carries the exact facts the deterministic layer produced and
//! a staleness verdict computed by the server; a stale request is refused
//! before any provider call. No operation can mutate anything.

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use wellos_domain::ai::{Confidence, ProviderInfo};
use wellos_domain::care_ops::{OpsForecast, PLACEMENT_MATCHER_VERSION};
use wellos_domain::care_ops_ai::{
    CareFlowBriefV1, DischargeBarrier, DischargeCoordinationV1, EvidenceItem,
    ForecastPressurePoint, OperationalForecastExplanationV1, OpsRecommendation, PlacementRankingV1,
    RankedPlacement, ShiftHandoffDraftV1, SuggestedStep, TheatreCaseNote, TheatreConflictNote,
    TheatrePlanExplanationV1, CARE_FLOW_BRIEF_SCHEMA, DISCHARGE_COORDINATION_SCHEMA,
    OPERATIONAL_FORECAST_EXPLANATION_SCHEMA, PLACEMENT_RANKING_SCHEMA, SHIFT_HANDOFF_DRAFT_SCHEMA,
    THEATRE_PLAN_EXPLANATION_SCHEMA,
};

use crate::{hash_json, GatewayError, Usage};

pub const CARE_FLOW_BRIEF_TEMPLATE: &str = "care-flow-brief@1.0.0";
pub const PLACEMENT_RANKING_TEMPLATE: &str = "placement-ranking@1.0.0";
pub const DISCHARGE_COORDINATION_TEMPLATE: &str = "discharge-coordination@1.0.0";
pub const THEATRE_PLAN_EXPLANATION_TEMPLATE: &str = "theatre-plan-explanation@1.0.0";
pub const OPERATIONAL_FORECAST_EXPLANATION_TEMPLATE: &str =
    "operational-forecast-explanation@1.0.0";
pub const SHIFT_HANDOFF_DRAFT_TEMPLATE: &str = "shift-handoff-draft@1.0.0";

pub const CARE_FLOW_BRIEF_DETERMINISTIC_PROMPT_VERSION: &str = "care-flow-brief-deterministic.v1";
pub const PLACEMENT_RANKING_DETERMINISTIC_PROMPT_VERSION: &str =
    "placement-ranking-deterministic.v1";
pub const DISCHARGE_COORDINATION_DETERMINISTIC_PROMPT_VERSION: &str =
    "discharge-coordination-deterministic.v1";
pub const THEATRE_PLAN_EXPLANATION_DETERMINISTIC_PROMPT_VERSION: &str =
    "theatre-plan-explanation-deterministic.v1";
pub const OPERATIONAL_FORECAST_EXPLANATION_DETERMINISTIC_PROMPT_VERSION: &str =
    "operational-forecast-explanation-deterministic.v1";
pub const SHIFT_HANDOFF_DRAFT_DETERMINISTIC_PROMPT_VERSION: &str =
    "shift-handoff-draft-deterministic.v1";

/// Most facts one request may carry; the server truncates deterministically.
pub const MAX_FACTS: usize = 120;
pub const MAX_CANDIDATES: usize = 20;
pub const MAX_CASES: usize = 40;

/// Provider-independent response envelope shared by the six operations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CareOpsResponse<T> {
    pub output: T,
    pub provider: ProviderInfo,
    pub prompt_version: String,
    pub input_hash: String,
    pub usage: Option<Usage>,
}

fn check_template(got: &str, want: &str) -> Result<(), GatewayError> {
    if got != want {
        return Err(GatewayError::PolicyDenied(format!(
            "unsupported template {got}"
        )));
    }
    Ok(())
}

/// Removes reference-like tokens (`scope:id`) that are not part of the
/// supplied evidence from echoed fact text, so a deterministic fallback never
/// repeats an identifier the request did not vouch for.
fn scrub_refs(text: &str, allowed: &[String]) -> String {
    text.split_whitespace()
        .filter(|tok| {
            let core = tok.trim_matches(|c: char| !c.is_alphanumeric());
            !core.contains(':') || allowed.iter().any(|a| a == core)
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn missing(language: &str, what: &str) -> String {
    if language.starts_with("es") {
        format!("Falta evidencia: {what}.")
    } else {
        format!("Missing evidence: {what}.")
    }
}

fn check_fresh(stale: bool, what: &str) -> Result<(), GatewayError> {
    if stale {
        return Err(GatewayError::PolicyDenied(format!(
            "{what} is stale: regenerate the deterministic facts first"
        )));
    }
    Ok(())
}

fn check_facts(facts: &[(String, String)], what: &str) -> Result<(), GatewayError> {
    if facts.len() > MAX_FACTS {
        return Err(GatewayError::PolicyDenied(format!(
            "{what} carries more than {MAX_FACTS} facts"
        )));
    }
    Ok(())
}

fn fact_text<'a>(facts: &'a [(String, String)], r: &str) -> Option<&'a str> {
    facts.iter().find(|(k, _)| k == r).map(|(_, v)| v.as_str())
}

fn es(language: &str) -> bool {
    language.starts_with("es")
}

fn set_schema(raw: &mut Value, schema: &str) {
    if let Some(obj) = raw.as_object_mut() {
        obj.insert("schema_version".into(), Value::from(schema));
    }
}

fn force_true(raw: &mut Value, list_key: &str, flag: &str) {
    if let Some(list) = raw.get_mut(list_key).and_then(Value::as_array_mut) {
        for r in list.iter_mut() {
            if let Some(r) = r.as_object_mut() {
                r.insert(flag.into(), Value::from(true));
            }
        }
    }
}

fn invalid(schema: &str, e: impl std::fmt::Display) -> GatewayError {
    GatewayError::InvalidOutput(format!("{schema} output does not match schema: {e}"))
}

fn envelope<T>(
    output: T,
    provider: ProviderInfo,
    prompt_version: &str,
    hash: String,
) -> CareOpsResponse<T> {
    CareOpsResponse {
        output,
        provider,
        prompt_version: prompt_version.into(),
        input_hash: hash,
        usage: None,
    }
}

fn limitation(language: &str) -> String {
    if es(language) {
        "Resumen operativo asistido: no sustituye la valoración clínica ni la decisión humana."
            .into()
    } else {
        "Assistive operational summary: not a clinical assessment and not a decision.".into()
    }
}

// ---------------------------------------------------------------------------
// care-flow-brief.v1
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AreaFacts {
    /// Area code: emergency, placement, discharge, theatre, workforce,
    /// transport, alerts, capacity.
    pub code: String,
    pub label: String,
    /// Deterministic value of the area pressure (0 none … 3 critical).
    pub pressure: u8,
    pub facts: Vec<(String, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CareFlowBriefRequest {
    pub template: String,
    pub language: String,
    pub facility_label: String,
    pub snapshot_generated_at: DateTime<Utc>,
    /// Set by the server when the snapshot no longer matches current facts.
    pub stale: bool,
    pub areas: Vec<AreaFacts>,
}

impl CareFlowBriefRequest {
    pub fn areas(&self) -> Vec<String> {
        self.areas.iter().map(|a| a.code.clone()).collect()
    }
    pub fn fact_refs(&self) -> Vec<String> {
        let mut out = vec!["snapshot:summary".to_string()];
        for a in &self.areas {
            out.push(format!("area:{}", a.code));
            out.extend(a.facts.iter().map(|(r, _)| r.clone()));
        }
        out
    }
    pub fn check(&self) -> Result<(), GatewayError> {
        check_template(&self.template, CARE_FLOW_BRIEF_TEMPLATE)?;
        check_fresh(self.stale, "command-center snapshot")?;
        let total: usize = self.areas.iter().map(|a| a.facts.len()).sum();
        if total > MAX_FACTS || self.areas.len() > 12 {
            return Err(GatewayError::PolicyDenied("snapshot is oversized".into()));
        }
        Ok(())
    }
}

pub fn parse_flow_brief(
    raw: &Value,
    req: &CareFlowBriefRequest,
) -> Result<CareFlowBriefV1, GatewayError> {
    let mut raw = raw.clone();
    set_schema(&mut raw, CARE_FLOW_BRIEF_SCHEMA);
    let parsed: CareFlowBriefV1 =
        serde_json::from_value(raw).map_err(|e| invalid(CARE_FLOW_BRIEF_SCHEMA, e))?;
    parsed
        .validate(&req.areas(), &req.fact_refs())
        .map_err(GatewayError::InvalidOutput)?;
    Ok(parsed)
}

pub fn deterministic_flow_brief(
    req: &CareFlowBriefRequest,
    provider: ProviderInfo,
) -> Result<CareOpsResponse<CareFlowBriefV1>, GatewayError> {
    req.check()?;
    let es = es(&req.language);
    let mut pressured: Vec<&AreaFacts> = req.areas.iter().filter(|a| a.pressure >= 2).collect();
    pressured.sort_by(|a, b| b.pressure.cmp(&a.pressure));
    let summary = if es {
        format!(
            "{}: {} área(s) bajo presión de {} evaluadas a las {}.",
            req.facility_label,
            pressured.len(),
            req.areas.len(),
            req.snapshot_generated_at.format("%H:%M UTC")
        )
    } else {
        format!(
            "{}: {} of {} areas under pressure as of {}.",
            req.facility_label,
            pressured.len(),
            req.areas.len(),
            req.snapshot_generated_at.format("%H:%M UTC")
        )
    };
    let allowed = req.fact_refs();
    let pressures = pressured
        .iter()
        .take(8)
        .map(|a| {
            let detail = a
                .facts
                .iter()
                .take(3)
                .map(|(_, v)| scrub_refs(v, &allowed))
                .collect::<Vec<_>>()
                .join("; ");
            let mut cited = vec![format!("area:{}", a.code)];
            cited.extend(a.facts.iter().take(3).map(|(r, _)| r.clone()));
            wellos_domain::care_ops_ai::FlowPressure {
                area: a.code.clone(),
                explanation: if detail.is_empty() {
                    a.label.clone()
                } else {
                    format!("{}: {detail}", a.label)
                },
                cited_sources: cited,
            }
        })
        .collect::<Vec<_>>();
    let human_decisions = pressured
        .iter()
        .take(8)
        .map(|a| EvidenceItem {
            text: if es {
                format!("Revisar {} y decidir las acciones necesarias.", a.label)
            } else {
                format!("Review {} and decide the required actions.", a.label)
            },
            cited_sources: vec![format!("area:{}", a.code)],
        })
        .collect();
    let output = CareFlowBriefV1 {
        schema_version: CARE_FLOW_BRIEF_SCHEMA.into(),
        missing_information: {
            let present: Vec<&str> = req.areas.iter().map(|a| a.code.as_str()).collect();
            [
                "emergency",
                "placement",
                "discharge",
                "theatre",
                "workforce",
                "transport",
            ]
            .iter()
            .filter(|c| !present.contains(c))
            .map(|c| missing(&req.language, &format!("area:{c}")))
            .collect()
        },
        summary,
        pressures,
        human_decisions,
        cited_sources: vec!["snapshot:summary".into()],
        confidence: if req.areas.is_empty() {
            Confidence::Low
        } else {
            Confidence::Medium
        },
        limitations: vec![limitation(&req.language)],
    };
    let raw =
        serde_json::to_value(&output).map_err(|e| GatewayError::InvalidOutput(e.to_string()))?;
    let output = parse_flow_brief(&raw, req)?;
    Ok(envelope(
        output,
        provider,
        CARE_FLOW_BRIEF_DETERMINISTIC_PROMPT_VERSION,
        hash_json(req),
    ))
}

// ---------------------------------------------------------------------------
// placement-ranking.v1
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlacementCandidateFact {
    /// `candidate:<location_id>`
    pub candidate_ref: String,
    pub label: String,
    pub score: f64,
    /// (factor code, contribution)
    pub factors: Vec<(String, f64)>,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlacementRankingRequest {
    pub template: String,
    pub language: String,
    pub matcher_version: String,
    /// Hash of the facts the candidates were generated from …
    pub facts_hash: String,
    /// … and of the facts as they are now; a mismatch is a stale request.
    pub current_facts_hash: String,
    pub request_facts: Vec<(String, String)>,
    pub candidates: Vec<PlacementCandidateFact>,
}

impl PlacementRankingRequest {
    pub fn candidate_refs(&self) -> Vec<String> {
        self.candidates
            .iter()
            .map(|c| c.candidate_ref.clone())
            .collect()
    }
    pub fn fact_refs(&self) -> Vec<String> {
        let mut out: Vec<String> = self.request_facts.iter().map(|(r, _)| r.clone()).collect();
        out.extend(self.candidate_refs());
        out
    }
    pub fn check(&self) -> Result<(), GatewayError> {
        check_template(&self.template, PLACEMENT_RANKING_TEMPLATE)?;
        if self.matcher_version != PLACEMENT_MATCHER_VERSION {
            return Err(GatewayError::PolicyDenied(format!(
                "unsupported matcher version {}",
                self.matcher_version
            )));
        }
        check_fresh(
            self.facts_hash != self.current_facts_hash,
            "placement candidate set",
        )?;
        check_facts(&self.request_facts, "placement request")?;
        if self.candidates.is_empty() {
            return Err(GatewayError::PolicyDenied(
                "no feasible candidates to rank".into(),
            ));
        }
        if self.candidates.len() > MAX_CANDIDATES {
            return Err(GatewayError::PolicyDenied("too many candidates".into()));
        }
        Ok(())
    }
}

pub fn parse_placement_ranking(
    raw: &Value,
    req: &PlacementRankingRequest,
) -> Result<PlacementRankingV1, GatewayError> {
    let mut raw = raw.clone();
    set_schema(&mut raw, PLACEMENT_RANKING_SCHEMA);
    let parsed: PlacementRankingV1 =
        serde_json::from_value(raw).map_err(|e| invalid(PLACEMENT_RANKING_SCHEMA, e))?;
    parsed
        .validate(&req.candidate_refs(), &req.fact_refs())
        .map_err(GatewayError::InvalidOutput)?;
    Ok(parsed)
}

pub fn deterministic_placement_ranking(
    req: &PlacementRankingRequest,
    provider: ProviderInfo,
) -> Result<CareOpsResponse<PlacementRankingV1>, GatewayError> {
    req.check()?;
    let es = es(&req.language);
    let mut ordered: Vec<&PlacementCandidateFact> = req.candidates.iter().collect();
    ordered.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let ranked = ordered
        .iter()
        .map(|c| {
            let top = c
                .factors
                .iter()
                .filter(|(_, v)| *v > 0.0)
                .map(|(k, _)| k.replace('_', " "))
                .take(3)
                .collect::<Vec<_>>()
                .join(", ");
            RankedPlacement {
                candidate_ref: c.candidate_ref.clone(),
                rationale: if es {
                    format!(
                        "{} (puntuación {:.0}): {}",
                        c.label,
                        c.score,
                        if top.is_empty() {
                            "cumple los requisitos".to_string()
                        } else {
                            top
                        }
                    )
                } else {
                    format!(
                        "{} (score {:.0}): {}",
                        c.label,
                        c.score,
                        if top.is_empty() {
                            "meets the requirements".to_string()
                        } else {
                            top
                        }
                    )
                },
                cited_sources: vec![c.candidate_ref.clone()],
            }
        })
        .collect::<Vec<_>>();
    let summary = if es {
        format!(
            "{} posición(es) viables ordenadas por la puntuación determinista {}; la confirmación es humana.",
            ranked.len(),
            req.matcher_version
        )
    } else {
        format!(
            "{} feasible position(s) in deterministic {} score order; confirmation is a human decision.",
            ranked.len(),
            req.matcher_version
        )
    };
    let mut cited: Vec<String> = req
        .request_facts
        .iter()
        .map(|(r, _)| r.clone())
        .take(5)
        .collect();
    if cited.is_empty() {
        cited.push(ranked[0].candidate_ref.clone());
    }
    let output = PlacementRankingV1 {
        schema_version: PLACEMENT_RANKING_SCHEMA.into(),
        missing_information: {
            let mut m = Vec::new();
            if req.request_facts.len() < 2 {
                m.push(missing(&req.language, "request:requirements"));
            }
            m.extend(
                req.candidates
                    .iter()
                    .filter(|c| c.factors.is_empty())
                    .map(|c| missing(&req.language, &format!("{}:factors", c.candidate_ref))),
            );
            m
        },
        summary,
        ranked,
        cited_sources: cited,
        confidence: Confidence::Medium,
        limitations: vec![limitation(&req.language)],
    };
    let raw =
        serde_json::to_value(&output).map_err(|e| GatewayError::InvalidOutput(e.to_string()))?;
    let output = parse_placement_ranking(&raw, req)?;
    Ok(envelope(
        output,
        provider,
        PLACEMENT_RANKING_DETERMINISTIC_PROMPT_VERSION,
        hash_json(req),
    ))
}

// ---------------------------------------------------------------------------
// discharge-coordination.v1
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskFact {
    /// `task:<id>`
    pub task_ref: String,
    pub title: String,
    pub status: String,
    pub barrier_category: Option<String>,
    pub due_at: Option<DateTime<Utc>>,
    pub overdue: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DischargeCoordinationRequest {
    pub template: String,
    pub language: String,
    pub stale: bool,
    pub episode_facts: Vec<(String, String)>,
    pub tasks: Vec<TaskFact>,
}

impl DischargeCoordinationRequest {
    pub fn task_refs(&self) -> Vec<String> {
        self.tasks.iter().map(|t| t.task_ref.clone()).collect()
    }
    pub fn fact_refs(&self) -> Vec<String> {
        let mut out: Vec<String> = self.episode_facts.iter().map(|(r, _)| r.clone()).collect();
        out.extend(self.task_refs());
        out
    }
    pub fn check(&self) -> Result<(), GatewayError> {
        check_template(&self.template, DISCHARGE_COORDINATION_TEMPLATE)?;
        check_fresh(self.stale, "episode snapshot")?;
        check_facts(&self.episode_facts, "episode")?;
        if self.tasks.len() > MAX_FACTS {
            return Err(GatewayError::PolicyDenied("too many tasks".into()));
        }
        Ok(())
    }
}

pub fn parse_discharge_coordination(
    raw: &Value,
    req: &DischargeCoordinationRequest,
) -> Result<DischargeCoordinationV1, GatewayError> {
    let mut raw = raw.clone();
    set_schema(&mut raw, DISCHARGE_COORDINATION_SCHEMA);
    force_true(&mut raw, "suggested_steps", "requires_confirmation");
    if let Some(obj) = raw.as_object_mut() {
        obj.insert("operational_only".into(), Value::from(true));
    }
    let parsed: DischargeCoordinationV1 =
        serde_json::from_value(raw).map_err(|e| invalid(DISCHARGE_COORDINATION_SCHEMA, e))?;
    parsed
        .validate(&req.task_refs(), &req.fact_refs())
        .map_err(GatewayError::InvalidOutput)?;
    Ok(parsed)
}

pub fn deterministic_discharge_coordination(
    req: &DischargeCoordinationRequest,
    provider: ProviderInfo,
) -> Result<CareOpsResponse<DischargeCoordinationV1>, GatewayError> {
    req.check()?;
    let es = es(&req.language);
    let open: Vec<&TaskFact> = req
        .tasks
        .iter()
        .filter(|t| !matches!(t.status.as_str(), "completed" | "cancelled"))
        .collect();
    let barriers: Vec<DischargeBarrier> = open
        .iter()
        .filter(|t| t.barrier_category.is_some())
        .take(MAX_CANDIDATES)
        .map(|t| DischargeBarrier {
            task_ref: t.task_ref.clone(),
            category: t.barrier_category.clone().unwrap_or_default(),
            explanation: if es {
                format!(
                    "{} ({}{})",
                    t.title,
                    t.status,
                    if t.overdue { ", vencida" } else { "" }
                )
            } else {
                format!(
                    "{} ({}{})",
                    t.title,
                    t.status,
                    if t.overdue { ", overdue" } else { "" }
                )
            },
            cited_sources: vec![t.task_ref.clone()],
        })
        .collect();
    let suggested_steps: Vec<SuggestedStep> = barriers
        .iter()
        .take(MAX_CANDIDATES)
        .map(|b| SuggestedStep {
            text: if es {
                format!(
                    "Confirmar con el equipo responsable el cierre de la barrera «{}».",
                    b.category.replace('_', " ")
                )
            } else {
                format!(
                    "Confirm with the accountable team how the '{}' barrier will be closed.",
                    b.category.replace('_', " ")
                )
            },
            requires_confirmation: true,
            cited_sources: vec![b.task_ref.clone()],
        })
        .collect();
    let status = fact_text(&req.episode_facts, "episode:status").unwrap_or("unknown");
    let summary = if es {
        format!(
            "Episodio en estado operativo «{status}»: {} tarea(s) abiertas, {} barrera(s) de alta registradas.",
            open.len(),
            barriers.len()
        )
    } else {
        format!(
            "Episode in operational state '{status}': {} open task(s), {} recorded discharge barrier(s).",
            open.len(),
            barriers.len()
        )
    };
    let mut cited: Vec<String> = req
        .episode_facts
        .iter()
        .map(|(r, _)| r.clone())
        .take(5)
        .collect();
    if cited.is_empty() {
        cited.extend(open.first().map(|t| t.task_ref.clone()));
    }
    if cited.is_empty() {
        return Err(GatewayError::PolicyDenied(
            "no episode facts to coordinate against".into(),
        ));
    }
    let output = DischargeCoordinationV1 {
        schema_version: DISCHARGE_COORDINATION_SCHEMA.into(),
        missing_information: {
            let mut m = Vec::new();
            if req.tasks.is_empty() {
                m.push(missing(&req.language, "tasks"));
            }
            if !req
                .episode_facts
                .iter()
                .any(|(r, _)| r == "episode:expected_discharge")
            {
                m.push(missing(&req.language, "episode:expected_discharge"));
            }
            m
        },
        summary,
        barriers,
        suggested_steps,
        operational_only: true,
        cited_sources: cited,
        confidence: if open.is_empty() {
            Confidence::Low
        } else {
            Confidence::Medium
        },
        limitations: vec![
            limitation(&req.language),
            if es {
                "«Listo para el alta» es un estado operativo; la idoneidad médica la decide el clínico responsable.".into()
            } else {
                "'Discharge ready' is an operational state; medical fitness is decided by the responsible clinician.".into()
            },
        ],
    };
    let raw =
        serde_json::to_value(&output).map_err(|e| GatewayError::InvalidOutput(e.to_string()))?;
    let output = parse_discharge_coordination(&raw, req)?;
    Ok(envelope(
        output,
        provider,
        DISCHARGE_COORDINATION_DETERMINISTIC_PROMPT_VERSION,
        hash_json(req),
    ))
}

// ---------------------------------------------------------------------------
// theatre-plan-explanation.v1
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TheatreCaseFact {
    /// `case:<id>`
    pub case_ref: String,
    pub label: String,
    pub status: String,
    pub feasible: bool,
    /// (conflict code, detail) from theatre-conflicts.v1
    pub conflicts: Vec<(String, String)>,
    pub missing_prerequisites: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TheatrePlanExplanationRequest {
    pub template: String,
    pub language: String,
    pub session_label: String,
    pub stale: bool,
    pub session_facts: Vec<(String, String)>,
    pub cases: Vec<TheatreCaseFact>,
}

impl TheatrePlanExplanationRequest {
    pub fn case_refs(&self) -> Vec<String> {
        self.cases.iter().map(|c| c.case_ref.clone()).collect()
    }
    pub fn conflict_codes(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for c in &self.cases {
            for (code, _) in &c.conflicts {
                if !out.contains(code) {
                    out.push(code.clone());
                }
            }
        }
        out
    }
    pub fn fact_refs(&self) -> Vec<String> {
        let mut out: Vec<String> = self.session_facts.iter().map(|(r, _)| r.clone()).collect();
        out.extend(self.case_refs());
        out.extend(
            self.conflict_codes()
                .into_iter()
                .map(|c| format!("conflict:{c}")),
        );
        out
    }
    pub fn check(&self) -> Result<(), GatewayError> {
        check_template(&self.template, THEATRE_PLAN_EXPLANATION_TEMPLATE)?;
        check_fresh(self.stale, "theatre plan")?;
        check_facts(&self.session_facts, "theatre session")?;
        if self.cases.len() > MAX_CASES {
            return Err(GatewayError::PolicyDenied("too many cases".into()));
        }
        Ok(())
    }
}

pub fn parse_theatre_plan(
    raw: &Value,
    req: &TheatrePlanExplanationRequest,
) -> Result<TheatrePlanExplanationV1, GatewayError> {
    let mut raw = raw.clone();
    set_schema(&mut raw, THEATRE_PLAN_EXPLANATION_SCHEMA);
    let parsed: TheatrePlanExplanationV1 =
        serde_json::from_value(raw).map_err(|e| invalid(THEATRE_PLAN_EXPLANATION_SCHEMA, e))?;
    parsed
        .validate(&req.case_refs(), &req.conflict_codes(), &req.fact_refs())
        .map_err(GatewayError::InvalidOutput)?;
    Ok(parsed)
}

pub fn deterministic_theatre_plan(
    req: &TheatrePlanExplanationRequest,
    provider: ProviderInfo,
) -> Result<CareOpsResponse<TheatrePlanExplanationV1>, GatewayError> {
    req.check()?;
    let es = es(&req.language);
    let blocked = req.cases.iter().filter(|c| !c.feasible).count();
    let cases = req
        .cases
        .iter()
        .take(MAX_CANDIDATES)
        .map(|c| {
            let readiness = if c.feasible && c.missing_prerequisites.is_empty() {
                "ready"
            } else if c.conflicts.is_empty() {
                "needs_review"
            } else {
                "blocked"
            };
            let mut cited = vec![c.case_ref.clone()];
            cited.extend(c.conflicts.iter().map(|(k, _)| format!("conflict:{k}")));
            let detail = if c.conflicts.is_empty() && c.missing_prerequisites.is_empty() {
                if es {
                    "sin conflictos detectados".to_string()
                } else {
                    "no conflicts detected".to_string()
                }
            } else {
                let mut parts: Vec<String> = c.conflicts.iter().map(|(_, d)| d.clone()).collect();
                if !c.missing_prerequisites.is_empty() {
                    parts.push(if es {
                        format!(
                            "prerrequisitos pendientes: {}",
                            c.missing_prerequisites.join(", ")
                        )
                    } else {
                        format!(
                            "pending prerequisites: {}",
                            c.missing_prerequisites.join(", ")
                        )
                    });
                }
                parts.join("; ")
            };
            TheatreCaseNote {
                case_ref: c.case_ref.clone(),
                readiness: readiness.into(),
                explanation: format!("{} ({}): {detail}", c.label, c.status),
                cited_sources: cited,
            }
        })
        .collect::<Vec<_>>();
    let conflicts = req
        .conflict_codes()
        .into_iter()
        .take(MAX_CANDIDATES)
        .map(|code| {
            let affected: Vec<String> = req
                .cases
                .iter()
                .filter(|c| c.conflicts.iter().any(|(k, _)| k == &code))
                .map(|c| c.case_ref.clone())
                .collect();
            let mut cited = vec![format!("conflict:{code}")];
            cited.extend(affected.iter().cloned());
            TheatreConflictNote {
                code: code.clone(),
                explanation: if es {
                    format!(
                        "{} afecta a {} caso(s); requiere decisión del coordinador.",
                        code.replace('_', " "),
                        affected.len()
                    )
                } else {
                    format!(
                        "{} affects {} case(s); a coordinator decision is required.",
                        code.replace('_', " "),
                        affected.len()
                    )
                },
                cited_sources: cited,
            }
        })
        .collect();
    let summary = if es {
        format!(
            "{}: {} caso(s), {} bloqueado(s) por el motor determinista.",
            req.session_label,
            req.cases.len(),
            blocked
        )
    } else {
        format!(
            "{}: {} case(s), {} blocked by the deterministic engine.",
            req.session_label,
            req.cases.len(),
            blocked
        )
    };
    let mut cited: Vec<String> = req
        .session_facts
        .iter()
        .map(|(r, _)| r.clone())
        .take(5)
        .collect();
    if cited.is_empty() {
        cited.extend(req.cases.first().map(|c| c.case_ref.clone()));
    }
    if cited.is_empty() {
        return Err(GatewayError::PolicyDenied("empty theatre plan".into()));
    }
    let output = TheatrePlanExplanationV1 {
        schema_version: THEATRE_PLAN_EXPLANATION_SCHEMA.into(),
        missing_information: if req.cases.is_empty() {
            vec![missing(&req.language, "cases")]
        } else {
            req.cases
                .iter()
                .flat_map(|c| {
                    c.missing_prerequisites
                        .iter()
                        .map(move |p| missing(&req.language, &format!("{}:{p}", c.case_ref)))
                })
                .collect()
        },
        summary,
        cases,
        conflicts,
        cited_sources: cited,
        confidence: Confidence::Medium,
        limitations: vec![limitation(&req.language)],
    };
    let raw =
        serde_json::to_value(&output).map_err(|e| GatewayError::InvalidOutput(e.to_string()))?;
    let output = parse_theatre_plan(&raw, req)?;
    Ok(envelope(
        output,
        provider,
        THEATRE_PLAN_EXPLANATION_DETERMINISTIC_PROMPT_VERSION,
        hash_json(req),
    ))
}

// ---------------------------------------------------------------------------
// operational-forecast-explanation.v1
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationalForecastExplanationRequest {
    pub template: String,
    pub language: String,
    pub scope_label: String,
    pub forecast: OpsForecast,
}

impl OperationalForecastExplanationRequest {
    pub fn dates(&self) -> Vec<NaiveDate> {
        match &self.forecast {
            OpsForecast::Ready { days, .. } => days.iter().map(|d| d.date).collect(),
            OpsForecast::InsufficientHistory { .. } => Vec::new(),
        }
    }
    pub fn fact_refs(&self) -> Vec<String> {
        let mut out = vec!["forecast:summary".to_string()];
        if let OpsForecast::Ready { days, .. } = &self.forecast {
            for d in days {
                out.push(format!("forecast:day:{}", d.date));
                for f in &d.factors {
                    let r = format!("forecast:factor:{f}");
                    if !out.contains(&r) {
                        out.push(r);
                    }
                }
            }
        }
        out
    }
    pub fn check(&self) -> Result<(), GatewayError> {
        check_template(&self.template, OPERATIONAL_FORECAST_EXPLANATION_TEMPLATE)
    }
}

pub fn parse_forecast_explanation(
    raw: &Value,
    req: &OperationalForecastExplanationRequest,
) -> Result<OperationalForecastExplanationV1, GatewayError> {
    let mut raw = raw.clone();
    set_schema(&mut raw, OPERATIONAL_FORECAST_EXPLANATION_SCHEMA);
    force_true(&mut raw, "recommendations", "requires_confirmation");
    let parsed: OperationalForecastExplanationV1 = serde_json::from_value(raw)
        .map_err(|e| invalid(OPERATIONAL_FORECAST_EXPLANATION_SCHEMA, e))?;
    parsed
        .validate(&req.dates(), &req.fact_refs())
        .map_err(GatewayError::InvalidOutput)?;
    Ok(parsed)
}

pub fn deterministic_forecast_explanation(
    req: &OperationalForecastExplanationRequest,
    provider: ProviderInfo,
) -> Result<CareOpsResponse<OperationalForecastExplanationV1>, GatewayError> {
    req.check()?;
    let es = es(&req.language);
    let output = match &req.forecast {
        OpsForecast::InsufficientHistory {
            history_days,
            required_days,
            ..
        } => OperationalForecastExplanationV1 {
            schema_version: OPERATIONAL_FORECAST_EXPLANATION_SCHEMA.into(),
            summary: if es {
                format!(
                    "Sin previsión para {}: {} día(s) de historial, se requieren {}.",
                    req.scope_label, history_days, required_days
                )
            } else {
                format!(
                    "No forecast for {}: {} day(s) of history, {} required.",
                    req.scope_label, history_days, required_days
                )
            },
            pressure_points: vec![],
            recommendations: vec![OpsRecommendation {
                category: "collect_more_history".into(),
                text: if es {
                    "Seguir registrando llegadas, ingresos y altas antes de planificar con la previsión.".into()
                } else {
                    "Keep recording arrivals, admissions and discharges before planning on the forecast.".into()
                },
                requires_confirmation: true,
            }],
            missing_information: vec![missing(
                &req.language,
                &format!("history:{history_days}/{required_days}"),
            )],
            cited_sources: vec!["forecast:summary".into()],
            confidence: Confidence::Low,
            limitations: vec![if es {
                "Evidencia insuficiente: no se ha generado previsión.".into()
            } else {
                "Insufficient evidence: no forecast was produced.".into()
            }],
        },
        OpsForecast::Ready {
            days,
            confidence,
            pressure_dates,
            ..
        } => {
            let arrivals: f64 = days.iter().map(|d| d.expected_arrivals).sum();
            let admissions: f64 = days.iter().map(|d| d.expected_admissions).sum();
            let discharges: f64 = days.iter().map(|d| d.expected_discharges).sum();
            let peak = days
                .iter()
                .map(|d| d.occupancy_pressure)
                .fold(0.0_f64, f64::max);
            let summary = if es {
                format!(
                    "{}: {:.0} llegadas, {:.0} ingresos y {:.0} altas esperadas en {} día(s); presión máxima de ocupación {:.0} % con {} día(s) de presión.",
                    req.scope_label, arrivals, admissions, discharges, days.len(), peak * 100.0, pressure_dates.len()
                )
            } else {
                format!(
                    "{}: {:.0} arrivals, {:.0} admissions and {:.0} discharges expected over {} day(s); peak occupancy pressure {:.0}% with {} pressure day(s).",
                    req.scope_label, arrivals, admissions, discharges, days.len(), peak * 100.0, pressure_dates.len()
                )
            };
            let pressure_points = days
                .iter()
                .filter(|d| pressure_dates.contains(&d.date))
                .take(10)
                .map(|d| {
                    let mut cited = vec![format!("forecast:day:{}", d.date)];
                    cited.extend(d.factors.iter().take(3).map(|f| format!("forecast:factor:{f}")));
                    ForecastPressurePoint {
                        date: d.date,
                        explanation: if es {
                            format!("Ocupación prevista {:.0} % con {:.0} llegadas ({:.0}–{:.0}) y {} hueco(s) de personal.", d.occupancy_pressure * 100.0, d.expected_arrivals, d.arrivals_low, d.arrivals_high, d.staffing_gaps)
                        } else {
                            format!("Forecast occupancy {:.0}% with {:.0} arrivals ({:.0}–{:.0}) and {} staffing gap(s).", d.occupancy_pressure * 100.0, d.expected_arrivals, d.arrivals_low, d.arrivals_high, d.staffing_gaps)
                        },
                        cited_sources: cited,
                    }
                })
                .collect::<Vec<_>>();
            let mut recommendations = Vec::new();
            if !pressure_points.is_empty() {
                recommendations.push(OpsRecommendation {
                    category: "review_discharge_pipeline".into(),
                    text: if es {
                        "Revisar las altas en preparación de los días con presión.".into()
                    } else {
                        "Review the discharge pipeline for the pressure days.".into()
                    },
                    requires_confirmation: true,
                });
            }
            if days.iter().any(|d| d.staffing_gaps > 0) {
                recommendations.push(OpsRecommendation {
                    category: "review_staffing".into(),
                    text: if es {
                        "Revisar los huecos de cobertura configurados en el horizonte.".into()
                    } else {
                        "Review the configured coverage gaps within the horizon.".into()
                    },
                    requires_confirmation: true,
                });
            }
            OperationalForecastExplanationV1 {
                schema_version: OPERATIONAL_FORECAST_EXPLANATION_SCHEMA.into(),
                summary,
                pressure_points,
                recommendations,
                missing_information: if days.is_empty() {
                    vec![missing(&req.language, "days")]
                } else {
                    vec![]
                },
                cited_sources: vec!["forecast:summary".into()],
                confidence: if *confidence >= 0.7 {
                    Confidence::Medium
                } else {
                    Confidence::Low
                },
                limitations: vec![
                    if es {
                        "Previsión agregada determinista; no predice pacientes concretos.".into()
                    } else {
                        "Aggregate deterministic forecast; it predicts no individual patient."
                            .into()
                    },
                    limitation(&req.language),
                ],
            }
        }
    };
    let raw =
        serde_json::to_value(&output).map_err(|e| GatewayError::InvalidOutput(e.to_string()))?;
    let output = parse_forecast_explanation(&raw, req)?;
    Ok(envelope(
        output,
        provider,
        OPERATIONAL_FORECAST_EXPLANATION_DETERMINISTIC_PROMPT_VERSION,
        hash_json(req),
    ))
}

// ---------------------------------------------------------------------------
// shift-handoff-draft.v1
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShiftHandoffDraftRequest {
    pub template: String,
    pub language: String,
    pub shift_label: String,
    pub stale: bool,
    /// Operational facts only: `episode:*`, `placement:*`, `task:*`,
    /// `escalation:*`, `appointment:*`, `order:*` references.
    pub facts: Vec<(String, String)>,
}

impl ShiftHandoffDraftRequest {
    pub fn fact_refs(&self) -> Vec<String> {
        self.facts.iter().map(|(r, _)| r.clone()).collect()
    }
    pub fn check(&self) -> Result<(), GatewayError> {
        check_template(&self.template, SHIFT_HANDOFF_DRAFT_TEMPLATE)?;
        check_fresh(self.stale, "episode snapshot")?;
        check_facts(&self.facts, "handoff")?;
        if self.facts.is_empty() {
            return Err(GatewayError::PolicyDenied("no facts to hand off".into()));
        }
        Ok(())
    }
}

pub fn parse_handoff_draft(
    raw: &Value,
    req: &ShiftHandoffDraftRequest,
) -> Result<ShiftHandoffDraftV1, GatewayError> {
    let mut raw = raw.clone();
    set_schema(&mut raw, SHIFT_HANDOFF_DRAFT_SCHEMA);
    let parsed: ShiftHandoffDraftV1 =
        serde_json::from_value(raw).map_err(|e| invalid(SHIFT_HANDOFF_DRAFT_SCHEMA, e))?;
    parsed
        .validate(&req.fact_refs())
        .map_err(GatewayError::InvalidOutput)?;
    Ok(parsed)
}

pub fn deterministic_handoff_draft(
    req: &ShiftHandoffDraftRequest,
    provider: ProviderInfo,
) -> Result<CareOpsResponse<ShiftHandoffDraftV1>, GatewayError> {
    req.check()?;
    let es = es(&req.language);
    let items = |prefix: &str| -> Vec<EvidenceItem> {
        req.facts
            .iter()
            .filter(|(r, _)| r.starts_with(prefix))
            .take(MAX_CANDIDATES)
            .map(|(r, v)| EvidenceItem {
                text: v.clone(),
                cited_sources: vec![r.clone()],
            })
            .collect()
    };
    let status = fact_text(&req.facts, "episode:status").unwrap_or("unknown");
    let placement = fact_text(&req.facts, "placement:current");
    let situation = if es {
        format!(
            "Turno {}: episodio en estado «{status}»{}.",
            req.shift_label,
            placement
                .map(|p| format!(", ubicación {p}"))
                .unwrap_or_default()
        )
    } else {
        format!(
            "Shift {}: episode in state '{status}'{}.",
            req.shift_label,
            placement
                .map(|p| format!(", located at {p}"))
                .unwrap_or_default()
        )
    };
    let background = fact_text(&req.facts, "episode:type")
        .map(|t| {
            if es {
                format!("Tipo de episodio: {t}.")
            } else {
                format!("Episode type: {t}.")
            }
        })
        .unwrap_or_else(|| {
            if es {
                "Sin antecedentes operativos adicionales.".into()
            } else {
                "No further operational background.".into()
            }
        });
    let output = ShiftHandoffDraftV1 {
        schema_version: SHIFT_HANDOFF_DRAFT_SCHEMA.into(),
        situation,
        background,
        pending: items("task:"),
        risks: items("escalation:"),
        barriers: items("barrier:"),
        next_steps: items("next:"),
        missing_information: ["placement:", "task:", "escalation:", "barrier:", "next:"]
            .iter()
            .filter(|p| !req.facts.iter().any(|(r, _)| r.starts_with(*p)))
            .map(|p| missing(&req.language, p.trim_end_matches(':')))
            .collect(),
        cited_sources: req.fact_refs().into_iter().take(5).collect(),
        confidence: Confidence::Medium,
        limitations: vec![
            limitation(&req.language),
            if es {
                "Borrador: no es un traspaso hasta que un profesional lo acepte.".into()
            } else {
                "Draft: not a handoff until a professional accepts it.".into()
            },
        ],
    };
    let raw =
        serde_json::to_value(&output).map_err(|e| GatewayError::InvalidOutput(e.to_string()))?;
    let output = parse_handoff_draft(&raw, req)?;
    Ok(envelope(
        output,
        provider,
        SHIFT_HANDOFF_DRAFT_DETERMINISTIC_PROMPT_VERSION,
        hash_json(req),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider() -> ProviderInfo {
        ProviderInfo {
            provider: "test".into(),
            model: "fixture".into(),
            model_version: "1".into(),
        }
    }

    #[test]
    fn stale_placement_candidates_are_refused_before_any_ranking() {
        let req = PlacementRankingRequest {
            template: PLACEMENT_RANKING_TEMPLATE.into(),
            language: "en".into(),
            matcher_version: PLACEMENT_MATCHER_VERSION.into(),
            facts_hash: "sha256:a".into(),
            current_facts_hash: "sha256:b".into(),
            request_facts: vec![("request:requirements".into(), "isolation".into())],
            candidates: vec![PlacementCandidateFact {
                candidate_ref: "candidate:1".into(),
                label: "Bed 1".into(),
                score: 90.0,
                factors: vec![],
                reasons: vec![],
            }],
        };
        assert!(matches!(
            deterministic_placement_ranking(&req, provider()),
            Err(GatewayError::PolicyDenied(_))
        ));
        let fresh = PlacementRankingRequest {
            current_facts_hash: "sha256:a".into(),
            ..req
        };
        let out = deterministic_placement_ranking(&fresh, provider()).unwrap();
        assert_eq!(out.output.ranked.len(), 1);
    }

    #[test]
    fn handoff_draft_is_bilingual_and_cites_only_supplied_facts() {
        let req = ShiftHandoffDraftRequest {
            template: SHIFT_HANDOFF_DRAFT_TEMPLATE.into(),
            language: "es".into(),
            shift_label: "noche".into(),
            stale: false,
            facts: vec![
                ("episode:status".into(), "active".into()),
                ("task:1".into(), "Fisioterapia pendiente".into()),
            ],
        };
        let out = deterministic_handoff_draft(&req, provider()).unwrap();
        assert!(out.output.situation.starts_with("Turno noche"));
        assert_eq!(
            out.output.pending[0].cited_sources,
            vec!["task:1".to_string()]
        );
    }
}
