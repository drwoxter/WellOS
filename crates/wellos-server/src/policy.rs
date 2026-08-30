//! Centralized policy decision point: RBAC plus contextual ABAC.
//!
//! Every clinically relevant action passes through [`authorize`]. Attributes
//! evaluated: role, tenant, care relationship, purpose of use, break-glass
//! state, and action. Decisions (including denials) are audited by callers via
//! [`crate::audit`]. This module is deliberately the single place authorization
//! logic lives so it can later be replaced by a policy engine.

use crate::auth::{AuthContext, RoleAssignment};
use crate::error::ApiError;
use sqlx::PgPool;
use uuid::Uuid;

/// Closed purpose-of-use vocabulary. The caller asserts a purpose, but the
/// action-to-purpose matrix decides whether that purpose can authorize the
/// requested action — asserting a different purpose never widens access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    Treatment,
    Operations,
    Emergency,
    Quality,
}

impl Purpose {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "treatment" => Some(Self::Treatment),
            "operations" => Some(Self::Operations),
            "emergency" => Some(Self::Emergency),
            "quality" => Some(Self::Quality),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Treatment => "treatment",
            Self::Operations => "operations",
            Self::Emergency => "emergency",
            Self::Quality => "quality",
        }
    }
}

pub mod actions {
    pub const PATIENT_REGISTER: &str = "patient.register";
    pub const PATIENT_READ: &str = "patient.read";
    pub const PATIENT_SEARCH: &str = "patient.search";
    pub const ENCOUNTER_START: &str = "encounter.start";
    pub const SERVICE_REQUEST_CREATE: &str = "service_request.create";
    pub const RESULT_INGEST: &str = "result.ingest";
    pub const RESULT_REVIEW: &str = "result.review";
    pub const PATIENT_NOTIFY: &str = "patient.notify";
    pub const LOOP_CLOSE: &str = "loop.close";
    pub const AI_REVIEW: &str = "ai.review";
    pub const AUDIT_READ: &str = "audit.read";
    pub const CONSENT_WRITE: &str = "consent.write";
    pub const WORKLIST_READ: &str = "worklist.read";
    pub const JOBS_RUN: &str = "jobs.run";
    pub const BREAK_GLASS_REVIEW: &str = "break_glass.review";
    pub const SERVICE_CREDENTIAL_MANAGE: &str = "service_credential.manage";
    pub const SERVICE_CREDENTIAL_READ: &str = "service_credential.read";
    pub const TENANT_META_READ: &str = "tenant.meta_read";

    pub const ALL: &[&str] = &[
        PATIENT_REGISTER,
        PATIENT_READ,
        PATIENT_SEARCH,
        ENCOUNTER_START,
        SERVICE_REQUEST_CREATE,
        RESULT_INGEST,
        RESULT_REVIEW,
        PATIENT_NOTIFY,
        LOOP_CLOSE,
        AI_REVIEW,
        AUDIT_READ,
        CONSENT_WRITE,
        WORKLIST_READ,
        JOBS_RUN,
        BREAK_GLASS_REVIEW,
        SERVICE_CREDENTIAL_MANAGE,
        SERVICE_CREDENTIAL_READ,
        TENANT_META_READ,
    ];

    /// Whether `s` names a known action (used to validate service scopes).
    pub fn is_known_action(s: &str) -> bool {
        ALL.contains(&s)
    }
}

/// Action-to-purpose matrix: which asserted purposes may authorize each
/// action. Clinical writes require treatment context; emergency purpose is
/// read-only; operations/quality purposes cover administrative and review
/// surfaces.
pub fn purpose_allows(purpose: Purpose, action: &str) -> bool {
    use actions::*;
    let allowed: &[Purpose] = match action {
        PATIENT_REGISTER => &[Purpose::Treatment, Purpose::Operations],
        PATIENT_READ => &[Purpose::Treatment, Purpose::Emergency],
        PATIENT_SEARCH => &[Purpose::Treatment, Purpose::Operations, Purpose::Emergency],
        ENCOUNTER_START
        | SERVICE_REQUEST_CREATE
        | RESULT_REVIEW
        | PATIENT_NOTIFY
        | LOOP_CLOSE
        | AI_REVIEW => &[Purpose::Treatment],
        RESULT_INGEST => &[Purpose::Treatment, Purpose::Operations],
        AUDIT_READ => &[Purpose::Operations, Purpose::Quality],
        CONSENT_WRITE => &[Purpose::Treatment, Purpose::Operations],
        WORKLIST_READ => &[Purpose::Treatment, Purpose::Operations, Purpose::Quality],
        JOBS_RUN => &[Purpose::Operations],
        BREAK_GLASS_REVIEW => &[Purpose::Operations, Purpose::Quality],
        SERVICE_CREDENTIAL_MANAGE | SERVICE_CREDENTIAL_READ => &[Purpose::Operations],
        TENANT_META_READ => &[
            Purpose::Treatment,
            Purpose::Operations,
            Purpose::Quality,
            Purpose::Emergency,
        ],
        _ => &[],
    };
    allowed.contains(&purpose)
}

