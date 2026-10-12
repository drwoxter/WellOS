//! Care Operations persistence: operational locations, care episodes,
//! placement requests/candidates/placements, coordination tasks, handoffs
//! and escalation timers.
//!
//! All loaders are tenant-scoped; every mutation runs inside the caller's
//! transaction, bumps the row `version`, appends to the append-only history
//! tables and leaves auditing/eventing to the route layer.

pub mod flow;
pub mod forecast;
pub mod theatre;
pub mod workforce;

use crate::auth::AuthContext;
use crate::error::ApiError;
use crate::scheduling;
use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use uuid::Uuid;
use wellos_domain::care_ops::{
    self as domain, EpisodeStatus, EpisodeTransition, PlacementCandidate, PlacementFacts,
    PlacementMatch, PlacementRequestStatus, PlacementRequestTransition, PlacementRequirements,
    PlacementStatus, PlacementTransition, PositionFact, PositionState, TaskStatus, TaskTransition,
};

pub const DEFAULT_LIMIT: i64 = 100;
pub const MAX_LIMIT: i64 = 500;

pub fn stale(what: &'static str) -> ApiError {
    ApiError::conflict(
        "stale_version",
        format!("{what} changed since it was loaded; reload and retry"),
    )
}

pub fn invalid_transition(e: domain::InvalidTransition) -> ApiError {
    ApiError::conflict("invalid_transition", e.to_string())
}

fn actor(ctx: &AuthContext) -> String {
    format!("user:{}", ctx.user_id)
}

// ---------------------------------------------------------------------------
// Operational locations
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct LocationRow {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub facility_id: Uuid,
    pub parent_id: Option<Uuid>,
    pub level: String,
    pub type_code: String,
    pub code: String,
    pub name_en: String,
    pub name_es: String,
    pub specialty_codes: Vec<String>,
    pub service_line_codes: Vec<String>,
    pub profession_requirements: Value,
    pub age_group_codes: Vec<String>,
    pub isolation_codes: Vec<String>,
    pub accessibility_codes: Vec<String>,
    pub equipment_codes: Vec<String>,
    pub room_policy: String,
    pub capacity: i32,
    pub operational_status: String,
    pub position_state: Option<String>,
    pub cleaning_minutes: i32,
    pub schedulable_resource_id: Option<Uuid>,
    pub effective_from: Option<NaiveDate>,
    pub effective_to: Option<NaiveDate>,
    pub active: bool,
    pub metadata: Value,
    pub version: i64,
    pub updated_at: DateTime<Utc>,
}

const LOCATION_COLS: &str = "id, tenant_id, facility_id, parent_id, level, type_code, code, name_en, name_es,
    specialty_codes, service_line_codes, profession_requirements, age_group_codes, isolation_codes,
    accessibility_codes, equipment_codes, room_policy, capacity, operational_status, position_state,
    cleaning_minutes, schedulable_resource_id, effective_from, effective_to, active, metadata, version,
    updated_at";

fn location_from_row(r: &PgRow) -> LocationRow {
    LocationRow {
        id: r.get("id"),
        tenant_id: r.get("tenant_id"),
        facility_id: r.get("facility_id"),
        parent_id: r.get("parent_id"),
        level: r.get("level"),
        type_code: r.get("type_code"),
        code: r.get("code"),
        name_en: r.get("name_en"),
        name_es: r.get("name_es"),
        specialty_codes: r.get("specialty_codes"),
        service_line_codes: r.get("service_line_codes"),
        profession_requirements: r.get("profession_requirements"),
        age_group_codes: r.get("age_group_codes"),
        isolation_codes: r.get("isolation_codes"),
        accessibility_codes: r.get("accessibility_codes"),
        equipment_codes: r.get("equipment_codes"),
        room_policy: r.get("room_policy"),
        capacity: r.get("capacity"),
        operational_status: r.get("operational_status"),
        position_state: r.get("position_state"),
        cleaning_minutes: r.get("cleaning_minutes"),
        schedulable_resource_id: r.get("schedulable_resource_id"),
        effective_from: r.get("effective_from"),
        effective_to: r.get("effective_to"),
        active: r.get("active"),
        metadata: r.get("metadata"),
        version: r.get("version"),
        updated_at: r.get("updated_at"),
    }
}

pub async fn list_locations(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    facility_ids: Option<&[Uuid]>,
) -> Result<Vec<LocationRow>, ApiError> {
    let sql = format!(
        "SELECT {LOCATION_COLS} FROM operational_locations
         WHERE tenant_id = $1 AND ($2::uuid[] IS NULL OR facility_id = ANY($2))
         ORDER BY facility_id, level, code"
    );
    let rows = sqlx::query(&sql)
        .bind(tenant_id)
        .bind(facility_ids)
        .fetch_all(&mut *conn)
        .await?;
    Ok(rows.iter().map(location_from_row).collect())
}

pub async fn load_location(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    id: Uuid,
    lock: bool,
) -> Result<LocationRow, ApiError> {
    let sql = format!(
        "SELECT {LOCATION_COLS} FROM operational_locations WHERE tenant_id = $1 AND id = $2 {}",
        if lock { "FOR UPDATE" } else { "" }
    );
    let row = sqlx::query(&sql)
        .bind(tenant_id)
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(ApiError::not_found)?;
    Ok(location_from_row(&row))
}

#[derive(Debug, Deserialize)]
pub struct LocationInput {
    pub facility_id: Uuid,
    pub parent_id: Option<Uuid>,
    pub level: String,
    pub type_code: String,
    pub code: String,
    pub name_en: String,
    pub name_es: String,
    #[serde(default)]
    pub specialty_codes: Vec<String>,
    #[serde(default)]
    pub service_line_codes: Vec<String>,
    #[serde(default = "empty_array")]
    pub profession_requirements: Value,
    #[serde(default)]
    pub age_group_codes: Vec<String>,
    #[serde(default)]
    pub isolation_codes: Vec<String>,
    #[serde(default)]
    pub accessibility_codes: Vec<String>,
    #[serde(default)]
    pub equipment_codes: Vec<String>,
    pub room_policy: Option<String>,
    pub capacity: Option<i32>,
    pub cleaning_minutes: Option<i32>,
    pub schedulable_resource_id: Option<Uuid>,
    pub effective_from: Option<NaiveDate>,
    pub effective_to: Option<NaiveDate>,
    #[serde(default = "empty_object")]
    pub metadata: Value,
}

fn empty_array() -> Value {
    json!([])
}
fn empty_object() -> Value {
    json!({})
}

pub const LEVELS: &[&str] = &["department", "unit", "room", "position"];

pub async fn create_location(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    input: &LocationInput,
) -> Result<LocationRow, ApiError> {
    if !LEVELS.contains(&input.level.as_str()) {
        return Err(ApiError::bad_request(
            "invalid_level",
            "unknown location level",
        ));
    }
    if let Some(parent) = input.parent_id {
        let p = load_location(conn, ctx.tenant_id, parent, false).await?;
        if p.facility_id != input.facility_id {
            return Err(ApiError::bad_request(
                "parent_facility_mismatch",
                "parent location belongs to another facility",
            ));
        }
        let order = |l: &str| LEVELS.iter().position(|x| *x == l).unwrap_or(0);
        if order(&p.level) >= order(&input.level) {
            return Err(ApiError::bad_request(
                "invalid_hierarchy",
                "parent must be a higher level than the child",
            ));
        }
    } else if input.level != "department" {
        return Err(ApiError::bad_request(
            "parent_required",
            "only departments may be top-level locations",
        ));
    }
    let position_state = (input.level == "position").then_some("available");
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO operational_locations
         (id, tenant_id, facility_id, parent_id, level, type_code, code, name_en, name_es,
          specialty_codes, service_line_codes, profession_requirements, age_group_codes,
          isolation_codes, accessibility_codes, equipment_codes, room_policy, capacity,
          position_state, cleaning_minutes, schedulable_resource_id, effective_from, effective_to,
          metadata, created_by)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22,$23,$24,$25)",
    )
    .bind(id)
    .bind(ctx.tenant_id)
    .bind(input.facility_id)
    .bind(input.parent_id)
    .bind(&input.level)
    .bind(&input.type_code)
    .bind(&input.code)
    .bind(&input.name_en)
    .bind(&input.name_es)
    .bind(&input.specialty_codes)
    .bind(&input.service_line_codes)
    .bind(&input.profession_requirements)
    .bind(&input.age_group_codes)
    .bind(&input.isolation_codes)
    .bind(&input.accessibility_codes)
    .bind(&input.equipment_codes)
    .bind(input.room_policy.as_deref().unwrap_or("mixed"))
    .bind(input.capacity.unwrap_or(1))
    .bind(position_state)
    .bind(input.cleaning_minutes.unwrap_or(30))
    .bind(input.schedulable_resource_id)
    .bind(input.effective_from)
    .bind(input.effective_to)
    .bind(&input.metadata)
    .bind(ctx.user_id)
    .execute(&mut *conn)
    .await
    .map_err(map_unique("location_code_exists", "a location with this code already exists"))?;
    if let Some(state) = position_state {
        record_location_history(conn, ctx, id, None, state, None, None, 1).await?;
    }
    load_location(conn, ctx.tenant_id, id, false).await
}

fn map_unique(code: &'static str, msg: &'static str) -> impl Fn(sqlx::Error) -> ApiError {
    move |e| match &e {
        sqlx::Error::Database(d) if d.code().as_deref() == Some("23505") => {
            ApiError::conflict(code, msg)
        }
        sqlx::Error::Database(d) if d.code().as_deref() == Some("23P01") => ApiError::conflict(
            "overlapping_placement",
            "the position is already held or occupied for this period",
        ),
        _ => ApiError::from(e),
    }
}

#[allow(clippy::too_many_arguments)]
async fn record_location_history(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    location_id: Uuid,
    from: Option<&str>,
    to: &str,
    reason: Option<&str>,
    note: Option<&str>,
    version: i64,
) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO operational_location_history
         (id, tenant_id, location_id, from_state, to_state, reason_code, note, actor, version)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)",
    )
    .bind(Uuid::now_v7())
    .bind(ctx.tenant_id)
    .bind(location_id)
    .bind(from)
    .bind(to)
    .bind(reason)
    .bind(note)
    .bind(actor(ctx))
    .bind(version)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Explicit human position state change (block, out-of-service, finish
/// cleaning, reopen). Held/occupied are reached only through placements.
pub async fn set_position_state(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    location_id: Uuid,
    expected_version: i64,
    to: PositionState,
    reason: Option<&str>,
    note: Option<&str>,
) -> Result<LocationRow, ApiError> {
    let loc = load_location(conn, ctx.tenant_id, location_id, true).await?;
    if loc.version != expected_version {
        return Err(stale("position"));
    }
    let from = loc
        .position_state
        .as_deref()
        .and_then(PositionState::parse)
        .ok_or_else(|| ApiError::bad_request("not_a_position", "only positions carry a state"))?;
    if matches!(to, PositionState::Held | PositionState::Occupied) {
        return Err(ApiError::bad_request(
            "placement_required",
            "held and occupied are set through placements, not directly",
        ));
    }
    if !from.can_transition(to) {
        return Err(ApiError::conflict(
            "invalid_transition",
            format!(
                "position cannot move from {} to {}",
                from.as_str(),
                to.as_str()
            ),
        ));
    }
    if from == PositionState::Occupied || from == PositionState::Held {
        let live: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM placements WHERE location_id = $1 AND status IN ('held','occupied')",
        )
        .bind(location_id)
        .fetch_one(&mut *conn)
        .await?;
        if live > 0 {
            return Err(ApiError::conflict(
                "live_placement",
                "release or transfer the current placement first",
            ));
        }
    }
    sqlx::query(
        "UPDATE operational_locations SET position_state = $3, version = version + 1, updated_at = now()
         WHERE tenant_id = $1 AND id = $2",
    )
    .bind(ctx.tenant_id)
    .bind(location_id)
    .bind(to.as_str())
    .execute(&mut *conn)
    .await?;
    record_location_history(
        conn,
        ctx,
        location_id,
        Some(from.as_str()),
        to.as_str(),
        reason,
        note,
        loc.version + 1,
    )
    .await?;
    load_location(conn, ctx.tenant_id, location_id, false).await
}

async fn transition_position(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    location_id: Uuid,
    to: PositionState,
    reason: &str,
) -> Result<(), ApiError> {
    let loc = load_location(conn, ctx.tenant_id, location_id, true).await?;
    let from = loc
        .position_state
        .as_deref()
        .and_then(PositionState::parse)
        .ok_or_else(|| {
            ApiError::bad_request("not_a_position", "placements target positions only")
        })?;
    if !from.can_transition(to) {
        return Err(ApiError::conflict(
            "position_unavailable",
            format!(
                "position is {} and cannot become {}",
                from.as_str(),
                to.as_str()
            ),
        ));
    }
    sqlx::query(
        "UPDATE operational_locations SET position_state = $2, version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(location_id)
    .bind(to.as_str())
    .execute(&mut *conn)
    .await?;
    record_location_history(
        conn,
        ctx,
        location_id,
        Some(from.as_str()),
        to.as_str(),
        Some(reason),
        None,
        loc.version + 1,
    )
    .await
}

#[derive(Debug, Deserialize)]
pub struct PeriodInput {
    pub kind: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: Option<DateTime<Utc>>,
    pub reason_code: Option<String>,
}

pub const PERIOD_KINDS: &[&str] = &["closure", "maintenance", "out_of_service", "cleaning"];

pub async fn create_period(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    location_id: Uuid,
    input: &PeriodInput,
) -> Result<Uuid, ApiError> {
    if !PERIOD_KINDS.contains(&input.kind.as_str()) {
        return Err(ApiError::bad_request(
            "invalid_period_kind",
            "unknown period kind",
        ));
    }
    load_location(conn, ctx.tenant_id, location_id, false).await?;
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO operational_location_periods (id, tenant_id, location_id, kind, starts_at, ends_at, reason_code, created_by)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
    )
    .bind(id)
    .bind(ctx.tenant_id)
    .bind(location_id)
    .bind(&input.kind)
    .bind(input.starts_at)
    .bind(input.ends_at)
    .bind(&input.reason_code)
    .bind(ctx.user_id)
    .execute(&mut *conn)
    .await?;
    Ok(id)
}

