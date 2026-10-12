//! Perioperative cases and theatre sessions. Scheduling reuses the existing
//! `resource_bookings` exclusion constraint when the theatre is linked to a
//! schedulable resource and always runs the deterministic conflict engine
//! inside the transaction.

use super::{invalid_transition, load_episode, load_location, stale};
use crate::auth::AuthContext;
use crate::error::ApiError;
use crate::scheduling;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use uuid::Uuid;
use wellos_domain::care_ops::{
    self as domain, CaseStatus, CaseTransition, Interval, Prerequisite, TheatreCaseFacts,
    TheatreCheck,
};

#[derive(Debug, Clone, Serialize)]
pub struct SessionRow {
    pub id: Uuid,
    pub facility_id: Uuid,
    pub theatre_location_id: Uuid,
    pub service_code: Option<String>,
    pub specialty_codes: Vec<String>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub turnaround_minutes: i32,
    pub status: String,
    pub version: i64,
}

const SESSION_COLS: &str =
    "id, facility_id, theatre_location_id, service_code, specialty_codes, starts_at, ends_at,
    turnaround_minutes, status, version";

fn session_from_row(r: &PgRow) -> SessionRow {
    SessionRow {
        id: r.get("id"),
        facility_id: r.get("facility_id"),
        theatre_location_id: r.get("theatre_location_id"),
        service_code: r.get("service_code"),
        specialty_codes: r.get("specialty_codes"),
        starts_at: r.get("starts_at"),
        ends_at: r.get("ends_at"),
        turnaround_minutes: r.get("turnaround_minutes"),
        status: r.get("status"),
        version: r.get("version"),
    }
}

#[derive(Debug, Deserialize)]
pub struct SessionInput {
    pub theatre_location_id: Uuid,
    pub service_code: Option<String>,
    #[serde(default)]
    pub specialty_codes: Vec<String>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub turnaround_minutes: Option<i32>,
}

pub async fn create_session(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    input: &SessionInput,
) -> Result<SessionRow, ApiError> {
    let theatre = load_location(conn, ctx.tenant_id, input.theatre_location_id, false).await?;
    if theatre.level != "position" {
        return Err(ApiError::bad_request(
            "not_a_theatre",
            "sessions are planned on theatre positions",
        ));
    }
    if input.ends_at <= input.starts_at {
        return Err(ApiError::bad_request(
            "invalid_interval",
            "session must end after it starts",
        ));
    }
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO theatre_sessions (id, tenant_id, facility_id, theatre_location_id, service_code, specialty_codes,
             starts_at, ends_at, turnaround_minutes, created_by)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
    )
    .bind(id)
    .bind(ctx.tenant_id)
    .bind(theatre.facility_id)
    .bind(theatre.id)
    .bind(&input.service_code)
    .bind(&input.specialty_codes)
    .bind(input.starts_at)
    .bind(input.ends_at)
    .bind(input.turnaround_minutes.unwrap_or(30))
    .bind(ctx.user_id)
    .execute(&mut *conn)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(d) if d.code().as_deref() == Some("23P01") => {
            ApiError::conflict("session_overlap", "the theatre already has a planned session in this interval")
        }
        _ => ApiError::from(e),
    })?;
    let sql = format!("SELECT {SESSION_COLS} FROM theatre_sessions WHERE id = $1");
    Ok(session_from_row(
        &sqlx::query(&sql).bind(id).fetch_one(&mut *conn).await?,
    ))
}

