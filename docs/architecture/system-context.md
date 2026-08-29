# System Context

```
 Clinicians / Registration / Lab / Privacy roles
                 │  (browser, EN/ES, WCAG 2.2 AA target)
                 ▼
        ┌─────────────────┐
        │  WellOS Web UI  │  Next.js, apps/web
        └────────┬────────┘
                 │ HTTPS + dev bearer token (prod: OIDC)
                 ▼
        ┌─────────────────┐      ┌──────────────────────┐
        │  WellOS Server  │─────▶│  dMind Model Gateway │
        │  (Axum, Rust)   │      │  (fake provider dev) │
        └────────┬────────┘      └──────────────────────┘
                 │ SQLx
                 ▼
        ┌─────────────────┐
        │  PostgreSQL 16  │  authoritative store, outbox, audit
        └─────────────────┘

 External (future): laboratory systems (HL7v2/FHIR), imaging, national
 registries, identity provider, object storage, NATS JetStream, telemetry.
```

## Actors

- **Registration staff** — register/search patients.
- **Physicians** — encounters, orders, review, notification, loop closure.
- **Nurses** — view, record notification; cannot close result loops.
- **Laboratory professionals** — ingest results (synthetic adapter today).
- **Clinical administrators** — worklists, overdue escalation jobs.
- **Privacy officer / security auditor** — audit trail access.
- **Research users** — no direct-care access (de-identified pipelines later).

## Trust boundaries

Browser↔server (authn/z, tenant resolution), server↔database (parameterized
SQL, tenant scoping in every query), server↔model gateway (consent + policy
gate; no PHI leaves the process with the fake provider). See
`docs/security/data-flow-and-trust-boundaries.md`.
