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
    pub const ENCOUNTER_DOCUMENT: &str = "encounter.document";
    pub const ENCOUNTER_SIGN: &str = "encounter.sign";
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
    /// Create scheduled visits and arrivals; mark arrived, cancel, no-show.
    pub const VISIT_MANAGE: &str = "visit.manage";
    /// Read access/arrival worklists and visit detail.
    pub const VISIT_READ: &str = "visit.read";
    /// Triage documentation, dMind triage proposal and its human review.
    pub const TRIAGE_WRITE: &str = "triage.write";
    /// Assign a professional or an explicit service queue to a visit.
    pub const CARE_TEAM_ASSIGN: &str = "care_team.assign";
    /// Acknowledge directed internal alerts.
    pub const ALERT_ACKNOWLEDGE: &str = "alert.acknowledge";
    /// Read a patient's deterministic risk assessment, its history and the
    /// facility risk worklist.
    pub const RISK_READ: &str = "risk.read";
    /// Recalculate risk, acknowledge a risk item, assign a follow-up owner.
    pub const RISK_MANAGE: &str = "risk.manage";
    /// Clinical review of a risk item, dMind risk-summary proposal and its
    /// human review, confirming a suggestion into a follow-up task.
    pub const RISK_REVIEW: &str = "risk.review";
    /// Read the minimal consent-gated risk projection prepared for future
    /// insurer collaboration. Never a coverage, pricing or denial decision.
    pub const RISK_PROJECTION_READ: &str = "risk.projection_read";
    /// Read open catalogs (services, specialties, professions, modalities,
    /// resource types, accessibility, locations, transport resources).
    pub const CATALOG_READ: &str = "catalog.read";
    /// Create, version, deactivate catalog entries and their facility
    /// availability. Never grants any clinical permission.
    pub const CATALOG_MANAGE: &str = "catalog.manage";
    /// Schedulable resources, availability rules, exceptions, service
    /// requirements, tenant scheduling policy, facility hours/location and
    /// the tenant operational calendar.
    pub const RESOURCE_MANAGE: &str = "resource.manage";
    /// Staff read of scheduling surfaces: access requests, matcher runs,
    /// offers, holds, appointments, resource lanes, cancellation events.
    pub const SCHEDULING_READ: &str = "scheduling.read";
    /// Staff scheduling administration: submit requests on behalf of a
    /// patient, run the matcher, hold, confirm, reschedule, cancel, mark
    /// fulfilled/no-show, override with a reason.
    pub const SCHEDULING_MANAGE: &str = "scheduling.manage";
    /// Patient/representative self-service through `/api/v1/me/...`; the
    /// patient is derived from an active access grant, never from input.
    pub const PATIENT_SELF_SERVICE: &str = "patient.self_service";
    /// Verify, list and revoke patient access grants (staff).
    pub const PATIENT_GRANT_MANAGE: &str = "patient_grant.manage";
    /// Inspect waitlists and cancellation recovery, override the offer
    /// order with a reason, pause or remove entries on a patient's behalf.
    pub const WAITLIST_MANAGE: &str = "waitlist.manage";
    /// Transport requests, vehicle assignment, status, live location
    /// (logistics only: never the clinical chart).
    pub const TRANSPORT_COORDINATE: &str = "transport.coordinate";
    /// Compute and read capacity forecasts and their explanations.
    pub const CAPACITY_REVIEW: &str = "capacity.review";
    /// Read and mark one's own in-app notifications.
    pub const NOTIFICATION_READ: &str = "notification.read";
    /// Compose, confirm, place and cancel diagnostic orders from a
    /// consultation (the accountable ordering clinician).
    pub const DIAGNOSTIC_ORDER_MANAGE: &str = "diagnostic_order.manage";
    /// Authorized reasoned override of a deterministic safety hard stop.
    pub const DIAGNOSTIC_SAFETY_OVERRIDE: &str = "diagnostic_safety.override";
    /// Read diagnostic orders, worklists, specimens and reports.
    pub const DIAGNOSTIC_READ: &str = "diagnostic.read";
    /// Fulfilment transitions: accept, schedule, start, complete, hold,
    /// reject, record acquisition status.
    pub const DIAGNOSTIC_FULFIL: &str = "diagnostic.fulfil";
    /// Specimen collection, custody events, rejection and recollection.
    pub const SPECIMEN_HANDLE: &str = "specimen.handle";
    /// Author, sign, amend and correct diagnostic reports and their
    /// structured results; register documents and imaging references.
    pub const DIAGNOSTIC_REPORT_WRITE: &str = "diagnostic_report.write";
    /// Professional review of a final/amended/corrected report.
    pub const DIAGNOSTIC_REVIEW: &str = "diagnostic_report.review";
    /// Clinician decision to release (or withhold) a reviewed report and its
    /// approved explanation to the patient.
    pub const DIAGNOSTIC_RELEASE: &str = "diagnostic_result.release";

    pub const ALL: &[&str] = &[
        PATIENT_REGISTER,
        PATIENT_READ,
        PATIENT_SEARCH,
        ENCOUNTER_START,
        ENCOUNTER_DOCUMENT,
        ENCOUNTER_SIGN,
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
        VISIT_MANAGE,
        VISIT_READ,
        TRIAGE_WRITE,
        CARE_TEAM_ASSIGN,
        ALERT_ACKNOWLEDGE,
        RISK_READ,
        RISK_MANAGE,
        RISK_REVIEW,
        RISK_PROJECTION_READ,
        CATALOG_READ,
        CATALOG_MANAGE,
        RESOURCE_MANAGE,
        SCHEDULING_READ,
        SCHEDULING_MANAGE,
        PATIENT_SELF_SERVICE,
        PATIENT_GRANT_MANAGE,
        WAITLIST_MANAGE,
        TRANSPORT_COORDINATE,
        CAPACITY_REVIEW,
        NOTIFICATION_READ,
        DIAGNOSTIC_ORDER_MANAGE,
        DIAGNOSTIC_SAFETY_OVERRIDE,
        DIAGNOSTIC_READ,
        DIAGNOSTIC_FULFIL,
        SPECIMEN_HANDLE,
        DIAGNOSTIC_REPORT_WRITE,
        DIAGNOSTIC_REVIEW,
        DIAGNOSTIC_RELEASE,
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
        | ENCOUNTER_DOCUMENT
        | ENCOUNTER_SIGN
        | SERVICE_REQUEST_CREATE
        | RESULT_REVIEW
        | PATIENT_NOTIFY
        | LOOP_CLOSE
        | AI_REVIEW
        | TRIAGE_WRITE
        | RISK_REVIEW => &[Purpose::Treatment],
        VISIT_MANAGE | VISIT_READ | CARE_TEAM_ASSIGN | ALERT_ACKNOWLEDGE | RISK_MANAGE => {
            &[Purpose::Treatment, Purpose::Operations]
        }
        RISK_READ => &[Purpose::Treatment, Purpose::Operations, Purpose::Quality],
        // The insurer projection is an operational data-sharing surface:
        // never a treatment context, never emergency access.
        RISK_PROJECTION_READ => &[Purpose::Operations],
        // Scheduling is care access: treatment or operations context. The
        // patient's own self-service never happens under an emergency or
        // quality purpose.
        SCHEDULING_READ | SCHEDULING_MANAGE | PATIENT_SELF_SERVICE | PATIENT_GRANT_MANAGE
        | WAITLIST_MANAGE | TRANSPORT_COORDINATE | NOTIFICATION_READ => {
            &[Purpose::Treatment, Purpose::Operations]
        }
        CATALOG_MANAGE | RESOURCE_MANAGE => &[Purpose::Operations],
        CAPACITY_REVIEW => &[Purpose::Operations, Purpose::Quality],
        CATALOG_READ => &[
            Purpose::Treatment,
            Purpose::Operations,
            Purpose::Quality,
            Purpose::Emergency,
        ],
        RESULT_INGEST => &[Purpose::Treatment, Purpose::Operations],
        // Ordering, overriding a safety stop, reviewing and releasing results
        // are clinical decisions: treatment context only.
        DIAGNOSTIC_ORDER_MANAGE
        | DIAGNOSTIC_SAFETY_OVERRIDE
        | DIAGNOSTIC_REVIEW
        | DIAGNOSTIC_RELEASE => &[Purpose::Treatment],
        DIAGNOSTIC_FULFIL | SPECIMEN_HANDLE | DIAGNOSTIC_REPORT_WRITE => {
            &[Purpose::Treatment, Purpose::Operations]
        }
        DIAGNOSTIC_READ => &[Purpose::Treatment, Purpose::Operations, Purpose::Quality],
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
    /// Machine role for future insurer integrations: consent-gated risk
    /// projection only, no chart, notes or clinical writes.
    pub const INSURER_INTEGRATION: &str = "insurer_integration_agent";
    /// Grants no actions by itself: marks users allowed to invoke
    /// break-glass emergency read access.
    pub const BREAK_GLASS_AUTHORIZED: &str = "break_glass_authorized";
    /// Transport personnel and dispatch: logistics only, no chart access.
    pub const TRANSPORT_COORDINATOR: &str = "transport_coordinator";
    /// Diagnostic performing professionals (radiology, cardiology,
    /// pathology, dentistry, endoscopy technologists and reporting
    /// specialists): fulfil orders, handle specimens, author and sign
    /// reports in their facility. Never order, review for the patient or
    /// release results.
    pub const DIAGNOSTIC_PROFESSIONAL: &str = "diagnostic_professional";
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
        INSURER_INTEGRATION,
        BREAK_GLASS_AUTHORIZED,
        TRANSPORT_COORDINATOR,
        DIAGNOSTIC_PROFESSIONAL,
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
            VISIT_MANAGE,
            VISIT_READ,
            ALERT_ACKNOWLEDGE,
            TENANT_META_READ,
            CATALOG_READ,
            SCHEDULING_READ,
            SCHEDULING_MANAGE,
            PATIENT_GRANT_MANAGE,
            WAITLIST_MANAGE,
            TRANSPORT_COORDINATE,
            CAPACITY_REVIEW,
            NOTIFICATION_READ,
        ],
        PHYSICIAN => &[
            PATIENT_SEARCH,
            PATIENT_READ,
            ENCOUNTER_START,
            ENCOUNTER_DOCUMENT,
            ENCOUNTER_SIGN,
            SERVICE_REQUEST_CREATE,
            RESULT_REVIEW,
            PATIENT_NOTIFY,
            LOOP_CLOSE,
            AI_REVIEW,
            WORKLIST_READ,
            VISIT_READ,
            TRIAGE_WRITE,
            CARE_TEAM_ASSIGN,
            ALERT_ACKNOWLEDGE,
            RISK_READ,
            RISK_MANAGE,
            RISK_REVIEW,
            TENANT_META_READ,
            CATALOG_READ,
            SCHEDULING_READ,
            NOTIFICATION_READ,
            DIAGNOSTIC_ORDER_MANAGE,
            DIAGNOSTIC_SAFETY_OVERRIDE,
            DIAGNOSTIC_READ,
            DIAGNOSTIC_FULFIL,
            DIAGNOSTIC_REPORT_WRITE,
            DIAGNOSTIC_REVIEW,
            DIAGNOSTIC_RELEASE,
        ],
        // Nurses have no PATIENT_NOTIFY grant: result notification requires
        // the encounter-based care relationship, and encounters name a single
        // practitioner. Triage and care-team routing are nursing functions
        // scoped by facility; they never imply consultation rights.
        NURSE => &[
            PATIENT_SEARCH,
            PATIENT_READ,
            WORKLIST_READ,
            VISIT_MANAGE,
            VISIT_READ,
            TRIAGE_WRITE,
            CARE_TEAM_ASSIGN,
            ALERT_ACKNOWLEDGE,
            RISK_READ,
            RISK_MANAGE,
            RISK_REVIEW,
            TENANT_META_READ,
            CATALOG_READ,
            SCHEDULING_READ,
            SCHEDULING_MANAGE,
            WAITLIST_MANAGE,
            NOTIFICATION_READ,
            DIAGNOSTIC_READ,
            DIAGNOSTIC_FULFIL,
            SPECIMEN_HANDLE,
        ],
        LAB => &[
            RESULT_INGEST,
            WORKLIST_READ,
            TENANT_META_READ,
            CATALOG_READ,
            DIAGNOSTIC_READ,
            DIAGNOSTIC_FULFIL,
            SPECIMEN_HANDLE,
            DIAGNOSTIC_REPORT_WRITE,
        ],
        DIAGNOSTIC_PROFESSIONAL => &[
            WORKLIST_READ,
            TENANT_META_READ,
            CATALOG_READ,
            SCHEDULING_READ,
            NOTIFICATION_READ,
            DIAGNOSTIC_READ,
            DIAGNOSTIC_FULFIL,
            SPECIMEN_HANDLE,
            DIAGNOSTIC_REPORT_WRITE,
        ],
        // Pharmacists read risk (medication/allergy safety domain) but the
        // review actions stay with the responsible clinical professional.
        PHARMACIST => &[
            PATIENT_SEARCH,
            PATIENT_READ,
            WORKLIST_READ,
            RISK_READ,
            TENANT_META_READ,
            CATALOG_READ,
            NOTIFICATION_READ,
        ],
        CLINICAL_ADMIN => &[
            PATIENT_SEARCH,
            PATIENT_READ,
            WORKLIST_READ,
            VISIT_MANAGE,
            VISIT_READ,
            CARE_TEAM_ASSIGN,
            RISK_READ,
            RISK_MANAGE,
            RISK_PROJECTION_READ,
            JOBS_RUN,
            TENANT_META_READ,
            CATALOG_READ,
            CATALOG_MANAGE,
            RESOURCE_MANAGE,
            SCHEDULING_READ,
            SCHEDULING_MANAGE,
            PATIENT_GRANT_MANAGE,
            WAITLIST_MANAGE,
            TRANSPORT_COORDINATE,
            CAPACITY_REVIEW,
            NOTIFICATION_READ,
            DIAGNOSTIC_READ,
        ],
        PRIVACY_OFFICER => &[
            AUDIT_READ,
            CONSENT_WRITE,
            BREAK_GLASS_REVIEW,
            SERVICE_CREDENTIAL_MANAGE,
            SERVICE_CREDENTIAL_READ,
            TENANT_META_READ,
            CATALOG_READ,
            PATIENT_GRANT_MANAGE,
        ],
        SECURITY_AUDITOR => &[
            AUDIT_READ,
            BREAK_GLASS_REVIEW,
            SERVICE_CREDENTIAL_READ,
            TENANT_META_READ,
            CATALOG_READ,
        ],
        // Research users have no direct-care access by design.
        RESEARCH => &[],
        // Patients and representatives act only through grant-scoped
        // self-service: no chart, no worklists, no staff scheduling.
        PATIENT_REP => &[
            PATIENT_SELF_SERVICE,
            CATALOG_READ,
            NOTIFICATION_READ,
            TENANT_META_READ,
        ],
        TRANSPORT_COORDINATOR => &[
            TRANSPORT_COORDINATE,
            CATALOG_READ,
            NOTIFICATION_READ,
            TENANT_META_READ,
        ],
        // dMind generates suggestions only; it never writes clinical results.
        DMIND_SERVICE => &[],
        // Interface agents deliver results and reports from source systems;
        // they never review, release or order.
        LAB_INTERFACE => &[RESULT_INGEST, DIAGNOSTIC_REPORT_WRITE],
        INSURER_INTEGRATION => &[RISK_PROJECTION_READ],
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
            | roles::INSURER_INTEGRATION
            | roles::BREAK_GLASS_AUTHORIZED
            | roles::PATIENT_REP
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
    // (an encounter between practitioner and patient, or an active
    // care-team assignment naming the caller) unless the caller's role is
    // non-clinical-contextual or break-glass is invoked.
    let needs_relationship = match action {
        // Consequential clinical transitions always require an established
        // care relationship, regardless of the caller's role: facility
        // assignment alone never authorizes acting on a patient's results.
        actions::RESULT_REVIEW
        | actions::PATIENT_NOTIFY
        | actions::LOOP_CLOSE
        | actions::AI_REVIEW
        | actions::ENCOUNTER_DOCUMENT
        | actions::ENCOUNTER_SIGN
        | actions::RISK_REVIEW => true,
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
            let related = has_care_relationship(pool, ctx, *patient_id, action).await?;
            if !related {
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

/// Whether the caller has an established care relationship with the
/// patient. Consequential result/documentation transitions rely on the
/// encounter relationship (encounters name one practitioner). Chart reads
/// additionally accept an active, patient-specific care-team assignment
/// naming the caller: membership is explicit and time-bounded, and never
/// inferred from a system role.
async fn has_care_relationship(
    pool: &PgPool,
    ctx: &AuthContext,
    patient_id: Uuid,
    action: &str,
) -> Result<bool, ApiError> {
    let encounter: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM encounters
         WHERE tenant_id = $1 AND patient_id = $2 AND practitioner_id = $3
         LIMIT 1",
    )
    .bind(ctx.tenant_id)
    .bind(patient_id)
    .bind(ctx.user_id)
    .fetch_optional(pool)
    .await?;
    if encounter.is_some() {
        return Ok(true);
    }
    // Risk review is a care-coordination judgement: an explicit care-team
    // assignment (including the risk follow-up owner assigned from the
    // worklist) establishes the relationship, as it does for chart reads.
    if action != actions::PATIENT_READ && action != actions::RISK_REVIEW {
        return Ok(false);
    }
    let assignment: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM care_team_assignments
         WHERE tenant_id = $1 AND patient_id = $2 AND assignee_user_id = $3
           AND active AND starts_at <= now() AND (ends_at IS NULL OR ends_at > now())
         LIMIT 1",
    )
    .bind(ctx.tenant_id)
    .bind(patient_id)
    .bind(ctx.user_id)
    .fetch_optional(pool)
    .await?;
    Ok(assignment.is_some())
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
    fn access_and_triage_grants_follow_function_not_hierarchy() {
        assert!(role_allows(roles::REGISTRATION, actions::VISIT_MANAGE));
        assert!(!role_allows(roles::REGISTRATION, actions::TRIAGE_WRITE));
        assert!(!role_allows(roles::REGISTRATION, actions::ENCOUNTER_START));
        assert!(role_allows(roles::NURSE, actions::TRIAGE_WRITE));
        assert!(role_allows(roles::NURSE, actions::CARE_TEAM_ASSIGN));
        assert!(!role_allows(roles::NURSE, actions::ENCOUNTER_START));
        assert!(role_allows(roles::PHYSICIAN, actions::VISIT_READ));
        assert!(!role_allows(roles::PHYSICIAN, actions::VISIT_MANAGE));
        assert!(!role_allows(roles::LAB, actions::VISIT_READ));
        assert!(!role_allows(roles::RESEARCH, actions::VISIT_READ));
        assert!(!role_allows(roles::DMIND_SERVICE, actions::TRIAGE_WRITE));
        assert!(purpose_allows(Purpose::Treatment, actions::TRIAGE_WRITE));
        assert!(!purpose_allows(Purpose::Operations, actions::TRIAGE_WRITE));
        assert!(purpose_allows(Purpose::Operations, actions::VISIT_MANAGE));
        assert!(!purpose_allows(Purpose::Emergency, actions::VISIT_MANAGE));
    }

    #[test]
    fn scheduling_grants_are_functional_and_least_privilege() {
        assert!(role_allows(roles::REGISTRATION, actions::SCHEDULING_MANAGE));
        assert!(role_allows(roles::NURSE, actions::SCHEDULING_MANAGE));
        assert!(role_allows(roles::PHYSICIAN, actions::SCHEDULING_READ));
        assert!(!role_allows(roles::PHYSICIAN, actions::SCHEDULING_MANAGE));
        assert!(!role_allows(roles::REGISTRATION, actions::CATALOG_MANAGE));
        assert!(!role_allows(roles::REGISTRATION, actions::RESOURCE_MANAGE));
        assert!(role_allows(roles::CLINICAL_ADMIN, actions::CATALOG_MANAGE));
        assert!(role_allows(roles::CLINICAL_ADMIN, actions::RESOURCE_MANAGE));
        // Patients: self-service only, nothing staff-facing.
        assert!(role_allows(
            roles::PATIENT_REP,
            actions::PATIENT_SELF_SERVICE
        ));
        for a in [
            actions::PATIENT_READ,
            actions::PATIENT_SEARCH,
            actions::SCHEDULING_READ,
            actions::SCHEDULING_MANAGE,
            actions::WAITLIST_MANAGE,
            actions::VISIT_READ,
            actions::WORKLIST_READ,
        ] {
            assert!(!role_allows(roles::PATIENT_REP, a), "{a}");
        }
        // Transport: logistics only.
        assert!(role_allows(
            roles::TRANSPORT_COORDINATOR,
            actions::TRANSPORT_COORDINATE
        ));
        for a in [
            actions::PATIENT_READ,
            actions::SCHEDULING_READ,
            actions::SCHEDULING_MANAGE,
            actions::RISK_READ,
        ] {
            assert!(!role_allows(roles::TRANSPORT_COORDINATOR, a), "{a}");
        }
        // Staff never act through the patient path.
        for r in [roles::REGISTRATION, roles::CLINICAL_ADMIN, roles::NURSE] {
            assert!(!role_allows(r, actions::PATIENT_SELF_SERVICE), "{r}");
        }
        assert!(!purpose_allows(
            Purpose::Emergency,
            actions::PATIENT_SELF_SERVICE
        ));
        assert!(!purpose_allows(Purpose::Treatment, actions::CATALOG_MANAGE));
    }

    #[test]
    fn only_authorized_roles_read_audit() {
        for role in roles::ALL {
            let expected = *role == roles::PRIVACY_OFFICER || *role == roles::SECURITY_AUDITOR;
            assert_eq!(role_allows(role, actions::AUDIT_READ), expected, "{role}");
        }
    }
}
