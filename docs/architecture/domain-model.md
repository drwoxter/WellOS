# Domain Model

## Core aggregates (implemented slice)

- **Tenant / Facility** — isolation root; every clinical row carries
  `tenant_id`. Tenants hold brand tokens (JSONB) for theming.
- **User / RoleAssignment** — synthetic identities with role grants; policy
  decisions derive from roles plus context (tenant, purpose of use,
  break-glass reason).
- **Patient** — synthetic demographics + identifier (unique per tenant).
- **Allergy / Medication / Condition** — encounter-context summary data.
- **Encounter** — clinical context for orders.
- **ServiceRequest** — laboratory order; carries `loop_state` and `version`
  (optimistic concurrency).
- **Observation** — append-only results; amendments reference the observation
  they amend (`amends`); idempotency key unique per tenant.
- **RuleEvaluation** — record of each deterministic rule run: rule id,
  version, input, outcome.
- **Alert / FollowUpTask** — created for critical results; alerts are
  acknowledged by loop closure.
- **AIArtifact** — governed AI output: status lifecycle, autonomy level,
  model metadata, input hash, structured output with citations and
  limitations.
- **Consent** — purpose-specific, versioned (e.g. `external_ai_processing`).
- **DataQualityIssue** — recorded when unsafe evaluation is refused
  (e.g. unit mismatch).
- **AuditEvent / BreakGlassEvent / OutboxEvent** — provenance backbone.

## Result loop state machine

```
ordered → received → reviewed → notified → closed
              ▲          │
              └──────────┘  (amended result reopens review)
```

Transitions are validated in `wellos-domain::result_loop`; invalid transitions
are rejected as conflicts. Closure requires receipt, review, notification, and
follow-up disposition to have been recorded.

## Deterministic rules

`wellos-domain::rules` holds versioned threshold rules (e.g. potassium
critical ≥ 6.5 or ≤ 2.5 mmol/L). Rules refuse to evaluate when units cannot be
safely converted (`UnitError::UnknownConversion`) — that path records a
DataQualityIssue instead of guessing.

## AIArtifact lifecycle

```
draft → awaiting_review → approved | rejected
                        → superseded | withdrawn | invalidated
gateway failure → unavailable (workflow continues)
```

Autonomy levels A0–A4; the result summary runs at **A2** (assistive draft,
human review mandatory). AI never orders, prescribes, diagnoses, or changes
treatment.