pub async fn list_sessions(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    facility_ids: Option<&[Uuid]>,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<Vec<SessionRow>, ApiError> {
    let sql = format!(
        "SELECT {SESSION_COLS} FROM theatre_sessions
         WHERE tenant_id = $1 AND ($2::uuid[] IS NULL OR facility_id = ANY($2)) AND starts_at < $4 AND ends_at > $3
         ORDER BY starts_at"
    );
    let rows = sqlx::query(&sql)
        .bind(tenant_id)
        .bind(facility_ids)
        .bind(from)
        .bind(to)
        .fetch_all(&mut *conn)
        .await?;
    Ok(rows.iter().map(session_from_row).collect())
}

#[derive(Debug, Clone, Serialize)]
pub struct CaseRow {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub facility_id: Uuid,
    pub episode_id: Uuid,
    pub patient_id: Uuid,
    pub procedure_code: String,
    pub responsible_service_code: String,
    pub responsible_user_id: Option<Uuid>,
    pub status: String,
    pub priority: String,
    pub expected_duration_minutes: i32,
    pub required_professionals: Value,
    pub required_equipment_codes: Vec<String>,
    pub prerequisites: Value,
    pub required_service_request_ids: Vec<Uuid>,
    pub checklist: Value,
    pub post_procedure_requirements: Value,
    pub session_id: Option<Uuid>,
    pub theatre_location_id: Option<Uuid>,
    pub scheduled_start: Option<DateTime<Utc>>,
    pub scheduled_end: Option<DateTime<Utc>>,
    pub booking_id: Option<Uuid>,
    pub team_user_ids: Vec<Uuid>,
    pub delay_reason_code: Option<String>,
    pub cancellation_reason_code: Option<String>,
    pub override_reason: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

const CASE_COLS: &str = "id, tenant_id, facility_id, episode_id, patient_id, procedure_code, responsible_service_code,
    responsible_user_id, status, priority, expected_duration_minutes, required_professionals, required_equipment_codes,
    prerequisites, required_service_request_ids, checklist, post_procedure_requirements, session_id, theatre_location_id,
    scheduled_start, scheduled_end, booking_id, team_user_ids, delay_reason_code, cancellation_reason_code, override_reason,
    version, created_at, updated_at";

fn case_from_row(r: &PgRow) -> CaseRow {
    CaseRow {
        id: r.get("id"),
        tenant_id: r.get("tenant_id"),
        facility_id: r.get("facility_id"),
        episode_id: r.get("episode_id"),
        patient_id: r.get("patient_id"),
        procedure_code: r.get("procedure_code"),
        responsible_service_code: r.get("responsible_service_code"),
        responsible_user_id: r.get("responsible_user_id"),
        status: r.get("status"),
        priority: r.get("priority"),
        expected_duration_minutes: r.get("expected_duration_minutes"),
        required_professionals: r.get("required_professionals"),
        required_equipment_codes: r.get("required_equipment_codes"),
        prerequisites: r.get("prerequisites"),
        required_service_request_ids: r.get("required_service_request_ids"),
        checklist: r.get("checklist"),
        post_procedure_requirements: r.get("post_procedure_requirements"),
        session_id: r.get("session_id"),
        theatre_location_id: r.get("theatre_location_id"),
        scheduled_start: r.get("scheduled_start"),
        scheduled_end: r.get("scheduled_end"),
        booking_id: r.get("booking_id"),
        team_user_ids: r.get("team_user_ids"),
        delay_reason_code: r.get("delay_reason_code"),
        cancellation_reason_code: r.get("cancellation_reason_code"),
        override_reason: r.get("override_reason"),
        version: r.get("version"),
        created_at: r.get("created_at"),
        updated_at: r.get("updated_at"),
    }
}

impl CaseRow {
    pub fn status_enum(&self) -> CaseStatus {
        CaseStatus::parse(&self.status).unwrap_or(CaseStatus::Cancelled)
    }
    pub fn prerequisites_typed(&self) -> Vec<Prerequisite> {
        serde_json::from_value(self.prerequisites.clone()).unwrap_or_default()
    }
}

pub async fn load_case(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    id: Uuid,
    lock: bool,
) -> Result<CaseRow, ApiError> {
    let sql = format!(
        "SELECT {CASE_COLS} FROM perioperative_cases WHERE tenant_id = $1 AND id = $2 {}",
        if lock { "FOR UPDATE" } else { "" }
    );
    let row = sqlx::query(&sql)
        .bind(tenant_id)
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(ApiError::not_found)?;
    Ok(case_from_row(&row))
}

pub async fn list_cases(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    facility_ids: Option<&[Uuid]>,
    episode_id: Option<Uuid>,
    open_only: bool,
    limit: i64,
) -> Result<Vec<CaseRow>, ApiError> {
    let sql = format!(
        "SELECT {CASE_COLS} FROM perioperative_cases
         WHERE tenant_id = $1 AND ($2::uuid[] IS NULL OR facility_id = ANY($2))
           AND ($3::uuid IS NULL OR episode_id = $3)
           AND (NOT $4 OR status NOT IN ('completed','cancelled'))
         ORDER BY scheduled_start NULLS LAST,
                  CASE priority WHEN 'emergency' THEN 0 WHEN 'urgent' THEN 1 WHEN 'priority' THEN 2 ELSE 3 END, created_at
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
    Ok(rows.iter().map(case_from_row).collect())
}

#[derive(Debug, Deserialize)]
pub struct CaseInput {
    pub episode_id: Uuid,
    pub procedure_code: String,
    pub responsible_service_code: String,
    pub responsible_user_id: Option<Uuid>,
    pub priority: Option<String>,
    pub expected_duration_minutes: i32,
    #[serde(default)]
    pub required_professionals: Value,
    #[serde(default)]
    pub required_equipment_codes: Vec<String>,
    #[serde(default)]
    pub prerequisites: Vec<Prerequisite>,
    #[serde(default)]
    pub required_service_request_ids: Vec<Uuid>,
    #[serde(default)]
    pub checklist: Value,
    #[serde(default)]
    pub post_procedure_requirements: Value,
}

#[allow(clippy::too_many_arguments)]
async fn record_case_history(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    case_id: Uuid,
    from: Option<&str>,
    to: &str,
    change_kind: &str,
    reason: Option<&str>,
    override_: bool,
    note: Option<&str>,
    version: i64,
) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO perioperative_case_history
         (id, tenant_id, case_id, from_status, to_status, change_kind, reason_code, override, note, actor, version)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
    )
    .bind(Uuid::now_v7())
    .bind(ctx.tenant_id)
    .bind(case_id)
    .bind(from)
    .bind(to)
    .bind(change_kind)
    .bind(reason)
    .bind(override_)
    .bind(note)
    .bind(format!("user:{}", ctx.user_id))
    .bind(version)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

pub async fn create_case(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    input: &CaseInput,
) -> Result<CaseRow, ApiError> {
    let ep = load_episode(conn, ctx.tenant_id, input.episode_id, false).await?;
    if !ep.status_enum().is_open() {
        return Err(ApiError::conflict(
            "episode_closed",
            "the episode is not open",
        ));
    }
    let priority = input.priority.as_deref().unwrap_or(&ep.priority);
    if !super::PRIORITIES.contains(&priority) {
        return Err(ApiError::bad_request(
            "invalid_priority",
            "unknown priority",
        ));
    }
    if !(5..=1440).contains(&input.expected_duration_minutes) {
        return Err(ApiError::bad_request(
            "invalid_duration",
            "expected duration must be 5..=1440 minutes",
        ));
    }
    if !super::catalog_code_exists(conn, ctx.tenant_id, "procedure", &input.procedure_code).await? {
        return Err(ApiError::bad_request(
            "unknown_procedure",
            "procedure is not an active catalog entry",
        ));
    }
    let empty_array = Value::Array(vec![]);
    let empty_object = Value::Object(Default::default());
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO perioperative_cases
         (id, tenant_id, facility_id, episode_id, patient_id, procedure_code, responsible_service_code, responsible_user_id,
          priority, expected_duration_minutes, required_professionals, required_equipment_codes, prerequisites,
          required_service_request_ids, checklist, post_procedure_requirements, created_by)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17)",
    )
    .bind(id)
    .bind(ctx.tenant_id)
    .bind(ep.facility_id)
    .bind(ep.id)
    .bind(ep.patient_id)
    .bind(&input.procedure_code)
    .bind(&input.responsible_service_code)
    .bind(input.responsible_user_id)
    .bind(priority)
    .bind(input.expected_duration_minutes)
    .bind(if input.required_professionals.is_array() { &input.required_professionals } else { &empty_array })
    .bind(&input.required_equipment_codes)
    .bind(serde_json::to_value(&input.prerequisites).unwrap_or(json!([])))
    .bind(&input.required_service_request_ids)
    .bind(if input.checklist.is_array() { &input.checklist } else { &empty_array })
    .bind(if input.post_procedure_requirements.is_object() { &input.post_procedure_requirements } else { &empty_object })
    .bind(ctx.user_id)
    .execute(&mut *conn)
    .await?;
    record_case_history(
        conn,
        ctx,
        id,
        None,
        "requested",
        "created",
        None,
        false,
        None,
        1,
    )
    .await?;
    load_case(conn, ctx.tenant_id, id, false).await
}

