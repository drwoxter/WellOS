# Data Flow and Trust Boundaries

```
[Browser]
   │ B1: HTTPS, Authorization: Bearer <token>
   ▼
[Next.js dev proxy /api, /fhir]          (development convenience only)
   │
   ▼
[wellos-server]
   │  ── B2: token → identity, tenant, roles (auth.rs)
   │  ── B3: policy decision per action (policy.rs) + audit emit
   │
   ├── B4: SQLx (parameterized, tenant-scoped) ──▶ [PostgreSQL]
   │        same txn: clinical write + rule eval + audit + outbox
   │
   └── B5: after commit ──▶ [dMind gateway → FakeProvider]
            consent + allow_external_ai gate; input hash recorded;
            offline in development, no network egress
```

## Boundaries

- **B1 Browser→server**: untrusted input. All payloads validated by typed
  deserialization; errors return structured codes without internals.
- **B2 Identity**: the client can never assert tenant, roles, or user id —
  only the token. Development tokens map to seeded synthetic users only.
- **B3 Policy**: every route names an action constant; denials are audited
  with actor and reason. Break-glass adds a mandatory reason and enhanced
  audit; it never crosses tenants.
- **B4 Database**: single authoritative store. Observations and audit are
  append-only. Outbox rows carry identifiers, not clinical payloads.
- **B5 AI gateway**: fires only after the clinical transaction commits;
  failures produce an `unavailable` artifact. External providers are disabled
  by default and additionally gated on purpose-specific consent.

## Data classes

| Class | Examples | Handling |
| --- | --- | --- |
| Clinical (synthetic today) | observations, conditions | tenant-scoped tables only; never in logs/events/telemetry |
| Governance | audit, break-glass, consent, rule evaluations | append-only; restricted read (audit roles) |
| Operational | health/ready, counts | PHI-free by construction |