pub async fn deactivate_period(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    period_id: Uuid,
) -> Result<(), ApiError> {
    let n = sqlx::query(
        "UPDATE operational_location_periods SET active = false, deactivated_at = now()
         WHERE tenant_id = $1 AND id = $2 AND active",
    )
    .bind(tenant_id)
    .bind(period_id)
    .execute(&mut *conn)
    .await?
    .rows_affected();
    if n == 0 {
        return Err(ApiError::not_found());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Care episodes
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct EpisodeRow {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub facility_id: Uuid,
    pub patient_id: Uuid,
    pub episode_type_code: String,
    pub status: String,
    pub responsible_service_code: Option<String>,
    pub responsible_queue_id: Option<Uuid>,
    pub responsible_user_id: Option<Uuid>,
    pub current_placement_id: Option<Uuid>,
    pub started_at: Option<DateTime<Utc>>,
    pub expected_end_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    pub priority: String,
    pub goals: Value,
    pub barriers: Value,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

const EPISODE_COLS: &str = "id, tenant_id, facility_id, patient_id, episode_type_code, status,
    responsible_service_code, responsible_queue_id, responsible_user_id, current_placement_id,
    started_at, expected_end_at, ended_at, priority, goals, barriers, version, created_at, updated_at";

fn episode_from_row(r: &PgRow) -> EpisodeRow {
    EpisodeRow {
        id: r.get("id"),
        tenant_id: r.get("tenant_id"),
        facility_id: r.get("facility_id"),
        patient_id: r.get("patient_id"),
        episode_type_code: r.get("episode_type_code"),
        status: r.get("status"),
        responsible_service_code: r.get("responsible_service_code"),
        responsible_queue_id: r.get("responsible_queue_id"),
        responsible_user_id: r.get("responsible_user_id"),
        current_placement_id: r.get("current_placement_id"),
        started_at: r.get("started_at"),
        expected_end_at: r.get("expected_end_at"),
        ended_at: r.get("ended_at"),
        priority: r.get("priority"),
        goals: r.get("goals"),
        barriers: r.get("barriers"),
        version: r.get("version"),
        created_at: r.get("created_at"),
        updated_at: r.get("updated_at"),
    }
}

impl EpisodeRow {
    pub fn status_enum(&self) -> EpisodeStatus {
        EpisodeStatus::parse(&self.status).unwrap_or(EpisodeStatus::Error)
    }

    pub fn open_barriers(&self) -> usize {
        self.barriers
            .as_array()
            .map(|a| {
                a.iter()
                    .filter(|b| b.get("status").and_then(Value::as_str) != Some("resolved"))
                    .count()
            })
            .unwrap_or(0)
    }
}

pub async fn load_episode(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    id: Uuid,
    lock: bool,
) -> Result<EpisodeRow, ApiError> {
    let sql = format!(
        "SELECT {EPISODE_COLS} FROM care_episodes WHERE tenant_id = $1 AND id = $2 {}",
        if lock { "FOR UPDATE" } else { "" }
    );
    let row = sqlx::query(&sql)
        .bind(tenant_id)
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(ApiError::not_found)?;
    Ok(episode_from_row(&row))
}

pub struct EpisodeFilter<'a> {
    pub facility_ids: Option<&'a [Uuid]>,
    pub patient_id: Option<Uuid>,
    pub statuses: &'a [String],
    pub open_only: bool,
    pub limit: i64,
}

pub async fn list_episodes(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    f: EpisodeFilter<'_>,
) -> Result<Vec<EpisodeRow>, ApiError> {
    let sql = format!(
        "SELECT {EPISODE_COLS} FROM care_episodes
         WHERE tenant_id = $1 AND ($2::uuid[] IS NULL OR facility_id = ANY($2))
           AND ($3::uuid IS NULL OR patient_id = $3)
           AND (cardinality($4::text[]) = 0 OR status = ANY($4))
           AND (NOT $5 OR status IN ('planned','active','discharge_preparation','discharge_ready'))
         ORDER BY CASE priority WHEN 'emergency' THEN 0 WHEN 'urgent' THEN 1 WHEN 'priority' THEN 2 ELSE 3 END,
                  created_at DESC
         LIMIT $6"
    );
    let rows = sqlx::query(&sql)
        .bind(tenant_id)
        .bind(f.facility_ids)
        .bind(f.patient_id)
        .bind(f.statuses)
        .bind(f.open_only)
        .bind(f.limit)
        .fetch_all(&mut *conn)
        .await?;
    Ok(rows.iter().map(episode_from_row).collect())
}

#[derive(Debug, Deserialize)]
pub struct EpisodeInput {
    pub facility_id: Uuid,
    pub patient_id: Uuid,
    pub episode_type_code: String,
    pub responsible_service_code: Option<String>,
    pub responsible_queue_id: Option<Uuid>,
    pub responsible_user_id: Option<Uuid>,
    pub priority: Option<String>,
    pub expected_end_at: Option<DateTime<Utc>>,
    #[serde(default = "empty_array")]
    pub goals: Value,
    #[serde(default = "empty_array")]
    pub barriers: Value,
    /// Existing authorities to link: [{"link_type":"visit","link_id":"..."}]
    #[serde(default)]
    pub links: Vec<LinkInput>,
    /// Start immediately (`active`) instead of `planned`.
    #[serde(default)]
    pub activate: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LinkInput {
    pub link_type: String,
    pub link_id: Uuid,
}

pub const PRIORITIES: &[&str] = &["routine", "priority", "urgent", "emergency"];
pub const LINK_TYPES: &[&str] = &[
    "visit",
    "encounter",
    "appointment",
    "service_request",
    "follow_up_task",
    "internal_alert",
    "care_team_assignment",
    "transport_request",
    "diagnostic_report",
];

/// The link target must exist for the same tenant and patient; the table
/// names are a closed set so the identifier splice is safe.
async fn verify_link(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    patient_id: Uuid,
    link: &LinkInput,
) -> Result<(), ApiError> {
    let (table, has_patient) = match link.link_type.as_str() {
        "visit" => ("visits", true),
        "encounter" => ("encounters", true),
        "appointment" => ("appointments", true),
        "service_request" => ("service_requests", true),
        "follow_up_task" => ("follow_up_tasks", true),
        "internal_alert" => ("internal_alerts", true),
        "care_team_assignment" => ("care_team_assignments", true),
        "transport_request" => ("transport_requests", true),
        "diagnostic_report" => ("diagnostic_reports", true),
        _ => {
            return Err(ApiError::bad_request(
                "invalid_link_type",
                "unknown link type",
            ))
        }
    };
    let sql = if has_patient {
        format!("SELECT 1 FROM {table} WHERE tenant_id = $1 AND id = $2 AND patient_id = $3")
    } else {
        format!("SELECT 1 FROM {table} WHERE tenant_id = $1 AND id = $2 AND $3::uuid IS NOT NULL")
    };
    let ok: Option<i32> = sqlx::query_scalar(&sql)
        .bind(tenant_id)
        .bind(link.link_id)
        .bind(patient_id)
        .fetch_optional(&mut *conn)
        .await?;
    if ok.is_none() {
        return Err(ApiError::bad_request(
            "link_target_not_found",
            format!(
                "{} {} does not belong to this patient",
                link.link_type, link.link_id
            ),
        ));
    }
    Ok(())
}

pub async fn add_link(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    ep: &EpisodeRow,
    link: &LinkInput,
) -> Result<(), ApiError> {
    verify_link(conn, ctx.tenant_id, ep.patient_id, link).await?;
    sqlx::query(
        "INSERT INTO care_episode_links (id, tenant_id, episode_id, link_type, link_id, created_by)
         VALUES ($1,$2,$3,$4,$5,$6) ON CONFLICT DO NOTHING",
    )
    .bind(Uuid::now_v7())
    .bind(ctx.tenant_id)
    .bind(ep.id)
    .bind(&link.link_type)
    .bind(link.link_id)
    .bind(ctx.user_id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

pub async fn episode_links(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    episode_id: Uuid,
) -> Result<Vec<LinkInput>, ApiError> {
    let rows = sqlx::query(
        "SELECT link_type, link_id FROM care_episode_links WHERE tenant_id = $1 AND episode_id = $2 ORDER BY created_at",
    )
    .bind(tenant_id)
    .bind(episode_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .iter()
        .map(|r| LinkInput {
            link_type: r.get("link_type"),
            link_id: r.get("link_id"),
        })
        .collect())
}

#[allow(clippy::too_many_arguments)]
async fn record_episode_history(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    episode_id: Uuid,
    from: Option<&str>,
    to: &str,
    change_kind: &str,
    reason: Option<&str>,
    note: Option<&str>,
    version: i64,
) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO care_episode_history
         (id, tenant_id, episode_id, from_status, to_status, change_kind, reason_code, note, actor, version)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
    )
    .bind(Uuid::now_v7())
    .bind(ctx.tenant_id)
    .bind(episode_id)
    .bind(from)
    .bind(to)
    .bind(change_kind)
    .bind(reason)
    .bind(note)
    .bind(actor(ctx))
    .bind(version)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

async fn catalog_code_exists(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    kind: &str,
    code: &str,
) -> Result<bool, ApiError> {
    let ok: Option<i32> = sqlx::query_scalar(
        "SELECT 1 FROM catalog_entries WHERE tenant_id = $1 AND kind = $2 AND code = $3 AND active",
    )
    .bind(tenant_id)
    .bind(kind)
    .bind(code)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(ok.is_some())
}

pub async fn create_episode(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    input: &EpisodeInput,
) -> Result<EpisodeRow, ApiError> {
    let priority = input.priority.as_deref().unwrap_or("routine");
    if !PRIORITIES.contains(&priority) {
        return Err(ApiError::bad_request(
            "invalid_priority",
            "unknown priority",
        ));
    }
    if !catalog_code_exists(
        conn,
        ctx.tenant_id,
        "episode_type",
        &input.episode_type_code,
    )
    .await?
    {
        return Err(ApiError::bad_request(
            "unknown_episode_type",
            "episode type is not an active tenant catalog entry",
        ));
    }
    let patient_ok: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM patients WHERE tenant_id = $1 AND id = $2")
            .bind(ctx.tenant_id)
            .bind(input.patient_id)
            .fetch_optional(&mut *conn)
            .await?;
    if patient_ok.is_none() {
        return Err(ApiError::not_found());
    }
    let facility_ok: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM facilities WHERE tenant_id = $1 AND id = $2")
            .bind(ctx.tenant_id)
            .bind(input.facility_id)
            .fetch_optional(&mut *conn)
            .await?;
    if facility_ok.is_none() {
        return Err(ApiError::not_found());
    }
    if !input.goals.is_array() || !input.barriers.is_array() {
        return Err(ApiError::bad_request(
            "invalid_shape",
            "goals and barriers must be arrays",
        ));
    }
    let id = Uuid::now_v7();
    let status = if input.activate {
        EpisodeStatus::Active
    } else {
        EpisodeStatus::Planned
    };
    let started_at = input.activate.then(Utc::now);
    sqlx::query(
        "INSERT INTO care_episodes
         (id, tenant_id, facility_id, patient_id, episode_type_code, status, responsible_service_code,
          responsible_queue_id, responsible_user_id, started_at, expected_end_at, priority, goals, barriers,
          provenance, created_by)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16)",
    )
    .bind(id)
    .bind(ctx.tenant_id)
    .bind(input.facility_id)
    .bind(input.patient_id)
    .bind(&input.episode_type_code)
    .bind(status.as_str())
    .bind(&input.responsible_service_code)
    .bind(input.responsible_queue_id)
    .bind(input.responsible_user_id)
    .bind(started_at)
    .bind(input.expected_end_at)
    .bind(priority)
    .bind(&input.goals)
    .bind(&input.barriers)
    .bind(json!({"created_by": ctx.user_id, "correlation_id": ctx.correlation_id}))
    .bind(ctx.user_id)
    .execute(&mut *conn)
    .await?;
    record_episode_history(
        conn,
        ctx,
        id,
        None,
        status.as_str(),
        "created",
        None,
        None,
        1,
    )
    .await?;
    let ep = load_episode(conn, ctx.tenant_id, id, false).await?;
    for l in &input.links {
        add_link(conn, ctx, &ep, l).await?;
    }
    Ok(ep)
}

#[derive(Debug, Deserialize)]
pub struct EpisodeTransitionInput {
    pub transition: String,
    pub expected_version: i64,
    pub reason_code: Option<String>,
    pub note: Option<String>,
}

pub async fn transition_episode(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    id: Uuid,
    input: &EpisodeTransitionInput,
) -> Result<EpisodeRow, ApiError> {
    let ep = load_episode(conn, ctx.tenant_id, id, true).await?;
    if ep.version != input.expected_version {
        return Err(stale("episode"));
    }
    let t = EpisodeTransition::parse(&input.transition)
        .ok_or_else(|| ApiError::bad_request("invalid_transition", "unknown transition"))?;
    let from = ep.status_enum();
    let to = from.apply(t).map_err(invalid_transition)?;
    if to == EpisodeStatus::DischargeReady {
        let blocking_tasks: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM care_coordination_tasks
             WHERE episode_id = $1 AND kind = 'discharge_preparation'
               AND status IN ('open','in_progress','blocked','handed_off')",
        )
        .bind(id)
        .fetch_one(&mut *conn)
        .await?;
        let blockers =
            domain::discharge_ready_blockers(ep.open_barriers(), blocking_tasks as usize);
        if !blockers.is_empty() {
            return Err(ApiError::conflict(
                "discharge_blocked",
                format!("operational readiness blocked: {}", blockers.join("; ")),
            ));
        }
    }
    if to.is_terminal() {
        let live: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM placements WHERE episode_id = $1 AND status IN ('held','occupied')",
        )
        .bind(id)
        .fetch_one(&mut *conn)
        .await?;
        if live > 0 {
            return Err(ApiError::conflict(
                "live_placement",
                "release the current placement before closing the episode",
            ));
        }
        sqlx::query(
            "UPDATE placement_requests SET status = 'cancelled', version = version + 1, updated_at = now()
             WHERE episode_id = $1 AND status IN ('requested','candidates_ready','held')",
        )
        .bind(id)
        .execute(&mut *conn)
        .await?;
    }
    let started_at = if to == EpisodeStatus::Active && ep.started_at.is_none() {
        Some(Utc::now())
    } else {
        ep.started_at
    };
    let ended_at = to.is_terminal().then(Utc::now);
    sqlx::query(
        "UPDATE care_episodes SET status = $3, started_at = $4, ended_at = $5, version = version + 1, updated_at = now()
         WHERE tenant_id = $1 AND id = $2",
    )
    .bind(ctx.tenant_id)
    .bind(id)
    .bind(to.as_str())
    .bind(started_at)
    .bind(ended_at)
    .execute(&mut *conn)
    .await?;
    record_episode_history(
        conn,
        ctx,
        id,
        Some(from.as_str()),
        to.as_str(),
        "transition",
        input.reason_code.as_deref(),
        input.note.as_deref(),
        ep.version + 1,
    )
    .await?;
    load_episode(conn, ctx.tenant_id, id, false).await
}

#[derive(Debug, Deserialize)]
pub struct EpisodeUpdateInput {
    pub expected_version: i64,
    pub goals: Option<Value>,
    pub barriers: Option<Value>,
    pub expected_end_at: Option<DateTime<Utc>>,
    pub responsible_service_code: Option<String>,
    pub responsible_queue_id: Option<Uuid>,
    pub responsible_user_id: Option<Uuid>,
    pub priority: Option<String>,
    pub note: Option<String>,
}

pub async fn update_episode(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    id: Uuid,
    input: &EpisodeUpdateInput,
) -> Result<EpisodeRow, ApiError> {
    let ep = load_episode(conn, ctx.tenant_id, id, true).await?;
    if ep.version != input.expected_version {
        return Err(stale("episode"));
    }
    if ep.status_enum().is_terminal() {
        return Err(ApiError::conflict(
            "episode_closed",
            "closed episodes are immutable",
        ));
    }
    if let Some(p) = &input.priority {
        if !PRIORITIES.contains(&p.as_str()) {
            return Err(ApiError::bad_request(
                "invalid_priority",
                "unknown priority",
            ));
        }
    }
    for v in [&input.goals, &input.barriers].into_iter().flatten() {
        if !v.is_array() {
            return Err(ApiError::bad_request(
                "invalid_shape",
                "goals and barriers must be arrays",
            ));
        }
    }
    sqlx::query(
        "UPDATE care_episodes SET goals = COALESCE($3, goals), barriers = COALESCE($4, barriers),
             expected_end_at = COALESCE($5, expected_end_at),
             responsible_service_code = COALESCE($6, responsible_service_code),
             responsible_queue_id = COALESCE($7, responsible_queue_id),
             responsible_user_id = COALESCE($8, responsible_user_id),
             priority = COALESCE($9, priority),
             version = version + 1, updated_at = now()
         WHERE tenant_id = $1 AND id = $2",
    )
    .bind(ctx.tenant_id)
    .bind(id)
    .bind(&input.goals)
    .bind(&input.barriers)
    .bind(input.expected_end_at)
    .bind(&input.responsible_service_code)
    .bind(input.responsible_queue_id)
    .bind(input.responsible_user_id)
    .bind(&input.priority)
    .execute(&mut *conn)
    .await?;
    record_episode_history(
        conn,
        ctx,
        id,
        Some(&ep.status),
        &ep.status,
        "updated",
        None,
        input.note.as_deref(),
        ep.version + 1,
    )
    .await?;
    load_episode(conn, ctx.tenant_id, id, false).await
}

pub async fn episode_history(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    episode_id: Uuid,
) -> Result<Vec<Value>, ApiError> {
    let rows = sqlx::query(
        "SELECT from_status, to_status, change_kind, reason_code, note, actor, version, recorded_at
         FROM care_episode_history WHERE tenant_id = $1 AND episode_id = $2 ORDER BY recorded_at, version",
    )
    .bind(tenant_id)
    .bind(episode_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .iter()
        .map(|r| {
            json!({
                "from_status": r.get::<Option<String>, _>("from_status"),
                "to_status": r.get::<String, _>("to_status"),
                "change_kind": r.get::<String, _>("change_kind"),
                "reason_code": r.get::<Option<String>, _>("reason_code"),
                "note": r.get::<Option<String>, _>("note"),
                "actor": r.get::<String, _>("actor"),
                "version": r.get::<i64, _>("version"),
                "recorded_at": r.get::<DateTime<Utc>, _>("recorded_at"),
            })
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Placement requests, candidates, placements
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct PlacementRequestRow {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub facility_id: Uuid,
    pub episode_id: Uuid,
    pub patient_id: Uuid,
    pub kind: String,
    pub status: String,
    pub priority: String,
    pub requirements: Value,
    pub requested_from: DateTime<Utc>,
    pub reason_code: Option<String>,
    pub note: Option<String>,
    pub matcher_version: Option<String>,
    pub facts_hash: Option<String>,
    pub ranking_artifact_id: Option<Uuid>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

const REQUEST_COLS: &str = "id, tenant_id, facility_id, episode_id, patient_id, kind, status, priority,
    requirements, requested_from, reason_code, note, matcher_version, facts_hash, ranking_artifact_id,
    version, created_at, updated_at";

fn request_from_row(r: &PgRow) -> PlacementRequestRow {
    PlacementRequestRow {
        id: r.get("id"),
        tenant_id: r.get("tenant_id"),
        facility_id: r.get("facility_id"),
        episode_id: r.get("episode_id"),
        patient_id: r.get("patient_id"),
        kind: r.get("kind"),
        status: r.get("status"),
        priority: r.get("priority"),
        requirements: r.get("requirements"),
        requested_from: r.get("requested_from"),
        reason_code: r.get("reason_code"),
        note: r.get("note"),
        matcher_version: r.get("matcher_version"),
        facts_hash: r.get("facts_hash"),
        ranking_artifact_id: r.get("ranking_artifact_id"),
        version: r.get("version"),
        created_at: r.get("created_at"),
        updated_at: r.get("updated_at"),
    }
}

impl PlacementRequestRow {
    pub fn status_enum(&self) -> PlacementRequestStatus {
        PlacementRequestStatus::parse(&self.status).unwrap_or(PlacementRequestStatus::Cancelled)
    }
    pub fn requirements_typed(&self) -> PlacementRequirements {
        serde_json::from_value(self.requirements.clone()).unwrap_or_default()
    }
}

pub async fn load_request(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    id: Uuid,
    lock: bool,
) -> Result<PlacementRequestRow, ApiError> {
    let sql = format!(
        "SELECT {REQUEST_COLS} FROM placement_requests WHERE tenant_id = $1 AND id = $2 {}",
        if lock { "FOR UPDATE" } else { "" }
    );
    let row = sqlx::query(&sql)
        .bind(tenant_id)
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(ApiError::not_found)?;
    Ok(request_from_row(&row))
}

pub async fn list_requests(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    facility_ids: Option<&[Uuid]>,
    episode_id: Option<Uuid>,
    open_only: bool,
    limit: i64,
) -> Result<Vec<PlacementRequestRow>, ApiError> {
    let sql = format!(
        "SELECT {REQUEST_COLS} FROM placement_requests
         WHERE tenant_id = $1 AND ($2::uuid[] IS NULL OR facility_id = ANY($2))
           AND ($3::uuid IS NULL OR episode_id = $3)
           AND (NOT $4 OR status IN ('requested','candidates_ready','held'))
         ORDER BY CASE priority WHEN 'emergency' THEN 0 WHEN 'urgent' THEN 1 WHEN 'priority' THEN 2 ELSE 3 END,
                  requested_from
         LIMIT $5"
    );
    let rows = sqlx::query(&sql)
        .bind(tenant_id)
        .bind(facility_ids)
        .bind(episode_id)
        .bind(open_only)
        .bind(limit)
        .fetch_all(&mut *conn)
        .await?;
    Ok(rows.iter().map(request_from_row).collect())
}

#[derive(Debug, Deserialize)]
pub struct PlacementRequestInput {
    pub episode_id: Uuid,
    pub kind: Option<String>,
    pub priority: Option<String>,
    #[serde(default)]
    pub requirements: PlacementRequirements,
    pub reason_code: Option<String>,
    pub note: Option<String>,
}

#[allow(clippy::too_many_arguments)]
async fn record_placement_history(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    placement_id: Option<Uuid>,
    request_id: Option<Uuid>,
    from: Option<&str>,
    to: &str,
    reason: Option<&str>,
    note: Option<&str>,
    version: i64,
) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO placement_history
         (id, tenant_id, placement_id, request_id, from_status, to_status, reason_code, note, actor, version)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
    )
    .bind(Uuid::now_v7())
    .bind(ctx.tenant_id)
    .bind(placement_id)
    .bind(request_id)
    .bind(from)
    .bind(to)
    .bind(reason)
    .bind(note)
    .bind(actor(ctx))
    .bind(version)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

pub async fn create_request(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    input: &PlacementRequestInput,
) -> Result<PlacementRequestRow, ApiError> {
    let ep = load_episode(conn, ctx.tenant_id, input.episode_id, true).await?;
    if !ep.status_enum().is_open() {
        return Err(ApiError::conflict(
            "episode_closed",
            "the episode is not open",
        ));
    }
    let kind = input.kind.as_deref().unwrap_or("admission");
    if !["admission", "transfer"].contains(&kind) {
        return Err(ApiError::bad_request(
            "invalid_kind",
            "kind must be admission or transfer",
        ));
    }
    if kind == "transfer" && ep.current_placement_id.is_none() {
        return Err(ApiError::conflict(
            "no_current_placement",
            "a transfer needs a current placement",
        ));
    }
    if kind == "admission" && ep.current_placement_id.is_some() {
        return Err(ApiError::conflict(
            "already_placed",
            "the episode already occupies a position; request a transfer",
        ));
    }
    let open: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM placement_requests WHERE episode_id = $1 AND status IN ('requested','candidates_ready','held')",
    )
    .bind(ep.id)
    .fetch_one(&mut *conn)
    .await?;
    if open > 0 {
        return Err(ApiError::conflict(
            "open_request_exists",
            "the episode already has an open placement request",
        ));
    }
    let priority = input.priority.as_deref().unwrap_or(&ep.priority);
    if !PRIORITIES.contains(&priority) {
        return Err(ApiError::bad_request(
            "invalid_priority",
            "unknown priority",
        ));
    }
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO placement_requests
         (id, tenant_id, facility_id, episode_id, patient_id, kind, status, priority, requirements, reason_code, note, created_by)
         VALUES ($1,$2,$3,$4,$5,$6,'requested',$7,$8,$9,$10,$11)",
    )
    .bind(id)
    .bind(ctx.tenant_id)
    .bind(ep.facility_id)
    .bind(ep.id)
    .bind(ep.patient_id)
    .bind(kind)
    .bind(priority)
    .bind(serde_json::to_value(&input.requirements).unwrap_or(json!({})))
    .bind(&input.reason_code)
    .bind(&input.note)
    .bind(ctx.user_id)
    .execute(&mut *conn)
    .await?;
    record_placement_history(
        conn,
        ctx,
        None,
        Some(id),
        None,
        "requested",
        input.reason_code.as_deref(),
        None,
        1,
    )
    .await?;
    load_request(conn, ctx.tenant_id, id, false).await
}

/// Load the deterministic facts for every position of the request's
/// facility as they stand now, including live placements, blocking periods,
/// room occupancy and staffed professions on the current shift.
pub async fn load_placement_facts(
    conn: &mut PgConnection,
    req: &PlacementRequestRow,
    ep: &EpisodeRow,
    now: DateTime<Utc>,
) -> Result<PlacementFacts, ApiError> {
    let policy = scheduling::load_policy(conn, req.tenant_id).await?;
    let closure = scheduling::active_closure_over(
        conn,
        req.tenant_id,
        req.facility_id,
        now,
        now + Duration::minutes(1),
        &policy.time_zone,
    )
    .await?;
    let tz_name: Option<String> =
        sqlx::query_scalar("SELECT time_zone FROM facility_scheduling WHERE facility_id = $1")
            .bind(req.facility_id)
            .fetch_optional(&mut *conn)
            .await?;
    let tz: chrono_tz::Tz = tz_name
        .as_deref()
        .unwrap_or(&policy.time_zone)
        .parse()
        .unwrap_or(chrono_tz::UTC);
    let local_date = now.with_timezone(&tz).date_naive();
    let rows = sqlx::query(
        "WITH RECURSIVE tree AS (
            SELECT l.id, l.parent_id, l.id AS root_id, l.level,
                   l.specialty_codes, l.service_line_codes, l.age_group_codes, l.isolation_codes,
                   l.accessibility_codes, l.equipment_codes,
                   (l.active AND l.operational_status = 'open') AS open_here, 0 AS depth
            FROM operational_locations l WHERE l.tenant_id = $1 AND l.facility_id = $2 AND l.level = 'position'
            UNION ALL
            SELECT p.id, p.parent_id, t.root_id, p.level,
                   t.specialty_codes || p.specialty_codes, t.service_line_codes || p.service_line_codes,
                   t.age_group_codes || p.age_group_codes, t.isolation_codes || p.isolation_codes,
                   t.accessibility_codes || p.accessibility_codes, t.equipment_codes || p.equipment_codes,
                   (t.open_here AND p.active AND p.operational_status = 'open'), t.depth + 1
            FROM operational_locations p JOIN tree t ON p.id = t.parent_id
         ), agg AS (
            SELECT DISTINCT ON (root_id) root_id, specialty_codes, service_line_codes, age_group_codes,
                   isolation_codes, accessibility_codes, equipment_codes, open_here
            FROM tree ORDER BY root_id, depth DESC
         )
         SELECT pos.id, pos.facility_id, pos.name_en AS position_name, pos.type_code, pos.position_state,
                pos.active, pos.effective_from, pos.effective_to, pos.cleaning_minutes, pos.room_policy,
                room.id AS room_id, room.name_en AS room_name, room.capacity AS room_capacity,
                room.room_policy AS room_room_policy,
                unit.id AS unit_id, COALESCE(unit.name_en, '') AS unit_name,
                agg.specialty_codes, agg.service_line_codes, agg.age_group_codes, agg.isolation_codes,
                agg.accessibility_codes, agg.equipment_codes, agg.open_here,
                EXISTS (SELECT 1 FROM placements pl WHERE pl.location_id = pos.id AND pl.status IN ('held','occupied')) AS live_placement,
                (SELECT per.kind FROM operational_location_periods per
                  WHERE per.active AND per.location_id IN (pos.id, room.id, unit.id)
                    AND per.starts_at <= $3 AND (per.ends_at IS NULL OR per.ends_at > $3)
                  ORDER BY per.starts_at LIMIT 1) AS blocking_period,
                COALESCE((SELECT array_agg(COALESCE(pt.sex, 'unknown'))
                  FROM placements pl2 JOIN operational_locations sib ON sib.id = pl2.location_id
                  JOIN patients pt ON pt.id = pl2.patient_id
                  WHERE pl2.status = 'occupied' AND sib.parent_id = room.id AND sib.id <> pos.id), '{}') AS room_occupant_sexes,
                (SELECT count(*) FROM placements pl3 JOIN operational_locations sib2 ON sib2.id = pl3.location_id
                  WHERE pl3.status IN ('held','occupied') AND sib2.parent_id = room.id AND sib2.id <> pos.id) AS room_occupant_count,
                (SELECT count(*) FROM operational_locations u WHERE u.level = 'position' AND u.active
                   AND u.id IN (SELECT id FROM tree WHERE root_id IN (
                        SELECT t2.root_id FROM tree t2 WHERE t2.id = unit.id)) ) AS unit_positions,
                (SELECT count(*) FROM placements plu JOIN tree tu ON tu.root_id = plu.location_id
                   WHERE tu.id = unit.id AND plu.status IN ('held','occupied')) AS unit_live,
                COALESCE((SELECT array_agg(DISTINCT a.profession_code) FROM workforce_shift_assignments a
                   JOIN workforce_shifts s ON s.id = a.shift_id
                   WHERE s.unit_location_id = unit.id AND s.status IN ('planned','active')
                     AND s.starts_at <= $3 AND s.ends_at > $3
                     AND a.status IN ('planned','confirmed')
                     AND NOT EXISTS (SELECT 1 FROM workforce_shift_absences ab
                                     WHERE ab.user_id = a.user_id AND ab.starts_at <= $3 AND ab.ends_at > $3)), '{}') AS staffed_professions
         FROM operational_locations pos
         JOIN agg ON agg.root_id = pos.id
         LEFT JOIN operational_locations room ON room.id = pos.parent_id AND room.level = 'room'
         LEFT JOIN operational_locations unit ON unit.id = COALESCE(room.parent_id, pos.parent_id) AND unit.level = 'unit'
         WHERE pos.tenant_id = $1 AND pos.facility_id = $2 AND pos.level = 'position'
         ORDER BY unit.code, room.code, pos.code",
    )
    .bind(req.tenant_id)
    .bind(req.facility_id)
    .bind(now)
    .fetch_all(&mut *conn)
    .await?;
    let positions = rows
        .iter()
        .map(|r| {
            let unit_positions: i64 = r.get("unit_positions");
            let unit_live: i64 = r.get("unit_live");
            let room_policy: Option<String> = r.get("room_room_policy");
            PositionFact {
                location_id: r.get("id"),
                facility_id: r.get("facility_id"),
                unit_id: r.get("unit_id"),
                room_id: r.get("room_id"),
                unit_name: r.get("unit_name"),
                room_name: r.get("room_name"),
                position_name: r.get("position_name"),
                type_code: r.get("type_code"),
                state: r
                    .get::<Option<String>, _>("position_state")
                    .as_deref()
                    .and_then(PositionState::parse)
                    .unwrap_or(PositionState::OutOfService),
                active: r.get("active"),
                hierarchy_open: r.get("open_here"),
                effective_from: r.get("effective_from"),
                effective_to: r.get("effective_to"),
                specialty_codes: dedup(r.get("specialty_codes")),
                service_line_codes: dedup(r.get("service_line_codes")),
                age_group_codes: dedup(r.get("age_group_codes")),
                isolation_codes: dedup(r.get("isolation_codes")),
                accessibility_codes: dedup(r.get("accessibility_codes")),
                equipment_codes: dedup(r.get("equipment_codes")),
                room_policy: room_policy.unwrap_or_else(|| r.get("room_policy")),
                room_occupant_sexes: r.get("room_occupant_sexes"),
                room_occupant_count: r.get::<i64, _>("room_occupant_count") as usize,
                room_capacity: r.get::<Option<i32>, _>("room_capacity").unwrap_or(1).max(1)
                    as usize,
                live_placement: r.get("live_placement"),
                blocking_period: r.get("blocking_period"),
                staffed_professions: r.get("staffed_professions"),
                unit_occupancy: if unit_positions > 0 {
                    (unit_live as f64 / unit_positions as f64).clamp(0.0, 1.0)
                } else {
                    0.0
                },
                cleaning_minutes: r.get::<i32, _>("cleaning_minutes") as i64,
            }
        })
        .collect();
    let mut requirements = req.requirements_typed();
    if requirements.sex.is_none() {
        requirements.sex = sqlx::query_scalar("SELECT sex FROM patients WHERE id = $1")
            .bind(req.patient_id)
            .fetch_optional(&mut *conn)
            .await?
            .flatten();
    }
    let current_location_id = match ep.current_placement_id {
        Some(pid) => {
            sqlx::query_scalar("SELECT location_id FROM placements WHERE id = $1")
                .bind(pid)
                .fetch_optional(&mut *conn)
                .await?
        }
        None => None,
    };
    Ok(PlacementFacts {
        tenant_id: req.tenant_id,
        facility_id: req.facility_id,
        facility_closed: closure.is_some(),
        local_date,
        requirements,
        positions,
        current_location_id,
    })
}

fn dedup(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v.dedup();
    v
}

/// Run the matcher and persist every candidate (feasible and rejected) with
/// its score decomposition, source facts and facts hash as a new generation.
pub async fn generate_candidates(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    request_id: Uuid,
    expected_version: Option<i64>,
) -> Result<(PlacementRequestRow, PlacementMatch, i32), ApiError> {
    let req = load_request(conn, ctx.tenant_id, request_id, true).await?;
    if let Some(v) = expected_version {
        if v != req.version {
            return Err(stale("placement request"));
        }
    }
    let from = req.status_enum();
    let to = from
        .apply(PlacementRequestTransition::CandidatesGenerated)
        .map_err(invalid_transition)?;
    let ep = load_episode(conn, ctx.tenant_id, req.episode_id, false).await?;
    let facts = load_placement_facts(conn, &req, &ep, Utc::now()).await?;
    let out = domain::match_placements(&facts);
    let generation: i32 = sqlx::query_scalar(
        "SELECT COALESCE(max(generation), 0) + 1 FROM placement_candidates WHERE request_id = $1",
    )
    .bind(req.id)
    .fetch_one(&mut *conn)
    .await?;
    let source_facts = json!({
        "facility_closed": facts.facility_closed,
        "local_date": facts.local_date,
        "requirements": facts.requirements,
        "position_count": facts.positions.len(),
    });
    for c in out.feasible.iter().chain(out.rejected.iter()) {
        sqlx::query(
            "INSERT INTO placement_candidates
             (id, tenant_id, request_id, location_id, matcher_version, generation, rank, feasible, score,
              score_factors, rejection_codes, source_facts, facts_hash)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)",
        )
        .bind(Uuid::now_v7())
        .bind(ctx.tenant_id)
        .bind(req.id)
        .bind(c.location_id)
        .bind(&out.matcher_version)
        .bind(generation)
        .bind(c.rank.map(|r| r as i32))
        .bind(c.feasible)
        .bind(c.score)
        .bind(serde_json::to_value(&c.factors).unwrap_or(json!([])))
        .bind(&c.rejection_codes)
        .bind(&source_facts)
        .bind(&out.facts_hash)
        .execute(&mut *conn)
        .await?;
    }
    sqlx::query(
        "UPDATE placement_requests SET status = $3, matcher_version = $4, facts_hash = $5,
             version = version + 1, updated_at = now()
         WHERE tenant_id = $1 AND id = $2",
    )
    .bind(ctx.tenant_id)
    .bind(req.id)
    .bind(to.as_str())
    .bind(&out.matcher_version)
    .bind(&out.facts_hash)
    .execute(&mut *conn)
    .await?;
    if from != to {
        record_placement_history(
            conn,
            ctx,
            None,
            Some(req.id),
            Some(from.as_str()),
            to.as_str(),
            None,
            None,
            req.version + 1,
        )
        .await?;
    }
    let req = load_request(conn, ctx.tenant_id, req.id, false).await?;
    Ok((req, out, generation))
}

#[derive(Debug, Clone, Serialize)]
pub struct CandidateRow {
    pub id: Uuid,
    pub location_id: Uuid,
    pub generation: i32,
    pub rank: Option<i32>,
    pub feasible: bool,
    pub score: Option<f64>,
    pub score_factors: Value,
    pub rejection_codes: Vec<String>,
    pub facts_hash: String,
    pub label: String,
    pub unit_id: Option<Uuid>,
}

pub async fn latest_candidates(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    request_id: Uuid,
) -> Result<Vec<CandidateRow>, ApiError> {
    let rows = sqlx::query(
        "SELECT c.id, c.location_id, c.generation, c.rank, c.feasible, c.score, c.score_factors, c.rejection_codes, c.facts_hash,
                pos.name_en AS position_name, room.name_en AS room_name, unit.name_en AS unit_name, unit.id AS unit_id
         FROM placement_candidates c
         JOIN operational_locations pos ON pos.id = c.location_id
         LEFT JOIN operational_locations room ON room.id = pos.parent_id AND room.level = 'room'
         LEFT JOIN operational_locations unit ON unit.id = COALESCE(room.parent_id, pos.parent_id) AND unit.level = 'unit'
         WHERE c.tenant_id = $1 AND c.request_id = $2
           AND c.generation = (SELECT max(generation) FROM placement_candidates WHERE request_id = $2)
         ORDER BY c.feasible DESC, c.rank NULLS LAST, pos.code",
    )
    .bind(tenant_id)
    .bind(request_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .iter()
        .map(|r| {
            let unit: Option<String> = r.get("unit_name");
            let room: Option<String> = r.get("room_name");
            let pos: String = r.get("position_name");
            let label = [unit, room, Some(pos)]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" / ");
            CandidateRow {
                id: r.get("id"),
                location_id: r.get("location_id"),
                generation: r.get("generation"),
                rank: r.get("rank"),
                feasible: r.get("feasible"),
                score: r.get("score"),
                score_factors: r.get("score_factors"),
                rejection_codes: r.get("rejection_codes"),
                facts_hash: r.get("facts_hash"),
                label,
                unit_id: r.get("unit_id"),
            }
        })
        .collect())
}

