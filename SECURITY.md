# Security Policy

WellOS is a development-stage project. It must not be deployed with real
patient data in its current form.

## Reporting a vulnerability

Email the maintainer (repository owner) with a description, reproduction steps,
and impact. Do not open public issues for exploitable vulnerabilities. You will
receive an acknowledgement within 5 business days.

## Scope and current posture

- Development authentication (`dev-<username>` bearer tokens) is intentionally
  non-production and works only against seeded synthetic users. Production
  deployments require an OIDC/OAuth 2.1 identity provider (see ADR-0006).
- All access decisions flow through a central RBAC + contextual ABAC policy
  module; denials are audited.
- Break-glass access requires an explicit reason, is limited to same-tenant
  resources, and produces enhanced audit records for retrospective review.
- Tenant isolation is enforced in every query path; cross-tenant access is
  denied regardless of role or break-glass.
- Audit and observation records are append-only by application convention.
- No secrets are committed; configuration is via environment variables.
- Logs and outbox events carry identifiers, not clinical payloads.

## Known gaps (tracked in docs/roadmap.md)

- No row-level security in PostgreSQL yet (application-level isolation only).
- No tamper-evident hash chain on audit events yet.
- No rate limiting or WAF in the development server.
- TLS is expected from the deployment environment, not the app process.