pub mod roles {
    pub const REGISTRATION: &str = "registration_staff";
    pub const PHYSICIAN: &str = "physician";
    pub const NURSE: &str = "nurse";
    pub const LAB: &str = "laboratory_professional";
    pub const PHARMACIST: &str = "pharmacist";
    pub const CLINICAL_ADMIN: &str = "clinical_administrator";
    pub const PRIVACY_OFFICER: &str = "privacy_officer";
    pub const SECURITY_AUDITOR: &str = "security_auditor";
    pub const RESEARCH: &str = "research_user";
    pub const PATIENT_REP: &str = "patient_representative";
    pub const DMIND_SERVICE: &str = "dmind_service_agent";
    pub const LAB_INTERFACE: &str = "lab_interface_agent";
    /// Grants no actions by itself: marks users allowed to invoke
    /// break-glass emergency read access.
    pub const BREAK_GLASS_AUTHORIZED: &str = "break_glass_authorized";
    pub const ALL: &[&str] = &[
        REGISTRATION,
        PHYSICIAN,
        NURSE,
        LAB,
        PHARMACIST,
        CLINICAL_ADMIN,
        PRIVACY_OFFICER,
        SECURITY_AUDITOR,
        RESEARCH,
        PATIENT_REP,
        DMIND_SERVICE,
        LAB_INTERFACE,
        BREAK_GLASS_AUTHORIZED,
    ];
}

/// Static RBAC matrix: which roles may attempt which actions. Contextual
/// (ABAC) checks are applied on top in [`authorize`].
pub fn role_allows(role: &str, action: &str) -> bool {
    use actions::*;
    use roles::*;
    let allowed: &[&str] = match role {
        REGISTRATION => &[
            PATIENT_REGISTER,
            PATIENT_READ,
            PATIENT_SEARCH,
            TENANT_META_READ,
        ],
        PHYSICIAN => &[
            PATIENT_SEARCH,
            PATIENT_READ,
            ENCOUNTER_START,
            SERVICE_REQUEST_CREATE,
            RESULT_REVIEW,
            PATIENT_NOTIFY,
            LOOP_CLOSE,
            AI_REVIEW,
            WORKLIST_READ,
            TENANT_META_READ,
        ],
        // Nurses have no PATIENT_NOTIFY grant: notification requires an
        // established care relationship, and encounters name a single
        // practitioner. A care-team assignment model (roadmap) is required
        // before nurse-performed notification can be authorized.
        NURSE => &[
            PATIENT_SEARCH,
            PATIENT_READ,
            WORKLIST_READ,
            TENANT_META_READ,
        ],
        LAB => &[RESULT_INGEST, WORKLIST_READ, TENANT_META_READ],
        PHARMACIST => &[
            PATIENT_SEARCH,
            PATIENT_READ,
            WORKLIST_READ,
            TENANT_META_READ,
        ],
        CLINICAL_ADMIN => &[
            PATIENT_SEARCH,
            PATIENT_READ,
            WORKLIST_READ,
            JOBS_RUN,
            TENANT_META_READ,
        ],
        PRIVACY_OFFICER => &[
            AUDIT_READ,
            CONSENT_WRITE,
            BREAK_GLASS_REVIEW,
            SERVICE_CREDENTIAL_MANAGE,
            SERVICE_CREDENTIAL_READ,
            TENANT_META_READ,
        ],
        SECURITY_AUDITOR => &[
            AUDIT_READ,
            BREAK_GLASS_REVIEW,
            SERVICE_CREDENTIAL_READ,
            TENANT_META_READ,
        ],
        // Research users have no direct-care access by design.
        RESEARCH => &[],
        PATIENT_REP => &[],
        // dMind generates suggestions only; it never writes clinical results.
        DMIND_SERVICE => &[],
        LAB_INTERFACE => &[RESULT_INGEST],
        BREAK_GLASS_AUTHORIZED => &[],
        _ => &[],
    };
    allowed.contains(&action)
}