#[derive(Debug, Clone, Serialize)]
pub struct PlacementRow {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub facility_id: Uuid,
    pub episode_id: Uuid,
    pub request_id: Option<Uuid>,
    pub candidate_id: Option<Uuid>,
    pub location_id: Uuid,
    pub patient_id: Uuid,
    pub status: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub facts_hash: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

const PLACEMENT_COLS: &str =
    "id, tenant_id, facility_id, episode_id, request_id, candidate_id, location_id, patient_id,
    status, starts_at, ends_at, expires_at, facts_hash, version, created_at";

fn placement_from_row(r: &PgRow) -> PlacementRow {
    PlacementRow {
        id: r.get("id"),
        tenant_id: r.get("tenant_id"),
        facility_id: r.get("facility_id"),
        episode_id: r.get("episode_id"),
        request_id: r.get("request_id"),
        candidate_id: r.get("candidate_id"),
        location_id: r.get("location_id"),
        patient_id: r.get("patient_id"),
        status: r.get("status"),
        starts_at: r.get("starts_at"),
        ends_at: r.get("ends_at"),
        expires_at: r.get("expires_at"),
        facts_hash: r.get("facts_hash"),
        version: r.get("version"),
        created_at: r.get("created_at"),
    }
}

pub async fn load_placement(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    id: Uuid,
    lock: bool,
) -> Result<PlacementRow, ApiError> {
    let sql = format!(
        "SELECT {PLACEMENT_COLS} FROM placements WHERE tenant_id = $1 AND id = $2 {}",
        if lock { "FOR UPDATE" } else { "" }
    );
    let row = sqlx::query(&sql)
        .bind(tenant_id)
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(ApiError::not_found)?;
    Ok(placement_from_row(&row))
}

pub async fn episode_placements(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    episode_id: Uuid,
) -> Result<Vec<PlacementRow>, ApiError> {
    let sql = format!(
        "SELECT {PLACEMENT_COLS} FROM placements WHERE tenant_id = $1 AND episode_id = $2 ORDER BY starts_at"
    );
    let rows = sqlx::query(&sql)
        .bind(tenant_id)
        .bind(episode_id)
        .fetch_all(&mut *conn)
        .await?;
    Ok(rows.iter().map(placement_from_row).collect())
}

#[derive(Debug, Deserialize)]
pub struct HoldInput {
    pub location_id: Uuid,
    pub expected_version: i64,
    /// Facts hash of the candidate set the user saw; a mismatch is a stale
    /// decision and is refused.
    pub facts_hash: String,
    pub hold_minutes: Option<i64>,
}

/// Re-run the hard constraints for one position inside the transaction so
/// a decision made on stale candidates (position since occupied, cleaning,
/// closed, facility closure created) is refused with a typed 409.
async fn revalidate_position(
    conn: &mut PgConnection,
    req: &PlacementRequestRow,
    ep: &EpisodeRow,
    location_id: Uuid,
) -> Result<(PositionFact, PlacementCandidate, PlacementFacts), ApiError> {
    let facts = load_placement_facts(conn, req, ep, Utc::now()).await?;
    let pos = facts
        .positions
        .iter()
        .find(|p| p.location_id == location_id)
        .cloned()
        .ok_or_else(|| {
            ApiError::bad_request("unknown_position", "the position is not in this facility")
        })?;
    let out = domain::match_placements(&facts);
    if let Some(c) = out.feasible.iter().find(|c| c.location_id == location_id) {
        return Ok((pos, c.clone(), facts));
    }
    let rejected = out
        .rejected
        .iter()
        .find(|c| c.location_id == location_id)
        .map(|c| c.rejection_codes.join(","))
        .unwrap_or_else(|| "infeasible".into());
    if facts.facility_closed {
        return Err(scheduling::facility_closed());
    }
    Err(ApiError::conflict(
        "placement_infeasible",
        format!("the position no longer satisfies the hard constraints: {rejected}"),
    ))
}

/// Time-limited hold on a feasible position (human action).
pub async fn hold(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    request_id: Uuid,
    input: &HoldInput,
) -> Result<(PlacementRequestRow, PlacementRow), ApiError> {
    let req = load_request(conn, ctx.tenant_id, request_id, true).await?;
    if req.version != input.expected_version {
        return Err(stale("placement request"));
    }
    if req.facts_hash.as_deref() != Some(input.facts_hash.as_str()) {
        return Err(ApiError::conflict(
            "stale_candidates",
            "the candidate set changed; regenerate candidates before deciding",
        ));
    }
    let to = req
        .status_enum()
        .apply(PlacementRequestTransition::Hold)
        .map_err(invalid_transition)?;
    let ep = load_episode(conn, ctx.tenant_id, req.episode_id, true).await?;
    let (_, _, facts) = revalidate_position(conn, &req, &ep, input.location_id).await?;
    let candidate_id = latest_candidate_id(conn, req.id, input.location_id).await?;
    let now = Utc::now();
    let expires = domain::hold_expiry(now, input.hold_minutes);
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO placements (id, tenant_id, facility_id, episode_id, request_id, candidate_id, location_id, patient_id,
             status, starts_at, expires_at, facts_hash, created_by)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,'held',$9,$10,$11,$12)",
    )
    .bind(id)
    .bind(ctx.tenant_id)
    .bind(req.facility_id)
    .bind(req.episode_id)
    .bind(req.id)
    .bind(candidate_id)
    .bind(input.location_id)
    .bind(req.patient_id)
    .bind(now)
    .bind(expires)
    .bind(domain::hash_facts(&serde_json::to_value(&facts).unwrap_or(Value::Null)))
    .bind(ctx.user_id)
    .execute(&mut *conn)
    .await
    .map_err(map_unique("overlapping_placement", "the position is already held or occupied"))?;
    transition_position(conn, ctx, input.location_id, PositionState::Held, "hold").await?;
    sqlx::query(
        "UPDATE placement_requests SET status = $3, version = version + 1, updated_at = now() WHERE tenant_id = $1 AND id = $2",
    )
    .bind(ctx.tenant_id)
    .bind(req.id)
    .bind(to.as_str())
    .execute(&mut *conn)
    .await?;
    record_placement_history(
        conn,
        ctx,
        Some(id),
        Some(req.id),
        None,
        "held",
        None,
        None,
        1,
    )
    .await?;
    record_placement_history(
        conn,
        ctx,
        None,
        Some(req.id),
        Some(&req.status),
        to.as_str(),
        None,
        None,
        req.version + 1,
    )
    .await?;
    Ok((
        load_request(conn, ctx.tenant_id, req.id, false).await?,
        load_placement(conn, ctx.tenant_id, id, false).await?,
    ))
}

