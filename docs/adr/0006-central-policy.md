# ADR-0006: Centralized Policy — RBAC plus Contextual ABAC

Status: Accepted · Date: 2026-08-29

## Context

Scattered per-route permission checks drift and rot; healthcare access control
needs role, tenant, purpose-of-use, and emergency (break-glass) context in one
auditable place.

## Decision

- One policy module (`wellos-server/src/policy.rs`) with named action
  constants (`patient.read`, `loop.close`, `audit.read`, ...); deny by
  default.
- Decisions combine roles (RBAC) with context (ABAC): tenant match, purpose of
  use, break-glass reason. Every denial is audited.
- Break-glass: mandatory reason, same-tenant only, enhanced audit event for
  retrospective review — it widens purpose, never tenancy.
- Identity is resolved server-side from the bearer token; development tokens
  (`dev-<username>`) are an explicit non-production stand-in for OIDC/OAuth
  2.1 (roadmap #1).

## Consequences

- The authorization matrix is testable as data (integration tests cover
  nurse/research/audit/cross-tenant cases).
- Adding an endpoint forces naming its action — no accidental open routes.
