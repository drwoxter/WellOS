//! Centralized policy decision point: RBAC plus contextual ABAC.
//!
//! Every clinically relevant action passes through [`authorize`]. Attributes
//! evaluated: role, tenant, care relationship, purpose of use, break-glass
//! state, and action. Decisions (including denials) are audited by callers via
//! [`crate::audit`]. This module is deliberately the single place authorization
//! logic lives so it can later be replaced by a policy engine.

use crate::auth::AuthContext;
use crate::error::ApiError;
use sqlx::PgPool;
use uuid::Uuid;

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
    ];
}

/// Static RBAC matrix: which roles may attempt which actions. Contextual
/// (ABAC) checks are applied on top in [`authorize`].
pub fn role_allows(role: &str, action: &str) -> bool {
    use actions::*;
    use roles::*;
    let allowed: &[&str] = match role {
        REGISTRATION => &[PATIENT_REGISTER, PATIENT_READ, PATIENT_SEARCH],
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
        ],
        NURSE => &[PATIENT_SEARCH, PATIENT_READ, PATIENT_NOTIFY, WORKLIST_READ],
        LAB => &[RESULT_INGEST, WORKLIST_READ],
        PHARMACIST => &[PATIENT_SEARCH, PATIENT_READ, WORKLIST_READ],
        CLINICAL_ADMIN => &[PATIENT_SEARCH, PATIENT_READ, WORKLIST_READ, JOBS_RUN],
        PRIVACY_OFFICER => &[AUDIT_READ, CONSENT_WRITE],
        SECURITY_AUDITOR => &[AUDIT_READ],
        // Research users have no direct-care access by design.
        RESEARCH => &[],
        PATIENT_REP => &[],
        DMIND_SERVICE => &[RESULT_INGEST],
        _ => &[],
    };
    allowed.contains(&action)
}

pub struct ResourceCtx {
    pub tenant_id: Uuid,
    pub patient_id: Option<Uuid>,
}

#[derive(Debug)]
pub struct Decision {
    pub allowed: bool,
    pub reason: String,
    pub used_break_glass: bool,
}

/// Central policy decision. Order: authentication (already done), tenant
/// isolation, RBAC, then contextual care-relationship checks with break-glass
/// as an audited exception path.
pub async fn authorize(
    pool: &PgPool,
    ctx: &AuthContext,
    action: &str,
    resource: Option<&ResourceCtx>,
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

    if !ctx.roles.iter().any(|role| role_allows(role, action)) {
        return Ok(Decision {
            allowed: false,
            reason: format!("role_lacks_permission:{action}"),
            used_break_glass: false,
        });
    }

    // Contextual check: clinical chart access requires a care relationship
    // (an encounter between practitioner and patient) unless the caller's
    // role is non-clinical-contextual or break-glass is invoked.
    let needs_relationship = matches!(
        action,
        actions::PATIENT_READ
            | actions::RESULT_REVIEW
            | actions::PATIENT_NOTIFY
            | actions::LOOP_CLOSE
            | actions::AI_REVIEW
    ) && ctx.has_role(roles::PHYSICIAN)
        && !ctx.has_role(roles::CLINICAL_ADMIN);

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
                if let Some(reason) = &ctx.break_glass_reason {
                    // Break-glass: allowed, but recorded for mandatory review.
                    sqlx::query(
                        "INSERT INTO break_glass_events (id, tenant_id, user_id, patient_id, reason)
                         VALUES ($1, $2, $3, $4, $5)",
                    )
                    .bind(Uuid::now_v7())
                    .bind(ctx.tenant_id)
                    .bind(ctx.user_id)
                    .bind(patient_id)
                    .bind(reason)
                    .execute(pool)
                    .await?;
                    return Ok(Decision {
                        allowed: true,
                        reason: "break_glass".into(),
                        used_break_glass: true,
                    });
                }
                return Ok(Decision {
                    allowed: false,
                    reason: "no_care_relationship".into(),
                    used_break_glass: false,
                });
            }
        }
    }

    Ok(Decision {
        allowed: true,
        reason: "rbac_allow".into(),
        used_break_glass: false,
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
        assert!(role_allows(roles::NURSE, actions::PATIENT_NOTIFY));
    }

    #[test]
    fn only_authorized_roles_read_audit() {
        for role in roles::ALL {
            let expected = *role == roles::PRIVACY_OFFICER || *role == roles::SECURITY_AUDITOR;
            assert_eq!(role_allows(role, actions::AUDIT_READ), expected, "{role}");
        }
    }
}