async fn latest_candidate_id(
    conn: &mut PgConnection,
    request_id: Uuid,
    location_id: Uuid,
) -> Result<Option<Uuid>, ApiError> {
    Ok(sqlx::query_scalar(
        "SELECT id FROM placement_candidates WHERE request_id = $1 AND location_id = $2 AND feasible
         ORDER BY generation DESC LIMIT 1",
    )
    .bind(request_id)
    .bind(location_id)
    .fetch_optional(&mut *conn)
    .await?)
}

async fn live_hold_for_request(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    request_id: Uuid,
) -> Result<Option<PlacementRow>, ApiError> {
    let sql = format!(
        "SELECT {PLACEMENT_COLS} FROM placements WHERE tenant_id = $1 AND request_id = $2 AND status = 'held' FOR UPDATE"
    );
    Ok(sqlx::query(&sql)
        .bind(tenant_id)
        .bind(request_id)
        .fetch_optional(&mut *conn)
        .await?
        .as_ref()
        .map(placement_from_row))
}

async fn end_placement(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    p: &PlacementRow,
    t: PlacementTransition,
    reason: Option<&str>,
    next_position_state: PositionState,
) -> Result<PlacementRow, ApiError> {
    let from = PlacementStatus::parse(&p.status).unwrap_or(PlacementStatus::Revoked);
    let to = from.apply(t).map_err(invalid_transition)?;
    sqlx::query(
        "UPDATE placements SET status = $2, ends_at = GREATEST($3, starts_at), version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(p.id)
    .bind(to.as_str())
    .bind(Utc::now())
    .execute(&mut *conn)
    .await?;
    transition_position(conn, ctx, p.location_id, next_position_state, to.as_str()).await?;
    record_placement_history(
        conn,
        ctx,
        Some(p.id),
        p.request_id,
        Some(from.as_str()),
        to.as_str(),
        reason,
        None,
        p.version + 1,
    )
    .await?;
    load_placement(conn, ctx.tenant_id, p.id, false).await
}

#[derive(Debug, Deserialize)]
pub struct ReleaseHoldInput {
    pub expected_version: i64,
    pub reason_code: Option<String>,
}

pub async fn release_hold(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    request_id: Uuid,
    input: &ReleaseHoldInput,
) -> Result<PlacementRequestRow, ApiError> {
    let req = load_request(conn, ctx.tenant_id, request_id, true).await?;
    if req.version != input.expected_version {
        return Err(stale("placement request"));
    }
    let to = req
        .status_enum()
        .apply(PlacementRequestTransition::ReleaseHold)
        .map_err(invalid_transition)?;
    if let Some(h) = live_hold_for_request(conn, ctx.tenant_id, req.id).await? {
        end_placement(
            conn,
            ctx,
            &h,
            PlacementTransition::Revoke,
            input.reason_code.as_deref(),
            PositionState::Available,
        )
        .await?;
    }
    sqlx::query(
        "UPDATE placement_requests SET status = $3, version = version + 1, updated_at = now() WHERE tenant_id = $1 AND id = $2",
    )
    .bind(ctx.tenant_id)
    .bind(req.id)
    .bind(to.as_str())
    .execute(&mut *conn)
    .await?;
    record_placement_history(
        conn,
        ctx,
        None,
        Some(req.id),
        Some(&req.status),
        to.as_str(),
        input.reason_code.as_deref(),
        None,
        req.version + 1,
    )
    .await?;
    load_request(conn, ctx.tenant_id, req.id, false).await
}

#[derive(Debug, Deserialize)]
pub struct ConfirmInput {
    pub expected_version: i64,
    /// Required when confirming without a hold.
    pub location_id: Option<Uuid>,
    pub facts_hash: Option<String>,
    pub note: Option<String>,
}

/// Human confirmation: the hold (or a fresh feasible candidate) becomes an
/// occupied placement; a transfer releases the previous position into
/// cleaning. Everything is revalidated inside the transaction.
pub async fn confirm(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    request_id: Uuid,
    input: &ConfirmInput,
) -> Result<(PlacementRequestRow, PlacementRow, EpisodeRow), ApiError> {
    let req = load_request(conn, ctx.tenant_id, request_id, true).await?;
    if req.version != input.expected_version {
        return Err(stale("placement request"));
    }
    let to = req
        .status_enum()
        .apply(PlacementRequestTransition::Confirm)
        .map_err(invalid_transition)?;
    let ep = load_episode(conn, ctx.tenant_id, req.episode_id, true).await?;
    if !ep.status_enum().is_open() {
        return Err(ApiError::conflict(
            "episode_closed",
            "the episode is not open",
        ));
    }
    let now = Utc::now();
    let placement = match live_hold_for_request(conn, ctx.tenant_id, req.id).await? {
        Some(h) => {
            if h.expires_at.is_some_and(|e| e <= now) {
                end_placement(
                    conn,
                    ctx,
                    &h,
                    PlacementTransition::Expire,
                    Some("hold_expired"),
                    PositionState::Available,
                )
                .await?;
                return Err(ApiError::conflict(
                    "hold_expired",
                    "the hold expired; regenerate candidates and hold again",
                ));
            }
            if let Some(l) = input.location_id {
                if l != h.location_id {
                    return Err(ApiError::conflict(
                        "hold_mismatch",
                        "release the current hold before choosing another position",
                    ));
                }
            }
            // The held position is excluded from the matcher (it is held), so
            // re-check the non-occupancy constraints directly.
            let facts = load_placement_facts(conn, &req, &ep, now).await?;
            if facts.facility_closed {
                end_placement(
                    conn,
                    ctx,
                    &h,
                    PlacementTransition::Revoke,
                    Some("facility_closed"),
                    PositionState::Available,
                )
                .await?;
                return Err(scheduling::facility_closed());
            }
            let pos = facts
                .positions
                .iter()
                .find(|p| p.location_id == h.location_id)
                .cloned()
                .ok_or_else(ApiError::not_found)?;
            let mut probe = pos.clone();
            probe.state = PositionState::Available;
            probe.live_placement = false;
            let codes = domain::placement_rejections(&facts, &probe);
            if !codes.is_empty() {
                end_placement(
                    conn,
                    ctx,
                    &h,
                    PlacementTransition::Revoke,
                    Some("infeasible"),
                    PositionState::Available,
                )
                .await?;
                return Err(ApiError::conflict(
                    "placement_infeasible",
                    format!(
                        "the held position no longer satisfies the hard constraints: {}",
                        codes.join(",")
                    ),
                ));
            }
            sqlx::query(
                "UPDATE placements SET status = 'occupied', starts_at = $2, expires_at = NULL, version = version + 1, updated_at = now() WHERE id = $1",
            )
            .bind(h.id)
            .bind(now)
            .execute(&mut *conn)
            .await?;
            transition_position(conn, ctx, h.location_id, PositionState::Occupied, "confirm")
                .await?;
            record_placement_history(
                conn,
                ctx,
                Some(h.id),
                Some(req.id),
                Some("held"),
                "occupied",
                None,
                input.note.as_deref(),
                h.version + 1,
            )
            .await?;
            load_placement(conn, ctx.tenant_id, h.id, false).await?
        }
        None => {
            let location_id = input.location_id.ok_or_else(|| {
                ApiError::bad_request("location_required", "choose a position or hold one first")
            })?;
            if req.facts_hash.as_deref() != input.facts_hash.as_deref()
                || input.facts_hash.is_none()
            {
                return Err(ApiError::conflict(
                    "stale_candidates",
                    "the candidate set changed; regenerate candidates before deciding",
                ));
            }
            let (_, _, facts) = revalidate_position(conn, &req, &ep, location_id).await?;
            let candidate_id = latest_candidate_id(conn, req.id, location_id).await?;
            let id = Uuid::now_v7();
            sqlx::query(
                "INSERT INTO placements (id, tenant_id, facility_id, episode_id, request_id, candidate_id, location_id, patient_id,
                     status, starts_at, facts_hash, created_by)
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,'occupied',$9,$10,$11)",
            )
            .bind(id)
            .bind(ctx.tenant_id)
            .bind(req.facility_id)
            .bind(req.episode_id)
            .bind(req.id)
            .bind(candidate_id)
            .bind(location_id)
            .bind(req.patient_id)
            .bind(now)
            .bind(domain::hash_facts(&serde_json::to_value(&facts).unwrap_or(Value::Null)))
            .bind(ctx.user_id)
            .execute(&mut *conn)
            .await
            .map_err(map_unique("overlapping_placement", "the position is already held or occupied"))?;
            transition_position(conn, ctx, location_id, PositionState::Occupied, "confirm").await?;
            record_placement_history(
                conn,
                ctx,
                Some(id),
                Some(req.id),
                None,
                "occupied",
                None,
                input.note.as_deref(),
                1,
            )
            .await?;
            load_placement(conn, ctx.tenant_id, id, false).await?
        }
    };
    // Transfer: the previous position goes to cleaning.
    if let Some(prev_id) = ep.current_placement_id {
        if prev_id != placement.id {
            let prev = load_placement(conn, ctx.tenant_id, prev_id, true).await?;
            if prev.status == "occupied" {
                end_placement(
                    conn,
                    ctx,
                    &prev,
                    PlacementTransition::Release,
                    Some("transfer"),
                    PositionState::Cleaning,
                )
                .await?;
            }
        }
    }
    sqlx::query(
        "UPDATE placement_requests SET status = $3, version = version + 1, updated_at = now() WHERE tenant_id = $1 AND id = $2",
    )
    .bind(ctx.tenant_id)
    .bind(req.id)
    .bind(to.as_str())
    .execute(&mut *conn)
    .await?;
    record_placement_history(
        conn,
        ctx,
        None,
        Some(req.id),
        Some(&req.status),
        to.as_str(),
        None,
        None,
        req.version + 1,
    )
    .await?;
    let new_status = if ep.status_enum() == EpisodeStatus::Planned {
        EpisodeStatus::Active
    } else {
        ep.status_enum()
    };
    sqlx::query(
        "UPDATE care_episodes SET current_placement_id = $3, status = $4, started_at = COALESCE(started_at, now()),
             version = version + 1, updated_at = now()
         WHERE tenant_id = $1 AND id = $2",
    )
    .bind(ctx.tenant_id)
    .bind(ep.id)
    .bind(placement.id)
    .bind(new_status.as_str())
    .execute(&mut *conn)
    .await?;
    record_episode_history(
        conn,
        ctx,
        ep.id,
        Some(&ep.status),
        new_status.as_str(),
        if req.kind == "transfer" {
            "transferred"
        } else {
            "placed"
        },
        None,
        None,
        ep.version + 1,
    )
    .await?;
    Ok((
        load_request(conn, ctx.tenant_id, req.id, false).await?,
        placement,
        load_episode(conn, ctx.tenant_id, ep.id, false).await?,
    ))
}

