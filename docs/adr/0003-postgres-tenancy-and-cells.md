# ADR-0003: PostgreSQL Tenancy and Regional-Cell Strategy

Status: Accepted · Date: 2026-08-29

## Context

Data residency laws require jurisdictional isolation; hospitals require strict
tenant isolation; the transactional store must be boring and provable.

## Decision

- PostgreSQL 16 is the single authoritative transactional database.
- **Cell** = independent deployment per jurisdiction (app + DB); no cross-cell
  replication of clinical data; server carries a cell identity.
- **Tenant** = organization within a cell; `tenant_id` column on every
  clinical table; all queries tenant-scoped from the authenticated identity;
  shared-schema multi-tenancy (not schema-per-tenant) for operability at this
  stage.

## Consequences

- Residency by architecture, not policy documents.
- Application-level scoping is the only isolation layer today; Postgres RLS is
  the planned second layer (roadmap #2).
- Cross-tenant features (e.g. transfers) must be explicit export/import flows.