/// Load the deterministic facts for scheduling `case` into `theatre` at
/// `proposed`, as the database stands now.
pub async fn load_theatre_facts(
    conn: &mut PgConnection,
    c: &CaseRow,
    theatre_id: Uuid,
    proposed: Interval,
    expected_case_version: i64,
    expected_theatre_version: i64,
    team: &[Uuid],
) -> Result<TheatreCaseFacts, ApiError> {
    let theatre = load_location(conn, c.tenant_id, theatre_id, false).await?;
    if theatre.facility_id != c.facility_id {
        return Err(ApiError::bad_request(
            "facility_scope",
            "the theatre belongs to another facility",
        ));
    }
    let policy = scheduling::load_policy(conn, c.tenant_id).await?;
    let closure = scheduling::active_closure_over(
        conn,
        c.tenant_id,
        c.facility_id,
        proposed.starts_at,
        proposed.ends_at,
        &policy.time_zone,
    )
    .await?;
    let period: Option<String> = sqlx::query_scalar(
        "SELECT kind FROM operational_location_periods WHERE active AND location_id = $1 AND starts_at < $3 AND (ends_at IS NULL OR ends_at > $2) LIMIT 1",
    )
    .bind(theatre_id)
    .bind(proposed.starts_at)
    .bind(proposed.ends_at)
    .fetch_optional(&mut *conn)
    .await?;
    let theatre_open = theatre.active
        && theatre.operational_status == "open"
        && theatre.position_state.as_deref() != Some("out_of_service")
        && theatre.position_state.as_deref() != Some("blocked")
        && period.is_none();
    let session = sqlx::query(
        "SELECT starts_at, ends_at, turnaround_minutes FROM theatre_sessions
         WHERE theatre_location_id = $1 AND status = 'planned' AND starts_at <= $2 AND ends_at >= $3 LIMIT 1",
    )
    .bind(theatre_id)
    .bind(proposed.starts_at)
    .bind(proposed.ends_at)
    .fetch_optional(&mut *conn)
    .await?;
    let (session, session_turnaround) = match session {
        Some(r) => (
            Some(Interval {
                starts_at: r.get("starts_at"),
                ends_at: r.get("ends_at"),
            }),
            r.get::<i32, _>("turnaround_minutes") as i64,
        ),
        None => (None, 0),
    };
    let others = sqlx::query(
        "SELECT id, scheduled_start, scheduled_end, version FROM perioperative_cases
         WHERE theatre_location_id = $1 AND id <> $2 AND status IN ('scheduled','in_theatre','delayed')
           AND scheduled_start < $4 AND scheduled_end > $3",
    )
    .bind(theatre_id)
    .bind(c.id)
    .bind(proposed.starts_at - Duration::hours(12))
    .bind(proposed.ends_at + Duration::hours(12))
    .fetch_all(&mut *conn)
    .await?;
    let theatre_cases = others
        .iter()
        .map(|r| {
            (
                r.get::<Uuid, _>("id"),
                Interval {
                    starts_at: r.get("scheduled_start"),
                    ends_at: r.get("scheduled_end"),
                },
                r.get::<i64, _>("version"),
            )
        })
        .collect();
    let commitments = sqlx::query(
        "SELECT unnest(team_user_ids) AS user_id, scheduled_start, scheduled_end FROM perioperative_cases
         WHERE tenant_id = $1 AND id <> $2 AND status IN ('scheduled','in_theatre','delayed')
           AND team_user_ids && $3 AND scheduled_start < $5 AND scheduled_end > $4",
    )
    .bind(c.tenant_id)
    .bind(c.id)
    .bind(team)
    .bind(proposed.starts_at)
    .bind(proposed.ends_at)
    .fetch_all(&mut *conn)
    .await?;
    let team_commitments = commitments
        .iter()
        .map(|r| {
            (
                r.get::<Uuid, _>("user_id"),
                Interval {
                    starts_at: r.get("scheduled_start"),
                    ends_at: r.get("scheduled_end"),
                },
            )
        })
        .collect();
    let required_results_complete = if c.required_service_request_ids.is_empty() {
        true
    } else {
        let pending: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM unnest($1::uuid[]) AS req(id)
             WHERE NOT EXISTS (SELECT 1 FROM diagnostic_reports d WHERE d.service_request_id = req.id AND d.status IN ('final','amended'))",
        )
        .bind(&c.required_service_request_ids)
        .fetch_one(&mut *conn)
        .await?;
        pending == 0
    };
    let destination_required = c
        .post_procedure_requirements
        .get("destination_required")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let destination_feasible_count = if destination_required {
        let codes: Vec<String> = c
            .post_procedure_requirements
            .get("destination_type_codes")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM operational_locations WHERE tenant_id = $1 AND facility_id = $2 AND level = 'position'
               AND active AND operational_status = 'open' AND position_state = 'available'
               AND (cardinality($3::text[]) = 0 OR type_code = ANY($3))",
        )
        .bind(c.tenant_id)
        .bind(c.facility_id)
        .bind(&codes)
        .fetch_one(&mut *conn)
        .await? as usize
    } else {
        0
    };
    Ok(TheatreCaseFacts {
        case_id: c.id,
        case_version: c.version,
        expected_case_version,
        theatre_location_id: theatre_id,
        theatre_version: theatre.version,
        expected_theatre_version,
        theatre_open,
        facility_closed: closure.is_some(),
        proposed,
        turnaround_minutes: session_turnaround.max(theatre.cleaning_minutes as i64),
        minimum_turnaround_minutes: theatre.cleaning_minutes as i64,
        session,
        theatre_cases,
        team_user_ids: team.to_vec(),
        team_commitments,
        required_equipment_codes: c.required_equipment_codes.clone(),
        theatre_equipment_codes: theatre.equipment_codes.clone(),
        prerequisites: c.prerequisites_typed(),
        required_results_complete,
        destination_required,
        destination_feasible_count,
    })
}