#[derive(Debug, Deserialize)]
pub struct CancelRequestInput {
    pub expected_version: i64,
    pub reason_code: Option<String>,
}

pub async fn cancel_request(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    request_id: Uuid,
    input: &CancelRequestInput,
) -> Result<PlacementRequestRow, ApiError> {
    let req = load_request(conn, ctx.tenant_id, request_id, true).await?;
    if req.version != input.expected_version {
        return Err(stale("placement request"));
    }
    let to = req
        .status_enum()
        .apply(PlacementRequestTransition::Cancel)
        .map_err(invalid_transition)?;
    if let Some(h) = live_hold_for_request(conn, ctx.tenant_id, req.id).await? {
        end_placement(
            conn,
            ctx,
            &h,
            PlacementTransition::Revoke,
            Some("request_cancelled"),
            PositionState::Available,
        )
        .await?;
    }
    sqlx::query(
        "UPDATE placement_requests SET status = $3, version = version + 1, updated_at = now() WHERE tenant_id = $1 AND id = $2",
    )
    .bind(ctx.tenant_id)
    .bind(req.id)
    .bind(to.as_str())
    .execute(&mut *conn)
    .await?;
    record_placement_history(
        conn,
        ctx,
        None,
        Some(req.id),
        Some(&req.status),
        to.as_str(),
        input.reason_code.as_deref(),
        None,
        req.version + 1,
    )
    .await?;
    load_request(conn, ctx.tenant_id, req.id, false).await
}

