# Regional Cells and Tenancy

## Model

- A **regional cell** is an independent deployment (app + database + object
  storage) pinned to a jurisdiction. Data never leaves its cell; there is no
  global clinical database. The server carries its cell identity
  (`AppState.cell`, e.g. `cell-dev-1`) and stamps it into audit context.
- A **tenant** is a hospital/organization inside a cell. Every clinical table
  carries `tenant_id`; every query is tenant-scoped; the authenticated
  identity's tenant is resolved server-side and can never be supplied by the
  client.

## Enforcement today

- Application-level scoping: all SQL predicates include `tenant_id` derived
  from the authenticated user.
- Cross-tenant access is denied and audited even with break-glass (break-glass
  widens purpose within a tenant, never across tenants).
- Integration tests assert cross-tenant denial (`api_integration.rs`).

## Planned hardening

1. PostgreSQL row-level security policies as a second enforcement layer.
2. Per-tenant encryption contexts for artifacts in object storage.
3. Cell-to-cell interchange only via explicit, consented, standards-based
   export (FHIR bulk), never database replication.

## Configuration, not forks

Tenant differences (branding via JSONB brand tokens, enabled modules, locale
defaults) are configuration rows, not code branches. Customer-specific forks
are explicitly rejected (see product principles).