#[derive(Debug, Deserialize)]
pub struct ScheduleInput {
    pub expected_version: i64,
    pub theatre_location_id: Uuid,
    pub expected_theatre_version: i64,
    pub scheduled_start: DateTime<Utc>,
    #[serde(default)]
    pub team_user_ids: Vec<Uuid>,
    /// Required to schedule over non-blocking conflicts.
    pub override_reason: Option<String>,
}

/// Readiness check without mutation.
pub async fn readiness(
    conn: &mut PgConnection,
    c: &CaseRow,
    theatre_id: Option<Uuid>,
    start: Option<DateTime<Utc>>,
) -> Result<Option<TheatreCheck>, ApiError> {
    let (Some(theatre_id), Some(start)) = (
        theatre_id.or(c.theatre_location_id),
        start.or(c.scheduled_start),
    ) else {
        return Ok(None);
    };
    let theatre = load_location(conn, c.tenant_id, theatre_id, false).await?;
    let proposed = Interval {
        starts_at: start,
        ends_at: start + Duration::minutes(c.expected_duration_minutes as i64),
    };
    let facts = load_theatre_facts(
        conn,
        c,
        theatre_id,
        proposed,
        c.version,
        theatre.version,
        &c.team_user_ids,
    )
    .await?;
    Ok(Some(domain::check_theatre(&facts)))
}