/// Roles whose `facility_id IS NULL` assignment grants tenant-wide access.
/// This allowlist is explicit: administrative, oversight, and machine roles
/// operate tenant-wide; the dedicated break-glass role may be granted
/// tenant-wide for emergency coverage. Ordinary clinical roles require
/// explicit facility assignments — a NULL facility grants them nothing
/// beyond facility-unscoped resources.
pub fn null_facility_is_tenant_wide(role: &str) -> bool {
    matches!(
        role,
        roles::CLINICAL_ADMIN
            | roles::PRIVACY_OFFICER
            | roles::SECURITY_AUDITOR
            | roles::DMIND_SERVICE
            | roles::LAB_INTERFACE
            | roles::BREAK_GLASS_AUTHORIZED
    )
}

/// Whether a role assignment covers a specific facility.
fn assignment_covers_facility(a: &RoleAssignment, facility: Uuid) -> bool {
    match a.facility_id {
        Some(f) => f == facility,
        None => null_facility_is_tenant_wide(&a.role),
    }
}

/// The set of facilities in which the caller may perform `action`, used to
/// scope list/search queries. `None` means tenant-wide (an explicitly
/// allowlisted tenant-wide assignment grants the action); otherwise the
/// explicit facility list (possibly empty).
pub fn facility_scope(ctx: &AuthContext, action: &str) -> Option<Vec<Uuid>> {
    let mut ids: Vec<Uuid> = Vec::new();
    for a in ctx.assignments.iter() {
        if !role_allows(&a.role, action) {
            continue;
        }
        match a.facility_id {
            None if null_facility_is_tenant_wide(&a.role) => return None,
            Some(f) => ids.push(f),
            None => {}
        }
    }
    ids.sort();
    ids.dedup();
    Some(ids)
}

pub struct ResourceCtx {
    pub tenant_id: Uuid,
    pub patient_id: Option<Uuid>,
    /// Facility derived from trusted database relationships (never from
    /// client input). `None` for facility-unscoped resources (tenant
    /// metadata, audit log, worklists filtered separately).
    pub facility_id: Option<Uuid>,
}

#[derive(Debug)]
pub struct Decision {
    pub allowed: bool,
    pub reason: String,
    pub used_break_glass: bool,
}

/// Central policy decision. Order: authentication (already done), tenant
/// isolation, RBAC, service scopes, purpose of use, then contextual
/// care-relationship checks with break-glass as an audited exception path.
pub async fn authorize(
    pool: &PgPool,
    ctx: &AuthContext,
    action: &str,
    resource: Option<&ResourceCtx>,
) -> Result<Decision, ApiError> {
    authorize_with_limit(pool, ctx, action, resource, 5).await
}