#[derive(Debug, Deserialize)]
pub struct ReleaseInput {
    pub expected_version: i64,
    pub reason_code: Option<String>,
}

/// Release an occupied placement (discharge/departure): the position goes
/// to cleaning and the episode loses its current placement. Does not close
/// the episode; completion stays a separate human transition.
pub async fn release(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    placement_id: Uuid,
    input: &ReleaseInput,
) -> Result<(PlacementRow, EpisodeRow), ApiError> {
    let p = load_placement(conn, ctx.tenant_id, placement_id, true).await?;
    if p.version != input.expected_version {
        return Err(stale("placement"));
    }
    let ep = load_episode(conn, ctx.tenant_id, p.episode_id, true).await?;
    let p = end_placement(
        conn,
        ctx,
        &p,
        PlacementTransition::Release,
        input.reason_code.as_deref(),
        PositionState::Cleaning,
    )
    .await?;
    if ep.current_placement_id == Some(p.id) {
        sqlx::query(
            "UPDATE care_episodes SET current_placement_id = NULL, version = version + 1, updated_at = now() WHERE id = $1",
        )
        .bind(ep.id)
        .execute(&mut *conn)
        .await?;
        record_episode_history(
            conn,
            ctx,
            ep.id,
            Some(&ep.status),
            &ep.status,
            "placement_released",
            input.reason_code.as_deref(),
            None,
            ep.version + 1,
        )
        .await?;
    }
    Ok((p, load_episode(conn, ctx.tenant_id, ep.id, false).await?))
}

/// Expire overdue holds (called by the worker pass and before snapshots).
pub async fn expire_holds(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    tenant_id: Uuid,
) -> Result<Vec<Uuid>, ApiError> {
    let sql = format!(
        "SELECT {PLACEMENT_COLS} FROM placements WHERE tenant_id = $1 AND status = 'held' AND expires_at <= now() FOR UPDATE SKIP LOCKED"
    );
    let rows = sqlx::query(&sql)
        .bind(tenant_id)
        .fetch_all(&mut *conn)
        .await?;
    let mut out = Vec::new();
    for r in rows.iter().map(placement_from_row) {
        end_placement(
            conn,
            ctx,
            &r,
            PlacementTransition::Expire,
            Some("hold_expired"),
            PositionState::Available,
        )
        .await?;
        if let Some(rid) = r.request_id {
            let req = load_request(conn, tenant_id, rid, true).await?;
            if req.status_enum() == PlacementRequestStatus::Held {
                sqlx::query(
                    "UPDATE placement_requests SET status = 'candidates_ready', version = version + 1, updated_at = now() WHERE id = $1",
                )
                .bind(rid)
                .execute(&mut *conn)
                .await?;
                record_placement_history(
                    conn,
                    ctx,
                    None,
                    Some(rid),
                    Some("held"),
                    "candidates_ready",
                    Some("hold_expired"),
                    None,
                    req.version + 1,
                )
                .await?;
            }
        }
        out.push(r.id);
    }
    Ok(out)
}