pub async fn schedule(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    case_id: Uuid,
    input: &ScheduleInput,
) -> Result<(CaseRow, TheatreCheck), ApiError> {
    let c = load_case(conn, ctx.tenant_id, case_id, true).await?;
    if c.version != input.expected_version {
        return Err(stale("case"));
    }
    let from = c.status_enum();
    let to = from
        .apply(CaseTransition::Schedule)
        .map_err(invalid_transition)?;
    let proposed = Interval {
        starts_at: input.scheduled_start,
        ends_at: input.scheduled_start + Duration::minutes(c.expected_duration_minutes as i64),
    };
    // Lock the theatre row so concurrent schedules serialise.
    let theatre = load_location(conn, ctx.tenant_id, input.theatre_location_id, true).await?;
    let facts = load_theatre_facts(
        conn,
        &c,
        input.theatre_location_id,
        proposed.clone(),
        input.expected_version,
        input.expected_theatre_version,
        &input.team_user_ids,
    )
    .await?;
    let check = domain::check_theatre(&facts);
    let blocking: Vec<&str> = check
        .conflicts
        .iter()
        .filter(|x| x.blocking)
        .map(|x| x.code.as_str())
        .collect();
    if blocking.iter().any(|c| c.starts_with("stale_")) {
        return Err(stale("theatre or case"));
    }
    if !blocking.is_empty() {
        return Err(ApiError::conflict(
            "theatre_conflict",
            format!("the case cannot be scheduled: {}", blocking.join(",")),
        )
        .with_details(json!({
            "engine_version": check.engine_version,
            "facts_hash": check.facts_hash,
            "conflicts": check.conflicts,
        })));
    }
    let overridden = !check.feasible;
    if overridden
        && input
            .override_reason
            .as_deref()
            .map(str::trim)
            .unwrap_or("")
            .is_empty()
    {
        return Err(ApiError::conflict(
            "override_reason_required",
            format!(
                "non-blocking conflicts need an explicit override reason: {}",
                check
                    .conflicts
                    .iter()
                    .map(|x| x.code.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            ),
        ));
    }
    let booking_id = match theatre.schedulable_resource_id {
        Some(res) => {
            let id = Uuid::now_v7();
            sqlx::query(
                "INSERT INTO resource_bookings (id, tenant_id, resource_id, slot_index, starts_at, ends_at, kind, status, created_by)
                 VALUES ($1,$2,$3,0,$4,$5,'theatre_case','active',$6)",
            )
            .bind(id)
            .bind(ctx.tenant_id)
            .bind(res)
            .bind(proposed.starts_at)
            .bind(proposed.ends_at)
            .bind(ctx.user_id)
            .execute(&mut *conn)
            .await
            .map_err(|e| match &e {
                sqlx::Error::Database(d) if d.code().as_deref() == Some("23P01") => {
                    ApiError::conflict("theatre_conflict", "the theatre resource is already booked for this interval")
                }
                _ => ApiError::from(e),
            })?;
            Some(id)
        }
        None => None,
    };
    if let Some(old) = c.booking_id {
        sqlx::query("UPDATE resource_bookings SET status = 'released', released_at = now() WHERE id = $1 AND status = 'active'").bind(old).execute(&mut *conn).await?;
    }
    let session_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM theatre_sessions WHERE theatre_location_id = $1 AND status = 'planned' AND starts_at <= $2 AND ends_at >= $3 LIMIT 1",
    )
    .bind(theatre.id)
    .bind(proposed.starts_at)
    .bind(proposed.ends_at)
    .fetch_optional(&mut *conn)
    .await?;
    sqlx::query(
        "UPDATE perioperative_cases SET status = $2, theatre_location_id = $3, scheduled_start = $4, scheduled_end = $5,
             session_id = $6, booking_id = $7, team_user_ids = $8, override_reason = $9, delay_reason_code = NULL,
             version = version + 1, updated_at = now()
         WHERE id = $1",
    )
    .bind(c.id)
    .bind(to.as_str())
    .bind(theatre.id)
    .bind(proposed.starts_at)
    .bind(proposed.ends_at)
    .bind(session_id)
    .bind(booking_id)
    .bind(&input.team_user_ids)
    .bind(if overridden { input.override_reason.as_deref() } else { None })
    .execute(&mut *conn)
    .await?;
    record_case_history(
        conn,
        ctx,
        c.id,
        Some(from.as_str()),
        to.as_str(),
        "scheduled",
        None,
        overridden,
        input.override_reason.as_deref(),
        c.version + 1,
    )
    .await?;
    Ok((load_case(conn, ctx.tenant_id, c.id, false).await?, check))
}