pub async fn authorize_with_limit(
    pool: &PgPool,
    ctx: &AuthContext,
    action: &str,
    resource: Option<&ResourceCtx>,
    break_glass_hourly_limit: i64,
) -> Result<Decision, ApiError> {
    // Tenant isolation is absolute: break-glass never crosses tenants.
    if let Some(r) = resource {
        if r.tenant_id != ctx.tenant_id {
            return Ok(Decision {
                allowed: false,
                reason: "cross_tenant_access".into(),
                used_break_glass: false,
            });
        }
    }

    let granting: Vec<&RoleAssignment> = ctx
        .assignments
        .iter()
        .filter(|a| role_allows(&a.role, action))
        .collect();
    if granting.is_empty() {
        return Ok(Decision {
            allowed: false,
            reason: format!("role_lacks_permission:{action}"),
            used_break_glass: false,
        });
    }

    // Service credentials are additionally bounded by explicit scopes: the
    // scope name is the action name.
    if ctx.is_service && !ctx.has_scope(action) {
        return Ok(Decision {
            allowed: false,
            reason: format!("scope_not_granted:{action}"),
            used_break_glass: false,
        });
    }

    // The asserted purpose must be valid for this action; changing the
    // header never widens access beyond this matrix.
    if !purpose_allows(ctx.purpose_of_use, action) {
        return Ok(Decision {
            allowed: false,
            reason: format!(
                "purpose_not_permitted:{}:{action}",
                ctx.purpose_of_use.as_str()
            ),
            used_break_glass: false,
        });
    }

    // Emergency purpose never grants broad tenant-wide search to ordinary
    // users: emergency lookup requires the dedicated break-glass role, and
    // subsequent chart access still passes through the full break-glass path
    // (patient-specific, same-tenant, read-only, rate-limited, reviewed).
    if ctx.purpose_of_use == Purpose::Emergency
        && matches!(action, actions::PATIENT_SEARCH | actions::PATIENT_READ)
        && !ctx.has_role(roles::BREAK_GLASS_AUTHORIZED)
    {
        return Ok(Decision {
            allowed: false,
            reason: "emergency_requires_break_glass_role".into(),
            used_break_glass: false,
        });
    }

    // Facility scope is enforced centrally: the resource's facility (derived
    // from trusted database relationships) must be covered by at least one
    // granting assignment. NULL-facility assignments cover the tenant only
    // for explicitly allowlisted roles. A gap can be bridged only by the
    // audited break-glass read path, and only when the dedicated break-glass
    // assignment itself covers that facility; every other facility-gap denial
    // uses one non-enumerating reason.
    let resource_facility = resource.and_then(|r| r.facility_id);
    if let Some(facility) = resource_facility {
        if !granting
            .iter()
            .any(|a| assignment_covers_facility(a, facility))
        {
            let deny = Decision {
                allowed: false,
                reason: "facility_scope_denied".into(),
                used_break_glass: false,
            };
            if action != actions::PATIENT_READ || ctx.break_glass_reason.is_none() {
                return Ok(deny);
            }
            let Some(ResourceCtx {
                patient_id: Some(patient_id),
                ..
            }) = resource
            else {
                return Ok(deny);
            };
            let break_glass_covers = ctx.assignments.iter().any(|a| {
                a.role == roles::BREAK_GLASS_AUTHORIZED && assignment_covers_facility(a, facility)
            });
            if !break_glass_covers {
                return Ok(deny);
            }
            let decision =
                break_glass_read(pool, ctx, *patient_id, break_glass_hourly_limit).await?;
            if decision.allowed {
                return Ok(decision);
            }
            return Ok(deny);
        }
    }

    // Contextual check: clinical chart access requires a care relationship
    // (an encounter between practitioner and patient) unless the caller's
    // role is non-clinical-contextual or break-glass is invoked.
    let needs_relationship = match action {
        // Consequential clinical transitions always require an established
        // care relationship, regardless of the caller's role: facility
        // assignment alone never authorizes acting on a patient's results.
        actions::RESULT_REVIEW
        | actions::PATIENT_NOTIFY
        | actions::LOOP_CLOSE
        | actions::AI_REVIEW => true,
        // Chart reads require a relationship for physicians; other clinical
        // roles read within their facility scope (enforced above), and
        // tenant-wide administrative reads remain explicit and audited.
        actions::PATIENT_READ => {
            ctx.has_role(roles::PHYSICIAN) && !ctx.has_role(roles::CLINICAL_ADMIN)
        }
        _ => false,
    };

    if needs_relationship {
        if let Some(ResourceCtx {
            patient_id: Some(patient_id),
            ..
        }) = resource
        {
            let related: Option<(Uuid,)> = sqlx::query_as(
                "SELECT id FROM encounters
                 WHERE tenant_id = $1 AND patient_id = $2 AND practitioner_id = $3
                 LIMIT 1",
            )
            .bind(ctx.tenant_id)
            .bind(patient_id)
            .bind(ctx.user_id)
            .fetch_optional(pool)
            .await?;
            if related.is_none() {
                // Break-glass grants emergency *read* access only; consequential
                // transitions (review, notify, close, AI review) still require
                // an established care relationship.
                if action != actions::PATIENT_READ {
                    return Ok(Decision {
                        allowed: false,
                        reason: "no_care_relationship".into(),
                        used_break_glass: false,
                    });
                }
                let Some(_) = &ctx.break_glass_reason else {
                    return Ok(Decision {
                        allowed: false,
                        reason: "no_care_relationship".into(),
                        used_break_glass: false,
                    });
                };
                // Break-glass is least-privilege: a dedicated server-side
                // role, an emergency purpose, a bounded non-empty reason,
                // a patient-specific resource (guaranteed here), same-tenant
                // access (enforced above), and a per-user rate limit.
                if !ctx.has_role(roles::BREAK_GLASS_AUTHORIZED) {
                    return Ok(Decision {
                        allowed: false,
                        reason: "break_glass_not_authorized".into(),
                        used_break_glass: false,
                    });
                }
                return break_glass_read(pool, ctx, *patient_id, break_glass_hourly_limit).await;
            }
        }
    }

    Ok(Decision {
        allowed: true,
        reason: "rbac_allow".into(),
        used_break_glass: false,
    })
}

