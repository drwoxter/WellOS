# Consent and Rights Model

## Consent

Consents are **purpose-specific and versioned** rows (`consents` table): a
subject (patient), a purpose (e.g. `treatment`, `external_ai_processing`,
`research_deidentified`), a status (granted/revoked), and validity timestamps.
A new decision supersedes the old row rather than overwriting it, preserving
the consent history.

### Enforcement points

- **External AI processing**: requires an active granted consent for the
  purpose *and* the tenant/server flag `allow_external_ai` (default `false`).
  The development fake provider is local, so no data leaves the process even
  when exercised.
- **Purpose of use**: every authenticated request carries a purpose; policy
  decisions and audit records include it. Break-glass overrides purpose within
  a tenant only, with a mandatory reason and enhanced audit.
- **Research**: research roles have no direct-care access; future research use
  is planned through de-identified pipelines, not raw clinical reads.

## Data-subject rights (direction)

| Right | Current support | Planned |
| --- | --- | --- |
| Access | FHIR R4 read facade (Patient, Observation, ServiceRequest) | Patient-facing export (FHIR bulk) |
| Rectification | Amendment model preserves prior versions and reopens review | Patient-initiated correction workflow |
| Erasure | Not implemented (append-only clinical record tension) | Jurisdiction-aware erasure/anonymization policy with clinical-record exemptions documented per cell |
| Restriction/objection | Consent revocation blocks optional processing (external AI) | Broader processing-purpose registry |
| Portability | FHIR facade | Full FHIR bulk export |

## Explicit non-claims

This model is an engineering foundation. It has not been reviewed by counsel
and does not constitute GDPR/HIPAA compliance. Jurisdictional variation is
handled by regional cells and per-cell policy configuration, to be specified
per deployment.