#[derive(Debug, Deserialize)]
pub struct CaseTransitionInput {
    pub transition: String,
    pub expected_version: i64,
    pub reason_code: Option<String>,
    pub note: Option<String>,
    /// Prerequisite updates applied before the transition: [{code,status,evidence_ref}]
    #[serde(default)]
    pub prerequisites: Vec<Prerequisite>,
    #[serde(default)]
    pub checklist: Option<Value>,
}

pub async fn transition_case(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    case_id: Uuid,
    input: &CaseTransitionInput,
) -> Result<CaseRow, ApiError> {
    let c = load_case(conn, ctx.tenant_id, case_id, true).await?;
    if c.version != input.expected_version {
        return Err(stale("case"));
    }
    let tr = CaseTransition::parse(&input.transition)
        .ok_or_else(|| ApiError::bad_request("invalid_transition", "unknown transition"))?;
    if tr == CaseTransition::Schedule {
        return Err(ApiError::bad_request(
            "use_schedule",
            "scheduling goes through the schedule endpoint",
        ));
    }
    let from = c.status_enum();
    let to = from.apply(tr).map_err(invalid_transition)?;
    if matches!(tr, CaseTransition::Delay | CaseTransition::Cancel) && input.reason_code.is_none() {
        return Err(ApiError::bad_request(
            "reason_required",
            "delays and cancellations need a reason code",
        ));
    }
    let mut prereqs = c.prerequisites_typed();
    for upd in &input.prerequisites {
        match prereqs.iter_mut().find(|p| p.code == upd.code) {
            Some(p) => {
                p.status = upd.status.clone();
                p.evidence_ref = upd.evidence_ref.clone();
            }
            None => prereqs.push(upd.clone()),
        }
    }
    if tr == CaseTransition::MarkReady
        && prereqs
            .iter()
            .any(|p| p.status != "met" && p.status != "waived")
    {
        return Err(ApiError::conflict(
            "missing_prerequisite",
            format!(
                "prerequisites not met: {}",
                prereqs
                    .iter()
                    .filter(|p| p.status != "met" && p.status != "waived")
                    .map(|p| p.code.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            ),
        ));
    }
    let releases_theatre = from.holds_theatre() && !to.holds_theatre();
    if releases_theatre {
        if let Some(b) = c.booking_id {
            sqlx::query("UPDATE resource_bookings SET status = 'released', released_at = now() WHERE id = $1 AND status = 'active'").bind(b).execute(&mut *conn).await?;
        }
    }
    sqlx::query(
        "UPDATE perioperative_cases SET status = $2, prerequisites = $3, checklist = COALESCE($4, checklist),
             delay_reason_code = CASE WHEN $5 = 'delayed' THEN $6 ELSE delay_reason_code END,
             cancellation_reason_code = CASE WHEN $5 = 'cancelled' THEN $6 ELSE cancellation_reason_code END,
             booking_id = CASE WHEN $7 THEN NULL ELSE booking_id END,
             version = version + 1, updated_at = now()
         WHERE id = $1",
    )
    .bind(c.id)
    .bind(to.as_str())
    .bind(serde_json::to_value(&prereqs).unwrap_or(json!([])))
    .bind(&input.checklist)
    .bind(to.as_str())
    .bind(&input.reason_code)
    .bind(releases_theatre)
    .execute(&mut *conn)
    .await?;
    record_case_history(
        conn,
        ctx,
        c.id,
        Some(from.as_str()),
        to.as_str(),
        "transition",
        input.reason_code.as_deref(),
        false,
        input.note.as_deref(),
        c.version + 1,
    )
    .await?;
    load_case(conn, ctx.tenant_id, c.id, false).await
}

pub async fn case_history(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    case_id: Uuid,
) -> Result<Vec<Value>, ApiError> {
    let rows = sqlx::query(
        "SELECT from_status, to_status, change_kind, reason_code, override, note, actor, version, recorded_at
         FROM perioperative_case_history WHERE tenant_id = $1 AND case_id = $2 ORDER BY recorded_at, version",
    )
    .bind(tenant_id)
    .bind(case_id)
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
                "override": r.get::<bool, _>("override"),
                "note": r.get::<Option<String>, _>("note"),
                "actor": r.get::<String, _>("actor"),
                "version": r.get::<i64, _>("version"),
                "recorded_at": r.get::<DateTime<Utc>, _>("recorded_at"),
            })
        })
        .collect())
}
