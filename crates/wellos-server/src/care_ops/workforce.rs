//! Workforce shifts, coverage requirements, assignments and absences.
//! Coverage is evaluated deterministically per shift; dMind may only
//! explain pressure or suggest capability-matching reallocations that a
//! human then applies through `change_assignment`.

use super::{catalog_code_exists, load_location, stale};
use crate::auth::AuthContext;
use crate::error::ApiError;
use chrono::{DateTime, NaiveDate, NaiveTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use uuid::Uuid;
use wellos_domain::care_ops::{self as domain, CoverageReport, CoverageRequirement, StaffFact};

#[derive(Debug, Clone, Serialize)]
pub struct ShiftRow {
    pub id: Uuid,
    pub facility_id: Uuid,
    pub unit_location_id: Option<Uuid>,
    pub label: String,
    pub local_date: NaiveDate,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub status: String,
    pub version: i64,
}

const SHIFT_COLS: &str =
    "id, facility_id, unit_location_id, label, local_date, starts_at, ends_at, status, version";

fn shift_from_row(r: &PgRow) -> ShiftRow {
    ShiftRow {
        id: r.get("id"),
        facility_id: r.get("facility_id"),
        unit_location_id: r.get("unit_location_id"),
        label: r.get("label"),
        local_date: r.get("local_date"),
        starts_at: r.get("starts_at"),
        ends_at: r.get("ends_at"),
        status: r.get("status"),
        version: r.get("version"),
    }
}

#[derive(Debug, Deserialize)]
pub struct RequirementInput {
    pub profession_code: String,
    #[serde(default)]
    pub capability_codes: Vec<String>,
    #[serde(default)]
    pub specialty_codes: Vec<String>,
    pub minimum_count: i32,
}

#[derive(Debug, Deserialize)]
pub struct ShiftInput {
    pub facility_id: Uuid,
    pub unit_location_id: Option<Uuid>,
    pub label: String,
    /// Local wall-clock start/end in the facility time zone; the end may be
    /// on the following day. DST transitions are resolved by the zone.
    pub local_date: NaiveDate,
    pub local_start: NaiveTime,
    pub local_end: NaiveTime,
    #[serde(default)]
    pub requirements: Vec<RequirementInput>,
}

pub async fn facility_tz(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    facility_id: Uuid,
) -> Result<chrono_tz::Tz, ApiError> {
    let tz: Option<String> =
        sqlx::query_scalar("SELECT time_zone FROM facility_scheduling WHERE facility_id = $1")
            .bind(facility_id)
            .fetch_optional(&mut *conn)
            .await?;
    let tz = match tz {
        Some(t) => t,
        None => {
            crate::scheduling::load_policy(conn, tenant_id)
                .await?
                .time_zone
        }
    };
    tz.parse()
        .map_err(|_| ApiError::internal("facility time zone is not a valid IANA zone"))
}

/// Resolve a local wall-clock time on `date` in `tz`; a non-existent time
/// (spring-forward gap) moves to the next valid instant, an ambiguous one
/// (fall-back) takes the earlier instant.
pub fn resolve_local(tz: chrono_tz::Tz, date: NaiveDate, time: NaiveTime) -> DateTime<Utc> {
    let naive = date.and_time(time);
    match tz.from_local_datetime(&naive) {
        chrono::LocalResult::Single(t) => t.with_timezone(&Utc),
        chrono::LocalResult::Ambiguous(a, _) => a.with_timezone(&Utc),
        chrono::LocalResult::None => {
            let shifted = naive + chrono::Duration::hours(1);
            tz.from_local_datetime(&shifted)
                .earliest()
                .map(|t| t.with_timezone(&Utc))
                .unwrap_or_else(|| Utc.from_utc_datetime(&naive))
        }
    }
}

pub async fn create_shift(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    input: &ShiftInput,
) -> Result<ShiftRow, ApiError> {
    if input.label.trim().is_empty() || input.label.len() > 80 {
        return Err(ApiError::bad_request(
            "invalid_label",
            "shift label is required (max 80 chars)",
        ));
    }
    if let Some(u) = input.unit_location_id {
        let loc = load_location(conn, ctx.tenant_id, u, false).await?;
        if loc.facility_id != input.facility_id {
            return Err(ApiError::bad_request(
                "facility_scope",
                "the unit belongs to another facility",
            ));
        }
    }
    let tz = facility_tz(conn, ctx.tenant_id, input.facility_id).await?;
    let starts_at = resolve_local(tz, input.local_date, input.local_start);
    let end_date = if input.local_end <= input.local_start {
        input.local_date.succ_opt().unwrap_or(input.local_date)
    } else {
        input.local_date
    };
    let ends_at = resolve_local(tz, end_date, input.local_end);
    if ends_at <= starts_at {
        return Err(ApiError::bad_request(
            "invalid_interval",
            "shift must end after it starts",
        ));
    }
    for r in &input.requirements {
        if !catalog_code_exists(conn, ctx.tenant_id, "profession", &r.profession_code).await? {
            return Err(ApiError::bad_request(
                "unknown_profession",
                format!("{} is not an active profession", r.profession_code),
            ));
        }
        if !(0..=1000).contains(&r.minimum_count) {
            return Err(ApiError::bad_request(
                "invalid_minimum",
                "minimum count must be 0..=1000",
            ));
        }
    }
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO workforce_shifts (id, tenant_id, facility_id, unit_location_id, label, local_date, starts_at, ends_at, created_by)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)",
    )
    .bind(id)
    .bind(ctx.tenant_id)
    .bind(input.facility_id)
    .bind(input.unit_location_id)
    .bind(input.label.trim())
    .bind(input.local_date)
    .bind(starts_at)
    .bind(ends_at)
    .bind(ctx.user_id)
    .execute(&mut *conn)
    .await?;
    for r in &input.requirements {
        sqlx::query(
            "INSERT INTO workforce_coverage_requirements (id, tenant_id, shift_id, profession_code, capability_codes, specialty_codes, minimum_count)
             VALUES ($1,$2,$3,$4,$5,$6,$7) ON CONFLICT DO NOTHING",
        )
        .bind(Uuid::now_v7())
        .bind(ctx.tenant_id)
        .bind(id)
        .bind(&r.profession_code)
        .bind(&r.capability_codes)
        .bind(&r.specialty_codes)
        .bind(r.minimum_count)
        .execute(&mut *conn)
        .await?;
    }
    load_shift(conn, ctx.tenant_id, id, false).await
}