pub async fn placement_history(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    episode_id: Uuid,
) -> Result<Vec<Value>, ApiError> {
    let rows = sqlx::query(
        "SELECT h.placement_id, h.request_id, h.from_status, h.to_status, h.reason_code, h.note, h.actor, h.version, h.recorded_at
         FROM placement_history h
         LEFT JOIN placements p ON p.id = h.placement_id
         LEFT JOIN placement_requests r ON r.id = h.request_id
         WHERE h.tenant_id = $1 AND COALESCE(p.episode_id, r.episode_id) = $2
         ORDER BY h.recorded_at, h.version",
    )
    .bind(tenant_id)
    .bind(episode_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .iter()
        .map(|r| {
            json!({
                "placement_id": r.get::<Option<Uuid>, _>("placement_id"),
                "request_id": r.get::<Option<Uuid>, _>("request_id"),
                "from_status": r.get::<Option<String>, _>("from_status"),
                "to_status": r.get::<String, _>("to_status"),
                "reason_code": r.get::<Option<String>, _>("reason_code"),
                "note": r.get::<Option<String>, _>("note"),
                "actor": r.get::<String, _>("actor"),
                "version": r.get::<i64, _>("version"),
                "recorded_at": r.get::<DateTime<Utc>, _>("recorded_at"),
            })
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Coordination tasks and handoffs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct TaskRow {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub facility_id: Uuid,
    pub episode_id: Uuid,
    pub patient_id: Uuid,
    pub title_en: String,
    pub title_es: String,
    pub kind: String,
    pub status: String,
    pub accountable_user_id: Option<Uuid>,
    pub accountable_queue_id: Option<Uuid>,
    pub care_team_assignment_id: Option<Uuid>,
    pub profession_code: Option<String>,
    pub capability_codes: Vec<String>,
    pub due_at: Option<DateTime<Utc>>,
    pub escalate_at: Option<DateTime<Utc>>,
    pub barrier_category_code: Option<String>,
    pub patient_visible: bool,
    pub patient_text_en: Option<String>,
    pub patient_text_es: Option<String>,
    pub completion_evidence: Option<Value>,
    pub completed_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub depends_on: Vec<Uuid>,
}

const TASK_COLS: &str = "t.id, t.tenant_id, t.facility_id, t.episode_id, t.patient_id, t.title_en, t.title_es, t.kind, t.status,
    t.accountable_user_id, t.accountable_queue_id, t.care_team_assignment_id, t.profession_code, t.capability_codes,
    t.due_at, t.escalate_at, t.barrier_category_code, t.patient_visible, t.patient_text_en, t.patient_text_es,
    t.completion_evidence, t.completed_at, t.version, t.created_at, t.updated_at,
    COALESCE((SELECT array_agg(d.depends_on_id) FROM care_coordination_task_dependencies d WHERE d.task_id = t.id), '{}') AS depends_on";

fn task_from_row(r: &PgRow) -> TaskRow {
    TaskRow {
        id: r.get("id"),
        tenant_id: r.get("tenant_id"),
        facility_id: r.get("facility_id"),
        episode_id: r.get("episode_id"),
        patient_id: r.get("patient_id"),
        title_en: r.get("title_en"),
        title_es: r.get("title_es"),
        kind: r.get("kind"),
        status: r.get("status"),
        accountable_user_id: r.get("accountable_user_id"),
        accountable_queue_id: r.get("accountable_queue_id"),
        care_team_assignment_id: r.get("care_team_assignment_id"),
        profession_code: r.get("profession_code"),
        capability_codes: r.get("capability_codes"),
        due_at: r.get("due_at"),
        escalate_at: r.get("escalate_at"),
        barrier_category_code: r.get("barrier_category_code"),
        patient_visible: r.get("patient_visible"),
        patient_text_en: r.get("patient_text_en"),
        patient_text_es: r.get("patient_text_es"),
        completion_evidence: r.get("completion_evidence"),
        completed_at: r.get("completed_at"),
        version: r.get("version"),
        created_at: r.get("created_at"),
        updated_at: r.get("updated_at"),
        depends_on: r.get("depends_on"),
    }
}

pub const TASK_KINDS: &[&str] = &[
    "coordination",
    "discharge_preparation",
    "transfer",
    "assessment",
    "education",
    "equipment",
    "transport",
    "documentation",
    "other",
];

pub async fn load_task(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    id: Uuid,
    lock: bool,
) -> Result<TaskRow, ApiError> {
    let sql = format!(
        "SELECT {TASK_COLS} FROM care_coordination_tasks t WHERE t.tenant_id = $1 AND t.id = $2 {}",
        if lock { "FOR UPDATE OF t" } else { "" }
    );
    let row = sqlx::query(&sql)
        .bind(tenant_id)
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(ApiError::not_found)?;
    Ok(task_from_row(&row))
}

pub async fn list_tasks(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    episode_id: Option<Uuid>,
    facility_ids: Option<&[Uuid]>,
    open_only: bool,
    limit: i64,
) -> Result<Vec<TaskRow>, ApiError> {
    let sql = format!(
        "SELECT {TASK_COLS} FROM care_coordination_tasks t
         WHERE t.tenant_id = $1 AND ($2::uuid IS NULL OR t.episode_id = $2)
           AND ($3::uuid[] IS NULL OR t.facility_id = ANY($3))
           AND (NOT $4 OR t.status IN ('open','in_progress','blocked','handed_off'))
         ORDER BY t.due_at NULLS LAST, t.created_at LIMIT $5"
    );
    let rows = sqlx::query(&sql)
        .bind(tenant_id)
        .bind(episode_id)
        .bind(facility_ids)
        .bind(open_only)
        .bind(limit)
        .fetch_all(&mut *conn)
        .await?;
    Ok(rows.iter().map(task_from_row).collect())
}

#[derive(Debug, Deserialize)]
pub struct TaskInput {
    pub episode_id: Uuid,
    pub title_en: String,
    pub title_es: String,
    pub kind: Option<String>,
    pub accountable_user_id: Option<Uuid>,
    pub accountable_queue_id: Option<Uuid>,
    pub care_team_assignment_id: Option<Uuid>,
    pub profession_code: Option<String>,
    #[serde(default)]
    pub capability_codes: Vec<String>,
    pub due_at: Option<DateTime<Utc>>,
    pub escalate_at: Option<DateTime<Utc>>,
    pub barrier_category_code: Option<String>,
    #[serde(default)]
    pub patient_visible: bool,
    pub patient_text_en: Option<String>,
    pub patient_text_es: Option<String>,
    #[serde(default)]
    pub depends_on: Vec<Uuid>,
}

#[allow(clippy::too_many_arguments)]
async fn record_task_history(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    task_id: Uuid,
    from: Option<&str>,
    to: &str,
    change_kind: &str,
    reason: Option<&str>,
    note: Option<&str>,
    version: i64,
) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO care_coordination_task_history
         (id, tenant_id, task_id, from_status, to_status, change_kind, reason_code, note, actor, version)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
    )
    .bind(Uuid::now_v7())
    .bind(ctx.tenant_id)
    .bind(task_id)
    .bind(from)
    .bind(to)
    .bind(change_kind)
    .bind(reason)
    .bind(note)
    .bind(actor(ctx))
    .bind(version)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

pub async fn create_task(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    input: &TaskInput,
) -> Result<TaskRow, ApiError> {
    let ep = load_episode(conn, ctx.tenant_id, input.episode_id, false).await?;
    if !ep.status_enum().is_open() {
        return Err(ApiError::conflict(
            "episode_closed",
            "the episode is not open",
        ));
    }
    let kind = input.kind.as_deref().unwrap_or("coordination");
    if !TASK_KINDS.contains(&kind) {
        return Err(ApiError::bad_request("invalid_kind", "unknown task kind"));
    }
    if input.accountable_user_id.is_none() && input.accountable_queue_id.is_none() {
        return Err(ApiError::bad_request(
            "accountable_required",
            "an accountable user or queue is required",
        ));
    }
    if input.patient_visible && input.patient_text_en.is_none() && input.patient_text_es.is_none() {
        return Err(ApiError::bad_request(
            "patient_text_required",
            "patient-visible tasks need patient wording",
        ));
    }
    if let Some(p) = &input.profession_code {
        if !catalog_code_exists(conn, ctx.tenant_id, "profession", p).await? {
            return Err(ApiError::bad_request(
                "unknown_profession",
                "profession is not an active catalog entry",
            ));
        }
    }
    for d in &input.depends_on {
        let dep = load_task(conn, ctx.tenant_id, *d, false).await?;
        if dep.episode_id != ep.id {
            return Err(ApiError::bad_request(
                "cross_episode_dependency",
                "dependencies must belong to the same episode",
            ));
        }
    }
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO care_coordination_tasks
         (id, tenant_id, facility_id, episode_id, patient_id, title_en, title_es, kind, accountable_user_id,
          accountable_queue_id, care_team_assignment_id, profession_code, capability_codes, due_at, escalate_at,
          barrier_category_code, patient_visible, patient_text_en, patient_text_es, created_by)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20)",
    )
    .bind(id)
    .bind(ctx.tenant_id)
    .bind(ep.facility_id)
    .bind(ep.id)
    .bind(ep.patient_id)
    .bind(&input.title_en)
    .bind(&input.title_es)
    .bind(kind)
    .bind(input.accountable_user_id)
    .bind(input.accountable_queue_id)
    .bind(input.care_team_assignment_id)
    .bind(&input.profession_code)
    .bind(&input.capability_codes)
    .bind(input.due_at)
    .bind(input.escalate_at)
    .bind(&input.barrier_category_code)
    .bind(input.patient_visible)
    .bind(&input.patient_text_en)
    .bind(&input.patient_text_es)
    .bind(ctx.user_id)
    .execute(&mut *conn)
    .await?;
    for d in &input.depends_on {
        sqlx::query("INSERT INTO care_coordination_task_dependencies (task_id, depends_on_id) VALUES ($1,$2) ON CONFLICT DO NOTHING")
            .bind(id)
            .bind(d)
            .execute(&mut *conn)
            .await?;
    }
    record_task_history(conn, ctx, id, None, "open", "created", None, None, 1).await?;
    load_task(conn, ctx.tenant_id, id, false).await
}

#[derive(Debug, Deserialize)]
pub struct TaskTransitionInput {
    pub transition: String,
    pub expected_version: i64,
    pub reason_code: Option<String>,
    pub note: Option<String>,
    pub completion_evidence: Option<Value>,
    /// For `hand_off`/reassignment: the new accountable party.
    pub accountable_user_id: Option<Uuid>,
    pub accountable_queue_id: Option<Uuid>,
}

pub async fn transition_task(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    id: Uuid,
    input: &TaskTransitionInput,
) -> Result<TaskRow, ApiError> {
    let t = load_task(conn, ctx.tenant_id, id, true).await?;
    if t.version != input.expected_version {
        return Err(stale("task"));
    }
    let tr = TaskTransition::parse(&input.transition)
        .ok_or_else(|| ApiError::bad_request("invalid_transition", "unknown transition"))?;
    let from = TaskStatus::parse(&t.status).unwrap_or(TaskStatus::Cancelled);
    let to = from.apply(tr).map_err(invalid_transition)?;
    if to == TaskStatus::Completed {
        let mut deps = Vec::new();
        for d in &t.depends_on {
            let dep = load_task(conn, ctx.tenant_id, *d, false).await?;
            deps.push((
                dep.id,
                TaskStatus::parse(&dep.status).unwrap_or(TaskStatus::Open),
            ));
        }
        let unmet = domain::unmet_dependencies(&deps);
        if !unmet.is_empty() {
            return Err(ApiError::conflict(
                "dependencies_open",
                format!("{} dependenc(ies) still open", unmet.len()),
            ));
        }
    }
    if tr == TaskTransition::HandOff
        && input.accountable_user_id.is_none()
        && input.accountable_queue_id.is_none()
    {
        return Err(ApiError::bad_request(
            "accountable_required",
            "a handoff names the new accountable user or queue",
        ));
    }
    let (user, queue) = if tr == TaskTransition::HandOff {
        (input.accountable_user_id, input.accountable_queue_id)
    } else {
        (t.accountable_user_id, t.accountable_queue_id)
    };
    sqlx::query(
        "UPDATE care_coordination_tasks SET status = $3, accountable_user_id = $4, accountable_queue_id = $5,
             completion_evidence = CASE WHEN $3 = 'completed' THEN $6 ELSE completion_evidence END,
             completed_at = CASE WHEN $3 = 'completed' THEN now() ELSE completed_at END,
             completed_by = CASE WHEN $3 = 'completed' THEN $7 ELSE completed_by END,
             version = version + 1, updated_at = now()
         WHERE tenant_id = $1 AND id = $2",
    )
    .bind(ctx.tenant_id)
    .bind(id)
    .bind(to.as_str())
    .bind(user)
    .bind(queue)
    .bind(&input.completion_evidence)
    .bind(ctx.user_id)
    .execute(&mut *conn)
    .await?;
    record_task_history(
        conn,
        ctx,
        id,
        Some(from.as_str()),
        to.as_str(),
        if tr == TaskTransition::HandOff {
            "handoff"
        } else {
            "transition"
        },
        input.reason_code.as_deref(),
        input.note.as_deref(),
        t.version + 1,
    )
    .await?;
    load_task(conn, ctx.tenant_id, id, false).await
}

#[derive(Debug, Deserialize)]
pub struct ReassignInput {
    pub expected_version: i64,
    pub accountable_user_id: Option<Uuid>,
    pub accountable_queue_id: Option<Uuid>,
    pub reason: String,
}

pub async fn reassign_task(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    id: Uuid,
    input: &ReassignInput,
) -> Result<TaskRow, ApiError> {
    let t = load_task(conn, ctx.tenant_id, id, true).await?;
    if t.version != input.expected_version {
        return Err(stale("task"));
    }
    if input.accountable_user_id.is_none() && input.accountable_queue_id.is_none() {
        return Err(ApiError::bad_request(
            "accountable_required",
            "an accountable user or queue is required",
        ));
    }
    if input.reason.trim().is_empty() {
        return Err(ApiError::bad_request(
            "reason_required",
            "a reassignment needs a reason",
        ));
    }
    if !TaskStatus::parse(&t.status).is_some_and(TaskStatus::is_open) {
        return Err(ApiError::conflict(
            "task_closed",
            "closed tasks cannot be reassigned",
        ));
    }
    sqlx::query(
        "UPDATE care_coordination_tasks SET accountable_user_id = $3, accountable_queue_id = $4, version = version + 1, updated_at = now()
         WHERE tenant_id = $1 AND id = $2",
    )
    .bind(ctx.tenant_id)
    .bind(id)
    .bind(input.accountable_user_id)
    .bind(input.accountable_queue_id)
    .execute(&mut *conn)
    .await?;
    record_task_history(
        conn,
        ctx,
        id,
        Some(&t.status),
        &t.status,
        "reassigned",
        None,
        Some(&input.reason),
        t.version + 1,
    )
    .await?;
    load_task(conn, ctx.tenant_id, id, false).await
}

#[derive(Debug, Clone, Serialize)]
pub struct HandoffRow {
    pub id: Uuid,
    pub episode_id: Uuid,
    pub shift_label: String,
    pub language: String,
    pub status: String,
    pub content: Value,
    pub facts_hash: String,
    pub origin: String,
    pub artifact_id: Option<Uuid>,
    pub author_id: Uuid,
    pub decided_by: Option<Uuid>,
    pub decided_at: Option<DateTime<Utc>>,
    pub decision_note: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

fn handoff_from_row(r: &PgRow) -> HandoffRow {
    HandoffRow {
        id: r.get("id"),
        episode_id: r.get("episode_id"),
        shift_label: r.get("shift_label"),
        language: r.get("language"),
        status: r.get("status"),
        content: r.get("content"),
        facts_hash: r.get("facts_hash"),
        origin: r.get("origin"),
        artifact_id: r.get("artifact_id"),
        author_id: r.get("author_id"),
        decided_by: r.get("decided_by"),
        decided_at: r.get("decided_at"),
        decision_note: r.get("decision_note"),
        version: r.get("version"),
        created_at: r.get("created_at"),
    }
}

const HANDOFF_COLS: &str = "id, episode_id, shift_label, language, status, content, facts_hash, origin, artifact_id, author_id,
    decided_by, decided_at, decision_note, version, created_at";

pub async fn episode_handoffs(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    episode_id: Uuid,
) -> Result<Vec<HandoffRow>, ApiError> {
    let sql = format!("SELECT {HANDOFF_COLS} FROM care_handoffs WHERE tenant_id = $1 AND episode_id = $2 ORDER BY created_at DESC");
    let rows = sqlx::query(&sql)
        .bind(tenant_id)
        .bind(episode_id)
        .fetch_all(&mut *conn)
        .await?;
    Ok(rows.iter().map(handoff_from_row).collect())
}

#[derive(Debug, Deserialize)]
pub struct HandoffInput {
    pub shift_label: String,
    pub language: Option<String>,
    /// {"situation":..., "background":..., "assessment":..., "recommendation":..., "pending":[...]}
    pub content: Value,
}

/// Human-authored handoff. Human handoffs are accepted immediately (the
/// author is accountable); dMind drafts are stored via `create_handoff_draft`.
pub async fn create_handoff(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    episode_id: Uuid,
    input: &HandoffInput,
) -> Result<HandoffRow, ApiError> {
    let ep = load_episode(conn, ctx.tenant_id, episode_id, false).await?;
    let lang = input.language.as_deref().unwrap_or("en");
    if !["en", "es"].contains(&lang) {
        return Err(ApiError::bad_request(
            "invalid_language",
            "language must be en or es",
        ));
    }
    if !input.content.is_object() || input.shift_label.trim().is_empty() {
        return Err(ApiError::bad_request(
            "invalid_handoff",
            "shift label and structured content are required",
        ));
    }
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO care_handoffs (id, tenant_id, facility_id, episode_id, patient_id, shift_label, language, status, content,
             source_facts, facts_hash, origin, author_id, decided_by, decided_at)
         VALUES ($1,$2,$3,$4,$5,$6,$7,'accepted',$8,'{}'::jsonb,$9,'human',$10,$10,now())",
    )
    .bind(id)
    .bind(ctx.tenant_id)
    .bind(ep.facility_id)
    .bind(ep.id)
    .bind(ep.patient_id)
    .bind(input.shift_label.trim())
    .bind(lang)
    .bind(&input.content)
    .bind(domain::hash_facts(&input.content))
    .bind(ctx.user_id)
    .execute(&mut *conn)
    .await?;
    let sql = format!("SELECT {HANDOFF_COLS} FROM care_handoffs WHERE id = $1");
    let row = sqlx::query(&sql).bind(id).fetch_one(&mut *conn).await?;
    Ok(handoff_from_row(&row))
}

/// Persist a dMind handoff draft; it stays `draft` until a human decides.
#[allow(clippy::too_many_arguments)]
pub async fn create_handoff_draft(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    ep: &EpisodeRow,
    shift_label: &str,
    language: &str,
    content: &Value,
    source_facts: &Value,
    artifact_id: Uuid,
) -> Result<HandoffRow, ApiError> {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO care_handoffs (id, tenant_id, facility_id, episode_id, patient_id, shift_label, language, status, content,
             source_facts, facts_hash, origin, artifact_id, author_id)
         VALUES ($1,$2,$3,$4,$5,$6,$7,'draft',$8,$9,$10,'dmind_draft',$11,$12)",
    )
    .bind(id)
    .bind(ctx.tenant_id)
    .bind(ep.facility_id)
    .bind(ep.id)
    .bind(ep.patient_id)
    .bind(shift_label)
    .bind(language)
    .bind(content)
    .bind(source_facts)
    .bind(domain::hash_facts(source_facts))
    .bind(artifact_id)
    .bind(ctx.user_id)
    .execute(&mut *conn)
    .await?;
    let sql = format!("SELECT {HANDOFF_COLS} FROM care_handoffs WHERE id = $1");
    let row = sqlx::query(&sql).bind(id).fetch_one(&mut *conn).await?;
    Ok(handoff_from_row(&row))
}