/// The audited break-glass read path: emergency purpose, bounded reason,
/// per-user rate limit under an advisory lock, and an immutable event row
/// pending mandatory review. Callers verify the dedicated role/assignment
/// before invoking this.
async fn break_glass_read(
    pool: &PgPool,
    ctx: &AuthContext,
    patient_id: Uuid,
    break_glass_hourly_limit: i64,
) -> Result<Decision, ApiError> {
    if ctx.purpose_of_use != Purpose::Emergency {
        return Ok(Decision {
            allowed: false,
            reason: "break_glass_requires_emergency_purpose".into(),
            used_break_glass: false,
        });
    }
    let reason = ctx.break_glass_reason.as_deref().unwrap_or("").trim();
    if reason.len() < 8 || reason.len() > 500 {
        return Ok(Decision {
            allowed: false,
            reason: "break_glass_reason_invalid".into(),
            used_break_glass: false,
        });
    }
    // Count and insert under a per-user transaction-scoped advisory lock so
    // concurrent requests cannot all pass the limit check before any
    // activation is recorded.
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text, 0))")
        .bind(ctx.user_id)
        .execute(&mut *tx)
        .await?;
    let (recent,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM break_glass_events
         WHERE user_id = $1 AND created_at > now() - interval '1 hour'",
    )
    .bind(ctx.user_id)
    .fetch_one(&mut *tx)
    .await?;
    if recent >= break_glass_hourly_limit {
        return Ok(Decision {
            allowed: false,
            reason: "break_glass_rate_limited".into(),
            used_break_glass: false,
        });
    }
    // Immutable break-glass record, pending mandatory review.
    sqlx::query(
        "INSERT INTO break_glass_events
         (id, tenant_id, user_id, patient_id, reason, correlation_id, purpose_of_use)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(Uuid::now_v7())
    .bind(ctx.tenant_id)
    .bind(ctx.user_id)
    .bind(patient_id)
    .bind(reason)
    .bind(ctx.correlation_id)
    .bind(ctx.purpose_of_use.as_str())
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Decision {
        allowed: true,
        reason: "break_glass".into(),
        used_break_glass: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn research_user_has_no_direct_care_access() {
        for action in [
            actions::PATIENT_READ,
            actions::PATIENT_SEARCH,
            actions::RESULT_REVIEW,
            actions::LOOP_CLOSE,
        ] {
            assert!(!role_allows(roles::RESEARCH, action));
        }
    }

    #[test]
    fn nurse_cannot_close_loop() {
        assert!(!role_allows(roles::NURSE, actions::LOOP_CLOSE));
        assert!(!role_allows(roles::NURSE, actions::PATIENT_NOTIFY));
        assert!(role_allows(roles::PHYSICIAN, actions::PATIENT_NOTIFY));
    }

    #[test]
    fn dmind_agent_cannot_ingest_results() {
        assert!(!role_allows(roles::DMIND_SERVICE, actions::RESULT_INGEST));
        assert!(role_allows(roles::LAB_INTERFACE, actions::RESULT_INGEST));
    }

    #[test]
    fn only_privacy_officer_manages_service_credentials() {
        for role in roles::ALL {
            let expected = *role == roles::PRIVACY_OFFICER;
            assert_eq!(
                role_allows(role, actions::SERVICE_CREDENTIAL_MANAGE),
                expected,
                "{role}"
            );
        }
        assert!(role_allows(
            roles::SECURITY_AUDITOR,
            actions::SERVICE_CREDENTIAL_READ
        ));
    }

    #[test]
    fn service_credential_actions_require_operations_purpose() {
        assert!(purpose_allows(
            Purpose::Operations,
            actions::SERVICE_CREDENTIAL_MANAGE
        ));
        for p in [Purpose::Treatment, Purpose::Emergency, Purpose::Quality] {
            assert!(!purpose_allows(p, actions::SERVICE_CREDENTIAL_MANAGE));
        }
    }

    #[test]
    fn every_action_is_known() {
        assert!(actions::is_known_action(actions::RESULT_INGEST));
        assert!(!actions::is_known_action("no.such.action"));
    }

    #[test]
    fn only_authorized_roles_read_audit() {
        for role in roles::ALL {
            let expected = *role == roles::PRIVACY_OFFICER || *role == roles::SECURITY_AUDITOR;
            assert_eq!(role_allows(role, actions::AUDIT_READ), expected, "{role}");
        }
    }
}