pub async fn load_shift(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    id: Uuid,
    lock: bool,
) -> Result<ShiftRow, ApiError> {
    let sql = format!(
        "SELECT {SHIFT_COLS} FROM workforce_shifts WHERE tenant_id = $1 AND id = $2 {}",
        if lock { "FOR UPDATE" } else { "" }
    );
    let row = sqlx::query(&sql)
        .bind(tenant_id)
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(ApiError::not_found)?;
    Ok(shift_from_row(&row))
}

pub async fn list_shifts(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    facility_ids: Option<&[Uuid]>,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<Vec<ShiftRow>, ApiError> {
    let sql = format!(
        "SELECT {SHIFT_COLS} FROM workforce_shifts
         WHERE tenant_id = $1 AND ($2::uuid[] IS NULL OR facility_id = ANY($2)) AND status <> 'cancelled'
           AND starts_at < $4 AND ends_at > $3
         ORDER BY starts_at, label"
    );
    let rows = sqlx::query(&sql)
        .bind(tenant_id)
        .bind(facility_ids)
        .bind(from)
        .bind(to)
        .fetch_all(&mut *conn)
        .await?;
    Ok(rows.iter().map(shift_from_row).collect())
}

#[derive(Debug, Clone, Serialize)]
pub struct AssignmentRow {
    pub id: Uuid,
    pub shift_id: Uuid,
    pub user_id: Option<Uuid>,
    pub resource_id: Option<Uuid>,
    pub profession_code: String,
    pub capability_codes: Vec<String>,
    pub specialty_codes: Vec<String>,
    pub role_in_shift: String,
    pub status: String,
    pub override_reason: Option<String>,
    pub version: i64,
    pub absent: bool,
}

pub async fn shift_assignments(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    shift_id: Uuid,
) -> Result<Vec<AssignmentRow>, ApiError> {
    let rows = sqlx::query(
        "SELECT a.id, a.shift_id, a.user_id, a.resource_id, a.profession_code, a.capability_codes, a.specialty_codes,
                a.role_in_shift, a.status, a.override_reason, a.version,
                EXISTS (SELECT 1 FROM workforce_shift_absences ab JOIN workforce_shifts s ON s.id = a.shift_id
                        WHERE ab.user_id = a.user_id AND ab.starts_at < s.ends_at AND ab.ends_at > s.starts_at) AS absent
         FROM workforce_shift_assignments a WHERE a.tenant_id = $1 AND a.shift_id = $2 ORDER BY a.profession_code, a.created_at",
    )
    .bind(tenant_id)
    .bind(shift_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .iter()
        .map(|r| AssignmentRow {
            id: r.get("id"),
            shift_id: r.get("shift_id"),
            user_id: r.get("user_id"),
            resource_id: r.get("resource_id"),
            profession_code: r.get("profession_code"),
            capability_codes: r.get("capability_codes"),
            specialty_codes: r.get("specialty_codes"),
            role_in_shift: r.get("role_in_shift"),
            status: r.get("status"),
            override_reason: r.get("override_reason"),
            version: r.get("version"),
            absent: r.get("absent"),
        })
        .collect())
}

pub async fn shift_requirements(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    shift_id: Uuid,
) -> Result<Vec<CoverageRequirement>, ApiError> {
    let rows = sqlx::query(
        "SELECT profession_code, capability_codes, specialty_codes, minimum_count FROM workforce_coverage_requirements
         WHERE tenant_id = $1 AND shift_id = $2 ORDER BY profession_code",
    )
    .bind(tenant_id)
    .bind(shift_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .iter()
        .map(|r| CoverageRequirement {
            profession_code: r.get("profession_code"),
            capability_codes: r.get("capability_codes"),
            specialty_codes: r.get("specialty_codes"),
            minimum_count: r.get::<i32, _>("minimum_count") as i64,
        })
        .collect())
}

pub fn staff_facts(assignments: &[AssignmentRow]) -> Vec<StaffFact> {
    assignments
        .iter()
        .map(|a| StaffFact {
            assignment_id: a.id,
            user_id: a.user_id,
            profession_code: a.profession_code.clone(),
            capability_codes: a.capability_codes.clone(),
            specialty_codes: a.specialty_codes.clone(),
            role_in_shift: a.role_in_shift.clone(),
            status: a.status.clone(),
            absent: a.absent,
        })
        .collect()
}

pub struct ShiftCoverage {
    pub shift: ShiftRow,
    pub requirements: Vec<CoverageRequirement>,
    pub assignments: Vec<AssignmentRow>,
    pub report: CoverageReport,
    /// Per gap: feasible float/on-call reallocation candidates (assignment ids).
    pub reallocations: Vec<Vec<Uuid>>,
}

pub async fn coverage(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    shift_id: Uuid,
) -> Result<ShiftCoverage, ApiError> {
    let shift = load_shift(conn, tenant_id, shift_id, false).await?;
    let requirements = shift_requirements(conn, tenant_id, shift_id).await?;
    let assignments = shift_assignments(conn, tenant_id, shift_id).await?;
    let staff = staff_facts(&assignments);
    let report = domain::evaluate_coverage(&requirements, &staff);
    // Float/on-call staff from overlapping shifts of the same facility widen the pool.
    let pool_rows = sqlx::query(
        "SELECT a.id, a.user_id, a.profession_code, a.capability_codes, a.specialty_codes, a.role_in_shift, a.status,
                EXISTS (SELECT 1 FROM workforce_shift_absences ab WHERE ab.user_id = a.user_id AND ab.starts_at < $4 AND ab.ends_at > $3) AS absent
         FROM workforce_shift_assignments a JOIN workforce_shifts s ON s.id = a.shift_id
         WHERE a.tenant_id = $1 AND s.facility_id = $2 AND s.id <> $5 AND s.status IN ('planned','active')
           AND a.role_in_shift IN ('float','on_call') AND a.status IN ('planned','confirmed')
           AND s.starts_at < $4 AND s.ends_at > $3",
    )
    .bind(tenant_id)
    .bind(shift.facility_id)
    .bind(shift.starts_at)
    .bind(shift.ends_at)
    .bind(shift.id)
    .fetch_all(&mut *conn)
    .await?;
    let mut pool = staff.clone();
    pool.extend(pool_rows.iter().map(|r| StaffFact {
        assignment_id: r.get("id"),
        user_id: r.get("user_id"),
        profession_code: r.get("profession_code"),
        capability_codes: r.get("capability_codes"),
        specialty_codes: r.get("specialty_codes"),
        role_in_shift: r.get("role_in_shift"),
        status: r.get("status"),
        absent: r.get("absent"),
    }));
    let reallocations = report
        .gaps
        .iter()
        .map(|g| {
            domain::reallocation_candidates(g, &pool)
                .iter()
                .map(|s| s.assignment_id)
                .collect()
        })
        .collect();
    Ok(ShiftCoverage {
        shift,
        requirements,
        assignments,
        report,
        reallocations,
    })
}

#[derive(Debug, Deserialize)]
pub struct AssignmentInput {
    pub user_id: Option<Uuid>,
    pub resource_id: Option<Uuid>,
    pub profession_code: String,
    #[serde(default)]
    pub capability_codes: Vec<String>,
    #[serde(default)]
    pub specialty_codes: Vec<String>,
    pub role_in_shift: Option<String>,
    /// Required when the assignee's profession does not match a requirement
    /// of the shift (deliberate human override).
    pub override_reason: Option<String>,
}

pub async fn add_assignment(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    shift_id: Uuid,
    input: &AssignmentInput,
) -> Result<AssignmentRow, ApiError> {
    let shift = load_shift(conn, ctx.tenant_id, shift_id, true).await?;
    if shift.status == "closed" || shift.status == "cancelled" {
        return Err(ApiError::conflict("shift_closed", "the shift is closed"));
    }
    if input.user_id.is_some() == input.resource_id.is_some() {
        return Err(ApiError::bad_request(
            "assignee_required",
            "exactly one of user_id or resource_id is required",
        ));
    }
    if let Some(u) = input.user_id {
        let ok: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM users WHERE tenant_id = $1 AND id = $2")
                .bind(ctx.tenant_id)
                .bind(u)
                .fetch_optional(&mut *conn)
                .await?;
        if ok.is_none() {
            return Err(ApiError::not_found());
        }
    }
    let role = input.role_in_shift.as_deref().unwrap_or("assigned");
    if !["assigned", "on_call", "float"].contains(&role) {
        return Err(ApiError::bad_request(
            "invalid_role",
            "role must be assigned, on_call or float",
        ));
    }
    if !catalog_code_exists(conn, ctx.tenant_id, "profession", &input.profession_code).await? {
        return Err(ApiError::bad_request(
            "unknown_profession",
            "profession is not an active catalog entry",
        ));
    }
    let reqs = shift_requirements(conn, ctx.tenant_id, shift_id).await?;
    let matches_requirement = reqs.is_empty()
        || reqs.iter().any(|r| {
            r.profession_code == input.profession_code
                && r.capability_codes
                    .iter()
                    .all(|c| input.capability_codes.contains(c))
                && (r.specialty_codes.is_empty()
                    || r.specialty_codes
                        .iter()
                        .any(|s| input.specialty_codes.contains(s)))
        });
    if !matches_requirement
        && role == "assigned"
        && input
            .override_reason
            .as_deref()
            .map(str::trim)
            .unwrap_or("")
            .is_empty()
    {
        return Err(ApiError::conflict(
            "capability_mismatch",
            "the assignee does not satisfy any coverage requirement of this shift; an override reason is required",
        ));
    }
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO workforce_shift_assignments (id, tenant_id, shift_id, user_id, resource_id, profession_code, capability_codes,
             specialty_codes, role_in_shift, override_reason, created_by)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
    )
    .bind(id)
    .bind(ctx.tenant_id)
    .bind(shift_id)
    .bind(input.user_id)
    .bind(input.resource_id)
    .bind(&input.profession_code)
    .bind(&input.capability_codes)
    .bind(&input.specialty_codes)
    .bind(role)
    .bind(if matches_requirement { None } else { input.override_reason.as_deref() })
    .bind(ctx.user_id)
    .execute(&mut *conn)
    .await?;
    sqlx::query(
        "UPDATE workforce_shifts SET version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(shift_id)
    .execute(&mut *conn)
    .await?;
    shift_assignments(conn, ctx.tenant_id, shift_id)
        .await?
        .into_iter()
        .find(|a| a.id == id)
        .ok_or_else(ApiError::not_found)
}

#[derive(Debug, Deserialize)]
pub struct AssignmentChangeInput {
    pub expected_version: i64,
    pub status: String,
    /// Reassignment target shift (human-approved reallocation).
    pub target_shift_id: Option<Uuid>,
    pub reason: Option<String>,
}

pub const ASSIGNMENT_STATUSES: &[&str] =
    &["planned", "confirmed", "absent", "reassigned", "cancelled"];

pub async fn change_assignment(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    assignment_id: Uuid,
    input: &AssignmentChangeInput,
) -> Result<Value, ApiError> {
    if !ASSIGNMENT_STATUSES.contains(&input.status.as_str()) {
        return Err(ApiError::bad_request(
            "invalid_status",
            "unknown assignment status",
        ));
    }
    let row = sqlx::query(
        "SELECT shift_id, version, status, user_id, resource_id, profession_code, capability_codes, specialty_codes, role_in_shift
         FROM workforce_shift_assignments WHERE tenant_id = $1 AND id = $2 FOR UPDATE",
    )
    .bind(ctx.tenant_id)
    .bind(assignment_id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(ApiError::not_found)?;
    let version: i64 = row.get("version");
    if version != input.expected_version {
        return Err(stale("assignment"));
    }
    let shift_id: Uuid = row.get("shift_id");
    let current: String = row.get("status");
    if current == "cancelled" || current == "reassigned" {
        return Err(ApiError::conflict(
            "assignment_closed",
            "the assignment is already closed",
        ));
    }
    if input.status == "reassigned" {
        let target = input.target_shift_id.ok_or_else(|| {
            ApiError::bad_request("target_required", "a reassignment names the target shift")
        })?;
        if input
            .reason
            .as_deref()
            .map(str::trim)
            .unwrap_or("")
            .is_empty()
        {
            return Err(ApiError::bad_request(
                "reason_required",
                "a reassignment needs a reason",
            ));
        }
        let target_shift = load_shift(conn, ctx.tenant_id, target, true).await?;
        let reqs = shift_requirements(conn, ctx.tenant_id, target).await?;
        let prof: String = row.get("profession_code");
        let caps: Vec<String> = row.get("capability_codes");
        let specs: Vec<String> = row.get("specialty_codes");
        let fits = reqs.is_empty()
            || reqs.iter().any(|r| {
                r.profession_code == prof
                    && r.capability_codes.iter().all(|c| caps.contains(c))
                    && (r.specialty_codes.is_empty()
                        || r.specialty_codes.iter().any(|s| specs.contains(s)))
            });
        if !fits {
            return Err(ApiError::conflict(
                "capability_mismatch",
                "the professional does not satisfy the target shift's requirements",
            ));
        }
        let new_id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO workforce_shift_assignments (id, tenant_id, shift_id, user_id, resource_id, profession_code, capability_codes,
                 specialty_codes, role_in_shift, status, override_reason, created_by)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,'assigned','confirmed',$9,$10)",
        )
        .bind(new_id)
        .bind(ctx.tenant_id)
        .bind(target_shift.id)
        .bind(row.get::<Option<Uuid>, _>("user_id"))
        .bind(row.get::<Option<Uuid>, _>("resource_id"))
        .bind(&prof)
        .bind(&caps)
        .bind(&specs)
        .bind(&input.reason)
        .bind(ctx.user_id)
        .execute(&mut *conn)
        .await?;
        sqlx::query(
            "UPDATE workforce_shifts SET version = version + 1, updated_at = now() WHERE id = $1",
        )
        .bind(target_shift.id)
        .execute(&mut *conn)
        .await?;
    }
    sqlx::query(
        "UPDATE workforce_shift_assignments SET status = $2, override_reason = COALESCE($3, override_reason), version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(assignment_id)
    .bind(&input.status)
    .bind(&input.reason)
    .execute(&mut *conn)
    .await?;
    sqlx::query(
        "UPDATE workforce_shifts SET version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(shift_id)
    .execute(&mut *conn)
    .await?;
    let cov = coverage(conn, ctx.tenant_id, shift_id).await?;
    Ok(
        serde_json::json!({"shift_id": shift_id, "covered": cov.report.covered, "gaps": cov.report.gaps}),
    )
}

#[derive(Debug, Deserialize)]
pub struct AbsenceInput {
    pub user_id: Uuid,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub reason_code: String,
}

pub async fn record_absence(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    input: &AbsenceInput,
) -> Result<Uuid, ApiError> {
    if !["planned_leave", "unplanned_absence", "training", "other"]
        .contains(&input.reason_code.as_str())
    {
        return Err(ApiError::bad_request(
            "invalid_reason",
            "unknown absence reason",
        ));
    }
    if input.ends_at <= input.starts_at {
        return Err(ApiError::bad_request(
            "invalid_interval",
            "absence must end after it starts",
        ));
    }
    let ok: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM users WHERE tenant_id = $1 AND id = $2")
            .bind(ctx.tenant_id)
            .bind(input.user_id)
            .fetch_optional(&mut *conn)
            .await?;
    if ok.is_none() {
        return Err(ApiError::not_found());
    }
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO workforce_shift_absences (id, tenant_id, user_id, starts_at, ends_at, reason_code, created_by) VALUES ($1,$2,$3,$4,$5,$6,$7)",
    )
    .bind(id)
    .bind(ctx.tenant_id)
    .bind(input.user_id)
    .bind(input.starts_at)
    .bind(input.ends_at)
    .bind(&input.reason_code)
    .bind(ctx.user_id)
    .execute(&mut *conn)
    .await?;
    Ok(id)
}