#[derive(Debug, Deserialize)]
pub struct HandoffDecisionInput {
    pub expected_version: i64,
    pub decision: String,
    pub note: Option<String>,
}

pub async fn decide_handoff(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    id: Uuid,
    input: &HandoffDecisionInput,
) -> Result<HandoffRow, ApiError> {
    let sql = format!(
        "SELECT {HANDOFF_COLS} FROM care_handoffs WHERE tenant_id = $1 AND id = $2 FOR UPDATE"
    );
    let row = sqlx::query(&sql)
        .bind(ctx.tenant_id)
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(ApiError::not_found)?;
    let h = handoff_from_row(&row);
    if h.version != input.expected_version {
        return Err(stale("handoff"));
    }
    if h.status != "draft" {
        return Err(ApiError::conflict(
            "already_decided",
            "the handoff is no longer a draft",
        ));
    }
    let status = match input.decision.as_str() {
        "accept" => "accepted",
        "reject" => "rejected",
        _ => {
            return Err(ApiError::bad_request(
                "invalid_decision",
                "decision must be accept or reject",
            ))
        }
    };
    sqlx::query(
        "UPDATE care_handoffs SET status = $2, decided_by = $3, decided_at = now(), decision_note = $4,
             version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(status)
    .bind(ctx.user_id)
    .bind(&input.note)
    .execute(&mut *conn)
    .await?;
    if let Some(a) = h.artifact_id {
        let st = if status == "accepted" {
            "accepted"
        } else {
            "rejected"
        };
        sqlx::query("UPDATE ai_artifacts SET status = $2, reviewed_by = $3, reviewed_at = now() WHERE id = $1 AND tenant_id = $4")
            .bind(a)
            .bind(st)
            .bind(ctx.user_id)
            .bind(ctx.tenant_id)
            .execute(&mut *conn)
            .await?;
    }
    let sql = format!("SELECT {HANDOFF_COLS} FROM care_handoffs WHERE id = $1");
    let row = sqlx::query(&sql).bind(id).fetch_one(&mut *conn).await?;
    Ok(handoff_from_row(&row))
}

// ---------------------------------------------------------------------------
// Escalation timers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct EscalationRow {
    pub id: Uuid,
    pub facility_id: Uuid,
    pub patient_id: Uuid,
    pub episode_id: Option<Uuid>,
    pub visit_id: Option<Uuid>,
    pub placement_request_id: Option<Uuid>,
    pub kind: String,
    pub threshold_minutes: i32,
    pub started_at: DateTime<Utc>,
    pub due_at: DateTime<Utc>,
    pub status: String,
    pub escalated_at: Option<DateTime<Utc>>,
    pub internal_alert_id: Option<Uuid>,
    pub version: i64,
}

const ESCALATION_COLS: &str = "id, facility_id, patient_id, episode_id, visit_id, placement_request_id, kind, threshold_minutes,
    started_at, due_at, status, escalated_at, internal_alert_id, version";

fn escalation_from_row(r: &PgRow) -> EscalationRow {
    EscalationRow {
        id: r.get("id"),
        facility_id: r.get("facility_id"),
        patient_id: r.get("patient_id"),
        episode_id: r.get("episode_id"),
        visit_id: r.get("visit_id"),
        placement_request_id: r.get("placement_request_id"),
        kind: r.get("kind"),
        threshold_minutes: r.get("threshold_minutes"),
        started_at: r.get("started_at"),
        due_at: r.get("due_at"),
        status: r.get("status"),
        escalated_at: r.get("escalated_at"),
        internal_alert_id: r.get("internal_alert_id"),
        version: r.get("version"),
    }
}

pub struct ArmEscalation<'a> {
    pub facility_id: Uuid,
    pub patient_id: Uuid,
    pub episode_id: Option<Uuid>,
    pub visit_id: Option<Uuid>,
    pub placement_request_id: Option<Uuid>,
    pub kind: &'a str,
    pub priority: &'a str,
    pub started_at: DateTime<Utc>,
    pub threshold_minutes: Option<i64>,
}

/// Arm a timer unless an equivalent open one already exists.
pub async fn arm_escalation(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    a: ArmEscalation<'_>,
) -> Result<Option<EscalationRow>, ApiError> {
    let minutes = match a
        .threshold_minutes
        .or_else(|| domain::default_escalation_minutes(a.kind, a.priority))
    {
        Some(m) if m >= 1 => m,
        _ => {
            return Err(ApiError::bad_request(
                "invalid_escalation",
                "unknown escalation kind or threshold",
            ))
        }
    };
    let existing: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM care_escalations WHERE tenant_id = $1 AND kind = $2 AND status IN ('armed','escalated','acknowledged')
           AND episode_id IS NOT DISTINCT FROM $3 AND visit_id IS NOT DISTINCT FROM $4
           AND placement_request_id IS NOT DISTINCT FROM $5",
    )
    .bind(ctx.tenant_id)
    .bind(a.kind)
    .bind(a.episode_id)
    .bind(a.visit_id)
    .bind(a.placement_request_id)
    .fetch_optional(&mut *conn)
    .await?;
    if existing.is_some() {
        return Ok(None);
    }
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO care_escalations (id, tenant_id, facility_id, patient_id, episode_id, visit_id, placement_request_id, kind,
             threshold_minutes, started_at, due_at, created_by)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$10 + make_interval(mins => $9),$11)",
    )
    .bind(id)
    .bind(ctx.tenant_id)
    .bind(a.facility_id)
    .bind(a.patient_id)
    .bind(a.episode_id)
    .bind(a.visit_id)
    .bind(a.placement_request_id)
    .bind(a.kind)
    .bind(minutes as i32)
    .bind(a.started_at)
    .bind(ctx.user_id)
    .execute(&mut *conn)
    .await?;
    let sql = format!("SELECT {ESCALATION_COLS} FROM care_escalations WHERE id = $1");
    let row = sqlx::query(&sql).bind(id).fetch_one(&mut *conn).await?;
    Ok(Some(escalation_from_row(&row)))
}

pub async fn list_escalations(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    facility_ids: Option<&[Uuid]>,
    episode_id: Option<Uuid>,
    open_only: bool,
) -> Result<Vec<EscalationRow>, ApiError> {
    let sql = format!(
        "SELECT {ESCALATION_COLS} FROM care_escalations
         WHERE tenant_id = $1 AND ($2::uuid[] IS NULL OR facility_id = ANY($2))
           AND ($3::uuid IS NULL OR episode_id = $3)
           AND (NOT $4 OR status IN ('armed','escalated','acknowledged'))
         ORDER BY due_at LIMIT 500"
    );
    let rows = sqlx::query(&sql)
        .bind(tenant_id)
        .bind(facility_ids)
        .bind(episode_id)
        .bind(open_only)
        .fetch_all(&mut *conn)
        .await?;
    Ok(rows.iter().map(escalation_from_row).collect())
}

/// Fire due timers: each becomes `escalated` and raises one internal alert
/// through the existing alert channel. Idempotent per timer.
pub async fn fire_due_escalations(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    tenant_id: Uuid,
    now: DateTime<Utc>,
) -> Result<Vec<EscalationRow>, ApiError> {
    let sql = format!(
        "SELECT {ESCALATION_COLS} FROM care_escalations WHERE tenant_id = $1 AND status = 'armed' AND due_at <= $2 FOR UPDATE SKIP LOCKED"
    );
    let rows = sqlx::query(&sql)
        .bind(tenant_id)
        .bind(now)
        .fetch_all(&mut *conn)
        .await?;
    let mut fired = Vec::new();
    for e in rows.iter().map(escalation_from_row) {
        if !domain::escalation_due(e.due_at, now) {
            continue;
        }
        let (queue, responsible_user): (Option<Uuid>, Option<Uuid>) = match e.episode_id {
            Some(ep) => sqlx::query_as(
                "SELECT responsible_queue_id, responsible_user_id FROM care_episodes WHERE id = $1",
            )
            .bind(ep)
            .fetch_optional(&mut *conn)
            .await?
            .unwrap_or((None, None)),
            None => (None, None),
        };
        let target_user = if queue.is_none() {
            Some(responsible_user.unwrap_or(ctx.user_id))
        } else {
            None
        };
        let visit_id: Option<Uuid> = match e.visit_id {
            Some(v) => Some(v),
            None => {
                sqlx::query_scalar(
                    "SELECT id FROM visits WHERE tenant_id = $1 AND patient_id = $2
                       AND status NOT IN ('completed', 'cancelled')
                     ORDER BY created_at DESC LIMIT 1",
                )
                .bind(tenant_id)
                .bind(e.patient_id)
                .fetch_optional(&mut *conn)
                .await?
            }
        };
        let alert_id = visit_id.map(|_| Uuid::now_v7());
        if let Some(alert_id) = alert_id {
            sqlx::query(
            "INSERT INTO internal_alerts (id, tenant_id, facility_id, patient_id, visit_id, kind, priority, target_user_id, target_queue_id, created_by)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
        )
        .bind(alert_id)
        .bind(tenant_id)
        .bind(e.facility_id)
        .bind(e.patient_id)
        .bind(visit_id)
        .bind(format!("care_ops_{}", e.kind))
        .bind("high")
        .bind(target_user)
        .bind(queue)
        .bind(ctx.user_id)
        .execute(&mut *conn)
        .await?;
        }
        sqlx::query(
            "UPDATE care_escalations SET status = 'escalated', escalated_at = $2, internal_alert_id = $3, version = version + 1, updated_at = now() WHERE id = $1",
        )
        .bind(e.id)
        .bind(now)
        .bind(alert_id)
        .execute(&mut *conn)
        .await?;
        let sql = format!("SELECT {ESCALATION_COLS} FROM care_escalations WHERE id = $1");
        let row = sqlx::query(&sql).bind(e.id).fetch_one(&mut *conn).await?;
        fired.push(escalation_from_row(&row));
    }
    Ok(fired)
}

#[derive(Debug, Deserialize)]
pub struct EscalationUpdateInput {
    pub expected_version: i64,
    pub status: String,
}

pub async fn update_escalation(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    id: Uuid,
    input: &EscalationUpdateInput,
) -> Result<EscalationRow, ApiError> {
    use wellos_domain::care_ops::EscalationStatus;
    let sql = format!("SELECT {ESCALATION_COLS} FROM care_escalations WHERE tenant_id = $1 AND id = $2 FOR UPDATE");
    let row = sqlx::query(&sql)
        .bind(ctx.tenant_id)
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(ApiError::not_found)?;
    let e = escalation_from_row(&row);
    if e.version != input.expected_version {
        return Err(stale("escalation"));
    }
    let from = EscalationStatus::parse(&e.status).unwrap_or(EscalationStatus::Cancelled);
    let to = EscalationStatus::parse(&input.status)
        .ok_or_else(|| ApiError::bad_request("invalid_status", "unknown escalation status"))?;
    if !from.can_transition(to) {
        return Err(ApiError::conflict(
            "invalid_transition",
            format!(
                "escalation cannot move from {} to {}",
                from.as_str(),
                to.as_str()
            ),
        ));
    }
    sqlx::query(
        "UPDATE care_escalations SET status = $2, resolved_at = CASE WHEN $2 IN ('resolved','cancelled') THEN now() ELSE resolved_at END,
             version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(to.as_str())
    .execute(&mut *conn)
    .await?;
    let sql = format!("SELECT {ESCALATION_COLS} FROM care_escalations WHERE id = $1");
    let row = sqlx::query(&sql).bind(id).fetch_one(&mut *conn).await?;
    Ok(escalation_from_row(&row))
}

/// Resolve open timers of `kind` bound to an episode/request (e.g. when a
/// placement is confirmed the boarding and placement-delay timers close).
pub async fn resolve_escalations(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    episode_id: Option<Uuid>,
    visit_id: Option<Uuid>,
    kinds: &[&str],
) -> Result<u64, ApiError> {
    let kinds: Vec<String> = kinds.iter().map(|k| k.to_string()).collect();
    Ok(sqlx::query(
        "UPDATE care_escalations SET status = 'resolved', resolved_at = now(), version = version + 1, updated_at = now()
         WHERE tenant_id = $1 AND status IN ('armed','escalated','acknowledged') AND kind = ANY($4)
           AND (($2::uuid IS NOT NULL AND episode_id = $2) OR ($3::uuid IS NOT NULL AND visit_id = $3))",
    )
    .bind(tenant_id)
    .bind(episode_id)
    .bind(visit_id)
    .bind(&kinds)
    .execute(&mut *conn)
    .await?
    .rows_affected())
}
